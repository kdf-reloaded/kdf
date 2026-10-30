use super::*;

impl From<(new_protocol::MakerOrderCreated, String)> for OrderbookItem {
    fn from(tuple: (new_protocol::MakerOrderCreated, String)) -> OrderbookItem {
        let (order, pubkey) = tuple;

        OrderbookItem {
            pubkey,
            base: order.base,
            rel: order.rel,
            price: order.price,
            max_volume: order.max_volume,
            min_volume: order.min_volume,
            uuid: order.uuid.into(),
            created_at: order.created_at,
            base_protocol_info: order.base_protocol_info,
            rel_protocol_info: order.rel_protocol_info,
            conf_settings: Some(order.conf_settings),
        }
    }
}

pub fn addr_format_from_protocol_info(protocol_info: &[u8]) -> AddressFormat {
    match rmp_serde::from_slice::<AddressFormat>(protocol_info) {
        Ok(format) => format,
        Err(_) => AddressFormat::Standard,
    }
}

pub(crate) struct ProcessTrieParams<'a> {
    pub(crate) pubkey: &'a str,
    pub(crate) alb_pair: &'a str,
    pub(crate) protocol_infos: &'a HashMap<Uuid, BaseRelProtocolInfo>,
    pub(crate) conf_infos: &'a HashMap<Uuid, OrderConfirmationsSettings>,
}

pub(crate) fn process_pubkey_full_trie(
    orderbook: &mut Orderbook,
    new_trie_orders: PubkeyOrders,
    params: ProcessTrieParams,
) -> Vec<TrieOp> {
    // 1) Index-only removal of existing orders for (pubkey, pair)
    orderbook.index_remove_pubkey_pair_orders(params.pubkey, params.alb_pair);

    // 2) Start with a single ClearPair op to reset trie/history/root for (pubkey, pair)
    let mut ops = vec![TrieOp::ClearPair {
        pubkey: params.pubkey.to_owned(),
        alb_pair: params.alb_pair.to_owned(),
    }];

    // 3) Re-insert all incoming orders (index + trie ops)
    for (uuid, order) in new_trie_orders {
        let item = OrderbookItem::from_p2p_and_info(
            order,
            params.protocol_infos.get(&uuid).cloned().unwrap_or_default(),
            params.conf_infos.get(&uuid).cloned(),
        );
        let mut insert_ops = orderbook.index_insert_or_update(item);
        ops.append(&mut insert_ops);
    }

    ops
}

pub(crate) fn process_trie_delta(
    orderbook: &mut Orderbook,
    delta_orders: HashMap<Uuid, Option<OrderbookP2PItem>>,
    params: ProcessTrieParams,
) -> Vec<TrieOp> {
    let mut ops = Vec::with_capacity(delta_orders.len());
    for (uuid, maybe_order) in delta_orders {
        match maybe_order {
            Some(order) => {
                let item = OrderbookItem::from_p2p_and_info(
                    order,
                    params.protocol_infos.get(&uuid).cloned().unwrap_or_default(),
                    params.conf_infos.get(&uuid).cloned(),
                );
                let mut insert_ops = orderbook.index_insert_or_update(item);
                ops.append(&mut insert_ops);
            },
            None => {
                if let Some((_removed, op)) = orderbook.index_remove(uuid) {
                    ops.push(op);
                }
            },
        }
    }
    ops
}

pub(crate) async fn process_orders_keep_alive(
    ctx: MmArc,
    propagated_from_peer: String,
    from_pubkey: String,
    keep_alive: new_protocol::PubkeyKeepAlive,
    i_am_relay: bool,
) -> bool {
    let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).expect("from_ctx failed");
    let deferral_ms = sync_answer_deferral_ms(&ctx);
    let to_request = keep_alive_sync_request(
        &ordermatch_ctx,
        &from_pubkey,
        keep_alive,
        i_am_relay,
        SyncRequestRoot::configured(&ctx),
        TRIE_CATCH_UP_BOUND_MS,
        deferral_ms,
    )
    .await;

    // `in_flight` ends the record of this request (CRD ch.32 R-Q5b) wherever the function ends: a
    // failed request, an early return or a dropped future all drop it. It lasts through the second
    // request of R-Q5c as well.
    let (req, in_flight) = match to_request {
        Some(to_request) => to_request,
        // The message was processed, simply forward it
        None => return true,
    };

    let trie_roots = match req {
        OrdermatchRequest::SyncPubkeyOrderbookState { trie_roots, .. } => trie_roots,
        // `keep_alive_sync_request` builds no other request.
        _ => return false,
    };
    let response = match request_sync_state(
        &ctx,
        &ordermatch_ctx,
        &from_pubkey,
        trie_roots,
        &propagated_from_peer,
        deferral_ms,
    )
    .await
    {
        Some(response) => response,
        None => return false,
    };

    // Phase 1: derive all index mutations under the Orderbook lock and enqueue their trie ops
    // before releasing it (CRD ch.32 R-T3b item 1).
    let queued_batches = {
        let mut orderbook = ordermatch_ctx.orderbook.lock();
        let mut ops = Vec::new();
        for (pair, diff) in response.pair_orders_diff {
            let params = ProcessTrieParams {
                pubkey: &from_pubkey,
                alb_pair: &pair,
                protocol_infos: &response.protocol_infos,
                conf_infos: &response.conf_infos,
            };
            let mut pair_ops = match diff {
                DeltaOrFullTrie::Delta(delta) => process_trie_delta(&mut orderbook, delta, params),
                DeltaOrFullTrie::FullTrie(values) => process_pubkey_full_trie(&mut orderbook, values, params),
            };
            ops.append(&mut pair_ops);
        }
        // Phase 2: the worker applies the batch in the background; queued under the guard.
        orderbook.queue_trie_ops(&ordermatch_ctx.trie_ops_tx, ops);
        orderbook.trie_ops_enqueued
    };

    // The request counts as in flight (CRD ch.32 R-Q5b) until the worker has applied the batch. The
    // forward below does not wait for that (R-F3b): the record ends on a task of its own.
    if let Some(in_flight) = in_flight {
        in_flight.complete_after_apply(ordermatch_ctx.trie_store.clone(), queued_batches, deferral_ms);
    }

    true
}

/// The keep-alive-driven request of CRD ch.32 R-Q4 to the forwarding `peer`, asked once more after a
/// quick failure for the pairs still at the all-zero placeholder (R-Q5c). The caller keeps the in-flight
/// record of R-Q5b for the whole call, so the requests parked behind it wait for the second answer too.
async fn request_sync_state(
    ctx: &MmArc,
    ordermatch_ctx: &OrdermatchContext,
    pubkey: &str,
    trie_roots: HashMap<AlbOrderedOrderbookPair, H64>,
    peer: &str,
    deferral_ms: u64,
) -> Option<SyncPubkeyOrderbookStateRes> {
    let sent_ms = now_ms();
    if let Some(response) = ask_for_sync_state(ctx, ordermatch_ctx, pubkey, trie_roots.clone(), peer, 1).await {
        return Some(response);
    }

    // R-Q5c item 1: only while the second answer can still arrive within the bound of R-Q5b, which a
    // request that failed by timing out never can.
    let retry_ms = sync_request_retry_ms(ctx);
    if retry_ms == 0 || now_ms().saturating_sub(sent_ms) + retry_ms >= deferral_ms {
        return None;
    }
    Timer::sleep(retry_ms as f64 / 1000.).await;

    // R-Q5c item 2: a pair that has left the placeholder meanwhile is not asked for again.
    let still_unsynchronized = pairs_at_placeholder(ordermatch_ctx, pubkey, trie_roots);
    if still_unsynchronized.is_empty() {
        return None;
    }
    ask_for_sync_state(ctx, ordermatch_ctx, pubkey, still_unsynchronized, peer, 2).await
}

/// One `SyncPubkeyOrderbookState` request for `trie_roots` of `pubkey` to `peer`; `attempt` is 1 for the
/// request and 2 for the re-ask. Any outcome but an answer (an error, "no data", a timeout, a transport
/// failure) is `None`. This is the only place where an own synchronization request is logged:
/// the first request and the re-ask both pass here.
async fn ask_for_sync_state(
    ctx: &MmArc,
    ordermatch_ctx: &OrdermatchContext,
    pubkey: &str,
    trie_roots: HashMap<AlbOrderedOrderbookPair, H64>,
    peer: &str,
    attempt: u8,
) -> Option<SyncPubkeyOrderbookStateRes> {
    let pairs = if debug_enabled() {
        pairs_label(trie_roots.keys())
    } else {
        String::new()
    };
    let req = OrdermatchRequest::SyncPubkeyOrderbookState {
        pubkey: pubkey.to_owned(),
        trie_roots,
    };
    let started_ms = now_ms();
    let result =
        request_one_peer::<SyncPubkeyOrderbookStateRes>(ctx.clone(), P2PRequest::Ordermatch(req), peer.to_owned())
            .await;
    let now = now_ms();
    let (class, error) = classify_sync_request(&result);
    ordermatch_ctx.sync_log.request(
        &RequestEvent {
            peer,
            pubkey,
            pairs: &pairs,
            attempt,
            class,
            elapsed_ms: now.saturating_sub(started_ms),
            error,
        },
        now,
    );
    result.ok().flatten()
}

/// The entries of `trie_roots` whose pair this node still holds at the all-zero placeholder for `pubkey`.
fn pairs_at_placeholder(
    ordermatch_ctx: &OrdermatchContext,
    pubkey: &str,
    trie_roots: HashMap<AlbOrderedOrderbookPair, H64>,
) -> HashMap<AlbOrderedOrderbookPair, H64> {
    let trie_store = ordermatch_ctx.trie_store.lock();
    let held = match trie_store.pubkeys_state.get(pubkey) {
        Some(state) => &state.trie_roots,
        None => return HashMap::new(),
    };
    trie_roots
        .into_iter()
        .filter(|(pair, _)| held.get(pair) == Some(&H64::default()))
        .collect()
}

/// The root a keep-alive synchronization request names for a diverged pair (CRD ch.32 R-Q4, R-Q4a).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum SyncRequestRoot {
    /// The root this node holds for the pair (R-Q4a, Reloaded's default).
    Held,
    /// The root the keep-alive advertised (R-Q4, the reference's form).
    Advertised,
}

impl SyncRequestRoot {
    /// The MM2.json boolean `orderbook_sync_names_held_root`, default `true`; only an explicit JSON
    /// `false` selects the advertised root.
    pub(crate) fn configured(ctx: &MmArc) -> Self {
        if ctx.conf["orderbook_sync_names_held_root"].as_bool() == Some(false) {
            SyncRequestRoot::Advertised
        } else {
            SyncRequestRoot::Held
        }
    }
}

/// The synchronization request (if any) a received keep-alive calls for (CRD ch.32 R-Q4). The
/// receipt time is recorded first; the roots are then compared with a trie store that reflects
/// every index operation completed before the keep-alive is handled (R-T3b item 4). This runs on
/// the keep-alive's own task, so it may wait for the worker up to `catch_up_bound_ms`; past the
/// bound the roots are compared with the store as it stands, which at worst costs a request a
/// caught-up store would not have needed.
///
/// With `deferral_ms` above zero (CRD ch.32 R-Q5b) the request comes with the record that marks it as
/// in flight. The record is made while the trie store is still locked, in the same step that creates
/// the all-zero placeholder entries of the request, so no request for the placeholder can be seen
/// without it.
pub(crate) async fn keep_alive_sync_request(
    ordermatch_ctx: &OrdermatchContext,
    from_pubkey: &str,
    keep_alive: new_protocol::PubkeyKeepAlive,
    i_am_relay: bool,
    request_root: SyncRequestRoot,
    catch_up_bound_ms: u64,
    deferral_ms: u64,
) -> Option<(OrdermatchRequest, Option<SyncInFlight>)> {
    // CRD ch.32 R-P3: the receiver's own clock, recorded before anything else.
    pubkey_state_mut(&mut ordermatch_ctx.trie_store.lock().pubkeys_state, from_pubkey).last_keep_alive =
        now_ms() / 1000;

    if ordermatch_ctx
        .with_consistent_state(catch_up_bound_ms, |_orderbook, _trie_store| ())
        .await
        .is_err()
    {
        log::debug!(
            "Trie store is behind the orderbook index, comparing the keep-alive of pubkey {} with it anyway",
            from_pubkey
        );
    }

    let subscribed_topics: HashSet<String> = {
        let subs = ordermatch_ctx.orderbook_subscriptions.read();
        subs.keys().cloned().collect()
    };

    let mut trie_store = ordermatch_ctx.trie_store.lock();
    let request = trie_store.prepare_sync_request_for_keep_alive(
        from_pubkey,
        keep_alive,
        i_am_relay,
        request_root,
        |topic: &str| subscribed_topics.contains(topic),
    )?;
    let in_flight = match &request {
        OrdermatchRequest::SyncPubkeyOrderbookState { trie_roots, .. } if deferral_ms > 0 => Some(
            ordermatch_ctx
                .sync_answer_deferral
                .track_own_request(from_pubkey, trie_roots.keys()),
        ),
        _ => None,
    };
    Some((request, in_flight))
}

pub(crate) fn process_maker_order_updated(
    ctx: MmArc,
    from_pubkey: String,
    updated_msg: new_protocol::MakerOrderUpdated,
) -> bool {
    let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).expect("from_ctx failed");
    let uuid = updated_msg.uuid();

    // Phase 1: mutate in-memory order, build trie ops and queue them under the same guard
    // (CRD ch.32 R-T3b item 1).
    {
        let mut orderbook = ordermatch_ctx.orderbook.lock();
        match orderbook.find_order_by_uuid_and_pubkey(&uuid, &from_pubkey) {
            Some(mut order) => {
                order.apply_updated(&updated_msg);
                let ops = orderbook.index_insert_or_update(order);
                orderbook.queue_trie_ops(&ordermatch_ctx.trie_ops_tx, ops);
            },
            None => {
                log::warn!(
                    "Couldn't find an order {}, ignoring, it will be synced upon pubkey keep alive",
                    uuid
                );
                return false;
            },
        }
    }
    true
}

// fn verify_pubkey_orderbook(orderbook: &GetOrderbookPubkeyItem) -> Result<(), String> {
//     let keys: Vec<(_, _)> = orderbook
//         .orders
//         .iter()
//         .map(|(uuid, order)| {
//             let order_bytes = rmp_serde::to_vec(&order).expect("Serialization should never fail");
//             (uuid.as_bytes(), Some(order_bytes))
//         })
//         .collect();
//     let (orders_root, proof) = &orderbook.pair_orders_trie_root;
//     verify_trie_proof::<Layout, _, _, _>(orders_root, proof, &keys)
//         .map_err(|e| ERRL!("Error on pair_orders_trie_root verification: {}", e))?;
//     Ok(())
// }

