//! Measurement harness: what does a relay hold in memory for a connected gossip peer that stops
//! consuming what it is sent?
//!
//! This is a separate integration-test binary because it installs a counting `#[global_allocator]`,
//! which applies to the whole binary. The heavy runs are `#[ignore]`; run them explicitly, one at a
//! time or with `--test-threads=1` (the tests also serialize on a process-wide lock because the
//! allocator counters are process-wide):
//!
//! ```text
//! cargo test -p mm2_p2p --test gossip_send_queue --release -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Sender `S` is, where possible, the production node (`spawn_gossipsub` with
//! `NodeType::RelayInMemory`). Receivers are plain test swarms built with the same transport upgrade
//! stack (multistream V1, Noise, yamux default) and the same gossipsub configuration as production
//! (`upgrade_transport` and `gossipsub_message_id` are private to the crate, so their few lines are
//! repeated below). A receiver is paused by simply no longer polling its swarm, which is built
//! without an executor, so nothing at all is read from the connection while paused.

use futures::channel::{mpsc, oneshot};
use futures::future::{select, Either};
use futures::lock::Mutex as AsyncMutex;
use futures::{pin_mut, Future, SinkExt, StreamExt};
use libp2p::core::transport::{Boxed as BoxedTransport, MemoryTransport};
use libp2p::core::{Endpoint, Multiaddr, Transport};
use libp2p::gossipsub::{Behaviour as Gossipsub, ConfigBuilder, Event as GossipsubEvent, IdentTopic, Message,
                        MessageAcceptance, MessageAuthenticity, MessageId, ValidationMode};
use libp2p::swarm::{ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, PollParameters, Swarm, SwarmBuilder,
                    SwarmEvent, THandler, THandlerInEvent, THandlerOutEvent, ToSwarm};
use libp2p::{identity, noise, PeerId};
use mm2_p2p::atomicdex_behaviour::AdexBehaviourCmd;
use mm2_p2p::{spawn_gossipsub, NodeType};
use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------------------------
// Counting allocator
// ---------------------------------------------------------------------------------------------

struct CountingAlloc;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn add(n: usize) {
    let now = CURRENT.fetch_add(n, Ordering::Relaxed) + n;
    PEAK.fetch_max(now, Ordering::Relaxed);
}

fn sub(n: usize) { CURRENT.fetch_sub(n, Ordering::Relaxed); }

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = System.alloc(layout);
        if !p.is_null() {
            add(layout.size());
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = System.alloc_zeroed(layout);
        if !p.is_null() {
            add(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout);
        sub(layout.size());
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = System.realloc(ptr, layout, new_size);
        if !p.is_null() {
            if new_size >= layout.size() {
                add(new_size - layout.size());
            } else {
                sub(layout.size() - new_size);
            }
        }
        p
    }
}

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

fn heap() -> usize { CURRENT.load(Ordering::Relaxed) }

fn peak() -> usize { PEAK.load(Ordering::Relaxed) }

fn reset_peak() { PEAK.store(heap(), Ordering::Relaxed); }

/// Resident set size (`VmRSS` of `/proc/self/status`) in MiB.
fn rss_mib() -> f64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let kb: f64 = status
        .lines()
        .find(|l| l.starts_with("VmRSS:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0);
    kb / 1024.0
}

extern "C" {
    fn malloc_trim(pad: usize) -> i32;
}

/// Ask glibc to return free memory to the OS (informational: shows whether RSS is free-but-cached).
fn trim() { unsafe { malloc_trim(0) }; }

fn mib(bytes: i64) -> f64 { bytes as f64 / (1024.0 * 1024.0) }

/// Wait until the heap size stops moving (two consecutive 50 ms samples within 256 KiB), at most
/// `cap`. Returns the settled heap and the time waited.
async fn settle(cap: Duration) -> (usize, Duration) {
    let start = Instant::now();
    let mut last = heap();
    let mut stable = 0;
    while start.elapsed() < cap {
        async_std::task::sleep(Duration::from_millis(50)).await;
        let now = heap();
        if (now as i64 - last as i64).abs() < 256 * 1024 {
            stable += 1;
            if stable >= 2 {
                return (now, start.elapsed());
            }
        } else {
            stable = 0;
        }
        last = now;
    }
    (heap(), start.elapsed())
}

// ---------------------------------------------------------------------------------------------
// Production-equivalent transport and gossipsub configuration
// ---------------------------------------------------------------------------------------------

lazy_static::lazy_static! {
    /// The allocator counters are process-wide, so the tests of this binary must not overlap.
    static ref SERIAL: AsyncMutex<()> = AsyncMutex::new(());
}

static NEXT_PORT: AtomicU64 = AtomicU64::new(40_000);

fn next_port() -> u64 { NEXT_PORT.fetch_add(1, Ordering::Relaxed) }

/// Same pipeline as the crate's private `upgrade_transport`, over the in-memory transport.
fn memory_transport(keypair: &identity::Keypair) -> BoxedTransport<(PeerId, libp2p::core::muxing::StreamMuxerBox)> {
    MemoryTransport::default()
        .upgrade(libp2p::core::upgrade::Version::V1)
        .authenticate(noise::Config::new(keypair).expect("noise key"))
        .multiplex(libp2p::yamux::Config::default())
        .timeout(Duration::from_secs(20))
        .map(|(peer, muxer), _| (peer, libp2p::core::muxing::StreamMuxerBox::new(muxer)))
        .boxed()
}

/// Same as the crate's private `gossipsub_message_id`.
fn message_id(message: &Message) -> MessageId {
    let mut s = DefaultHasher::new();
    message.data.hash(&mut s);
    message.sequence_number.hash(&mut s);
    MessageId::from(s.finish().to_be_bytes().to_vec())
}

/// Same gossipsub settings as `start_gossipsub` for the given role, before `build()`.
fn gossip_builder(i_am_relay: bool) -> ConfigBuilder {
    let (mesh_n_low, mesh_n, mesh_n_high) = if i_am_relay { (4, 8, 12) } else { (2, 4, 6) };
    let mut builder = ConfigBuilder::default();
    builder
        .message_id_fn(message_id)
        .i_am_relay(i_am_relay)
        .mesh_n_low(mesh_n_low)
        .mesh_n(mesh_n)
        .mesh_n_high(mesh_n_high)
        .mesh_outbound_min(mesh_n_low / 2)
        .validate_messages()
        .validation_mode(ValidationMode::Permissive)
        .max_transmit_size(1024 * 1024 - 100);
    builder
}

fn gossip_config(i_am_relay: bool) -> libp2p::gossipsub::Config {
    gossip_builder(i_am_relay).build().expect("valid gossipsub config")
}

// ---------------------------------------------------------------------------------------------
// A gossipsub behaviour wrapper that counts the events it hands to connection handlers
// ---------------------------------------------------------------------------------------------

struct CountingGossipsub {
    inner: Gossipsub,
    notify_events: Arc<AtomicUsize>,
}

impl NetworkBehaviour for CountingGossipsub {
    type ConnectionHandler = THandler<Gossipsub>;
    type ToSwarm = GossipsubEvent;

    fn handle_established_inbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        local_addr: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        self.inner
            .handle_established_inbound_connection(connection_id, peer, local_addr, remote_addr)
    }

    fn handle_established_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        addr: &Multiaddr,
        role_override: Endpoint,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        self.inner
            .handle_established_outbound_connection(connection_id, peer, addr, role_override)
    }

    fn handle_pending_inbound_connection(
        &mut self,
        connection_id: ConnectionId,
        local_addr: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<(), ConnectionDenied> {
        self.inner
            .handle_pending_inbound_connection(connection_id, local_addr, remote_addr)
    }

    fn handle_pending_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        maybe_peer: Option<PeerId>,
        addresses: &[Multiaddr],
        effective_role: Endpoint,
    ) -> Result<Vec<Multiaddr>, ConnectionDenied> {
        self.inner
            .handle_pending_outbound_connection(connection_id, maybe_peer, addresses, effective_role)
    }

    fn on_swarm_event(&mut self, event: FromSwarm<Self::ConnectionHandler>) { self.inner.on_swarm_event(event) }

    fn on_connection_handler_event(
        &mut self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        self.inner.on_connection_handler_event(peer_id, connection_id, event)
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
        params: &mut impl PollParameters,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        let polled = self.inner.poll(cx, params);
        if let Poll::Ready(ToSwarm::NotifyHandler { .. }) = &polled {
            self.notify_events.fetch_add(1, Ordering::Relaxed);
        }
        polled
    }
}

