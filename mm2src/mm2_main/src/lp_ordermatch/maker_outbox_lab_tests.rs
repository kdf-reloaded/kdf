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
//! The small-scale test with a relay that keeps up runs by default. The full scenario is `#[ignore]`d; run it with
//!
//! ```text
//! cargo test -p mm2_main --lib maker_outbox_lab_tests::t_mp8_full -- --ignored --nocapture
//! ```
//!
//! Environment of the full run: `MP8_LOAD_SECS` (load duration, default 300), `MP8_CONVERGE_SECS` (how long to
//! wait for convergence after the load, default 900), `MP8_RELAY_MSGS_PER_S` (default 1), `MP8_RELAY_SYNCS`
//! (`false` makes the relay apply the messages only, default `true`), `MP8_COALESCING` (the maker's
//! `maker_update_coalescing`, default `true`; `false` is the comparison without the outbox).

use super::{alb_ordered_pair, broadcast_keep_alives_for_own_pubkeys, maker_order_created_p2p_notify,
            maker_order_updated_p2p_notify, maker_outbox_loop, new_protocol, MakerOrder, OrderConfirmationsSettings,
            OrdermatchContext};
use crate::mm2::lp_native_dex::SwarmPollRecorder;
use crate::mm2::lp_network::{p2p_event_process_loop, P2PContext};
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
    /// Whether the relay emulates the synchronization it requests after a keep-alive.
    relay_syncs: bool,
    /// The `maker_update_coalescing` setting of the maker; `false` publishes every message at once.
    coalescing: bool,
    /// Candidate C1: the flush before a keep-alive and the reconnect round release within the budget.
    c1: bool,
    /// Candidate C2: ping round-trip pressure.
    c2: bool,
    /// Candidate C4: a ping failure does not close the connection while the outbox is degraded.
    c4: bool,
    /// Candidate C1': C1, but a keep-alive waits at most 10 s for its pairs to drain.
    c1p: bool,
    /// Candidates C5a (1) and C5b (2): a relay connection close is the pressure signal.
    c5: u8,
    /// Candidate C6: pressure when the data queue has been non-empty for 2 s.
    c6: bool,
    /// Seconds between two printed rows.
    row_secs: u64,
}

