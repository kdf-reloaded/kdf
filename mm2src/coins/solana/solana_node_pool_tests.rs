//! The node pool of a Solana coin (CRD ch.40 R40.1.5, the maintainer's decision of 2026-11-19): the order, the health
//! check, the set-aside back-off, one operation on one node, and the safety of a send. Stub nodes on loopback sockets
//! count the requests they get; the clock and the random source are injected, so nothing here sleeps for timing.
//! No assertion message carries a key or an address.

use super::*;
use crate::solana::rpc_pool::{parse_node_order, PoolClock, SolanaNodeOrder, SolanaRpcPool, HEALTH_VALID_MS,
                              SET_ASIDE_FIRST_MS, SET_ASIDE_MAX_MS};
use common::block_on;
use rand::rngs::SmallRng;
use rand::SeedableRng;
use serde_json::json;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

const SIGNATURE: &str = "5h3kS6vr8b8X9ksuY3jLZQbHWfqaJsy2DqERkdMzfiNJjQAQ4qAW9z3GjvPvjyDgyy3yL5fNwbpxgvCQEwTtq8R";

/// A clock the test moves by hand.
#[derive(Debug, Default)]
struct TestClock(AtomicU64);

impl TestClock {
    fn advance(&self, ms: u64) { self.0.fetch_add(ms, Ordering::SeqCst); }
}

impl PoolClock for TestClock {
    fn now_ms(&self) -> u64 { self.0.load(Ordering::SeqCst) + 1_000 }
}

/// How the stub answers.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Healthy,
    /// `getHealth` answers an error; everything else is served.
    UnhealthyAnswer,
    /// The connection is closed without an answer, whatever the request.
    Down,
    /// Everything is served, except that a `sendTransaction` is recorded and its answer dropped.
    DropsSendAnswer,
}

struct Stub {
    url: String,
    mode: Arc<Mutex<Mode>>,
    log: Arc<Mutex<Vec<(String, Json)>>>,
    stop: Arc<AtomicBool>,
}

impl Drop for Stub {
    fn drop(&mut self) { self.stop.store(true, Ordering::SeqCst); }
}

impl Stub {
    fn set_mode(&self, mode: Mode) { *self.mode.lock().expect("the mode") = mode; }

    fn count(&self, method: &str) -> usize {
        self.log
            .lock()
            .expect("the log")
            .iter()
            .filter(|(m, _)| m == method)
            .count()
    }

    fn total(&self) -> usize { self.log.lock().expect("the log").len() }

    fn params_of(&self, method: &str) -> Vec<Json> {
        self.log
            .lock()
            .expect("the log")
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, p)| p.clone())
            .collect()
    }
}

fn read_body(stream: &mut TcpStream) -> Json {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        raw.extend_from_slice(&buf[..n]);
        let text = String::from_utf8_lossy(&raw).to_ascii_lowercase();
        if let Some((head, body)) = text.split_once("\r\n\r\n") {
            let length = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if body.len() >= length {
                break;
            }
        }
    }
    let text = String::from_utf8_lossy(&raw).into_owned();
    let body = text.split_once("\r\n\r\n").map_or("", |(_, body)| body);
    serde_json::from_str(body).unwrap_or(Json::Null)
}