// ---------------------------------------------------------------------------------------------
// Swarm driver with pause / resume / run-closure control
// ---------------------------------------------------------------------------------------------

type RunFn<B> = Box<dyn FnOnce(&mut Swarm<B>) + Send>;

enum Ctl<B: NetworkBehaviour> {
    /// Stop polling the swarm until `Resume`. The ack is sent once the driver has stopped.
    Pause(oneshot::Sender<()>),
    Resume,
    /// Drop the swarm (closes every connection) and end the driver.
    Stop,
    /// Run a closure on the swarm between two polls.
    Run(RunFn<B>, oneshot::Sender<()>),
}

enum Step<B: NetworkBehaviour> {
    Ctl(Option<Ctl<B>>),
    Event(B::ToSwarm),
    Other,
}

async fn drive<B, F>(
    mut swarm: Swarm<B>,
    mut ctl: mpsc::UnboundedReceiver<Ctl<B>>,
    delay_us: Arc<AtomicU64>,
    mut on_event: F,
) where
    B: NetworkBehaviour,
    F: FnMut(&mut Swarm<B>, B::ToSwarm),
{
    let mut paused = false;
    loop {
        let step: Step<B> = if paused {
            Step::Ctl(ctl.next().await)
        } else {
            let next_ctl = ctl.next();
            let next_event = swarm.select_next_some();
            pin_mut!(next_ctl, next_event);
            match select(next_ctl, next_event).await {
                Either::Left((c, _)) => Step::Ctl(c),
                Either::Right((SwarmEvent::Behaviour(e), _)) => Step::Event(e),
                Either::Right((_, _)) => Step::Other,
            }
        };
        match step {
            Step::Ctl(None) | Step::Ctl(Some(Ctl::Stop)) => return,
            Step::Ctl(Some(Ctl::Pause(ack))) => {
                paused = true;
                let _ = ack.send(());
            },
            Step::Ctl(Some(Ctl::Resume)) => paused = false,
            Step::Ctl(Some(Ctl::Run(f, ack))) => {
                f(&mut swarm);
                let _ = ack.send(());
            },
            Step::Event(e) => {
                on_event(&mut swarm, e);
                // Optional throttle: a slow reader.
                let d = delay_us.load(Ordering::Relaxed);
                if d > 0 {
                    async_std::task::sleep(Duration::from_micros(d)).await;
                }
            },
            Step::Other => {},
        }
    }
}

/// Handle to a driven swarm.
struct Driven<B: NetworkBehaviour> {
    ctl: mpsc::UnboundedSender<Ctl<B>>,
}

impl<B: NetworkBehaviour> Driven<B> {
    async fn pause(&self) {
        let (tx, rx) = oneshot::channel();
        self.ctl.unbounded_send(Ctl::Pause(tx)).expect("driver alive");
        rx.await.expect("pause ack");
    }

    fn resume(&self) { let _ = self.ctl.unbounded_send(Ctl::Resume); }

    fn stop(&self) { let _ = self.ctl.unbounded_send(Ctl::Stop); }

    async fn run(&self, f: impl FnOnce(&mut Swarm<B>) + Send + 'static) {
        let (tx, rx) = oneshot::channel();
        self.ctl
            .unbounded_send(Ctl::Run(Box::new(f), tx))
            .expect("driver alive");
        rx.await.expect("run ack");
    }
}

// ---------------------------------------------------------------------------------------------
// Receiver
// ---------------------------------------------------------------------------------------------

const PROBE: u32 = u32::MAX;

#[derive(Default)]
struct Recv {
    /// Non-probe messages delivered to the application.
    count: AtomicUsize,
    probes: AtomicUsize,
    /// Arrival order of message indexes.
    order: Mutex<Vec<u32>>,
}

fn msg_index(data: &[u8]) -> u32 {
    let mut b = [0u8; 4];
    b.copy_from_slice(&data[..4]);
    u32::from_le_bytes(b)
}

fn payload(idx: u32, size: usize) -> Vec<u8> {
    let mut v = vec![0xA5u8; size.max(4)];
    v[..4].copy_from_slice(&idx.to_le_bytes());
    v
}

/// Receiver behaviour: gossipsub plus the standard ping, so that a receiver which is polled answers
/// the relay's pings exactly as a real peer would, and one which is not polled does not.
#[derive(NetworkBehaviour)]
struct ReceiverBehaviour {
    gossipsub: Gossipsub,
    ping: libp2p::ping::Behaviour,
}

struct Receiver {
    peer_id: PeerId,
    drv: Driven<ReceiverBehaviour>,
    recv: Arc<Recv>,
    /// Sleep after every behaviour event, in microseconds (0 = full speed).
    delay_us: Arc<AtomicU64>,
}

fn spawn_receiver(topics: &[String], dial_port: u64) -> Receiver {
    spawn_receiver_with(topics, dial_port, gossip_config(false))
}

fn spawn_receiver_with(topics: &[String], dial_port: u64, config: libp2p::gossipsub::Config) -> Receiver {
    let keypair = identity::Keypair::generate_ed25519();
    let peer_id = PeerId::from(keypair.public());
    let mut gossipsub = Gossipsub::new(MessageAuthenticity::Author(peer_id), config).expect("gossipsub");
    for t in topics {
        gossipsub.subscribe(&IdentTopic::new(t.clone())).expect("subscribe");
    }
    let behaviour = ReceiverBehaviour {
        gossipsub,
        ping: libp2p::ping::Behaviour::new(libp2p::ping::Config::new()),
    };
    // No executor: connections are polled inside `Swarm::poll`, so a paused receiver reads nothing.
    let mut swarm = SwarmBuilder::without_executor(memory_transport(&keypair), behaviour, peer_id).build();
    let addr: Multiaddr = format!("/memory/{dial_port}").parse().unwrap();
    swarm.dial(addr).expect("dial");

    let recv = Arc::new(Recv::default());
    let recv_cb = recv.clone();
    let (ctl_tx, ctl_rx) = mpsc::unbounded();
    let delay_us = Arc::new(AtomicU64::new(0));
    tokio::spawn(drive(swarm, ctl_rx, delay_us.clone(), move |swarm, event| {
        if let ReceiverBehaviourEvent::Gossipsub(GossipsubEvent::Message {
            propagation_source,
            message_id,
            message,
        }) = event
        {
            let idx = msg_index(&message.data);
            if idx == PROBE {
                recv_cb.probes.fetch_add(1, Ordering::Relaxed);
            } else {
                recv_cb.count.fetch_add(1, Ordering::Relaxed);
                recv_cb.order.lock().unwrap().push(idx);
            }
            // `validate_messages` is on: report a verdict so the message is released from the cache.
            // `Ignore` does not forward it back.
            let _ = swarm.behaviour_mut().gossipsub.report_message_validation_result(
                &message_id,
                &propagation_source,
                MessageAcceptance::Ignore,
            );
        }
    }));
    Receiver {
        peer_id,
        drv: Driven { ctl: ctl_tx },
        recv,
        delay_us,
    }
}

// ---------------------------------------------------------------------------------------------
// Production sender
// ---------------------------------------------------------------------------------------------

