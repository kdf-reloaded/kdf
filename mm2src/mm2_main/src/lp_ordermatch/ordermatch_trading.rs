use super::*;
use coins::request_fee_scope::with_request_fee_scope;

#[cfg(feature = "ibc-routing-for-swaps")]
fn tendermint_chain_id_from_conf(conf: &Json, platform_conf: Option<&Json>) -> Option<String> {
    let protocol: CoinProtocol = json::from_value(conf["protocol"].clone()).ok()?;

    match protocol {
        CoinProtocol::TENDERMINT { chain_id, .. } => Some(chain_id),
        CoinProtocol::TENDERMINTTOKEN { .. } => {
            let platform_protocol: CoinProtocol = json::from_value(platform_conf?["protocol"].clone()).ok()?;
            match platform_protocol {
                CoinProtocol::TENDERMINT { chain_id, .. } => Some(chain_id),
                _ => None,
            }
        },
        _ => None,
    }
}

#[cfg(feature = "ibc-routing-for-swaps")]
fn tendermint_chain_id(ctx: &MmArc, coin: &MmCoinEnum) -> Option<String> {
    let conf = coin_conf(ctx, coin.ticker());
    let platform_conf = match json::from_value::<CoinProtocol>(conf["protocol"].clone()).ok()? {
        CoinProtocol::TENDERMINTTOKEN { platform, .. } => Some(coin_conf(ctx, &platform)),
        _ => None,
    };

    tendermint_chain_id_from_conf(&conf, platform_conf.as_ref())
}

#[cfg(feature = "ibc-routing-for-swaps")]
fn min_balance_for_ibc_routing_from_conf(conf: &Json) -> MmNumber {
    if let Some(amount) = conf["min_balance_for_ibc_routing"].as_str() {
        if let Ok(parsed) = amount.parse::<BigDecimal>() {
            return MmNumber::from(parsed);
        }
    }
    if conf["min_balance_for_ibc_routing"].is_number() {
        let amount = conf["min_balance_for_ibc_routing"].to_string();
        if let Ok(parsed) = amount.parse::<BigDecimal>() {
            return MmNumber::from(parsed);
        }
    }

    MmNumber::from(2i32)
}

#[cfg(feature = "ibc-routing-for-swaps")]
fn min_balance_for_ibc_routing(ctx: &MmArc, coin: &MmCoinEnum) -> MmNumber {
    let conf = coin_conf(ctx, coin.ticker());
    min_balance_for_ibc_routing_from_conf(&conf)
}

#[cfg(feature = "ibc-routing-for-swaps")]
async fn ensure_ibc_routing_min_balance(
    ctx: &MmArc,
    base_coin: &MmCoinEnum,
    rel_coin: &MmCoinEnum,
) -> Result<(), String> {
    let Some(base_chain_id) = tendermint_chain_id(ctx, base_coin) else {
        return Ok(());
    };
    let Some(rel_chain_id) = tendermint_chain_id(ctx, rel_coin) else {
        return Ok(());
    };

    if base_chain_id == rel_chain_id {
        return Ok(());
    }

    let required_min_balance = min_balance_for_ibc_routing(ctx, base_coin);
    let current_balance: MmNumber = base_coin
        .my_spendable_balance()
        .compat()
        .await
        .map_err(|e| e.to_string())?
        .into();
    if current_balance < required_min_balance {
        return ERR!(
            "IBC routing requires minimum balance on HTLC coin {}: required {}, current {}",
            base_coin.ticker(),
            required_min_balance.to_decimal(),
            current_balance.to_decimal()
        );
    }

    Ok(())
}

/// The per-order step of the periodic maintenance for an own maker order absent from the local
/// orderbook (CRD ch.59 R59.6.7 item 2): record the pair subscription (and subscribe on the mesh
/// only if there was no record), then re-broadcast the created notification. No orderbook request
/// is made.
pub(crate) fn reannounce_missing_own_order(
    ctx: &MmArc,
    order: &MakerOrder,
    base_protocol_info: Vec<u8>,
    rel_protocol_info: Vec<u8>,
) {
    if let Err(e) = record_topic_subscription_without_request(ctx, order.orderbook_topic()) {
        log::error!("Error {} on recording the subscription of order {}", e, order.uuid);
        return;
    }
    maker_order_created_p2p_notify(ctx.clone(), order, base_protocol_info, rel_protocol_info);
}

pub(crate) fn maker_order_created_p2p_notify(
    ctx: MmArc,
    order: &MakerOrder,
    base_protocol_info: Vec<u8>,
    rel_protocol_info: Vec<u8>,
) {
    let topic = order.orderbook_topic();
    let message = new_protocol::MakerOrderCreated {
        uuid: order.uuid.into(),
        base: order.base_orderbook_ticker().to_owned(),
        rel: order.rel_orderbook_ticker().to_owned(),
        price: order.price.to_ratio(),
        max_volume: order.available_amount().to_ratio(),
        min_volume: order.min_base_vol.to_ratio(),
        conf_settings: order.conf_settings.unwrap(),
        created_at: now_ms() / 1000,
        timestamp: now_ms() / 1000,
        pair_trie_root: H64::default(),
        base_protocol_info,
        rel_protocol_info,
    };

    let key_pair = match order.p2p_keypair() {
        Some(k) => k,
        None => ctx.secp256k1_key_pair(),
    };

    // The local state is updated at once; only the gossip message waits in the outbox (CRD ch.32 R-MP1 item 3).
    let orderbook_item: OrderbookItem = (message.clone(), hex::encode(key_pair.public_slice())).into();
    insert_or_update_my_order(&ctx, orderbook_item, order);
    publish_maker_order_message(
        &ctx,
        order.p2p_keypair(),
        order.uuid,
        topic,
        OutboxMessage::Created(message),
    );
}

#[cfg(all(test, feature = "ibc-routing-for-swaps"))]
mod tests {
    use super::*;

    #[test]
    fn test_tendermint_chain_id_from_conf_tendermint() {
        let conf = json::json!({
            "protocol": {
                "type": "TENDERMINT",
                "protocol_data": {
                    "account_prefix": "cosmos",
                    "chain_id": "cosmoshub-4"
                }
            }
        });

        let chain_id = tendermint_chain_id_from_conf(&conf, None);
        assert_eq!(chain_id.as_deref(), Some("cosmoshub-4"));
    }

    #[test]
    fn test_tendermint_chain_id_from_conf_tendermint_token_uses_platform() {
        let token_conf = json::json!({
            "protocol": {
                "type": "TENDERMINTTOKEN",
                "protocol_data": {
                    "platform": "ATOM",
                    "denom": "uatom",
                    "decimals": 6
                }
            }
        });
        let platform_conf = json::json!({
            "protocol": {
                "type": "TENDERMINT",
                "protocol_data": {
                    "account_prefix": "cosmos",
                    "chain_id": "cosmoshub-4"
                }
            }
        });

        let chain_id = tendermint_chain_id_from_conf(&token_conf, Some(&platform_conf));
        assert_eq!(chain_id.as_deref(), Some("cosmoshub-4"));
    }

    #[test]
    fn test_tendermint_chain_id_from_conf_tendermint_token_without_platform_conf_returns_none() {
        let token_conf = json::json!({
            "protocol": {
                "type": "TENDERMINTTOKEN",
                "protocol_data": {
                    "platform": "ATOM",
                    "denom": "uatom",
                    "decimals": 6
                }
            }
        });

        let chain_id = tendermint_chain_id_from_conf(&token_conf, None);
        assert!(chain_id.is_none());
    }

    #[test]
    fn test_min_balance_for_ibc_routing_from_conf_string() {
        let conf = json::json!({ "min_balance_for_ibc_routing": "2.75" });
        let min = min_balance_for_ibc_routing_from_conf(&conf);
        assert_eq!(min.to_decimal(), "2.75".parse::<BigDecimal>().unwrap());
    }

    #[test]
    fn test_min_balance_for_ibc_routing_from_conf_numeric() {
        let conf = json::json!({ "min_balance_for_ibc_routing": 3.5 });
        let min = min_balance_for_ibc_routing_from_conf(&conf);
        assert_eq!(min.to_decimal(), "3.5".parse::<BigDecimal>().unwrap());
    }

    #[test]
    fn test_min_balance_for_ibc_routing_from_conf_invalid_falls_back_to_default() {
        let conf = json::json!({ "min_balance_for_ibc_routing": "not-a-number" });
        let min = min_balance_for_ibc_routing_from_conf(&conf);
        assert_eq!(min.to_decimal(), BigDecimal::from(2));
    }

    #[test]
    fn test_min_balance_for_ibc_routing_from_conf_missing_falls_back_to_default() {
        let conf = json::json!({});
        let min = min_balance_for_ibc_routing_from_conf(&conf);
        assert_eq!(min.to_decimal(), BigDecimal::from(2));
    }
}

pub(crate) fn process_my_maker_order_updated(ctx: &MmArc, message: &new_protocol::MakerOrderUpdated) {
    let ordermatch_ctx = OrdermatchContext::from_ctx(ctx).expect("from_ctx failed");
    let mut orderbook = ordermatch_ctx.orderbook.lock();

    let uuid = message.uuid();
    if let Some(mut order) = orderbook.find_order_by_uuid(&uuid) {
        order.apply_updated(message);
        // Phase 1: index update under orderbook lock, collect trie ops
        let ops = orderbook.index_insert_or_update(order);
        // Phase 2: queue the trie ops before the guard is released (CRD ch.32 R-T3b item 1)
        orderbook.queue_trie_ops(&ordermatch_ctx.trie_ops_tx, ops);
    }
}

pub(crate) fn maker_order_updated_p2p_notify(
    ctx: MmArc,
    topic: String,
    message: new_protocol::MakerOrderUpdated,
    p2p_privkey: Option<&KeyPair>,
) {
    process_my_maker_order_updated(&ctx, &message);
    publish_maker_order_message(
        &ctx,
        p2p_privkey,
        message.uuid(),
        topic,
        OutboxMessage::Updated(message),
    );
}

pub(crate) fn maker_order_cancelled_p2p_notify(ctx: MmArc, order: &MakerOrder) {
    let message = new_protocol::MakerOrderCancelled {
        uuid: order.uuid.into(),
        timestamp: now_ms() / 1000,
        pair_trie_root: H64::default(),
    };
    delete_my_order(&ctx, order.uuid, order.p2p_privkey.as_ref());
    log::debug!("maker_order_cancelled_p2p_notify called, message {:?}", message);
    publish_maker_order_message(
        &ctx,
        order.p2p_keypair(),
        order.uuid,
        order.orderbook_topic(),
        OutboxMessage::Cancelled(message),
    );
}

/// The pubkey, as hex, a maker-order message signed with `key_pair` is published under: the per-order key, or
/// the persistent key of the node.
fn maker_pubkey_hex(ctx: &MmArc, key_pair: Option<&KeyPair>) -> String {
    match key_pair {
        Some(key_pair) => hex::encode(key_pair.public_slice()),
        None => hex::encode(ctx.secp256k1_key_pair().public_slice()),
    }
}

/// Publishes what the outbox released. The cancellation leaves with priority, like every swap-negotiation
/// message, because `broadcast_ordermatch_message` marks the priority by message kind.
fn publish_released(ctx: &MmArc, releases: Vec<Release>) {
    for release in releases {
        let message = match release.message {
            OutboxMessage::Created(created) => new_protocol::OrdermatchMessage::MakerOrderCreated(created),
            OutboxMessage::Updated(updated) => new_protocol::OrdermatchMessage::MakerOrderUpdated(updated),
            OutboxMessage::Cancelled(cancelled) => new_protocol::OrdermatchMessage::MakerOrderCancelled(cancelled),
        };
        broadcast_ordermatch_message(ctx, [release.topic], message, release.key_pair.as_ref());
    }
}

/// Publishes what the outbox released, and remembers when it last released a created or updated message, which a
/// relay close needs (CRD ch.32 R-MP2 item 2).
///
/// The releases were counted as in flight under the outbox lock that released them (`count_in_flight`); they stop being
/// in flight here, once they are handed to the swarm, so that a keep-alive waits for them (CRD ch.32 R-MP3 items 2, 3).
fn publish_outbox_releases(ctx: &MmArc, ordermatch_ctx: &OrdermatchContext, releases: Vec<Release>) {
    let count = releases.len();
    if releases
        .iter()
        .any(|release| !matches!(release.message, OutboxMessage::Cancelled(_)))
    {
        ordermatch_ctx
            .maker_outbox
            .last_release_ms
            .store(now_ms(), std::sync::atomic::Ordering::Relaxed);
    }
    publish_released(ctx, releases);
    ordermatch_ctx
        .maker_outbox
        .in_flight
        .fetch_sub(count, std::sync::atomic::Ordering::SeqCst);
}

/// Counts `releases` as in flight. It is called while the outbox lock that released them is still held, so that no
/// reader sees them neither pending nor in flight.
fn count_in_flight(ordermatch_ctx: &OrdermatchContext, releases: &[Release]) {
    ordermatch_ctx
        .maker_outbox
        .in_flight
        .fetch_add(releases.len(), std::sync::atomic::Ordering::SeqCst);
}

/// The log line of a signal, a rate change or a change of the degraded mode (CRD ch.32 R-MP2 item 5). No order
/// content.
fn log_outbox_rate(line: RateLog) {
    let pubkey = &line.pubkey[..line.pubkey.len().min(16)];
    let signal = line
        .signal
        .map_or(String::new(), |kind| format!(" (a {} signal came)", kind.as_str()));
    if line.degraded {
        log::warn!(
            "Maker publication of pubkey {}{}: a relay is not keeping up, the update rate is {:.3} messages/s and \
             created and updated messages are not published; peers learn the changes through the keep-alive \
             synchronization",
            pubkey,
            signal,
            line.rate
        );
    } else {
        log::info!(
            "Maker publication of pubkey {}{}: the update rate is {:.3} messages/s",
            pubkey,
            signal,
            line.rate
        );
    }
}

/// Hands a maker-order message to the outbox of its pubkey (CRD ch.32 §32.9B R-MP1, R-MP2), and publishes at once
/// what the outbox lets go: a cancellation, and the oldest pending messages while the budget allows.
/// With `maker_update_coalescing` set to `false` the message is published at once, as without the outbox.
pub(crate) fn publish_maker_order_message(
    ctx: &MmArc,
    key_pair: Option<&KeyPair>,
    uuid: Uuid,
    topic: String,
    message: OutboxMessage,
) {
    let ordermatch_ctx = if maker_update_coalescing(ctx) {
        match OrdermatchContext::from_ctx(ctx) {
            Ok(ordermatch_ctx) => Some(ordermatch_ctx),
            Err(e) => {
                log::error!("No ordermatch context for the maker outbox, publishing at once: {}", e);
                None
            },
        }
    } else {
        None
    };
    let Some(ordermatch_ctx) = ordermatch_ctx else {
        let release = Release {
            topic,
            message,
            key_pair: key_pair.copied(),
        };
        publish_released(ctx, vec![release]);
        return;
    };

    let pubkey = maker_pubkey_hex(ctx, key_pair);
    let (releases, log_line) = {
        let mut outboxes = ordermatch_ctx.maker_outbox.outboxes.lock();
        let (releases, log_line) = outboxes.enqueue(
            now_ms(),
            relay_queue_signal(ctx),
            maker_update_rate_max(ctx),
            &pubkey,
            key_pair.copied(),
            uuid,
            topic,
            message,
        );
        count_in_flight(&ordermatch_ctx, &releases);
        (releases, log_line)
    };
    log_line.into_iter().for_each(log_outbox_rate);
    publish_outbox_releases(ctx, &ordermatch_ctx, releases);
}