/// Request best asks and bids for the given `base` and `rel` coins from relays.
/// Set `asks_num` and/or `bids_num` to get corresponding number of best asks and bids or None to get all of the available orders.
///
/// # Safety
///
/// The function locks [`MmCtx::p2p_ctx`] and [`MmCtx::ordermatch_ctx`]
pub(crate) async fn request_and_fill_orderbook(ctx: &MmArc, base: &str, rel: &str) -> Result<(), String> {
    let request = OrdermatchRequest::GetOrderbook {
        base: base.to_string(),
        rel: rel.to_string(),
    };

    let response = try_s!(request_any_relay::<GetOrderbookRes>(ctx.clone(), P2PRequest::Ordermatch(request)).await);
    let (pubkey_orders, protocol_infos, conf_infos) = match response {
        Some((
            GetOrderbookRes {
                pubkey_orders,
                protocol_infos,
                conf_infos,
            },
            _peer_id,
        )) => (pubkey_orders, protocol_infos, conf_infos),
        None => return Ok(()),
    };

    let ordermatch_ctx = OrdermatchContext::from_ctx(ctx).unwrap();

    let my_pubsecp = mm2_internal_pubkey_hex(ctx)?;

    // Phase 1: build all index mutations under the Orderbook lock and queue their trie ops
    // before releasing it (CRD ch.32 R-T3b item 1).
    {
        let mut orderbook = ordermatch_ctx.orderbook.lock();

        let alb_pair = alb_ordered_pair(base, rel);
        let mut all_ops = Vec::new();

        for (pubkey, GetOrderbookPubkeyItem { orders, .. }) in pubkey_orders {
            if is_my_order(&pubkey, &my_pubsecp, &orderbook.my_p2p_pubkeys) {
                continue;
            }

            let pubkey_bytes = match hex::decode(&pubkey) {
                Ok(b) => b,
                Err(e) => {
                    log::warn!("Error {} decoding pubkey {}", e, pubkey);
                    continue;
                },
            };
            if is_pubkey_banned(ctx, &pubkey_bytes[1..].into()) {
                log::warn!("Pubkey {} is banned", pubkey);
                continue;
            }
            let params = ProcessTrieParams {
                pubkey: &pubkey,
                alb_pair: &alb_pair,
                protocol_infos: &protocol_infos,
                conf_infos: &conf_infos,
            };
            let mut pair_ops = process_pubkey_full_trie(&mut orderbook, orders, params);
            all_ops.append(&mut pair_ops);
        }

        // Phase 2: queue for background application, still under the guard.
        orderbook.queue_trie_ops(&ordermatch_ctx.trie_ops_tx, all_ops);
    }

    let topic = orderbook_topic_from_base_rel(base, rel);
    {
        let mut subs = ordermatch_ctx.orderbook_subscriptions.write();
        subs.insert(topic, OrderbookRequestingState::Requested);
    }

    Ok(())
}

/// Insert or update a peer's order.
/// Note this function locks the [`OrdermatchContext::orderbook`] mutex.
pub(crate) fn insert_or_update_order(ctx: &MmArc, item: OrderbookItem) {
    let ordermatch_ctx = OrdermatchContext::from_ctx(ctx).expect("from_ctx failed");
    let p2p_item = OrderbookP2PItem::from(item.clone());
    let topic = orderbook_topic_from_base_rel(&item.base, &item.rel);

    // Phase 1: index under Orderbook lock; Phase 2: the trie ops are queued before the guard is
    // released (CRD ch.32 R-T3b item 1).
    {
        let mut orderbook = ordermatch_ctx.orderbook.lock();
        let ops = orderbook.index_insert_or_update(item);
        orderbook.queue_trie_ops(&ordermatch_ctx.trie_ops_tx, ops);
    }

    let _ = ctx
        .event_stream_manager
        .send_fn(&StreamerId::OrderbookUpdate { topic }, || {
            orderbook_events::OrderbookItemChangeEvent::NewOrUpdatedItem(Box::new(p2p_item))
        });
}

/// Insert or update our own maker order, tracking per-order P2P pubkeys (ZHTLC).
pub(crate) fn insert_or_update_my_order(ctx: &MmArc, item: OrderbookItem, my_order: &MakerOrder) {
    let ordermatch_ctx = OrdermatchContext::from_ctx(ctx).expect("from_ctx failed");
    let p2p_item = OrderbookP2PItem::from(item.clone());
    let topic = orderbook_topic_from_base_rel(&item.base, &item.rel);

    // Phase 1: index + my_p2p_pubkeys under Orderbook lock; Phase 2: the trie ops are queued
    // before the guard is released (CRD ch.32 R-T3b item 1).
    {
        let mut orderbook = ordermatch_ctx.orderbook.lock();
        let ops = orderbook.index_insert_or_update(item);
        if let Some(ref key) = my_order.p2p_privkey {
            orderbook.my_p2p_pubkeys.insert(hex::encode(key.public_slice()));
        }
        orderbook.queue_trie_ops(&ordermatch_ctx.trie_ops_tx, ops);
    }

    let _ = ctx
        .event_stream_manager
        .send_fn(&StreamerId::OrderbookUpdate { topic }, || {
            orderbook_events::OrderbookItemChangeEvent::NewOrUpdatedItem(Box::new(p2p_item))
        });
}

pub(crate) fn delete_order(ctx: &MmArc, pubkey: &str, uuid: Uuid) {
    let ordermatch_ctx = OrdermatchContext::from_ctx(ctx).expect("from_ctx failed");

    // Phase 1: update index and queue the trie op under the same guard (CRD ch.32 R-T3b item 1)
    let maybe_topic = {
        let mut orderbook = ordermatch_ctx.orderbook.lock();

        // Record this UUID so that a late-arriving create message won't resurrect the order.
        orderbook.recently_cancelled.insert(uuid, pubkey.to_string());

        if let Some(order) = orderbook.order_set.get(&uuid) {
            if order.pubkey == pubkey {
                let topic = orderbook_topic_from_base_rel(&order.base, &order.rel);
                orderbook.index_remove(uuid).map(|(_removed, op)| {
                    orderbook.queue_trie_ops(&ordermatch_ctx.trie_ops_tx, vec![op]);
                    topic
                })
            } else {
                None
            }
        } else {
            None
        }
    };

    // Phase 2: emit SSE event
    if let Some(topic) = maybe_topic {
        let _ = ctx
            .event_stream_manager
            .send_fn(&StreamerId::OrderbookUpdate { topic }, || {
                orderbook_events::OrderbookItemChangeEvent::RemovedItem(uuid)
            });
    }
}

pub(crate) fn delete_my_order(ctx: &MmArc, uuid: Uuid, p2p_privkey: Option<&SerializableSecp256k1Keypair>) {
    let ordermatch_ctx: Arc<OrdermatchContext> = OrdermatchContext::from_ctx(ctx).expect("from_ctx failed");

    // Phase 1: index remove + pubkey cleanup; the trie op is queued under the same guard
    // (CRD ch.32 R-T3b item 1)
    let topic = {
        let mut orderbook = ordermatch_ctx.orderbook.lock();
        let topic = orderbook
            .order_set
            .get(&uuid)
            .map(|o| orderbook_topic_from_base_rel(&o.base, &o.rel));
        if let Some((_removed, op)) = orderbook.index_remove(uuid) {
            orderbook.queue_trie_ops(&ordermatch_ctx.trie_ops_tx, vec![op]);
        }
        if let Some(key) = p2p_privkey {
            orderbook.my_p2p_pubkeys.remove(&hex::encode(key.public_slice()));
        }
        topic
    };

    // Phase 2: emit SSE event
    if let Some(topic) = topic {
        let _ = ctx
            .event_stream_manager
            .send_fn(&StreamerId::OrderbookUpdate { topic }, || {
                orderbook_events::OrderbookItemChangeEvent::RemovedItem(uuid)
            });
    }
}

/// Check if an orderbook entry belongs to this node.
///
/// Privacy coins (ZHTLC) use a random keypair per order, so the order's pubkey
/// won't match the persistent secp256k1 key. We check both the persistent pubkey
/// and the set of per-order P2P pubkeys tracked in `Orderbook::my_p2p_pubkeys`.
#[inline(always)]
pub(crate) fn is_my_order(order_pubkey: &str, my_pub: &Option<String>, my_p2p_pubkeys: &HashSet<String>) -> bool {
    my_pub.as_deref() == Some(order_pubkey) || my_p2p_pubkeys.contains(order_pubkey)
}

/// Retrieve this node's persistent secp256k1 pubkey as hex, or `None` if the
/// crypto context has not been initialized yet (browse / no-login mode).
pub(crate) fn mm2_internal_pubkey_hex(ctx: &MmArc) -> Result<Option<String>, String> {
    use crypto::CryptoCtxError;
    match CryptoCtx::from_ctx(ctx) {
        Ok(crypto_ctx) => Ok(Some(crypto_ctx.mm2_internal_pubkey_hex())),
        Err(e) => match e.get_inner() {
            CryptoCtxError::NotInitialized => Ok(None),
            CryptoCtxError::Internal(msg) => Err(msg.clone()),
        },
    }
}

/// CRD ch.59 R59.6.1a: an accepted order message from `pubkey` (the key its signature verified for,
/// in the hex form the trie store keys its records by) counts as liveness, exactly as a keep-alive
/// does (R59.6.1). It only refreshes a record the node already holds; it never creates one.
fn refresh_liveness_from_order_message(ctx: &MmArc, pubkey: &str) {
    if !order_messages_refresh_liveness(ctx) {
        return;
    }
    let ordermatch_ctx = match OrdermatchContext::from_ctx(ctx) {
        Ok(ordermatch_ctx) => ordermatch_ctx,
        Err(e) => {
            log::warn!("Cannot refresh the liveness of pubkey {}: {}", pubkey, e);
            return;
        },
    };
    let mut trie_store = ordermatch_ctx.trie_store.lock();
    if let Some(state) = trie_store.pubkeys_state.get_mut(pubkey) {
        state.last_keep_alive = now_ms() / 1000;
    }
}

/// Attempts to decode a message and process it returning whether the message is valid and worth rebroadcasting
pub async fn process_msg(ctx: MmArc, topics: Vec<String>, from_peer: String, msg: &[u8], i_am_relay: bool) -> bool {
    process_msg_with_priority(ctx, topics, from_peer, msg, i_am_relay)
        .await
        .0
}