fn spawn(mode: Mode) -> Stub {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind the stub node");
    let port = listener.local_addr().expect("local address").port();
    listener.set_nonblocking(true).expect("non-blocking listener");
    let stop = Arc::new(AtomicBool::new(false));
    let mode = Arc::new(Mutex::new(mode));
    let log = Arc::new(Mutex::new(Vec::new()));
    let (stop_t, mode_t, log_t) = (stop.clone(), mode.clone(), log.clone());
    std::thread::spawn(move || {
        while !stop_t.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream.set_nonblocking(false).ok();
                    let request = read_body(&mut stream);
                    let method = request["method"].as_str().unwrap_or_default().to_owned();
                    let mode = *mode_t.lock().expect("the mode");
                    log_t
                        .lock()
                        .expect("the log")
                        .push((method.clone(), request["params"].clone()));
                    if mode == Mode::Down || (mode == Mode::DropsSendAnswer && method == "sendTransaction") {
                        continue;
                    }
                    let context = json!({ "slot": 1 });
                    let result = match method.as_str() {
                        "getHealth" if mode == Mode::UnhealthyAnswer => {
                            let body = json!({ "jsonrpc": "2.0", "id": request["id"],
                                "error": { "code": -32005, "message": "Node is unhealthy" } })
                            .to_string();
                            respond(&mut stream, &body);
                            continue;
                        },
                        "getHealth" => json!("ok"),
                        "getBalance" => json!({ "context": context, "value": 1_000_000_000u64 }),
                        "getBlockHeight" => json!(7),
                        "getLatestBlockhash" => json!({ "context": context, "value": {
                            "blockhash": solana_hash::Hash::new_from_array([7; 32]).to_string(),
                            "lastValidBlockHeight": 100 } }),
                        "getFeeForMessage" => json!({ "context": context, "value": 5000 }),
                        "sendTransaction" => json!(SIGNATURE),
                        _ => Json::Null,
                    };
                    respond(
                        &mut stream,
                        &json!({ "jsonrpc": "2.0", "id": request["id"], "result": result }).to_string(),
                    );
                },
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                },
                Err(_) => break,
            }
        }
    });
    Stub {
        url: format!("http://127.0.0.1:{port}"),
        mode,
        log,
        stop,
    }
}

fn respond(stream: &mut TcpStream, body: &str) {
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).ok();
    stream.flush().ok();
}

fn pool_of(stubs: &[&Stub], order: SolanaNodeOrder, clock: Arc<TestClock>, seed: u64) -> SolanaRpcPool {
    SolanaRpcPool::with_parts(
        stubs.iter().map(|s| s.url.clone()).collect(),
        None,
        order,
        clock,
        &mut SmallRng::seed_from_u64(seed),
    )
    .expect("a non-empty pool")
}

fn balance(pool: &SolanaRpcPool) -> Result<u64, RpcError> {
    block_on(pool.get_balance(&Pubkey::new_from_array([1; 32])))
}

#[test]
fn solana_node_order_parsing() {
    assert!(
        parse_node_order(&json!({})) == (SolanaNodeOrder::Random, None),
        "absent is random"
    );
    assert!(parse_node_order(&json!({ "solana_node_order": "random" })) == (SolanaNodeOrder::Random, None));
    assert!(parse_node_order(&json!({ "solana_node_order": "listed" })) == (SolanaNodeOrder::Listed, None));
    for other in [json!("mixed"), json!("measured"), json!("Listed"), json!(5)] {
        let (order, warning) = parse_node_order(&json!({ "solana_node_order": other.clone() }));
        assert!(order == SolanaNodeOrder::Random, "an unknown value is random");
        let warning = warning.expect("one warning");
        let shown = other.as_str().map_or_else(|| other.to_string(), str::to_owned);
        assert!(
            warning.contains("solana_node_order") && warning.contains(&shown),
            "names key and value"
        );
    }
}