fn spawn_fn(fut: Box<dyn Future<Output = ()> + Send + Unpin + 'static>) { tokio::spawn(fut); }

struct ProdSender {
    cmd_tx: mpsc::Sender<AdexBehaviourCmd>,
    abort: futures::future::AbortHandle,
    port: u64,
}

impl ProdSender {
    async fn start() -> ProdSender {
        let port = next_port();
        let (cmd_tx, mut event_rx, _peer_id, abort) =
            spawn_gossipsub(None, spawn_fn, vec![], NodeType::RelayInMemory { port }, |_| {})
                .await
                .expect("spawn_gossipsub");
        // Drain the application event channel so that it never fills.
        tokio::spawn(async move { while event_rx.next().await.is_some() {} });
        ProdSender { cmd_tx, abort, port }
    }

    async fn publish(&mut self, topics: &[String], msg: Vec<u8>) {
        self.cmd_tx
            .send(AdexBehaviourCmd::PublishMsg {
                topics: topics.to_vec(),
                msg,
            })
            .await
            .expect("cmd channel");
    }

    /// Number of connected peers; also acts as a barrier because commands are processed in order.
    async fn peers(&mut self) -> usize {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(AdexBehaviourCmd::GetPeersInfo { result_tx: tx })
            .await
            .expect("cmd channel");
        rx.await.expect("peers info").len()
    }

    fn shutdown(&self) { self.abort.abort(); }
}

/// Publish probe messages once a second until every receiver has seen one (mesh is formed).
async fn wait_ready(s: &mut ProdSender, topic: &str, receivers: &[&Receiver]) {
    let topics = [topic.to_owned()];
    for _ in 0..40 {
        s.publish(&topics, payload(PROBE, 64)).await;
        async_std::task::sleep(Duration::from_secs(1)).await;
        if receivers.iter().all(|r| r.recv.probes.load(Ordering::Relaxed) > 0) {
            return;
        }
    }
    panic!("receivers never saw a probe message");
}

// ---------------------------------------------------------------------------------------------
// Generic scenario runner
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum End {
    /// Resume the paused receivers and measure the drain.
    Resume,
    /// Drop the paused receivers (connection closes) and measure the release.
    DropPaused,
}

struct Scenario {
    label: String,
    payload_size: usize,
    /// Number of `PublishMsg` commands.
    cmds: usize,
    /// All receivers subscribe to these topics.
    topics: Vec<String>,
    /// Each command publishes to these topics (one `PublishMsg` with several topics).
    publish_topics: Vec<String>,
    receivers: usize,
    paused: usize,
    end: End,
    /// Table row every this many commands.
    block: usize,
}

fn topic_names(prefix: &str, k: usize) -> Vec<String> { (0..k).map(|i| format!("{prefix}-{i}")).collect() }

async fn run_scenario(sc: Scenario) {
    println!("\n### {}", sc.label);
    println!(
        "payload {} B, {} PublishMsg commands x {} topic(s), {} receiver(s) of which {} paused",
        sc.payload_size,
        sc.cmds,
        sc.publish_topics.len(),
        sc.receivers,
        sc.paused
    );
    let mut s = ProdSender::start().await;
    let receivers: Vec<Receiver> = (0..sc.receivers).map(|_| spawn_receiver(&sc.topics, s.port)).collect();
    let refs: Vec<&Receiver> = receivers.iter().collect();
    wait_ready(&mut s, &sc.topics[0], &refs).await;
    // Let the readiness probes leave the message cache (5 heartbeats) before taking the baseline.
    async_std::task::sleep(Duration::from_secs(8)).await;
    let (base, _) = settle(Duration::from_secs(5)).await;
    let base_rss = rss_mib();
    reset_peak();
    println!(
        "baseline heap {:.2} MiB, RSS {:.1} MiB (peers on S: {})",
        mib(base as i64),
        base_rss,
        s.peers().await
    );

    for r in receivers.iter().take(sc.paused) {
        r.drv.pause().await;
    }

    // Publish phase.
    println!("\n| msgs published | S-queued msgs | heap delta MiB | peak delta MiB | RSS MiB | publish+barrier ms/block | settle ms |");
    println!("|---:|---:|---:|---:|---:|---:|---:|");
    let per_cmd = sc.publish_topics.len();
    let mut block_start = Instant::now();
    let mut last_row = (0usize, 0f64);
    for c in 1..=sc.cmds {
        s.publish(&sc.publish_topics, payload((c - 1) as u32, sc.payload_size))
            .await;
        if c % sc.block == 0 || c == sc.cmds {
            s.peers().await; // barrier
            let publish_ms = block_start.elapsed().as_secs_f64() * 1000.0;
            let (h, waited) = settle(Duration::from_secs(5)).await;
            println!(
                "| {} | {} | {:.2} | {:.2} | {:.0} | {:.0} | {:.0} |",
                c * per_cmd,
                c * per_cmd,
                mib(h as i64 - base as i64),
                mib(peak() as i64 - base as i64),
                rss_mib(),
                publish_ms,
                waited.as_secs_f64() * 1000.0
            );
            last_row = (c * per_cmd, mib(h as i64 - base as i64));
            block_start = Instant::now();
        }
    }
    let expected = sc.cmds * per_cmd;
    let (msgs, delta) = last_row;
    if delta > 0.0 && msgs > 0 {
        let per_msg = delta * 1024.0 * 1024.0 / (msgs * sc.paused.max(1)) as f64;
        println!(
            "\nheap per queued message per paused peer at the end: {:.0} B (payload {} B, ratio {:.2})",
            per_msg,
            sc.payload_size,
            per_msg / sc.payload_size as f64
        );
    }

    // Is the memory moving while the peer stays paused?
    async_std::task::sleep(Duration::from_secs(5)).await;
    let (h5, _) = settle(Duration::from_secs(3)).await;
    println!(
        "5 s later, receivers still paused: heap delta {:.2} MiB, peers on S {}",
        mib(h5 as i64 - base as i64),
        s.peers().await
    );

    // Healthy receivers: everything delivered?
    for (i, r) in receivers.iter().enumerate().skip(sc.paused) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while r.recv.count.load(Ordering::Relaxed) < expected && Instant::now() < deadline {
            async_std::task::sleep(Duration::from_millis(100)).await;
        }
        println!(
            "healthy receiver {}: delivered {}/{} (lost {})",
            i,
            r.recv.count.load(Ordering::Relaxed),
            expected,
            expected.saturating_sub(r.recv.count.load(Ordering::Relaxed))
        );
    }
    for (i, r) in receivers.iter().enumerate().take(sc.paused) {
        println!(
            "paused receiver {} before end: delivered {}/{}",
            i,
            r.recv.count.load(Ordering::Relaxed),
            expected
        );
    }

    // End of the scenario.
    if sc.paused > 0 {
        reset_peak();
        let t0 = Instant::now();
        match sc.end {
            End::Resume => {
                for r in receivers.iter().take(sc.paused) {
                    r.drv.resume();
                }
                let deadline = Instant::now() + Duration::from_secs(300);
                let mut first_at = None;
                println!("\ndrain curve (every 500 ms from resume)");
                println!("| t s | delivered (paused rx 0) | heap delta MiB | RSS MiB | RSS delta MiB |");
                println!("|---:|---:|---:|---:|---:|");
                let mut next_row = Duration::ZERO;
                let mut drained_at = None;
                loop {
                    if t0.elapsed() >= next_row {
                        curve_row(t0, base, base_rss, receivers[0].recv.count.load(Ordering::Relaxed));
                        next_row += Duration::from_millis(500);
                    }
                    let done = receivers
                        .iter()
                        .take(sc.paused)
                        .all(|r| r.recv.count.load(Ordering::Relaxed) >= expected);
                    if first_at.is_none() && receivers[0].recv.count.load(Ordering::Relaxed) > 0 {
                        first_at = Some(t0.elapsed());
                    }
                    if done && drained_at.is_none() {
                        drained_at = Some(t0.elapsed());
                    }
                    // 10 s after the queue is empty, then stop (long tail follows below).
                    if let Some(d) = drained_at {
                        if t0.elapsed() > d + Duration::from_secs(10) {
                            break;
                        }
                    }
                    if Instant::now() > deadline {
                        break;
                    }
                    async_std::task::sleep(Duration::from_millis(20)).await;
                }
                println!("(delivery complete after {:?})", drained_at);
                let tail = env_usize("GSQ_TAIL_SECS", 60) as u64;
                if tail > 0 {
                    println!("\nlong tail, every 10 s, receivers idle");
                    println!("| t s | delivered | heap delta MiB | RSS MiB | RSS delta MiB |");
                    println!("|---:|---:|---:|---:|---:|");
                    let tail_start = Instant::now();
                    while tail_start.elapsed() < Duration::from_secs(tail) {
                        async_std::task::sleep(Duration::from_secs(10)).await;
                        curve_row(t0, base, base_rss, receivers[0].recv.count.load(Ordering::Relaxed));
                    }
                }
                let drain = drained_at.unwrap_or_else(|| t0.elapsed());
                let r0 = &receivers[0];
                let got = r0.recv.count.load(Ordering::Relaxed);
                let order = r0.recv.order.lock().unwrap().clone();
                let desc = order.windows(2).filter(|w| w[1] < w[0]).count();
                println!(
                    "\nafter resume: paused receiver 0 delivered {}/{} (lost {}), first message after {:?}, \
                     all paused receivers done in {:.2?}, S peak during drain {:.2} MiB over baseline",
                    got,
                    expected,
                    expected.saturating_sub(got),
                    first_at,
                    drain,
                    mib(peak() as i64 - base as i64)
                );
                println!(
                    "arrival order: first 6 = {:?}, last 6 = {:?}, descending adjacent pairs {}/{}",
                    &order[..order.len().min(6)],
                    &order[order.len().saturating_sub(6)..],
                    desc,
                    order.len().saturating_sub(1)
                );
                println!("peers on S after drain: {}", s.peers().await);
                let all: Vec<&Receiver> = receivers.iter().collect();
                final_report(base, base_rss, &all).await;
            },
            End::DropPaused => {
                for r in receivers.iter().take(sc.paused) {
                    r.drv.stop();
                }
                for _ in 0..40 {
                    async_std::task::sleep(Duration::from_millis(250)).await;
                    if s.peers().await <= sc.receivers - sc.paused {
                        break;
                    }
                }
                println!(
                    "\nafter dropping the paused receivers: S saw the disconnect after {:.2?}, peers on S {}",
                    t0.elapsed(),
                    s.peers().await
                );
            },
        }
        let (h_end, _) = settle(Duration::from_secs(5)).await;
        println!(
            "heap delta right after the end phase: {:.2} MiB",
            mib(h_end as i64 - base as i64)
        );
        // Message cache and duplicate cache age out.
        async_std::task::sleep(Duration::from_secs(8)).await;
        let (h_late, _) = settle(Duration::from_secs(5)).await;
        println!(
            "heap delta 8 s later (caches aged): {:.2} MiB, RSS {:.1} MiB (before the run {:.1} MiB)",
            mib(h_late as i64 - base as i64),
            rss_mib(),
            base_rss
        );
    }

    for r in &receivers {
        r.drv.stop();
    }
    s.shutdown();
    async_std::task::sleep(Duration::from_secs(1)).await;
}