/// Like [`process_msg`]; the second value tells whether a relay propagates the message with priority (CRD ch.32
/// §32.9B R-MP1 item 2): a cancellation or a swap-negotiation message.
pub async fn process_msg_with_priority(
    ctx: MmArc,
    _topics: Vec<String>,
    from_peer: String,
    msg: &[u8],
    i_am_relay: bool,
) -> (bool, bool) {
    match decode_signed::<new_protocol::OrdermatchMessage>(msg) {
        Ok((message, _sig, pubkey)) => {
            if is_pubkey_banned(&ctx, &pubkey.unprefixed().into()) {
                log::warn!("Pubkey {} is banned", pubkey.to_hex());
                return (false, false);
            }
            let priority = message.is_priority();
            let propagate = match message {
                new_protocol::OrdermatchMessage::MakerOrderCreated(created_msg) => {
                    let order: OrderbookItem = (created_msg, hex::encode(pubkey.to_bytes().as_slice())).into();
                    insert_or_update_order(&ctx, order);
                    refresh_liveness_from_order_message(&ctx, &pubkey.to_hex());
                    true
                },
                new_protocol::OrdermatchMessage::PubkeyKeepAlive(keep_alive) => {
                    process_orders_keep_alive(ctx, from_peer, pubkey.to_hex(), keep_alive, i_am_relay).await
                },
                new_protocol::OrdermatchMessage::TakerRequest(taker_request) => {
                    let msg = TakerRequest::from_new_proto_and_pubkey(taker_request, pubkey.unprefixed().into());
                    process_taker_request(ctx, pubkey.unprefixed().into(), msg).await;
                    true
                },
                new_protocol::OrdermatchMessage::MakerReserved(maker_reserved) => {
                    let msg = MakerReserved::from_new_proto_and_pubkey(maker_reserved, pubkey.unprefixed().into());
                    // spawn because process_maker_reserved may take significant time to run
                    spawn(process_maker_reserved(ctx, pubkey.unprefixed().into(), msg));
                    true
                },
                new_protocol::OrdermatchMessage::TakerConnect(taker_connect) => {
                    process_taker_connect(ctx, pubkey.unprefixed().into(), taker_connect.into()).await;
                    true
                },
                new_protocol::OrdermatchMessage::MakerConnected(maker_connected) => {
                    process_maker_connected(ctx, pubkey.unprefixed().into(), maker_connected.into()).await;
                    true
                },
                new_protocol::OrdermatchMessage::MakerOrderCancelled(cancelled_msg) => {
                    delete_order(&ctx, &pubkey.to_hex(), cancelled_msg.uuid.into());
                    refresh_liveness_from_order_message(&ctx, &pubkey.to_hex());
                    true
                },
                new_protocol::OrdermatchMessage::MakerOrderUpdated(updated_msg) => {
                    let accepted = process_maker_order_updated(ctx.clone(), pubkey.to_hex(), updated_msg);
                    if accepted {
                        refresh_liveness_from_order_message(&ctx, &pubkey.to_hex());
                    }
                    accepted
                },
            };
            (propagate, priority)
        },
        Err(e) => {
            log::error!("Error {} while decoding signed message", e);
            (false, false)
        },
    }
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum OrdermatchRequest {
    /// Get an orderbook for the given pair.
    GetOrderbook {
        base: String,
        rel: String,
    },
    /// Sync specific pubkey orderbook state if our known Patricia trie state doesn't match the latest keep alive message
    SyncPubkeyOrderbookState {
        pubkey: String,
        /// Request using this condition
        trie_roots: HashMap<AlbOrderedOrderbookPair, H64>,
    },
    BestOrders {
        coin: String,
        action: BestOrdersAction,
        volume: BigRational,
    },
    OrderbookDepth {
        pairs: Vec<(String, String)>,
    },
    /// Request best orders for a specific coin and action limited by the number of results.
    BestOrdersByNumber {
        coin: String,
        action: BestOrdersAction,
        number: usize,
    },
}

#[derive(Debug)]
pub(crate) struct TryFromBytesError(String);

impl From<String> for TryFromBytesError {
    fn from(string: String) -> Self { TryFromBytesError(string) }
}

pub(crate) trait TryFromBytes {
    fn try_from_bytes(bytes: Vec<u8>) -> Result<Self, TryFromBytesError>
    where
        Self: Sized;
}

impl TryFromBytes for String {
    fn try_from_bytes(bytes: Vec<u8>) -> Result<Self, TryFromBytesError> {
        String::from_utf8(bytes).map_err(|e| ERRL!("{}", e).into())
    }
}

impl TryFromBytes for OrderbookP2PItem {
    fn try_from_bytes(bytes: Vec<u8>) -> Result<Self, TryFromBytesError> {
        rmp_serde::from_read(bytes.as_slice()).map_err(|e| ERRL!("{}", e).into())
    }
}

impl TryFromBytes for H64 {
    fn try_from_bytes(bytes: Vec<u8>) -> Result<Self, TryFromBytesError> {
        bytes.try_into().map_err(|e| ERRL!("{:?}", e).into())
    }
}

impl TryFromBytes for Uuid {
    fn try_from_bytes(bytes: Vec<u8>) -> Result<Self, TryFromBytesError> {
        Uuid::from_slice(&bytes).map_err(|e| ERRL!("{}", e).into())
    }
}

pub fn process_peer_request(ctx: MmArc, request: OrdermatchRequest) -> Result<Option<Vec<u8>>, String> {
    log::debug!("Got ordermatch request {:?}", request);
    match request {
        OrdermatchRequest::GetOrderbook { base, rel } => process_get_orderbook_request(ctx, base, rel),
        OrdermatchRequest::SyncPubkeyOrderbookState { pubkey, trie_roots } => {
            let response = process_sync_pubkey_orderbook_state(ctx, pubkey, trie_roots);
            response.map(|res| res.map(|r| encode_message(&r).expect("Serialization failed")))
        },
        OrdermatchRequest::BestOrders { coin, action, volume } => {
            best_orders::process_best_orders_p2p_request(ctx, coin, action, volume)
        },
        OrdermatchRequest::BestOrdersByNumber { coin, action, number } => {
            best_orders::process_best_orders_p2p_request_by_number(ctx, coin, action, number)
        },
        OrdermatchRequest::OrderbookDepth { pairs } => orderbook_depth::process_orderbook_depth_p2p_request(ctx, pairs),
    }
}

pub(crate) type TrieProof = Vec<Vec<u8>>;

#[derive(Debug, Deserialize, Serialize)]
#[cfg_attr(test, derive(PartialEq))]
pub(crate) struct GetOrderbookPubkeyItem {
    /// Timestamp of the latest keep alive message received.
    pub(crate) last_keep_alive: u64,
    /// last signed OrdermatchMessage payload
    pub(crate) last_signed_pubkey_payload: Vec<u8>,
    /// Requested orders.
    pub(crate) orders: PubkeyOrders,
}

/// Do not change this struct as it will break backward compatibility
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[cfg_attr(test, derive(PartialEq))]
pub(crate) struct BaseRelProtocolInfo {
    pub(crate) base: Vec<u8>,
    pub(crate) rel: Vec<u8>,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct GetOrderbookRes {
    /// Asks and bids grouped by pubkey.
    pub(crate) pubkey_orders: HashMap<String, GetOrderbookPubkeyItem>,
    #[serde(default)]
    pub(crate) protocol_infos: HashMap<Uuid, BaseRelProtocolInfo>,
    #[serde(default)]
    pub(crate) conf_infos: HashMap<Uuid, OrderConfirmationsSettings>,
}

pub(crate) struct GetPubkeysOrdersRes {
    pub(crate) total_number_of_orders: usize,
    pub(crate) uuids_by_pubkey: HashMap<String, PubkeyOrders>,
    pub(crate) protocol_infos: HashMap<Uuid, BaseRelProtocolInfo>,
    pub(crate) conf_infos: HashMap<Uuid, OrderConfirmationsSettings>,
}

pub(crate) fn get_pubkeys_orders(orderbook: &Orderbook, base: String, rel: String) -> GetPubkeysOrdersRes {
    let asks = orderbook.unordered.get(&(base.clone(), rel.clone()));
    let bids = orderbook.unordered.get(&(rel, base));

    let asks_num = asks.map(|x| x.len()).unwrap_or(0);
    let bids_num = bids.map(|x| x.len()).unwrap_or(0);
    let total_number_of_orders = asks_num + bids_num;

    // flatten Option(asks) and Option(bids) to avoid cloning
    let orders = asks.iter().chain(bids.iter()).copied().flatten();

    let mut uuids_by_pubkey = HashMap::new();
    let mut protocol_infos = HashMap::new();
    let mut conf_infos = HashMap::new();
    for uuid in orders {
        let order = orderbook
            .order_set
            .get(uuid)
            .expect("Orderbook::ordered contains an uuid that is not in Orderbook::order_set");
        let uuids = uuids_by_pubkey.entry(order.pubkey.clone()).or_insert_with(Vec::new);
        protocol_infos.insert(order.uuid, order.base_rel_proto_info());
        if let Some(info) = order.conf_settings {
            conf_infos.insert(order.uuid, info);
        }
        uuids.push((*uuid, order.clone().into()))
    }

    GetPubkeysOrdersRes {
        total_number_of_orders,
        uuids_by_pubkey,
        protocol_infos,
        conf_infos,
    }
}

pub(crate) fn process_get_orderbook_request(ctx: MmArc, base: String, rel: String) -> Result<Option<Vec<u8>>, String> {
    let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
    let orderbook = ordermatch_ctx.orderbook.lock();

    let pubkeys_orders = get_pubkeys_orders(&orderbook, base, rel);
    if pubkeys_orders.total_number_of_orders > MAX_ORDERS_NUMBER_IN_ORDERBOOK_RESPONSE {
        return ERR!("Orderbook too large");
    }

    let trie_store = ordermatch_ctx.trie_store.lock();

    let orders_to_send = pubkeys_orders
        .uuids_by_pubkey
        .into_iter()
        .map(|(pubkey, orders)| {
            let pubkey_state = trie_store.pubkeys_state.get(&pubkey).ok_or(ERRL!(
                "TrieStore::pubkeys_state is expected to contain the {:?} pubkey",
                pubkey
            ))?;

            let item = GetOrderbookPubkeyItem {
                last_keep_alive: pubkey_state.last_keep_alive,
                orders,
                // TODO save last signed payload to pubkey state
                last_signed_pubkey_payload: vec![],
            };

            Ok((pubkey, item))
        })
        .collect::<Result<HashMap<_, _>, String>>()?;

    let response = GetOrderbookRes {
        pubkey_orders: orders_to_send,
        protocol_infos: pubkeys_orders.protocol_infos,
        conf_infos: pubkeys_orders.conf_infos,
    };
    let encoded = try_s!(encode_message(&response));
    Ok(Some(encoded))
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) enum DeltaOrFullTrie<Key: Eq + std::hash::Hash, Value> {
    Delta(HashMap<Key, Option<Value>>),
    FullTrie(Vec<(Key, Value)>),
}

impl<Key: Eq + std::hash::Hash, V1> DeltaOrFullTrie<Key, V1> {
    pub fn map_to<V2: From<V1>>(self, mut on_each: impl FnMut(&Key, Option<&V1>)) -> DeltaOrFullTrie<Key, V2> {
        match self {
            DeltaOrFullTrie::Delta(delta) => {
                delta.iter().for_each(|(key, val)| on_each(key, val.as_ref()));
                let new_map = delta
                    .into_iter()
                    .map(|(key, value)| (key, value.map(From::from)))
                    .collect();
                DeltaOrFullTrie::Delta(new_map)
            },
            DeltaOrFullTrie::FullTrie(trie) => {
                trie.iter().for_each(|(key, val)| on_each(key, Some(val)));
                let new_trie = trie.into_iter().map(|(key, value)| (key, value.into())).collect();
                DeltaOrFullTrie::FullTrie(new_trie)
            },
        }
    }
}

#[derive(Debug)]
pub(crate) enum TrieDiffHistoryError {
    TrieDbError(Box<trie_db::TrieError<H64, sp_trie::Error>>),
    TryFromBytesError(TryFromBytesError),
    GetterNoneForKeyFromTrie,
}

impl std::fmt::Display for TrieDiffHistoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { write!(f, "({:?})", self) }
}

impl From<TryFromBytesError> for TrieDiffHistoryError {
    fn from(error: TryFromBytesError) -> TrieDiffHistoryError { TrieDiffHistoryError::TryFromBytesError(error) }
}

impl From<Box<trie_db::TrieError<H64, sp_trie::Error>>> for TrieDiffHistoryError {
    fn from(error: Box<trie_db::TrieError<H64, sp_trie::Error>>) -> TrieDiffHistoryError {
        TrieDiffHistoryError::TrieDbError(error)
    }
}

/// What a full-trie read does with a key whose value the getter cannot supply.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum MissingValue {
    /// The trie store and the read index have diverged with nothing queued to explain it: an error
    /// (CRD ch.32 R-T5).
    Fail,
    /// The removal of the value is still queued for the trie worker: leave the key out (CRD ch.32
    /// R-T3b item 2).
    Skip,
}

pub(crate) fn get_full_trie<Key, Value>(
    trie_root: &H64,
    db: &MemoryDB<Blake2Hasher64>,
    getter: impl Fn(&Key) -> Option<Value>,
    on_missing: MissingValue,
) -> Result<Vec<(Key, Value)>, TrieDiffHistoryError>
where
    Key: Clone + Eq + std::hash::Hash + TryFromBytes,
{
    let trie = TrieDB::<Layout>::new(db, trie_root)?;
    let mut values = Vec::new();
    for key_value in trie.iter()? {
        let (key, _) = key_value?;
        let key = TryFromBytes::try_from_bytes(key)?;
        match (getter(&key), on_missing) {
            (Some(val), _) => values.push((key, val)),
            (None, MissingValue::Skip) => (),
            (None, MissingValue::Fail) => return Err(TrieDiffHistoryError::GetterNoneForKeyFromTrie),
        }
    }
    Ok(values)
}