#[test]
fn set_aside_time_doubles_from_20_s_up_to_12_h() {
    let clock = Arc::new(TestClock::default());
    let stub = spawn(Mode::Down);
    let pool = pool_of(&[&stub], SolanaNodeOrder::Listed, clock.clone(), 1);
    let mut expected = SET_ASIDE_FIRST_MS;
    assert!(balance(&pool).is_err(), "the node is down");
    for _ in 0..30 {
        // One millisecond before the end the node is still set aside: with every node set aside it is tried once,
        // and that failure neither extends the set-aside nor doubles it.
        let asked = stub.total();
        clock.advance(expected - 1);
        assert!(balance(&pool).is_err(), "all set aside: tried once");
        assert!(stub.total() == asked + 1, "tried once while every node is set aside");
        // After the set-aside has ended, the next failure doubles it, at most to 12 h.
        clock.advance(1);
        assert!(balance(&pool).is_err(), "still down");
        expected = (expected * 2).min(SET_ASIDE_MAX_MS);
    }
    assert!(expected == SET_ASIDE_MAX_MS, "the cap is reached");
    // A success resets it.
    clock.advance(SET_ASIDE_MAX_MS);
    stub.set_mode(Mode::Healthy);
    assert!(balance(&pool).is_ok(), "recovered");
    stub.set_mode(Mode::Down);
    assert!(balance(&pool).is_err());
    let asked = stub.total();
    clock.advance(SET_ASIDE_FIRST_MS - 1);
    assert!(balance(&pool).is_err());
    assert!(stub.total() == asked + 1, "after a success the back-off starts over");
}

/// Failures reported on one node within one set-aside window (parallel operations meeting the same fault) set it
/// aside for the first time only; the next failure after the window doubles it; a success resets it; a recovered
/// higher-ranked node is used again on time.
#[test]
fn concurrent_failures_in_one_window_do_not_stack_the_set_aside() {
    let (a, b) = (spawn(Mode::Down), spawn(Mode::Healthy));
    let clock = Arc::new(TestClock::default());
    let pool = pool_of(&[&a, &b], SolanaNodeOrder::Listed, clock.clone(), 1);
    // Six failures of node 0 are reported inside one window.
    for _ in 0..6 {
        pool.report_failure_for_test(0);
    }
    assert!(
        pool.set_aside_remaining_ms_for_test(0) == SET_ASIDE_FIRST_MS,
        "20 s, not more"
    );
    // The node recovers; it is used again as soon as the 20 s have passed.
    a.set_mode(Mode::Healthy);
    clock.advance(SET_ASIDE_FIRST_MS - 1);
    assert!(
        balance(&pool).is_ok() && a.count("getBalance") == 0,
        "still set aside one millisecond before the end"
    );
    clock.advance(1);
    assert!(
        balance(&pool).is_ok() && a.count("getBalance") == 1,
        "the higher-ranked node is back on time"
    );
    // A success reset the count: a failure now is the first again.
    pool.report_failure_for_test(0);
    assert!(
        pool.set_aside_remaining_ms_for_test(0) == SET_ASIDE_FIRST_MS,
        "reset to the base"
    );
    // A failure after that window ends doubles it.
    clock.advance(SET_ASIDE_FIRST_MS);
    pool.report_failure_for_test(0);
    assert!(
        pool.set_aside_remaining_ms_for_test(0) == 2 * SET_ASIDE_FIRST_MS,
        "40 s after a later failure"
    );
}

#[test]
fn listed_order_uses_the_first_healthy_node_for_every_request() {
    let (a, b) = (spawn(Mode::Healthy), spawn(Mode::Healthy));
    let pool = pool_of(&[&a, &b], SolanaNodeOrder::Listed, Arc::new(TestClock::default()), 1);
    for _ in 0..5 {
        assert!(balance(&pool).is_ok(), "served");
    }
    assert!(
        a.count("getBalance") == 5 && b.total() == 0,
        "the first node served everything"
    );
}