/// The close of the last connection to a relay (CRD ch.32 §32.9B R-MP2 item 2, the close signal). It is a signal if
/// an outbox holds a pending created or updated message, or released one within the preceding 30 seconds; every
/// outbox then drops to the floor in degraded mode.
pub(crate) fn relay_connection_closed(ctx: &MmArc) {
    let Ok(ordermatch_ctx) = OrdermatchContext::from_ctx(ctx) else {
        return;
    };
    let now = now_ms();
    let last_release = ordermatch_ctx
        .maker_outbox
        .last_release_ms
        .load(std::sync::atomic::Ordering::Relaxed);
    let mut outboxes = ordermatch_ctx.maker_outbox.outboxes.lock();
    if close_is_signal(outboxes.has_pending(), Some(last_release), now) {
        // The line that enters degraded mode is logged at once (R-MP2 item 5).
        outboxes
            .signal(now, SignalKind::Close)
            .into_iter()
            .for_each(log_outbox_rate);
        ordermatch_ctx
            .maker_outbox
            .close_signals
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// One step of the outbox: adapts the rate and releases what the budget allows. It also records the outbox
/// gauges (CRD ch.32 R-MP5).
pub(crate) fn pump_maker_outbox(ctx: &MmArc) {
    let Ok(ordermatch_ctx) = OrdermatchContext::from_ctx(ctx) else {
        return;
    };
    let ceiling = maker_update_rate_max(ctx);
    let queue_signal = relay_queue_signal(ctx);
    let (releases, log_lines, metrics) = {
        let mut outboxes = ordermatch_ctx.maker_outbox.outboxes.lock();
        let (releases, log_lines) = outboxes.tick(now_ms(), queue_signal, ceiling);
        count_in_flight(&ordermatch_ctx, &releases);
        (releases, log_lines, outboxes.metrics(ceiling))
    };
    log_lines.into_iter().for_each(log_outbox_rate);
    publish_outbox_releases(ctx, &ordermatch_ctx, releases);

    mm_gauge!(ctx.metrics, "p2p.outbox.pending", metrics.pending as i64);
    // Gauges hold integers: the rate is reported in thousandths of a message per second.
    mm_gauge!(ctx.metrics, "p2p.outbox.rate", (metrics.rate * 1000.0).round() as i64);
    mm_gauge!(ctx.metrics, "p2p.outbox.pressure", i64::from(queue_signal));
    mm_gauge!(ctx.metrics, "p2p.outbox.degraded", i64::from(metrics.degraded));
    mm_gauge!(
        ctx.metrics,
        "p2p.outbox.signals.queue.count",
        relay_queue_signal_count(ctx) as i64
    );
    mm_gauge!(
        ctx.metrics,
        "p2p.outbox.signals.close.count",
        ordermatch_ctx
            .maker_outbox
            .close_signals
            .load(std::sync::atomic::Ordering::Relaxed) as i64
    );
}

/// How often the outbox is stepped. A release is at most this late when the budget allows it.
const OUTBOX_PUMP_INTERVAL_SECS: f64 = 0.1;

pub async fn maker_outbox_loop(ctx: MmArc) {
    while !ctx.is_stopping() {
        pump_maker_outbox(&ctx);
        Timer::sleep(OUTBOX_PUMP_INTERVAL_SECS).await;
    }
}

/// How long after a relay joined the relay mesh the keep-alive round is published (CRD ch.32 R-MP4 asks for
/// within 5 seconds). The delay lets further relays that join at the same time share the one round.
const RECONNECT_ROUND_DELAY_SECS: f64 = 1.0;

/// Schedules the keep-alive round that follows a relay joining the relay mesh (CRD ch.32 R-MP4): one
/// keep-alive per pair per maker pubkey, each after the bounded wait of R-MP3 item 2. Joins within the delay share
/// one round.
pub(crate) fn schedule_reconnect_keep_alive_round(ctx: MmArc) {
    let Ok(ordermatch_ctx) = OrdermatchContext::from_ctx(&ctx) else {
        return;
    };
    if ordermatch_ctx
        .maker_outbox
        .reconnect_round_pending
        .swap(true, std::sync::atomic::Ordering::SeqCst)
    {
        return;
    }
    spawn(async move {
        Timer::sleep(RECONNECT_ROUND_DELAY_SECS).await;
        ordermatch_ctx
            .maker_outbox
            .reconnect_round_pending
            .store(false, std::sync::atomic::Ordering::SeqCst);
        if ctx.is_stopping() {
            return;
        }
        let Ok(crypto_ctx) = CryptoCtx::from_ctx(&ctx) else {
            return;
        };
        broadcast_keep_alives_for_own_pubkeys(&ctx, &ordermatch_ctx, &crypto_ctx.mm2_internal_pubkey_hex()).await;
    });
}

pub struct BalanceUpdateOrdermatchHandler {
    pub(crate) ctx: MmWeak,
}

impl BalanceUpdateOrdermatchHandler {
    pub fn new(ctx: MmArc) -> Self { BalanceUpdateOrdermatchHandler { ctx: ctx.weak() } }
}

#[async_trait]
impl BalanceTradeFeeUpdatedHandler for BalanceUpdateOrdermatchHandler {
    async fn balance_updated(&self, coin: &MmCoinEnum, new_balance: &BigDecimal) {
        // The scope belongs to the node; a node that is gone has nothing to update (as in the implementation).
        let Some(ctx) = MmArc::from_weak(&self.ctx) else { return };
        with_request_fee_scope(&ctx, self.balance_updated_impl(coin, new_balance)).await
    }
}

impl BalanceUpdateOrdermatchHandler {
    async fn balance_updated_impl(&self, coin: &MmCoinEnum, new_balance: &BigDecimal) {
        let ctx = match MmArc::from_weak(&self.ctx) {
            Some(ctx) => ctx,
            None => return,
        };
        if coin.wallet_only(&ctx) {
            log::warn!(
                "coin: {} is wallet only, skip BalanceTradeFeeUpdatedHandler",
                coin.ticker()
            );
            return;
        }
        // Get the max maker available volume to check if the wallet balances are sufficient for the issued maker orders.
        // Note although the maker orders are issued already, but they are not matched yet, so pass the `OrderIssue` stage.
        let new_volume = match calc_max_maker_vol(&ctx, coin, new_balance, FeeApproxStage::OrderIssue).await {
            Ok(info) => info.volume,
            Err(e) if e.get_inner().not_sufficient_balance() => MmNumber::from(0),
            Err(e) => {
                log::warn!("Couldn't handle the 'balance_updated' event: {}", e);
                return;
            },
        };

        let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
        let my_maker_orders = ordermatch_ctx.maker_orders_ctx.lock().clone_orders();

        for (_, order_mutex) in my_maker_orders {
            // The order's lock is held for the in-memory work and the announcement only; the deletion from
            // storage runs after it is released (CRD ch.12 R7a).
            let cancelled = {
                let mut order = order_mutex.lock().await;
                if order.base != coin.ticker() {
                    continue;
                }

                // This checks that the order hasn't been removed by another process
                let cancelled = if new_volume < order.min_base_vol {
                    RemovedMakerOrder::take_out(&ordermatch_ctx, &order)
                } else {
                    None
                };
                if cancelled.is_some() {
                    maker_order_cancelled_p2p_notify(ctx.clone(), &order);
                } else if new_volume < order.available_amount() {
                    order.max_base_vol = &order.reserved_amount() + &new_volume;
                    let mut update_msg = new_protocol::MakerOrderUpdated::new(order.uuid);
                    update_msg.with_new_max_volume(order.available_amount().into());
                    maker_order_updated_p2p_notify(
                        ctx.clone(),
                        order.orderbook_topic(),
                        update_msg,
                        order.p2p_keypair(),
                    );
                }
                cancelled
            };
            if let Some(cancelled) = cancelled {
                cancelled
                    .delete(&ctx, MakerOrderCancellationReason::InsufficientBalance)
                    .await;
            }
        }
    }
}

/// The trie roots a pubkey advertises (CRD ch.32 R-K2a): every pair root except the zero and the
/// empty-trie root.
pub(crate) fn advertisable_roots(
    all_roots: &HashMap<AlbOrderedOrderbookPair, H64>,
) -> HashMap<AlbOrderedOrderbookPair, H64> {
    all_roots
        .iter()
        .filter(|(_, root)| **root != H64::default() && **root != hashed_null_node::<Layout>())
        .map(|(alb_pair, root)| (alb_pair.clone(), *root))
        .collect()
}

/// The roots the keep-alives for `pubkey` would carry, taken from a snapshot in which the trie
/// store has applied every completed index operation (CRD ch.32 R-T3b item 2).
/// `Ok(None)` means the pubkey has no state; `Err` means the worker did not catch up within the
/// bound, so no root may be advertised this round.
pub(crate) async fn keep_alive_payload_for_pub(
    ordermatch_ctx: &OrdermatchContext,
    pubkey: &str,
    catch_up_bound_ms: u64,
) -> Result<Option<HashMap<AlbOrderedOrderbookPair, H64>>, TrieNotCaughtUp> {
    ordermatch_ctx
        .with_consistent_state(catch_up_bound_ms, |_orderbook, trie_store| {
            trie_store
                .pubkeys_state
                .get(pubkey)
                .map(|state| advertisable_roots(&state.trie_roots))
        })
        .await
}

/// The longest a maker pubkey's keep-alives wait for the pending messages of their pairs (CRD ch.32 §32.9B R-MP3
/// item 2).
const KEEP_ALIVE_MAX_WAIT_MS: u64 = 10_000;

/// Waits until the pending outbox messages of the pairs `topics` of `pubkey` have been released within the budget, for
/// at most `max_wait_ms`, or until the outbox is degraded (CRD ch.32 §32.9B R-MP3 item 2). The pump does the
/// releasing; the wait never releases anything itself.
pub(crate) async fn wait_for_pairs_to_drain(
    ordermatch_ctx: &OrdermatchContext,
    pubkey: &str,
    topics: &[String],
    max_wait_ms: u64,
) {
    let started = now_ms();
    loop {
        // A released message that is still being handed to the swarm counts as pending: the keep-alive must not
        // overtake it (R-MP3 item 3).
        let (pending, degraded) = {
            let outboxes = ordermatch_ctx.maker_outbox.outboxes.lock();
            let in_flight = ordermatch_ctx
                .maker_outbox
                .in_flight
                .load(std::sync::atomic::Ordering::SeqCst);
            (
                outboxes.pending_for_topics(pubkey, topics) + in_flight,
                outboxes.is_degraded(pubkey),
            )
        };
        if pending == 0 || degraded || now_ms().saturating_sub(started) >= max_wait_ms {
            return;
        }
        Timer::sleep(KEEP_ALIVE_WAIT_POLL_SECS).await;
    }
}

/// How often the wait of R-MP3 item 2 looks at the outbox.
const KEEP_ALIVE_WAIT_POLL_SECS: f64 = 0.1;

/// Publishes the keep-alives of `pubkey` (CRD ch.32 R-K2a): one message per advertisable pair,
/// carrying that pair's root alone, on that pair's topic only.
///
/// Before them, the node waits up to 10 seconds in total for the pending outbox messages of those pairs to be released
/// within the budget, and not at all in degraded mode (R-MP3 item 2).
pub(crate) async fn broadcast_keep_alive_for_pub(
    ctx: &MmArc,
    ordermatch_ctx: &OrdermatchContext,
    pubkey: &str,
    p2p_privkey: Option<&KeyPair>,
) {
    let mut trie_roots = match keep_alive_payload_for_pub(ordermatch_ctx, pubkey, TRIE_CATCH_UP_BOUND_MS).await {
        Ok(Some(roots)) => roots,
        Ok(None) => return,
        Err(TrieNotCaughtUp) => {
            log::warn!(
                "Trie store is behind the orderbook index, skipping the keep-alive of pubkey {} this round",
                pubkey
            );
            return;
        },
    };

    // CRD ch.32 R-MP3 item 2: the changes of the pairs leave before the keep-alives that account for them, where the
    // budget allows. The roots are read again after a wait, so that they are current.
    let topics: Vec<String> = trie_roots
        .keys()
        .map(|alb_pair| orderbook_topic_from_ordered_pair(alb_pair))
        .collect();
    let must_wait = {
        let outboxes = ordermatch_ctx.maker_outbox.outboxes.lock();
        let in_flight = ordermatch_ctx
            .maker_outbox
            .in_flight
            .load(std::sync::atomic::Ordering::SeqCst);
        outboxes.pending_for_topics(pubkey, &topics) + in_flight > 0 && !outboxes.is_degraded(pubkey)
    };
    if must_wait {
        wait_for_pairs_to_drain(ordermatch_ctx, pubkey, &topics, KEEP_ALIVE_MAX_WAIT_MS).await;
        trie_roots = match keep_alive_payload_for_pub(ordermatch_ctx, pubkey, TRIE_CATCH_UP_BOUND_MS).await {
            Ok(Some(roots)) => roots,
            Ok(None) => return,
            Err(TrieNotCaughtUp) => return,
        };
    }

    let timestamp = now_ms() / 1000;
    for (alb_pair, root) in trie_roots {
        let topic = orderbook_topic_from_ordered_pair(&alb_pair);
        let message = new_protocol::PubkeyKeepAlive {
            trie_roots: HashMap::from([(alb_pair, root)]),
            timestamp,
        };
        broadcast_ordermatch_message(ctx, [topic], message.into(), p2p_privkey);
    }
}

pub async fn broadcast_maker_orders_keep_alive_loop(ctx: MmArc) {
    let crypto_ctx = match CryptoCtx::from_ctx(&ctx) {
        Ok(c) => c,
        Err(_) => {
            // No signing identity (e.g. no-login mode) — nothing to broadcast.
            return;
        },
    };
    let persistent_pubsecp = crypto_ctx.mm2_internal_pubkey_hex();

    while !ctx.is_stopping() {
        Timer::sleep(MIN_ORDER_KEEP_ALIVE_INTERVAL as f64).await;
        let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).expect("from_ctx failed");
        broadcast_keep_alives_for_own_pubkeys(&ctx, &ordermatch_ctx, &persistent_pubsecp).await;
    }
}

/// The keep-alives of every pubkey of this node's maker orders: the per-order keys, then the persistent one.
/// It serves the regular tick and the round after a relay joined the relay mesh (CRD ch.32 R-MP4).
pub(crate) async fn broadcast_keep_alives_for_own_pubkeys(
    ctx: &MmArc,
    ordermatch_ctx: &OrdermatchContext,
    persistent_pubsecp: &str,
) {
    let my_orders = ordermatch_ctx.maker_orders_ctx.lock().clone_orders();
    for (_, order_mutex) in my_orders {
        let order = order_mutex.lock().await;
        if let Some(p2p_privkey) = order.p2p_privkey {
            // Artem Vitae
            // I tried if let Some(p2p_privkey) = order_mutex.lock().await.p2p_privkey
            // but it seems to keep holding the guard
            drop(order);
            let pubsecp = hex::encode(p2p_privkey.public_slice());
            broadcast_keep_alive_for_pub(ctx, ordermatch_ctx, &pubsecp, Some(p2p_privkey.key_pair())).await;
        }
    }

    broadcast_keep_alive_for_pub(ctx, ordermatch_ctx, persistent_pubsecp, None).await;
}

pub(crate) fn broadcast_ordermatch_message(
    ctx: &MmArc,
    topics: impl IntoIterator<Item = String>,
    msg: new_protocol::OrdermatchMessage,
    p2p_privkey: Option<&KeyPair>,
) {
    let (secret, peer_id) = match p2p_privkey {
        Some(k) => (k.private_bytes(), Some(k.libp2p_peer_id())),
        None => (ctx.secp256k1_key_pair().private_bytes(), None),
    };
    let encoded_msg = encode_and_sign(&msg, &secret).unwrap();
    // CRD ch.32 §32.9B R-MP1 item 2: the cancellation and the swap-negotiation messages leave with priority.
    broadcast_p2p_msg_with_priority(
        ctx,
        topics.into_iter().collect(),
        encoded_msg,
        peer_id,
        msg.is_priority(),
    );
}

#[cfg_attr(test, mockable)]
pub(crate) fn lp_connect_start_bob(ctx: MmArc, maker_match: MakerMatch, maker_order: MakerOrder) {
    spawn(async move {
        // aka "maker_loop"
        let taker_coin = match lp_coinfind(&ctx, &maker_order.rel).await {
            Ok(Some(c)) => c,
            Ok(None) => {
                log::error!("Coin {} is not found/enabled", maker_order.rel);
                return;
            },
            Err(e) => {
                log::error!("!lp_coinfind({}): {}", maker_order.rel, e);
                return;
            },
        };

        let maker_coin = match lp_coinfind(&ctx, &maker_order.base).await {
            Ok(Some(c)) => c,
            Ok(None) => {
                log::error!("Coin {} is not found/enabled", maker_order.base);
                return;
            },
            Err(e) => {
                log::error!("!lp_coinfind({}): {}", maker_order.base, e);
                return;
            },
        };
        let alice = bits256::from(maker_match.request.sender_pubkey.0);
        let maker_amount = maker_match.reserved.get_base_amount().to_decimal();
        let taker_amount = maker_match.reserved.get_rel_amount().to_decimal();
        let privkey = &ctx.secp256k1_key_pair().private().secret;
        let my_persistent_pub = compressed_pub_key_from_priv_raw(&privkey[..], ChecksumType::DSHA256).unwrap();
        let uuid = maker_match.request.uuid;
        let my_conf_settings = choose_maker_confs_and_notas(
            maker_order.conf_settings,
            &maker_match.request,
            &maker_coin,
            &taker_coin,
        );
        // detect atomic lock time version implicitly by conf_settings existence in taker request
        let atomic_locktime_v = match maker_match.request.conf_settings {
            Some(_) => {
                let other_conf_settings =
                    choose_taker_confs_and_notas(&maker_match.request, &maker_match.reserved, &maker_coin, &taker_coin);
                AtomicLocktimeVersion::V2 {
                    my_conf_settings,
                    other_conf_settings,
                }
            },
            None => AtomicLocktimeVersion::V1,
        };
        let lock_time = lp_atomic_locktime(maker_coin.ticker(), taker_coin.ticker(), atomic_locktime_v);
        log_tag!(
            ctx,
            "";
            fmt = "Entering the maker_swap_loop {}/{} with uuid: {}",
            maker_coin.ticker(),
            taker_coin.ticker(),
            uuid
        );

        let now = now_ms() / 1000;
        if let Err(e) = insert_new_swap_to_db(ctx.clone(), maker_coin.ticker(), taker_coin.ticker(), uuid, now).await {
            error!("Error {} on new swap insertion", e);
        }
        let maker_swap = MakerSwap::new(
            ctx.clone(),
            alice,
            maker_amount,
            taker_amount,
            my_persistent_pub,
            uuid,
            Some(maker_order.uuid),
            my_conf_settings,
            maker_coin,
            taker_coin,
            lock_time,
            maker_order.p2p_privkey.map(SerializableSecp256k1Keypair::into_inner),
        );
        run_maker_swap(RunMakerSwapInput::StartNew(maker_swap), ctx).await;
    });
}

pub(crate) fn lp_connected_alice(ctx: MmArc, taker_order: TakerOrder, taker_match: TakerMatch) {
    spawn(async move {
        // aka "taker_loop"
        let maker = bits256::from(taker_match.reserved.sender_pubkey.0);
        let taker_coin_ticker = taker_order.taker_coin_ticker();
        let taker_coin = match lp_coinfind(&ctx, taker_coin_ticker).await {
            Ok(Some(c)) => c,
            Ok(None) => {
                log::error!("Coin {} is not found/enabled", taker_coin_ticker);
                return;
            },
            Err(e) => {
                log::error!("!lp_coinfind({}): {}", taker_coin_ticker, e);
                return;
            },
        };

        let maker_coin_ticker = taker_order.maker_coin_ticker();
        let maker_coin = match lp_coinfind(&ctx, maker_coin_ticker).await {
            Ok(Some(c)) => c,
            Ok(None) => {
                log::error!("Coin {} is not found/enabled", maker_coin_ticker);
                return;
            },
            Err(e) => {
                log::error!("!lp_coinfind({}): {}", maker_coin_ticker, e);
                return;
            },
        };

        let privkey = &ctx.secp256k1_key_pair().private().secret;
        let my_persistent_pub = compressed_pub_key_from_priv_raw(&privkey[..], ChecksumType::DSHA256).unwrap();
        let maker_amount = taker_match.reserved.get_base_amount().clone();
        let taker_amount = taker_match.reserved.get_rel_amount().clone();
        let uuid = taker_match.reserved.taker_order_uuid;

        let my_conf_settings =
            choose_taker_confs_and_notas(&taker_order.request, &taker_match.reserved, &maker_coin, &taker_coin);
        // detect atomic lock time version implicitly by conf_settings existence in maker reserved
        let atomic_locktime_v = match taker_match.reserved.conf_settings {
            Some(_) => {
                let other_conf_settings = choose_maker_confs_and_notas(
                    taker_match.reserved.conf_settings,
                    &taker_order.request,
                    &maker_coin,
                    &taker_coin,
                );
                AtomicLocktimeVersion::V2 {
                    my_conf_settings,
                    other_conf_settings,
                }
            },
            None => AtomicLocktimeVersion::V1,
        };
        let locktime = lp_atomic_locktime(maker_coin.ticker(), taker_coin.ticker(), atomic_locktime_v);
        log_tag!(
            ctx,
            "";
            fmt = "Entering the taker_swap_loop {}/{} with uuid: {}",
            maker_coin.ticker(),
            taker_coin.ticker(),
            uuid
        );
        let now = now_ms() / 1000;
        if let Err(e) = insert_new_swap_to_db(ctx.clone(), taker_coin.ticker(), maker_coin.ticker(), uuid, now).await {
            error!("Error {} on new swap insertion", e);
        }
        let taker_swap = TakerSwap::new(
            ctx.clone(),
            maker,
            maker_amount,
            taker_amount,
            my_persistent_pub,
            uuid,
            Some(uuid),
            my_conf_settings,
            maker_coin,
            taker_coin,
            locktime,
            taker_order.p2p_privkey.map(SerializableSecp256k1Keypair::into_inner),
        );
        run_taker_swap(RunTakerSwapInput::StartNew(taker_swap), ctx).await
    });
}

/// Selects the tracked public keys whose state the maintenance sweep must evict (CRD ch.59 R59.6.4),
/// together with the identifiers of the orders they own.
///
/// The node's own persistent public key and every per-order public key it makes orders under
/// (`my_p2p_pubkeys`) are never selected (R59.6.5): our own keep-alives are not delivered back to us,
/// so their liveness instant would otherwise age out.
pub(crate) fn stale_pubkeys(
    pubkeys_state: &HashMap<String, OrderbookPubkeyState>,
    my_persistent_pubkey: &str,
    my_p2p_pubkeys: &HashSet<String>,
    timeout_secs: u64,
    now_secs: u64,
) -> (Vec<Uuid>, Vec<String>) {
    let mut uuids_to_remove = vec![];
    let mut pubkeys_to_remove = vec![];
    for (pubkey, state) in pubkeys_state.iter() {
        let is_own = pubkey == my_persistent_pubkey || my_p2p_pubkeys.contains(pubkey);
        if !is_own && state.last_keep_alive + timeout_secs <= now_secs {
            uuids_to_remove.extend(state.orders_uuids.iter().map(|(uuid, _)| *uuid));
            pubkeys_to_remove.push(pubkey.clone());
        }
    }
    (uuids_to_remove, pubkeys_to_remove)
}

/// Cancels the maker orders whose own timeout has elapsed (CRD ch.12 §12.8). The order's lock is held for the
/// announcement only; the deletion from storage runs after it is released (R7a).
pub(crate) async fn cancel_expired_maker_orders(ctx: &MmArc, ordermatch_ctx: &OrdermatchContext) {
    let expired = ordermatch_ctx.maker_orders_ctx.lock().drop_expired();
    for (uuid, order_mutex, gate) in expired {
        log::info!("Order '{}' is expired, cancelling", uuid);
        let expired = {
            let order = order_mutex.lock().await;
            maker_order_cancelled_p2p_notify(ctx.clone(), &order);
            RemovedMakerOrder::already_dropped(gate, &order)
        };
        expired.delete(ctx, MakerOrderCancellationReason::Expired).await;
    }
}

pub async fn lp_ordermatch_loop(ctx: MmArc) {
    let crypto_ctx = match CryptoCtx::from_ctx(&ctx) {
        Ok(c) => c,
        Err(_) => {
            // No signing identity (e.g. no-login mode) — ordermatch loop inactive.
            return;
        },
    };
    let my_pubsecp = crypto_ctx.mm2_internal_pubkey_hex();

    let maker_order_timeout = ctx.conf["maker_order_timeout"].as_u64().unwrap_or(MAKER_ORDER_TIMEOUT);
    loop {
        if ctx.is_stopping() {
            break;
        }
        let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();

        handle_timed_out_taker_orders(ctx.clone(), &ordermatch_ctx).await;
        handle_timed_out_maker_matches(ctx.clone(), &ordermatch_ctx).await;
        check_balance_for_maker_orders(ctx.clone(), &ordermatch_ctx).await;

        // Collect maker orders whose per-order timeout has elapsed (TimedMap TTL).
        cancel_expired_maker_orders(&ctx, &ordermatch_ctx).await;

        {
            // Remove "timed out" pubkeys states with their orders from orderbook.
            // Phase 1: identify stale pubkeys and their orders using trie_store,
            // then remove index entries under orderbook lock, collecting trie ops.
            // Lock order: `orderbook` first (cloned out and released), then `trie_store`,
            // matching `process_sync_pubkey_orderbook_state`; the two are never held together here.
            let my_p2p_pubkeys = ordermatch_ctx.orderbook.lock().my_p2p_pubkeys.clone();
            let (uuids_to_remove, pubkeys_to_remove) = {
                let trie_store = ordermatch_ctx.trie_store.lock();
                stale_pubkeys(
                    &trie_store.pubkeys_state,
                    &my_pubsecp,
                    &my_p2p_pubkeys,
                    maker_order_timeout,
                    now_ms() / 1000,
                )
            };

            {
                let mut ops = Vec::new();
                let mut orderbook = ordermatch_ctx.orderbook.lock();
                for uuid in uuids_to_remove {
                    if let Some((_item, op)) = orderbook.index_remove(uuid) {
                        ops.push(op);
                    }
                }
                for pubkey in &pubkeys_to_remove {
                    ops.push(TrieOp::RemovePubkey { pubkey: pubkey.clone() });
                }
                // Phase 2: queue the trie ops before the guard is released (CRD ch.32 R-T3b item 1)
                orderbook.queue_trie_ops(&ordermatch_ctx.trie_ops_tx, ops);
                collect_orderbook_metrics(&ctx, &orderbook);
            }
        }

        reconcile_missing_own_maker_orders(&ctx, &ordermatch_ctx).await;

        Timer::sleep(0.777).await;
    }
}

/// Period of the trie store maintenance. The diffs of a pair history live 300 s, so one that is past
/// its time to live is held at most a tenth longer than it has to.
const TRIE_MAINTENANCE_PERIOD_SECS: f64 = 30.;
/// The nodes no trie refers to any more are purged on every 20th tick, that is every 600 s.
const MEMORY_DB_PURGE_EVERY_TICKS: u64 = 20;

/// One round of the trie store maintenance (never awaits, so no lock is held across a suspension):
/// expire the histories, purge `memory_db` when `purge` is set, and report the gauges. The trie store
/// lock and the orderbook lock are taken one after the other, never together.
pub(crate) fn run_trie_store_maintenance(ctx: &MmArc, ordermatch_ctx: &OrdermatchContext, purge: bool) {
    let snapshot = {
        let mut trie_store = ordermatch_ctx.trie_store.lock();
        trie_store.expire_histories();
        if purge {
            trie_store.purge_memory_db();
        }
        // `memory_db` is measured on purge ticks only, right after the purge, where the table is small.
        trie_store.snapshot(purge)
    };
    // Read after `applied_batches` (see `collect_trie_store_metrics`).
    let enqueued_batches = ordermatch_ctx.orderbook.lock().trie_ops_enqueued;
    collect_trie_store_metrics(ctx, &snapshot, enqueued_batches);
    if purge {
        // Both locks are released here: `malloc_trim` can take milliseconds and must not delay the trie worker.
        release_freed_heap();
    }
}

/// Parses the resident set size in bytes out of the content of `/proc/self/statm` (the second field is the
/// number of resident pages).
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn parse_statm_rss_bytes(statm: &str, page_size: u64) -> Option<u64> {
    let resident_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    resident_pages.checked_mul(page_size)
}

/// The resident set size of this process in bytes, or `None` when it cannot be read.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn current_rss_bytes() -> Option<u64> {
    // SAFETY: `sysconf` has no preconditions.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let page_size = u64::try_from(page_size).ok().filter(|size| *size > 0)?;
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    parse_statm_rss_bytes(&statm, page_size)
}