/// After the drain: what does the process still look like, can glibc give memory back, and how much
/// of what remains belongs to the receivers (test artifact) rather than the sender.
async fn final_report(base: usize, base_rss: f64, receivers: &[&Receiver]) {
    let (h, _) = settle(Duration::from_secs(3)).await;
    println!(
        "\nend state, receivers still alive: heap delta {:.2} MiB, RSS {:.1} MiB ({:+.1} vs before the run)",
        mib(h as i64 - base as i64),
        rss_mib(),
        rss_mib() - base_rss
    );
    trim();
    async_std::task::sleep(Duration::from_millis(500)).await;
    println!(
        "after malloc_trim(0): RSS {:.1} MiB ({:+.1} vs before the run)",
        rss_mib(),
        rss_mib() - base_rss
    );
    for r in receivers {
        r.drv.stop();
    }
    async_std::task::sleep(Duration::from_secs(3)).await;
    let (h2, _) = settle(Duration::from_secs(3)).await;
    println!(
        "after dropping the receivers (sender state only): heap delta {:.2} MiB, RSS {:.1} MiB ({:+.1})",
        mib(h2 as i64 - base as i64),
        rss_mib(),
        rss_mib() - base_rss
    );
    trim();
    async_std::task::sleep(Duration::from_millis(500)).await;
    println!(
        "after malloc_trim(0) again: RSS {:.1} MiB ({:+.1} vs before the run)",
        rss_mib(),
        rss_mib() - base_rss
    );
}

fn curve_row(t0: Instant, base_heap: usize, base_rss: f64, delivered: usize) {
    let rss = rss_mib();
    println!(
        "| {:.1} | {} | {:.2} | {:.1} | {:+.1} |",
        t0.elapsed().as_secs_f64(),
        delivered,
        mib(heap() as i64 - base_heap as i64),
        rss,
        rss - base_rss
    );
}

/// Payload sizes to run; `GSQ_ONLY_SIZE` selects one (each size in its own process gives a clean RSS).
fn sizes() -> Vec<usize> {
    match std::env::var("GSQ_ONLY_SIZE").ok().and_then(|v| v.parse().ok()) {
        Some(one) => vec![one],
        None => vec![1024, 8 * 1024],
    }
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

/// Small, stable variant with the production node (default limits: 2,048 messages, 4 MiB): a paused
/// receiver makes the relay hold what it publishes while below the cap, and a resumed receiver gets
/// all of it, in the order it was published (T-RQ1.2).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paused_peer_queue_grows_and_drains() {
    let _guard = SERIAL.lock().await;
    let topics = topic_names("gsq-fast", 1);
    let mut s = ProdSender::start().await;
    let r = spawn_receiver(&topics, s.port);
    wait_ready(&mut s, &topics[0], &[&r]).await;
    async_std::task::sleep(Duration::from_secs(2)).await;
    let (base, _) = settle(Duration::from_secs(5)).await;

    r.drv.pause().await;
    const N: usize = 1500;
    const SIZE: usize = 1024;
    for i in 0..N {
        s.publish(&topics, payload(i as u32, SIZE)).await;
    }
    s.peers().await;
    let (held, _) = settle(Duration::from_secs(5)).await;
    let delta = held as i64 - base as i64;
    println!("paused: {N} x {SIZE} B published, heap delta {:.2} MiB", mib(delta));
    // Far more than the yamux window (256 KiB) plus transport buffers can explain.
    assert!(
        delta > (N * SIZE) as i64 / 2,
        "relay did not hold the unread messages: {delta} bytes"
    );
    assert_eq!(r.recv.count.load(Ordering::Relaxed), 0, "paused receiver read messages");

    r.drv.resume();
    let deadline = Instant::now() + Duration::from_secs(60);
    while r.recv.count.load(Ordering::Relaxed) < N && Instant::now() < deadline {
        async_std::task::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(r.recv.count.load(Ordering::Relaxed), N, "messages lost after resume");
    let order = r.recv.order.lock().unwrap().clone();
    assert_eq!(
        order,
        (0..N as u32).collect::<Vec<_>>(),
        "backlog was not delivered in order"
    );
    r.drv.stop();
    s.shutdown();
}

// ---------------------------------------------------------------------------------------------
// Send queue bound (CRD ch.28 §28.9B RQ1), tests T-RQ1.1 to T-RQ1.4, T-RQ1.6, T-RQ1.7. T-RQ1.5 (control
// RPCs never dropped) is covered by the unit tests of the vendored crate
// ---------------------------------------------------------------------------------------------

/// Captures the warnings of the gossipsub crate, so that tests can check what was logged.
struct CaptureLogger;

static LOG_LINES: Mutex<Vec<String>> = Mutex::new(Vec::new());

impl log::Log for CaptureLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool { metadata.level() <= log::Level::Warn }

    fn log(&self, record: &log::Record) {
        if record.target().starts_with("libp2p_gossipsub") && record.level() <= log::Level::Warn {
            LOG_LINES.lock().unwrap().push(record.args().to_string());
        }
    }

    fn flush(&self) {}
}