fn env_or<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

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
    /// (published at s from the load start, pair, latency in s or `None` when not delivered).
    keep_alives: Vec<(f64, String, Option<f64>)>,
    longest_gap_s: f64,
    converged_after_s: Option<f64>,
    closes: usize,
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
    println!(
        "candidates: C1 {} C1' {} C2 {} C4 {} C5 {} C6 {}",
        params.c1, params.c1p, params.c2, params.c4, params.c5, params.c6
    );
    {
        use super::maker_outbox::lab;
        mm2_p2p::lab_knobs::reset_run();
        lab::CLOSES_SEEN.store(0, Ordering::Relaxed);
        lab::LAST_RELEASE_MS.store(0, Ordering::Relaxed);
        lab::MODE.store(params.c5, Ordering::Relaxed);
        lab::WAIT_CAP_MS.store(if params.c1p { 10_000 } else { u64::MAX }, Ordering::Relaxed);
        mm2_p2p::lab_knobs::set_c6(params.c6);
        mm2_p2p::lab_knobs::set_degraded(false);
    }
    super::maker_outbox::lab::BUDGETED_FLUSH.store(params.c1 || params.c1p, Ordering::Relaxed);
    super::maker_outbox::lab::SLOT_MS.store(params.keep_alive_secs * 1000, Ordering::Relaxed);
    mm2_p2p::lab_knobs::set_rtt_pressure(params.c2);
    mm2_p2p::lab_knobs::set_hold_ping_close_when_degraded(params.c4);
    let relay = SlowRelay::start();

    // The maker: the production swarm, event loop, outbox pump and keep-alive function.
    let ctx = MmCtxBuilder::new()
        .with_conf(json!({ "maker_update_coalescing": params.coalescing }))
        .into_mm_arc();
    ctx.init_metrics().unwrap();
    let crypto_ctx = CryptoCtx::init_with_iguana_passphrase(ctx.clone(), "maker rate control lab passphrase").unwrap();
    let persistent_pubkey = crypto_ctx.mm2_internal_pubkey_hex();
    let relay_pressure = Arc::new(AtomicBool::new(false));
    let recorder = SwarmPollRecorder::new(ctx.clone(), relay_pressure.clone());
    let (cmd_tx, event_rx, _peer_id, abort) = block_on(spawn_gossipsub(
        None,
        spawn_boxed,
        vec![RelayAddress::Memory(relay.port)],
        NodeType::LightInMemory,
        move |swarm| recorder.record(swarm.behaviour()),
    ))
    .expect("spawn the maker swarm");
    P2PContext::new(cmd_tx.clone())
        .with_relay_pressure(relay_pressure)
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
        let pending = octx.maker_outbox.outboxes.lock().metrics(10.0).pending;
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
    if params.relay_syncs {
        *relay.state.oracle.lock().unwrap() = Some(SyncOracle {
            octx: octx.clone(),
            orders: order_pairs,
        });
    }

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

    println!("\n| t s | issued | pending | rate msg/s | pressure | queued | dropped | relay got | updates | keep-alives | syncs | relay closes |");
    println!("|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|");
    let row = |t: f64| {
        println!(
            "| {:.0} | {} | {} | {:.3} | {} | {} | {} | {} | {} | {} | {} | {} |",
            t,
            issued.load(Ordering::Relaxed),
            gauge(&ctx, "p2p.outbox.pending"),
            gauge(&ctx, "p2p.outbox.rate") as f64 / 1000.0,
            gauge(&ctx, "p2p.outbox.pressure"),
            gauge(&ctx, "p2p.send_queue.len"),
            gauge(&ctx, "p2p.send_queue.dropped.count"),
            relay.state.messages.load(Ordering::Relaxed),
            relay.state.updates.load(Ordering::Relaxed),
            relay.state.keep_alives.lock().unwrap().len(),
            relay.state.sync_requests.load(Ordering::Relaxed),
            relay.state.closes.lock().unwrap().len(),
        );
    };
    let mut next_row = Duration::ZERO;
    let mut next_sample = Duration::ZERO;
    let mut peak_queue = 0i64;
    let mut rate_trace: Vec<(u64, f64)> = Vec::new();
    // The messages the outbox had released and the relay had read when the handler queue first held something.
    let mut hidden_backlog: Option<(u64, usize)> = None;
    let released_at_load_start = super::maker_outbox::lab::RELEASED.load(Ordering::Relaxed);
    let mut next_trace = Duration::ZERO;
    let mut first_degraded_ms: Option<u64> = None;
    while load_start.elapsed() < Duration::from_secs(params.load_secs) {
        if first_degraded_ms.is_none() && mm2_p2p::lab_knobs::degraded() {
            first_degraded_ms = Some(now_ms());
        }
        if load_start.elapsed() >= next_sample {
            let queued = gauge(&ctx, "p2p.send_queue.len");
            peak_queue = peak_queue.max(queued);
            if queued > 0 && hidden_backlog.is_none() {
                hidden_backlog = Some((
                    super::maker_outbox::lab::RELEASED.load(Ordering::Relaxed) - released_at_load_start,
                    relay.state.messages.load(Ordering::Relaxed),
                ));
            }
            next_sample += Duration::from_secs(1);
        }
        if load_start.elapsed() >= next_trace {
            rate_trace.push((
                load_start.elapsed().as_secs(),
                gauge(&ctx, "p2p.outbox.rate") as f64 / 1000.0,
            ));
            next_trace += Duration::from_secs(30);
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
        peak_queue = peak_queue.max(gauge(&ctx, "p2p.send_queue.len"));
        if first_degraded_ms.is_none() && mm2_p2p::lab_knobs::degraded() {
            first_degraded_ms = Some(now_ms());
        }
        if converge_started.elapsed() >= next_row {
            row(params.load_secs as f64 + converge_started.elapsed().as_secs_f64());
            next_row += Duration::from_secs(params.row_secs);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    keep_alive_stop.store(true, Ordering::Relaxed);
    keep_alive_thread.join().unwrap();
    let end_ms = now_ms();

    // The judgement of the keep-alives published during the load and its converge phase.
    let seen = relay.state.keep_alives.lock().unwrap().clone();
    let ticks = keep_alive_ticks.lock().unwrap().clone();
    let mut keep_alives: Vec<(f64, String, Option<f64>)> = Vec::new();
    let pairs_seen: std::collections::BTreeSet<String> = seen.iter().map(|k| k.pair.clone()).collect();
    for tick_ms in ticks.iter().filter(|t| **t >= load_start_ms) {
        let tick_s = tick_ms / 1000;
        for pair in &pairs_seen {
            // A keep-alive carries the second its roots were read, within a second of the tick.
            let latency = seen
                .iter()
                .filter(|k| &k.pair == pair && (tick_s..=tick_s + 2).contains(&k.sent_secs))
                .map(|k| (k.received_ms.saturating_sub(*tick_ms)) as f64 / 1000.0)
                .fold(None, |best: Option<f64>, l| Some(best.map_or(l, |b| b.min(l))));
            keep_alives.push(((*tick_ms - load_start_ms) as f64 / 1000.0, pair.clone(), latency));
        }
    }
    // Gaps between delivered keep-alives (any pair) over the load; the load's start and end are boundaries.
    let mut arrivals: Vec<f64> = seen
        .iter()
        .filter(|k| k.received_ms >= load_start_ms && k.received_ms <= load_end_ms)
        .map(|k| (k.received_ms - load_start_ms) as f64 / 1000.0)
        .collect();
    arrivals.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let load_s = (load_end_ms - load_start_ms) as f64 / 1000.0;
    let mut points = vec![0.0];
    points.extend(arrivals);
    points.push(load_s);
    let longest_gap_s = points.windows(2).map(|w| w[1] - w[0]).fold(0.0, f64::max);

    println!("\nkeep-alives published by the tick (s from the load start); latency in s");
    println!("| tick at | pair | latency |");
    println!("|---:|:---|---:|");
    for (at, pair, latency) in &keep_alives {
        match latency {
            Some(l) => println!("| {:.0} | {} | {:.1} |", at, pair, l),
            None => println!("| {:.0} | {} | not delivered |", at, pair),
        }
    }
    let delivered = keep_alives.iter().filter(|k| k.2.is_some()).count();
    let slow = keep_alives.iter().filter(|k| k.2.is_none_or(|l| l > 30.0)).count();
    println!(
        "\nkeep-alives: {} published by the tick, {} delivered, {} not within 30 s",
        keep_alives.len(),
        delivered,
        slow
    );
    println!(
        "longest gap between delivered keep-alives during the load: {:.1} s (limit 90 s)",
        longest_gap_s
    );
    match converged_after {
        Some(s) => println!(
            "the relay's order set equals the maker's {:.1} s after the load stopped",
            s
        ),
        None => println!(
            "the relay's order set did NOT converge within {} s",
            params.converge_secs
        ),
    }
    let closes = relay.state.closes.lock().unwrap().len();
    println!(
        "relay connection closes: {} at (s from the load start): {:?}; established: {}",
        closes,
        relay
            .state
            .closes
            .lock()
            .unwrap()
            .iter()
            .map(|c| (*c as f64 - load_start_ms as f64) / 1000.0)
            .collect::<Vec<_>>(),
        relay.state.connects.lock().unwrap().len()
    );
    println!(
        "synchronization requests the relay would have sent: {}",
        relay.state.sync_requests.load(Ordering::Relaxed)
    );
    println!(
        "run took {:.0} s after the load",
        (end_ms - load_end_ms) as f64 / 1000.0
    );

    // The summary of the run, in the shape of the comparison table.
    let mut during_load: Vec<f64> = seen
        .iter()
        .filter(|k| k.received_ms >= load_start_ms && k.received_ms <= load_end_ms)
        .map(|k| k.received_ms.saturating_sub(k.sent_secs * 1000) as f64 / 1000.0)
        .collect();
    during_load.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = during_load.get(during_load.len() / 2).copied();
    let max_latency = during_load.last().copied();
    let rss_mib = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|kb| kb.parse::<f64>().ok())
        })
        .map(|kb| kb / 1024.0);
    println!(
        "\nSUMMARY candidates C1={} C1'={} C2={} C4={} C5={} C6={} relay={} msg/s load={} s",
        params.c1, params.c1p, params.c2, params.c4, params.c5, params.c6, params.relay_msgs_per_s, params.load_secs
    );
    // Times relative to the load start, in seconds.
    let rel = |ms: Option<u64>| ms.map(|ms| (ms as f64 - load_start_ms as f64) / 1000.0);
    let close_times = mm2_p2p::lab_knobs::relay_close_times();
    let first_close_ms = close_times.first().copied();
    let first_nonempty = mm2_p2p::lab_knobs::first_nonempty_ms();
    let c6_fired = mm2_p2p::lab_knobs::c6_first_raised_ms();
    println!(
        "SUMMARY queue first non-empty at {:?} s, C6 raised at {:?} s, first close at {:?} s, first degraded at {:?} s (from the load start)",
        rel(first_nonempty),
        rel(c6_fired),
        rel(first_close_ms),
        rel(first_degraded_ms)
    );
    println!(
        "SUMMARY degraded before the first close: {}; C6 before the first close: {}; queue non-empty before the first close: {}",
        match (first_degraded_ms, first_close_ms) {
            (Some(d), Some(c)) => d < c,
            (Some(_), None) => true,
            _ => false,
        },
        match (c6_fired, first_close_ms) {
            (Some(f), Some(c)) => f < c,
            (Some(_), None) => true,
            _ => false,
        },
        match (first_nonempty, first_close_ms) {
            (Some(f), Some(c)) => f < c,
            (Some(_), None) => true,
            _ => false,
        }
    );
    let first_signal = [c6_fired, first_close_ms, first_degraded_ms]
        .into_iter()
        .flatten()
        .min();
    println!(
        "SUMMARY residual: first signal (C6, a close or degraded) {:?} s after the load start",
        rel(first_signal)
    );
    // Keep-alives before and after the first close (relay side), within the load.
    // The first close is the one the maker saw. The relay only notices it after it has read what was queued ahead
    // of the close, which can be much later.
    let split_ms = first_close_ms.unwrap_or(load_end_ms).clamp(load_start_ms, load_end_ms);
    for (name, from, to) in [
        ("before first close", load_start_ms, split_ms),
        ("after first close", split_ms, load_end_ms),
    ] {
        let mut lat: Vec<f64> = seen
            .iter()
            .filter(|k| k.received_ms >= from && k.received_ms < to)
            .map(|k| k.received_ms.saturating_sub(k.sent_secs * 1000) as f64 / 1000.0)
            .collect();
        let mut at: Vec<f64> = seen
            .iter()
            .filter(|k| k.received_ms >= from && k.received_ms < to)
            .map(|k| k.received_ms as f64 / 1000.0)
            .collect();
        lat.sort_by(|a, b| a.partial_cmp(b).unwrap());
        at.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let mut pts = vec![from as f64 / 1000.0];
        pts.extend(at);
        pts.push(to as f64 / 1000.0);
        let gap = pts.windows(2).map(|w| w[1] - w[0]).fold(0.0, f64::max);
        println!(
            "SUMMARY {}: {} s window, keep-alives {}, latency median {:?} s, max {:?} s, longest gap {:.1} s",
            name,
            (to - from) / 1000,
            lat.len(),
            lat.get(lat.len() / 2),
            lat.last(),
            gap
        );
    }
    println!(
        "SUMMARY keep-alives received during the load: {}, latency median {:?} s, max {:?} s",
        during_load.len(),
        median,
        max_latency
    );
    println!("SUMMARY longest gap: {:.1} s", longest_gap_s);
    println!(
        "SUMMARY closes at (s): {:?}",
        relay
            .state
            .closes
            .lock()
            .unwrap()
            .iter()
            .map(|c| ((*c as f64 - load_start_ms as f64) / 1000.0).round())
            .collect::<Vec<_>>()
    );
    println!("SUMMARY rate trace (s, msg/s): {:?}", rate_trace);
    println!("SUMMARY peak handler queue: {}", peak_queue);
    println!("SUMMARY drops: {}", gauge(&ctx, "p2p.send_queue.dropped.count"));
    println!(
        "SUMMARY convergence: {:?} s after the load, sync requests {}",
        converged_after,
        relay.state.sync_requests.load(Ordering::Relaxed)
    );
    println!(
        "SUMMARY hidden backlog when the queue first held something (released, read by relay): {:?}",
        hidden_backlog
    );
    println!(
        "SUMMARY ping results (ms since the first ping, round trip ms or fail): {:?}",
        mm2_p2p::lab_knobs::ping_log()
    );
    println!(
        "SUMMARY RSS at the end: {:?} MiB (debug build, maker and relay in one process)",
        rss_mib
    );
    abort.abort();
    Outcome {
        keep_alives,
        longest_gap_s,
        converged_after_s: converged_after,
        closes,
    }
}