/// After the purge the freed memory of the trie store is still held in the glibc arenas and counted in the
/// resident set of the process: hand it back to the system. The line with the resident set before and after is
/// logged when the resident set can be read.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn release_freed_heap() {
    let rss_before = current_rss_bytes();
    // SAFETY: `malloc_trim` has no preconditions, it may be called at any time from any thread.
    unsafe {
        libc::malloc_trim(0);
    }
    if let (Some(before), Some(after)) = (rss_before, current_rss_bytes()) {
        log::debug!("malloc_trim after trie store purge: rss {} -> {} bytes", before, after);
    }
}

/// Only glibc has `malloc_trim`: nothing to do elsewhere.
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn release_freed_heap() {}

/// The maintenance step for the own maker orders that are absent from the local orderbook (CRD ch.60 R60.4.6
/// item 5, ch.59 R59.6.7): reconcile each with a fresh balance read, then re-announce it or cancel it.
///
/// The reads (the pair lookup, the base coin's spendable balance and the maximum maker volume, which reads a
/// fee) take network round trips, so no order's lock is held across them: the lock is taken briefly to copy the
/// tickers, and again to apply the result. The balance of a base coin is read once per pass, however many missing
/// orders it has.
pub(crate) async fn reconcile_missing_own_maker_orders(ctx: &MmArc, ordermatch_ctx: &OrdermatchContext) {
    let mut missing_uuids = Vec::new();
    let mut to_cancel = Vec::new();
    {
        let orderbook = ordermatch_ctx.orderbook.lock();
        for (uuid, _) in ordermatch_ctx.maker_orders_ctx.lock().iter() {
            if !orderbook.order_set.contains_key(uuid) {
                missing_uuids.push(*uuid);
            }
        }
    }

    // What the reads need from each missing order, copied under its lock, which is released at once.
    let mut missing_orders = Vec::new();
    for uuid in missing_uuids {
        let order_mutex = match ordermatch_ctx.maker_orders_ctx.lock().get_order(&uuid) {
            Some(o) => o.clone(),
            None => continue,
        };
        let (base_ticker, rel_ticker) = {
            let order = order_mutex.lock().await;
            (order.base.clone(), order.rel.clone())
        };
        missing_orders.push((uuid, order_mutex, base_ticker, rel_ticker));
    }

    // The maximum maker volume of each base coin, or why it could not be had: the balance and the fee are read
    // once per base coin and pass, with no order lock held.
    let mut max_vols: HashMap<String, Result<MmNumber, String>> = HashMap::new();
    let mut reads = Vec::new();
    for (uuid, order_mutex, base_ticker, rel_ticker) in missing_orders {
        let (base, rel) = match find_pair(ctx, &base_ticker, &rel_ticker).await {
            Ok(Some(pair)) => pair,
            _ => continue,
        };
        if !max_vols.contains_key(&base_ticker) {
            let max_vol = match base.my_spendable_balance().compat().await {
                Ok(current_balance) => {
                    match calc_max_maker_vol(ctx, &base, &current_balance, FeeApproxStage::OrderIssue).await {
                        Ok(info) => Ok(info.volume),
                        Err(e) => Err(e.to_string()),
                    }
                },
                Err(e) => Err(e.to_string()),
            };
            max_vols.insert(base_ticker.clone(), max_vol);
        }
        reads.push((uuid, order_mutex, base_ticker, base, rel));
    }

    // Apply under the order's lock, to an order that is still there.
    for (uuid, order_mutex, base_ticker, base, rel) in reads {
        let max_vol = match max_vols.get(&base_ticker) {
            Some(Ok(max_vol)) => max_vol.clone(),
            Some(Err(e)) => {
                log::info!("Error {} on balance check to kickstart order {}, cancelling", e, uuid);
                to_cancel.push(uuid);
                continue;
            },
            None => continue,
        };

        let mut order = order_mutex.lock().await;
        let maker_orders = ordermatch_ctx.maker_orders_ctx.lock();
        // The order may have been cancelled or filled since it was read: then it is left alone and not announced.
        if !maker_orders.contains_key(&uuid) {
            continue;
        }

        if max_vol < order.available_amount() {
            order.max_base_vol = order.reserved_amount() + max_vol;
        }
        if order.available_amount() < order.min_base_vol {
            log::info!("Insufficient volume available for order {}, cancelling", uuid);
            to_cancel.push(uuid);
            continue;
        }

        // notify other nodes only if maker order is still there keeping maker_orders locked during the operation
        reannounce_missing_own_order(ctx, &order, base.coin_protocol_info(), rel.coin_protocol_info());
    }

    for uuid in to_cancel {
        let maybe_order_mutex = ordermatch_ctx.maker_orders_ctx.lock().get_order(&uuid);
        if let Some(order_mutex) = maybe_order_mutex {
            let cancelled = {
                let order = order_mutex.lock().await;
                // This checks that the order hasn't been removed by another process
                RemovedMakerOrder::take_out(ordermatch_ctx, &order)
            };
            if let Some(cancelled) = cancelled {
                cancelled
                    .delete(ctx, MakerOrderCancellationReason::InsufficientBalance)
                    .await;
            }
        }
    }
}