/// Installs the capture logger (once) and forgets earlier lines.
fn start_log_capture() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = log::set_logger(&CaptureLogger);
        log::set_max_level(log::LevelFilter::Warn);
    });
    LOG_LINES.lock().unwrap().clear();
}

/// Receiver with a short message cache, so that the memory it holds does not hide the sender's.
fn spawn_fast_receiver(topic: &str, dial_port: u64) -> Receiver {
    let mut builder = gossip_builder(false);
    builder
        .history_length(2)
        .history_gossip(1)
        .heartbeat_initial_delay(Duration::from_millis(100))
        .heartbeat_interval(Duration::from_millis(100));
    spawn_receiver_with(
        &[topic.to_owned()],
        dial_port,
        builder.build().expect("valid gossipsub config"),
    )
}

fn log_lines_with(needle: &str) -> Vec<String> {
    LOG_LINES
        .lock()
        .unwrap()
        .iter()
        .filter(|l| l.contains(needle))
        .cloned()
        .collect()
}

/// A plain gossipsub node that listens on the in-memory transport; unlike the production node its
/// send queue limits can be configured.
struct TestSender {
    port: u64,
    drv: Driven<Gossipsub>,
}

fn spawn_test_sender(limits: impl FnOnce(&mut ConfigBuilder)) -> TestSender {
    let keypair = identity::Keypair::generate_ed25519();
    let peer_id = PeerId::from(keypair.public());
    let mut builder = gossip_builder(true);
    // A short message cache and a fast heartbeat, so that memory measurements are not dominated by
    // the cache and the mesh forms quickly.
    builder
        .history_length(2)
        .history_gossip(1)
        .heartbeat_initial_delay(Duration::from_millis(100))
        .heartbeat_interval(Duration::from_millis(100));
    limits(&mut builder);
    let gossipsub = Gossipsub::new(
        MessageAuthenticity::Author(peer_id),
        builder.build().expect("valid gossipsub config"),
    )
    .expect("gossipsub");
    let mut swarm = SwarmBuilder::without_executor(memory_transport(&keypair), gossipsub, peer_id).build();
    let port = next_port();
    swarm
        .listen_on(format!("/memory/{port}").parse().unwrap())
        .expect("listen");
    let (ctl_tx, ctl_rx) = mpsc::unbounded();
    tokio::spawn(drive(swarm, ctl_rx, Arc::new(AtomicU64::new(0)), |_, _| {}));
    TestSender {
        port,
        drv: Driven { ctl: ctl_tx },
    }
}

impl TestSender {
    /// Publishes messages `range` (payload index and size as for the receivers) in one go.
    async fn publish_range(&self, topic: &str, range: std::ops::Range<u32>, size: usize) {
        let topic = IdentTopic::new(topic);
        self.drv
            .run(move |swarm| {
                for i in range {
                    let _ = swarm.behaviour_mut().publish(topic.clone(), payload(i, size));
                }
            })
            .await;
    }

    async fn is_connected(&self, peer: PeerId) -> bool {
        let out = Arc::new(AtomicUsize::new(0));
        let out_cb = out.clone();
        self.drv
            .run(move |swarm| out_cb.store(swarm.is_connected(&peer) as usize, Ordering::Relaxed))
            .await;
        out.load(Ordering::Relaxed) == 1
    }

    /// Publishes `range` in batches of `batch`, after each batch waiting until every `healthy`
    /// receiver has seen everything published so far. Keeps healthy peers' queues short.
    async fn publish_paced(
        &self,
        topic: &str,
        range: std::ops::Range<u32>,
        size: usize,
        batch: u32,
        healthy: &[&Receiver],
    ) {
        let mut next = range.start;
        while next < range.end {
            let end = (next + batch).min(range.end);
            self.publish_range(topic, next..end, size).await;
            next = end;
            let deadline = Instant::now() + Duration::from_secs(30);
            while healthy
                .iter()
                .any(|r| r.recv.count.load(Ordering::Relaxed) < next as usize)
            {
                assert!(Instant::now() < deadline, "a healthy receiver stalled at {next}");
                async_std::task::sleep(Duration::from_millis(1)).await;
            }
        }
    }

    fn stop(&self) { self.drv.stop(); }
}

/// Publishes probes until every receiver has seen one (subscriptions have been exchanged).
async fn wait_ready_test(s: &TestSender, topic: &str, receivers: &[&Receiver]) {
    for _ in 0..100 {
        let t = IdentTopic::new(topic);
        s.drv
            .run(move |swarm| {
                let _ = swarm.behaviour_mut().publish(t, payload(PROBE, 64));
            })
            .await;
        async_std::task::sleep(Duration::from_millis(200)).await;
        if receivers.iter().all(|r| r.recv.probes.load(Ordering::Relaxed) > 0) {
            return;
        }
    }
    panic!("receivers never saw a probe message");
}

async fn wait_for(what: &str, timeout: Duration, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        async_std::task::sleep(Duration::from_millis(20)).await;
    }
}

fn received(r: &Receiver) -> Vec<u32> { r.recv.order.lock().unwrap().clone() }

/// T-RQ1.1: a peer that keeps reading receives every message of a 1,000-message burst, in order, and
/// no drop is logged. Uses the production node with its default limits.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rq1_healthy_peer_unaffected() {
    let _guard = SERIAL.lock().await;
    start_log_capture();
    let topics = topic_names("gsq-rq1-1", 1);
    let mut s = ProdSender::start().await;
    let r = spawn_receiver(&topics, s.port);
    wait_ready(&mut s, &topics[0], &[&r]).await;
    const N: usize = 1000;
    for i in 0..N {
        s.publish(&topics, payload(i as u32, 256)).await;
    }
    wait_for("all messages delivered", Duration::from_secs(30), || {
        r.recv.count.load(Ordering::Relaxed) >= N
    })
    .await;
    assert_eq!(received(&r), (0..N as u32).collect::<Vec<_>>(), "not in order");
    assert!(
        log_lines_with("send queue").is_empty(),
        "unexpected send queue log lines: {:?}",
        log_lines_with("send queue")
    );
    r.drv.stop();
    s.shutdown();
}

/// What a paused peer got after it resumed.
struct PausedRun {
    paused_got: Vec<u32>,
    healthy_ok: bool,
    /// Sender heap growth while publishing the second part of the messages to the paused peer.
    late_growth: i64,
}

/// T-RQ1.2 to T-RQ1.4: a paused peer next to two healthy peers. The sender's memory stops growing
/// once the queue toward the paused peer is at its cap; after resume the paused peer gets the newest
/// messages and not the oldest; the healthy peers get every message.
async fn paused_peer_with_small_limits(
    label: &str,
    size: usize,
    max_messages: usize,
    max_bytes: usize,
    total: u32,
) -> PausedRun {
    let topic = "gsq-rq1-paused";
    let s = spawn_test_sender(|b| {
        b.send_queue_max_messages(max_messages)
            .send_queue_max_bytes(max_bytes)
            .send_queue_stuck_timeout(Duration::from_secs(600));
    });
    let paused = spawn_fast_receiver(topic, s.port);
    let h1 = spawn_fast_receiver(topic, s.port);
    let h2 = spawn_fast_receiver(topic, s.port);
    wait_ready_test(&s, topic, &[&paused, &h1, &h2]).await;
    async_std::task::sleep(Duration::from_millis(800)).await;
    paused.drv.pause().await;
    let (base, _) = settle(Duration::from_secs(5)).await;

    let third = total / 3;
    s.publish_paced(topic, 0..third, size, 4, &[&h1, &h2]).await;
    let (mid, _) = settle(Duration::from_secs(5)).await;
    s.publish_paced(topic, third..total, size, 4, &[&h1, &h2]).await;
    let (end, _) = settle(Duration::from_secs(5)).await;
    let late_growth = end as i64 - mid as i64;
    println!(
        "{label}: heap delta after first third {:.2} MiB, growth during the remaining two thirds {:.2} MiB",
        mib(mid as i64 - base as i64),
        mib(late_growth)
    );
    assert_eq!(
        paused.recv.count.load(Ordering::Relaxed),
        0,
        "paused receiver read messages"
    );

    wait_for("healthy peers got every message", Duration::from_secs(30), || {
        h1.recv.count.load(Ordering::Relaxed) >= total as usize
            && h2.recv.count.load(Ordering::Relaxed) >= total as usize
    })
    .await;
    let want: Vec<u32> = (0..total).collect();
    let healthy_ok = received(&h1) == want && received(&h2) == want;

    paused.drv.resume();
    wait_for("paused peer got the newest message", Duration::from_secs(30), || {
        received(&paused).last() == Some(&(total - 1))
    })
    .await;
    // Nothing more is coming; give stragglers a moment.
    async_std::task::sleep(Duration::from_millis(500)).await;
    let paused_got = received(&paused);
    for r in [&paused, &h1, &h2] {
        r.drv.stop();
    }
    s.stop();
    PausedRun {
        paused_got,
        healthy_ok,
        late_growth,
    }
}