impl<Key: Clone + Eq + std::hash::Hash + TryFromBytes, Value: Clone> DeltaOrFullTrie<Key, Value> {
    pub(crate) fn from_history(
        history: &TrieDiffHistory<Key, Value>,
        from_hash: H64,
        actual_trie_root: H64,
        db: &MemoryDB<Blake2Hasher64>,
        getter: impl Fn(&Key) -> Option<Value>,
        on_missing: MissingValue,
    ) -> Result<DeltaOrFullTrie<Key, Value>, TrieDiffHistoryError> {
        if let Some(delta) = history.get(&from_hash) {
            let mut current_delta = delta;
            let mut total_delta = HashMap::new();
            total_delta.extend(delta.delta.iter().cloned());
            while let Some(cur) = history.get(&current_delta.next_root) {
                current_delta = cur;
                total_delta.extend(current_delta.delta.iter().cloned());
            }
            if current_delta.next_root == actual_trie_root {
                return Ok(DeltaOrFullTrie::Delta(total_delta));
            }

            log::warn!(
                "History started from {:?} ends with not up-to-date trie root {:?}",
                from_hash,
                actual_trie_root
            );
        }

        let trie = get_full_trie(&actual_trie_root, db, getter, on_missing)?;
        Ok(DeltaOrFullTrie::FullTrie(trie))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct SyncPubkeyOrderbookStateRes {
    /// last signed OrdermatchMessage payload from pubkey
    pub(crate) last_signed_pubkey_payload: Vec<u8>,
    pub(crate) pair_orders_diff: HashMap<AlbOrderedOrderbookPair, DeltaOrFullTrie<Uuid, OrderbookP2PItem>>,
    #[serde(default)]
    pub(crate) protocol_infos: HashMap<Uuid, BaseRelProtocolInfo>,
    #[serde(default)]
    pub(crate) conf_infos: HashMap<Uuid, OrderConfirmationsSettings>,
}

/// Why a synchronization answer failed, by kind. The kind is decided where the failure
/// happens, never from the text.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum SyncAnswerFailure {
    /// A requested pair has no root.
    MissingPair,
    /// A requested pair is held at the all-zero placeholder root.
    Placeholder,
    /// Any other trie or getter failure.
    Trie,
}

impl From<SyncAnswerFailure> for AnswerClass {
    fn from(failure: SyncAnswerFailure) -> Self {
        match failure {
            SyncAnswerFailure::MissingPair => AnswerClass::MissingPair,
            SyncAnswerFailure::Placeholder => AnswerClass::Placeholder,
            SyncAnswerFailure::Trie => AnswerClass::TrieError,
        }
    }
}

/// The failure of a synchronization answer. `text` is what the peer is sent, in the form the `ERRL!` and
/// `try_s!` macros give it (the file name and line, then the reason).
#[derive(Debug)]
pub(crate) struct SyncAnswerError {
    pub(crate) failure: SyncAnswerFailure,
    pub(crate) text: String,
}

/// CRD ch.32 R-T3b items 2 and 3: the answer is built from the trie store as it stands, without
/// waiting and without refusing. While the worker still has queued events, a full snapshot leaves
/// out an order the read index no longer holds; once it has caught up, such an order is the
/// divergence of R-T5 and fails the answer.
pub(crate) fn process_sync_pubkey_orderbook_state(
    ctx: MmArc,
    pubkey: String,
    trie_roots: HashMap<AlbOrderedOrderbookPair, H64>,
) -> Result<Option<SyncPubkeyOrderbookStateRes>, String> {
    process_sync_pubkey_orderbook_state_classified(ctx, pubkey, trie_roots).map_err(|e| e.text)
}

/// [`process_sync_pubkey_orderbook_state`] with the kind of the failure, for the synchronization logging.
pub(crate) fn process_sync_pubkey_orderbook_state_classified(
    ctx: MmArc,
    pubkey: String,
    trie_roots: HashMap<AlbOrderedOrderbookPair, H64>,
) -> Result<Option<SyncPubkeyOrderbookStateRes>, SyncAnswerError> {
    let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
    // Lock order: `orderbook`, then `trie_store`.
    let orderbook = ordermatch_ctx.orderbook.lock();
    let trie_store = ordermatch_ctx.trie_store.lock();
    let on_missing = if orderbook.trie_ops_enqueued == trie_store.applied_batches {
        MissingValue::Fail
    } else {
        MissingValue::Skip
    };
    build_sync_pubkey_orderbook_state_answer(&orderbook, &trie_store, &pubkey, trie_roots, on_missing)
}

fn build_sync_pubkey_orderbook_state_answer(
    orderbook: &Orderbook,
    trie_store: &TrieStore,
    pubkey: &str,
    trie_roots: HashMap<AlbOrderedOrderbookPair, H64>,
    on_missing: MissingValue,
) -> Result<Option<SyncPubkeyOrderbookStateRes>, SyncAnswerError> {
    let pubkey_state = match trie_store.pubkeys_state.get(pubkey) {
        Some(s) => s,
        None => return Ok(None),
    };

    let order_getter = |uuid: &Uuid| orderbook.order_set.get(uuid).cloned();
    let pair_orders_diff: Result<HashMap<_, _>, SyncAnswerError> = trie_roots
        .into_iter()
        .map(|(pair, root)| {
            let actual_pair_root = pubkey_state.trie_roots.get(&pair).ok_or_else(|| SyncAnswerError {
                failure: SyncAnswerFailure::MissingPair,
                text: ERRL!("No pair trie root for {}", pair),
            })?;

            let delta_result = match pubkey_state.order_pairs_trie_state_history.get(&pair) {
                Some(history) => DeltaOrFullTrie::from_history(
                    history,
                    root,
                    *actual_pair_root,
                    &trie_store.memory_db,
                    order_getter,
                    on_missing,
                ),
                None => get_full_trie(actual_pair_root, &trie_store.memory_db, order_getter, on_missing)
                    .map(DeltaOrFullTrie::FullTrie),
            };

            // The held root of a placeholder pair is all zeros, which no trie has: the read fails there.
            let delta = delta_result.map_err(|e| SyncAnswerError {
                failure: if *actual_pair_root == H64::default() {
                    SyncAnswerFailure::Placeholder
                } else {
                    SyncAnswerFailure::Trie
                },
                text: ERRL!("{}", e),
            })?;
            Ok((pair, delta))
        })
        .collect();

    let pair_orders_diff = pair_orders_diff.map_err(|e| SyncAnswerError {
        failure: e.failure,
        text: ERRL!("{}", e.text),
    })?;
    let mut protocol_infos = HashMap::new();
    let mut conf_infos = HashMap::new();
    let pair_orders_diff = pair_orders_diff
        .into_iter()
        .map(|(pair, trie)| {
            let new_trie = trie.map_to(|uuid, order| match order {
                Some(o) => {
                    protocol_infos.insert(o.uuid, BaseRelProtocolInfo {
                        base: o.base_protocol_info.clone(),
                        rel: o.rel_protocol_info.clone(),
                    });
                    if let Some(info) = o.conf_settings {
                        conf_infos.insert(o.uuid, info);
                    }
                },
                None => {
                    protocol_infos.remove(uuid);
                    conf_infos.remove(uuid);
                },
            });
            (pair, new_trie)
        })
        .collect();
    let last_signed_pubkey_payload = vec![];
    let result = SyncPubkeyOrderbookStateRes {
        last_signed_pubkey_payload,
        pair_orders_diff,
        protocol_infos,
        conf_infos,
    };
    Ok(Some(result))
}

pub(crate) fn alb_ordered_pair(base: &str, rel: &str) -> AlbOrderedOrderbookPair {
    let (first, second) = if base < rel { (base, rel) } else { (rel, base) };
    let mut res = first.to_owned();
    res.push(':');
    res.push_str(second);
    res
}

pub(crate) fn orderbook_topic_from_base_rel(base: &str, rel: &str) -> String {
    pub_sub_topic(ORDERBOOK_PREFIX, &alb_ordered_pair(base, rel))
}

pub(crate) fn orderbook_topic_from_ordered_pair(pair: &str) -> String { pub_sub_topic(ORDERBOOK_PREFIX, pair) }

#[test]
pub(crate) fn test_alb_ordered_pair() {
    assert_eq!("BTC:KMD", alb_ordered_pair("KMD", "BTC"));
    assert_eq!("BTCH:KMD", alb_ordered_pair("KMD", "BTCH"));
    assert_eq!("KMD:QTUM", alb_ordered_pair("QTUM", "KMD"));
}

#[allow(dead_code)]
pub(crate) fn parse_orderbook_pair_from_topic(topic: &str) -> Option<(&str, &str)> {
    let mut split = topic.split(TOPIC_SEPARATOR);
    match split.next() {
        Some(ORDERBOOK_PREFIX) => match split.next() {
            Some(maybe_pair) => {
                let colon = maybe_pair.find(':');
                match colon {
                    Some(index) => {
                        if index + 1 < maybe_pair.len() {
                            Some((&maybe_pair[..index], &maybe_pair[index + 1..]))
                        } else {
                            None
                        }
                    },
                    None => None,
                }
            },
            None => None,
        },
        _ => None,
    }
}

#[test]
pub(crate) fn test_parse_orderbook_pair_from_topic() {
    assert_eq!(Some(("BTC", "KMD")), parse_orderbook_pair_from_topic("orbk/BTC:KMD"));
    assert_eq!(None, parse_orderbook_pair_from_topic("orbk/BTC:"));
}
/// The order is ordered by [`OrderbookItem::price`] and [`OrderbookItem::uuid`].
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct OrderedByPriceOrder {
    pub(crate) price: MmNumber,
    pub(crate) uuid: Uuid,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum OrderbookRequestingState {
    /// The orderbook was requested from relays.
    #[allow(dead_code)]
    Requested,
    /// We subscribed to a topic at `subscribed_at` time, but the orderbook was not requested.
    NotRequested { subscribed_at: u64 },
}

pub(crate) type H64 = [u8; 8];

/// A narrow contract for trie mutations. The Orderbook builds these ops,
/// TrieStore applies them (and only TrieStore mutates MemoryDB/history).
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum TrieOp {
    /// Reset an entire (pubkey, pair) subtrie.
    /// Drops the subtrie's root and delta history for this (pubkey, pair).
    ClearPair { pubkey: String, alb_pair: String },
    Insert {
        pubkey: String,
        alb_pair: String,
        uuid: Uuid,
        /// Full OrderbookItem is needed to maintain delta history.
        order: OrderbookItem,
    },
    Remove {
        pubkey: String,
        alb_pair: String,
        uuid: Uuid,
    },
    /// Remove all trie state for a pubkey after prior per-UUID removals have been applied.
    RemovePubkey { pubkey: String },
    #[cfg(test)]
    /// Barrier op: notify when all previous ops have been applied.
    Flush(oneshot::Sender<()>),
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct TrieDiff<Key, Value> {
    pub(crate) delta: Vec<(Key, Option<Value>)>,
    pub(crate) next_root: H64,
}

#[derive(Debug)]
pub(crate) struct TrieDiffHistory<Key, Value> {
    pub(crate) inner: TimeCache<H64, TrieDiff<Key, Value>>,
}

/// The most deltas the history of one `(pubkey, pair)` slice holds (CRD ch.32 R-K6). The oldest delta
/// is dropped when a recorded one would exceed it, whether or not it has reached its time to live.
pub(crate) const TRIE_DIFF_HISTORY_CAP: usize = 2048;

impl<Key, Value> TrieDiffHistory<Key, Value> {
    /// Record the change `insert_at -> diff.next_root`. Returns the number of older deltas dropped to
    /// keep the history within [`TRIE_DIFF_HISTORY_CAP`] (CRD ch.32 R-K6); a requester whose root was
    /// among them gets the full snapshot, like one whose root expired.
    pub(crate) fn insert_new_diff(&mut self, insert_at: H64, diff: TrieDiff<Key, Value>) -> usize {
        if insert_at == diff.next_root {
            // do nothing to avoid cycles in diff history
            return 0;
        }

        match self.inner.remove(diff.next_root) {
            Some(mut diff) => {
                // we reached a state that was already reached previously
                // history can be cleaned up to this state hash
                while let Some(next_diff) = self.inner.remove(diff.next_root) {
                    diff = next_diff;
                }
                0
            },
            None => {
                self.inner.insert(insert_at, diff);
                let mut evicted = 0;
                while self.inner.len() > TRIE_DIFF_HISTORY_CAP && self.inner.pop_oldest().is_some() {
                    evicted += 1;
                }
                evicted
            },
        }
    }

    #[allow(dead_code)]
    pub(crate) fn remove_key(&mut self, key: H64) { self.inner.remove(key); }

    #[allow(dead_code)]
    pub(crate) fn contains_key(&self, key: &H64) -> bool { self.inner.contains_key(key) }

    pub(crate) fn get(&self, key: &H64) -> Option<&TrieDiff<Key, Value>> { self.inner.get(key) }

    #[allow(dead_code)]
    pub(crate) fn len(&self) -> usize { self.inner.len() }
}

pub(crate) type TrieOrderHistory = TrieDiffHistory<Uuid, OrderbookItem>;

pub(crate) struct OrderbookPubkeyState {
    /// Timestamp of the latest keep alive message received
    pub(crate) last_keep_alive: u64,
    /// The map storing historical data about specific pair subtrie changes
    /// Used to get diffs of orders of pair between specific root hashes
    pub(crate) order_pairs_trie_state_history: TimeCache<AlbOrderedOrderbookPair, TrieOrderHistory>,
    /// The known UUIDs owned by pubkey with alphabetically ordered pair to ease the lookup during pubkey orderbook requests
    pub(crate) orders_uuids: HashSet<(Uuid, AlbOrderedOrderbookPair)>,
    /// The map storing alphabetically ordered pair with trie root hash of orders owned by pubkey.
    pub(crate) trie_roots: HashMap<AlbOrderedOrderbookPair, H64>,
}

impl OrderbookPubkeyState {
    pub fn with_history_timeout(ttl: Duration) -> OrderbookPubkeyState {
        OrderbookPubkeyState {
            last_keep_alive: now_ms() / 1000,
            order_pairs_trie_state_history: TimeCache::new(ttl),
            orders_uuids: HashSet::default(),
            trie_roots: HashMap::default(),
        }
    }
}

pub(crate) fn get_trie_mut<'a>(
    mem_db: &'a mut MemoryDB<Blake2Hasher64>,
    root: &'a mut H64,
) -> Result<TrieDBMut<'a, Layout>, String> {
    if *root == H64::default() {
        Ok(TrieDBMut::new(mem_db, root))
    } else {
        TrieDBMut::from_existing(mem_db, root).map_err(|e| ERRL!("{:?}", e))
    }
}

pub(crate) fn pubkey_state_mut<'a>(
    state: &'a mut HashMap<String, OrderbookPubkeyState>,
    from_pubkey: &str,
) -> &'a mut OrderbookPubkeyState {
    state
        .entry(from_pubkey.to_string())
        .or_insert_with(|| OrderbookPubkeyState::with_history_timeout(Duration::new(TRIE_STATE_HISTORY_TIMEOUT, 0)))
}

pub(crate) fn order_pair_root_mut<'a>(state: &'a mut HashMap<AlbOrderedOrderbookPair, H64>, pair: &str) -> &'a mut H64 {
    state.entry(pair.to_string()).or_default()
}

pub(crate) fn pair_history_mut<'a>(
    state: &'a mut TimeCache<AlbOrderedOrderbookPair, TrieOrderHistory>,
    pair: &str,
) -> &'a mut TrieOrderHistory {
    state
        .entry(pair.into())
        .or_insert_with_update_expiration(|| TrieOrderHistory {
            inner: TimeCache::new(Duration::from_secs(TRIE_ORDER_HISTORY_TIMEOUT)),
        })
}

/// `parity_util_mem::malloc_size` crushes for some reason on wasm32
#[cfg(target_arch = "wasm32")]
pub(crate) fn collect_orderbook_metrics(_ctx: &MmArc, _orderbook: &Orderbook) {}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn collect_orderbook_metrics(ctx: &MmArc, orderbook: &Orderbook) {
    mm_gauge!(ctx.metrics, "orderbook.len", orderbook.order_set.len() as i64);
}

/// Remove the nodes of a pair trie from `memory_db`: every leaf is deleted from the trie the way
/// `TrieStore::apply_remove` deletes one, so the reference count of each node the trie held drops by
/// one. A node that another trie holds too (nodes are content addressed) stays readable for it; a
/// node nobody refers to any more is reclaimed by the next `MemoryDB::purge`.
///
/// The zero root of a pair that was announced but never synchronized, and the root of an empty
/// trie, hold no nodes.
fn release_pair_trie(memory_db: &mut MemoryDB<Blake2Hasher64>, root: &H64) {
    if *root == H64::default() || *root == hashed_null_node::<Layout>() {
        return;
    }
    let keys = match collect_trie_keys(memory_db, root) {
        Ok(keys) => keys,
        Err(e) => {
            log::error!("Failed to read the keys of the trie with root {:?}: {:?}", root, e);
            return;
        },
    };
    if let Err(e) =
        delta_trie_root::<Layout, _, _, _, _, _>(memory_db, *root, keys.into_iter().map(|key| (key, None::<Vec<u8>>)))
    {
        log::error!("Failed to remove the nodes of the trie with root {:?}: {:?}", root, e);
    }
}

fn collect_trie_keys(
    memory_db: &MemoryDB<Blake2Hasher64>,
    root: &H64,
) -> Result<Vec<Vec<u8>>, Box<trie_db::TrieError<H64, sp_trie::Error>>> {
    let trie = TrieDB::<Layout>::new(memory_db, root)?;
    let mut keys = Vec::new();
    for key_value in trie.iter()? {
        let (key, _) = key_value?;
        keys.push(key);
    }
    Ok(keys)
}

/// What the maintenance tick of the trie store reports (see [`TrieStore::snapshot`]).
pub(crate) struct TrieStoreSnapshot {
    pub(crate) applied_batches: u64,
    /// Pubkeys the store tracks.
    pub(crate) pubkeys: usize,
    /// Pair histories over all pubkeys.
    pub(crate) history_pairs: usize,
    /// Diffs held by all pair histories.
    pub(crate) history_deltas: usize,
    /// Nodes of `memory_db` some trie refers to (nodes awaiting the next purge are not counted).
    /// `None` unless the snapshot was asked to measure `memory_db`: counting is O(nodes).
    pub(crate) memory_db_entries: Option<usize>,
    /// Heap size of `memory_db`, table capacity included; `None` like `memory_db_entries`.
    /// `malloc_size` is unusable on wasm32.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) memory_db_bytes: Option<usize>,
}

/// Emit the gauges of the trie store and of the queue that feeds it; aggregates only, because a seed
/// tracks many pubkeys.
pub(crate) fn collect_trie_store_metrics(ctx: &MmArc, snapshot: &TrieStoreSnapshot, enqueued_batches: u64) {
    // `applied_batches` is read before `enqueued_batches` and both only grow, so the difference does
    // not go below zero; the saturation only guards the subtraction itself.
    let pending = enqueued_batches.saturating_sub(snapshot.applied_batches);
    mm_gauge!(ctx.metrics, "orderbook.trie_ops.pending", pending as i64);
    mm_gauge!(ctx.metrics, "orderbook.pubkeys", snapshot.pubkeys as i64);
    mm_gauge!(ctx.metrics, "orderbook.history.pairs", snapshot.history_pairs as i64);
    mm_gauge!(ctx.metrics, "orderbook.history.deltas", snapshot.history_deltas as i64);
    if let Some(entries) = snapshot.memory_db_entries {
        mm_gauge!(ctx.metrics, "orderbook.memory_db.entries", entries as i64);
    }
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(bytes) = snapshot.memory_db_bytes {
        mm_gauge!(ctx.metrics, "orderbook.memory_db", bytes as i64);
    }
}

/// Trie-related state extracted from `Orderbook` to reduce contention on the main
/// order index. All trie operations go through this store via the async TrieOp channel.
#[derive(Default)]
pub(crate) struct TrieStore {
    /// A map of orderbook states of known maker pubkeys.
    pub(crate) pubkeys_state: HashMap<String, OrderbookPubkeyState>,
    /// MemoryDB instance to store Patricia Tries data.
    pub(crate) memory_db: MemoryDB<Blake2Hasher64>,
    /// Number of queued batches the worker has applied (CRD ch.32 R-T3b); see
    /// [`Orderbook::trie_ops_enqueued`].
    pub(crate) applied_batches: u64,
    /// Leaf insertions and removals applied since the node store was last purged (CRD ch.32 R-K5
    /// item 2).
    pub(crate) leaf_updates_since_purge: u64,
    /// When the node store was last purged (early or on the timer), milliseconds since the epoch;
    /// zero before the first purge.
    last_purge_ms: u64,
    /// Early purges done so far (`orderbook.trie_store.early_purges`, CRD ch.32 R-K5 item 3).
    pub(crate) early_purges: u64,
    /// Deltas dropped by the cap on a pair history so far (`orderbook.history.evicted`, CRD ch.32 R-K6).
    pub(crate) history_evicted: u64,
}