/// Keeps the memory of the trie store bounded independently of the traffic: histories expire on a
/// timer, not on the next update of their pair, and `memory_db` is purged and shrunk every 600 s.
pub async fn clean_memory_loop(ctx_weak: MmWeak) {
    let mut tick: u64 = 0;
    loop {
        {
            let ctx = match MmArc::from_weak(&ctx_weak) {
                Some(ctx) => ctx,
                None => return,
            };
            if ctx.is_stopping() {
                break;
            }

            let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
            run_trie_store_maintenance(&ctx, &ordermatch_ctx, tick.is_multiple_of(MEMORY_DB_PURGE_EVERY_TICKS));
        }
        tick += 1;
        Timer::sleep(TRIE_MAINTENANCE_PERIOD_SECS).await;
    }
}

/// Transforms the timed out and unmatched GTC taker orders to maker.
///
/// # Safety
///
/// The function locks the [`OrdermatchContext::maker_orders_ctx`] and [`OrdermatchContext::my_taker_orders`] mutexes.
pub(crate) async fn handle_timed_out_taker_orders(ctx: MmArc, ordermatch_ctx: &OrdermatchContext) {
    let mut my_taker_orders = ordermatch_ctx.my_taker_orders.lock().await;

    let storage = MyOrdersStorage::new(ctx.clone());
    let mut my_actual_taker_orders = HashMap::with_capacity(my_taker_orders.len());

    for (uuid, order) in my_taker_orders.drain() {
        if order.created_at + order.timeout * 1000 >= now_ms() {
            my_actual_taker_orders.insert(uuid, order);
            continue;
        }

        if !order.matches.is_empty() || order.order_type != OrderType::GoodTillCancelled {
            delete_my_taker_order(ctx.clone(), order, TakerOrderCancellationReason::TimedOut)
                .compat()
                .await
                .ok();
            continue;
        }

        // transform the timed out taker order to maker

        delete_my_taker_order(ctx.clone(), order.clone(), TakerOrderCancellationReason::ToMaker)
            .compat()
            .await
            .ok();
        let maker_order: MakerOrder = order.into();
        {
            let order_arc = Arc::new(AsyncMutex::new(maker_order.clone()));
            ordermatch_ctx
                .maker_orders_ctx
                .lock()
                .add_order(&maker_order, order_arc);
        }
        // CRD ch.60 R60.4.6 item 4: the watch of the base coin starts unseeded, no balance is read here.
        start_maker_balance_watch(&ctx, ordermatch_ctx, &maker_order.base, None);

        storage
            .save_new_active_maker_order(&maker_order)
            .await
            .error_log_with_msg("!save_new_active_maker_order");
        if maker_order.save_in_history {
            storage
                .update_was_taker_in_filtering_history(uuid)
                .await
                .error_log_with_msg("!update_was_taker_in_filtering_history");
        }

        // notify other peers
        if let Ok(Some((base, rel))) = find_pair(&ctx, &maker_order.base, &maker_order.rel).await {
            maker_order_created_p2p_notify(
                ctx.clone(),
                &maker_order,
                base.coin_protocol_info(),
                rel.coin_protocol_info(),
            );
        }
    }

    *my_taker_orders = my_actual_taker_orders;
}

/// # Safety
///
/// The function locks the [`OrdermatchContext::maker_orders_ctx`] mutex.
pub(crate) async fn check_balance_for_maker_orders(ctx: MmArc, ordermatch_ctx: &OrdermatchContext) {
    let my_maker_orders = ordermatch_ctx.maker_orders_ctx.lock().clone_orders();

    for (_, order) in my_maker_orders {
        let cancelled = {
            let order = order.lock().await;
            if order.available_amount() >= order.min_base_vol || order.has_ongoing_matches() {
                continue;
            }

            let reason = if order.matches.is_empty() {
                MakerOrderCancellationReason::InsufficientBalance
            } else {
                MakerOrderCancellationReason::Fulfilled
            };
            // This checks that the order hasn't been removed by another process
            let cancelled = RemovedMakerOrder::take_out(ordermatch_ctx, &order);
            if cancelled.is_some() {
                maker_order_cancelled_p2p_notify(ctx.clone(), &order);
            }
            cancelled.map(|cancelled| (cancelled, reason))
        };
        if let Some((cancelled, reason)) = cancelled {
            cancelled.delete(&ctx, reason).await;
        }
    }
}

/// Removes timed out unfinished matches to unlock the reserved amount.
///
/// # Safety
///
/// The function locks the [`OrdermatchContext::maker_orders_ctx`] mutex.
pub(crate) async fn handle_timed_out_maker_matches(ctx: MmArc, ordermatch_ctx: &OrdermatchContext) {
    let now = now_ms();
    let my_maker_orders = ordermatch_ctx.maker_orders_ctx.lock().clone_orders();

    for order in my_maker_orders.values() {
        // The state is published under the order's lock and stored after it is released (CRD ch.12 R7a, R7b).
        let published = {
            let mut order = order.lock().await;
            let old_len = order.matches.len();
            order.matches.retain(|_, order_match| {
                order_match.last_updated + ORDER_MATCH_TIMEOUT * 1000 > now || order_match.connected.is_some()
            });
            if old_len != order.matches.len() {
                PublishedState::publish(ordermatch_ctx, &order, false)
            } else {
                None
            }
        };
        if let Some(published) = published {
            published
                .complete(&ctx)
                .await
                .error_log_with_msg("!update_active_maker_order");
        }
    }
}

pub(crate) async fn process_maker_reserved(ctx: MmArc, from_pubkey: H256Json, reserved_msg: MakerReserved) {
    log::debug!("Processing MakerReserved {:?}", reserved_msg);
    let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
    {
        let my_taker_orders = ordermatch_ctx.my_taker_orders.lock().await;
        if !my_taker_orders.contains_key(&reserved_msg.taker_order_uuid) {
            return;
        }
    }

    let our_public_id = ctx.public_id().unwrap();
    if our_public_id.bytes == from_pubkey.0 {
        log::warn!("Skip maker reserved from our pubkey");
        return;
    }

    let uuid = reserved_msg.taker_order_uuid;
    {
        let mut pending_map = ordermatch_ctx.pending_maker_reserved.lock().await;
        let pending_for_order = pending_map
            .entry(reserved_msg.taker_order_uuid)
            .or_insert_with(Vec::new);
        pending_for_order.push(reserved_msg);
        if pending_for_order.len() > 1 {
            // messages will be sorted by price and processed in the first called handler
            return;
        }
    }

    Timer::sleep(3.).await;

    let mut my_taker_orders = ordermatch_ctx.my_taker_orders.lock().await;
    let my_order = match my_taker_orders.entry(uuid) {
        Entry::Vacant(_) => return,
        Entry::Occupied(entry) => entry.into_mut(),
    };

    // our base and rel coins should match maker's side tickers for a proper is_coin_protocol_supported check
    let (base_coin, rel_coin) = match find_pair(&ctx, my_order.maker_coin_ticker(), my_order.taker_coin_ticker()).await
    {
        Ok(Some(c)) => c,
        _ => return, // attempt to match with deactivated coin
    };
    let mut pending_map = ordermatch_ctx.pending_maker_reserved.lock().await;
    if let Some(mut reserved_messages) = pending_map.remove(&uuid) {
        reserved_messages.sort_unstable_by_key(|r| r.price());

        for reserved_msg in reserved_messages {
            // send "connect" message if reserved message targets our pubkey AND
            // reserved amounts match our order AND order is NOT reserved by someone else (empty matches)
            if (my_order.match_reserved(&reserved_msg) == MatchReservedResult::Matched && my_order.matches.is_empty())
                && base_coin.is_coin_protocol_supported(&reserved_msg.base_protocol_info)
                && rel_coin.is_coin_protocol_supported(&reserved_msg.rel_protocol_info)
            {
                let connect = TakerConnect {
                    sender_pubkey: H256Json::from(our_public_id.bytes),
                    dest_pub_key: reserved_msg.sender_pubkey,
                    taker_order_uuid: reserved_msg.taker_order_uuid,
                    maker_order_uuid: reserved_msg.maker_order_uuid,
                };
                let topic = my_order.orderbook_topic();
                broadcast_ordermatch_message(&ctx, vec![topic], connect.clone().into(), my_order.p2p_keypair());
                let taker_match = TakerMatch {
                    reserved: reserved_msg,
                    connect,
                    connected: None,
                    last_updated: now_ms(),
                };
                my_order
                    .matches
                    .insert(taker_match.reserved.maker_order_uuid, taker_match.clone());
                let _ = ctx.event_stream_manager.send_fn(&StreamerId::OrderStatus, || {
                    order_events::OrderStatusEvent::TakerMatch(taker_match)
                });
                MyOrdersStorage::new(ctx)
                    .update_active_taker_order(my_order)
                    .await
                    .error_log_with_msg("!update_active_taker_order");
                return;
            }
        }
    }
}

pub(crate) async fn process_maker_connected(ctx: MmArc, from_pubkey: H256Json, connected: MakerConnected) {
    log::debug!("Processing MakerConnected {:?}", connected);
    let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
    let our_public_id = ctx.public_id().unwrap();
    if our_public_id.bytes == from_pubkey.0 {
        log::warn!("Skip maker connected from our pubkey");
        return;
    }

    let mut my_taker_orders = ordermatch_ctx.my_taker_orders.lock().await;
    let my_order_entry = match my_taker_orders.entry(connected.taker_order_uuid) {
        Entry::Occupied(e) => e,
        Entry::Vacant(_) => return,
    };
    let order_match = match my_order_entry.get().matches.get(&connected.maker_order_uuid) {
        Some(o) => o,
        None => {
            log::warn!(
                "Our node doesn't have the match with uuid {}",
                connected.maker_order_uuid
            );
            return;
        },
    };

    if order_match.reserved.sender_pubkey != from_pubkey {
        log::error!("Connected message sender pubkey != reserved message sender pubkey");
        return;
    }
    // alice
    let _ = ctx.event_stream_manager.send_fn(&StreamerId::OrderStatus, || {
        order_events::OrderStatusEvent::TakerConnected(order_match.clone())
    });
    lp_connected_alice(ctx.clone(), my_order_entry.get().clone(), order_match.clone());
    // remove the matched order immediately
    let order = my_order_entry.remove();
    delete_my_taker_order(ctx, order, TakerOrderCancellationReason::Fulfilled)
        .compat()
        .await
        .ok();
}

pub(crate) async fn process_taker_request(ctx: MmArc, from_pubkey: H256Json, taker_request: TakerRequest) {
    let our_public_id: H256Json = ctx.public_id().unwrap().bytes.into();
    if our_public_id == from_pubkey {
        log::warn!("Skip the request originating from our pubkey");
        return;
    }
    log::debug!("Processing request {:?}", taker_request);

    if !taker_request.can_match_with_maker_pubkey(&our_public_id) {
        return;
    }

    let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
    let mut my_orders = ordermatch_ctx.maker_orders_ctx.lock().clone_orders();
    let filtered = my_orders
        .iter_mut()
        .filter(|(uuid, _)| taker_request.can_match_with_uuid(uuid));

    for (uuid, order) in filtered {
        let mut order = order.lock().await;
        if let OrderMatchResult::Matched((base_amount, rel_amount)) = order.match_with_request(&taker_request) {
            let (base_coin, rel_coin) = match find_pair(&ctx, &order.base, &order.rel).await {
                Ok(Some(c)) => c,
                _ => return, // attempt to match with deactivated coin
            };

            if !order.matches.contains_key(&taker_request.uuid)
                && base_coin.is_coin_protocol_supported(taker_request.base_protocol_info_for_maker())
                && rel_coin.is_coin_protocol_supported(taker_request.rel_protocol_info_for_maker())
            {
                let reserved = MakerReserved {
                    dest_pub_key: taker_request.sender_pubkey,
                    sender_pubkey: our_public_id,
                    base: order.base_orderbook_ticker().to_owned(),
                    base_amount: base_amount.clone(),
                    rel_amount: rel_amount.clone(),
                    rel: order.rel_orderbook_ticker().to_owned(),
                    taker_order_uuid: taker_request.uuid,
                    maker_order_uuid: *uuid,
                    conf_settings: order.conf_settings.or_else(|| {
                        Some(OrderConfirmationsSettings {
                            base_confs: base_coin.required_confirmations(),
                            base_nota: base_coin.requires_notarization(),
                            rel_confs: rel_coin.required_confirmations(),
                            rel_nota: rel_coin.requires_notarization(),
                        })
                    }),
                    base_protocol_info: Some(base_coin.coin_protocol_info()),
                    rel_protocol_info: Some(rel_coin.coin_protocol_info()),
                    swap_version: order.swap_version,
                };
                let topic = order.orderbook_topic();
                log::debug!("Request matched sending reserved {:?}", reserved);
                broadcast_ordermatch_message(&ctx, vec![topic], reserved.clone().into(), order.p2p_keypair());
                let maker_match = MakerMatch {
                    request: taker_request,
                    reserved,
                    connect: None,
                    connected: None,
                    last_updated: now_ms(),
                };
                order.matches.insert(maker_match.request.uuid, maker_match.clone());
                let _ = ctx.event_stream_manager.send_fn(&StreamerId::OrderStatus, || {
                    order_events::OrderStatusEvent::MakerMatch(maker_match)
                });
                store_negotiated_maker_order(&ctx, &ordermatch_ctx, &order).await;
            }
            return;
        }
    }
}

pub(crate) async fn process_taker_connect(ctx: MmArc, sender_pubkey: H256Json, connect_msg: TakerConnect) {
    log::debug!("Processing TakerConnect {:?}", connect_msg);
    let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
    let our_public_id = ctx.public_id().unwrap();
    if our_public_id.bytes == sender_pubkey.0 {
        log::warn!("Skip taker connect from our pubkey");
        return;
    }

    let order_mutex = {
        match ordermatch_ctx
            .maker_orders_ctx
            .lock()
            .get_order(&connect_msg.maker_order_uuid)
        {
            Some(o) => o,
            None => return,
        }
    };
    let mut my_order = order_mutex.lock().await;
    let order_match = match my_order.matches.get_mut(&connect_msg.taker_order_uuid) {
        Some(o) => o,
        None => {
            log::warn!(
                "Our node doesn't have the match with uuid {}",
                connect_msg.taker_order_uuid
            );
            return;
        },
    };
    if order_match.request.sender_pubkey != sender_pubkey {
        log::warn!("Connect message sender pubkey != request message sender pubkey");
        return;
    }

    if order_match.connected.is_none() && order_match.connect.is_none() {
        let connected = MakerConnected {
            sender_pubkey: our_public_id.bytes.into(),
            dest_pub_key: connect_msg.sender_pubkey,
            taker_order_uuid: connect_msg.taker_order_uuid,
            maker_order_uuid: connect_msg.maker_order_uuid,
            method: "connected".into(),
        };
        order_match.connect = Some(connect_msg);
        order_match.connected = Some(connected.clone());
        let order_match = order_match.clone();
        let _ = ctx.event_stream_manager.send_fn(&StreamerId::OrderStatus, || {
            order_events::OrderStatusEvent::MakerConnected(order_match.clone())
        });
        my_order.started_swaps.push(order_match.request.uuid);
        lp_connect_start_bob(ctx.clone(), order_match, my_order.clone());
        let topic = my_order.orderbook_topic();
        broadcast_ordermatch_message(&ctx, vec![topic.clone()], connected.into(), my_order.p2p_keypair());

        // If volume is less order will be cancelled a bit later
        if my_order.available_amount() >= my_order.min_base_vol {
            let mut updated_msg = new_protocol::MakerOrderUpdated::new(my_order.uuid);
            updated_msg.with_new_max_volume(my_order.available_amount().into());
            maker_order_updated_p2p_notify(ctx.clone(), topic, updated_msg, my_order.p2p_keypair());
        }
        store_negotiated_maker_order(&ctx, &ordermatch_ctx, &my_order).await;
    }
}