fn assert_newest_kept_oldest_dropped(run: &PausedRun, tail: u32, total: u32) {
    assert!(run.healthy_ok, "a healthy peer missed or reordered messages");
    let got = &run.paused_got;
    assert!(got.windows(2).all(|w| w[0] < w[1]), "backlog not delivered in order");
    let tail_missing: Vec<u32> = (total - tail..total).filter(|i| !got.contains(i)).collect();
    assert!(tail_missing.is_empty(), "newest messages missing: {tail_missing:?}");
    assert!(
        !got.contains(&(total / 2)),
        "message from the middle of the backlog was delivered"
    );
    assert!(
        got.len() <= total as usize / 2,
        "paused peer got {} of {total}: nothing was dropped",
        got.len()
    );
    println!(
        "paused peer got {} of {total}, first {:?}",
        got.len(),
        &got[..got.len().min(5)]
    );
}

/// T-RQ1.2 and T-RQ1.3: the count cap (64 messages) with small payloads.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rq1_count_cap_drops_oldest() {
    let _guard = SERIAL.lock().await;
    start_log_capture();
    let run = paused_peer_with_small_limits("count cap", 1024, 64, 64 * 1024 * 1024, 3000).await;
    // Small payloads: the heap here is dominated by per-message state of the gossipsub nodes
    // themselves, so memory is checked by the byte cap test below, not here.
    assert_newest_kept_oldest_dropped(&run, 64, 3000);
    // Without the cap, everything beyond the transport window (about 256 KiB) would be queued.
    assert!(run.paused_got.len() < 1000, "got {} messages", run.paused_got.len());
}

/// T-RQ1.3 (memory) and T-RQ1.4: the byte cap (256 KiB) with 32 KiB payloads acts first.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rq1_byte_cap_drops_oldest() {
    let _guard = SERIAL.lock().await;
    start_log_capture();
    let run = paused_peer_with_small_limits("byte cap", 32 * 1024, 100_000, 256 * 1024, 300).await;
    // Unbounded, the second two thirds would add about 6.4 MiB of queued payload.
    assert!(
        run.late_growth < 2 * 1024 * 1024,
        "sender memory kept growing: {} bytes",
        run.late_growth
    );
    assert_newest_kept_oldest_dropped(&run, 6, 300);
}

/// T-RQ1.6 and T-RQ1.7: a peer at the cap for the full (shortened) period is disconnected; a peer
/// that drains before the period ends is not. The drop warning appears once per peer, and closing
/// logs one line.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rq1_stuck_peer_is_closed_and_draining_peer_is_not() {
    let _guard = SERIAL.lock().await;
    start_log_capture();
    let topic = "gsq-rq1-stuck";
    const PERIOD: Duration = Duration::from_secs(6);
    let s = spawn_test_sender(|b| {
        b.send_queue_max_messages(32).send_queue_stuck_timeout(PERIOD);
    });
    let stuck = spawn_fast_receiver(topic, s.port);
    let drains = spawn_fast_receiver(topic, s.port);
    let healthy = spawn_fast_receiver(topic, s.port);
    wait_ready_test(&s, topic, &[&stuck, &drains, &healthy]).await;
    stuck.drv.pause().await;
    drains.drv.pause().await;

    let t0 = Instant::now();
    s.publish_paced(topic, 0..1200, 1024, 8, &[&healthy]).await;
    assert!(
        t0.elapsed() < PERIOD / 2,
        "publishing took too long for the test to be meaningful: {:?}",
        t0.elapsed()
    );
    // The draining peer resumes well before the period ends and catches up.
    drains.drv.resume();
    wait_for("draining peer got the newest message", Duration::from_secs(20), || {
        received(&drains).last() == Some(&1199)
    })
    .await;

    // The paused peer is closed once the period has passed since its first drop.
    let deadline = Instant::now() + PERIOD + Duration::from_secs(10);
    while s.is_connected(stuck.peer_id).await {
        assert!(Instant::now() < deadline, "the stuck peer was never disconnected");
        async_std::task::sleep(Duration::from_millis(100)).await;
    }
    assert!(t0.elapsed() >= PERIOD, "closed before the period: {:?}", t0.elapsed());
    async_std::task::sleep(Duration::from_secs(1)).await;
    assert!(s.is_connected(drains.peer_id).await, "the draining peer was closed");
    assert!(s.is_connected(healthy.peer_id).await, "the healthy peer was closed");

    let closing = log_lines_with("Closing the connection");
    assert_eq!(closing.len(), 1, "close lines: {closing:?}");
    assert!(closing[0].contains(&stuck.peer_id.to_string()));
    for (name, r) in [("stuck", &stuck), ("draining", &drains)] {
        let first = log_lines_with(&format!("toward peer {} is full", r.peer_id));
        assert_eq!(first.len(), 1, "{name} peer first-drop lines: {first:?}");
        let summaries = log_lines_with(&format!("toward peer {} is still full", r.peer_id));
        assert!(summaries.len() <= 1, "{name} peer summary lines: {summaries:?}");
    }
    assert!(
        log_lines_with(&healthy.peer_id.to_string()).is_empty(),
        "healthy peer was logged"
    );
    for r in [&stuck, &drains, &healthy] {
        r.drv.stop();
    }
    s.stop();
}

/// Scenario A: healthy baseline, receiver polled throughout.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "heavy measurement; run explicitly"]
async fn scenario_a_healthy() {
    let _guard = SERIAL.lock().await;
    let m = env_usize("GSQ_M", 20_000);
    for size in sizes() {
        run_scenario(Scenario {
            label: format!("A healthy receiver, {size} B"),
            payload_size: size,
            cmds: m,
            topics: topic_names("gsq-a", 1),
            publish_topics: topic_names("gsq-a", 1),
            receivers: 1,
            paused: 0,
            end: End::Resume,
            block: 1000,
        })
        .await;
    }
}

/// Scenario B: one paused receiver, then resume (B1) or drop (B2).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "heavy measurement; run explicitly"]
async fn scenario_b_paused() {
    let _guard = SERIAL.lock().await;
    let m = env_usize("GSQ_M", 20_000);
    for size in sizes() {
        run_scenario(Scenario {
            label: format!("B1 paused receiver then resume, {size} B"),
            payload_size: size,
            cmds: m,
            topics: topic_names("gsq-b", 1),
            publish_topics: topic_names("gsq-b", 1),
            receivers: 1,
            paused: 1,
            end: End::Resume,
            block: 1000,
        })
        .await;
    }
    run_scenario(Scenario {
        label: "B2 paused receiver then dropped, 1024 B".to_owned(),
        payload_size: 1024,
        cmds: m,
        topics: topic_names("gsq-b", 1),
        publish_topics: topic_names("gsq-b", 1),
        receivers: 1,
        paused: 1,
        end: End::DropPaused,
        block: 1000,
    })
    .await;
}

