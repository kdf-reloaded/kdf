//! T-MP8 (CRD ch.32 §32.9B): the maker of a large book changes its orders quickly toward a relay that reads
//! slowly, in process.
//!
//! The maker is a real node: the production swarm (`spawn_gossipsub`, a light node), the production event loop,
//! the maker outbox and its pump, the keep-alive function and the order functions `maker_order_created_p2p_notify`
//! and `maker_order_updated_p2p_notify`. The relay is a plain gossipsub node (relay settings, ping, no
//! application) that reads one message at a time and sleeps after each, so it takes about `relay_msgs_per_s`
//! messages per second and answers pings between them. Its application model applies the created, updated and
//! cancelled messages it receives to an order set and records every keep-alive with its arrival time. Both sides
//! run over the in-memory transport with the production upgrade stack.
//!
//! What stands in for the rest of the network: the relay is not a KDF node, and the transport is the in-memory
//! one. A real relay that receives a keep-alive with a root it does not hold requests a synchronization from the
//! maker (CRD ch.32 §32.7.3). The relay here does the equivalent without the request: when a keep-alive arrives
//! and its model of that pair differs from the maker's orders (read from the maker's order book), it counts one
//! synchronization request and adopts the maker's orders for the pair at once. So the relay's order set can only
//! catch up with changes that were lost on the way after a keep-alive has reached it. The keep-alive tick is
//! driven by this file (the production loop's interval is a constant of the build), and calls the production
//! keep-alive function.
//!
//! The healthy-relay test runs by default. T-MP8 itself takes more than ten minutes per relay speed and is `#[ignore]`d.
//! Run both speeds, in parallel, with
//!
//! ```text
//! cargo test -p mm2_main --lib maker_outbox_lab_tests::t_mp8 -- --ignored --nocapture
//! ```
//!
//! or one of them as `t_mp8_relay_1_msg_per_s` or `t_mp8_relay_0_3_msg_per_s`. The load duration can be shortened
//! with `MP8_LOAD_SECS` for a trial; the binding criteria are for at least 600 s.

use super::{alb_ordered_pair, broadcast_keep_alives_for_own_pubkeys, maker_order_created_p2p_notify,
            maker_order_updated_p2p_notify, maker_outbox_loop, new_protocol, MakerOrder, OrderConfirmationsSettings,
            OrdermatchContext};
use crate::mm2::lp_native_dex::SwarmPollRecorder;
use crate::mm2::lp_network::{p2p_event_process_loop, P2PContext, RelayQueueSignal};
use crate::mm2::lp_swap::swap_versioning::SwapVersion;
use common::executor::{spawn, spawn_boxed};
use common::mm_number::BigRational;
use common::{block_on, now_ms};
use crypto::CryptoCtx;
use futures::channel::oneshot;
use futures::future::{select, Either};
use futures::{pin_mut, StreamExt};
use libp2p::core::transport::{Boxed as BoxedTransport, MemoryTransport};
use libp2p::core::{Multiaddr, Transport};
use libp2p::gossipsub::{Behaviour as Gossipsub, ConfigBuilder, Event as GossipsubEvent, Message, MessageAcceptance,
                        MessageAuthenticity, MessageId, ValidationMode};
use libp2p::swarm::{NetworkBehaviour, SwarmBuilder, SwarmEvent};
use libp2p::{identity, noise};
use mm2_core::mm_ctx::{MmArc, MmCtxBuilder};
use mm2_metrics::MetricsOps;
use mm2_p2p::atomicdex_behaviour::get_relay_mesh;
use mm2_p2p::{decode_signed, spawn_gossipsub, NodeType, RelayAddress};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

/// Same as the production message id (payload and sequence number through the standard hasher).
fn message_id(message: &Message) -> MessageId {
    let mut s = DefaultHasher::new();
    message.data.hash(&mut s);
    message.sequence_number.hash(&mut s);
    MessageId::from(s.finish().to_be_bytes().to_vec())
}