/// Leaf updates (insertions and removals) after which the worker purges the node store before the
/// timer does (CRD ch.32 R-K5 item 2): about 35 MB of unreferenced nodes.
pub(crate) const EARLY_PURGE_LEAF_UPDATES: u64 = 100_000;
/// The least time between two purges of the node store when the next one is due by the number of
/// leaf updates (CRD ch.32 R-K5 item 2).
pub(crate) const EARLY_PURGE_MIN_INTERVAL_MS: u64 = 5_000;
/// The most queued batches the worker applies as one sequence (CRD ch.32 R-K5 item 1).
pub(crate) const DRAIN_MAX_BATCHES: usize = 1024;
/// The most events the worker applies as one sequence (CRD ch.32 R-K5 item 1). A single batch larger
/// than this is applied alone. It bounds how long the worker holds the trie store lock, which the
/// readers that hold the orderbook lock wait for: measured in release on a loaded machine, 2,048 events of
/// new orders took a median of 26 to 45 ms, 4,096 events 52 to 90 ms and 16,384 events 232 to 374 ms.
pub(crate) const DRAIN_MAX_EVENTS: usize = 2048;

impl TrieStore {
    /// Apply one queued batch and count it. A batch made only of the test-only `Flush` barrier is not
    /// counted because it is not queued through [`Orderbook::queue_trie_ops`]. The worker applies
    /// everything waiting as one sequence ([`TrieStore::apply_batches`]); this is the one-by-one
    /// reference of it.
    #[cfg(test)]
    pub(crate) fn apply_batch(&mut self, ops: Vec<TrieOp>) { self.apply_batches(vec![ops]); }

    /// Apply the queued `batches`, in queue order, as one sequence (CRD ch.32 R-K5 item 1): the
    /// outcome is that of applying them one by one (R-T3a), only the delta history can hold fewer
    /// entries. Counts every batch as applied.
    pub(crate) fn apply_batches(&mut self, batches: Vec<Vec<TrieOp>>) {
        // A batch made only of the test-only `Flush` barrier is not queued through
        // `Orderbook::queue_trie_ops` and so is not counted.
        #[cfg(test)]
        let counted = batches
            .iter()
            .filter(|ops| !ops.iter().any(|op| matches!(op, TrieOp::Flush(_))))
            .count() as u64;
        #[cfg(not(test))]
        let counted = batches.len() as u64;
        self.apply_ops(batches.into_iter().flatten());
        self.applied_batches += counted;
    }

    /// Purge the unreferenced nodes now if 100,000 leaf updates were applied since the last purge and
    /// the last purge is at least 5 s old (CRD ch.32 R-K5 item 2). The table is not shrunk: that stays
    /// with the timer purge. Returns whether it purged.
    pub(crate) fn purge_if_loaded(&mut self, now_ms: u64) -> bool {
        if self.leaf_updates_since_purge < EARLY_PURGE_LEAF_UPDATES
            || now_ms.saturating_sub(self.last_purge_ms) < EARLY_PURGE_MIN_INTERVAL_MS
        {
            return false;
        }
        self.memory_db.purge();
        self.leaf_updates_since_purge = 0;
        self.last_purge_ms = now_ms;
        self.early_purges += 1;
        true
    }

    /// Apply a sequence of trie operations produced by the Orderbook.
    /// This is the only place mutating MemoryDB and trie histories.
    pub(crate) fn apply_ops<I>(&mut self, ops: I)
    where
        I: IntoIterator<Item = TrieOp>,
    {
        // CRD ch.32 R-T3a: the outcome of a batch MUST equal applying its events one at a time in
        // queue order. Grouping (R-T3) only coalesces events whose effect is provably identical:
        //  * per uuid, a later insert supersedes everything earlier (the final leaf is the inserted
        //    value either way); a remove after an insert is kept next to it (the insert still
        //    shapes the trie/pubkey state it would have created);
        //  * a `ClearPair` discards the earlier pending events of its slice (a clear erases their
        //    effect), later events survive;
        //  * `RemovePubkey` is a barrier: pending groups of that pubkey are applied first.
        // Intermediate states are never observable (readers take the trie store lock), but the
        // diff history may hold fewer entries than sequential application would produce.
        #[derive(Default)]
        struct UuidPending {
            insert: Option<(usize, OrderbookItem)>,
            remove: Option<usize>,
        }
        #[derive(Default)]
        struct Group {
            /// Some event (possibly discarded by a clear) touched the pubkey state.
            touched: bool,
            clear: bool,
            pending: HashMap<Uuid, UuidPending>,
        }

        fn apply_group(store: &mut TrieStore, pubkey: &str, alb_pair: &str, g: Group) {
            if g.touched {
                // Sequential application would have created the pubkey state.
                pubkey_state_mut(&mut store.pubkeys_state, pubkey);
            }
            if g.clear {
                store.apply_clear_pair(pubkey, alb_pair);
            }
            let mut inserts = Vec::new();
            let mut removes = Vec::new();
            for (uuid, p) in g.pending {
                if let Some((seq, order)) = p.insert {
                    inserts.push((seq, uuid, order));
                }
                if let Some(seq) = p.remove {
                    removes.push((seq, uuid));
                }
            }
            inserts.sort_by_key(|(seq, ..)| *seq);
            removes.sort_by_key(|(seq, _)| *seq);
            for (_, uuid, order) in inserts {
                store.apply_insert(pubkey, alb_pair, uuid, order);
            }
            for (_, uuid) in removes {
                store.apply_remove(pubkey, alb_pair, uuid);
            }
        }

        let mut groups: HashMap<(String, String), Group> = HashMap::new();
        let mut seq = 0usize;
        #[cfg(test)]
        let mut flush_senders: Vec<oneshot::Sender<()>> = Vec::new();

        for op in ops {
            seq += 1;
            match op {
                TrieOp::ClearPair { pubkey, alb_pair } => {
                    let g = groups.entry((pubkey, alb_pair)).or_default();
                    g.clear = true;
                    g.pending.clear();
                },
                TrieOp::Insert {
                    pubkey,
                    alb_pair,
                    uuid,
                    order,
                } => {
                    let g = groups.entry((pubkey, alb_pair)).or_default();
                    g.touched = true;
                    g.pending.insert(uuid, UuidPending {
                        insert: Some((seq, order)),
                        remove: None,
                    });
                },
                TrieOp::Remove { pubkey, alb_pair, uuid } => {
                    let g = groups.entry((pubkey, alb_pair)).or_default();
                    g.touched = true;
                    g.pending.entry(uuid).or_default().remove = Some(seq);
                },
                TrieOp::RemovePubkey { pubkey } => {
                    let keys: Vec<_> = groups.keys().filter(|(pk, _)| *pk == pubkey).cloned().collect();
                    for key in keys {
                        if let Some(g) = groups.remove(&key) {
                            apply_group(self, &key.0, &key.1, g);
                        }
                    }
                    if let Some(state) = self.pubkeys_state.remove(&pubkey) {
                        for root in state.trie_roots.values() {
                            release_pair_trie(&mut self.memory_db, root);
                        }
                    }
                },
                #[cfg(test)]
                TrieOp::Flush(done) => {
                    flush_senders.push(done);
                },
            }
        }

        for ((pubkey, alb_pair), g) in groups {
            apply_group(self, &pubkey, &alb_pair, g);
        }

        #[cfg(test)]
        for tx in flush_senders {
            let _ = tx.send(());
        }
    }

    pub(crate) fn apply_insert(&mut self, pubkey: &str, alb_pair: &str, uuid: Uuid, order: OrderbookItem) {
        let pubkey_state = pubkey_state_mut(&mut self.pubkeys_state, pubkey);
        let pair_root = order_pair_root_mut(&mut pubkey_state.trie_roots, alb_pair);
        let prev_root = *pair_root;

        pubkey_state.orders_uuids.insert((uuid, alb_pair.to_owned()));

        {
            let mut pair_trie = match get_trie_mut(&mut self.memory_db, pair_root) {
                Ok(trie) => trie,
                Err(e) => {
                    log::error!("Error {} getting trie with root {:?}", e, prev_root);
                    return;
                },
            };
            let order_bytes = order.trie_state_bytes();
            if let Err(e) = pair_trie.insert(uuid.as_bytes(), &order_bytes) {
                log::error!("Error {:?} on insertion to trie. Key {}", e, uuid);
                return;
            };
        }
        self.leaf_updates_since_purge += 1;

        if prev_root != H64::default() {
            let history = pair_history_mut(&mut pubkey_state.order_pairs_trie_state_history, alb_pair);
            self.history_evicted += history.insert_new_diff(prev_root, TrieDiff {
                delta: vec![(uuid, Some(order))],
                next_root: *pair_root,
            }) as u64;
        }
    }

    pub(crate) fn apply_remove(&mut self, pubkey: &str, alb_pair: &str, uuid: Uuid) {
        let pubkey_state = pubkey_state_mut(&mut self.pubkeys_state, pubkey);
        let pair_state = order_pair_root_mut(&mut pubkey_state.trie_roots, alb_pair);
        let old_state = *pair_state;

        pubkey_state.orders_uuids.remove(&(uuid, alb_pair.to_owned()));

        *pair_state = match delta_trie_root::<Layout, _, _, _, _, _>(&mut self.memory_db, old_state, vec![(
            *uuid.as_bytes(),
            None::<Vec<u8>>,
        )]) {
            Ok(root) => root,
            Err(_) => {
                log::error!("Failed to get existing trie with root {:?}", pair_state);
                return;
            },
        };
        self.leaf_updates_since_purge += 1;

        if pubkey_state
            .order_pairs_trie_state_history
            .get(&alb_pair.to_owned())
            .is_some()
        {
            let history = pair_history_mut(&mut pubkey_state.order_pairs_trie_state_history, alb_pair);
            self.history_evicted += history.insert_new_diff(old_state, TrieDiff {
                delta: vec![(uuid, None)],
                next_root: *pair_state,
            }) as u64;
        }
    }

    pub(crate) fn apply_clear_pair(&mut self, pubkey: &str, alb_pair: &str) {
        if let Some(pubkey_state) = self.pubkeys_state.get_mut(pubkey) {
            pubkey_state.order_pairs_trie_state_history.remove(alb_pair.into());
            pubkey_state.orders_uuids.retain(|(_uuid, pair)| pair != alb_pair);
            if let Some(root) = pubkey_state.trie_roots.remove(alb_pair) {
                release_pair_trie(&mut self.memory_db, &root);
            }
        }
    }

    /// The periodic upkeep of the trie store: the histories drop what is past its time to live and
    /// give back the room the burst that filled them left behind. Expiry otherwise happens only when
    /// a pair receives its next update, and an idle pair would keep its whole history.
    ///
    /// A pair history is never removed because its diffs ran out: `apply_remove` records a diff only
    /// while the pair history exists, so the pair histories leave by their own time to live only.
    pub(crate) fn expire_histories(&mut self) {
        for pubkey_state in self.pubkeys_state.values_mut() {
            let pair_histories = &mut pubkey_state.order_pairs_trie_state_history;
            pair_histories.remove_expired();
            for history in pair_histories.values_mut() {
                history.inner.remove_expired();
                history.inner.shrink_if_sparse();
            }
            pair_histories.shrink_if_sparse();
        }
    }

    /// Drop the nodes no trie refers to any more and shrink the table that held them.
    pub(crate) fn purge_memory_db(&mut self) {
        self.memory_db.purge();
        self.memory_db.shrink_to_fit();
        // The early purge counts from this one (CRD ch.32 R-K5 item 2).
        self.leaf_updates_since_purge = 0;
        self.last_purge_ms = now_ms();
    }

    /// Aggregate counts for the gauges of the maintenance tick. The per-tick counts walk the pubkeys
    /// and pair histories only; `memory_db` is measured (O(nodes)) just when `with_memory_db` is set,
    /// which the tick does right after a purge, when the table holds live nodes only.
    pub(crate) fn snapshot(&self, with_memory_db: bool) -> TrieStoreSnapshot {
        let mut history_pairs = 0;
        let mut history_deltas = 0;
        for pubkey_state in self.pubkeys_state.values() {
            for (_pair, history) in pubkey_state.order_pairs_trie_state_history.iter() {
                history_pairs += 1;
                history_deltas += history.get_element().len();
            }
        }
        TrieStoreSnapshot {
            applied_batches: self.applied_batches,
            pubkeys: self.pubkeys_state.len(),
            history_pairs,
            history_deltas,
            memory_db_entries: with_memory_db.then(|| self.memory_db.keys().len()),
            #[cfg(not(target_arch = "wasm32"))]
            memory_db_bytes: with_memory_db.then(|| parity_util_mem::malloc_size(&self.memory_db)),
        }
    }

    /// Build a SyncPubkeyOrderbookState request if keep-alive indicates our local trie roots are stale.
    /// Topic subscription is provided via `is_subscribed` callback to avoid touching Orderbook.
    pub(crate) fn prepare_sync_request_for_keep_alive(
        &mut self,
        from_pubkey: &str,
        message: new_protocol::PubkeyKeepAlive,
        i_am_relay: bool,
        request_root: SyncRequestRoot,
        is_subscribed: impl Fn(&str) -> bool,
    ) -> Option<OrdermatchRequest> {
        let pubkey_state = pubkey_state_mut(&mut self.pubkeys_state, from_pubkey);
        // CRD ch.32 R-P3: expiry is judged on the receiver's own clock; the sender-supplied
        // `message.timestamp` is never stored.
        pubkey_state.last_keep_alive = now_ms() / 1000;

        let mut trie_roots_to_request = HashMap::new();
        for (alb_pair, trie_root) in message.trie_roots {
            let topic = orderbook_topic_from_ordered_pair(&alb_pair);
            let subscribed = is_subscribed(&topic);
            if !subscribed && !i_am_relay {
                continue;
            }

            if trie_root == H64::default() || trie_root == hashed_null_node::<Layout>() {
                log::debug!(
                    "Received zero or hashed_null_node pair {} trie root from pub {}",
                    alb_pair,
                    from_pubkey
                );
                continue;
            }

            let actual_trie_root = order_pair_root_mut(&mut pubkey_state.trie_roots, &alb_pair);
            if *actual_trie_root != trie_root {
                let named_root = match request_root {
                    // The responder answers with the changes since the root the request names
                    // (`DeltaOrFullTrie::from_history`); the zero root of a pair not synchronized yet
                    // is never a history key and gets the full set.
                    SyncRequestRoot::Held => *actual_trie_root,
                    SyncRequestRoot::Advertised => trie_root,
                };
                trie_roots_to_request.insert(alb_pair, named_root);
            }
        }

        if trie_roots_to_request.is_empty() {
            return None;
        }

        Some(OrdermatchRequest::SyncPubkeyOrderbookState {
            pubkey: from_pubkey.to_owned(),
            trie_roots: trie_roots_to_request,
        })
    }
}