fn check(outcome: &Outcome, expect_all_within: f64, max_gap: f64) {
    let late: Vec<_> = outcome
        .keep_alives
        .iter()
        .filter(|(_, _, latency)| latency.is_none_or(|l| l > expect_all_within))
        .collect();
    assert!(
        late.is_empty(),
        "keep-alives not delivered within {expect_all_within} s: {late:?}"
    );
    assert!(
        outcome.longest_gap_s <= max_gap,
        "a gap of {:.1} s between delivered keep-alives",
        outcome.longest_gap_s
    );
    assert!(
        outcome.converged_after_s.is_some(),
        "the relay's order set did not converge"
    );
}

/// The harness of T-MP8 at a small scale, with a relay that keeps up with the ceiling of 10 messages per second
/// (it reads 20 per second): 60 orders on two pairs, 30 updates per second for 12 s, a keep-alive every 4 s.
/// This checks the pipeline: the merging, the release at the ceiling, the flush before each keep-alive, the
/// keep-alive arriving promptly and the convergence. It is not the slow-relay scenario of T-MP8, which is
/// `t_mp8_full`.
#[test]
fn t_mp8_harness_with_a_relay_that_keeps_up() {
    let outcome = run(Params {
        orders: 60,
        pairs: 2,
        updates_per_s: 30.0,
        relay_msgs_per_s: 20.0,
        load_secs: 12,
        keep_alive_secs: 4,
        converge_secs: 60,
        relay_syncs: true,
        coalescing: true,
        c1: false,
        c2: false,
        c4: false,
        c1p: false,
        c5: 0,
        c6: false,
        row_secs: 4,
    });
    check(&outcome, 30.0, 90.0);
    assert_eq!(outcome.closes, 0, "the relay connection closed");
}