/// Stores the state of an order that a negotiation message has changed, with the order's lock still held by the
/// caller (CRD ch.12 R7a: the negotiation paths are outside the rule). The write passes through the order's
/// gate like every other, so it takes its place in the order of the order's changes (R7b).
async fn store_negotiated_maker_order(ctx: &MmArc, ordermatch_ctx: &OrdermatchContext, order: &MakerOrder) {
    if let Some(published) = PublishedState::publish(ordermatch_ctx, order, false) {
        published
            .complete(ctx)
            .await
            .error_log_with_msg("!update_active_maker_order");
    }
}

#[derive(Deserialize, Debug)]
pub struct AutoBuyInput {
    pub(crate) base: String,
    pub(crate) rel: String,
    pub(crate) price: MmNumber,
    pub(crate) volume: MmNumber,
    pub(crate) timeout: Option<u64>,
    /// Not used. Deprecated.
    #[allow(dead_code)]
    pub(crate) duration: Option<u32>,
    // TODO: remove this field on API refactoring, method should be separated from params
    pub(crate) method: String,
    #[allow(dead_code)]
    pub(crate) gui: Option<String>,
    #[serde(rename = "destpubkey")]
    #[serde(default)]
    #[allow(dead_code)]
    pub(crate) dest_pub_key: H256Json,
    #[serde(default)]
    pub(crate) match_by: MatchBy,
    #[serde(default)]
    pub(crate) order_type: OrderType,
    pub(crate) base_confs: Option<u64>,
    pub(crate) base_nota: Option<bool>,
    pub(crate) rel_confs: Option<u64>,
    pub(crate) rel_nota: Option<bool>,
    pub(crate) min_volume: Option<MmNumber>,
    #[serde(default = "get_true")]
    pub(crate) save_in_history: bool,
}

#[derive(Deserialize)]
pub struct StartSwapRequest {
    base: String,
    rel: String,
    base_coin_amount: MmNumber,
    rel_coin_amount: MmNumber,
    method: StartSwapMethod,
    #[serde(default)]
    dest_pubkey: Option<H256Json>,
    #[serde(default)]
    dest_pub_key: Option<H256Json>,
    #[serde(default)]
    match_by: MatchBy,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum StartSwapMethod {
    SetPrice {},
    Buy {},
    Sell {},
}

#[derive(Serialize)]
pub struct StartSwapResponse {
    uuid: Uuid,
    status: &'static str,
    swap_type: &'static str,
}

#[derive(Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum StartSwapError {
    #[display(fmt = "Invalid start_swap request: {}", _0)]
    InvalidRequest(String),
    #[display(fmt = "Internal error: {}", _0)]
    Internal(String),
}