#[test]
fn random_order_is_a_stable_permutation_and_varies_across_activations() {
    let urls: Vec<String> = (0..5).map(|i| format!("http://127.0.0.1:{}", 40_000 + i)).collect();
    let mut firsts = std::collections::BTreeSet::new();
    for seed in 0..40u64 {
        let pool = SolanaRpcPool::with_parts(
            urls.clone(),
            None,
            SolanaNodeOrder::Random,
            Arc::new(TestClock::default()),
            &mut SmallRng::seed_from_u64(seed),
        )
        .expect("a pool");
        let order: Vec<String> = pool.urls().iter().map(|u| (*u).to_owned()).collect();
        let mut sorted = order.clone();
        sorted.sort();
        let mut listed = urls.clone();
        listed.sort();
        assert!(sorted == listed, "a permutation of the list");
        assert!(
            pool.urls().iter().map(|u| (*u).to_owned()).collect::<Vec<_>>() == order,
            "stable in one coin"
        );
        firsts.insert(order[0].clone());
    }
    assert!(firsts.len() > 1, "more than one node is first across activations");
    let listed = SolanaRpcPool::with_parts(
        urls.clone(),
        None,
        SolanaNodeOrder::Listed,
        Arc::new(TestClock::default()),
        &mut SmallRng::seed_from_u64(3),
    )
    .expect("a pool");
    assert!(
        listed.urls().iter().map(|u| (*u).to_owned()).collect::<Vec<_>>() == urls,
        "listed keeps the order"
    );
}

#[test]
fn an_unhealthy_first_node_is_skipped_and_gets_no_other_request() {
    let (a, b) = (spawn(Mode::UnhealthyAnswer), spawn(Mode::Healthy));
    let pool = pool_of(&[&a, &b], SolanaNodeOrder::Listed, Arc::new(TestClock::default()), 1);
    assert!(balance(&pool).is_ok() && balance(&pool).is_ok(), "served");
    assert!(
        a.total() == a.count("getHealth") && a.count("getBalance") == 0,
        "only a health probe reached it"
    );
    assert!(b.count("getBalance") == 2, "the second node served");
}

#[test]
fn the_health_check_is_remembered_for_30_s() {
    let a = spawn(Mode::Healthy);
    let clock = Arc::new(TestClock::default());
    let pool = pool_of(&[&a], SolanaNodeOrder::Listed, clock.clone(), 1);
    assert!(balance(&pool).is_ok() && balance(&pool).is_ok());
    assert!(a.count("getHealth") == 1, "not repeated within 30 s");
    clock.advance(HEALTH_VALID_MS - 1);
    assert!(balance(&pool).is_ok());
    assert!(a.count("getHealth") == 1, "still remembered");
    clock.advance(1);
    assert!(balance(&pool).is_ok());
    assert!(a.count("getHealth") == 2, "repeated after 30 s");
}

#[test]
fn a_recovered_higher_ranked_node_is_used_again_after_its_set_aside_and_a_health_check() {
    let (a, b) = (spawn(Mode::Down), spawn(Mode::Healthy));
    let clock = Arc::new(TestClock::default());
    let pool = pool_of(&[&a, &b], SolanaNodeOrder::Listed, clock.clone(), 1);
    assert!(balance(&pool).is_ok(), "the second node serves");
    assert!(b.count("getBalance") == 1, "served by the second node");
    a.set_mode(Mode::Healthy);
    assert!(balance(&pool).is_ok());
    assert!(a.count("getBalance") == 0, "still set aside");
    clock.advance(SET_ASIDE_FIRST_MS);
    assert!(balance(&pool).is_ok());
    assert!(a.count("getBalance") == 1, "the higher-ranked node is back");
    assert!(a.count("getHealth") >= 1, "its health was checked first");
}

#[test]
fn every_node_set_aside_each_is_tried_once_before_the_operation_fails() {
    let (a, b) = (spawn(Mode::Down), spawn(Mode::Down));
    let pool = pool_of(&[&a, &b], SolanaNodeOrder::Listed, Arc::new(TestClock::default()), 1);
    assert!(balance(&pool).is_err(), "no node answers");
    let (asked_a, asked_b) = (a.total(), b.total());
    assert!(asked_a == 1 && asked_b == 1, "each node was tried once");
    // Both are set aside now; the next operation still tries each once.
    let error = balance(&pool).expect_err("still no node");
    assert!(
        a.total() == asked_a + 1 && b.total() == asked_b + 1,
        "each is tried once again"
    );
    assert!(matches!(error.kind, RpcErrorKind::Transport(_)), "a transport error");
}