/// Take, after `first` (the batch that woke the worker), every batch already waiting in `rx`, up to
/// [`DRAIN_MAX_BATCHES`] batches or [`DRAIN_MAX_EVENTS`] events (CRD ch.32 R-K5 item 1). A waiting
/// batch that would pass the event bound is left in `carry` for the next sequence; it is the first
/// batch of that one, and so a batch larger than the bound is applied alone. Returns the batches in
/// queue order and whether the queue is closed.
pub(crate) fn take_waiting_batches(
    rx: &mut UnboundedReceiver<Vec<TrieOp>>,
    first: Vec<TrieOp>,
    carry: &mut Option<Vec<TrieOp>>,
) -> (Vec<Vec<TrieOp>>, bool) {
    let mut events = first.len();
    let mut batches = vec![first];
    while batches.len() < DRAIN_MAX_BATCHES {
        match rx.try_recv() {
            Ok(batch) => {
                if events + batch.len() > DRAIN_MAX_EVENTS {
                    *carry = Some(batch);
                    return (batches, false);
                }
                events += batch.len();
                batches.push(batch);
            },
            Err(TryRecvError::Closed) => return (batches, true),
            Err(TryRecvError::Empty) => break,
        }
    }
    (batches, false)
}

/// Apply `batches` as one sequence under the caller's hold of the trie store lock, purge early if
/// due, and report the counters the sequence moved. `reported` is what has been reported already.
pub(crate) fn apply_sequence(
    store: &mut TrieStore,
    batches: Vec<Vec<TrieOp>>,
    metrics: &MetricsWeak,
    reported: &mut (u64, u64),
) {
    store.apply_batches(batches);
    store.purge_if_loaded(now_ms());
    let (early_purges, evicted) = (store.early_purges, store.history_evicted);
    if early_purges > reported.0 {
        mm_counter!(*metrics, "orderbook.trie_store.early_purges", early_purges - reported.0);
    }
    if evicted > reported.1 {
        mm_counter!(*metrics, "orderbook.history.evicted", evicted - reported.1);
    }
    *reported = (early_purges, evicted);
}

/// The trie worker: applies what the queue holds, in bulk (CRD ch.32 R-K5). Ends when the queue is
/// closed. The counters are reported through `metrics`, which does not keep the metrics alive.
pub(crate) async fn run_trie_store_worker(
    mut rx: UnboundedReceiver<Vec<TrieOp>>,
    trie_store: Arc<PaMutex<TrieStore>>,
    metrics: MetricsWeak,
) {
    let mut reported = (0, 0);
    let mut carry = None;
    loop {
        let first = match carry.take() {
            Some(batch) => batch,
            None => match rx.next().await {
                Some(batch) => batch,
                None => return,
            },
        };
        let (batches, closed) = take_waiting_batches(&mut rx, first, &mut carry);
        apply_sequence(&mut trie_store.lock(), batches, &metrics, &mut reported);
        if closed {
            return;
        }
    }
}

pub(crate) fn spawn_trie_store_worker(
    ctx: &MmArc,
    trie_store: Arc<PaMutex<TrieStore>>,
) -> UnboundedSender<Vec<TrieOp>> {
    let (tx, rx) = unbounded::<Vec<TrieOp>>();
    common::executor::spawn(run_trie_store_worker(rx, trie_store, ctx.metrics.weak()));
    tx
}

/// Upper bound of the wait for the trie worker to catch up with the read index (CRD ch.32 R-T3b
/// item 2). Short compared with the keep-alive interval.
pub(crate) const TRIE_CATCH_UP_BOUND_MS: u64 = 250;
/// Pause between two catch-up checks of the async (keep-alive) gate.
const TRIE_CATCH_UP_STEP_MS: u64 = 2;

/// The trie store did not catch up with the read index within the bound.
#[derive(Debug, PartialEq)]
pub(crate) struct TrieNotCaughtUp;

impl OrdermatchContext {
    /// One catch-up check, no waiting: with both locks held (orderbook, then trie store), run `f` if
    /// every batch queued so far has been applied. Neither lock is held when this returns.
    pub(crate) fn with_consistent_state_now<R>(&self, f: &mut impl FnMut(&Orderbook, &TrieStore) -> R) -> Option<R> {
        let orderbook = self.orderbook.lock();
        let trie_store = self.trie_store.lock();
        if orderbook.trie_ops_enqueued == trie_store.applied_batches {
            Some(f(&orderbook, &trie_store))
        } else {
            None
        }
    }

    /// Run `f` on a snapshot in which the trie store reflects every completed index operation
    /// (CRD ch.32 R-T3b item 2), waiting at most `bound_ms` with a timer (never while holding a
    /// lock). Only for a caller on its own async task (the keep-alive producer, and the receiver of a
    /// keep-alive, which runs on a task of its own per message, R-T3b item 4): code on the inbound
    /// networking path (the answer to a peer request) must not wait; it answers from the store as it
    /// stands (R-T3b items 2 and 3).
    pub(crate) async fn with_consistent_state<R>(
        &self,
        bound_ms: u64,
        mut f: impl FnMut(&Orderbook, &TrieStore) -> R,
    ) -> Result<R, TrieNotCaughtUp> {
        let mut waited = 0;
        loop {
            if let Some(r) = self.with_consistent_state_now(&mut f) {
                return Ok(r);
            }
            if waited >= bound_ms {
                return Err(TrieNotCaughtUp);
            }
            Timer::sleep(TRIE_CATCH_UP_STEP_MS as f64 / 1000.).await;
            waited += TRIE_CATCH_UP_STEP_MS;
        }
    }
}

/// How long to remember a cancelled order UUID to guard against out-of-order P2P messages.
const RECENTLY_CANCELLED_TIMEOUT: Duration = Duration::from_secs(120);

pub(crate) struct Orderbook {
    /// A map from (base, rel).
    pub(crate) ordered: HashMap<(String, String), BTreeSet<OrderedByPriceOrder>>,
    /// A map from base ticker to the set of another tickers to track the existing pairs
    pub(crate) pairs_existing_for_base: HashMap<String, HashSet<String>>,
    /// A map from rel ticker to the set of another tickers to track the existing pairs
    pub(crate) pairs_existing_for_rel: HashMap<String, HashSet<String>>,
    /// A map from (base, rel).
    pub(crate) unordered: HashMap<(String, String), HashSet<Uuid>>,
    pub(crate) order_set: HashMap<Uuid, OrderbookItem>,
    /// Recently cancelled order UUIDs mapped to the cancelling pubkey.
    /// Guards against re-creation when P2P cancel arrives before the create message.
    pub(crate) recently_cancelled: TimeCache<Uuid, String>,
    /// Per-order P2P pubkeys owned by this node (e.g. ZHTLC random keypairs).
    /// Used by `is_my_order()` alongside the persistent secp256k1 pubkey.
    pub(crate) my_p2p_pubkeys: HashSet<String>,
    /// Number of trie-event batches queued for the trie worker (CRD ch.32 R-T3b). Incremented only
    /// by [`Orderbook::queue_trie_ops`], under the orderbook lock. Compared with
    /// [`TrieStore::applied_batches`] to tell whether the trie has caught up with the index.
    pub(crate) trie_ops_enqueued: u64,
}

impl Default for Orderbook {
    fn default() -> Self {
        Orderbook {
            ordered: HashMap::default(),
            pairs_existing_for_base: HashMap::default(),
            pairs_existing_for_rel: HashMap::default(),
            unordered: HashMap::default(),
            order_set: HashMap::default(),
            recently_cancelled: TimeCache::new(RECENTLY_CANCELLED_TIMEOUT),
            my_p2p_pubkeys: HashSet::default(),
            trie_ops_enqueued: 0,
        }
    }
}

pub(crate) fn hashed_null_node<T: TrieConfiguration>() -> TrieHash<T> { <T::Codec as NodeCodecT>::hashed_null_node() }

impl Orderbook {
    /// Queue the trie events of an index mutation for the trie worker (CRD ch.32 R-T3b item 1).
    ///
    /// Takes `&mut self` so that it can only be called while the caller holds the orderbook guard
    /// of the mutation: two operations on one order reach the trie in the order they reached the
    /// index. The send never blocks. An empty batch is not queued (and not counted).
    pub(crate) fn queue_trie_ops(&mut self, tx: &UnboundedSender<Vec<TrieOp>>, ops: Vec<TrieOp>) {
        if ops.is_empty() {
            return;
        }
        match tx.unbounded_send(ops) {
            Ok(()) => self.trie_ops_enqueued += 1,
            Err(e) => log::error!("Failed to send trie ops: {}", e),
        }
    }

    pub(crate) fn find_order_by_uuid_and_pubkey(&self, uuid: &Uuid, from_pubkey: &str) -> Option<OrderbookItem> {
        self.order_set.get(uuid).and_then(|order| {
            if order.pubkey == from_pubkey {
                Some(order.clone())
            } else {
                None
            }
        })
    }

    pub(crate) fn find_order_by_uuid(&self, uuid: &Uuid) -> Option<OrderbookItem> { self.order_set.get(uuid).cloned() }

    /// Index-only method: updates in-memory indices and returns the trie mutations
    /// that must be applied by TrieStore. No trie/memory_db mutation happens here.
    pub(crate) fn index_insert_or_update(&mut self, order: OrderbookItem) -> Vec<TrieOp> {
        if self.recently_cancelled.get_unexpired(&order.uuid) == Some(&order.pubkey) {
            log::debug!(
                "Ignoring a late copy of order {}: it was cancelled less than {} s ago",
                order.uuid,
                RECENTLY_CANCELLED_TIMEOUT.as_secs()
            );
            return Vec::new();
        }

        let mut trie_ops = vec![];
        let zero = BigRational::from_integer(0.into());

        if order.max_volume <= zero || order.price <= zero || order.min_volume < zero {
            if let Some((_removed, op)) = self.index_remove(order.uuid) {
                trie_ops.push(op);
            }
            return trie_ops;
        }

        let alb_pair = alb_ordered_pair(&order.base, &order.rel);
        let op = TrieOp::Insert {
            pubkey: order.pubkey.clone(),
            alb_pair,
            uuid: order.uuid,
            order: order.clone(),
        };
        self.index_insert_or_update_inner(order);
        trie_ops.push(op);
        trie_ops
    }

    /// Pure index update (no trie changes): replaces/creates an order in memory structures.
    pub(crate) fn index_insert_or_update_inner(&mut self, order: OrderbookItem) {
        log::debug!("Inserting order {:?}", order);

        let base_rel = (order.base.clone(), order.rel.clone());

        let ordered = self.ordered.entry(base_rel.clone()).or_default();

        // have to clone to drop immutable ordered borrow
        let existing = ordered
            .iter()
            .find(|maybe_existing| maybe_existing.uuid == order.uuid)
            .cloned();

        if let Some(exists) = existing {
            ordered.remove(&exists);
        }
        ordered.insert(OrderedByPriceOrder {
            uuid: order.uuid,
            price: order.price.clone().into(),
        });

        self.pairs_existing_for_base
            .entry(order.base.clone())
            .or_default()
            .insert(order.rel.clone());

        self.pairs_existing_for_rel
            .entry(order.rel.clone())
            .or_default()
            .insert(order.base.clone());

        self.unordered.entry(base_rel).or_default().insert(order.uuid);

        self.order_set.insert(order.uuid, order);
    }

    /// Pure index removal (no trie changes): removes from in-memory indices
    /// and returns the removed order and TrieOp.
    pub(crate) fn index_remove(&mut self, uuid: Uuid) -> Option<(OrderbookItem, TrieOp)> {
        let order = self.order_set.remove(&uuid)?;
        let base_rel = (order.base.clone(), order.rel.clone());

        let order_to_delete = OrderedByPriceOrder {
            price: order.price.clone().into(),
            uuid,
        };

        if let Some(orders) = self.ordered.get_mut(&base_rel) {
            orders.remove(&order_to_delete);
            if orders.is_empty() {
                self.ordered.remove(&base_rel);
            }
        }

        if let Some(orders) = self.unordered.get_mut(&base_rel) {
            orders.remove(&order_to_delete.uuid);
            if orders.is_empty() {
                self.unordered.remove(&base_rel);
            }
        }

        let alb_pair = alb_ordered_pair(&order.base, &order.rel);
        let op = TrieOp::Remove {
            pubkey: order.pubkey.clone(),
            alb_pair,
            uuid,
        };

        Some((order, op))
    }

    /// Index-only removal of all orders for a (pubkey, pair). Trie cleanup is handled
    /// by a single ClearPair op at the trie layer.
    pub(crate) fn index_remove_pubkey_pair_orders(&mut self, pubkey: &str, alb_pair: &str) {
        let (base, rel) = match alb_pair.split_once(':') {
            Some((a, b)) => (a, b),
            None => return,
        };

        let pairs = [(base.to_owned(), rel.to_owned()), (rel.to_owned(), base.to_owned())];

        for pair in pairs {
            if let Some(uuids) = self.unordered.get(&pair).cloned() {
                for uuid in uuids {
                    if let Some(order) = self.order_set.get(&uuid) {
                        if order.pubkey == pubkey {
                            // ignore the trie op here — ClearPair handles it at the trie layer
                            let _ = self.index_remove(uuid);
                        }
                    }
                }
            }
        }
    }

    pub(crate) fn orderbook_item_with_proof(&self, order: OrderbookItem) -> OrderbookItemWithProof {
        OrderbookItemWithProof {
            order,
            last_message_payload: vec![],
            proof: vec![],
        }
    }
}
/// Orderbook Item P2P message
/// DO NOT CHANGE - it will break backwards compatibility
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct OrderbookP2PItem {
    pub(crate) pubkey: String,
    pub(crate) base: String,
    pub(crate) rel: String,
    pub(crate) price: BigRational,
    pub(crate) max_volume: BigRational,
    pub(crate) min_volume: BigRational,
    pub(crate) uuid: Uuid,
    pub(crate) created_at: u64,
}

impl OrderbookP2PItem {
    pub(crate) fn as_rpc_best_orders_buy(
        &self,
        address: String,
        conf_settings: Option<&OrderConfirmationsSettings>,
        is_mine: bool,
    ) -> RpcOrderbookEntry {
        let price_mm = MmNumber::from(self.price.clone());
        let max_vol_mm = MmNumber::from(self.max_volume.clone());
        let min_vol_mm = MmNumber::from(self.min_volume.clone());

        let base_max_volume = max_vol_mm.clone().into();
        let base_min_volume = min_vol_mm.clone().into();
        let rel_max_volume = (&max_vol_mm * &price_mm).into();
        let rel_min_volume = (&min_vol_mm * &price_mm).into();

        RpcOrderbookEntry {
            coin: self.rel.clone(),
            address,
            price: price_mm.to_decimal(),
            price_rat: price_mm.to_ratio(),
            price_fraction: price_mm.to_fraction(),
            max_volume: max_vol_mm.to_decimal(),
            max_volume_rat: max_vol_mm.to_ratio(),
            max_volume_fraction: max_vol_mm.to_fraction(),
            min_volume: min_vol_mm.to_decimal(),
            min_volume_rat: min_vol_mm.to_ratio(),
            min_volume_fraction: min_vol_mm.to_fraction(),
            pubkey: self.pubkey.clone(),
            age: (now_ms() as i64 / 1000),
            zcredits: 0,
            uuid: self.uuid,
            is_mine,
            base_max_volume,
            base_min_volume,
            rel_max_volume,
            rel_min_volume,
            conf_settings: conf_settings.cloned(),
        }
    }