impl HttpStatusCode for StartSwapError {
    fn status_code(&self) -> StatusCode {
        match self {
            StartSwapError::InvalidRequest(_) => StatusCode::BAD_REQUEST,
            StartSwapError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

fn start_swap_price(base_amount: &MmNumber, rel_amount: &MmNumber) -> MmResult<MmNumber, StartSwapError> {
    if base_amount.is_zero() {
        return MmError::err(StartSwapError::InvalidRequest(
            "base_coin_amount must be greater than zero".to_owned(),
        ));
    }
    Ok(rel_amount / base_amount)
}

fn start_swap_dest_pubkey(req: &StartSwapRequest) -> H256Json {
    req.dest_pubkey.or(req.dest_pub_key).unwrap_or_default()
}

#[cfg(test)]
mod start_swap_tests {
    use super::*;

    #[test]
    fn test_start_swap_accepts_flutter_sell_payload() {
        let req: StartSwapRequest = json::from_value(json!({
            "base": "RIN",
            "rel": "KMD",
            "base_coin_amount": "0.001",
            "rel_coin_amount": "0.00454545",
            "method": {
                "sell": {}
            }
        }))
        .unwrap();

        assert!(matches!(req.method, StartSwapMethod::Sell {}));
        let price = match start_swap_price(&req.base_coin_amount, &req.rel_coin_amount) {
            Ok(price) => price,
            Err(e) => panic!("{}", e),
        };
        assert_eq!(price, MmNumber::from("4.54545"));
    }

    #[test]
    fn test_start_swap_rejects_zero_base_amount() {
        let err = start_swap_price(&MmNumber::from("0"), &MmNumber::from("0.00454545")).unwrap_err();
        assert!(matches!(err.into_inner(), StartSwapError::InvalidRequest(_)));
    }
}

pub async fn start_swap_rpc(ctx: MmArc, req: StartSwapRequest) -> MmResult<StartSwapResponse, StartSwapError> {
    let price = start_swap_price(&req.base_coin_amount, &req.rel_coin_amount)?;
    match req.method {
        StartSwapMethod::SetPrice {} => {
            let maker_order = create_maker_order(&ctx, SetPriceReq {
                base: req.base,
                rel: req.rel,
                price,
                max: false,
                volume: req.base_coin_amount,
                min_volume: None,
                cancel_previous: true,
                base_confs: None,
                base_nota: None,
                rel_confs: None,
                rel_nota: None,
                save_in_history: true,
                timeout_in_minutes: None,
            })
            .await
            .map_to_mm(StartSwapError::Internal)?;
            Ok(StartSwapResponse {
                uuid: maker_order.uuid,
                status: "Created",
                swap_type: "Maker",
            })
        },
        StartSwapMethod::Buy {} | StartSwapMethod::Sell {} => {
            let method = match req.method {
                StartSwapMethod::Buy {} => "buy",
                StartSwapMethod::Sell {} => "sell",
                StartSwapMethod::SetPrice {} => unreachable!(),
            };
            let response = match method {
                "buy" => {
                    buy(
                        ctx,
                        json!({
                            "base": req.base,
                            "rel": req.rel,
                            "price": price,
                            "volume": req.base_coin_amount,
                            "method": method,
                            "destpubkey": start_swap_dest_pubkey(&req),
                            "match_by": req.match_by,
                        }),
                    )
                    .await
                },
                "sell" => {
                    sell(
                        ctx,
                        json!({
                            "base": req.base,
                            "rel": req.rel,
                            "price": price,
                            "volume": req.base_coin_amount,
                            "method": method,
                            "destpubkey": start_swap_dest_pubkey(&req),
                            "match_by": req.match_by,
                        }),
                    )
                    .await
                },
                _ => unreachable!(),
            }
            .map_to_mm(StartSwapError::Internal)?;
            let body: Json =
                json::from_slice(response.body()).map_to_mm(|e| StartSwapError::Internal(e.to_string()))?;
            let uuid = json::from_value(body["result"]["uuid"].clone())
                .map_to_mm(|e| StartSwapError::Internal(e.to_string()))?;
            Ok(StartSwapResponse {
                uuid,
                status: "Created",
                swap_type: "Taker",
            })
        },
    }
}

pub async fn buy(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let scope_ctx = ctx.clone();
    with_request_fee_scope(&scope_ctx, buy_impl(ctx, req)).await
}

async fn buy_impl(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let input: AutoBuyInput = try_s!(json::from_value(req));
    if input.base == input.rel {
        return ERR!("Base and rel must be different coins");
    }
    let rel_coin = try_s!(lp_coinfind(&ctx, &input.rel).await);
    let rel_coin = try_s!(rel_coin.ok_or("Rel coin is not found or inactive"));
    let base_coin = try_s!(lp_coinfind(&ctx, &input.base).await);
    let base_coin: MmCoinEnum = try_s!(base_coin.ok_or("Base coin is not found or inactive"));
    if let Some(coin) = untradable_coin_ticker(&base_coin, &rel_coin) {
        return ERR!("Trading is not supported for coin {}", coin);
    }
    if base_coin.wallet_only(&ctx) {
        return ERR!("Base coin {} is wallet only", input.base);
    }
    if rel_coin.wallet_only(&ctx) {
        return ERR!("Rel coin {} is wallet only", input.rel);
    }
    let my_amount = &input.volume * &input.price;
    try_s!(
        check_balance_for_taker_swap(
            &ctx,
            &rel_coin,
            &base_coin,
            my_amount,
            None,
            None,
            FeeApproxStage::OrderIssue
        )
        .await
    );
    let res = try_s!(lp_auto_buy(&ctx, &base_coin, &rel_coin, input).await).into_bytes();
    Ok(try_s!(Response::builder().body(res)))
}

pub async fn sell(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let scope_ctx = ctx.clone();
    with_request_fee_scope(&scope_ctx, sell_impl(ctx, req)).await
}

async fn sell_impl(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let input: AutoBuyInput = try_s!(json::from_value(req));
    if input.base == input.rel {
        return ERR!("Base and rel must be different coins");
    }
    let base_coin = try_s!(lp_coinfind(&ctx, &input.base).await);
    let base_coin = try_s!(base_coin.ok_or("Base coin is not found or inactive"));
    let rel_coin = try_s!(lp_coinfind(&ctx, &input.rel).await);
    let rel_coin = try_s!(rel_coin.ok_or("Rel coin is not found or inactive"));
    if let Some(coin) = untradable_coin_ticker(&base_coin, &rel_coin) {
        return ERR!("Trading is not supported for coin {}", coin);
    }
    if base_coin.wallet_only(&ctx) {
        return ERR!("Base coin {} is wallet only", input.base);
    }
    if rel_coin.wallet_only(&ctx) {
        return ERR!("Rel coin {} is wallet only", input.rel);
    }
    try_s!(
        check_balance_for_taker_swap(
            &ctx,
            &base_coin,
            &rel_coin,
            input.volume.clone(),
            None,
            None,
            FeeApproxStage::OrderIssue
        )
        .await
    );
    let res = try_s!(lp_auto_buy(&ctx, &base_coin, &rel_coin, input).await).into_bytes();
    Ok(try_s!(Response::builder().body(res)))
}

/// Created when maker order is matched with taker request
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct MakerMatch {
    pub(crate) request: TakerRequest,
    pub(crate) reserved: MakerReserved,
    pub(crate) connect: Option<TakerConnect>,
    pub(crate) connected: Option<MakerConnected>,
    pub(crate) last_updated: u64,
}

/// Created upon taker request broadcast
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct TakerMatch {
    pub(crate) reserved: MakerReserved,
    pub(crate) connect: TakerConnect,
    pub(crate) connected: Option<MakerConnected>,
    pub(crate) last_updated: u64,
}

impl<'a> From<&'a TakerRequest> for TakerRequestForRpc<'a> {
    fn from(request: &'a TakerRequest) -> TakerRequestForRpc<'a> {
        TakerRequestForRpc {
            base: &request.base,
            rel: &request.rel,
            base_amount: request.base_amount.to_decimal(),
            base_amount_rat: request.base_amount.to_ratio(),
            rel_amount: request.rel_amount.to_decimal(),
            rel_amount_rat: request.rel_amount.to_ratio(),
            action: &request.action,
            uuid: &request.uuid,
            method: "request".to_string(),
            sender_pubkey: &request.sender_pubkey,
            dest_pub_key: &request.dest_pub_key,
            match_by: &request.match_by,
            conf_settings: &request.conf_settings,
        }
    }
}

construct_detailed!(DetailedMinVolume, min_volume);

#[derive(Serialize)]
pub(crate) struct LpautobuyResult<'a> {
    #[serde(flatten)]
    pub(crate) request: TakerRequestForRpc<'a>,
    pub(crate) order_type: &'a OrderType,
    #[serde(flatten)]
    pub(crate) min_volume: DetailedMinVolume,
    pub(crate) base_orderbook_ticker: &'a Option<String>,
    pub(crate) rel_orderbook_ticker: &'a Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TakerRequestForRpc<'a> {
    pub(crate) base: &'a str,
    pub(crate) rel: &'a str,
    pub(crate) base_amount: BigDecimal,
    pub(crate) base_amount_rat: BigRational,
    pub(crate) rel_amount: BigDecimal,
    pub(crate) rel_amount_rat: BigRational,
    pub(crate) action: &'a TakerAction,
    pub(crate) uuid: &'a Uuid,
    pub(crate) method: String,
    pub(crate) sender_pubkey: &'a H256Json,
    pub(crate) dest_pub_key: &'a H256Json,
    pub(crate) match_by: &'a MatchBy,
    pub(crate) conf_settings: &'a Option<OrderConfirmationsSettings>,
}

#[allow(clippy::needless_borrow)]
pub async fn lp_auto_buy(
    ctx: &MmArc,
    base_coin: &MmCoinEnum,
    rel_coin: &MmCoinEnum,
    input: AutoBuyInput,
) -> Result<String, String> {
    if input.price < MmNumber::from(BigRational::new(1.into(), 100_000_000.into())) {
        return ERR!("Price is too low, minimum is 0.00000001");
    }

    let action = match Some(input.method.as_ref()) {
        Some("buy") => TakerAction::Buy,
        Some("sell") => TakerAction::Sell,
        _ => return ERR!("Auto buy must be called only from buy/sell RPC methods"),
    };
    // CRD ch.60 R60.6.7 item 3: the smallest trade the order can produce must be spendable on both legs.
    try_s!(check_pre_trade_spend_safety(ctx, base_coin, rel_coin, input.min_volume.clone(), &input.price).await);
    let ordermatch_ctx = try_s!(OrdermatchContext::from_ctx(ctx));
    let mut my_taker_orders = ordermatch_ctx.my_taker_orders.lock().await;
    let our_public_id = try_s!(ctx.public_id());
    let rel_volume = &input.volume * &input.price;
    let conf_settings = OrderConfirmationsSettings {
        base_confs: input.base_confs.unwrap_or_else(|| base_coin.required_confirmations()),
        base_nota: input.base_nota.unwrap_or_else(|| base_coin.requires_notarization()),
        rel_confs: input.rel_confs.unwrap_or_else(|| rel_coin.required_confirmations()),
        rel_nota: input.rel_nota.unwrap_or_else(|| rel_coin.requires_notarization()),
    };
    let mut order_builder = TakerOrderBuilder::new(base_coin, rel_coin)
        .with_base_amount(input.volume)
        .with_rel_amount(rel_volume)
        .with_action(action)
        .with_match_by(input.match_by)
        .with_min_volume(input.min_volume)
        .with_order_type(input.order_type)
        .with_conf_settings(conf_settings)
        .with_sender_pubkey(H256Json::from(our_public_id.bytes))
        .with_save_in_history(input.save_in_history)
        .with_base_orderbook_ticker(ordermatch_ctx.orderbook_ticker(base_coin.ticker()))
        .with_rel_orderbook_ticker(ordermatch_ctx.orderbook_ticker(rel_coin.ticker()));
    if let Some(timeout) = input.timeout {
        order_builder = order_builder.with_timeout(timeout);
    }
    let order = try_s!(order_builder.build());

    let request_orderbook = false;
    try_s!(
        subscribe_to_orderbook_topic(
            ctx,
            order.base_orderbook_ticker(),
            order.rel_orderbook_ticker(),
            request_orderbook
        )
        .await
    );
    broadcast_ordermatch_message(
        ctx,
        vec![order.orderbook_topic()],
        order.clone().into(),
        order.p2p_keypair(),
    );

    let result = json!({ "result": LpautobuyResult {
        request: (&order.request).into(),
        order_type: &order.order_type,
        min_volume: order.min_volume.clone().into(),
        base_orderbook_ticker: &order.base_orderbook_ticker,
        rel_orderbook_ticker: &order.rel_orderbook_ticker,
    } });

    save_my_new_taker_order(ctx.clone(), &order)
        .await
        .map_err(|e| ERRL!("{}", e))?;
    my_taker_orders.insert(order.request.uuid, order);
    Ok(result.to_string())
}

#[derive(Deserialize)]
pub struct SetPriceReq {
    pub(crate) base: String,
    pub(crate) rel: String,
    pub(crate) price: MmNumber,
    #[serde(default)]
    pub(crate) max: bool,
    #[serde(default)]
    pub(crate) volume: MmNumber,
    pub(crate) min_volume: Option<MmNumber>,
    #[serde(default = "get_true")]
    pub(crate) cancel_previous: bool,
    pub(crate) base_confs: Option<u64>,
    pub(crate) base_nota: Option<bool>,
    pub(crate) rel_confs: Option<u64>,
    pub(crate) rel_nota: Option<bool>,
    #[serde(default = "get_true")]
    pub(crate) save_in_history: bool,
    /// Optional per-order timeout in minutes.  When set the order will be
    /// automatically cancelled once the TTL elapses.
    pub(crate) timeout_in_minutes: Option<u16>,
}

#[derive(Deserialize)]
pub struct MakerOrderUpdateReq {
    pub(crate) uuid: Uuid,
    pub(crate) new_price: Option<MmNumber>,
    pub(crate) max: Option<bool>,
    pub(crate) volume_delta: Option<MmNumber>,
    pub(crate) min_volume: Option<MmNumber>,
    pub(crate) base_confs: Option<u64>,
    pub(crate) base_nota: Option<bool>,
    pub(crate) rel_confs: Option<u64>,
    pub(crate) rel_nota: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct MakerReservedForRpc<'a> {
    pub(crate) base: &'a str,
    pub(crate) rel: &'a str,
    pub(crate) base_amount: BigDecimal,
    pub(crate) base_amount_rat: BigRational,
    pub(crate) rel_amount: BigDecimal,
    pub(crate) rel_amount_rat: BigRational,
    pub(crate) taker_order_uuid: &'a Uuid,
    pub(crate) maker_order_uuid: &'a Uuid,
    pub(crate) sender_pubkey: &'a H256Json,
    pub(crate) dest_pub_key: &'a H256Json,
    pub(crate) conf_settings: &'a Option<OrderConfirmationsSettings>,
    pub(crate) method: String,
}

#[derive(Debug, Serialize)]
pub struct TakerConnectForRpc<'a> {
    pub(crate) taker_order_uuid: &'a Uuid,
    pub(crate) maker_order_uuid: &'a Uuid,
    pub(crate) method: String,
    pub(crate) sender_pubkey: &'a H256Json,
    pub(crate) dest_pub_key: &'a H256Json,
}

impl<'a> From<&'a TakerConnect> for TakerConnectForRpc<'a> {
    fn from(connect: &'a TakerConnect) -> TakerConnectForRpc<'a> {
        TakerConnectForRpc {
            taker_order_uuid: &connect.taker_order_uuid,
            maker_order_uuid: &connect.maker_order_uuid,
            method: "connect".to_string(),
            sender_pubkey: &connect.sender_pubkey,
            dest_pub_key: &connect.dest_pub_key,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct MakerConnectedForRpc<'a> {
    pub(crate) taker_order_uuid: &'a Uuid,
    pub(crate) maker_order_uuid: &'a Uuid,
    pub(crate) method: String,
    pub(crate) sender_pubkey: &'a H256Json,
    pub(crate) dest_pub_key: &'a H256Json,
}

impl<'a> From<&'a MakerConnected> for MakerConnectedForRpc<'a> {
    fn from(connected: &'a MakerConnected) -> MakerConnectedForRpc<'a> {
        MakerConnectedForRpc {
            taker_order_uuid: &connected.taker_order_uuid,
            maker_order_uuid: &connected.maker_order_uuid,
            method: "connected".to_string(),
            sender_pubkey: &connected.sender_pubkey,
            dest_pub_key: &connected.dest_pub_key,
        }
    }
}

impl<'a> From<&'a MakerReserved> for MakerReservedForRpc<'a> {
    fn from(reserved: &MakerReserved) -> MakerReservedForRpc<'_> {
        MakerReservedForRpc {
            base: &reserved.base,
            rel: &reserved.rel,
            base_amount: reserved.base_amount.to_decimal(),
            base_amount_rat: reserved.base_amount.to_ratio(),
            rel_amount: reserved.rel_amount.to_decimal(),
            rel_amount_rat: reserved.rel_amount.to_ratio(),
            taker_order_uuid: &reserved.taker_order_uuid,
            maker_order_uuid: &reserved.maker_order_uuid,
            sender_pubkey: &reserved.sender_pubkey,
            dest_pub_key: &reserved.dest_pub_key,
            conf_settings: &reserved.conf_settings,
            method: "reserved".to_string(),
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct MakerMatchForRpc<'a> {
    pub(crate) request: TakerRequestForRpc<'a>,
    pub(crate) reserved: MakerReservedForRpc<'a>,
    pub(crate) connect: Option<TakerConnectForRpc<'a>>,
    pub(crate) connected: Option<MakerConnectedForRpc<'a>>,
    pub(crate) last_updated: u64,
}

#[allow(clippy::needless_borrow)]
impl<'a> From<&'a MakerMatch> for MakerMatchForRpc<'a> {
    fn from(maker_match: &'a MakerMatch) -> MakerMatchForRpc<'a> {
        MakerMatchForRpc {
            request: (&maker_match.request).into(),
            reserved: (&maker_match.reserved).into(),
            connect: maker_match.connect.as_ref().map(Into::into),
            connected: maker_match.connected.as_ref().map(Into::into),
            last_updated: maker_match.last_updated,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct MakerOrderForRpc<'a> {
    pub(crate) base: &'a str,
    pub(crate) rel: &'a str,
    pub(crate) price: RenderedDecimal,
    pub(crate) price_rat: &'a MmNumber,
    pub(crate) max_base_vol: RenderedDecimal,
    pub(crate) max_base_vol_rat: &'a MmNumber,
    pub(crate) min_base_vol: RenderedDecimal,
    pub(crate) min_base_vol_rat: &'a MmNumber,
    pub(crate) created_at: u64,
    pub(crate) updated_at: Option<u64>,
    pub(crate) matches: HashMap<Uuid, MakerMatchForRpc<'a>>,
    pub(crate) started_swaps: &'a [Uuid],
    pub(crate) uuid: Uuid,
    pub(crate) conf_settings: &'a Option<OrderConfirmationsSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) changes_history: &'a Option<Vec<HistoricalOrder>>,
    pub(crate) base_orderbook_ticker: &'a Option<String>,
    pub(crate) rel_orderbook_ticker: &'a Option<String>,
}

impl<'a> From<&'a MakerOrder> for MakerOrderForRpc<'a> {
    fn from(order: &'a MakerOrder) -> MakerOrderForRpc<'a> {
        MakerOrderForRpc {
            base: &order.base,
            rel: &order.rel,
            price: order.price.to_rendered_decimal(),
            price_rat: &order.price,
            max_base_vol: order.max_base_vol.to_rendered_decimal(),
            max_base_vol_rat: &order.max_base_vol,
            min_base_vol: order.min_base_vol.to_rendered_decimal(),
            min_base_vol_rat: &order.min_base_vol,
            created_at: order.created_at,
            updated_at: order.updated_at,
            matches: order
                .matches
                .iter()
                .map(|(uuid, order_match)| (*uuid, order_match.into()))
                .collect(),
            started_swaps: &order.started_swaps,
            uuid: order.uuid,
            conf_settings: &order.conf_settings,
            changes_history: &order.changes_history,
            base_orderbook_ticker: &order.base_orderbook_ticker,
            rel_orderbook_ticker: &order.rel_orderbook_ticker,
        }
    }
}

/// Cancels the orders in case of error on different checks
/// https://github.com/KomodoPlatform/atomicDEX-API/issues/794
pub(crate) async fn cancel_orders_on_error<T, E>(ctx: &MmArc, req: &SetPriceReq, error: E) -> Result<T, E> {
    if req.cancel_previous {
        let ordermatch_ctx = OrdermatchContext::from_ctx(ctx).unwrap();
        cancel_previous_maker_orders(ctx, &ordermatch_ctx, &req.base, &req.rel).await;
    }
    Err(error)
}

pub(crate) async fn get_max_volume(
    ctx: &MmArc,
    my_coin: &MmCoinEnum,
    other_coin: &MmCoinEnum,
) -> Result<MmNumber, String> {
    get_max_volume_and_balance(ctx, my_coin, other_coin)
        .await
        .map(|(volume, _balance)| volume)
}

/// The maximum maker volume together with the spendable balance of `my_coin` that it was computed from.
async fn get_max_volume_and_balance(
    ctx: &MmArc,
    my_coin: &MmCoinEnum,
    other_coin: &MmCoinEnum,
) -> Result<(MmNumber, BigDecimal), String> {
    let my_balance = try_s!(my_coin.my_spendable_balance().compat().await);
    // first check if `rel_coin` balance is sufficient
    let other_coin_trade_fee = try_s!(
        other_coin
            .get_receiver_trade_fee(FeeApproxStage::OrderIssue)
            .compat()
            .await
    );
    try_s!(check_other_coin_balance_for_swap(ctx, other_coin, None, other_coin_trade_fee).await);
    // calculate max maker volume
    // note the `calc_max_maker_vol` returns [`CheckBalanceError::NotSufficientBalance`] error if the balance of `base_coin` is not sufficient
    let info = try_s!(calc_max_maker_vol(ctx, my_coin, &my_balance, FeeApproxStage::OrderIssue).await);
    Ok((info.volume, my_balance))
}

pub async fn create_maker_order(ctx: &MmArc, req: SetPriceReq) -> Result<MakerOrder, String> {
    with_request_fee_scope(ctx, create_maker_order_impl(ctx, req)).await
}

async fn create_maker_order_impl(ctx: &MmArc, req: SetPriceReq) -> Result<MakerOrder, String> {
    let base_coin: MmCoinEnum = match try_s!(lp_coinfind(ctx, &req.base).await) {
        Some(coin) => coin,
        None => return ERR!("Base coin {} is not found", req.base),
    };

    let rel_coin: MmCoinEnum = match try_s!(lp_coinfind(ctx, &req.rel).await) {
        Some(coin) => coin,
        None => return ERR!("Rel coin {} is not found", req.rel),
    };

    if let Some(coin) = untradable_coin_ticker(&base_coin, &rel_coin) {
        return ERR!("Trading is not supported for coin {}", coin);
    }
    if base_coin.wallet_only(ctx) {
        return ERR!("Base coin {} is wallet only", req.base);
    }
    if rel_coin.wallet_only(ctx) {
        return ERR!("Rel coin {} is wallet only", req.rel);
    }
    // CRD ch.60 R60.6.7 item 3: the smallest trade the order can produce must be spendable on both legs.
    try_s!(check_pre_trade_spend_safety(ctx, &base_coin, &rel_coin, req.min_volume.clone(), &req.price).await);

    #[cfg(feature = "ibc-routing-for-swaps")]
    {
        try_s!(
            ensure_ibc_routing_min_balance(ctx, &base_coin, &rel_coin)
                .or_else(|e| cancel_orders_on_error(ctx, &req, e))
                .await
        );
    }

    // The spendable balance of the base coin that the checks above read seeds the balance watch (CRD ch.60
    // R60.4.6 item 3); it is the quantity the watch polls.
    let (volume, base_balance_read) = if req.max {
        try_s!(
            get_max_volume_and_balance(ctx, &base_coin, &rel_coin)
                .or_else(|e| cancel_orders_on_error(ctx, &req, e))
                .await
        )
    } else {
        let base_balance = try_s!(
            check_balance_for_maker_swap(
                ctx,
                &base_coin,
                &rel_coin,
                req.volume.clone(),
                None,
                None,
                FeeApproxStage::OrderIssue
            )
            .or_else(|e| cancel_orders_on_error(ctx, &req, e))
            .await
        );
        (req.volume.clone(), base_balance)
    };

    let ordermatch_ctx = try_s!(OrdermatchContext::from_ctx(ctx));

    if req.cancel_previous {
        cancel_previous_maker_orders(ctx, &ordermatch_ctx, &req.base, &req.rel).await;
    }

    let conf_settings = OrderConfirmationsSettings {
        base_confs: req.base_confs.unwrap_or_else(|| base_coin.required_confirmations()),
        base_nota: req.base_nota.unwrap_or_else(|| base_coin.requires_notarization()),
        rel_confs: req.rel_confs.unwrap_or_else(|| rel_coin.required_confirmations()),
        rel_nota: req.rel_nota.unwrap_or_else(|| rel_coin.requires_notarization()),
    };
    let builder = MakerOrderBuilder::new(&base_coin, &rel_coin)
        .with_max_base_vol(volume)
        .with_min_base_vol(req.min_volume)
        .with_price(req.price)
        .with_conf_settings(conf_settings)
        .with_save_in_history(req.save_in_history)
        .with_base_orderbook_ticker(ordermatch_ctx.orderbook_ticker(base_coin.ticker()))
        .with_rel_orderbook_ticker(ordermatch_ctx.orderbook_ticker(rel_coin.ticker()))
        .with_timeout(req.timeout_in_minutes);

    let new_order = try_s!(builder.build());

    let request_orderbook = false;
    try_s!(
        subscribe_to_orderbook_topic(
            ctx,
            new_order.base_orderbook_ticker(),
            new_order.rel_orderbook_ticker(),
            request_orderbook
        )
        .await
    );
    save_my_new_maker_order(ctx.clone(), &new_order)
        .await
        .map_err(|e| ERRL!("{}", e))?;
    maker_order_created_p2p_notify(
        ctx.clone(),
        &new_order,
        base_coin.coin_protocol_info(),
        rel_coin.coin_protocol_info(),
    );

    {
        let order_arc = Arc::new(AsyncMutex::new(new_order.clone()));
        ordermatch_ctx.maker_orders_ctx.lock().add_order(&new_order, order_arc);
    }
    start_maker_balance_watch(ctx, &ordermatch_ctx, &new_order.base, Some(base_balance_read));
    Ok(new_order)
}

pub async fn set_price(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let req: SetPriceReq = try_s!(json::from_value(req));
    let maker_order = create_maker_order(&ctx, req).await?;
    let rpc_result = MakerOrderForRpc::from(&maker_order);
    let res = try_s!(json::to_vec(&json!({ "result": rpc_result })));
    Ok(try_s!(Response::builder().body(res)))
}

/// Removes the previous orders if there're some to allow multiple setprice call per pair.
/// It's common use case now as `autoprice` doesn't work with new ordermatching and
/// MM2 users request the coins price from aggregators by their own scripts issuing
/// repetitive setprice calls with new price
///
/// # Safety
///
/// The function locks the [`OrdermatchContext::maker_orders_ctx`] mutex.
pub(crate) async fn cancel_previous_maker_orders(
    ctx: &MmArc,
    ordermatch_ctx: &OrdermatchContext,
    base_to_delete: &str,
    rel_to_delete: &str,
) {
    let my_maker_orders = ordermatch_ctx.maker_orders_ctx.lock().clone_orders();

    for (_, order) in my_maker_orders {
        let cancelled = {
            let order = order.lock().await;
            let to_delete = order.base == base_to_delete && order.rel == rel_to_delete;
            // This checks that the order hasn't been removed by another process
            let cancelled = if to_delete {
                RemovedMakerOrder::take_out(ordermatch_ctx, &order)
            } else {
                None
            };
            if cancelled.is_some() {
                maker_order_cancelled_p2p_notify(ctx.clone(), &order);
            }
            cancelled
        };
        if let Some(cancelled) = cancelled {
            cancelled.delete(ctx, MakerOrderCancellationReason::Cancelled).await;
        }
    }
}

pub async fn update_maker_order(ctx: &MmArc, req: MakerOrderUpdateReq) -> Result<MakerOrder, String> {
    with_request_fee_scope(ctx, update_maker_order_impl(ctx, req)).await
}

async fn update_maker_order_impl(ctx: &MmArc, req: MakerOrderUpdateReq) -> Result<MakerOrder, String> {
    let ordermatch_ctx = try_s!(OrdermatchContext::from_ctx(ctx));
    let order_mutex = {
        match ordermatch_ctx.maker_orders_ctx.lock().get_order(&req.uuid) {
            Some(order) => order,
            None => return ERR!("There is no order with UUID {}", req.uuid),
        }
    };

    let order_before_update = order_mutex.lock().await.clone();
    if order_before_update.has_ongoing_matches() {
        return ERR!("Can't update an order that has ongoing matches");
    }

    let base = order_before_update.base.as_str();
    let rel = order_before_update.rel.as_str();
    let (base_coin, rel_coin) = match find_pair(ctx, base, rel).await {
        Ok(Some(c)) => c,
        _ => return ERR!("Base coin {} and/or rel coin {} are not activated", base, rel),
    };

    let original_conf_settings = order_before_update.conf_settings.unwrap();
    let updated_conf_settings = OrderConfirmationsSettings {
        base_confs: req.base_confs.unwrap_or(original_conf_settings.base_confs),
        base_nota: req.base_nota.unwrap_or(original_conf_settings.base_nota),
        rel_confs: req.rel_confs.unwrap_or(original_conf_settings.rel_confs),
        rel_nota: req.rel_nota.unwrap_or(original_conf_settings.rel_nota),
    };

    let original_volume = order_before_update.max_base_vol.clone();
    let reserved_amount = order_before_update.reserved_amount();

    let mut update_msg = new_protocol::MakerOrderUpdated::new(req.uuid);
    update_msg.with_new_conf_settings(updated_conf_settings);

    // Validate and Add new_price to update_msg if new_price is found in the request
    let new_price = match req.new_price {
        Some(new_price) => {
            try_s!(validate_price(new_price.clone()));
            update_msg.with_new_price(new_price.clone().into());
            new_price
        },
        None => order_before_update.price.clone(),
    };

    let min_base_amount = base_coin.min_trading_vol();
    let min_rel_amount = rel_coin.min_trading_vol();

    // Add min_volume to update_msg if min_volume is found in the request
    if let Some(min_volume) = req.min_volume.clone() {
        // Validate and Calculate Minimum Volume
        let actual_min_vol = try_s!(validate_and_get_min_vol(
            min_base_amount.clone(),
            min_rel_amount.clone(),
            Some(min_volume),
            new_price.clone()
        ));
        update_msg.with_new_min_volume(actual_min_vol.into());
    }

    // CRD ch.60 R60.6.7 item 3 and its Amendment (Decision AC): the order as updated must still be spendable on
    // both legs, whatever the update changes. The fee-rate input may come from the short cache (ch.38 R38.6.6j
    // item 2, R38.6.6k), so within its lifetime the check makes no node request. The minimum volume checked is
    // the requested one, else the one the order already carries.
    let min_volume_after_update = req
        .min_volume
        .clone()
        .or_else(|| Some(order_before_update.min_base_vol.clone()));
    try_s!(check_pre_trade_spend_safety(ctx, &base_coin, &rel_coin, min_volume_after_update, &new_price).await);

    // Calculate order volume and add to update_msg if new_volume is found in the request
    let new_volume = if req.max.unwrap_or(false) {
        let max_volume = try_s!(get_max_volume(ctx, &base_coin, &rel_coin).await) + reserved_amount.clone();
        update_msg.with_new_max_volume(max_volume.clone().into());
        max_volume
    } else if Option::is_some(&req.volume_delta) {
        let volume = original_volume + req.volume_delta.unwrap();
        if volume <= MmNumber::from("0") {
            return ERR!("New volume {} should be more than zero", volume);
        }
        try_s!(
            check_balance_for_maker_swap(
                ctx,
                &base_coin,
                &rel_coin,
                volume.clone(),
                None,
                None,
                FeeApproxStage::OrderIssue
            )
            .await
        );
        update_msg.with_new_max_volume(volume.clone().into());
        volume
    } else {
        original_volume
    };

    if new_volume <= reserved_amount {
        return ERR!(
            "New volume {} should be more than reserved amount for order matches {}",
            new_volume,
            reserved_amount
        );
    }

    // Validate Order Volume
    try_s!(validate_max_vol(
        min_base_amount.clone(),
        min_rel_amount.clone(),
        new_volume.clone() - reserved_amount.clone(),
        req.min_volume.clone(),
        new_price
    ));

    let order_mutex = {
        match ordermatch_ctx.maker_orders_ctx.lock().get_order(&req.uuid) {
            Some(order) => order,
            None => return ERR!("Order with UUID: {} has been deleted", req.uuid),
        }
    };

    // The order's lock is held for the in-memory work only: the update is applied and its state is published to the
    // order's write gate under the lock, and stored after the lock is released (CRD ch.12 R7a, R7b).
    let (applied, published, broadcast) = {
        let mut order = order_mutex.lock().await;
        if *order != order_before_update {
            return ERR!("Order state has changed after price/volume/balance checks. Please try to update the order again if it's still needed.");
        }
        if !ordermatch_ctx.maker_orders_ctx.lock().contains_key(&req.uuid) {
            return ERR!("Order with UUID: {} has been deleted", req.uuid);
        }
        order.apply_updated(&update_msg);
        let broadcast =
            !suppress_unchanged_order_updates(ctx) || maker_order_update_changes_order(&order_before_update, &order);
        let published = match PublishedState::publish(&ordermatch_ctx, &order, true) {
            Some(published) => published,
            None => return ERR!("Order with UUID: {} has been deleted", req.uuid),
        };
        (order.clone(), published, broadcast)
    };

    if let Err(e) = published.complete(ctx).await {
        // Back to the state before the update, unless the order has changed again, or been removed, since.
        let mut order = order_mutex.lock().await;
        if *order == applied && ordermatch_ctx.maker_orders_ctx.lock().contains_key(&req.uuid) {
            *order = order_before_update;
        }
        return ERR!("Error on saving updated order state to database:{}", e);
    }

    // Saved. The announcement goes out under the order's lock, so it takes its place among the announcements of
    // the order's other changes. An order that was cancelled in the meantime has announced its cancellation.
    let order = order_mutex.lock().await;
    // CRD ch.59 R59.4.6: an update that changes nothing peers see is not broadcast.
    if broadcast && ordermatch_ctx.maker_orders_ctx.lock().contains_key(&req.uuid) {
        // CRD ch.12 R7a: every value the message carries is the order's state now, not the one the update was
        // computed from, because another path (the balance handler, a negotiation) may have changed the order
        // while the write was open. With no such change the values are the ones the update set.
        if update_msg.new_price().is_some() {
            update_msg.with_new_price(order.price.clone().into());
        }
        if update_msg.new_min_volume().is_some() {
            update_msg.with_new_min_volume(order.min_base_vol.clone().into());
        }
        if let Some(conf_settings) = order.conf_settings {
            update_msg.with_new_conf_settings(conf_settings);
        }
        update_msg.with_new_max_volume(order.available_amount().into());
        maker_order_updated_p2p_notify(ctx.clone(), order.orderbook_topic(), update_msg, order.p2p_keypair());
    }
    Ok(order.clone())
}

/// Whether an update turned `before` into an order that differs in anything the order-updated
/// message carries: price, maximum volume, minimum volume or confirmation settings. Amounts are
/// compared as numbers (CRD ch.59 R59.4.6).
pub(crate) fn maker_order_update_changes_order(before: &MakerOrder, after: &MakerOrder) -> bool {
    before.price != after.price
        || before.max_base_vol != after.max_base_vol
        || before.min_base_vol != after.min_base_vol
        || before.conf_settings != after.conf_settings
}

pub async fn update_maker_order_rpc(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let req: MakerOrderUpdateReq = try_s!(json::from_value(req));
    let order = try_s!(update_maker_order(&ctx, req).await);
    let rpc_result = MakerOrderForRpc::from(&order);
    let res = try_s!(json::to_vec(&json!({ "result": rpc_result })));

    Ok(try_s!(Response::builder().body(res)))
}

#[derive(Deserialize)]
pub(crate) struct OrderStatusReq {
    pub(crate) uuid: Uuid,
}

#[derive(Serialize)]
pub(crate) struct OrderForRpcWithCancellationReason<'a> {
    #[serde(flatten)]
    pub(crate) order: OrderForRpc<'a>,
    pub(crate) cancellation_reason: &'a str,
}

pub async fn order_status(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let req: OrderStatusReq = try_s!(json::from_value(req));

    let ordermatch_ctx = try_s!(OrdermatchContext::from_ctx(&ctx));
    let storage = MyOrdersStorage::new(ctx.clone());

    let maybe_order_mutex = ordermatch_ctx.maker_orders_ctx.lock().get_order(&req.uuid);
    if let Some(order_mutex) = maybe_order_mutex {
        let order = order_mutex.lock().await.clone();
        let res = json!({
            "type": "Maker",
            "order": MakerOrderForMyOrdersRpc::from(&order),
        });
        return Response::builder()
            .body(json::to_vec(&res).expect("Serialization failed"))
            .map_err(|e| ERRL!("{}", e));
    }

    let taker_orders = ordermatch_ctx.my_taker_orders.lock().await;
    if let Some(order) = taker_orders.get(&req.uuid) {
        let res = json!({
            "type": "Taker",
            "order": TakerOrderForRpc::from(order),
        });
        return Response::builder()
            .body(json::to_vec(&res).expect("Serialization failed"))
            .map_err(|e| ERRL!("{}", e));
    }

    let order = try_s!(storage.load_order_from_history(req.uuid).await);
    let cancellation_reason = &try_s!(storage.select_order_status(req.uuid).await);

    let res = json!(OrderForRpcWithCancellationReason {
        order: OrderForRpc::from(&order),
        cancellation_reason,
    });
    Response::builder()
        .body(json::to_vec(&res).expect("Serialization failed"))
        .map_err(|e| ERRL!("{}", e))
}

#[derive(Display)]
pub enum MakerOrderCancellationReason {
    Fulfilled,
    InsufficientBalance,
    Cancelled,
    Expired,
}

#[derive(Display)]
pub enum TakerOrderCancellationReason {
    Fulfilled,
    ToMaker,
    TimedOut,
    Cancelled,
}

#[derive(Debug, Deserialize)]
pub struct MyOrdersFilter {
    pub order_type: Option<String>,
    pub initial_action: Option<String>,
    pub base: Option<String>,
    pub rel: Option<String>,
    pub from_price: Option<MmNumber>,
    pub to_price: Option<MmNumber>,
    pub from_volume: Option<MmNumber>,
    pub to_volume: Option<MmNumber>,
    pub from_timestamp: Option<u64>,
    pub to_timestamp: Option<u64>,
    pub was_taker: Option<bool>,
    pub status: Option<String>,
    #[serde(default)]
    pub include_details: bool,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", content = "order")]
pub enum Order {
    Maker(MakerOrder),
    Taker(TakerOrder),
}

impl<'a> From<&'a Order> for OrderForRpc<'a> {
    fn from(order: &'a Order) -> OrderForRpc<'a> {
        match order {
            Order::Maker(o) => OrderForRpc::Maker(MakerOrderForRpc::from(o)),
            Order::Taker(o) => OrderForRpc::Taker(TakerOrderForRpc::from(o)),
        }
    }
}

impl Order {
    pub fn uuid(&self) -> Uuid {
        match self {
            Order::Maker(maker) => maker.uuid,
            Order::Taker(taker) => taker.request.uuid,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct UuidParseError {
    pub(crate) uuid: String,
    pub(crate) warning: String,
}

#[derive(Debug, Default)]
pub struct RecentOrdersSelectResult {
    /// Orders matching the query
    pub orders: Vec<FilteringOrder>,
    /// Total count of orders matching the query
    pub total_count: usize,
    /// The number of skipped orders
    pub skipped: usize,
}

#[derive(Debug, Serialize)]
pub struct FilteringOrder {
    pub uuid: String,
    pub order_type: String,
    pub initial_action: String,
    pub base: String,
    pub rel: String,
    pub price: f64,
    pub volume: f64,
    pub created_at: i64,
    pub last_updated: i64,
    pub was_taker: i8,
    pub status: String,
}

/// Returns *all* uuids of swaps, which match the selected filter.
pub async fn orders_history_by_filter(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let storage = MyOrdersStorage::new(ctx.clone());

    let filter: MyOrdersFilter = try_s!(json::from_value(req));
    let db_result = try_s!(storage.select_orders_by_filter(&filter, None).await);

    let mut warnings = vec![];
    let rpc_orders = if filter.include_details {
        let mut vec = Vec::with_capacity(db_result.orders.len());
        for order in db_result.orders.iter() {
            let uuid = match Uuid::parse_str(order.uuid.as_str()) {
                Ok(uuid) => uuid,
                Err(e) => {
                    let warning = format!(
                        "Order details for Uuid {} were skipped because uuid could not be parsed",
                        order.uuid
                    );
                    log::warn!("{}, error {}", warning, e);
                    warnings.push(UuidParseError {
                        uuid: order.uuid.clone(),
                        warning,
                    });
                    continue;
                },
            };

            if let Ok(order) = storage.load_order_from_history(uuid).await {
                vec.push(order);
                continue;
            }

            let ordermatch_ctx = try_s!(OrdermatchContext::from_ctx(&ctx));
            if order.order_type == "Maker" {
                let maybe_order_mutex = ordermatch_ctx.maker_orders_ctx.lock().get_order(&uuid);
                if let Some(maker_order_mutex) = maybe_order_mutex {
                    let maker_order = maker_order_mutex.lock().await.clone();
                    vec.push(Order::Maker(maker_order));
                }
                continue;
            }

            let taker_orders = ordermatch_ctx.my_taker_orders.lock().await;
            if let Some(taker_order) = taker_orders.get(&uuid) {
                vec.push(Order::Taker(taker_order.to_owned()));
            }
        }
        vec
    } else {
        vec![]
    };

    let details: Vec<_> = rpc_orders.iter().map(OrderForRpc::from).collect();

    let json = json!({
    "result": {
        "orders": db_result.orders,
        "details": details,
        "found_records": db_result.total_count,
        "warnings": warnings,
    }});

    let res = try_s!(json::to_vec(&json));

    Ok(try_s!(Response::builder().body(res)))
}

#[derive(Deserialize)]
pub struct CancelOrderReq {
    pub(crate) uuid: Uuid,
}

#[derive(Debug, Deserialize, Serialize, SerializeErrorType, Display)]
#[serde(tag = "error_type", content = "error_data")]
pub enum CancelOrderError {
    #[display(fmt = "Cannot retrieve order match context.")]
    CannotRetrieveOrderMatchContext,
    #[display(fmt = "Order {} is being matched now, can't cancel", uuid)]
    OrderBeingMatched { uuid: Uuid },
    #[display(fmt = "Order {} not found", uuid)]
    UUIDNotFound { uuid: Uuid },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CancelOrderResponse {
    pub(crate) result: String,
}

pub async fn cancel_order(ctx: MmArc, req: CancelOrderReq) -> Result<CancelOrderResponse, MmError<CancelOrderError>> {
    let ordermatch_ctx = match OrdermatchContext::from_ctx(&ctx) {
        Ok(x) => x,
        Err(_) => return MmError::err(CancelOrderError::CannotRetrieveOrderMatchContext),
    };
    let maybe_order_mutex = ordermatch_ctx.maker_orders_ctx.lock().get_order(&req.uuid);
    if let Some(order_mutex) = maybe_order_mutex {
        let cancelled = {
            let order = order_mutex.lock().await;
            if !order.is_cancellable() {
                return MmError::err(CancelOrderError::OrderBeingMatched { uuid: req.uuid });
            }
            // This checks that the order hasn't been removed by another process
            let cancelled = RemovedMakerOrder::take_out(&ordermatch_ctx, &order);
            if cancelled.is_some() {
                maker_order_cancelled_p2p_notify(ctx.clone(), &order);
            }
            cancelled
        };
        if let Some(cancelled) = cancelled {
            cancelled.delete(&ctx, MakerOrderCancellationReason::Cancelled).await;
        }
        return Ok(CancelOrderResponse {
            result: "success".to_string(),
        });
    }

    let mut taker_orders = ordermatch_ctx.my_taker_orders.lock().await;
    match taker_orders.entry(req.uuid) {
        Entry::Occupied(order) => {
            if !order.get().is_cancellable() {
                return MmError::err(CancelOrderError::UUIDNotFound { uuid: req.uuid });
            }
            let order = order.remove();
            delete_my_taker_order(ctx, order, TakerOrderCancellationReason::Cancelled)
                .compat()
                .await
                .ok();
            return Ok(CancelOrderResponse {
                result: "success".to_string(),
            });
        },
        // error is returned
        Entry::Vacant(_) => (),
    }
    MmError::err(CancelOrderError::UUIDNotFound { uuid: req.uuid })
}

pub async fn cancel_order_rpc(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let req: CancelOrderReq = try_s!(json::from_value(req));

    let ordermatch_ctx = try_s!(OrdermatchContext::from_ctx(&ctx));
    let maybe_order_mutex = ordermatch_ctx.maker_orders_ctx.lock().get_order(&req.uuid);
    if let Some(order_mutex) = maybe_order_mutex {
        let cancelled = {
            let order = order_mutex.lock().await;
            if !order.is_cancellable() {
                return ERR!("Order {} is being matched now, can't cancel", req.uuid);
            }
            // This checks that the order hasn't been removed by another process
            let cancelled = RemovedMakerOrder::take_out(&ordermatch_ctx, &order);
            if cancelled.is_some() {
                maker_order_cancelled_p2p_notify(ctx.clone(), &order);
            }
            cancelled
        };
        if let Some(cancelled) = cancelled {
            cancelled.delete(&ctx, MakerOrderCancellationReason::Cancelled).await;
        }
        let res = json!({
            "result": "success"
        });
        return Response::builder()
            .body(json::to_vec(&res).expect("Serialization failed"))
            .map_err(|e| ERRL!("{}", e));
    }

    let mut taker_orders = ordermatch_ctx.my_taker_orders.lock().await;
    match taker_orders.entry(req.uuid) {
        Entry::Occupied(order) => {
            if !order.get().is_cancellable() {
                return ERR!("Order {} is being matched now, can't cancel", req.uuid);
            }
            let order = order.remove();
            delete_my_taker_order(ctx, order, TakerOrderCancellationReason::Cancelled)
                .compat()
                .await
                .ok();
            let res = json!({
                "result": "success"
            });
            return Response::builder()
                .body(json::to_vec(&res).expect("Serialization failed"))
                .map_err(|e| ERRL!("{}", e));
        },
        // error is returned
        Entry::Vacant(_) => (),
    }

    let res = json!({
        "error": format!("Order with uuid {} is not found", req.uuid),
    });
    Response::builder()
        .status(404)
        .body(json::to_vec(&res).expect("Serialization failed"))
        .map_err(|e| ERRL!("{}", e))
}

#[derive(Serialize)]
pub(crate) struct MakerOrderForMyOrdersRpc<'a> {
    #[serde(flatten)]
    pub(crate) order: MakerOrderForRpc<'a>,
    pub(crate) cancellable: bool,
    pub(crate) available_amount: RenderedDecimal,
}

impl<'a> From<&'a MakerOrder> for MakerOrderForMyOrdersRpc<'a> {
    fn from(order: &'a MakerOrder) -> MakerOrderForMyOrdersRpc<'a> {
        MakerOrderForMyOrdersRpc {
            order: order.into(),
            cancellable: order.is_cancellable(),
            available_amount: order.available_amount_rendered(),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct TakerMatchForRpc<'a> {
    pub(crate) reserved: MakerReservedForRpc<'a>,
    pub(crate) connect: TakerConnectForRpc<'a>,
    pub(crate) connected: Option<MakerConnectedForRpc<'a>>,
    pub(crate) last_updated: u64,
}

#[allow(clippy::needless_borrow)]
impl<'a> From<&'a TakerMatch> for TakerMatchForRpc<'a> {
    fn from(taker_match: &'a TakerMatch) -> TakerMatchForRpc<'a> {
        TakerMatchForRpc {
            reserved: (&taker_match.reserved).into(),
            connect: (&taker_match.connect).into(),
            connected: taker_match.connected.as_ref().map(|connected| connected.into()),
            last_updated: 0,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct TakerOrderForRpc<'a> {
    pub(crate) created_at: u64,
    pub(crate) request: TakerRequestForRpc<'a>,
    pub(crate) matches: HashMap<Uuid, TakerMatchForRpc<'a>>,
    pub(crate) order_type: &'a OrderType,
    pub(crate) cancellable: bool,
    pub(crate) base_orderbook_ticker: &'a Option<String>,
    pub(crate) rel_orderbook_ticker: &'a Option<String>,
}

#[allow(clippy::needless_borrow)]
impl<'a> From<&'a TakerOrder> for TakerOrderForRpc<'a> {
    fn from(order: &'a TakerOrder) -> TakerOrderForRpc<'a> {
        TakerOrderForRpc {
            created_at: order.created_at,
            request: (&order.request).into(),
            matches: order
                .matches
                .iter()
                .map(|(uuid, taker_match)| (*uuid, taker_match.into()))
                .collect(),
            cancellable: order.is_cancellable(),
            order_type: &order.order_type,
            base_orderbook_ticker: &order.base_orderbook_ticker,
            rel_orderbook_ticker: &order.rel_orderbook_ticker,
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "type", content = "order")]
pub(crate) enum OrderForRpc<'a> {
    Maker(MakerOrderForRpc<'a>),
    Taker(TakerOrderForRpc<'a>),
}

/// The `my_orders` answer. Field order is the wire order: `result`, then `maker_orders` and `taker_orders`.
#[derive(Serialize)]
struct MyOrdersResponse<'a> {
    result: MyOrdersResult<'a>,
}

#[derive(Serialize)]
struct MyOrdersResult<'a> {
    maker_orders: HashMap<&'a Uuid, MakerOrderForMyOrdersRpc<'a>>,
    taker_orders: HashMap<&'a Uuid, TakerOrderForRpc<'a>>,
}

pub async fn my_orders(ctx: MmArc) -> Result<Response<Vec<u8>>, String> {
    let ordermatch_ctx = try_s!(OrdermatchContext::from_ctx(&ctx));
    let my_maker_orders = ordermatch_ctx.maker_orders_ctx.lock().clone_orders();
    let mut maker_orders_map = HashMap::with_capacity(my_maker_orders.len());
    for (uuid, order_mutex) in my_maker_orders.iter() {
        let order = order_mutex.lock().await.clone();
        maker_orders_map.insert(uuid, order);
    }
    let maker_orders_for_rpc: HashMap<_, _> = maker_orders_map
        .iter()
        .map(|(uuid, order)| (*uuid, MakerOrderForMyOrdersRpc::from(order)))
        .collect();

    let taker_orders = ordermatch_ctx.my_taker_orders.lock().await;
    let taker_orders_for_rpc: HashMap<_, _> = taker_orders
        .iter()
        .map(|(uuid, order)| (uuid, TakerOrderForRpc::from(order)))
        .collect();
    let res = MyOrdersResponse {
        result: MyOrdersResult {
            maker_orders: maker_orders_for_rpc,
            taker_orders: taker_orders_for_rpc,
        },
    };
    Response::builder()
        .body(try_s!(json::to_vec(&res)))
        .map_err(|e| ERRL!("{}", e))
}

pub fn my_maker_orders_dir(ctx: &MmArc) -> PathBuf { ctx.dbdir().join("ORDERS").join("MY").join("MAKER") }

pub(crate) fn my_taker_orders_dir(ctx: &MmArc) -> PathBuf { ctx.dbdir().join("ORDERS").join("MY").join("TAKER") }

pub(crate) fn my_orders_history_dir(ctx: &MmArc) -> PathBuf { ctx.dbdir().join("ORDERS").join("MY").join("HISTORY") }

pub fn my_maker_order_file_path(ctx: &MmArc, uuid: &Uuid) -> PathBuf {
    my_maker_orders_dir(ctx).join(format!("{}.json", uuid))
}

pub(crate) fn my_taker_order_file_path(ctx: &MmArc, uuid: &Uuid) -> PathBuf {
    my_taker_orders_dir(ctx).join(format!("{}.json", uuid))
}

pub(crate) fn my_order_history_file_path(ctx: &MmArc, uuid: &Uuid) -> PathBuf {
    my_orders_history_dir(ctx).join(format!("{}.json", uuid))
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HistoricalOrder {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_base_vol: Option<MmNumber>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) min_base_vol: Option<MmNumber>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) price: Option<MmNumber>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) updated_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) conf_settings: Option<OrderConfirmationsSettings>,
}

/// The log line for a saved order that must be cancelled at startup because it involves a Tendermint platform
/// coin or token while [`coins::TENDERMINT_SWAPS_AVAILABLE`] is `false`; `None` when the order is kept. The coins
/// configuration decides, so the coins do not have to be activated yet.
pub(super) fn tendermint_order_cancel_message(
    ctx: &MmArc,
    kind: &str,
    uuid: &Uuid,
    base: &str,
    rel: &str,
) -> Option<String> {
    let is_tendermint = |ticker: &str| {
        matches!(
            coin_conf(ctx, ticker)["protocol"]["type"].as_str(),
            Some("TENDERMINT" | "TENDERMINTTOKEN")
        )
    };
    if coins::TENDERMINT_SWAPS_AVAILABLE || !(is_tendermint(base) || is_tendermint(rel)) {
        return None;
    }
    Some(format!(
        "Cancelled saved {kind} order {uuid} ({base}/{rel}): Tendermint swaps are not available in this build"
    ))
}

pub async fn orders_kick_start(ctx: &MmArc) -> Result<HashSet<String>, String> {
    let mut coins = HashSet::new();
    let ordermatch_ctx = try_s!(OrdermatchContext::from_ctx(ctx));

    let storage = MyOrdersStorage::new(ctx.clone());
    let saved_maker_orders = try_s!(storage.load_active_maker_orders().await);
    let saved_taker_orders = try_s!(storage.load_active_taker_orders().await);

    for order in saved_maker_orders {
        if let Some(message) = tendermint_order_cancel_message(ctx, "maker", &order.uuid, &order.base, &order.rel) {
            log::info!("{}", message);
            delete_my_maker_order(ctx.clone(), order, MakerOrderCancellationReason::Cancelled)
                .compat()
                .await
                .ok();
            continue;
        }
        coins.insert(order.base.clone());
        coins.insert(order.rel.clone());
        let order_arc = Arc::new(AsyncMutex::new(order.clone()));
        ordermatch_ctx.maker_orders_ctx.lock().add_order(&order, order_arc);
        // CRD ch.60 R60.4.6 item 4: the watch of the base coin starts unseeded, no balance is read here.
        start_maker_balance_watch(ctx, &ordermatch_ctx, &order.base, None);
    }

    let mut taker_orders = ordermatch_ctx.my_taker_orders.lock().await;
    for order in saved_taker_orders {
        let request = &order.request;
        if let Some(message) = tendermint_order_cancel_message(ctx, "taker", &request.uuid, &request.base, &request.rel)
        {
            log::info!("{}", message);
            delete_my_taker_order(ctx.clone(), order, TakerOrderCancellationReason::Cancelled)
                .compat()
                .await
                .ok();
            continue;
        }
        coins.insert(order.request.base.clone());
        coins.insert(order.request.rel.clone());
        taker_orders.insert(order.request.uuid, order);
    }
    Ok(coins)
}

#[derive(Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum CancelBy {
    /// All orders of current node
    All,
    /// All orders of specific pair
    Pair { base: String, rel: String },
    /// All orders using the coin ticker as base or rel
    Coin { ticker: String },
}

pub async fn cancel_orders_by(ctx: &MmArc, cancel_by: CancelBy) -> Result<(Vec<Uuid>, Vec<Uuid>), String> {
    let mut cancelled = vec![];
    let mut cancelled_maker_orders: Vec<RemovedMakerOrder> = vec![];
    let mut cancelled_taker_orders = vec![];
    let mut currently_matching = vec![];

    let ordermatch_ctx = try_s!(OrdermatchContext::from_ctx(ctx));
    let maker_orders = ordermatch_ctx.maker_orders_ctx.lock().clone_orders();
    let mut taker_orders = ordermatch_ctx.my_taker_orders.lock().await;

    // `$order` is the locked order. A cancelled order leaves the container, and its cancellation is announced,
    // while its lock is held; the deletion from storage runs after the loop with no order lock held (CRD ch.12
    // R7a).
    macro_rules! cancel_maker_if_true {
        ($e: expr, $uuid: ident, $order: ident) => {
            if $e {
                if $order.is_cancellable() {
                    // This checks that the order hasn't been removed by another process
                    if let Some(removed) = RemovedMakerOrder::take_out(&ordermatch_ctx, &$order) {
                        maker_order_cancelled_p2p_notify(ctx.clone(), &$order);
                        cancelled_maker_orders.push(removed);
                    }
                    cancelled.push($uuid);
                } else {
                    currently_matching.push($uuid);
                }
            }
        };
    }

    macro_rules! cancel_taker_if_true {
        ($e: expr, $uuid: ident, $order: ident) => {
            if $e {
                if $order.is_cancellable() {
                    cancelled_taker_orders.push($order);
                    cancelled.push($uuid);
                    None
                } else {
                    currently_matching.push($uuid);
                    Some(($uuid, $order))
                }
            } else {
                Some(($uuid, $order))
            }
        };
    }

    match cancel_by {
        CancelBy::All => {
            for (uuid, order) in maker_orders.iter() {
                let uuid = *uuid;
                let order = order.lock().await;
                cancel_maker_if_true!(true, uuid, order);
            }
            *taker_orders = taker_orders
                .drain()
                .filter_map(|(uuid, order)| cancel_taker_if_true!(true, uuid, order))
                .collect();
        },
        CancelBy::Pair { base, rel } => {
            for (uuid, order) in maker_orders.iter() {
                let uuid = *uuid;
                let order = order.lock().await;
                cancel_maker_if_true!(order.base == base && order.rel == rel, uuid, order);
            }
            *taker_orders = taker_orders
                .drain()
                .filter_map(|(uuid, order)| {
                    cancel_taker_if_true!(order.request.base == base && order.request.rel == rel, uuid, order)
                })
                .collect();
        },
        CancelBy::Coin { ticker } => {
            for (uuid, order) in maker_orders.iter() {
                let uuid = *uuid;
                let order = order.lock().await;
                cancel_maker_if_true!(order.base == ticker || order.rel == ticker, uuid, order);
            }
            *taker_orders = taker_orders
                .drain()
                .filter_map(|(uuid, order)| {
                    cancel_taker_if_true!(order.request.base == ticker || order.request.rel == ticker, uuid, order)
                })
                .collect();
        },
    };
    for order in cancelled_maker_orders {
        order.delete(ctx, MakerOrderCancellationReason::Cancelled).await;
    }
    for order in cancelled_taker_orders {
        delete_my_taker_order(ctx.clone(), order, TakerOrderCancellationReason::Cancelled)
            .compat()
            .await
            .ok();
    }
    Ok((cancelled, currently_matching))
}

pub async fn cancel_all_orders(
    ctx: MmArc,
    cancel_by: CancelBy,
) -> Result<CancelAllOrdersResponse, MmError<CancelAllOrdersError>> {
    cancel_orders_by(&ctx, cancel_by)
        .await
        .map(|(cancelled, currently_matching)| CancelAllOrdersResponse {
            cancelled,
            currently_matching,
        })
        .map_to_mm(CancelAllOrdersError::LegacyError)
}

pub async fn cancel_all_orders_rpc(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let cancel_by: CancelBy = try_s!(json::from_value(req["cancel_by"].clone()));

    let (cancelled, currently_matching) = try_s!(cancel_orders_by(&ctx, cancel_by).await);

    let res = json!({
        "result": {
            "cancelled": cancelled,
            "currently_matching": currently_matching,
        }
    });
    Response::builder()
        .body(json::to_vec(&res).expect("Serialization failed"))
        .map_err(|e| ERRL!("{}", e))
}

#[cfg(all(test, target_os = "linux", target_env = "gnu"))]
mod heap_release_tests {
    use super::*;

    #[test]
    fn test_parse_statm_rss_bytes() {
        // size resident shared text lib data dt
        assert_eq!(
            parse_statm_rss_bytes("5000 1234 300 10 0 900 0\n", 4096),
            Some(1234 * 4096)
        );
        assert_eq!(parse_statm_rss_bytes("5000 0 0 0 0 0 0", 4096), Some(0));
        assert_eq!(parse_statm_rss_bytes("", 4096), None);
        assert_eq!(parse_statm_rss_bytes("5000", 4096), None);
        assert_eq!(parse_statm_rss_bytes("5000 abc 1", 4096), None);
        assert_eq!(parse_statm_rss_bytes("5000 -3 1", 4096), None);
        assert_eq!(parse_statm_rss_bytes("5000 18446744073709551615 1", 4096), None);
    }

    #[test]
    fn test_current_rss_bytes_is_readable_and_release_freed_heap_does_not_panic() {
        assert!(current_rss_bytes().is_some_and(|rss| rss > 0));
        release_freed_heap();
    }
}