/// The production upgrade stack over the in-memory transport.
fn memory_transport(
    keypair: &identity::Keypair,
) -> BoxedTransport<(libp2p::PeerId, libp2p::core::muxing::StreamMuxerBox)> {
    MemoryTransport::default()
        .upgrade(libp2p::core::upgrade::Version::V1)
        .authenticate(noise::Config::new(keypair).expect("noise key"))
        .multiplex(libp2p::yamux::Config::default())
        .timeout(Duration::from_secs(20))
        .map(|(peer, muxer), _| (peer, libp2p::core::muxing::StreamMuxerBox::new(muxer)))
        .boxed()
}

#[derive(Clone, Debug, PartialEq)]
struct ModelOrder {
    price: BigRational,
    max_volume: BigRational,
    min_volume: BigRational,
}

/// A keep-alive as the relay saw it.
#[derive(Clone, Debug)]
struct KeepAliveSeen {
    pair: String,
    /// The timestamp the maker put in the message, in seconds.
    sent_secs: u64,
    received_ms: u64,
}

/// What the relay reads to emulate the synchronization it would request after a keep-alive.
struct SyncOracle {
    octx: Arc<OrdermatchContext>,
    /// The maker's orders with their ordered pair.
    orders: Vec<(Uuid, String)>,
}

#[derive(Default)]
struct RelayState {
    /// Emulates the synchronization request of a relay; `None` applies the messages only.
    oracle: Mutex<Option<SyncOracle>>,
    sync_requests: AtomicUsize,
    /// The relay's view of the maker's orders.
    model: Mutex<HashMap<Uuid, ModelOrder>>,
    keep_alives: Mutex<Vec<KeepAliveSeen>>,
    messages: AtomicUsize,
    updates: AtomicUsize,
    connects: Mutex<Vec<u64>>,
    closes: Mutex<Vec<u64>>,
}

impl RelayState {
    fn apply(&self, message: new_protocol::OrdermatchMessage) {
        match message {
            new_protocol::OrdermatchMessage::MakerOrderCreated(created) => {
                self.model.lock().unwrap().insert(created.uuid.into(), ModelOrder {
                    price: created.price,
                    max_volume: created.max_volume,
                    min_volume: created.min_volume,
                });
            },
            new_protocol::OrdermatchMessage::MakerOrderUpdated(updated) => {
                self.updates.fetch_add(1, Ordering::Relaxed);
                if let Some(order) = self.model.lock().unwrap().get_mut(&updated.uuid()) {
                    if let Some(price) = updated.new_price() {
                        order.price = price.to_ratio();
                    }
                    if let Some(max_volume) = updated.new_max_volume() {
                        order.max_volume = max_volume.to_ratio();
                    }
                    if let Some(min_volume) = updated.new_min_volume() {
                        order.min_volume = min_volume.to_ratio();
                    }
                }
            },
            new_protocol::OrdermatchMessage::MakerOrderCancelled(cancelled) => {
                self.model.lock().unwrap().remove(&cancelled.uuid.into());
            },
            new_protocol::OrdermatchMessage::PubkeyKeepAlive(keep_alive) => {
                let received_ms = now_ms();
                {
                    let mut seen = self.keep_alives.lock().unwrap();
                    for pair in keep_alive.trie_roots.keys() {
                        seen.push(KeepAliveSeen {
                            pair: pair.clone(),
                            sent_secs: keep_alive.timestamp,
                            received_ms,
                        });
                    }
                }
                for pair in keep_alive.trie_roots.keys() {
                    self.synchronize(pair);
                }
            },
            _ => {},
        }
    }
}

impl RelayState {
    /// The relay's synchronization of one pair after a keep-alive, if the model differs from the maker.
    fn synchronize(&self, pair: &str) {
        let oracle = self.oracle.lock().unwrap();
        let Some(oracle) = oracle.as_ref() else {
            return;
        };
        let maker_view: HashMap<Uuid, ModelOrder> = {
            let orderbook = oracle.octx.orderbook.lock();
            oracle
                .orders
                .iter()
                .filter(|(_, order_pair)| order_pair == pair)
                .filter_map(|(uuid, _)| {
                    orderbook.find_order_by_uuid(uuid).map(|item| {
                        (*uuid, ModelOrder {
                            price: item.price,
                            max_volume: item.max_volume,
                            min_volume: item.min_volume,
                        })
                    })
                })
                .collect()
        };
        let mut model = self.model.lock().unwrap();
        let held: HashMap<Uuid, ModelOrder> = oracle
            .orders
            .iter()
            .filter(|(_, order_pair)| order_pair == pair)
            .filter_map(|(uuid, _)| model.get(uuid).map(|order| (*uuid, order.clone())))
            .collect();
        if held != maker_view {
            self.sync_requests.fetch_add(1, Ordering::Relaxed);
            model.extend(maker_view);
        }
    }
}