    pub(crate) fn as_rpc_best_orders_buy_v2(
        &self,
        address: OrderbookAddress,
        conf_settings: Option<&OrderConfirmationsSettings>,
        is_mine: bool,
    ) -> RpcOrderbookEntryV2 {
        let price_mm = MmNumber::from(self.price.clone());
        let max_vol_mm = MmNumber::from(self.max_volume.clone());
        let min_vol_mm = MmNumber::from(self.min_volume.clone());

        RpcOrderbookEntryV2 {
            coin: self.rel.clone(),
            address,
            rel_max_volume: (&max_vol_mm * &price_mm).into(),
            rel_min_volume: (&min_vol_mm * &price_mm).into(),
            price: price_mm.into(),
            pubkey: self.pubkey.clone(),
            uuid: self.uuid,
            is_mine,
            base_max_volume: max_vol_mm.into(),
            base_min_volume: min_vol_mm.into(),
            conf_settings: conf_settings.cloned(),
        }
    }

    pub(crate) fn as_rpc_best_orders_sell(
        &self,
        address: String,
        conf_settings: Option<&OrderConfirmationsSettings>,
        is_mine: bool,
    ) -> RpcOrderbookEntry {
        let price_mm = MmNumber::from(1i32) / self.price.clone().into();
        let max_vol_mm = MmNumber::from(self.max_volume.clone());
        let min_vol_mm = MmNumber::from(self.min_volume.clone());

        let base_max_volume = (&max_vol_mm / &price_mm).into();
        let base_min_volume = (&min_vol_mm / &price_mm).into();
        let rel_max_volume = max_vol_mm.clone().into();
        let rel_min_volume = min_vol_mm.clone().into();
        let conf_settings = conf_settings.map(|conf| conf.reversed());

        RpcOrderbookEntry {
            coin: self.base.clone(),
            address,
            price: price_mm.to_decimal(),
            price_rat: price_mm.to_ratio(),
            price_fraction: price_mm.to_fraction(),
            max_volume: max_vol_mm.to_decimal(),
            max_volume_rat: max_vol_mm.to_ratio(),
            max_volume_fraction: max_vol_mm.to_fraction(),
            min_volume: min_vol_mm.to_decimal(),
            min_volume_rat: min_vol_mm.to_ratio(),
            min_volume_fraction: min_vol_mm.to_fraction(),
            pubkey: self.pubkey.clone(),
            age: (now_ms() as i64 / 1000),
            zcredits: 0,
            uuid: self.uuid,
            is_mine,
            base_max_volume,
            base_min_volume,
            rel_max_volume,
            rel_min_volume,
            conf_settings,
        }
    }

    pub(crate) fn as_rpc_best_orders_sell_v2(
        &self,
        address: OrderbookAddress,
        conf_settings: Option<&OrderConfirmationsSettings>,
        is_mine: bool,
    ) -> RpcOrderbookEntryV2 {
        let price_mm = MmNumber::from(1i32) / self.price.clone().into();
        let max_vol_mm = MmNumber::from(self.max_volume.clone());
        let min_vol_mm = MmNumber::from(self.min_volume.clone());

        let conf_settings = conf_settings.map(|conf| conf.reversed());

        RpcOrderbookEntryV2 {
            coin: self.base.clone(),
            address,
            base_max_volume: (&max_vol_mm / &price_mm).into(),
            base_min_volume: (&min_vol_mm / &price_mm).into(),
            price: price_mm.into(),
            pubkey: self.pubkey.clone(),
            uuid: self.uuid,
            is_mine,
            rel_max_volume: max_vol_mm.into(),
            rel_min_volume: min_vol_mm.into(),
            conf_settings,
        }
    }
}

/// Despite it looks the same as OrderbookItemWithProof it's better to have a separate struct to avoid compatibility
/// breakage if we need to add more fields to the OrderbookItemWithProof
/// DO NOT ADD more fields in this struct as it will break backward compatibility.
/// Add them to the BestOrdersRes instead
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct OrderbookP2PItemWithProof {
    /// Orderbook item
    pub(crate) order: OrderbookP2PItem,
    /// Last pubkey message payload that contains most recent pair trie root
    pub(crate) last_message_payload: Vec<u8>,
    /// Proof confirming that orderbook item is in the pair trie
    pub(crate) proof: TrieProof,
}

impl From<OrderbookItemWithProof> for OrderbookP2PItemWithProof {
    fn from(o: OrderbookItemWithProof) -> Self {
        OrderbookP2PItemWithProof {
            order: o.order.into(),
            last_message_payload: o.last_message_payload,
            proof: o.proof,
        }
    }
}