/// Scenario C: three receivers, one (C1) or two (C2) paused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "heavy measurement; run explicitly"]
async fn scenario_c_fanout() {
    let _guard = SERIAL.lock().await;
    let m = env_usize("GSQ_M", 20_000).min(10_000);
    let size = env_usize("GSQ_C_SIZE", 1024);
    for paused in [1, 2] {
        run_scenario(Scenario {
            label: format!("C{paused} fan-out: 3 receivers, {paused} paused, {size} B"),
            payload_size: size,
            cmds: m,
            topics: topic_names("gsq-c", 1),
            publish_topics: topic_names("gsq-c", 1),
            receivers: 3,
            paused,
            end: End::Resume,
            block: 1000,
        })
        .await;
    }
}

/// Scenario D: one `PublishMsg` over four topics, paused receiver subscribed to all four; compared
/// with the same number of single-topic messages.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "heavy measurement; run explicitly"]
async fn scenario_d_topics() {
    let _guard = SERIAL.lock().await;
    let cmds = env_usize("GSQ_D_CMDS", 2_500);
    run_scenario(Scenario {
        label: "D1 one PublishMsg over K=4 topics, paused receiver subscribed to all, 1024 B".to_owned(),
        payload_size: 1024,
        cmds,
        topics: topic_names("gsq-d", 4),
        publish_topics: topic_names("gsq-d", 4),
        receivers: 1,
        paused: 1,
        end: End::Resume,
        block: 250,
    })
    .await;
    run_scenario(Scenario {
        label: "D2 reference: 4x as many single-topic PublishMsg, paused receiver, 1024 B".to_owned(),
        payload_size: 1024,
        cmds: cmds * 4,
        topics: topic_names("gsq-d", 1),
        publish_topics: topic_names("gsq-d", 1),
        receivers: 1,
        paused: 1,
        end: End::Resume,
        block: 1000,
    })
    .await;
}

/// Scenario E: attribution. A hand-built relay `S` (same gossipsub configuration, executor on the
/// test runtime) is driven in stages so the behaviour event queue and the connection handler queue
/// can be told apart: publish without polling the swarm, then let it poll, then drop the receiver.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "heavy measurement; run explicitly"]
async fn scenario_e_attribution() {
    let _guard = SERIAL.lock().await;
    let n = env_usize("GSQ_E_MSGS", 4_000);
    let size = env_usize("GSQ_E_SIZE", 1024);
    let topic = "gsq-e".to_owned();
    println!("\n### E attribution (hand-built relay S), {n} x {size} B, one paused receiver");

    let keypair = identity::Keypair::generate_ed25519();
    let peer_id = PeerId::from(keypair.public());
    let notify_events = Arc::new(AtomicUsize::new(0));
    let behaviour = CountingGossipsub {
        inner: Gossipsub::new(MessageAuthenticity::Author(peer_id), gossip_config(true)).expect("gossipsub"),
        notify_events: notify_events.clone(),
    };
    let mut swarm = SwarmBuilder::with_tokio_executor(memory_transport(&keypair), behaviour, peer_id).build();
    let port = next_port();
    swarm.listen_on(format!("/memory/{port}").parse().unwrap()).unwrap();
    let (ctl_tx, ctl_rx) = mpsc::unbounded();
    tokio::spawn(drive(swarm, ctl_rx, Arc::new(AtomicU64::new(0)), |_, _| {}));
    let s: Driven<CountingGossipsub> = Driven { ctl: ctl_tx };

    let r = spawn_receiver(std::slice::from_ref(&topic), port);
    // Readiness probes.
    let mut ready = false;
    for _ in 0..40 {
        let t = topic.clone();
        s.run(move |sw| {
            let _ = sw.behaviour_mut().inner.publish(IdentTopic::new(t), payload(PROBE, 64));
        })
        .await;
        async_std::task::sleep(Duration::from_secs(1)).await;
        if r.recv.probes.load(Ordering::Relaxed) > 0 {
            ready = true;
            break;
        }
    }
    assert!(ready, "receiver never saw a probe");
    async_std::task::sleep(Duration::from_secs(8)).await;
    let (base, _) = settle(Duration::from_secs(5)).await;
    r.drv.pause().await;
    let events_before = notify_events.load(Ordering::Relaxed);
    println!("baseline heap {:.2} MiB", mib(base as i64));
    println!(
        "size_of ToSwarm<Event, HandlerIn> = {} B (one slot of the behaviour event queue), size_of HandlerIn = {} B",
        std::mem::size_of::<ToSwarm<GossipsubEvent, THandlerInEvent<Gossipsub>>>(),
        std::mem::size_of::<THandlerInEvent<Gossipsub>>()
    );

    // Stage 1: publish n messages without polling the swarm in between (the closure runs between
    // two polls), so everything the behaviour produces sits in its own event queue.
    reset_peak();
    let t = topic.clone();
    let t0 = Instant::now();
    s.run(move |sw| {
        for i in 0..n {
            sw.behaviour_mut()
                .inner
                .publish(IdentTopic::new(t.clone()), payload(i as u32, size))
                .expect("publish");
        }
    })
    .await;
    let publish_took = t0.elapsed();
    // The driver polls the swarm right after the closure; read the heap as soon as the ack came,
    // which races with that poll. Therefore also report the stage-2 numbers.
    let h1 = heap();
    println!(
        "stage 1 (closure returned after {:.2?}): heap delta {:.2} MiB, NotifyHandler events seen so far {}",
        publish_took,
        mib(h1 as i64 - base as i64),
        notify_events.load(Ordering::Relaxed) - events_before
    );
    let (h2, _) = settle(Duration::from_secs(5)).await;
    let events = notify_events.load(Ordering::Relaxed) - events_before;
    println!(
        "stage 2 (swarm polled, quiescent): heap delta {:.2} MiB, peak {:.2} MiB, NotifyHandler events {} = {:.2} per message",
        mib(h2 as i64 - base as i64),
        mib(peak() as i64 - base as i64),
        events,
        events as f64 / n as f64
    );
    println!(
        "per message: heap {:.0} B (payload {size} B)",
        (h2 as i64 - base as i64) as f64 / n as f64
    );

    // Stage 3: the message cache ages out; the queue (if any) stays.
    async_std::task::sleep(Duration::from_secs(8)).await;
    let (h3, _) = settle(Duration::from_secs(5)).await;
    println!(
        "stage 3 (8 s later, caches aged): heap delta {:.2} MiB, per message {:.0} B",
        mib(h3 as i64 - base as i64),
        (h3 as i64 - base as i64) as f64 / n as f64
    );

    // Stage 4: the receiver goes away, the connection (and its handler) is dropped.
    r.drv.stop();
    for waited in [1u64, 3, 10, 20] {
        async_std::task::sleep(Duration::from_secs(waited)).await;
        let (h4, _) = settle(Duration::from_secs(5)).await;
        println!(
            "stage 4 (receiver dropped, ~{waited} s more): heap delta {:.2} MiB",
            mib(h4 as i64 - base as i64)
        );
    }

    // Stage 5: capacity or leak? Wait out the 60 s duplicate-cache lifetime, then queue the same
    // number of messages for a second paused receiver and drop it again. If the residual after
    // stage 4 is reusable capacity, it stays about the same; if it is a leak, it grows.
    async_std::task::sleep(Duration::from_secs(65)).await;
    let (h5a, _) = settle(Duration::from_secs(5)).await;
    println!(
        "stage 5a (65 s later, before the second round): heap delta {:.2} MiB",
        mib(h5a as i64 - base as i64)
    );
    let r2 = spawn_receiver(std::slice::from_ref(&topic), port);
    let mut ready = false;
    for _ in 0..40 {
        let t = topic.clone();
        s.run(move |sw| {
            let _ = sw.behaviour_mut().inner.publish(IdentTopic::new(t), payload(PROBE, 64));
        })
        .await;
        async_std::task::sleep(Duration::from_secs(1)).await;
        if r2.recv.probes.load(Ordering::Relaxed) > 0 {
            ready = true;
            break;
        }
    }
    assert!(ready, "second receiver never saw a probe");
    r2.drv.pause().await;
    let t = topic.clone();
    s.run(move |sw| {
        for i in 0..n {
            sw.behaviour_mut()
                .inner
                .publish(IdentTopic::new(t.clone()), payload(i as u32, size))
                .expect("publish");
        }
    })
    .await;
    let (h5b, _) = settle(Duration::from_secs(5)).await;
    println!(
        "stage 5b (second round queued): heap delta {:.2} MiB",
        mib(h5b as i64 - base as i64)
    );
    r2.drv.stop();
    async_std::task::sleep(Duration::from_secs(10)).await;
    let (h5c, _) = settle(Duration::from_secs(5)).await;
    println!(
        "stage 5c (second receiver dropped): heap delta {:.2} MiB (stage 4 residual was the reference)",
        mib(h5c as i64 - base as i64)
    );
    s.stop();
    async_std::task::sleep(Duration::from_secs(1)).await;
}