#[derive(NetworkBehaviour)]
struct RelayBehaviour {
    gossipsub: Gossipsub,
    ping: libp2p::ping::Behaviour,
}

struct SlowRelay {
    port: u64,
    state: Arc<RelayState>,
    /// Sleep after every message, in microseconds; 0 reads at full speed.
    delay_us: Arc<AtomicU64>,
    stop: Option<oneshot::Sender<()>>,
}

static NEXT_PORT: AtomicU64 = AtomicU64::new(61_000);

impl SlowRelay {
    fn start() -> SlowRelay {
        let keypair = identity::Keypair::generate_ed25519();
        let peer_id = libp2p::PeerId::from(keypair.public());
        let mut builder = ConfigBuilder::default();
        // The relay settings of the production node.
        builder
            .message_id_fn(message_id)
            .i_am_relay(true)
            .mesh_n_low(4)
            .mesh_n(8)
            .mesh_n_high(12)
            .mesh_outbound_min(2)
            .validate_messages()
            .validation_mode(ValidationMode::Permissive)
            .max_transmit_size(1024 * 1024 - 100);
        let gossipsub = Gossipsub::new(
            MessageAuthenticity::Author(peer_id),
            builder.build().expect("valid gossipsub config"),
        )
        .expect("gossipsub");
        let behaviour = RelayBehaviour {
            gossipsub,
            ping: libp2p::ping::Behaviour::new(libp2p::ping::Config::new()),
        };
        // No executor: connections are polled inside `Swarm::poll`, so the sleeping relay reads nothing.
        let mut swarm = SwarmBuilder::without_executor(memory_transport(&keypair), behaviour, peer_id).build();
        let port = NEXT_PORT.fetch_add(1, Ordering::Relaxed);
        swarm
            .listen_on(format!("/memory/{port}").parse::<Multiaddr>().unwrap())
            .expect("listen");

        let state = Arc::new(RelayState::default());
        let delay_us = Arc::new(AtomicU64::new(0));
        let (stop_tx, mut stop_rx) = oneshot::channel::<()>();
        let (state_task, delay_task) = (state.clone(), delay_us.clone());
        async_std::task::spawn(async move {
            loop {
                let event = {
                    let next = swarm.select_next_some();
                    pin_mut!(next);
                    match select(&mut stop_rx, next).await {
                        Either::Left(_) => return,
                        Either::Right((event, _)) => event,
                    }
                };
                match event {
                    SwarmEvent::Behaviour(RelayBehaviourEvent::Gossipsub(GossipsubEvent::Message {
                        propagation_source,
                        message_id,
                        message,
                    })) => {
                        state_task.messages.fetch_add(1, Ordering::Relaxed);
                        if let Ok((decoded, _sig, _pubkey)) =
                            decode_signed::<new_protocol::OrdermatchMessage>(&message.data)
                        {
                            state_task.apply(decoded);
                        }
                        // `validate_messages` is on: report a verdict so the message is released. `Ignore`
                        // does not forward it.
                        let _ = swarm.behaviour_mut().gossipsub.report_message_validation_result(
                            &message_id,
                            &propagation_source,
                            MessageAcceptance::Ignore,
                        );
                        let delay = delay_task.load(Ordering::Relaxed);
                        if delay > 0 {
                            async_std::task::sleep(Duration::from_micros(delay)).await;
                        }
                    },
                    SwarmEvent::ConnectionEstablished { .. } => state_task.connects.lock().unwrap().push(now_ms()),
                    SwarmEvent::ConnectionClosed { num_established: 0, .. } => {
                        state_task.closes.lock().unwrap().push(now_ms())
                    },
                    _ => {},
                }
            }
        });
        SlowRelay {
            port,
            state,
            delay_us,
            stop: Some(stop_tx),
        }
    }

    fn set_rate(&self, msgs_per_s: f64) {
        let delay = if msgs_per_s > 0.0 {
            (1_000_000.0 / msgs_per_s) as u64
        } else {
            0
        };
        self.delay_us.store(delay, Ordering::Relaxed);
    }
}