impl From<OrderbookItem> for OrderbookP2PItem {
    fn from(o: OrderbookItem) -> OrderbookP2PItem {
        OrderbookP2PItem {
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

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OrderbookItem {
    pub(crate) pubkey: String,
    pub(crate) base: String,
    pub(crate) rel: String,
    pub(crate) price: BigRational,
    pub(crate) max_volume: BigRational,
    pub(crate) min_volume: BigRational,
    pub(crate) uuid: Uuid,
    pub(crate) created_at: u64,
    pub(crate) base_protocol_info: Vec<u8>,
    pub(crate) rel_protocol_info: Vec<u8>,
    pub(crate) conf_settings: Option<OrderConfirmationsSettings>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OrderbookItemWithProof {
    /// Orderbook item
    pub(crate) order: OrderbookItem,
    /// Last pubkey message payload that contains most recent pair trie root
    pub(crate) last_message_payload: Vec<u8>,
    /// Proof confirming that orderbook item is in the pair trie
    pub(crate) proof: TrieProof,
}

/// Concrete implementation of Hasher using Blake2b 64-bit hashes
#[derive(Debug)]
pub struct Blake2Hasher64;

impl Hasher for Blake2Hasher64 {
    type Out = [u8; 8];
    type StdHasher = Hash256StdHasher;
    const LENGTH: usize = 8;

    fn hash(x: &[u8]) -> Self::Out {
        let mut hasher = Blake2bVar::new(8).expect("8 is valid VarBlake2b output_size");
        hasher.update(x);
        let mut res: [u8; 8] = Default::default();
        hasher.finalize_variable(&mut res).expect("hashing to succeed");
        res
    }
}

pub(crate) type Layout = sp_trie::LayoutV0<Blake2Hasher64>;

impl OrderbookItem {
    pub(crate) fn apply_updated(&mut self, msg: &new_protocol::MakerOrderUpdated) {
        if let Some(new_price) = msg.new_price() {
            self.price = new_price.into();
        }

        if let Some(new_max_volume) = msg.new_max_volume() {
            self.max_volume = new_max_volume.into();
        }

        if let Some(new_min_volume) = msg.new_min_volume() {
            self.min_volume = new_min_volume.into();
        }
    }

    pub(crate) fn as_rpc_entry_ask(&self, address: String, is_mine: bool) -> RpcOrderbookEntry {
        let price_mm = MmNumber::from(self.price.clone());
        let max_vol_mm = MmNumber::from(self.max_volume.clone());
        let min_vol_mm = MmNumber::from(self.min_volume.clone());

        let base_max_volume = max_vol_mm.clone().into();
        let base_min_volume = min_vol_mm.clone().into();
        let rel_max_volume = (&max_vol_mm * &price_mm).into();
        let rel_min_volume = (&min_vol_mm * &price_mm).into();

        RpcOrderbookEntry {
            coin: self.base.clone(),
            address,
            price: price_mm.to_decimal(),
            price_rat: price_mm.to_ratio(),
            price_fraction: price_mm.to_fraction(),
            max_volume: max_vol_mm.to_decimal(),
            max_volume_rat: max_vol_mm.to_ratio(),
            max_volume_fraction: max_vol_mm.to_fraction(),
            min_volume: min_vol_mm.to_decimal(),
            min_volume_rat: min_vol_mm.to_ratio(),
            min_volume_fraction: min_vol_mm.to_fraction(),
            pubkey: self.pubkey.clone(),
            age: (now_ms() as i64 / 1000),
            zcredits: 0,
            uuid: self.uuid,
            is_mine,
            base_max_volume,
            base_min_volume,
            rel_max_volume,
            rel_min_volume,
            conf_settings: self.conf_settings,
        }
    }

    pub(crate) fn as_rpc_entry_bid(&self, address: String, is_mine: bool) -> RpcOrderbookEntry {
        let price_mm = MmNumber::from(1i32) / self.price.clone().into();
        let max_vol_mm = MmNumber::from(self.max_volume.clone());
        let min_vol_mm = MmNumber::from(self.min_volume.clone());

        let base_max_volume = (&max_vol_mm / &price_mm).into();
        let base_min_volume = (&min_vol_mm / &price_mm).into();
        let rel_max_volume = max_vol_mm.clone().into();
        let rel_min_volume = min_vol_mm.clone().into();
        let conf_settings = self.conf_settings.map(|conf| conf.reversed());

        RpcOrderbookEntry {
            coin: self.base.clone(),
            address,
            price: price_mm.to_decimal(),
            price_rat: price_mm.to_ratio(),
            price_fraction: price_mm.to_fraction(),
            max_volume: max_vol_mm.to_decimal(),
            max_volume_rat: max_vol_mm.to_ratio(),
            max_volume_fraction: max_vol_mm.to_fraction(),
            min_volume: min_vol_mm.to_decimal(),
            min_volume_rat: min_vol_mm.to_ratio(),
            min_volume_fraction: min_vol_mm.to_fraction(),
            pubkey: self.pubkey.clone(),
            age: (now_ms() as i64 / 1000),
            zcredits: 0,
            uuid: self.uuid,
            is_mine,
            base_max_volume,
            base_min_volume,
            rel_max_volume,
            rel_min_volume,
            conf_settings,
        }
    }

    pub(crate) fn as_rpc_v2_entry_ask(&self, address: OrderbookAddress, is_mine: bool) -> RpcOrderbookEntryV2 {
        let price_mm = MmNumber::from(self.price.clone());
        let max_vol_mm = MmNumber::from(self.max_volume.clone());
        let min_vol_mm = MmNumber::from(self.min_volume.clone());

        RpcOrderbookEntryV2 {
            coin: self.base.clone(),
            address,
            rel_max_volume: (&max_vol_mm * &price_mm).into(),
            rel_min_volume: (&min_vol_mm * &price_mm).into(),
            price: price_mm.into(),
            pubkey: self.pubkey.clone(),
            uuid: self.uuid,
            is_mine,
            base_max_volume: max_vol_mm.into(),
            base_min_volume: min_vol_mm.into(),
            conf_settings: self.conf_settings,
        }
    }

    pub(crate) fn as_rpc_v2_entry_bid(&self, address: OrderbookAddress, is_mine: bool) -> RpcOrderbookEntryV2 {
        let price_mm = MmNumber::from(1i32) / self.price.clone().into();
        let max_vol_mm = MmNumber::from(self.max_volume.clone());
        let min_vol_mm = MmNumber::from(self.min_volume.clone());

        let conf_settings = self.conf_settings.map(|conf| conf.reversed());

        RpcOrderbookEntryV2 {
            coin: self.base.clone(),
            address,
            base_max_volume: (&max_vol_mm / &price_mm).into(),
            base_min_volume: (&min_vol_mm / &price_mm).into(),
            price: price_mm.into(),
            pubkey: self.pubkey.clone(),
            uuid: self.uuid,
            is_mine,
            rel_max_volume: max_vol_mm.into(),
            rel_min_volume: min_vol_mm.into(),
            conf_settings,
        }
    }

    pub(crate) fn from_p2p_and_info(
        o: OrderbookP2PItem,
        proto_info: BaseRelProtocolInfo,
        conf_info: Option<OrderConfirmationsSettings>,
    ) -> Self {
        OrderbookItem {
            pubkey: o.pubkey,
            base: o.base,
            rel: o.rel,
            price: o.price,
            max_volume: o.max_volume,
            min_volume: o.min_volume,
            uuid: o.uuid,
            created_at: o.created_at,
            base_protocol_info: proto_info.base,
            rel_protocol_info: proto_info.rel,
            conf_settings: conf_info,
        }
    }

    pub(crate) fn base_rel_proto_info(&self) -> BaseRelProtocolInfo {
        BaseRelProtocolInfo {
            base: self.base_protocol_info.clone(),
            rel: self.rel_protocol_info.clone(),
        }
    }

    /// Serialize order partially to store in the trie
    /// AVOID CHANGING THIS as much as possible because it will cause a kind of "hard fork"
    pub(crate) fn trie_state_bytes(&self) -> Vec<u8> {
        #[derive(Serialize)]
        struct OrderbookItemHelper<'a> {
            pubkey: &'a str,
            pub(crate) base: &'a str,
            pub(crate) rel: &'a str,
            pub(crate) price: &'a BigRational,
            pub(crate) max_volume: &'a BigRational,
            pub(crate) min_volume: &'a BigRational,
            pub(crate) uuid: &'a Uuid,
            pub(crate) created_at: &'a u64,
        }

        let helper = OrderbookItemHelper {
            pubkey: &self.pubkey,
            base: &self.base,
            rel: &self.rel,
            price: &self.price,
            max_volume: &self.max_volume,
            min_volume: &self.min_volume,
            uuid: &self.uuid,
            created_at: &self.created_at,
        };

        rmp_serde::to_vec(&helper).expect("Serialization should never fail")
    }
}

/// Subscribe to an orderbook topic (see [`orderbook_topic`]).
/// If the `request_orderbook` is true and the orderbook for the given pair of coins is not requested yet (or is not filled up yet),
/// request and fill the orderbook.
///
/// # Safety
///
/// The function locks [`MmCtx::p2p_ctx`] and [`MmCtx::ordermatch_ctx`]
pub(crate) async fn subscribe_to_orderbook_topic(
    ctx: &MmArc,
    base: &str,
    rel: &str,
    request_orderbook: bool,
) -> Result<(), String> {
    let current_timestamp = now_ms() / 1000;
    let topic = orderbook_topic_from_base_rel(base, rel);
    let is_orderbook_filled = {
        let ordermatch_ctx = try_s!(OrdermatchContext::from_ctx(ctx));
        let mut subscriptions = ordermatch_ctx.orderbook_subscriptions.write();

        match subscriptions.entry(topic.clone()) {
            Entry::Vacant(e) => {
                // we weren't subscribed to the topic yet
                e.insert(OrderbookRequestingState::NotRequested {
                    subscribed_at: current_timestamp,
                });
                subscribe_to_topic(ctx, topic.clone());
                // orderbook is not filled
                false
            },
            Entry::Occupied(e) => match e.get() {
                OrderbookRequestingState::Requested => {
                    // We are subscribed to the topic and the orderbook was requested already
                    true
                },
                OrderbookRequestingState::NotRequested { subscribed_at } => {
                    // We are subscribed to the topic. Also we didn't request the orderbook,
                    // True if enough time has passed for the orderbook to fill by OrdermatchRequest::SyncPubkeyOrderbookState.
                    *subscribed_at + ORDERBOOK_REQUESTING_TIMEOUT < current_timestamp
                },
            },
        }
    };

    if !is_orderbook_filled && request_orderbook {
        try_s!(request_and_fill_orderbook(ctx, base, rel).await);
    }

    Ok(())
}

/// Records the subscription to `topic` as "subscribed, not yet requested" when no record exists and
/// subscribes to the topic on the mesh in that case only; an existing record (including `Requested`)
/// is kept unchanged and no second mesh subscription is made. No orderbook request is made
/// (CRD ch.59 R59.6.7 item 2). The subscription lock is held only for the synchronous
/// record-and-subscribe step, never across an await.
pub(crate) fn record_topic_subscription_without_request(ctx: &MmArc, topic: String) -> Result<(), String> {
    let current_timestamp = now_ms() / 1000;
    let ordermatch_ctx = try_s!(OrdermatchContext::from_ctx(ctx));
    let mut subscriptions = ordermatch_ctx.orderbook_subscriptions.write();
    if let Entry::Vacant(e) = subscriptions.entry(topic.clone()) {
        e.insert(OrderbookRequestingState::NotRequested {
            subscribed_at: current_timestamp,
        });
        subscribe_to_topic(ctx, topic);
    }
    Ok(())
}

construct_detailed!(DetailedBaseMaxVolume, base_max_volume);
construct_detailed!(DetailedBaseMinVolume, base_min_volume);
construct_detailed!(DetailedRelMaxVolume, rel_max_volume);
construct_detailed!(DetailedRelMinVolume, rel_min_volume);

#[derive(Debug, Serialize)]
pub struct RpcOrderbookEntry {
    pub(crate) coin: String,
    pub(crate) address: String,
    pub(crate) price: BigDecimal,
    pub(crate) price_rat: BigRational,
    pub(crate) price_fraction: Fraction,
    #[serde(rename = "maxvolume")]
    pub(crate) max_volume: BigDecimal,
    pub(crate) max_volume_rat: BigRational,
    pub(crate) max_volume_fraction: Fraction,
    pub(crate) min_volume: BigDecimal,
    pub(crate) min_volume_rat: BigRational,
    pub(crate) min_volume_fraction: Fraction,
    pubkey: String,
    pub(crate) age: i64,
    pub(crate) zcredits: u64,
    pub(crate) uuid: Uuid,
    pub(crate) is_mine: bool,
    #[serde(flatten)]
    pub(crate) base_max_volume: DetailedBaseMaxVolume,
    #[serde(flatten)]
    pub(crate) base_min_volume: DetailedBaseMinVolume,
    #[serde(flatten)]
    pub(crate) rel_max_volume: DetailedRelMaxVolume,
    #[serde(flatten)]
    pub(crate) rel_min_volume: DetailedRelMinVolume,
    #[serde(flatten)]
    pub(crate) conf_settings: Option<OrderConfirmationsSettings>,
}

#[derive(Debug, Serialize)]
pub struct RpcOrderbookEntryV2 {
    pub(crate) coin: String,
    pub(crate) address: OrderbookAddress,
    pub(crate) price: MmNumberMultiRepr,
    pubkey: String,
    pub(crate) uuid: Uuid,
    pub(crate) is_mine: bool,
    pub(crate) base_max_volume: MmNumberMultiRepr,
    pub(crate) base_min_volume: MmNumberMultiRepr,
    pub(crate) rel_max_volume: MmNumberMultiRepr,
    pub(crate) rel_min_volume: MmNumberMultiRepr,
    pub(crate) conf_settings: Option<OrderConfirmationsSettings>,
}

pub(crate) fn choose_maker_confs_and_notas(
    maker_confs: Option<OrderConfirmationsSettings>,
    taker_req: &TakerRequest,
    maker_coin: &MmCoinEnum,
    taker_coin: &MmCoinEnum,
) -> SwapConfirmationsSettings {
    let maker_settings = maker_confs.unwrap_or(OrderConfirmationsSettings {
        base_confs: maker_coin.required_confirmations(),
        base_nota: maker_coin.requires_notarization(),
        rel_confs: taker_coin.required_confirmations(),
        rel_nota: taker_coin.requires_notarization(),
    });

    let (maker_coin_confs, maker_coin_nota, taker_coin_confs, taker_coin_nota) = match taker_req.conf_settings {
        Some(taker_settings) => match taker_req.action {
            TakerAction::Sell => {
                let maker_coin_confs = if taker_settings.rel_confs < maker_settings.base_confs {
                    taker_settings.rel_confs
                } else {
                    maker_settings.base_confs
                };
                let maker_coin_nota = if !taker_settings.rel_nota {
                    taker_settings.rel_nota
                } else {
                    maker_settings.base_nota
                };
                (
                    maker_coin_confs,
                    maker_coin_nota,
                    maker_settings.rel_confs,
                    maker_settings.rel_nota,
                )
            },
            TakerAction::Buy => {
                let maker_coin_confs = if taker_settings.base_confs < maker_settings.base_confs {
                    taker_settings.base_confs
                } else {
                    maker_settings.base_confs
                };
                let maker_coin_nota = if !taker_settings.base_nota {
                    taker_settings.base_nota
                } else {
                    maker_settings.base_nota
                };
                (
                    maker_coin_confs,
                    maker_coin_nota,
                    maker_settings.rel_confs,
                    maker_settings.rel_nota,
                )
            },
        },
        None => (
            maker_settings.base_confs,
            maker_settings.base_nota,
            maker_settings.rel_confs,
            maker_settings.rel_nota,
        ),
    };

    SwapConfirmationsSettings {
        maker_coin_confs,
        maker_coin_nota,
        taker_coin_confs,
        taker_coin_nota,
    }
}

pub(crate) fn choose_taker_confs_and_notas(
    taker_req: &TakerRequest,
    maker_reserved: &MakerReserved,
    maker_coin: &MmCoinEnum,
    taker_coin: &MmCoinEnum,
) -> SwapConfirmationsSettings {
    let (mut taker_coin_confs, mut taker_coin_nota, maker_coin_confs, maker_coin_nota) = match taker_req.action {
        TakerAction::Buy => match taker_req.conf_settings {
            Some(s) => (s.rel_confs, s.rel_nota, s.base_confs, s.base_nota),
            None => (
                taker_coin.required_confirmations(),
                taker_coin.requires_notarization(),
                maker_coin.required_confirmations(),
                maker_coin.requires_notarization(),
            ),
        },
        TakerAction::Sell => match taker_req.conf_settings {
            Some(s) => (s.base_confs, s.base_nota, s.rel_confs, s.rel_nota),
            None => (
                taker_coin.required_confirmations(),
                taker_coin.requires_notarization(),
                maker_coin.required_confirmations(),
                maker_coin.requires_notarization(),
            ),
        },
    };
    if let Some(settings_from_maker) = maker_reserved.conf_settings {
        if settings_from_maker.rel_confs < taker_coin_confs {
            taker_coin_confs = settings_from_maker.rel_confs;
        }
        if !settings_from_maker.rel_nota {
            taker_coin_nota = settings_from_maker.rel_nota;
        }
    }
    SwapConfirmationsSettings {
        maker_coin_confs,
        maker_coin_nota,
        taker_coin_confs,
        taker_coin_nota,
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "address_type", content = "address_data")]
pub enum OrderbookAddress {
    Transparent(String),
    Shielded,
}

impl OrderbookAddress {
    /// Renders the outcome for the legacy (envelope-less) RPC generation, where `address` is a plain
    /// string: the transparent address itself, or the fixed placeholder `Shielded`
    /// (CRD ch.32 R-F6d, R-F6e). Display only.
    pub(crate) fn into_legacy_string(self) -> String {
        match self {
            OrderbookAddress::Transparent(addr) => addr,
            OrderbookAddress::Shielded => "Shielded".to_owned(),
        }
    }
}

#[derive(Debug, Display)]
pub(crate) enum OrderbookAddrErr {
    AddrFromPubkeyError(String),
    CoinIsNotSupported(String),
    DeserializationError(json::Error),
    InvalidPlatformCoinProtocol(String),
    PlatformCoinConfIsNull(String),
}

impl From<json::Error> for OrderbookAddrErr {
    fn from(err: json::Error) -> Self { OrderbookAddrErr::DeserializationError(err) }
}

pub(crate) fn orderbook_address(
    ctx: &MmArc,
    coin: &str,
    conf: &Json,
    pubkey: &str,
    addr_format: UtxoAddressFormat,
) -> Result<OrderbookAddress, MmError<OrderbookAddrErr>> {
    let protocol: CoinProtocol = CoinProtocol::from_conf_json(conf["protocol"].clone())?;
    match protocol {
        CoinProtocol::ERC20 { .. } | CoinProtocol::ETH { .. } => coins::eth::addr_from_pubkey_str(pubkey)
            .map(OrderbookAddress::Transparent)
            .map_to_mm(OrderbookAddrErr::AddrFromPubkeyError),
        CoinProtocol::UTXO | CoinProtocol::QTUM | CoinProtocol::QRC20 { .. } | CoinProtocol::BCH { .. } => {
            coins::utxo::address_by_conf_and_pubkey_str(coin, conf, pubkey, addr_format)
                .map(OrderbookAddress::Transparent)
                .map_to_mm(OrderbookAddrErr::AddrFromPubkeyError)
        },
        CoinProtocol::SLPTOKEN { platform, .. } => {
            let platform_conf = coin_conf(ctx, &platform);
            if platform_conf.is_null() {
                return MmError::err(OrderbookAddrErr::PlatformCoinConfIsNull(platform));
            }
            // TODO is there any way to make it better without duplicating the prefix in the SLP conf?
            let platform_protocol: CoinProtocol = CoinProtocol::from_conf_json(platform_conf["protocol"].clone())?;
            match platform_protocol {
                CoinProtocol::BCH { slp_prefix } => coins::utxo::slp::slp_addr_from_pubkey_str(pubkey, &slp_prefix)
                    .map(OrderbookAddress::Transparent)
                    .mm_err(|e| OrderbookAddrErr::AddrFromPubkeyError(e.to_string())),
                _ => MmError::err(OrderbookAddrErr::InvalidPlatformCoinProtocol(platform)),
            }
        },
        #[cfg(not(target_arch = "wasm32"))]
        CoinProtocol::SOLANA | CoinProtocol::SPLTOKEN { .. } => {
            MmError::err(OrderbookAddrErr::CoinIsNotSupported(coin.to_owned()))
        },
        // Sia's receiving address is derived from a per-coin ed25519 key, not
        // from the order's secp256k1 P2P identity pubkey this function is
        // given -- no derivation from that pubkey can ever produce it, the
        // same reason a shielded coin has no transparent address to show
        // (CRD ch.32 R-F6b). Returning the unsupported-coin error here (as
        // opposed to the shielded success below) fed the caller's
        // drop-this-order-on-any-error path and silently removed every live
        // Sia order from both `orderbook` and `best_orders`, in either query
        // direction.
        CoinProtocol::SIA => Ok(OrderbookAddress::Shielded),
        // Lightning deliberately keeps its payment-receiving identity
        // private; showing one here would leak exactly what the protocol
        // withholds (CRD ch.32 R-F6b). Same fix, same reasoning as Sia above.
        #[cfg(not(target_arch = "wasm32"))]
        CoinProtocol::LIGHTNING { .. } => Ok(OrderbookAddress::Shielded),
        CoinProtocol::ZHTLC(_) => Ok(OrderbookAddress::Shielded),
        CoinProtocol::TENDERMINT { account_prefix, .. } => {
            coins::tendermint::account_id_from_pubkey_hex(&account_prefix, pubkey)
                .map(|addr| OrderbookAddress::Transparent(addr.to_string()))
                .map_to_mm(|e| OrderbookAddrErr::AddrFromPubkeyError(e.to_string()))
        },
        CoinProtocol::TENDERMINTTOKEN { platform, .. } => {
            let platform_conf = coin_conf(ctx, &platform);
            if platform_conf.is_null() {
                return MmError::err(OrderbookAddrErr::PlatformCoinConfIsNull(platform));
            }
            let platform_protocol: CoinProtocol = CoinProtocol::from_conf_json(platform_conf["protocol"].clone())?;
            match platform_protocol {
                CoinProtocol::TENDERMINT { account_prefix, .. } => {
                    coins::tendermint::account_id_from_pubkey_hex(&account_prefix, pubkey)
                        .map(|addr| OrderbookAddress::Transparent(addr.to_string()))
                        .map_to_mm(|e| OrderbookAddrErr::AddrFromPubkeyError(e.to_string()))
                },
                _ => MmError::err(OrderbookAddrErr::InvalidPlatformCoinProtocol(platform)),
            }
        },
        CoinProtocol::TRX { .. } | CoinProtocol::TRC20 { .. } => coins::eth::tron::addr_from_pubkey_str(pubkey)
            .map(OrderbookAddress::Transparent)
            .map_to_mm(OrderbookAddrErr::AddrFromPubkeyError),
        CoinProtocol::NFT { .. } => MmError::err(OrderbookAddrErr::CoinIsNotSupported(coin.to_owned())),
    }
}

#[cfg(test)]
mod orderbook_address_tests {
    use super::*;
    use mm2_test_helpers::for_tests::mm_ctx_with_custom_db;

    // Regression for a live Sia order silently disappearing from `orderbook`
    // and `best_orders`: address resolution used to return the
    // unsupported-coin error for Sia (and Lightning), which the caller in
    // orderbook_rpc.rs treats as fatal for that one order and drops it
    // entirely (CRD ch.32 R-F6c). It must instead succeed with the shielded
    // marker, the same outcome ZHTLC and privacy-shielded coins already get
    // (R-F6b), so the order stays in the response.
    #[test]
    fn sia_address_resolution_succeeds_shielded_instead_of_dropping_the_order() {
        let ctx = mm_ctx_with_custom_db();
        let conf = json!({"protocol": {"type": "SIA"}});
        let result = orderbook_address(&ctx, "SC", &conf, "irrelevant-for-sia", UtxoAddressFormat::Standard);
        assert!(
            matches!(result, Ok(OrderbookAddress::Shielded)),
            "expected Ok(Shielded), got {result:?}"
        );
    }

    #[test]
    fn lightning_address_resolution_succeeds_shielded_instead_of_dropping_the_order() {
        let ctx = mm_ctx_with_custom_db();
        let fee_and_confs = json!({"default_fee_per_kb": 1000, "n_blocks": 1});
        let conf = json!({"protocol": {"type": "LIGHTNING", "protocol_data": {
            "platform": "BTC",
            "network": "mainnet",
            "confirmations": {"background": fee_and_confs, "normal": fee_and_confs, "high_priority": fee_and_confs},
        }}});
        let result = orderbook_address(
            &ctx,
            "tBTC-lightning",
            &conf,
            "irrelevant-for-lightning",
            UtxoAddressFormat::Standard,
        );
        assert!(
            matches!(result, Ok(OrderbookAddress::Shielded)),
            "expected Ok(Shielded), got {result:?}"
        );
    }
}