/// Scenario F: how long does the production node keep a receiver that stops polling altogether (no
/// pongs either)? Reports when the ping-failure disconnect releases the queue.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "heavy measurement; run explicitly (takes about two minutes)"]
async fn scenario_f_ping_disconnect() {
    let _guard = SERIAL.lock().await;
    let topics = topic_names("gsq-f", 1);
    println!("\n### F fully paused receiver, how long does S keep the connection");
    let mut s = ProdSender::start().await;
    let r = spawn_receiver(&topics, s.port);
    wait_ready(&mut s, &topics[0], &[&r]).await;
    async_std::task::sleep(Duration::from_secs(8)).await;
    let (base, _) = settle(Duration::from_secs(5)).await;
    r.drv.pause().await;
    let t0 = Instant::now();
    const N: usize = 5000;
    for i in 0..N {
        s.publish(&topics, payload(i as u32, 1024)).await;
    }
    println!("| t s | peers on S | heap delta MiB |");
    println!("|---:|---:|---:|");
    let mut dropped_at = None;
    for _ in 0..40 {
        let peers = s.peers().await;
        println!(
            "| {:.0} | {} | {:.2} |",
            t0.elapsed().as_secs_f64(),
            peers,
            mib(heap() as i64 - base as i64)
        );
        if peers == 0 && dropped_at.is_none() {
            dropped_at = Some(t0.elapsed());
            break;
        }
        async_std::task::sleep(Duration::from_secs(5)).await;
    }
    println!("S dropped the unresponsive receiver after {dropped_at:?}");
    async_std::task::sleep(Duration::from_secs(10)).await;
    let (h, _) = settle(Duration::from_secs(5)).await;
    println!(
        "heap delta 10 s after the disconnect: {:.2} MiB",
        mib(h as i64 - base as i64)
    );
    r.drv.stop();
    s.shutdown();
}

/// Scenario G: slow reader. The receiver is polled but sleeps after every event, so it reads at
/// roughly half the publish rate. After publishing stops, the drain is recorded (heap and RSS).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "heavy measurement; run explicitly"]
async fn scenario_g_slow_reader() {
    let _guard = SERIAL.lock().await;
    let size = env_usize("GSQ_G_SIZE", 1024);
    let secs = env_usize("GSQ_G_SECS", 20) as u64;
    let topics = topic_names("gsq-g", 1);
    println!("\n### G slow reader, {size} B, publishing for {secs} s at about twice the reader's rate");
    let mut s = ProdSender::start().await;
    let r = spawn_receiver(&topics, s.port);
    wait_ready(&mut s, &topics[0], &[&r]).await;
    r.delay_us.store(1000, Ordering::Relaxed);

    // Calibrate the reader: a burst of messages, time until all arrived.
    const CAL: usize = 600;
    let t = Instant::now();
    for i in 0..CAL {
        s.publish(&topics, payload(i as u32, size)).await;
    }
    while r.recv.count.load(Ordering::Relaxed) < CAL && t.elapsed() < Duration::from_secs(30) {
        async_std::task::sleep(Duration::from_millis(5)).await;
    }
    let read_rate = CAL as f64 / t.elapsed().as_secs_f64();
    let pub_rate = 2.0 * read_rate;
    println!("calibration: reader {read_rate:.0} msg/s, publish rate set to {pub_rate:.0} msg/s");
    r.recv.count.store(0, Ordering::Relaxed);
    r.recv.order.lock().unwrap().clear();

    async_std::task::sleep(Duration::from_secs(8)).await;
    let (base, _) = settle(Duration::from_secs(5)).await;
    let base_rss = rss_mib();
    reset_peak();
    println!("baseline heap {:.2} MiB, RSS {:.1} MiB", mib(base as i64), base_rss);

    println!("\n| t s | published | delivered | backlog | heap delta MiB | peak delta MiB | RSS MiB | RSS delta MiB |");
    println!("|---:|---:|---:|---:|---:|---:|---:|---:|");
    let t0 = Instant::now();
    let mut published = 0usize;
    let mut next_row = Duration::from_secs(2);
    while t0.elapsed() < Duration::from_secs(secs) {
        let target = (t0.elapsed().as_secs_f64() * pub_rate) as usize;
        while published < target {
            s.publish(&topics, payload(published as u32, size)).await;
            published += 1;
        }
        if t0.elapsed() >= next_row {
            let d = r.recv.count.load(Ordering::Relaxed);
            println!(
                "| {:.1} | {} | {} | {} | {:.2} | {:.2} | {:.1} | {:+.1} |",
                t0.elapsed().as_secs_f64(),
                published,
                d,
                published - d,
                mib(heap() as i64 - base as i64),
                mib(peak() as i64 - base as i64),
                rss_mib(),
                rss_mib() - base_rss
            );
            next_row += Duration::from_secs(2);
        }
        async_std::task::sleep(Duration::from_millis(5)).await;
    }
    let backlog_at_stop = published - r.recv.count.load(Ordering::Relaxed);
    println!("publishing stopped: published {published}, backlog {backlog_at_stop}");

    // Drain curve.
    let t1 = Instant::now();
    println!("\ndrain curve (every 500 ms; reader stays throttled)");
    println!("| t s | delivered | heap delta MiB | RSS MiB | RSS delta MiB |");
    println!("|---:|---:|---:|---:|---:|");
    let mut next_row = Duration::ZERO;
    let mut drained_at = None;
    loop {
        if t1.elapsed() >= next_row {
            curve_row(t1, base, base_rss, r.recv.count.load(Ordering::Relaxed));
            next_row += Duration::from_millis(500);
        }
        if drained_at.is_none() && r.recv.count.load(Ordering::Relaxed) >= published {
            drained_at = Some(t1.elapsed());
        }
        if let Some(d) = drained_at {
            if t1.elapsed() > d + Duration::from_secs(10) {
                break;
            }
        }
        if t1.elapsed() > Duration::from_secs(300) {
            break;
        }
        async_std::task::sleep(Duration::from_millis(20)).await;
    }
    println!(
        "(queue empty after {drained_at:?}; delivered {}/{published})",
        r.recv.count.load(Ordering::Relaxed)
    );
    let tail = env_usize("GSQ_TAIL_SECS", 60) as u64;
    if tail > 0 {
        println!("\nlong tail, every 10 s");
        println!("| t s | delivered | heap delta MiB | RSS MiB | RSS delta MiB |");
        println!("|---:|---:|---:|---:|---:|");
        let tail_start = Instant::now();
        while tail_start.elapsed() < Duration::from_secs(tail) {
            async_std::task::sleep(Duration::from_secs(10)).await;
            curve_row(t1, base, base_rss, r.recv.count.load(Ordering::Relaxed));
        }
    }
    final_report(base, base_rss, &[&r]).await;
    s.shutdown();
}