/// T-MP8: 600 orders, 90 updates per second, a relay that reads about one message per second, for at least 300 s.
/// Every keep-alive reaches the relay within 30 s of its tick, no gap between delivered keep-alives exceeds 90 s,
/// and after the load stops the relay's order set converges to the maker's.
#[test]
#[ignore = "heavy: about 10 to 20 minutes; run explicitly"]
fn t_mp8_full() {
    let outcome = run(Params {
        orders: env_or("MP8_ORDERS", 600),
        pairs: env_or("MP8_PAIRS", 3),
        updates_per_s: env_or("MP8_UPDATES_PER_S", 90.0),
        relay_msgs_per_s: env_or("MP8_RELAY_MSGS_PER_S", 1.0),
        load_secs: env_or("MP8_LOAD_SECS", 300),
        keep_alive_secs: env_or("MP8_KEEP_ALIVE_SECS", 30),
        converge_secs: env_or("MP8_CONVERGE_SECS", 900),
        relay_syncs: env_or("MP8_RELAY_SYNCS", true),
        coalescing: env_or("MP8_COALESCING", true),
        c1: env_or("MP8_C1", false),
        c2: env_or("MP8_C2", false),
        c4: env_or("MP8_C4", false),
        c1p: env_or("MP8_C1P", false),
        c5: match std::env::var("MP8_C5").as_deref() {
            Ok("a") => 1,
            Ok("b") => 2,
            _ => 0,
        },
        c6: env_or("MP8_C6", false),
        row_secs: 15,
    });
    check(&outcome, 30.0, 90.0);
}