impl Drop for SlowRelay {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

fn ratio(n: u64) -> BigRational { BigRational::from_integer(n.into()) }

fn maker_order(base: &str, rel: &str) -> MakerOrder {
    MakerOrder {
        uuid: Uuid::new_v4(),
        base: base.into(),
        rel: rel.into(),
        created_at: now_ms(),
        updated_at: Some(now_ms()),
        matches: HashMap::new(),
        max_base_vol: 10.into(),
        min_base_vol: 1.into(),
        price: 1.into(),
        started_swaps: vec![],
        conf_settings: Some(OrderConfirmationsSettings::default()),
        changes_history: None,
        save_in_history: false,
        base_orderbook_ticker: None,
        rel_orderbook_ticker: None,
        p2p_privkey: None,
        timeout_in_minutes: None,
        swap_version: SwapVersion::default(),
    }
}

struct Params {
    orders: usize,
    pairs: usize,
    updates_per_s: f64,
    relay_msgs_per_s: f64,
    load_secs: u64,
    keep_alive_secs: u64,
    converge_secs: u64,
    /// Seconds between two printed rows.
    row_secs: u64,
}

fn env_or<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// A gauge of the node's metrics, as the node reports it; -1 if it has not been recorded.
fn gauge(ctx: &MmArc, key: &str) -> i64 {
    let json = ctx.metrics.collect_json().unwrap();
    json["metrics"]
        .as_array()
        .and_then(|metrics| metrics.iter().find(|m| m["key"] == key))
        .and_then(|m| m["value"].as_i64())
        .unwrap_or(-1)
}

/// Deterministic xorshift generator, so every run picks the same orders.
struct Rng64(u64);

impl Rng64 {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// The result of a run, checked by the tests.
struct Outcome {
    /// Seconds from the load start to the first signal (a queue signal, a close signal or degraded mode) the maker
    /// reported in its metrics; `None` if there was none.
    first_signal_s: Option<f64>,
    /// For every tick from the first signal on, and every pair: the delay in seconds from the tick to the arrival of
    /// its keep-alive, `None` if it never arrived.
    delays_after: Vec<Option<f64>>,
    /// The longest gap between delivered keep-alives from the first signal to the end of the load, and before it.
    gap_after_s: f64,
    gap_before_s: f64,
    converged_after_s: Option<f64>,
    degraded_seen: bool,
}

fn run(params: Params) -> Outcome {
    println!(
        "\n### T-MP8: {} orders on {} pairs, {} updates/s for {} s, relay reads {} msg/s, keep-alive every {} s",
        params.orders,
        params.pairs,
        params.updates_per_s,
        params.load_secs,
        params.relay_msgs_per_s,
        params.keep_alive_secs
    );
    let relay = SlowRelay::start();

    // The maker: the production swarm, event loop, outbox pump and keep-alive function.
    let ctx = MmCtxBuilder::new()
        .with_conf(json!({ "maker_update_coalescing": true }))
        .into_mm_arc();
    ctx.init_metrics().unwrap();
    let crypto_ctx = CryptoCtx::init_with_iguana_passphrase(ctx.clone(), "maker rate control lab passphrase").unwrap();
    let persistent_pubkey = crypto_ctx.mm2_internal_pubkey_hex();
    let relay_queue_signal = Arc::new(RelayQueueSignal::default());
    let recorder = SwarmPollRecorder::new(ctx.clone(), relay_queue_signal.clone());
    let (cmd_tx, event_rx, _peer_id, abort) = block_on(spawn_gossipsub(
        None,
        spawn_boxed,
        vec![RelayAddress::Memory(relay.port)],
        NodeType::LightInMemory,
        move |swarm| recorder.record(swarm.behaviour()),
    ))
    .expect("spawn the maker swarm");
    P2PContext::new(cmd_tx.clone())
        .with_relay_queue_signal(relay_queue_signal)
        .store_to_mm_arc(&ctx);
    spawn(p2p_event_process_loop(ctx.weak(), event_rx, false));
    spawn(maker_outbox_loop(ctx.clone()));

    // The connection and the relay mesh.
    let started = Instant::now();
    while block_on(get_relay_mesh(cmd_tx.clone())).is_empty() {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the maker never joined the relay mesh"
        );
        std::thread::sleep(Duration::from_millis(200));
    }