fn signed_transfer() -> Transaction {
    let key = Keypair::new_from_array([3; 32]);
    let tx = solana_system_transaction::transfer(
        &key,
        &Pubkey::new_from_array([4; 32]),
        5,
        solana_hash::Hash::new_from_array([7; 32]),
    );
    tx
}

#[test]
fn a_send_whose_answer_is_lost_is_retried_with_the_same_signed_bytes_and_never_rebuilt() {
    let (a, b) = (spawn(Mode::DropsSendAnswer), spawn(Mode::Healthy));
    let pool = pool_of(&[&a, &b], SolanaNodeOrder::Listed, Arc::new(TestClock::default()), 1);
    let tx = signed_transfer();
    let signature = block_on(pool.send_transaction(&tx)).expect("the second node takes it");
    assert!(signature.to_string() == SIGNATURE, "the answer of the second node");
    let sent_a = a.params_of("sendTransaction");
    let sent_b = b.params_of("sendTransaction");
    assert!(sent_a.len() == 1 && sent_b.len() == 1, "one send on each node");
    assert!(sent_a[0][0] == sent_b[0][0], "byte-identical transaction bytes");
    assert!(
        a.count("getLatestBlockhash") + b.count("getLatestBlockhash") == 0,
        "no new blockhash was asked"
    );
}

#[test]
fn when_every_node_fails_the_send_error_carries_the_signature() {
    let (a, b) = (spawn(Mode::DropsSendAnswer), spawn(Mode::DropsSendAnswer));
    let urls = vec![a.url.clone(), b.url.clone()];
    let params: SolanaActivationParams =
        serde_json::from_value(json!({ "confirmation_commitment": "finalized", "client_urls": urls })).expect("params");
    let coin = block_on(solana_coin_from_conf_and_params("SOL", &json!({}), params, &[7u8; 32])).expect("a coin");
    let tx = signed_transfer();
    let expected = tx.signatures[0].to_string();
    let hex = hex::encode(bincode::serialize(&tx).expect("serializable"));
    let error = block_on(common::Future01CompatExt::compat(coin.send_raw_tx(&hex))).expect_err("no node takes it");
    assert!(error.contains(&expected), "the error carries the signature");
    assert!(
        a.count("sendTransaction") == 1 && b.count("sendTransaction") == 1,
        "each node got the same send once"
    );
    assert!(
        a.params_of("sendTransaction")[0] == b.params_of("sendTransaction")[0],
        "identical bytes"
    );
}

#[test]
fn one_withdraw_reads_from_one_node_and_a_failure_reruns_it_on_the_next() {
    let (a, b) = (spawn(Mode::Healthy), spawn(Mode::Healthy));
    let urls = vec![a.url.clone(), b.url.clone()];
    let params: SolanaActivationParams =
        serde_json::from_value(json!({ "confirmation_commitment": "finalized", "client_urls": urls })).expect("params");
    let coin = block_on(solana_coin_from_conf_and_params("SOL", &json!({}), params, &[7u8; 32])).expect("a coin");
    block_on(coin.withdraw_reads()).expect("the reads succeed");
    assert!(
        a.count("getLatestBlockhash") == 1 && a.count("getFeeForMessage") == 1 && a.count("getBalance") == 1,
        "all reads on the first node"
    );
    assert!(b.total() == 0, "none on the second");
    // The first node starts dropping connections: the reads are rerun, whole, on the second.
    a.set_mode(Mode::Down);
    let clock_skip = coin.rpc().urls().len();
    assert!(clock_skip == 2);
    // Its health is remembered, so the failure happens in the middle of the operation.
    block_on(coin.withdraw_reads()).expect("the second node takes over");
    assert!(
        b.count("getLatestBlockhash") == 1 && b.count("getFeeForMessage") == 1 && b.count("getBalance") == 1,
        "the whole operation ran on the second node"
    );
}