    // The book: created through the production function; the relay reads at full speed meanwhile.
    let octx = OrdermatchContext::from_ctx(&ctx).unwrap();
    let pair_names: Vec<(String, String)> = (0..params.pairs)
        .map(|i| (format!("LA{i}"), format!("LB{i}")))
        .collect();
    let mut orders: Vec<(Uuid, String)> = Vec::new();
    let mut order_pairs: Vec<(Uuid, String)> = Vec::new();
    for i in 0..params.orders {
        let (base, rel) = &pair_names[i % params.pairs];
        let order = maker_order(base, rel);
        orders.push((order.uuid, order.orderbook_topic()));
        order_pairs.push((order.uuid, alb_ordered_pair(base, rel)));
        maker_order_created_p2p_notify(ctx.clone(), &order, vec![], vec![]);
    }
    let setup_started = Instant::now();
    loop {
        let pending = octx.maker_outbox.outboxes.lock().metrics(20.0).pending;
        let known = relay.state.model.lock().unwrap().len();
        if pending == 0 && known == params.orders {
            break;
        }
        assert!(
            setup_started.elapsed() < Duration::from_secs(300),
            "the book was not published: pending {pending}, relay knows {known}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    println!(
        "book of {} orders published in {:.1} s",
        params.orders,
        setup_started.elapsed().as_secs_f64()
    );
    octx.wait_trie_ops_flushed();
    *relay.state.oracle.lock().unwrap() = Some(SyncOracle {
        octx: octx.clone(),
        orders: order_pairs,
    });

    // The load.
    relay.set_rate(params.relay_msgs_per_s);
    let load_start = Instant::now();
    let load_start_ms = now_ms();
    let stop = Arc::new(AtomicBool::new(false));
    let issued = Arc::new(AtomicU64::new(0));
    let updater = {
        let (ctx, stop, issued, orders) = (ctx.clone(), stop.clone(), issued.clone(), orders.clone());
        let ups = params.updates_per_s;
        std::thread::spawn(move || {
            let mut rng = Rng64(0x9E37_79B9_7F4A_7C15);
            let mut counter = 100u64;
            let t0 = Instant::now();
            while !stop.load(Ordering::Relaxed) {
                let target = (t0.elapsed().as_secs_f64() * ups) as u64;
                while issued.load(Ordering::Relaxed) < target {
                    let (uuid, topic) = &orders[(rng.next() % orders.len() as u64) as usize];
                    counter += 1;
                    let mut message = new_protocol::MakerOrderUpdated::new(*uuid);
                    message.with_new_price(ratio(counter));
                    maker_order_updated_p2p_notify(ctx.clone(), topic.clone(), message, None);
                    issued.fetch_add(1, Ordering::Relaxed);
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        })
    };
    let keep_alive_ticks: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
    let keep_alive_stop = Arc::new(AtomicBool::new(false));
    let keep_alive_thread = {
        let (ctx, stop, ticks, persistent_pubkey) = (
            ctx.clone(),
            keep_alive_stop.clone(),
            keep_alive_ticks.clone(),
            persistent_pubkey.clone(),
        );
        let interval = Duration::from_secs(params.keep_alive_secs);
        std::thread::spawn(move || {
            let octx = OrdermatchContext::from_ctx(&ctx).unwrap();
            let t0 = Instant::now();
            let mut n = 1u32;
            while !stop.load(Ordering::Relaxed) {
                if t0.elapsed() >= interval * n {
                    ticks.lock().unwrap().push(now_ms());
                    block_on(broadcast_keep_alives_for_own_pubkeys(&ctx, &octx, &persistent_pubkey));
                    n += 1;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        })
    };

    println!(
        "\n| t s | issued | pending | rate msg/s | degraded | queue signals | close signals | queued | dropped | relay got | updates | keep-alives | syncs |"
    );
    println!("|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|");
    let row = |t: f64| {
        println!(
            "| {:.0} | {} | {} | {:.3} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            t,
            issued.load(Ordering::Relaxed),
            gauge(&ctx, "p2p.outbox.pending"),
            gauge(&ctx, "p2p.outbox.rate") as f64 / 1000.0,
            gauge(&ctx, "p2p.outbox.degraded"),
            gauge(&ctx, "p2p.outbox.signals.queue.count"),
            gauge(&ctx, "p2p.outbox.signals.close.count"),
            gauge(&ctx, "p2p.send_queue.len"),
            gauge(&ctx, "p2p.send_queue.dropped.count"),
            relay.state.messages.load(Ordering::Relaxed),
            relay.state.updates.load(Ordering::Relaxed),
            relay.state.keep_alives.lock().unwrap().len(),
            relay.state.sync_requests.load(Ordering::Relaxed),
        );
    };
    let mut first_signal_ms: Option<u64> = None;
    let mut degraded_seen = false;
    let mut watch = |first_signal_ms: &mut Option<u64>| {
        if first_signal_ms.is_none()
            && (gauge(&ctx, "p2p.outbox.signals.queue.count") > 0
                || gauge(&ctx, "p2p.outbox.signals.close.count") > 0
                || gauge(&ctx, "p2p.outbox.degraded") > 0)
        {
            *first_signal_ms = Some(now_ms());
        }
        degraded_seen |= gauge(&ctx, "p2p.outbox.degraded") > 0;
    };
    let mut next_row = Duration::ZERO;
    let mut next_watch = Duration::ZERO;
    while load_start.elapsed() < Duration::from_secs(params.load_secs) {
        if load_start.elapsed() >= next_watch {
            watch(&mut first_signal_ms);
            next_watch += Duration::from_secs(1);
        }
        if load_start.elapsed() >= next_row {
            row(load_start.elapsed().as_secs_f64());
            next_row += Duration::from_secs(params.row_secs);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let load_end_ms = now_ms();
    let load_issued = issued.load(Ordering::Relaxed);
    // The load stops; the keep-alives and the pump go on.
    stop.store(true, Ordering::Relaxed);
    updater.join().unwrap();
    println!("\nload stopped after {} updates", load_issued);

    // Convergence: the relay's order set equals the maker's.
    let converge_started = Instant::now();
    let mut converged_after = None;
    let mut next_row = Duration::ZERO;
    while converge_started.elapsed() < Duration::from_secs(params.converge_secs) {
        let maker_view: HashMap<Uuid, ModelOrder> = {
            let orderbook = octx.orderbook.lock();
            orders
                .iter()
                .filter_map(|(uuid, _)| {
                    orderbook.find_order_by_uuid(uuid).map(|item| {
                        (*uuid, ModelOrder {
                            price: item.price,
                            max_volume: item.max_volume,
                            min_volume: item.min_volume,
                        })
                    })
                })
                .collect()
        };
        if *relay.state.model.lock().unwrap() == maker_view {
            converged_after = Some(converge_started.elapsed().as_secs_f64());
            break;
        }
        watch(&mut first_signal_ms);
        if converge_started.elapsed() >= next_row {
            row(params.load_secs as f64 + converge_started.elapsed().as_secs_f64());
            next_row += Duration::from_secs(params.row_secs);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    // Wait on a condition, not on a guess: the keep-alives of the last tick that is judged (one within the load) may still
    // be on their way when the order sets match. They have up to 20 s, which is more than the bounded wait of R-MP3 item 2
    // and a slow link add, and the wait ends as soon as every pair's keep-alive of that tick has arrived.
    let last_judged_tick_s = keep_alive_ticks
        .lock()
        .unwrap()
        .iter()
        .rfind(|t| **t <= load_end_ms)
        .map(|t| t / 1000);
    if let Some(tick_s) = last_judged_tick_s {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            let seen = relay.state.keep_alives.lock().unwrap();
            let pairs: std::collections::BTreeSet<&String> = seen.iter().map(|k| &k.pair).collect();
            let arrived = |pair: &String| seen.iter().any(|k| &k.pair == pair && k.sent_secs >= tick_s);
            if !pairs.is_empty() && pairs.iter().all(|pair| arrived(pair)) {
                break;
            }
            drop(seen);
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    keep_alive_stop.store(true, Ordering::Relaxed);
    keep_alive_thread.join().unwrap();

    // The keep-alives: for every tick and pair, the delay from the tick to the arrival.
    let seen = relay.state.keep_alives.lock().unwrap().clone();
    let ticks = keep_alive_ticks.lock().unwrap().clone();
    let pairs_seen: std::collections::BTreeSet<String> = seen.iter().map(|k| k.pair.clone()).collect();
    let rel = |ms: u64| (ms as f64 - load_start_ms as f64) / 1000.0;
    // A keep-alive carries the second its roots were read, up to the 10 s wait and a second after the tick.
    let delay_of = |tick_ms: u64, pair: &String| {
        let tick_s = tick_ms / 1000;
        seen.iter()
            .filter(|k| &k.pair == pair && (tick_s..=tick_s + 12).contains(&k.sent_secs))
            .map(|k| k.received_ms.saturating_sub(tick_ms) as f64 / 1000.0)
            .fold(None, |best: Option<f64>, d| Some(best.map_or(d, |b| b.min(d))))
    };
    let first_signal_s = first_signal_ms.map(rel);
    let signal_from_ms = first_signal_ms.unwrap_or(load_start_ms);
    println!("\nkeep-alives: tick at (s from the load start), pair, delay in s");
    let mut delays_after = Vec::new();
    for tick_ms in ticks.iter().filter(|t| **t >= load_start_ms) {
        for pair in &pairs_seen {
            let delay = delay_of(*tick_ms, pair);
            println!(
                "| {:.0} | {} | {} |",
                rel(*tick_ms),
                pair,
                delay.map_or("not delivered".to_owned(), |d| format!("{:.1}", d))
            );
            if *tick_ms >= signal_from_ms && *tick_ms <= load_end_ms {
                delays_after.push(delay);
            }
        }
    }
    // The gaps between delivered keep-alives (any pair), with the boundaries of the window as ends.
    let gap = |from_ms: u64, to_ms: u64| {
        let mut points: Vec<f64> = seen
            .iter()
            .filter(|k| k.received_ms >= from_ms && k.received_ms <= to_ms)
            .map(|k| k.received_ms as f64 / 1000.0)
            .collect();
        points.sort_by(|a, b| a.partial_cmp(b).unwrap());
        points.insert(0, from_ms as f64 / 1000.0);
        points.push(to_ms as f64 / 1000.0);
        points.windows(2).map(|w| w[1] - w[0]).fold(0.0, f64::max)
    };
    let gap_after_s = gap(signal_from_ms, load_end_ms);
    let gap_before_s = if first_signal_ms.is_some() {
        gap(load_start_ms, signal_from_ms)
    } else {
        0.0
    };

    let mut sorted: Vec<f64> = delays_after.iter().map(|d| d.unwrap_or(f64::INFINITY)).collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "\nSUMMARY relay {} msg/s: first signal at {:?} s after the load start (degraded seen: {})",
        params.relay_msgs_per_s, first_signal_s, degraded_seen
    );
    println!(
        "SUMMARY after the first signal: {} keep-alives expected, {} delivered, delay median {:?} s, max {:?} s",
        delays_after.len(),
        delays_after.iter().flatten().count(),
        sorted.get(sorted.len() / 2),
        sorted.last()
    );
    println!(
        "SUMMARY longest gap between delivered keep-alives: {:.1} s before the first signal, {:.1} s from it to the end of the load",
        gap_before_s, gap_after_s
    );
    println!(
        "SUMMARY the relay's order set equals the maker's {:?} s after the load stopped; sync requests {}",
        converged_after,
        relay.state.sync_requests.load(Ordering::Relaxed)
    );
    println!(
        "SUMMARY gauges at the end: rate {} (thousandths msg/s), degraded {}, pressure {}, queue signals {}, close signals {}, queued {}, dropped {}",
        gauge(&ctx, "p2p.outbox.rate"),
        gauge(&ctx, "p2p.outbox.degraded"),
        gauge(&ctx, "p2p.outbox.pressure"),
        gauge(&ctx, "p2p.outbox.signals.queue.count"),
        gauge(&ctx, "p2p.outbox.signals.close.count"),
        gauge(&ctx, "p2p.send_queue.len"),
        gauge(&ctx, "p2p.send_queue.dropped.count"),
    );
    abort.abort();
    Outcome {
        first_signal_s,
        delays_after,
        gap_after_s,
        gap_before_s,
        converged_after_s: converged_after,
        degraded_seen,
    }
}

/// The pass criteria of T-MP8: after the first signal the median tick-to-arrival delay is at most 10 s and no keep-alive
/// arrives later than 60 s after its tick, no gap between delivered keep-alives exceeds 90 s (before the first signal
/// one such gap is allowed), and the relay's order set converges within 120 s of the load stopping.
///
/// A keep-alive that never arrives is not an arrival: it counts as infinitely late in the median, and the 90 s gap
/// bounds how long keep-alives can stay missing. The keep-alives that never arrive are those sent into a connection that
/// then closes; the number is printed in the summary.
fn check_t_mp8(outcome: &Outcome) {
    let mut all: Vec<f64> = outcome
        .delays_after
        .iter()
        .map(|d| d.unwrap_or(f64::INFINITY))
        .collect();
    all.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert!(!all.is_empty(), "no keep-alive tick after the first signal");
    let median = all[all.len() / 2];
    assert!(median <= 10.0, "median keep-alive delay {median} s");
    let latest = outcome.delays_after.iter().flatten().fold(0.0_f64, |a, b| a.max(*b));
    assert!(latest <= 60.0, "a keep-alive arrived {latest} s after its tick");
    assert!(
        outcome.gap_after_s <= 90.0,
        "a gap of {:.1} s between delivered keep-alives after the first signal",
        outcome.gap_after_s
    );
    let converged = outcome.converged_after_s;
    assert!(
        converged.is_some_and(|s| s <= 120.0),
        "the relay's order set converged after {converged:?} s"
    );
}

/// The harness with a relay that keeps up (it reads 20 per second, the maker releases at most 20): no signal, the rate
/// stays at the ceiling, the keep-alives arrive promptly and the relay's order set equals the maker's. This is a
/// pipeline check of the harness that T-MP8 uses, not the slow-relay scenario.
///
/// The bounds are loose on purpose, so that a busy CI runner does not trip them, and still far from what a held-back
/// keep-alive would show: a keep-alive that waited out the bounded wait of R-MP3 item 2 arrives 10 s or more after its
/// tick, and a signal would show as a first signal or degraded mode.
#[test]
fn t_mp8_harness_with_a_relay_that_keeps_up() {
    /// The longest a keep-alive may take from its tick on a healthy link, half of the bounded wait.
    const PROMPT_S: f64 = 5.0;
    let outcome = run(Params {
        orders: 60,
        pairs: 2,
        updates_per_s: 8.0,
        relay_msgs_per_s: 20.0,
        load_secs: 12,
        keep_alive_secs: 4,
        converge_secs: 90,
        row_secs: 4,
    });
    assert_eq!(
        outcome.first_signal_s, None,
        "a signal came toward a relay that keeps up"
    );
    assert!(!outcome.degraded_seen);
    assert!(
        !outcome.delays_after.is_empty(),
        "no keep-alive tick was judged: {:?}",
        outcome.delays_after
    );
    assert!(
        outcome.delays_after.iter().all(|d| d.is_some_and(|d| d <= PROMPT_S)),
        "a keep-alive took more than {PROMPT_S} s or never came: {:?}",
        outcome.delays_after
    );
    assert!(outcome.converged_after_s.is_some_and(|s| s <= 60.0));
}

fn t_mp8(relay_msgs_per_s: f64) {
    let outcome = run(Params {
        orders: 600,
        pairs: 3,
        updates_per_s: 90.0,
        relay_msgs_per_s,
        load_secs: env_or("MP8_LOAD_SECS", 600),
        keep_alive_secs: 30,
        converge_secs: 300,
        row_secs: 30,
    });
    check_t_mp8(&outcome);
}

/// T-MP8: 600 orders, 90 updates per second for at least 600 s, toward a relay that reads about 1 message per second.
#[test]
#[ignore = "takes about 15 minutes; run with --ignored (see the module documentation)"]
fn t_mp8_relay_1_msg_per_s() { t_mp8(1.0) }

/// T-MP8 toward a relay that reads about 0.3 messages per second.
#[test]
#[ignore = "takes about 15 minutes; run with --ignored (see the module documentation)"]
fn t_mp8_relay_0_3_msg_per_s() { t_mp8(0.3) }
