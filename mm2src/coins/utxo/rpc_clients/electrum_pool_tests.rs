//! Tests of the Electrum connection pool against scripted stub servers (CRD ch.38 T38.6.4.1 - T38.6.4.13-R).
//!
//! The pool is built with millisecond timings, so no test waits real seconds. The production figures are
//! pinned in `electrum_pool::budget_tests`; the 12-hour suspension cap is tested there with a clock stub.

use super::electrum_measure::NEAR_OFFSET_DEFAULT_MS;
use super::electrum_pool::{parse_server_order, ElectrumPoolOptions, ElectrumPoolTimings, OpenedByCall, ServerOrder,
                           ServerStateSnapshot};
use super::electrum_stub_server::{StubBehavior, StubServer};
use super::*;
use crate::utxo::UtxoRpcMode;
use common::block_on;
use common::executor::Timer;
use futures::channel::mpsc as futures_mpsc;
use serde_json::json;
use std::sync::atomic::AtomicU64 as TestCounter;
use std::time::Instant;

const TICKER: &str = "RIN";
const CLIENT_NAME: &str = "stub-test";

fn ms(n: u64) -> Duration { Duration::from_millis(n) }

/// The production figures divided by about fifty, with the cadences that tests drive by hand pushed out.
fn test_timings() -> ElectrumPoolTimings {
    ElectrumPoolTimings {
        establishment: ms(400),
        request: ms(400),
        idle_cutoff: ms(30_000),
        ping_interval: ms(30_000),
        maintenance_interval: ms(30_000),
        short_repass: ms(30),
        suspension_base: ms(300),
        suspension_cap: ms(2_400),
        call_budget: ms(3_000),
        budget_reserve: ms(100),
        round_floor: ms(100),
        remeasure_limit: ms(600),
        remeasure_cap: ms(7_200),
        poor_demotion: ms(900),
        stale_floor: ms(300),
        latency_floor: ms(300),
        slow_floor: ms(60),
    }
}

fn pool_client(
    servers: &[&StubServer],
    min_connected: usize,
    max_connected: Option<usize>,
    timings: ElectrumPoolTimings,
) -> Arc<ElectrumClientImpl> {
    pool_client_of(
        servers.iter().map(|server| server.request()).collect(),
        min_connected,
        max_connected,
        timings,
    )
}

fn pool_client_of(
    requests: Vec<ElectrumRpcRequest>,
    min_connected: usize,
    max_connected: Option<usize>,
    timings: ElectrumPoolTimings,
) -> Arc<ElectrumClientImpl> {
    pool_client_ordered(requests, min_connected, max_connected, timings, ServerOrder::Listed)
}

/// The pool of the listed `requests`, ranked by `order` (R38.6.4b-R). Every other helper ranks by the listed order.
fn pool_client_ordered(
    requests: Vec<ElectrumRpcRequest>,
    min_connected: usize,
    max_connected: Option<usize>,
    timings: ElectrumPoolTimings,
    order: ServerOrder,
) -> Arc<ElectrumClientImpl> {
    // These tests are about the priority order; the spreading has its own tests.
    pool_client_configured(requests, min_connected, max_connected, timings, order, false)
}

/// Like [`pool_client_ordered`], with `electrum_spread_reads` (R38.6.4a item 5) set as given. The near factor is
/// `0`: the tests that use this are about the turn over every maintained session, whatever the latencies are.
fn pool_client_configured(
    requests: Vec<ElectrumRpcRequest>,
    min_connected: usize,
    max_connected: Option<usize>,
    timings: ElectrumPoolTimings,
    order: ServerOrder,
    spread_reads: bool,
) -> Arc<ElectrumClientImpl> {
    pool_client_spreading(
        requests,
        min_connected,
        max_connected,
        timings,
        order,
        spread_reads,
        0.,
        NEAR_OFFSET_DEFAULT_MS,
    )
}

/// Like [`pool_client_configured`], with `electrum_spread_near_factor` and `electrum_spread_near_offset_ms`
/// (R38.6.4a item 8) set as given.
fn pool_client_spreading(
    requests: Vec<ElectrumRpcRequest>,
    min_connected: usize,
    max_connected: Option<usize>,
    timings: ElectrumPoolTimings,
    order: ServerOrder,
    spread_reads: bool,
    near_factor: f64,
    near_offset_ms: f64,
) -> Arc<ElectrumClientImpl> {
    let mut client = ElectrumClientImpl::new(TICKER.into(), vec![]);
    client.configure_pool(ElectrumPoolOptions {
        min_connected,
        max_connected,
        client_name: Some(CLIENT_NAME.into()),
        timings,
        order,
        spread_reads,
        near_factor,
        near_offset_ms,
        block_interval: None,
        // These tests are about the priority order, the spreading and the measured order of R38.6.4a and R38.6.4l;
        // the tip checks of R38.6.4o have their own tests and would add a header request to every join.
        tip_checks: false,
    });
    // A server that cannot be used stays in the pool as removed; the tests look at the snapshot.
    let _ = client.add_servers(&requests);
    Arc::new(client)
}

fn rpc_item(method: &str, params: Vec<Json>) -> JsonRpcRequest {
    static IDS: TestCounter = TestCounter::new(1_000_000);
    JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: format!("t{}", IDS.fetch_add(1, AtomicOrdering::Relaxed)),
        method: method.into(),
        params,
    }
}

fn rpc(method: &str, params: Vec<Json>) -> JsonRpcRequestEnum { JsonRpcRequestEnum::Single(rpc_item(method, params)) }

async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let started = Instant::now();
    while !condition() {
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "timed out waiting for {what}"
        );
        Timer::sleep(0.01).await;
    }
}

fn snap(client: &ElectrumClientImpl, rank: usize) -> ServerStateSnapshot { client.pool_snapshot()[rank].clone() }

fn maintained(client: &ElectrumClientImpl) -> Vec<usize> {
    client
        .pool_snapshot()
        .iter()
        .filter(|server| server.maintained)
        .map(|server| server.rank)
        .collect()
}

/// Asks the pool and returns the address of the server that answered.
async fn ask(client: &ElectrumClientImpl, method: &str) -> Result<String, String> {
    let budget = client.pool.timings.call_budget;
    client
        .pool
        .route(rpc(method, vec![]), budget)
        .await
        .map(|(addr, _)| addr.0)
}

fn seconds_until(moment: Option<f64>) -> f64 { moment.map_or(0., |moment| moment - now_float()) }

/// Spawns the pool's maintenance loop (and no ping loop).
fn start(client: &Arc<ElectrumClientImpl>) { client.start_pool(false); }

#[test]
fn t38_6_4_1_bounds_and_validation() {
    let servers = |count: usize| -> Vec<ElectrumRpcRequest> {
        (0..count)
            .map(|i| ElectrumRpcRequest {
                url: format!("127.0.0.1:{}", 40_000 + i),
                protocol: Default::default(),
                disable_cert_verification: false,
                spki_sha256: None,
                timeout_sec: None,
            })
            .collect()
    };
    let mode = |min_connected, max_connected| UtxoRpcMode::Electrum {
        servers: servers(2),
        min_connected,
        max_connected,
    };
    // A floor of zero and a floor above the ceiling are unusable settings.
    assert!(mode(Some(0), None).electrum_connection_settings_error().is_some());
    assert!(mode(Some(3), Some(2)).electrum_connection_settings_error().is_some());
    // A ceiling above the list length, and a floor above the list length, are accepted (R38.6.4b item 3).
    assert!(mode(None, Some(10)).electrum_connection_settings_error().is_none());
    assert!(mode(Some(5), None).electrum_connection_settings_error().is_none());

    block_on(async {
        // The ceiling above the list length behaves as the list length.
        let (a, b) = (
            StubServer::start(StubBehavior::Answer).await,
            StubServer::start(StubBehavior::Answer).await,
        );
        let client = pool_client(&[&a, &b], 1, Some(10), test_timings());
        assert_eq!(client.pool.ceiling(), 2);
        start(&client);
        wait_until("both servers maintained", || maintained(&client) == vec![0, 1]).await;
        assert_eq!(client.count_connected().await, 2);

        // A floor above the list length activates, keeps every reachable server, and runs on the regular cadence.
        let (c, d) = (
            StubServer::start(StubBehavior::Answer).await,
            StubServer::start(StubBehavior::Down).await,
        );
        let timings = test_timings();
        let client = pool_client(&[&c, &d], 5, None, timings.clone());
        start(&client);
        wait_until("the reachable server maintained", || maintained(&client) == vec![0]).await;
        assert_eq!(client.pool.next_pass_delay(), timings.maintenance_interval);
        // The coin is usable.
        assert_eq!(ask(&client, "server.ping").await.unwrap(), c.addr());
    });
}

#[test]
fn t38_6_4_2_priority_maintenance_by_trigger() {
    block_on(async {
        let (s1, s2, s3) = (
            StubServer::start(StubBehavior::Down).await,
            StubServer::start(StubBehavior::Answer).await,
            StubServer::start(StubBehavior::Answer).await,
        );
        let client = pool_client(&[&s1, &s2, &s3], 1, Some(2), test_timings());
        start(&client);
        wait_until("servers 2 and 3 maintained", || maintained(&client) == vec![1, 2]).await;
        assert!(snap(&client, 0).candidate);

        s1.set_behavior(StubBehavior::Answer).await;
        // The pass skips a suspended server that the floor does not need, so the suspension has to lapse.
        wait_until("the suspension of server 1 to lapse", || {
            snap(&client, 0).suspended_until.is_none()
        })
        .await;
        client.pool.trigger();
        wait_until("servers 1 and 2 maintained", || maintained(&client) == vec![0, 1]).await;
        let demoted = snap(&client, 2);
        assert!(!demoted.maintained && demoted.candidate && !demoted.connected);
    });
}

#[test]
fn t38_6_4_2_priority_maintenance_on_the_regular_cadence() {
    block_on(async {
        let (s1, s2, s3) = (
            StubServer::start(StubBehavior::Down).await,
            StubServer::start(StubBehavior::Answer).await,
            StubServer::start(StubBehavior::Answer).await,
        );
        let timings = ElectrumPoolTimings {
            maintenance_interval: ms(150),
            ..test_timings()
        };
        let client = pool_client(&[&s1, &s2, &s3], 1, Some(2), timings);
        start(&client);
        wait_until("servers 2 and 3 maintained", || maintained(&client) == vec![1, 2]).await;

        s1.set_behavior(StubBehavior::Answer).await;
        // No trigger: a pass on the cadence finds the recovered server once its suspension lapsed.
        wait_until("servers 1 and 2 maintained", || maintained(&client) == vec![0, 1]).await;
        assert!(!snap(&client, 2).maintained);
    });
}

#[test]
fn t38_6_4_3_floor_refill_from_the_remaining_candidate() {
    block_on(async {
        let servers = [
            StubServer::start(StubBehavior::Answer).await,
            StubServer::start(StubBehavior::Answer).await,
            StubServer::start(StubBehavior::Answer).await,
        ];
        let client = pool_client(&[&servers[0], &servers[1], &servers[2]], 2, Some(2), test_timings());
        start(&client);
        wait_until("servers 1 and 2 maintained", || maintained(&client) == vec![0, 1]).await;

        // A maintained connection drops: the set falls below the floor and a pass refills it at once.
        servers[0].kill_connections();
        wait_until("the set refilled from server 3", || maintained(&client) == vec![1, 2]).await;
        assert!(snap(&client, 0).suspended_until.is_some() || snap(&client, 0).candidate);
    });
}

#[test]
fn t38_6_4_3_a_pass_below_the_floor_repeats_on_the_short_pause() {
    block_on(async {
        let (a, b) = (
            StubServer::start(StubBehavior::Answer).await,
            StubServer::start(StubBehavior::Answer).await,
        );
        let client = pool_client(&[&a, &b], 2, Some(2), test_timings());
        start(&client);
        wait_until("both servers maintained", || maintained(&client) == vec![0, 1]).await;

        // Server A stays reachable at the socket but refuses to serve: no candidate can refill the set.
        a.set_behavior(StubBehavior::RefuseAfter(0)).await;
        a.kill_connections();
        wait_until("the set to fall to server B", || maintained(&client) == vec![1]).await;
        let attempts_then = a.accepted();
        Timer::sleep(0.4).await;
        assert!(
            a.accepted() >= attempts_then + 3,
            "the pass repeated {} times in 0.4 s",
            a.accepted() - attempts_then
        );
        // The coin stays usable over the remaining connection.
        assert_eq!(ask(&client, "server.ping").await.unwrap(), b.addr());
        assert_eq!(maintained(&client), vec![1]);
    });
}

#[test]
fn t38_6_4_4_suspension_doubles_and_resets() {
    block_on(async {
        let (a, b) = (
            StubServer::start(StubBehavior::RefuseAfter(0)).await,
            StubServer::start(StubBehavior::Answer).await,
        );
        let timings = test_timings();
        let base = timings.suspension_base.as_secs_f64();
        let client = pool_client(&[&a, &b], 1, Some(2), timings);
        // Passes are run by hand.
        client.pool.run_pass().await;
        assert_eq!(maintained(&client), vec![1]);

        // First failure: 20 s (scaled), once.
        let first = snap(&client, 0);
        assert_eq!(first.suspension_level, 1);
        let left = seconds_until(first.suspended_until);
        assert!(left > 0. && left <= base, "first suspension: {left}");

        // A pass during the suspension skips the server: the floor is met without it.
        let attempts = a.accepted();
        client.pool.run_pass().await;
        assert_eq!(a.accepted(), attempts);

        // After the suspension lapsed the server is tried and fails again: twice as long.
        wait_until("the first suspension to lapse", || {
            snap(&client, 0).suspended_until.is_none()
        })
        .await;
        client.pool.run_pass().await;
        assert_eq!(a.accepted(), attempts + 1);
        let second = snap(&client, 0);
        assert_eq!(second.suspension_level, 2);
        let left = seconds_until(second.suspended_until);
        assert!(left > base && left <= 2. * base, "second suspension: {left}");

        // Reachable again: a successful connection resets the server ...
        a.set_behavior(StubBehavior::Answer).await;
        wait_until("the second suspension to lapse", || {
            snap(&client, 0).suspended_until.is_none()
        })
        .await;
        client.pool.run_pass().await;
        assert_eq!(maintained(&client), vec![0, 1]);
        assert_eq!(snap(&client, 0).suspension_level, 0);

        // ... so the next disconnection suspends it for the base time again.
        a.kill_connections();
        wait_until("server A to be dropped", || !snap(&client, 0).maintained).await;
        let dropped = snap(&client, 0);
        assert_eq!(dropped.suspension_level, 1);
        let left = seconds_until(dropped.suspended_until);
        assert!(left > 0. && left <= base, "suspension after a disconnection: {left}");
    });
}

#[test]
fn t38_6_4_4_a_suspended_server_is_connected_when_the_floor_needs_it() {
    block_on(async {
        let server = StubServer::start(StubBehavior::Answer).await;
        let timings = ElectrumPoolTimings {
            suspension_base: ms(1_000),
            ..test_timings()
        };
        let client = pool_client(&[&server], 1, None, timings);
        start(&client);
        wait_until("the server maintained", || maintained(&client) == vec![0]).await;

        let dropped = Instant::now();
        server.kill_connections();
        wait_until("the server maintained again", || {
            maintained(&client) == vec![0] && server.accepted() >= 2
        })
        .await;
        // It did not wait for the one-second suspension to lapse.
        assert!(dropped.elapsed() < ms(800), "{:?}", dropped.elapsed());
    });
}

#[test]
fn t38_6_4_4_a_directed_request_connects_a_suspended_server() {
    block_on(async {
        let (a, b) = (
            StubServer::start(StubBehavior::Answer).await,
            StubServer::start(StubBehavior::Answer).await,
        );
        b.set_result("blockchain.block.header", json!("00"));
        let client = pool_client(&[&a, &b], 1, Some(1), test_timings());
        client.pool.run_pass().await;
        assert_eq!(maintained(&client), vec![0]);

        // Server B joined and left once (a directed request), so it is suspended now.
        let request = rpc("blockchain.block.header", vec![json!(5)]);
        client.pool.route_directed(request.clone(), &b.addr()).await.unwrap();
        wait_until("server B released", || !snap(&client, 1).connected).await;
        assert!(snap(&client, 1).suspended_until.is_some());

        // Suspension governs maintenance only: the directed request connects it anyway.
        let (addr, _) = client.pool.route_directed(request, &b.addr()).await.unwrap();
        assert_eq!(addr.0, b.addr());
        assert_eq!(b.requests("blockchain.block.header"), 2);
        assert_eq!(b.accepted(), 2);
    });
}

#[test]
fn t38_6_4_5_permanent_removal() {
    block_on(async {
        let good = StubServer::start(StubBehavior::Answer).await;
        let rejecting = StubServer::start(StubBehavior::BadVersion).await;
        let ws = ElectrumRpcRequest {
            url: "127.0.0.1:1".into(),
            protocol: ElectrumProtocol::WS,
            disable_cert_verification: false,
            spki_sha256: None,
            timeout_sec: None,
        };
        let unparseable = ElectrumRpcRequest {
            url: "not-an-address".into(),
            protocol: Default::default(),
            disable_cert_verification: false,
            spki_sha256: None,
            timeout_sec: None,
        };
        let unresolvable = ElectrumRpcRequest {
            url: "no-such-host.invalid:50001".into(),
            protocol: Default::default(),
            disable_cert_verification: false,
            spki_sha256: None,
            timeout_sec: None,
        };
        // A TLS address needs a host name; an IP literal has none.
        let tls_without_a_name = ElectrumRpcRequest {
            url: "127.0.0.1:50002".into(),
            protocol: ElectrumProtocol::SSL,
            disable_cert_verification: false,
            spki_sha256: None,
            timeout_sec: None,
        };
        let client = pool_client_of(
            vec![
                unparseable,
                ws,
                tls_without_a_name,
                rejecting.request(),
                unresolvable,
                good.request(),
            ],
            2,
            None,
            test_timings(),
        );
        // The unusable entries leave the pool at once.
        let snapshot = client.pool_snapshot();
        assert!(snapshot[0].removed && snapshot[1].removed && snapshot[2].removed);

        client.pool.run_pass().await;
        let snapshot = client.pool_snapshot();
        // The server that rejects the versions is removed by the first pass, and never counts toward the floor.
        assert!(snapshot[3].removed, "{:?}", snapshot[3]);
        assert_eq!(client.count_connections().await, 2);
        // The server whose name does not resolve is suspended and stays.
        assert!(
            !snapshot[4].removed && snapshot[4].suspended_until.is_some(),
            "{:?}",
            snapshot[4]
        );
        assert_eq!(maintained(&client), vec![5]);
        // The floor of 2 can be met by the two remaining servers, so passes repeat on the short pause.
        assert_eq!(client.pool.next_pass_delay(), client.pool.timings.short_repass);
    });
}

#[test]
fn t38_6_4_5_a_pool_without_a_usable_server_fails_every_request() {
    block_on(async {
        let ws = ElectrumRpcRequest {
            url: "127.0.0.1:1".into(),
            protocol: ElectrumProtocol::WS,
            disable_cert_verification: false,
            spki_sha256: None,
            timeout_sec: None,
        };
        let client = pool_client_of(vec![ws], 1, None, test_timings());
        assert_eq!(client.count_connections().await, 0);
        let error = ask(&client, "server.ping").await.unwrap_err();
        assert!(error.contains(TICKER), "{error}");
    });
}

#[test]
fn t38_6_4_6_establishment_is_bounded_by_timeout_sec_and_suspends() {
    block_on(async {
        let silent = StubServer::start(StubBehavior::Silent).await;
        let mut request = silent.request();
        request.timeout_sec = Some(0.25);
        let client = pool_client_of(vec![request], 1, None, test_timings());

        let started = Instant::now();
        let error = client
            .pool
            .route_directed(rpc("blockchain.block.header", vec![json!(1)]), &silent.addr())
            .await
            .unwrap_err();
        let elapsed = started.elapsed();
        assert!(elapsed >= ms(240) && elapsed < ms(390), "{elapsed:?}: {error}");
        assert!(error.contains(&silent.addr()) && error.contains(TICKER), "{error}");
        let state = snap(&client, 0);
        assert!(!state.connected && state.suspended_until.is_some());
        assert_eq!(silent.requests("blockchain.block.header"), 0);
    });
}

#[test]
fn t38_6_4_6_establishment_defaults_to_twenty_seconds_scaled() {
    block_on(async {
        let silent = StubServer::start(StubBehavior::Silent).await;
        let client = pool_client(&[&silent], 1, None, test_timings());
        let started = Instant::now();
        client
            .pool
            .route_directed(rpc("blockchain.block.header", vec![json!(1)]), &silent.addr())
            .await
            .unwrap_err();
        let elapsed = started.elapsed();
        assert!(elapsed >= ms(390) && elapsed < ms(700), "{elapsed:?}");
    });
}

#[test]
fn t38_6_4_6_a_request_over_a_connected_server_fails_at_the_request_bound() {
    block_on(async {
        let server = StubServer::start(StubBehavior::Answer).await;
        let client = pool_client(&[&server], 1, None, test_timings());
        client.pool.run_pass().await;
        assert_eq!(maintained(&client), vec![0]);

        server.set_behavior(StubBehavior::BlackHoleRequests).await;
        let started = Instant::now();
        let error = ask(&client, "blockchain.block.header").await.unwrap_err();
        let elapsed = started.elapsed();
        // The maintained attempt and the retry over the pool each take the request bound.
        assert!(elapsed >= ms(380) && elapsed < ms(1_500), "{elapsed:?}: {error}");
        assert!(error.contains(&server.addr()), "{error}");
        // The connection was closed on the failure, so the server was suspended.
        assert!(server.requests("blockchain.block.header") >= 1);
        assert!(snap(&client, 0).suspension_level >= 1);
    });
}

#[test]
fn t38_6_4_6_a_silent_connection_is_closed_after_the_idle_cutoff_and_not_earlier() {
    block_on(async {
        let server = StubServer::start(StubBehavior::Answer).await;
        let timings = ElectrumPoolTimings {
            idle_cutoff: ms(700),
            ..test_timings()
        };
        let client = pool_client(&[&server], 1, None, timings);
        let started = Instant::now();
        client.pool.run_pass().await;
        assert!(snap(&client, 0).maintained);

        Timer::sleep(0.4).await;
        assert!(snap(&client, 0).connected, "closed after {:?}", started.elapsed());
        wait_until("the idle connection to be closed", || !snap(&client, 0).connected).await;
        assert!(started.elapsed() >= ms(690), "closed after {:?}", started.elapsed());
        let state = snap(&client, 0);
        assert!(!state.maintained && state.suspended_until.is_some());
    });
}

#[test]
fn t38_6_4_6_pings_go_to_maintained_connections_only() {
    block_on(async {
        let (s1, s2, s3) = (
            StubServer::start(StubBehavior::Down).await,
            StubServer::start(StubBehavior::Answer).await,
            StubServer::start(StubBehavior::Answer).await,
        );
        let timings = ElectrumPoolTimings {
            ping_interval: ms(100),
            ..test_timings()
        };
        let client = pool_client(&[&s1, &s2, &s3], 1, Some(2), timings);
        client.start_pool(true);
        wait_until("servers 2 and 3 maintained", || maintained(&client) == vec![1, 2]).await;
        wait_until("pings on both", || {
            s2.requests("server.ping") >= 2 && s3.requests("server.ping") >= 2
        })
        .await;

        // Server 1 recovers and takes server 3's place; server 3 is demoted and no longer pinged.
        s1.set_behavior(StubBehavior::Answer).await;
        wait_until("the suspension of server 1 to lapse", || {
            snap(&client, 0).suspended_until.is_none()
        })
        .await;
        client.pool.trigger();
        wait_until("servers 1 and 2 maintained", || maintained(&client) == vec![0, 1]).await;
        Timer::sleep(0.15).await;
        let pings_of_the_demoted = s3.requests("server.ping");
        let pings_of_the_first = s1.requests("server.ping");
        Timer::sleep(0.45).await;
        assert_eq!(s3.requests("server.ping"), pings_of_the_demoted);
        assert!(s1.requests("server.ping") >= pings_of_the_first + 3);
    });
}

#[test]
fn t38_6_4_7_a_failing_server_leaves_the_set_and_the_next_one_answers() {
    block_on(async {
        let (a, b) = (
            StubServer::start(StubBehavior::Answer).await,
            StubServer::start(StubBehavior::Answer).await,
        );
        let client = pool_client(&[&a, &b], 1, Some(2), test_timings());
        client.pool.run_pass().await;
        assert_eq!(maintained(&client), vec![0, 1]);

        a.set_behavior(StubBehavior::BlackHoleRequests).await;
        assert_eq!(ask(&client, "blockchain.block.header").await.unwrap(), b.addr());
        assert_eq!(a.requests("blockchain.block.header"), 1);
        let state = snap(&client, 0);
        assert!(!state.maintained && !state.connected && state.suspended_until.is_some());
        assert_eq!(maintained(&client), vec![1]);

        // The priority is kept for the servers still maintained: the next request does not touch A.
        assert_eq!(ask(&client, "blockchain.block.header").await.unwrap(), b.addr());
        assert_eq!(a.requests("blockchain.block.header"), 1);
    });
}

#[test]
fn t38_6_4_7_a_failure_below_the_floor_triggers_a_pass() {
    block_on(async {
        let (a, b) = (
            StubServer::start(StubBehavior::Answer).await,
            StubServer::start(StubBehavior::Answer).await,
        );
        let client = pool_client(&[&a, &b], 2, Some(2), test_timings());
        start(&client);
        wait_until("both maintained", || maintained(&client) == vec![0, 1]).await;
        assert_eq!(a.accepted(), 1);

        a.set_behavior(StubBehavior::BlackHoleRequests).await;
        assert_eq!(ask(&client, "blockchain.block.header").await.unwrap(), b.addr());
        // The set fell below the floor, so a pass reconnects the failed server (suspension notwithstanding).
        wait_until("a pass to reconnect server A", || a.accepted() >= 2).await;
        wait_until("both maintained again", || maintained(&client) == vec![0, 1]).await;
    });
}

#[test]
fn t38_6_4_7_when_every_maintained_server_fails_the_pool_answers_and_the_extra_connection_is_released() {
    block_on(async {
        let servers = [
            StubServer::start(StubBehavior::Answer).await,
            StubServer::start(StubBehavior::Answer).await,
            StubServer::start(StubBehavior::Answer).await,
        ];
        let [a, b, c] = &servers;
        let client = pool_client(&[a, b, c], 1, Some(2), test_timings());
        client.pool.run_pass().await;
        assert_eq!(maintained(&client), vec![0, 1]);
        assert_eq!(c.accepted(), 0);

        a.set_behavior(StubBehavior::BlackHoleRequests).await;
        b.set_behavior(StubBehavior::BlackHoleRequests).await;
        assert_eq!(ask(&client, "blockchain.block.header").await.unwrap(), c.addr());
        // C was connected for the request and released afterwards; it never joined the set.
        assert_eq!(c.accepted(), 1);
        wait_until("server C to be released", || !snap(&client, 2).connected).await;
        let state = snap(&client, 2);
        assert!(!state.maintained && state.suspended_until.is_some());
        // The failed servers left the set, which never exceeded the ceiling.
        assert!(maintained(&client).len() <= client.pool.ceiling());
        assert_eq!(maintained(&client), Vec::<usize>::new());
    });
}

#[test]
fn t38_6_4_7_a_ping_failing_everywhere_does_not_touch_the_other_servers() {
    block_on(async {
        let servers = [
            StubServer::start(StubBehavior::Answer).await,
            StubServer::start(StubBehavior::Answer).await,
            StubServer::start(StubBehavior::Answer).await,
        ];
        let [a, b, c] = &servers;
        let client = pool_client(&[a, b, c], 1, Some(2), test_timings());
        client.pool.run_pass().await;
        assert_eq!(maintained(&client), vec![0, 1]);

        a.set_behavior(StubBehavior::Silent).await;
        b.set_behavior(StubBehavior::Silent).await;
        let ping = rpc("server.ping", vec![]);
        let error = client.pool.route(ping, ms(3_000)).await.unwrap_err();
        assert!(error.contains(&a.addr()) && error.contains(&b.addr()), "{error}");
        // Both failed pings closed their connections; the pool was not retried for it.
        assert_eq!(c.accepted(), 0);
        assert_eq!(maintained(&client), Vec::<usize>::new());
    });
}

#[test]
fn t38_6_4_8_a_directed_request_connects_a_server_outside_the_set_and_releases_it() {
    block_on(async {
        let (a, b) = (
            StubServer::start(StubBehavior::Answer).await,
            StubServer::start(StubBehavior::Answer).await,
        );
        b.set_result("blockchain.block.header", json!("00"));
        let client = pool_client(&[&a, &b], 1, Some(1), test_timings());
        client.pool.run_pass().await;
        assert_eq!(maintained(&client), vec![0]);
        assert_eq!(b.accepted(), 0);

        let (addr, _) = client
            .pool
            .route_directed(rpc("blockchain.block.header", vec![json!(5)]), &b.addr())
            .await
            .unwrap();
        assert_eq!(addr.0, b.addr());
        assert_eq!(b.accepted(), 1);
        wait_until("the directed connection to be released", || !snap(&client, 1).connected).await;
        assert!(!snap(&client, 1).maintained);

        // The version negotiation never opens a connection.
        let version = rpc("server.version", vec![json!("x"), json!(["1.2", "1.4"])]);
        let error = client.pool.route_directed(version, &b.addr()).await.unwrap_err();
        assert!(error.contains(&b.addr()) && error.contains(TICKER), "{error}");
        assert_eq!(b.accepted(), 1);

        // A server outside the pool fails without a connection attempt.
        let error = client
            .pool
            .route_directed(rpc("blockchain.block.header", vec![json!(5)]), "127.0.0.1:9")
            .await
            .unwrap_err();
        assert!(error.contains("127.0.0.1:9") && error.contains(TICKER), "{error}");
        assert_eq!((a.accepted(), b.accepted()), (1, 1));
    });
}

#[test]
fn t38_6_4_9_a_subscription_is_placed_again_on_a_remaining_connection() {
    block_on(async {
        let (a, b) = (
            StubServer::start(StubBehavior::Answer).await,
            StubServer::start(StubBehavior::Answer).await,
        );
        let client = pool_client(&[&a, &b], 1, Some(2), test_timings());
        start(&client);
        wait_until("both maintained", || maintained(&client) == vec![0, 1]).await;

        let script_hash = "5a11e2e1bd0ac1b0a6f1f3a1c0aa0001".to_owned();
        let (watcher_tx, mut watcher_rx) = futures_mpsc::unbounded();
        watch_scripthash(script_hash.clone(), watcher_tx);
        ElectrumClient(client.clone())
            .subscribe_scripthash(script_hash.clone())
            .await
            .unwrap();
        assert_eq!(
            (
                a.requests(BLOCKCHAIN_SCRIPTHASH_SUB_ID),
                b.requests(BLOCKCHAIN_SCRIPTHASH_SUB_ID)
            ),
            (1, 0)
        );

        // The connection carrying the subscription ends.
        a.kill_connections();
        wait_until("the subscription placed on server B", || {
            b.requests(BLOCKCHAIN_SCRIPTHASH_SUB_ID) >= 1
        })
        .await;
        assert_eq!(
            b.last_request(BLOCKCHAIN_SCRIPTHASH_SUB_ID).unwrap()["params"],
            json!([script_hash])
        );

        // A later notification reaches the same watcher.
        b.notify(BLOCKCHAIN_SCRIPTHASH_SUB_ID, json!([script_hash, "status"]));
        let woken = common::executor::Timer::sleep(5.);
        let notified = futures::future::select(Box::pin(watcher_rx.next()), Box::pin(woken)).await;
        match notified {
            futures::future::Either::Left((Some(hash), _)) => assert_eq!(hash, script_hash),
            _ => panic!("the watcher was not woken"),
        }
        unwatch_scripthash(&script_hash);
    });
}

#[test]
fn t38_6_4_9_with_no_connection_left_the_subscription_waits_for_one() {
    block_on(async {
        let server = StubServer::start(StubBehavior::Answer).await;
        let client = pool_client(&[&server], 1, None, test_timings());
        start(&client);
        wait_until("the server maintained", || maintained(&client) == vec![0]).await;

        let script_hash = "5a11e2e1bd0ac1b0a6f1f3a1c0aa0002".to_owned();
        ElectrumClient(client.clone())
            .subscribe_scripthash(script_hash.clone())
            .await
            .unwrap();
        assert_eq!(server.requests(BLOCKCHAIN_SCRIPTHASH_SUB_ID), 1);

        // The only connection ends; the pass reconnects the server and the subscription follows.
        server.kill_connections();
        wait_until("the subscription placed again", || {
            server.requests(BLOCKCHAIN_SCRIPTHASH_SUB_ID) >= 2
        })
        .await;
        assert!(server.accepted() >= 2);
        unwatch_scripthash(&script_hash);
    });
}

#[test]
fn t38_6_4_10_the_failure_names_the_coin_and_every_server_tried() {
    block_on(async {
        let down = StubServer::start(StubBehavior::Down).await;
        let silent = StubServer::start(StubBehavior::Silent).await;
        let refusing = StubServer::start(StubBehavior::RefuseAfter(0)).await;
        let client = pool_client(&[&down, &silent, &refusing], 1, None, test_timings());
        client.pool.run_pass().await;

        let error = ask(&client, "blockchain.block.header").await.unwrap_err();
        assert!(error.contains(TICKER), "{error}");
        for server in [&down, &silent, &refusing] {
            let addr = server.addr();
            assert_eq!(error.matches(&format!("{addr}: ")).count(), 1, "{addr} in {error}");
        }
    });
}

#[test]
fn t38_6_4_11_every_server_gets_its_turn_within_the_call_budget() {
    block_on(async {
        let servers = [
            StubServer::start(StubBehavior::Answer).await,
            StubServer::start(StubBehavior::Answer).await,
            StubServer::start(StubBehavior::Answer).await,
            StubServer::start(StubBehavior::Answer).await,
        ];
        let timings = ElectrumPoolTimings {
            call_budget: ms(1_200),
            ..test_timings()
        };
        let client = pool_client(&[&servers[0], &servers[1], &servers[2], &servers[3]], 1, None, timings);
        client.pool.run_pass().await;
        assert_eq!(maintained(&client), vec![0, 1, 2, 3]);
        for server in &servers[..3] {
            server.set_behavior(StubBehavior::BlackHoleRequests).await;
        }

        let started = Instant::now();
        let answered = ask(&client, "blockchain.block.header").await.unwrap();
        assert_eq!(answered, servers[3].addr());
        assert!(started.elapsed() < ms(1_200), "{:?}", started.elapsed());
        for server in &servers[..3] {
            assert_eq!(server.requests("blockchain.block.header"), 1);
        }
    });
}

#[test]
fn t38_6_4_11_a_tight_budget_ends_the_call_and_names_both_servers() {
    block_on(async {
        let (a, b) = (
            StubServer::start(StubBehavior::Answer).await,
            StubServer::start(StubBehavior::Answer).await,
        );
        let client = pool_client(&[&a, &b], 1, None, test_timings());
        client.pool.run_pass().await;
        a.set_behavior(StubBehavior::BlackHoleRequests).await;
        b.set_behavior(StubBehavior::BlackHoleRequests).await;

        let budget = ms(600);
        let started = Instant::now();
        let error = client
            .pool
            .route(rpc("blockchain.block.header", vec![]), budget)
            .await
            .unwrap_err();
        let elapsed = started.elapsed();
        assert!(elapsed < budget + ms(150), "{elapsed:?}");
        assert!(elapsed >= ms(300), "{elapsed:?}");
        assert!(
            error.contains(&a.addr()) && error.contains(&b.addr()) && error.contains(TICKER),
            "{error}"
        );
    });
}

#[test]
fn the_wallet_maintainers_case_dead_servers_first_and_a_live_one_last() {
    block_on(async {
        let dead = StubServer::start(StubBehavior::Down).await;
        let silent = StubServer::start(StubBehavior::Silent).await;
        let live = StubServer::start(StubBehavior::Answer).await;
        live.set_result("blockchain.headers.subscribe", json!({"height": 777, "hex": "00"}));
        let client = pool_client(&[&dead, &silent, &live], 1, Some(1), test_timings());

        // The activation probe finds the live server in the time of its own connection, without waiting
        // for the silent server, and the block-height query that follows is answered at once.
        let started = Instant::now();
        client.activation_probe().await.unwrap();
        assert_eq!(maintained(&client), vec![2]);
        start(&client);
        let height = ElectrumClient(client.clone()).get_block_count().compat().await.unwrap();
        assert_eq!(height, 777);
        assert!(started.elapsed() < ms(350), "{:?}", started.elapsed());
        assert_eq!(maintained(&client), vec![2]);
    });
}

/// Waits until the activation probe has ended: every server of the pool is either maintained, or has no
/// connection and is suspended or removed.
async fn wait_for_probe_end(client: &ElectrumClientImpl) {
    // A connection joins the set as it is made, so "every server maintained or released" no longer tells the end.
    wait_until("the probe to end", || client.pool.probe_ended()).await;
    wait_until("the probe to end", || {
        client.pool_snapshot().iter().all(|server| {
            server.maintained || (!server.connected && (server.removed || server.suspended_until.is_some()))
        })
    })
    .await;
}

/// Waits until the probe has released every connection it does not keep: each server is maintained or not connected.
/// Unlike [`wait_for_probe_end`] it does not need a released server to be suspended still, which suits tests with a
/// short suspension.
async fn wait_for_probe_released(client: &ElectrumClientImpl) {
    wait_until("the probe to end", || client.pool.probe_ended()).await;
    wait_until("the probe to release its connections", || {
        client
            .pool_snapshot()
            .iter()
            .all(|server| server.maintained || !server.connected)
    })
    .await;
}

/// T38.6.4.12-R: three servers, `max_connected` 1, the first two silent, the third live: the build returns
/// after the third server's establishment, well inside one round budget, and the third server is maintained.
#[test]
fn t38_6_4_12_r_the_probe_returns_after_the_live_servers_establishment() {
    block_on(async {
        let (a, b, c) = (
            StubServer::start(StubBehavior::Silent).await,
            StubServer::start(StubBehavior::Silent).await,
            StubServer::start(StubBehavior::Answer).await,
        );
        let timings = test_timings();
        let round = super::electrum_pool::round_budget(
            timings.call_budget,
            3,
            timings.establishment + timings.request,
            &timings,
        );
        let client = pool_client(&[&a, &b, &c], 1, Some(1), timings);

        let started = Instant::now();
        client.activation_probe().await.unwrap();
        let elapsed = started.elapsed();
        assert!(elapsed < round, "{elapsed:?} against the round budget {round:?}");
        assert!(elapsed < ms(300), "{elapsed:?}");
        assert_eq!(maintained(&client), vec![2]);

        // The silent servers run out their establishment bound in the background and are suspended;
        // the set never exceeds the ceiling.
        wait_for_probe_end(&client).await;
        assert_eq!(maintained(&client), vec![2]);
        assert!(snap(&client, 0).suspended_until.is_some() && snap(&client, 1).suspended_until.is_some());
    });
}

/// T38.6.4.12-R: every server live, `max_connected` 1: after the probe exactly one server is maintained, the
/// highest-ranked of those that connected, and the others are released and suspended.
#[test]
fn t38_6_4_12_r_with_every_server_live_the_highest_ranked_stays() {
    block_on(async {
        let servers = [
            StubServer::start(StubBehavior::Answer).await,
            StubServer::start(StubBehavior::Answer).await,
            StubServer::start(StubBehavior::Answer).await,
        ];
        // The highest-ranked server connects last.
        servers[0].set_accept_delay(ms(120));
        servers[1].set_accept_delay(ms(60));
        let client = pool_client(&[&servers[0], &servers[1], &servers[2]], 1, Some(1), test_timings());

        client.activation_probe().await.unwrap();
        wait_for_probe_end(&client).await;
        assert_eq!(maintained(&client), vec![0]);
        for rank in [1, 2] {
            let state = snap(&client, rank);
            assert!(
                !state.maintained && !state.connected && !state.removed && state.suspended_until.is_some(),
                "{state:?}"
            );
        }
        assert!(snap(&client, 0).suspended_until.is_none());
        // A server that connected after the first success but did not outrank the member is released at once.
        let client_two = pool_client(&[&servers[2], &servers[1]], 1, Some(1), test_timings());
        client_two.activation_probe().await.unwrap();
        wait_for_probe_end(&client_two).await;
        assert_eq!(maintained(&client_two), vec![0]);
    });
}

/// T38.6.4.12-R: a server whose attempt failed is suspended, one that rejects the version range is removed.
#[test]
fn t38_6_4_12_r_failed_attempts_suspend_and_a_rejected_version_removes() {
    block_on(async {
        let down = StubServer::start(StubBehavior::Down).await;
        let rejecting = StubServer::start(StubBehavior::BadVersion).await;
        let live = StubServer::start(StubBehavior::Answer).await;
        let client = pool_client(&[&down, &rejecting, &live], 1, Some(1), test_timings());

        client.activation_probe().await.unwrap();
        wait_for_probe_end(&client).await;
        let down = snap(&client, 0);
        assert!(!down.removed && down.suspended_until.is_some(), "{down:?}");
        assert!(snap(&client, 1).removed, "{:?}", snap(&client, 1));
        assert_eq!(maintained(&client), vec![2]);
    });
}

/// T38.6.4.12-R: with every server dead the build fails within the call budget, naming the coin and each
/// `host:port`.
#[test]
fn t38_6_4_12_r_with_every_server_dead_the_probe_fails_naming_each_server() {
    block_on(async {
        let (down, silent, refusing) = (
            StubServer::start(StubBehavior::Down).await,
            StubServer::start(StubBehavior::Silent).await,
            StubServer::start(StubBehavior::RefuseAfter(0)).await,
        );
        let timings = test_timings();
        let budget = timings.call_budget;
        let client = pool_client(&[&down, &silent, &refusing], 1, Some(1), timings);

        let started = Instant::now();
        let error = client.activation_probe().await.unwrap_err();
        assert!(started.elapsed() < budget, "{:?}", started.elapsed());
        assert!(error.contains(TICKER), "{error}");
        for server in [&down, &silent, &refusing] {
            let addr = server.addr();
            assert_eq!(error.matches(&format!("{addr}: ")).count(), 1, "{addr} in {error}");
        }
        assert!(maintained(&client).is_empty());
    });
}

/// T38.6.4.12-R: the build never waits longer than the call budget, also when the establishment bound is longer.
#[test]
fn t38_6_4_12_r_the_probe_never_waits_longer_than_the_call_budget() {
    block_on(async {
        let (a, b) = (
            StubServer::start(StubBehavior::Silent).await,
            StubServer::start(StubBehavior::Silent).await,
        );
        let timings = ElectrumPoolTimings {
            establishment: ms(5_000),
            call_budget: ms(500),
            ..test_timings()
        };
        let client = pool_client(&[&a, &b], 1, None, timings);

        let started = Instant::now();
        let error = client.activation_probe().await.unwrap_err();
        let elapsed = started.elapsed();
        assert!(elapsed >= ms(450) && elapsed < ms(800), "{elapsed:?}");
        assert!(
            error.contains(TICKER) && error.contains(&a.addr()) && error.contains(&b.addr()),
            "{error}"
        );
        // A connection that would be established after the call ended is released at once, not maintained.
        assert!(maintained(&client).is_empty());
    });
}

#[test]
fn a_json_rpc_error_answer_ends_the_request_and_closes_nothing() {
    block_on(async {
        let (a, b) = (
            StubServer::start(StubBehavior::JsonRpcError).await,
            StubServer::start(StubBehavior::Answer).await,
        );
        let client = pool_client(&[&a, &b], 1, Some(2), test_timings());
        client.pool.run_pass().await;
        assert_eq!(maintained(&client), vec![0, 1]);

        let error = ElectrumClient(client.clone())
            .scripthash_get_balance("aabb")
            .compat()
            .await
            .unwrap_err();
        match &error.error {
            JsonRpcErrorType::Response(addr, answer) => {
                assert_eq!(addr.0, a.addr());
                assert_eq!(answer["message"], json!("stub error"));
            },
            other => panic!("expected the server's answer, got {other:?}"),
        }
        // The next server was not asked, and no connection was closed.
        assert_eq!(b.ordinary_requests(), 0);
        assert_eq!(maintained(&client), vec![0, 1]);
        assert_eq!((a.accepted(), b.accepted()), (1, 1));
    });
}

#[test]
fn the_requests_on_the_wire_are_the_same_as_before() {
    block_on(async {
        let server = StubServer::start(StubBehavior::Answer).await;
        server.set_result("blockchain.headers.subscribe", json!({"height": 5, "hex": "00"}));
        let client = pool_client(&[&server], 1, None, test_timings());
        client.pool.run_pass().await;

        let version = server.last_request("server.version").unwrap();
        assert_eq!(version["jsonrpc"], json!("2.0"));
        assert_eq!(version["params"], json!([CLIENT_NAME, ["1.2", "1.4"]]));
        client.pool.ping_maintained().await;
        let ping = server.last_request("server.ping").unwrap();
        assert_eq!(
            (ping["jsonrpc"].clone(), ping["params"].clone()),
            (json!("2.0"), json!([]))
        );
        assert_eq!(server.requests("server.version"), 1);
        ElectrumClient(client.clone()).get_block_count().compat().await.unwrap();
        let headers = server.last_request("blockchain.headers.subscribe").unwrap();
        assert_eq!(headers["params"], json!([]));
    });
}

/// Every field of a JSON-RPC response has a default, so the parser has to try the notification form first;
/// otherwise a notification is taken for an answer with an empty id and dropped (R38.6.5, R38.6.4h).
#[test]
fn a_subscription_notification_is_not_taken_for_a_response() {
    block_on(async {
        let hash = "5a11e2e1bd0ac1b0a6f1f3a1c0aa0099".to_owned();
        let (tx, mut rx) = futures_mpsc::unbounded();
        watch_scripthash(hash.clone(), tx);
        let pending: JsonRpcPendingRequestsShared = Arc::new(AsyncMutex::new(Default::default()));
        let notification =
            json!({"jsonrpc": "2.0", "method": BLOCKCHAIN_SCRIPTHASH_SUB_ID, "params": [hash, "status"]});
        super::electrum_rpc_client::electrum_process_json_for_test(notification, &pending).await;
        assert_eq!(rx.try_recv().ok(), Some(hash.clone()));
        unwatch_scripthash(&hash);

        // An answer still reaches the request waiting for it.
        let (answer_tx, answer_rx) = async_oneshot::channel();
        pending.lock().await.insert(JsonRpcId::Single("42".into()), answer_tx);
        let answer = json!({"jsonrpc": "2.0", "id": "42", "result": 7});
        super::electrum_rpc_client::electrum_process_json_for_test(answer, &pending).await;
        match answer_rx.await.unwrap() {
            JsonRpcResponseEnum::Single(single) => assert_eq!(single.result, json!(7)),
            JsonRpcResponseEnum::Batch(_) => panic!("a batch answer to a single request"),
        }
    });
}

/// R38.6.4e item 1: two attempts to establish the same server's connection do not both connect; the second waits.
#[test]
fn t38_6_4_6_two_concurrent_attempts_make_one_connection() {
    block_on(async {
        let server = StubServer::start(StubBehavior::Answer).await;
        server.set_accept_delay(ms(150));
        let client = pool_client(&[&server], 1, None, test_timings());
        let slot = client.pool.slots()[0].clone();

        let (first, second) = futures::join!(
            client.pool.ensure_connected(&slot, None),
            client.pool.ensure_connected(&slot, None)
        );
        let (first, second) = (first.unwrap(), second.unwrap());
        assert_eq!(first.0.id(), second.0.id());
        // Exactly one of the two opened it.
        assert_ne!(first.1, second.1);
        assert_eq!(server.accepted(), 1);
        assert_eq!(server.requests("server.version"), 1);
    });
}

/// The establishment is a task of its own, so a caller polled by an executor with no Tokio reactor (a
/// plain `futures` executor, as some tests and callers use) still connects.
#[test]
fn a_request_polled_outside_the_tokio_runtime_still_connects() {
    let server = block_on(StubServer::start(StubBehavior::Answer));
    server.set_result("blockchain.headers.subscribe", json!({"height": 9, "hex": "00"}));
    let client = pool_client(&[&server], 1, None, test_timings());
    let height = futures::executor::block_on(ElectrumClient(client.clone()).get_block_count().compat()).unwrap();
    assert_eq!(height, 9);
}

/// A caller that stops waiting does not leave a half-built connection behind: the attempt finishes and the
/// connection it made is released with the call that asked for it.
#[test]
fn a_connection_made_after_the_call_ended_is_released() {
    block_on(async {
        let (a, b) = (
            StubServer::start(StubBehavior::Answer).await,
            StubServer::start(StubBehavior::Answer).await,
        );
        b.set_accept_delay(ms(250));
        b.set_result("blockchain.block.header", json!("00"));
        let client = pool_client(&[&a, &b], 1, Some(1), test_timings());
        client.pool.run_pass().await;
        assert_eq!(maintained(&client), vec![0]);

        // A server answered, so the call ended while B was still being connected for it.
        a.set_result("blockchain.block.header", json!("00"));
        assert_eq!(ask(&client, "blockchain.block.header").await.unwrap(), a.addr());
        let slot = client.pool.slots()[1].clone();
        let opened = OpenedByCall::default();
        let attempt = client.pool.ensure_connected(&slot, Some(&opened));
        let abandoned = futures::future::select(Box::pin(attempt), Box::pin(Timer::sleep(0.05))).await;
        assert!(matches!(abandoned, futures::future::Either::Right(_)));
        client.pool.release_opened_for_test(&opened);

        wait_until("the late connection to be released", || {
            b.accepted() >= 1 && !snap(&client, 1).connected && snap(&client, 1).suspended_until.is_some()
        })
        .await;
        assert!(!snap(&client, 1).maintained);
    });
}

#[test]
fn a_down_stub_keeps_its_port_reserved_and_refuses_connections() {
    block_on(async {
        let down = StubServer::start(StubBehavior::Down).await;
        let port = down.addr().rsplit(':').next().unwrap().parse::<u16>().unwrap();

        // Nobody else can take the port while the stub lives, and connecting to it is refused.
        let taken = tokio::net::TcpListener::bind(("127.0.0.1", port)).await;
        assert_eq!(taken.unwrap_err().kind(), std::io::ErrorKind::AddrInUse);
        let refused = tokio::net::TcpStream::connect(("127.0.0.1", port)).await;
        assert_eq!(refused.unwrap_err().kind(), std::io::ErrorKind::ConnectionRefused);

        // Switching to a live behavior listens on the same port.
        down.set_behavior(StubBehavior::Answer).await;
        assert!(tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok());

        // Switching back keeps the port closed to connections and unavailable to others.
        down.set_behavior(StubBehavior::Down).await;
        let refused = tokio::net::TcpStream::connect(("127.0.0.1", port)).await;
        assert_eq!(refused.unwrap_err().kind(), std::io::ErrorKind::ConnectionRefused);
        let taken = tokio::net::TcpListener::bind(("127.0.0.1", port)).await;
        assert_eq!(taken.unwrap_err().kind(), std::io::ErrorKind::AddrInUse);

        // Dropping the stub frees the port.
        drop(down);
        assert!(tokio::net::TcpListener::bind(("127.0.0.1", port)).await.is_ok());
    });
}

// ---- T38.6.4.13-R: the server order drawn at activation ---------------------------------------------

/// The injected draw of these tests: rank 0 is the listed server 2, rank 1 the listed server 0, rank 2 the
/// listed server 1.
fn drawn() -> ServerOrder { ServerOrder::Fixed(vec![2, 0, 1]) }

fn three_servers(
    servers: &[StubServer; 3],
    max_connected: Option<usize>,
    order: ServerOrder,
) -> Arc<ElectrumClientImpl> {
    pool_client_ordered(
        servers.iter().map(|server| server.request()).collect(),
        1,
        max_connected,
        test_timings(),
        order,
    )
}

async fn three_answering() -> [StubServer; 3] {
    [
        StubServer::start(StubBehavior::Answer).await,
        StubServer::start(StubBehavior::Answer).await,
        StubServer::start(StubBehavior::Answer).await,
    ]
}

fn ranked_addrs(client: &ElectrumClientImpl) -> Vec<String> {
    client
        .pool_snapshot()
        .iter()
        .map(|server| server.addr.clone())
        .collect()
}

/// R38.6.4b-R item 1: the rank is the position in the drawn order; the maintained set and the routing follow it.
#[test]
fn t38_6_4_13_r_the_maintained_set_and_the_routing_follow_the_drawn_order() {
    block_on(async {
        let servers = three_answering().await;
        let client = three_servers(&servers, Some(2), drawn());
        assert_eq!(ranked_addrs(&client), vec![
            servers[2].addr(),
            servers[0].addr(),
            servers[1].addr()
        ]);

        client.pool.run_pass().await;
        assert_eq!(maintained(&client), vec![0, 1]);
        assert_eq!(snap(&client, 0).addr, servers[2].addr());
        assert_eq!(snap(&client, 1).addr, servers[0].addr());
        assert!(!snap(&client, 2).connected);
        assert_eq!(ask(&client, "server.ping").await.unwrap(), servers[2].addr());
    });
}

/// R38.6.4f: the retry visits the servers in the drawn order.
#[test]
fn t38_6_4_13_r_the_retry_visits_the_servers_in_the_drawn_order() {
    block_on(async {
        let servers = three_answering().await;
        let timings = ElectrumPoolTimings {
            call_budget: ms(1_200),
            ..test_timings()
        };
        let client = pool_client_ordered(
            servers.iter().map(|server| server.request()).collect(),
            1,
            None,
            timings,
            drawn(),
        );
        client.pool.run_pass().await;
        assert_eq!(maintained(&client), vec![0, 1, 2]);
        // Rank 0 (listed server 2) swallows the request: rank 1 (listed server 0) answers, rank 2 is not asked.
        servers[2].set_behavior(StubBehavior::BlackHoleRequests).await;
        assert_eq!(
            ask(&client, "blockchain.block.header").await.unwrap(),
            servers[0].addr()
        );
        assert_eq!(servers[2].requests("blockchain.block.header"), 1);
        assert_eq!(servers[0].requests("blockchain.block.header"), 1);
        assert_eq!(servers[1].requests("blockchain.block.header"), 0);
    });
}

/// R38.6.4c: a recovered server outranking the lowest-ranked member displaces it, by the drawn order.
#[test]
fn t38_6_4_13_r_displacement_follows_the_drawn_order() {
    block_on(async {
        let servers = [
            StubServer::start(StubBehavior::Answer).await,
            StubServer::start(StubBehavior::Answer).await,
            StubServer::start(StubBehavior::Down).await,
        ];
        let client = three_servers(&servers, Some(1), drawn());
        start(&client);
        // Rank 0 (listed server 2) is down: rank 1 (listed server 0) is maintained.
        wait_until("rank 1 maintained", || maintained(&client) == vec![1]).await;

        servers[2].set_behavior(StubBehavior::Answer).await;
        wait_until("the suspension of rank 0 to lapse", || {
            snap(&client, 0).suspended_until.is_none()
        })
        .await;
        client.pool.trigger();
        wait_until("rank 0 displaces rank 1", || maintained(&client) == vec![0]).await;
        assert_eq!(snap(&client, 0).addr, servers[2].addr());
    });
}

/// R38.6.4k-R item 3: the probe keeps the highest-ranked server of those that connected, by the drawn order.
#[test]
fn t38_6_4_13_r_the_probe_keeps_the_highest_ranked_by_the_drawn_order() {
    block_on(async {
        let servers = three_answering().await;
        // Rank 0 (listed server 2) connects last, rank 1 (listed server 0) after rank 2 (listed server 1).
        servers[2].set_accept_delay(ms(120));
        servers[0].set_accept_delay(ms(60));
        let client = three_servers(&servers, Some(1), drawn());
        client.activation_probe().await.unwrap();
        wait_for_probe_end(&client).await;
        assert_eq!(maintained(&client), vec![0]);
        assert_eq!(snap(&client, 0).addr, servers[2].addr());
    });
}

/// R38.6.4b-R item 3: with the switch at listed the listed order is the priority.
#[test]
fn t38_6_4_13_r_with_the_switch_at_listed_the_listed_order_is_kept() {
    block_on(async {
        let servers = three_answering().await;
        let client = three_servers(&servers, Some(2), ServerOrder::Listed);
        assert_eq!(ranked_addrs(&client), vec![
            servers[0].addr(),
            servers[1].addr(),
            servers[2].addr()
        ]);
        client.pool.run_pass().await;
        assert_eq!(maintained(&client), vec![0, 1]);
        assert_eq!(ask(&client, "server.ping").await.unwrap(), servers[0].addr());
    });
}

/// R38.6.4b-R item 1: `random` is the default, `listed` keeps the listed order, `mixed` and `measured` are
/// recognized (R38.6.4l), anything else is `random` with one warning naming the switch and the value.
#[test]
fn t38_6_4_13_r_the_switch_parses_as_stated() {
    let order_of = |conf: Json| parse_server_order(&conf);
    for conf in [json!({}), json!({ "electrum_server_order": "random" })] {
        let (order, warning) = order_of(conf.clone());
        assert!(matches!(order, ServerOrder::Random), "{conf}");
        assert_eq!(warning, None, "{conf}");
    }
    let (order, warning) = order_of(json!({ "electrum_server_order": "listed" }));
    assert!(matches!(order, ServerOrder::Listed));
    assert_eq!(warning, None);
    // The measured values are recognized (R38.6.4b-R item 3, T38.6.4l.1).
    let (order, warning) = order_of(json!({ "electrum_server_order": "mixed" }));
    assert!(matches!(order, ServerOrder::Mixed));
    assert_eq!(warning, None);
    let (order, warning) = order_of(json!({ "electrum_server_order": "measured" }));
    assert!(matches!(order, ServerOrder::Measured));
    assert_eq!(warning, None);

    for (value, shown) in [
        (json!("Mixed"), "Mixed"),
        (json!("Listed"), "Listed"),
        (json!(true), "true"),
        (json!(1), "1"),
    ] {
        let (order, warning) = order_of(json!({ "electrum_server_order": value }));
        assert!(matches!(order, ServerOrder::Random), "{value}");
        assert_eq!(
            warning.as_deref(),
            Some(format!("electrum_server_order \"{shown}\" is not recognized; using \"random\"").as_str()),
            "{value}"
        );
    }
}

/// R38.6.4b-R item 2: two activations draw independently, and a draw keeps every server exactly once.
#[test]
fn t38_6_4_13_r_two_draws_with_different_seeds_differ() {
    use rand::rngs::SmallRng;
    use rand::SeedableRng;
    let listed: Vec<usize> = (0..8).collect();
    let draw = |seed: u64| ServerOrder::shuffle_with(listed.clone(), &mut SmallRng::seed_from_u64(seed));
    let (first, second) = (draw(1), draw(2));
    assert_ne!(first, second);
    for order in [&first, &second] {
        let mut sorted = (*order).clone();
        sorted.sort_unstable();
        assert_eq!(sorted, listed);
    }
    // The same seed draws the same order: the draw is the only source of difference.
    assert_eq!(draw(1), first);

    // The shuffled pool holds every listed server once.
    block_on(async {
        let servers = three_answering().await;
        let client = three_servers(&servers, None, ServerOrder::Random);
        let mut ranked = ranked_addrs(&client);
        ranked.sort();
        let mut expected: Vec<String> = servers.iter().map(|server| server.addr()).collect();
        expected.sort();
        assert_eq!(ranked, expected);
    });
}

/// R38.6.4b-R: over many draws with the real random source each server comes first about equally often.
#[test]
fn t38_6_4_13_r_each_server_comes_first_about_equally_often() {
    let mut rng = common::small_rng();
    let mut first = [0usize; 3];
    for _ in 0..900 {
        first[ServerOrder::shuffle_with(vec![0usize, 1, 2], &mut rng)[0]] += 1;
    }
    for (server, count) in first.iter().enumerate() {
        assert!(
            (200..=400).contains(count),
            "server {server} came first {count} times of 900: {first:?}"
        );
    }
}

// ---- CRD ch.38 R38.6.4a: spreading read-only queries over the maintained sessions ------------------------------------

/// Three answering servers ranked in the listed order, all in the maintained set, with spreading as given.
async fn three_maintained(spread_reads: bool) -> ([StubServer; 3], Arc<ElectrumClientImpl>) {
    let servers = three_answering().await;
    let client = pool_client_configured(
        servers.iter().map(|server| server.request()).collect(),
        1,
        None,
        test_timings(),
        ServerOrder::Listed,
        spread_reads,
    );
    client.pool.run_pass().await;
    assert_eq!(maintained(&client), vec![0, 1, 2]);
    (servers, client)
}

/// Routes `request` and returns the address of the server that answered.
async fn route_to(client: &ElectrumClientImpl, request: JsonRpcRequestEnum) -> Result<String, String> {
    let budget = client.pool.timings.call_budget;
    client.pool.route(request, budget).await.map(|(addr, _)| addr.0)
}

fn batch_of(methods: &[&str]) -> JsonRpcRequestEnum {
    JsonRpcRequestEnum::new_batch(methods.iter().map(|m| rpc_item(m, vec![json!("aa")])).collect())
}

/// How many request messages each server got that are not the connection's own traffic.
fn messages_per_server(servers: &[StubServer; 3]) -> Vec<usize> {
    servers.iter().map(|server| server.request_messages().len()).collect()
}

/// Every method this client sends that is a read-only query in the sense of R38.6.4a item 1 is on the list, and
/// nothing else is: a broadcast, every subscription but the one-shot tip read, the ping and the negotiation.
#[test]
fn t38_6_4_14_the_read_only_methods_are_the_closed_list_of_the_chapter() {
    use super::electrum_pool::is_read_only;
    for method in [
        "blockchain.scripthash.get_balance",
        "blockchain.scripthash.listunspent",
        "blockchain.scripthash.get_history",
        "blockchain.block.header",
        "blockchain.block.headers",
        "blockchain.headers.subscribe",
        "blockchain.transaction.get",
        "blockchain.estimatefee",
        "blockchain.relayfee",
    ] {
        assert!(is_read_only(&rpc(method, vec![])), "{method}");
    }
    for method in [
        "blockchain.transaction.broadcast",
        "blockchain.scripthash.subscribe",
        "blockchain.contract.event.subscribe",
        "server.ping",
        "server.version",
        "blockchain.transaction.get_merkle",
        "blockchain.contract.call",
        "not.a.method",
    ] {
        assert!(!is_read_only(&rpc(method, vec![])), "{method}");
    }
    // A batch is read-only only if every element is.
    assert!(is_read_only(&batch_of(&[
        "blockchain.scripthash.get_balance",
        "blockchain.scripthash.listunspent"
    ])));
    assert!(!is_read_only(&batch_of(&[
        "blockchain.scripthash.get_balance",
        "blockchain.transaction.broadcast"
    ])));
    assert!(!is_read_only(&batch_of(&[])));
}

#[test]
fn t38_6_4_14_reads_go_round_the_maintained_sessions_in_turn() {
    block_on(async {
        let (servers, client) = three_maintained(true).await;
        let mut answered = Vec::new();
        for _ in 0..7 {
            answered.push(ask(&client, "blockchain.block.header").await.unwrap());
        }
        let addr = |i: usize| servers[i].addr();
        assert_eq!(answered, vec![
            addr(0),
            addr(1),
            addr(2),
            addr(0),
            addr(1),
            addr(2),
            addr(0)
        ]);
        assert_eq!(messages_per_server(&servers), vec![3, 2, 2]);
    });
}

#[test]
fn t38_6_4_14_the_other_read_methods_spread_too() {
    block_on(async {
        let (servers, client) = three_maintained(true).await;
        for method in [
            "blockchain.scripthash.get_balance",
            "blockchain.scripthash.listunspent",
            "blockchain.scripthash.get_history",
            "blockchain.headers.subscribe",
            "blockchain.transaction.get",
            "blockchain.estimatefee",
        ] {
            let answered = ask(&client, method).await.unwrap();
            let turn = (0..3).find(|i| servers[*i].addr() == answered).unwrap();
            assert_eq!(servers[turn].requests(method), 1, "{method}");
        }
        // Six reads over three sessions: two each.
        assert_eq!(messages_per_server(&servers), vec![2, 2, 2]);
    });
}

#[test]
fn t38_6_4_14_a_broadcast_a_subscription_and_a_ping_keep_the_priority_routing() {
    block_on(async {
        let (servers, client) = three_maintained(true).await;
        let first = servers[0].addr();
        // A read first, so that the turn has moved on; the others must not follow it.
        assert_eq!(ask(&client, "blockchain.block.header").await.unwrap(), first);
        for _ in 0..3 {
            assert_eq!(ask(&client, "blockchain.transaction.broadcast").await.unwrap(), first);
            assert_eq!(
                route_to(&client, rpc("blockchain.scripthash.subscribe", vec![json!("aa")]))
                    .await
                    .unwrap(),
                first
            );
            assert_eq!(
                route_to(&client, rpc("blockchain.contract.event.subscribe", vec![]))
                    .await
                    .unwrap(),
                first
            );
        }
        assert_eq!(servers[0].requests("blockchain.transaction.broadcast"), 3);
        assert_eq!(servers[0].requests("blockchain.scripthash.subscribe"), 3);
        assert_eq!(servers[1].ordinary_requests() + servers[2].ordinary_requests(), 0);

        // The ping goes over every maintained session at once, as before.
        assert_eq!(ask(&client, "server.ping").await.unwrap(), first);
        assert!(servers.iter().all(|server| server.requests("server.ping") == 1));

        // Those did not move the turn: the next read is the second server's.
        assert_eq!(
            ask(&client, "blockchain.block.header").await.unwrap(),
            servers[1].addr()
        );
    });
}

#[test]
fn t38_6_4_14_a_batch_of_reads_spreads_and_a_batch_with_a_non_read_does_not() {
    block_on(async {
        let (servers, client) = three_maintained(true).await;
        let reads = ["blockchain.scripthash.get_balance", "blockchain.scripthash.listunspent"];
        let mut answered = Vec::new();
        for _ in 0..3 {
            answered.push(route_to(&client, batch_of(&reads)).await.unwrap());
        }
        assert_eq!(answered, servers.iter().map(StubServer::addr).collect::<Vec<_>>());

        // A batch with one broadcast in it keeps the priority routing, three times over.
        let mixed = ["blockchain.scripthash.get_balance", "blockchain.transaction.broadcast"];
        for _ in 0..3 {
            assert_eq!(route_to(&client, batch_of(&mixed)).await.unwrap(), servers[0].addr());
        }
        assert_eq!(messages_per_server(&servers), vec![4, 1, 1]);
    });
}

#[test]
fn t38_6_4_14_a_read_whose_session_fails_falls_back_to_the_other_sessions() {
    block_on(async {
        let (servers, client) = three_maintained(true).await;
        assert_eq!(
            ask(&client, "blockchain.block.header").await.unwrap(),
            servers[0].addr()
        );

        // The second session is next in turn and never answers: the read goes on to the third.
        servers[1].set_behavior(StubBehavior::BlackHoleRequests).await;
        let started = Instant::now();
        assert_eq!(
            ask(&client, "blockchain.block.header").await.unwrap(),
            servers[2].addr()
        );
        assert!(started.elapsed() < ms(3_000), "{:?}", started.elapsed());
        assert_eq!(servers[1].requests("blockchain.block.header"), 1);
        assert!(!snap(&client, 1).maintained, "the failed session is closed and demoted");
    });
    block_on(async {
        // The turn is the second session's, and the second and the third are dead: the read comes round to
        // the first, one attempt at a time, within the call budget (R38.6.4j keeps its arithmetic).
        let (servers, client) = three_maintained(true).await;
        assert_eq!(
            ask(&client, "blockchain.block.header").await.unwrap(),
            servers[0].addr()
        );
        servers[1].set_behavior(StubBehavior::BlackHoleRequests).await;
        servers[2].set_behavior(StubBehavior::BlackHoleRequests).await;
        let started = Instant::now();
        let answered = ask(&client, "blockchain.block.header").await.unwrap();
        assert_eq!(answered, servers[0].addr());
        assert!(
            started.elapsed() < client.pool.timings.call_budget,
            "{:?}",
            started.elapsed()
        );
        assert_eq!(servers[1].requests("blockchain.block.header"), 1);
        assert_eq!(servers[2].requests("blockchain.block.header"), 1);
    });
}

#[test]
fn t38_6_4_14_with_spreading_off_every_read_goes_to_the_first_ranked_server() {
    block_on(async {
        let (servers, client) = three_maintained(false).await;
        for _ in 0..6 {
            assert_eq!(
                ask(&client, "blockchain.block.header").await.unwrap(),
                servers[0].addr()
            );
        }
        assert_eq!(
            route_to(&client, batch_of(&["blockchain.scripthash.get_balance"]))
                .await
                .unwrap(),
            servers[0].addr()
        );
        assert_eq!(messages_per_server(&servers), vec![7, 0, 0]);
    });
}

#[test]
fn t38_6_4_14_a_directed_request_is_not_affected() {
    block_on(async {
        let (servers, client) = three_maintained(true).await;
        for _ in 0..3 {
            let (addr, _) = client
                .pool
                .route_directed(rpc("blockchain.block.header", vec![]), &servers[2].addr())
                .await
                .unwrap();
            assert_eq!(addr.0, servers[2].addr());
        }
        assert_eq!(messages_per_server(&servers), vec![0, 0, 3]);
        // Nor did it use the turn of the spread reads.
        assert_eq!(
            ask(&client, "blockchain.block.header").await.unwrap(),
            servers[0].addr()
        );
    });
}

#[test]
fn t38_6_4_14_the_switch_in_mm2_json_defaults_to_on_and_false_turns_it_off() {
    use super::electrum_pool::parse_spread_reads;
    assert_eq!(parse_spread_reads(&json!({})), (true, None));
    assert_eq!(
        parse_spread_reads(&json!({"electrum_spread_reads": true})),
        (true, None)
    );
    assert_eq!(
        parse_spread_reads(&json!({"electrum_spread_reads": false})),
        (false, None)
    );
    // Not a boolean: spreading stays on and one warning names the key.
    let (spread, warning) = parse_spread_reads(&json!({"electrum_spread_reads": "false"}));
    assert!(spread);
    assert!(warning.unwrap().contains("electrum_spread_reads"));
    // The pool option defaults to on.
    assert!(ElectrumPoolOptions::default().spread_reads);
}

// ---- CRD ch.38 R38.6.4a items 6-8: reads over the near sessions ------------------------------------------------------

/// The factor the pool tests of the near sessions set, as the chapter's cases state it. It is set on purpose and is
/// not the default of the switch, which is the constant `NEAR_FACTOR_DEFAULT` (D38.6.4a.1).
const TEST_NEAR_FACTOR: f64 = 2.;
const READ: &str = "blockchain.block.header";
/// The delays are real wall-clock waits, wide enough that scheduling jitter on a loaded machine cannot move a
/// stub across the limit: the near stubs stay within a factor of two of each other with 50 ms to spare, the far
/// one is eight times the fast ones.
const FAST_MS: u64 = 50;
const SLOW_MS: u64 = 400;

fn near_timings() -> ElectrumPoolTimings {
    ElectrumPoolTimings {
        request: ms(2_000),
        call_budget: ms(10_000),
        ..lenient()
    }
}

/// Three answering stubs, listed in this order, whose reads and pings take `delays` (milliseconds); the pool ranks
/// them by `order`, spreads reads as `spread_reads` says with the near factor `factor` and the default offset, and
/// has all three maintained.
async fn near_pool(
    delays: [u64; 3],
    order: ServerOrder,
    spread_reads: bool,
    factor: f64,
    timings: ElectrumPoolTimings,
) -> ([StubServer; 3], Arc<ElectrumClientImpl>) {
    near_pool_at(delays, order, spread_reads, factor, NEAR_OFFSET_DEFAULT_MS, timings).await
}

/// Like [`near_pool`], with the near offset `offset_ms` as well.
async fn near_pool_at(
    delays: [u64; 3],
    order: ServerOrder,
    spread_reads: bool,
    factor: f64,
    offset_ms: f64,
    timings: ElectrumPoolTimings,
) -> ([StubServer; 3], Arc<ElectrumClientImpl>) {
    let servers = three_answering().await;
    for (server, delay) in servers.iter().zip(delays) {
        server.set_reply_delay(READ, ms(delay));
        server.set_reply_delay("server.ping", ms(delay));
    }
    let client = pool_client_spreading(
        servers.iter().map(|server| server.request()).collect(),
        1,
        None,
        timings,
        order,
        spread_reads,
        factor,
        offset_ms,
    );
    client.pool.run_pass().await;
    assert_eq!(maintained(&client).len(), 3);
    (servers, client)
}

/// One ping over every maintained session gives each its first sample, whatever the delays are.
async fn warm_up(servers: &[StubServer; 3], client: &ElectrumClientImpl) {
    client.pool.ping_maintained().await;
    for server in servers {
        assert_eq!(server.requests(READ), 0);
        assert_eq!(snap_of(client, server).samples, 1);
    }
}

/// T38.6.4a.2: with two fast stubs and a far one, each with a sample, 30 reads in a row are all answered by the two
/// fast ones, in turn.
#[test]
fn t38_6_4a_2_reads_avoid_a_far_session() {
    block_on(async {
        let (servers, client) = near_pool(
            [FAST_MS, FAST_MS, SLOW_MS],
            ServerOrder::Listed,
            true,
            TEST_NEAR_FACTOR,
            near_timings(),
        )
        .await;
        warm_up(&servers, &client).await;
        let mut answered = Vec::new();
        for _ in 0..30 {
            answered.push(ask(&client, READ).await.unwrap());
        }
        let count = |server: &StubServer| answered.iter().filter(|addr| **addr == server.addr()).count();
        assert_eq!(count(&servers[2]), 0, "{answered:?}");
        assert!((14..=16).contains(&count(&servers[0])), "{answered:?}");
        assert!((14..=16).contains(&count(&servers[1])), "{answered:?}");
        // The far one was not asked at all: the warm-up is a ping.
        assert_eq!(servers[2].requests(READ), 0);
        // The turn goes to the near sessions one after the other.
        assert!(answered.windows(2).all(|pair| pair[0] != pair[1]), "{answered:?}");
    });
}

/// T38.6.4a.8: with two maintained stubs, a fast one and a far one, each with a sample, the floor of two near
/// sessions keeps both in turn: 30 reads in a row are answered by both, 15 within one each.
#[test]
fn t38_6_4a_8_two_sessions_keep_both_in_turn() {
    block_on(async {
        let servers = [
            StubServer::start(StubBehavior::Answer).await,
            StubServer::start(StubBehavior::Answer).await,
        ];
        for (server, delay) in servers.iter().zip([FAST_MS, SLOW_MS]) {
            server.set_reply_delay(READ, ms(delay));
            server.set_reply_delay("server.ping", ms(delay));
        }
        let client = pool_client_spreading(
            servers.iter().map(|server| server.request()).collect(),
            1,
            None,
            near_timings(),
            ServerOrder::Listed,
            true,
            TEST_NEAR_FACTOR,
            NEAR_OFFSET_DEFAULT_MS,
        );
        client.pool.run_pass().await;
        assert_eq!(maintained(&client).len(), 2);
        // One ping over both gives each its first sample, whatever the delays are.
        client.pool.ping_maintained().await;
        for server in &servers {
            assert_eq!(snap_of(&client, server).samples, 1);
        }
        let mut answered = Vec::new();
        for _ in 0..30 {
            answered.push(ask(&client, READ).await.unwrap());
        }
        let count = |server: &StubServer| answered.iter().filter(|addr| **addr == server.addr()).count();
        assert!((14..=16).contains(&count(&servers[0])), "{answered:?}");
        assert!((14..=16).contains(&count(&servers[1])), "{answered:?}");
        // The turn goes to the two sessions one after the other.
        assert!(answered.windows(2).all(|pair| pair[0] != pair[1]), "{answered:?}");
    });
}

/// T38.6.4a.3: a far session is still a fallback: with both near stubs failing the read, the far one answers it,
/// after the near ones were tried.
#[test]
fn t38_6_4a_3_a_far_session_is_still_a_fallback() {
    block_on(async {
        let (servers, client) = near_pool(
            [FAST_MS, FAST_MS, SLOW_MS],
            ServerOrder::Listed,
            true,
            TEST_NEAR_FACTOR,
            near_timings(),
        )
        .await;
        warm_up(&servers, &client).await;
        servers[0].set_behavior(StubBehavior::BlackHoleRequests).await;
        servers[1].set_behavior(StubBehavior::BlackHoleRequests).await;
        assert_eq!(ask(&client, READ).await.unwrap(), servers[2].addr());
        // Both near sessions were asked first (the failed ones are closed and demoted), the far one last.
        assert_eq!(servers[0].requests(READ), 1);
        assert_eq!(servers[1].requests(READ), 1);
        assert_eq!(servers[2].requests(READ), 1);
        assert!(!snap(&client, 0).maintained && !snap(&client, 1).maintained);
    });
}

/// T38.6.4a.4: a recovered session comes back: once the slow stub answers fast and the pings have moved its
/// smoothed latency within the limit, it answers reads again, and not before.
#[test]
fn t38_6_4a_4_a_recovered_session_comes_back() {
    block_on(async {
        let timings = ElectrumPoolTimings {
            ping_interval: ms(100),
            ..near_timings()
        };
        let (servers, client) = near_pool(
            [FAST_MS, FAST_MS, SLOW_MS],
            ServerOrder::Listed,
            true,
            TEST_NEAR_FACTOR,
            timings,
        )
        .await;
        warm_up(&servers, &client).await;
        for _ in 0..4 {
            assert_ne!(ask(&client, READ).await.unwrap(), servers[2].addr());
        }

        // The stub recovers. Its figure is still the old one, so the first read after that avoids it too.
        servers[2].set_reply_delay(READ, ms(FAST_MS));
        servers[2].set_reply_delay("server.ping", ms(FAST_MS));
        assert_ne!(ask(&client, READ).await.unwrap(), servers[2].addr());
        client.start_pool(true);

        let started = Instant::now();
        let mut avoided = 0;
        loop {
            let answer = ask(&client, READ).await.unwrap();
            if answer == servers[2].addr() {
                break;
            }
            avoided += 1;
            assert!(started.elapsed() < ms(20_000), "the recovered session never came back");
            Timer::sleep(0.06).await;
        }
        // It took several pings to move the average, so some reads went to the fast ones first (a small
        // allowance for the order the answers and the samples arrive in) ...
        assert!(avoided >= 3, "{avoided}");
        // ... and by the time it answered, its figure was within the limit.
        let figures: Vec<f64> = servers
            .iter()
            .map(|server| snap_of(&client, server).smoothed_ms.unwrap())
            .collect();
        let fastest = figures.iter().copied().fold(f64::INFINITY, f64::min);
        let limit = (TEST_NEAR_FACTOR * fastest).max(fastest + NEAR_OFFSET_DEFAULT_MS);
        assert!(figures[2] <= limit + 20., "{figures:?}");
    });
}

/// T38.6.4a.5: under `listed` and under `random`, requests and pings give each maintained session a smoothed
/// latency. The tip checks are off here (R38.6.4o item 11): with them on, a header request goes out on joining under
/// every order (R38.6.4o item 3), which is not a sample (T38.6.4l.1, T38.6.4o.3).
#[test]
fn t38_6_4a_5_samples_under_random_and_listed() {
    block_on(async {
        for order in [ServerOrder::Listed, ServerOrder::Random] {
            let (servers, client) = near_pool([5, 5, 5], order.clone(), true, TEST_NEAR_FACTOR, near_timings()).await;
            // Joining through the pass sent no header request and left no figure.
            for server in &servers {
                assert_eq!(server.requests(HEADERS), 0, "{order:?}");
                assert_eq!(snap_of(&client, server).samples, 0, "{order:?}");
                assert_eq!(snap_of(&client, server).smoothed_ms, None, "{order:?}");
            }
            // A ping over every maintained session gives each a first sample ...
            warm_up(&servers, &client).await;
            assert!(servers
                .iter()
                .all(|server| snap_of(&client, server).smoothed_ms.is_some()));
            // ... and another ping gives each another.
            client.pool.ping_maintained().await;
            for server in &servers {
                assert_eq!(snap_of(&client, server).samples, 2, "{order:?}");
                // The warm-up was a ping, and this is the second.
                assert_eq!(server.requests("server.ping"), 2, "{order:?}");
            }
            // Still no header request anywhere, no round, nothing judged.
            Timer::sleep(0.2).await;
            for server in &servers {
                assert_eq!(server.requests(HEADERS), 0, "{order:?}");
                assert!(
                    !methods_received(server).iter().any(|method| method == HEADERS),
                    "{order:?}"
                );
            }
            assert!(client.pool.remeasure_rounds().is_empty(), "{order:?}");
            assert!(client.pool_snapshot().iter().all(|state| !state.poor), "{order:?}");
        }

        // With the default `electrum_tip_checks` (true) exactly one header request goes out on joining, under both
        // orders (R38.6.4o item 3); its answer is the first sample (R38.6.4a item 7), and nothing is judged.
        for order in [ServerOrder::Listed, ServerOrder::Random] {
            let servers = [stub_at(5, 100).await, stub_at(5, 100).await, stub_at(5, 100).await];
            let client = checked_pool(
                &[&servers[0], &servers[1], &servers[2]],
                PoolSetup {
                    order: order.clone(),
                    ..PoolSetup::new(TICKER)
                },
                near_timings(),
                script_draw(&[], 0.5),
            );
            client.pool.run_pass().await;
            wait_until("the header requests", || {
                servers.iter().all(|server| server.requests(HEADERS) == 1)
            })
            .await;
            Timer::sleep(0.2).await;
            for server in &servers {
                assert_eq!(server.requests(HEADERS), 1, "{order:?}");
                assert_eq!(snap_of(&client, server).samples, 1, "{order:?}");
            }
            assert!(client.pool.remeasure_rounds().is_empty(), "{order:?}");
        }
    });
}

/// T38.6.4a.6: the values of `electrum_spread_near_factor`.
#[test]
fn t38_6_4a_6_the_near_factor_switch_parses_as_stated() {
    use super::electrum_measure::{near_sessions, NEAR_FACTOR_DEFAULT};
    use super::electrum_pool::parse_spread_near_factor;
    // Absent: the default, no warning.
    assert_eq!(parse_spread_near_factor(&json!({})), (NEAR_FACTOR_DEFAULT, None));
    assert_eq!(
        parse_spread_near_factor(&json!({"electrum_spread_reads": false})),
        (NEAR_FACTOR_DEFAULT, None)
    );
    // A non-negative number is used as it is.
    for (value, expected) in [
        (json!(0), 0.),
        (json!(0.0), 0.),
        (json!(0.5), 0.5),
        (json!(1), 1.),
        (json!(2), 2.),
        (json!(3.5), 3.5),
    ] {
        assert_eq!(
            parse_spread_near_factor(&json!({ "electrum_spread_near_factor": value })),
            (expected, None),
            "{value}"
        );
    }
    // `0` makes every maintained session near, and `0.5` means `1`.
    let figures = [Some(25.), Some(27.), Some(32.), Some(108.), Some(230.), Some(266.)];
    assert_eq!(near_sessions(&figures, 0., NEAR_OFFSET_DEFAULT_MS), vec![true; 6]);
    assert_eq!(
        near_sessions(&figures, 0.5, NEAR_OFFSET_DEFAULT_MS),
        near_sessions(&figures, 1., NEAR_OFFSET_DEFAULT_MS)
    );
    // Anything else: the default, with one warning that names the key and the value.
    for (value, shown) in [
        (json!("2"), "\"2\""),
        (json!("fast"), "\"fast\""),
        (json!(-1), "-1"),
        (json!(-0.5), "-0.5"),
        (json!(null), "null"),
        (json!(true), "true"),
        (json!([2]), "[2]"),
    ] {
        let (factor, warning) = parse_spread_near_factor(&json!({ "electrum_spread_near_factor": value }));
        assert_eq!(factor, NEAR_FACTOR_DEFAULT, "{value}");
        let warning = warning.unwrap_or_else(|| panic!("no warning for {value}"));
        assert!(warning.contains("electrum_spread_near_factor"), "{warning}");
        assert!(warning.contains(shown), "{warning}");
    }
    // The pool option defaults to the constant.
    assert_eq!(ElectrumPoolOptions::default().near_factor, NEAR_FACTOR_DEFAULT);
}

/// T38.6.4a.6: the values of `electrum_spread_near_offset_ms`.
#[test]
fn t38_6_4a_6_the_near_offset_switch_parses_as_stated() {
    use super::electrum_pool::parse_spread_near_offset_ms;
    // Absent: the default, no warning; the other switches do not count.
    assert_eq!(parse_spread_near_offset_ms(&json!({})), (NEAR_OFFSET_DEFAULT_MS, None));
    assert_eq!(
        parse_spread_near_offset_ms(&json!({"electrum_spread_near_factor": 3})),
        (NEAR_OFFSET_DEFAULT_MS, None)
    );
    // A non-negative number is used as it is: `0` leaves the factor to decide, `50` admits 50 ms.
    for (value, expected) in [
        (json!(0), 0.),
        (json!(0.0), 0.),
        (json!(50), 50.),
        (json!(12.5), 12.5),
        (json!(1000), 1000.),
    ] {
        assert_eq!(
            parse_spread_near_offset_ms(&json!({ "electrum_spread_near_offset_ms": value })),
            (expected, None),
            "{value}"
        );
    }
    // Anything else: the default, with one warning that names the key and the value.
    for (value, shown) in [
        (json!("50"), "\"50\""),
        (json!("fast"), "\"fast\""),
        (json!(-1), "-1"),
        (json!(-0.5), "-0.5"),
        (json!(null), "null"),
        (json!(true), "true"),
        (json!([50]), "[50]"),
        (json!({"ms": 50}), "{\"ms\":50}"),
    ] {
        let (offset, warning) = parse_spread_near_offset_ms(&json!({ "electrum_spread_near_offset_ms": value }));
        assert_eq!(offset, NEAR_OFFSET_DEFAULT_MS, "{value}");
        let warning = warning.unwrap_or_else(|| panic!("no warning for {value}"));
        assert!(warning.contains("electrum_spread_near_offset_ms"), "{warning}");
        assert!(warning.contains(shown), "{warning}");
    }
    // The pool option defaults to the constant.
    assert_eq!(ElectrumPoolOptions::default().near_offset_ms, NEAR_OFFSET_DEFAULT_MS);
}

/// T38.6.4a.6: the offset reaches the pool: with the factor at 2 and an offset wide enough, the far session
/// is near and takes its turn.
#[test]
fn t38_6_4a_6_a_wide_offset_gives_every_session_its_turn() {
    block_on(async {
        let (servers, client) = near_pool_at(
            [FAST_MS, FAST_MS, SLOW_MS],
            ServerOrder::Listed,
            true,
            TEST_NEAR_FACTOR,
            10_000.,
            near_timings(),
        )
        .await;
        warm_up(&servers, &client).await;
        for _ in 0..9 {
            ask(&client, READ).await.unwrap();
        }
        assert_eq!(
            servers.iter().map(|server| server.requests(READ)).collect::<Vec<_>>(),
            vec![3, 3, 3]
        );
    });
}

/// T38.6.4a.6: the factor reaches the pool: `0` makes the far session near, so it takes its turn.
#[test]
fn t38_6_4a_6_a_factor_of_zero_gives_every_session_its_turn() {
    block_on(async {
        let (servers, client) = near_pool(
            [FAST_MS, FAST_MS, SLOW_MS],
            ServerOrder::Listed,
            true,
            0.,
            near_timings(),
        )
        .await;
        for _ in 0..9 {
            ask(&client, READ).await.unwrap();
        }
        assert_eq!(
            servers.iter().map(|server| server.requests(READ)).collect::<Vec<_>>(),
            vec![3, 3, 3]
        );
    });
}

/// T38.6.4a.6: with `electrum_spread_reads` `false`, reads follow the priority order whatever the factor.
#[test]
fn t38_6_4a_6_with_spreading_off_the_factor_has_no_effect() {
    block_on(async {
        for factor in [0., TEST_NEAR_FACTOR] {
            let (servers, client) = near_pool(
                [FAST_MS, FAST_MS, SLOW_MS],
                ServerOrder::Listed,
                false,
                factor,
                near_timings(),
            )
            .await;
            for _ in 0..6 {
                assert_eq!(ask(&client, READ).await.unwrap(), servers[0].addr(), "factor {factor}");
            }
            assert_eq!(servers[1].ordinary_requests() + servers[2].ordinary_requests(), 0);
        }
    });
}

/// T38.6.4a.7: writes keep their routing: with the far stub first in priority, a broadcast and the subscriptions go
/// to it, while the reads avoid it.
#[test]
fn t38_6_4a_7_writes_keep_their_routing() {
    block_on(async {
        let (servers, client) = near_pool(
            [SLOW_MS, FAST_MS, FAST_MS],
            ServerOrder::Listed,
            true,
            TEST_NEAR_FACTOR,
            near_timings(),
        )
        .await;
        warm_up(&servers, &client).await;
        // Every ordinary request is a sample (R38.6.4a item 7), the writes included: the far stub is as slow
        // for them as for the reads, so they do not bring it near.
        for method in [
            "blockchain.transaction.broadcast",
            "blockchain.scripthash.subscribe",
            "blockchain.contract.event.subscribe",
        ] {
            servers[0].set_reply_delay(method, ms(SLOW_MS));
        }
        let first = servers[0].addr();
        for _ in 0..3 {
            assert_eq!(ask(&client, "blockchain.transaction.broadcast").await.unwrap(), first);
        }
        assert_eq!(
            route_to(&client, rpc("blockchain.scripthash.subscribe", vec![json!("aa")]))
                .await
                .unwrap(),
            first
        );
        assert_eq!(
            route_to(&client, rpc("blockchain.contract.event.subscribe", vec![]))
                .await
                .unwrap(),
            first
        );
        assert_eq!(servers[0].requests("blockchain.transaction.broadcast"), 3);
        assert_eq!(servers[0].requests("blockchain.scripthash.subscribe"), 1);
        assert_eq!(servers[1].ordinary_requests() + servers[2].ordinary_requests(), 0);
        // The reads still avoid it.
        for _ in 0..6 {
            assert_ne!(ask(&client, READ).await.unwrap(), first);
        }
        assert_eq!(servers[0].requests(READ), 0);
    });
}

// ---- CRD ch.38 R38.6.4l: the measured server order ---------------------------------------------------------------

const HEADERS: &str = "blockchain.headers.subscribe";

/// A random source for the pool: first `numbers`, then `fallback` for ever. A draw of 0.99 leaves a shuffle as
/// it is, and a draw of 0 takes the first of the places left.
fn script_draw(numbers: &[f64], fallback: f64) -> impl FnMut() -> f64 + Send + 'static {
    let mut numbers = numbers.to_vec().into_iter();
    move || numbers.next().unwrap_or(fallback)
}

/// The numbers that leave the first shuffle of `servers` servers in the listed order.
fn identity(servers: usize) -> Vec<f64> { vec![0.99; servers.saturating_sub(1)] }

/// A pool over `servers` with a measured (or other) `order`, whose random choices come from `draw`. The tip checks of
/// R38.6.4o are off, so that these tests stay about R38.6.4l as it was before them.
fn measured_pool(
    servers: &[&StubServer],
    max_connected: Option<usize>,
    order: ServerOrder,
    timings: ElectrumPoolTimings,
    block_interval: Option<Duration>,
    draw: impl FnMut() -> f64 + Send + 'static,
) -> Arc<ElectrumClientImpl> {
    checked_pool(
        servers,
        PoolSetup {
            max_connected,
            order,
            block_interval,
            spread_reads: false,
            tip_checks: false,
            ..PoolSetup::new(TICKER)
        },
        timings,
        draw,
    )
}

/// How a pool of the tests of R38.6.4l and R38.6.4o is set up.
struct PoolSetup {
    ticker: &'static str,
    max_connected: Option<usize>,
    order: ServerOrder,
    /// Whether read-only queries are spread (R38.6.4a item 5).
    spread_reads: bool,
    near_factor: f64,
    block_interval: Option<Duration>,
    /// The `electrum_tip_checks` switch (R38.6.4o item 11).
    tip_checks: bool,
}

impl PoolSetup {
    /// The listed order, spreading over every maintained session, the tip checks on and no `avg_blocktime`.
    fn new(ticker: &'static str) -> PoolSetup {
        PoolSetup {
            ticker,
            max_connected: None,
            order: ServerOrder::Listed,
            spread_reads: true,
            near_factor: 0.,
            block_interval: None,
            tip_checks: true,
        }
    }
}

/// A pool over `servers` as `setup` says, whose random choices come from `draw`.
fn checked_pool(
    servers: &[&StubServer],
    setup: PoolSetup,
    timings: ElectrumPoolTimings,
    draw: impl FnMut() -> f64 + Send + 'static,
) -> Arc<ElectrumClientImpl> {
    let mut client = ElectrumClientImpl::new(setup.ticker.into(), vec![]);
    client.configure_pool(ElectrumPoolOptions {
        min_connected: 1,
        max_connected: setup.max_connected,
        client_name: Some(CLIENT_NAME.into()),
        timings,
        order: setup.order,
        spread_reads: setup.spread_reads,
        near_factor: setup.near_factor,
        near_offset_ms: NEAR_OFFSET_DEFAULT_MS,
        block_interval: setup.block_interval,
        tip_checks: setup.tip_checks,
    });
    client.pool.set_draw_for_test(draw);
    let requests: Vec<ElectrumRpcRequest> = servers.iter().map(|server| server.request()).collect();
    let _ = client.add_servers(&requests);
    Arc::new(client)
}

/// Runs the activation probe of `client` to its end and starts the pool's background tasks.
async fn activate_and_start(client: &Arc<ElectrumClientImpl>) {
    client.activation_probe().await.unwrap();
    wait_for_probe_end(client).await;
    client.start_pool(false);
}

fn snap_of(client: &ElectrumClientImpl, server: &StubServer) -> ServerStateSnapshot {
    client
        .pool_snapshot()
        .into_iter()
        .find(|state| state.addr == server.addr())
        .expect("the server is in the pool")
}

fn rank_of(client: &ElectrumClientImpl) -> Vec<String> { ranked_addrs(client) }

/// The methods a stub received, in order, of every message.
fn methods_received(server: &StubServer) -> Vec<String> {
    server
        .messages()
        .iter()
        .flat_map(|message| match message {
            Json::Array(batch) => batch.clone(),
            single => vec![single.clone()],
        })
        .map(|request| request["method"].as_str().unwrap_or_default().to_owned())
        .collect()
}

/// The test timings with a stale limit that never strikes: for the tests that are not about P1 of a lone
/// connection, whose stub never pushes a header.
fn quiet_timings() -> ElectrumPoolTimings {
    ElectrumPoolTimings {
        stale_floor: ms(60_000),
        ..test_timings()
    }
}

/// Timings for the tests whose latencies are real wall-clock delays: the bounds are wide enough that a loaded
/// machine cannot make a request time out, push a server over the latency limit or let a poor mark lapse early,
/// so the tests compare delays that differ by hundreds of milliseconds and cannot flip on scheduling jitter.
fn lenient() -> ElectrumPoolTimings {
    ElectrumPoolTimings {
        establishment: ms(5_000),
        request: ms(6_000),
        call_budget: ms(15_000),
        // A server released early in a slow probe stays suspended until the probe ends.
        suspension_base: ms(2_000),
        suspension_cap: ms(8_000),
        latency_floor: ms(10_000),
        poor_demotion: ms(4_000),
        ..quiet_timings()
    }
}

/// Runs `asks` requests of `method` at once and waits for all of them.
async fn ask_together(client: &ElectrumClientImpl, method: &str, asks: usize) {
    let answers = futures::future::join_all((0..asks).map(|_| ask(client, method))).await;
    for answer in answers {
        answer.unwrap();
    }
}

async fn stub_at(delay: u64, tip: u64) -> StubServer {
    let server = StubServer::start(StubBehavior::Answer).await;
    server.set_reply_delay(HEADERS, ms(delay));
    server.set_tip(tip);
    server
}

/// T38.6.4l.1: under `random` and `listed` no stub gets a header request as part of its probe attempt, each gets
/// exactly one after it joins the maintained set (R38.6.4o item 3), and no re-measure round and no trigger runs.
#[test]
fn t38_6_4l_1_random_and_listed_measure_nothing() {
    block_on(async {
        for order in [ServerOrder::Random, ServerOrder::Listed] {
            let servers = [stub_at(5, 100).await, stub_at(5, 100).await, stub_at(5, 100).await];
            let client = checked_pool(
                &[&servers[0], &servers[1], &servers[2]],
                PoolSetup {
                    order: order.clone(),
                    spread_reads: false,
                    ..PoolSetup::new(TICKER)
                },
                test_timings(),
                script_draw(&[], 0.5),
            );
            activate_and_start(&client).await;
            // A slow request is a sample of the smoothed latency (R38.6.4a item 7), but nothing is judged by it
            // and a re-measure round does not run.
            servers[0].set_reply_delay("blockchain.block.header", ms(150));
            for _ in 0..6 {
                ask(&client, "blockchain.block.header").await.unwrap();
            }
            client.pool.run_remeasure_round().await;
            Timer::sleep(0.2).await;
            // The one header request of each stub is the one on joining, right after `server.version`.
            for server in &servers {
                assert_eq!(server.requests(HEADERS), 1, "{order:?}");
                assert_eq!(methods_received(server)[..2], ["server.version", HEADERS], "{order:?}");
            }
            assert!(client.pool.remeasure_rounds().is_empty(), "{order:?}");
            // The six reads went to the first-ranked server and are six samples (the delay of one server only
            // is not the point: the rank, under `random`, is a draw); the answer to each stub's header request on
            // joining is a sample too (R38.6.4a item 7), three more.
            assert_eq!(
                client.pool_snapshot().iter().map(|state| state.samples).sum::<u32>(),
                9,
                "{order:?}"
            );
            assert!(client.pool_snapshot().iter().all(|state| !state.poor));
        }
    });
}

/// T38.6.4l.2: each stub gets exactly one header request in the probe, right after `server.version`; the figures
/// are the stubs' reply delays and tips; a stub that does not answer has no measurement.
#[test]
fn t38_6_4l_2_the_activation_round_measures_each_server_once() {
    block_on(async {
        let (a, b) = (stub_at(100, 100).await, stub_at(400, 100).await);
        let c = StubServer::start(StubBehavior::BlackHoleRequests).await;
        let client = measured_pool(
            &[&a, &b, &c],
            None,
            ServerOrder::Mixed,
            ElectrumPoolTimings {
                request: ms(1_500),
                ..lenient()
            },
            None,
            // The first shuffle leaves the listed order; the weighted draw then takes the first place left.
            script_draw(&[0.99, 0.99, 0., 0., 0.], 0.),
        );
        client.activation_probe().await.unwrap();
        wait_for_probe_end(&client).await;
        for server in [&a, &b, &c] {
            assert_eq!(server.requests(HEADERS), 1);
            assert_eq!(methods_received(server)[..2], ["server.version", HEADERS]);
        }
        let (sa, sb, sc) = (snap_of(&client, &a), snap_of(&client, &b), snap_of(&client, &c));
        // The figure is the reply delay plus the round trip, which a loaded machine can stretch: it is never below
        // the delay and, being answered, below the request bound.
        let in_range = |ms_measured: Option<f64>, delay: f64| {
            ms_measured.map_or(false, |measured| measured >= delay && measured < 1_500.)
        };
        assert!(in_range(sa.activation_ms, 100.), "{sa:?}");
        assert!(in_range(sb.activation_ms, 400.), "{sb:?}");
        assert_eq!(sc.activation_ms, None, "{sc:?}");
        assert_eq!((sa.height, sb.height), (Some(100), Some(100)));
        // The rank: the measured servers first, the silent one after them.
        assert_eq!(rank_of(&client), vec![a.addr(), b.addr(), c.addr()]);
    });
}

/// T38.6.4l.3 and T38.6.4l.6: with tips at H, H - 1 and H - 2 the third server ranks last; the measured rank
/// takes effect only when the probe ends, and the set, the routing and the displacement follow it afterwards.
#[test]
fn t38_6_4l_6_the_measured_rank_takes_effect_when_the_probe_ends() {
    block_on(async {
        // Listed: s0 answers its header request last (800 ms), s1 after 300 ms, s2 after 10 ms.
        let servers = [stub_at(800, 100).await, stub_at(300, 100).await, stub_at(10, 100).await];
        let client = measured_pool(
            &[&servers[0], &servers[1], &servers[2]],
            Some(1),
            ServerOrder::Measured,
            lenient(),
            None,
            script_draw(&identity(3), 0.5),
        );
        let listed: Vec<String> = servers.iter().map(|server| server.addr()).collect();
        let started = Instant::now();
        client.activation_probe().await.unwrap();
        // The build returns on the first establishment, long before the slowest header answer, and the rank is
        // still the uniform draw.
        assert!(started.elapsed() < ms(800), "{:?}", started.elapsed());
        assert_eq!(rank_of(&client), listed);
        assert_eq!(snap_of(&client, &servers[0]).activation_ms, None);

        wait_for_probe_end(&client).await;
        assert!(started.elapsed() >= ms(800), "the probe ended before the last answer");
        let measured_rank = vec![servers[2].addr(), servers[1].addr(), servers[0].addr()];
        assert_eq!(rank_of(&client), measured_rank);
        // The probe kept the highest-ranked server of the uniform draw (s0); the measured rank reaches the
        // set through the next pass, once the suspension of the released servers has lapsed.
        assert_eq!(snap_of(&client, &servers[0]).maintained, true);
        client.start_pool(false);
        wait_until("the suspension of the best server to lapse", || {
            snap_of(&client, &servers[2]).suspended_until.is_none()
        })
        .await;
        client.pool.trigger();
        wait_until("the best server displaces the first", || maintained(&client) == vec![0]).await;
        assert_eq!(snap(&client, 0).addr, servers[2].addr());
        assert_eq!(ask(&client, "server.ping").await.unwrap(), servers[2].addr());

        // A re-activation measures again.
        let again = measured_pool(
            &[&servers[0], &servers[1], &servers[2]],
            Some(1),
            ServerOrder::Measured,
            lenient(),
            None,
            script_draw(&identity(3), 0.5),
        );
        again.activation_probe().await.unwrap();
        wait_for_probe_end(&again).await;
        // One request per server for each activation round; the best server also joined the first pool through
        // a maintenance pass with no header request sent over it, so it sent one more on joining (T38.6.4l.15).
        for (index, server) in servers.iter().enumerate() {
            assert_eq!(
                server.requests(HEADERS),
                if index == 2 { 3 } else { 2 },
                "server {index}"
            );
        }
    });
}

/// T38.6.4l.3: with tips at H, H and H - 2 the third server (the fastest) is not eligible and ranks after every
/// eligible one; with tips at H, H - 1 and H - 2 the agreed tip is H - 1 and all three are eligible, so the fastest
/// ranks first.
#[test]
fn t38_6_4l_3_a_server_two_blocks_behind_the_agreed_tip_ranks_last() {
    block_on(async {
        // The lagging server is the fastest.
        let servers = [stub_at(200, 100).await, stub_at(200, 100).await, stub_at(5, 98).await];
        let client = measured_pool(
            &[&servers[0], &servers[1], &servers[2]],
            None,
            ServerOrder::Measured,
            lenient(),
            None,
            script_draw(&identity(3), 0.5),
        );
        client.activation_probe().await.unwrap();
        wait_for_probe_end(&client).await;
        let rank = rank_of(&client);
        assert_eq!(rank[2], servers[2].addr(), "{rank:?}");
        assert!(rank[..2].contains(&servers[0].addr()) && rank[..2].contains(&servers[1].addr()));

        // H, H - 1 and H - 2: all three are eligible, so the fastest ranks first.
        let servers = [stub_at(200, 100).await, stub_at(200, 99).await, stub_at(5, 98).await];
        let client = measured_pool(
            &[&servers[0], &servers[1], &servers[2]],
            None,
            ServerOrder::Measured,
            lenient(),
            None,
            script_draw(&identity(3), 0.5),
        );
        client.activation_probe().await.unwrap();
        wait_for_probe_end(&client).await;
        assert_eq!(rank_of(&client)[0], servers[2].addr());
    });
}

/// T38.6.4l.7: with two maintained connections, a connection two blocks behind triggers on its second
/// notification; one block behind never does.
#[test]
fn t38_6_4l_7_p1_with_two_maintained_connections() {
    block_on(async {
        let (a, b) = (stub_at(5, 100).await, stub_at(5, 100).await);
        let client = measured_pool(
            &[&a, &b],
            None,
            ServerOrder::Measured,
            test_timings(),
            None,
            script_draw(&identity(2), 0.5),
        );
        activate_and_start(&client).await;
        assert_eq!(maintained(&client).len(), 2);

        // One block behind: never.
        for _ in 0..4 {
            b.push_header(99);
        }
        Timer::sleep(0.15).await;
        assert!(!snap_of(&client, &b).poor);

        // Two blocks behind: one notification does not trigger, two consecutive ones do.
        b.push_header(98);
        Timer::sleep(0.15).await;
        assert!(!snap_of(&client, &b).poor);
        b.push_header(98);
        wait_until("the lagging connection to be marked poor", || snap_of(&client, &b).poor).await;
        assert!(!snap_of(&client, &a).poor);
        // It stays maintained and keeps answering.
        assert_eq!(maintained(&client).len(), 2);
    });
}

/// T38.6.4l.7: a notification that is not lagging resets the count of consecutive ones.
#[test]
fn t38_6_4l_7_a_notification_in_between_resets_the_count() {
    block_on(async {
        let (a, b) = (stub_at(5, 100).await, stub_at(5, 100).await);
        let client = measured_pool(
            &[&a, &b],
            None,
            ServerOrder::Measured,
            test_timings(),
            None,
            script_draw(&identity(2), 0.5),
        );
        activate_and_start(&client).await;
        for height in [98, 100, 98, 100, 98] {
            b.push_header(height);
            Timer::sleep(0.05).await;
        }
        Timer::sleep(0.1).await;
        assert!(!snap_of(&client, &b).poor);
    });
}

/// T38.6.4l.15: under `mixed` and `measured`, a server that joins through an ordinary maintenance pass, with no
/// measuring connection, gets exactly one header request right after joining, and P1 then covers it. Under `random`
/// the same join sends one too unless `electrum_tip_checks` is off (R38.6.4o item 3, item 11, T38.6.4o.3 and
/// T38.6.4o.11), which is the setting of the last part of this test.
#[test]
fn t38_6_4l_15_a_connection_joining_through_a_pass_sends_a_header_request() {
    block_on(async {
        for order in [ServerOrder::Mixed, ServerOrder::Measured] {
            let (a, b) = (stub_at(5, 100).await, stub_at(5, 100).await);
            let client = measured_pool(
                &[&a, &b],
                None,
                order.clone(),
                quiet_timings(),
                None,
                script_draw(&identity(2), 0.5),
            );
            // No activation probe: both servers join through the first maintenance pass.
            client.start_pool(false);
            wait_until("both servers to join", || maintained(&client).len() == 2).await;
            wait_until("both header requests to be answered", || {
                snap_of(&client, &a).height == Some(100) && snap_of(&client, &b).height == Some(100)
            })
            .await;
            // Exactly one, right after `server.version`, and no measuring connection.
            Timer::sleep(0.2).await;
            for server in [&a, &b] {
                assert_eq!(server.requests(HEADERS), 1, "{order:?}");
                assert_eq!(methods_received(server)[..2], ["server.version", HEADERS], "{order:?}");
                assert_eq!(server.accepted(), 1, "{order:?}");
            }
            // Its answer is the first sample of the smoothed latency (R38.6.4a item 7), and no re-measure round ran.
            assert!(client.pool_snapshot().iter().all(|state| state.samples == 1));
            assert!(client.pool.remeasure_rounds().is_empty());

            // P1 covers the joined connection: two consecutive notifications 2 blocks behind trigger it.
            b.push_header(98);
            Timer::sleep(0.15).await;
            assert!(!snap_of(&client, &b).poor, "{order:?}");
            b.push_header(98);
            wait_until("the lagging connection to be marked poor", || snap_of(&client, &b).poor).await;
            assert!(!snap_of(&client, &a).poor, "{order:?}");
        }

        // Under `random` with the tip checks off the same join sends no header request.
        let (a, b) = (stub_at(5, 100).await, stub_at(5, 100).await);
        let client = measured_pool(
            &[&a, &b],
            None,
            ServerOrder::Random,
            quiet_timings(),
            None,
            script_draw(&[], 0.5),
        );
        client.start_pool(false);
        wait_until("both servers to join", || maintained(&client).len() == 2).await;
        Timer::sleep(0.3).await;
        assert_eq!(a.requests(HEADERS) + b.requests(HEADERS), 0);

        // With the default `electrum_tip_checks` the same join sends one request each, but no trigger runs
        // (R38.6.4l item 1, R38.6.4o items 3 and 11): a lagging notification marks nothing poor.
        let (a, b) = (stub_at(5, 100).await, stub_at(5, 100).await);
        let client = checked_pool(
            &[&a, &b],
            PoolSetup {
                order: ServerOrder::Random,
                ..PoolSetup::new("L15DEFAULT")
            },
            quiet_timings(),
            script_draw(&[], 0.5),
        );
        client.start_pool(false);
        wait_until("both servers to join", || maintained(&client).len() == 2).await;
        wait_until("both header requests to be answered", || {
            snap_of(&client, &a).height == Some(100) && snap_of(&client, &b).height == Some(100)
        })
        .await;
        assert_eq!((a.requests(HEADERS), b.requests(HEADERS)), (1, 1));
        b.push_header(98);
        b.push_header(98);
        Timer::sleep(0.3).await;
        assert!(client.pool_snapshot().iter().all(|state| !state.poor));
        assert!(client.pool.remeasure_rounds().is_empty());
    });
}

/// T38.6.4l.8: with one maintained connection, no header notification for longer than the limit triggers; a
/// notification restarts the count; the limit grows with the coin's block interval.
///
/// The limit is a second or more, and every "not yet" check applies only while the time since the event it counts
/// from is under the limit, which the test measures itself: a machine too loaded to keep to the schedule skips a
/// check, it cannot fail one. The "stale only after the limit" checks are lower bounds and need no guard.
#[test]
fn t38_6_4l_8_p1_with_one_maintained_connection() {
    block_on(async {
        // No block interval: the limit is the floor (1 s in this test).
        let floor = ms(1_000);
        let a = stub_at(5, 100).await;
        let client = measured_pool(
            &[&a],
            None,
            ServerOrder::Measured,
            ElectrumPoolTimings {
                stale_floor: floor,
                ..lenient()
            },
            None,
            script_draw(&[], 0.5),
        );
        let started = Instant::now();
        activate_and_start(&client).await;
        // A notification every 0.3 s for two limits: each one restarts the count, so the connection never turns
        // stale although it has been maintained for longer than the limit.
        let mut last_push = started;
        let mut kept_schedule = true;
        while started.elapsed() < floor * 2 {
            Timer::sleep(0.3).await;
            let poor = snap_of(&client, &a).poor;
            if last_push.elapsed() < floor {
                assert!(!poor, "{:?}", last_push.elapsed());
            } else {
                kept_schedule = false;
                break;
            }
            last_push = Instant::now();
            a.push_header(101);
        }
        if kept_schedule {
            // No notification any more: stale only after the limit, counted from the last notification (which
            // arrived after `last_push` was taken).
            wait_until("the lone connection to turn stale", || snap_of(&client, &a).poor).await;
            assert!(last_push.elapsed() >= floor, "{:?}", last_push.elapsed());
            assert_eq!(maintained(&client).len(), 1);
        }

        // A block interval of 400 ms makes the limit 1.2 s, above the 0.6 s floor.
        let limit = ms(1_200);
        let b = stub_at(5, 100).await;
        let client = measured_pool(
            &[&b],
            None,
            ServerOrder::Measured,
            ElectrumPoolTimings {
                stale_floor: ms(600),
                ..lenient()
            },
            Some(ms(400)),
            script_draw(&[], 0.5),
        );
        let started = Instant::now();
        activate_and_start(&client).await;
        // Past the floor and short of the limit: not stale.
        Timer::sleep(0.9).await;
        let poor = snap_of(&client, &b).poor;
        if started.elapsed() < limit {
            assert!(!poor, "{:?}", started.elapsed());
        }
        wait_until("the connection to turn stale", || snap_of(&client, &b).poor).await;
        assert!(started.elapsed() >= limit, "{:?}", started.elapsed());
    });
}

/// T38.6.4l.8: a maintained connection over which no header notification can arrive never triggers P1.
#[test]
fn t38_6_4l_8_a_connection_without_notifications_never_turns_stale() {
    block_on(async {
        // No tip: the header answer has no height, so nothing is measured and nothing is pushed.
        let a = StubServer::start(StubBehavior::Answer).await;
        let client = measured_pool(
            &[&a],
            None,
            ServerOrder::Measured,
            quiet_timings(),
            None,
            script_draw(&[], 0.5),
        );
        activate_and_start(&client).await;
        Timer::sleep(1.0).await;
        assert!(!snap_of(&client, &a).poor);
        assert!(client.pool.remeasure_rounds().is_empty());
    });
}

/// T38.6.4l.9 and T38.6.4l.10: P2 needs five samples; the connection stays maintained and keeps answering; a
/// further trigger restarts the poor-demotion mark.
#[test]
fn t38_6_4l_9_p2_needs_five_slow_samples() {
    block_on(async {
        // The server's own reference latency is its few-millisecond activation round; the samples take 1.2 s, a
        // slowness no scheduling jitter in the reference can hide.
        let a = stub_at(10, 100).await;
        let client = measured_pool(
            &[&a],
            None,
            ServerOrder::Measured,
            lenient(),
            None,
            script_draw(&[], 0.5),
        );
        activate_and_start(&client).await;
        a.set_reply_delay("blockchain.block.header", ms(1_200));
        // The answer to the activation header request is the first sample (R38.6.4a item 7), so three slow samples
        // (asked at once, so they take as long as one) make four: not enough.
        assert_eq!(snap_of(&client, &a).samples, 1);
        ask_together(&client, "blockchain.block.header", 3).await;
        wait_until("four samples", || snap_of(&client, &a).samples == 4).await;
        let state = snap_of(&client, &a);
        assert!(!state.poor, "{state:?}");
        // The fifth makes it poor.
        ask(&client, "blockchain.block.header").await.unwrap();
        wait_until("the slow connection to be marked poor", || snap_of(&client, &a).poor).await;
        let first = snap_of(&client, &a);
        // The average started at the few milliseconds of the header answer and moved towards 1.2 s.
        assert!(first.smoothed_ms.unwrap() > 500.);

        // It stays maintained and keeps answering (T38.6.4l.10); a further slow sample restarts the mark.
        assert_eq!(maintained(&client), vec![0]);
        ask(&client, "blockchain.block.header").await.unwrap();
        wait_until("the mark to restart", || {
            snap_of(&client, &a).poor_until > first.poor_until
        })
        .await;
        assert_eq!(maintained(&client), vec![0]);
    });
}

/// T38.6.4l.11: no stub outside the maintained set gets a header request while nothing triggers, however many
/// maintenance passes run.
#[test]
fn t38_6_4l_11_nothing_measures_on_a_schedule() {
    block_on(async {
        let servers = [stub_at(5, 100).await, stub_at(150, 100).await, stub_at(300, 100).await];
        let timings = ElectrumPoolTimings {
            maintenance_interval: ms(80),
            ..lenient()
        };
        let client = measured_pool(
            &[&servers[0], &servers[1], &servers[2]],
            Some(1),
            ServerOrder::Measured,
            timings,
            None,
            script_draw(&identity(3), 0.5),
        );
        activate_and_start(&client).await;
        // The set the probe formed may still move to the measured rank through a maintenance pass, and the server
        // that joins that way sends its one header request on joining (T38.6.4l.15). Let that settle first.
        wait_until("the best server to be maintained and to have answered", || {
            maintained(&client) == vec![0] && snap(&client, 0).height.is_some()
        })
        .await;
        let counts: Vec<usize> = servers.iter().map(|server| server.requests(HEADERS)).collect();
        for (server, count) in servers.iter().zip(&counts) {
            if snap_of(&client, server).maintained {
                assert!((1..=2).contains(count), "{count}");
            } else {
                // Only the activation round's request.
                assert_eq!(*count, 1);
            }
        }
        Timer::sleep(1.5).await;
        let after: Vec<usize> = servers.iter().map(|server| server.requests(HEADERS)).collect();
        assert_eq!(after, counts);
        assert!(client.pool.remeasure_rounds().is_empty());
    });
}

/// Four servers answering their header request in 20, 270, 520 and 770 ms, ranked in that order, with the first
/// two maintained (`max_connected` 2), the second made slow so that it is triggered. The latencies differ by
/// a quarter of a second, so that scheduling jitter on a loaded machine cannot reorder them.
async fn replacement_scenario(order: ServerOrder, draw: Vec<f64>) -> ([StubServer; 4], Arc<ElectrumClientImpl>) {
    let servers = [
        stub_at(20, 100).await,
        stub_at(270, 100).await,
        stub_at(520, 100).await,
        stub_at(770, 100).await,
    ];
    let client = measured_pool(
        &[&servers[0], &servers[1], &servers[2], &servers[3]],
        Some(2),
        order,
        lenient(),
        None,
        script_draw(&draw, 0.99),
    );
    client.activation_probe().await.unwrap();
    wait_for_probe_end(&client).await;
    assert_eq!(maintained(&client), vec![0, 1]);
    assert_eq!(
        rank_of(&client),
        servers.iter().map(|server| server.addr()).collect::<Vec<_>>()
    );
    wait_until("the released servers to leave their suspension", || {
        client
            .pool_snapshot()
            .iter()
            .all(|state| state.suspended_until.is_none())
    })
    .await;
    client.start_pool(false);
    (servers, client)
}

/// Makes the second server slow: five pings, sent at once, that take 3 s each, far above four times its
/// 270 ms activation latency.
async fn make_second_server_slow(servers: &[StubServer; 4], client: &Arc<ElectrumClientImpl>) {
    servers[1].set_reply_delay("server.ping", ms(3_000));
    futures::future::join_all((0..5).map(|_| client.pool.ping_maintained())).await;
}

/// T38.6.4l.12: with rank A, B, C, D, A and B maintained and B triggered, a draw choosing D leaves A and D
/// maintained and the rank A, D, C, B; D joins over its measuring connection and C's is released.
#[test]
fn t38_6_4l_12_the_replacement_takes_the_poor_servers_place() {
    block_on(async {
        // The first shuffle leaves the order; the activation round takes the first place left each time; the
        // replacement draw (0.99) chooses the last of the two candidates.
        let (servers, client) =
            replacement_scenario(ServerOrder::Mixed, vec![0.99, 0.99, 0.99, 0., 0., 0., 0., 0.99]).await;
        // A watched script hash exists while the servers are measured (T38.6.4l.14).
        ElectrumClient(client.clone())
            .subscribe_scripthash("aa".to_owned())
            .await
            .unwrap();
        make_second_server_slow(&servers, &client).await;

        wait_until("the replacement to join", || snap_of(&client, &servers[3]).maintained).await;
        wait_until("the poor server to leave", || !snap_of(&client, &servers[1]).maintained).await;
        assert_eq!(rank_of(&client), vec![
            servers[0].addr(),
            servers[3].addr(),
            servers[2].addr(),
            servers[1].addr()
        ]);
        assert!(snap_of(&client, &servers[0]).maintained);
        assert!(!snap_of(&client, &servers[2]).maintained);
        // D joined over its measuring connection, without a second one; C's measuring connection is gone.
        assert_eq!(servers[3].accepted(), 2);
        assert_eq!(servers[3].open_connections(), 1);
        assert_eq!(servers[2].accepted(), 2);
        wait_until("the measuring connection of C to close", || {
            servers[2].open_connections() == 0
        })
        .await;
        assert_eq!(client.pool.remeasure_rounds().len(), 1);

        // T38.6.4l.14: C was measured twice and did not join: version negotiation and the header request alone.
        for method in methods_received(&servers[2]) {
            assert!(method == "server.version" || method == HEADERS, "{method}");
        }
    });
}

/// T38.6.4l.12: under `measured` the candidate of highest quality is chosen.
#[test]
fn t38_6_4l_12_under_measured_the_best_candidate_is_chosen() {
    block_on(async {
        let (servers, client) = replacement_scenario(ServerOrder::Measured, identity(4)).await;
        // In the round the last candidate answers fastest.
        servers[3].set_reply_delay(HEADERS, ms(20));
        servers[2].set_reply_delay(HEADERS, ms(400));
        make_second_server_slow(&servers, &client).await;
        wait_until("the replacement to join", || snap_of(&client, &servers[3]).maintained).await;
        wait_until("the poor server to leave", || !snap_of(&client, &servers[1]).maintained).await;
        assert_eq!(rank_of(&client), vec![
            servers[0].addr(),
            servers[3].addr(),
            servers[2].addr(),
            servers[1].addr()
        ]);
    });
}

/// T38.6.4l.13 and T38.6.4l.11: with every candidate two or more blocks behind the agreed tip, the poor server stays
/// maintained, the rank is unchanged, and the next round starts no earlier than the limit later. The agreed tip is
/// the second-highest height (R38.6.4o item 2), so two maintained connections at 100 make the candidates at 95 lag.
#[test]
fn t38_6_4l_13_without_an_eligible_candidate_the_poor_server_stays() {
    block_on(async {
        // `a` is the fastest and the one that is made slow; `d` is the other maintained connection.
        let a = stub_at(10, 100).await;
        let d = stub_at(100, 100).await;
        let (b, c) = (stub_at(10, 95).await, stub_at(10, 95).await);
        // A re-measure limit of 2 s: the second round starts that long after the first, long after the first
        // round's connections were made, so the counts below are not read while a round is under way.
        let client = measured_pool(
            &[&a, &d, &b, &c],
            Some(2),
            ServerOrder::Measured,
            ElectrumPoolTimings {
                remeasure_limit: ms(2_000),
                ..lenient()
            },
            None,
            script_draw(&identity(4), 0.5),
        );
        client.activation_probe().await.unwrap();
        wait_for_probe_end(&client).await;
        let rank = rank_of(&client);
        assert_eq!(rank[..2], [a.addr(), d.addr()]);
        wait_until("the released servers to leave their suspension", || {
            snap_of(&client, &b).suspended_until.is_none() && snap_of(&client, &c).suspended_until.is_none()
        })
        .await;
        client.start_pool(false);
        a.set_reply_delay("blockchain.block.header", ms(1_200));
        ask_together(&client, "blockchain.block.header", 5).await;
        wait_until("two rounds", || client.pool.remeasure_rounds().len() >= 2).await;
        let rounds = client.pool.remeasure_rounds();
        // Two triggers close together start their rounds at least the limit (2 s) apart.
        assert!(rounds[1] - rounds[0] >= 2.0 - 0.01, "{rounds:?}");
        assert_eq!(rank_of(&client), rank);
        assert_eq!(maintained(&client), vec![0, 1]);
        assert!(snap_of(&client, &a).poor);
        // Each round makes one measuring connection per candidate, and no more.
        wait_until("the second round's connections", || {
            b.accepted() >= 1 + rounds.len() && c.accepted() >= 1 + rounds.len()
        })
        .await;
        assert_eq!(b.accepted(), 1 + rounds.len());
        assert_eq!(c.accepted(), 1 + rounds.len());
        // Nothing but the negotiation and the header request travelled over them.
        for server in [&b, &c] {
            for method in methods_received(server) {
                assert!(method == "server.version" || method == HEADERS, "{method}");
            }
        }
    });
}

/// T38.6.9.1 (R38.6.9a, native build): a server whose address carries userinfo, a path or a query is named by
/// `host:port` or by the placeholder and its list position, and no text or debug form carries more.
#[test]
fn t38_6_9_1_servers_are_named_host_and_port_only() {
    use crate::node_url::UNNAMED_SERVER;
    const SECRETS: [&str; 4] = ["user", "secret", "/ws/", "token"];
    block_on(async {
        let down = StubServer::start(StubBehavior::Down).await;
        let down_request = down.request();
        let listed = vec![
            ElectrumRpcRequest {
                url: "user:secret@127.0.0.1:50002".to_owned(),
                ..down_request.clone()
            },
            ElectrumRpcRequest {
                url: "tcp://127.0.0.1:50002/ws/x?token=y".to_owned(),
                ..down_request.clone()
            },
            ElectrumRpcRequest {
                url: "secret".to_owned(),
                ..down_request.clone()
            },
            down_request.clone(),
        ];
        let mut client = ElectrumClientImpl::new(TICKER.into(), vec![]);
        client.configure_pool(ElectrumPoolOptions {
            client_name: Some(CLIENT_NAME.into()),
            timings: test_timings(),
            ..Default::default()
        });
        let removed = client.add_servers(&listed);
        assert_eq!(removed.len(), 3, "three of the four addresses are unusable");
        let names: Vec<&str> = removed.iter().map(|(name, _)| name.as_str()).collect();
        assert!(
            names[..2].iter().all(|name| *name == "127.0.0.1:50002"),
            "reducible addresses are named host:port: {names:?}"
        );
        assert_eq!(names[2], format!("{UNNAMED_SERVER} #3"));
        let client = Arc::new(client);
        let failure = client.activation_probe().await.unwrap_err();
        let texts: Vec<String> = removed
            .iter()
            .map(|(name, reason)| format!("{name}: {reason}"))
            .chain([failure, format!("{:?}", listed), format!("{:?}", client)])
            .collect();
        for text in &texts {
            for secret in SECRETS {
                assert!(!text.contains(secret), "a credential part reaches a text");
            }
        }
        assert!(
            texts[texts.len() - 3].contains(&down.addr()),
            "the refusing server is named host:port"
        );
    });
}

// ---- CRD ch.38 R38.6.4o: server staleness -----------------------------------------------------------------------

/// The WARN lines of the pool of `ticker` logged so far, in order. Empty when another logger owns the process, in
/// which case a test must not assert that a line is present (see `install_log_capture`).
fn warn_lines(ticker: &str) -> Vec<String> {
    use crate::eth::alloy_compat::transport::test_support::captured_log_lines;
    let prefix = format!("WARN {ticker}: ");
    captured_log_lines()
        .into_iter()
        .filter(|line| line.starts_with(&prefix))
        .collect()
}

/// Joins every server of `client` through a maintenance pass and waits until each connection has its height, which
/// is the answer to the header request it sends on joining (R38.6.4o item 3).
async fn join_with_heights(client: &Arc<ElectrumClientImpl>, servers: &[&StubServer]) {
    client.pool.run_pass().await;
    wait_until("every connection to have a height", || {
        servers.iter().all(|server| snap_of(client, server).height.is_some())
    })
    .await;
}

/// What `electrum_servers` says of `server`'s entry.
fn view_of(client: &ElectrumClientImpl, server: &StubServer) -> super::electrum_pool::ElectrumServerView {
    client
        .servers_view()
        .servers
        .into_iter()
        .find(|view| view.server == server.addr())
        .expect("the server is in the pool")
}

/// The servers that answered `asks` read-only queries in a row.
async fn answered_by(client: &ElectrumClientImpl, asks: usize) -> Vec<String> {
    let mut answered = Vec::new();
    for _ in 0..asks {
        answered.push(ask(client, READ).await.unwrap());
    }
    answered
}

/// T38.6.4o.3: under `random`, with a coin whose tolerance is 3 blocks, each stub gets exactly one header request
/// after joining; a stub 4 blocks behind is out of date as behind after its first answer, takes no read, and is
/// named in one WARN line; when it notifies the tip it is up to date again, with one WARN line, and takes reads
/// again. A stub 3 blocks behind is never out of date.
#[test]
fn t38_6_4o_3_a_connection_behind_is_out_of_date_at_once_and_reversibly() {
    let capturing = crate::eth::alloy_compat::transport::test_support::install_log_capture();
    block_on(async {
        const H: u64 = 1_000;
        let ticker = "OOD3";
        let (a, b, c) = (stub_at(5, H).await, stub_at(5, H).await, stub_at(5, H - 4).await);
        let client = checked_pool(
            &[&a, &b, &c],
            PoolSetup {
                order: ServerOrder::Random,
                block_interval: Some(Duration::from_secs(60)),
                ..PoolSetup::new(ticker)
            },
            quiet_timings(),
            script_draw(&[], 0.5),
        );
        assert_eq!(client.servers_view().tolerance_blocks, 3);
        join_with_heights(&client, &[&a, &b, &c]).await;
        for server in [&a, &b, &c] {
            assert_eq!(server.requests(HEADERS), 1);
        }
        let behind = view_of(&client, &c);
        assert_eq!(behind.out_of_date, Some("behind"), "{behind:?}");
        assert_eq!((behind.height, behind.lag), (Some(H - 4), Some(4)));
        assert_eq!(client.servers_view().agreed_tip, Some(H));

        // 30 reads are answered by the first two only.
        let answered = answered_by(&client, 30).await;
        assert!(answered.iter().all(|addr| *addr != c.addr()), "{answered:?}");
        assert!(answered.contains(&a.addr()) && answered.contains(&b.addr()));
        assert_eq!(c.requests(READ), 0);
        if capturing {
            assert_eq!(warn_lines(ticker), vec![format!(
                "WARN {ticker}: Electrum server {} is 4 blocks behind the other servers (tip {H})",
                c.addr()
            )]);
        }

        // It notifies the tip: up to date again, one more line, and it takes reads again.
        c.set_tip(H);
        wait_until("the stub to be up to date again", || {
            view_of(&client, &c).out_of_date.is_none()
        })
        .await;
        let answered = answered_by(&client, 30).await;
        assert!(answered.contains(&c.addr()), "{answered:?}");
        if capturing {
            let lines = warn_lines(ticker);
            assert_eq!(lines.len(), 2, "{lines:?}");
            assert_eq!(
                lines[1],
                format!("WARN {ticker}: Electrum server {} is up to date again", c.addr())
            );
        }

        // 3 blocks behind is within the tolerance: never out of date, nothing logged.
        let ticker = "OOD3B";
        let (a, b, d) = (stub_at(5, H).await, stub_at(5, H).await, stub_at(5, H - 3).await);
        let client = checked_pool(
            &[&a, &b, &d],
            PoolSetup {
                order: ServerOrder::Random,
                block_interval: Some(Duration::from_secs(60)),
                ..PoolSetup::new(ticker)
            },
            quiet_timings(),
            script_draw(&[], 0.5),
        );
        join_with_heights(&client, &[&a, &b, &d]).await;
        answered_by(&client, 10).await;
        assert!(client
            .servers_view()
            .servers
            .iter()
            .all(|view| view.out_of_date.is_none()));
        assert!(warn_lines(ticker).is_empty());
    });
}

/// T38.6.4o.4 (the field case): under the defaults, with two maintained stubs at H and H - 1000, the stale one listed
/// first, every read and every broadcast goes to the stub at H, although the floor of two near sessions would
/// otherwise keep both in turn.
#[test]
fn t38_6_4o_4_two_servers_one_far_behind() {
    use super::electrum_measure::NEAR_FACTOR_DEFAULT;
    block_on(async {
        let (stale, good) = (stub_at(5, 2_000 - 1_000).await, stub_at(5, 2_000).await);
        let client = checked_pool(
            &[&stale, &good],
            PoolSetup {
                near_factor: NEAR_FACTOR_DEFAULT,
                block_interval: Some(Duration::from_secs(60)),
                ..PoolSetup::new("OOD4")
            },
            quiet_timings(),
            script_draw(&[], 0.5),
        );
        join_with_heights(&client, &[&stale, &good]).await;
        assert_eq!(view_of(&client, &stale).out_of_date, Some("behind"));
        // The reads, with a sample on each session so that the near decision has its figures.
        client.pool.ping_maintained().await;
        let answered = answered_by(&client, 20).await;
        assert!(answered.iter().all(|addr| *addr == good.addr()), "{answered:?}");
        assert_eq!(stale.requests(READ), 0);
        // The broadcasts follow the priority order, which puts the stale stub first, but not an out-of-date one.
        for _ in 0..5 {
            let broadcast = rpc("blockchain.transaction.broadcast", vec![json!("aa")]);
            assert_eq!(route_to(&client, broadcast).await.unwrap(), good.addr());
        }
        assert_eq!(stale.requests("blockchain.transaction.broadcast"), 0);
        assert_eq!(good.requests("blockchain.transaction.broadcast"), 5);

        // It is still a fallback: with the good stub failing a read, the stale one answers.
        good.set_behavior(StubBehavior::Down).await;
        assert_eq!(ask(&client, READ).await.unwrap(), stale.addr());
    });
}

/// T38.6.4o.5: with three maintained stubs and the stuck period shortened, two stubs notifying new tips and one
/// silent, the silent one becomes out of date as stuck after the period and not before, and leaves the read turn. With
/// every stub silent for longer than the period none is stuck. With a single maintained stub item 4 does not apply.
#[test]
fn t38_6_4o_5_a_silent_connection_is_stuck_while_another_one_hears_news() {
    let capturing = crate::eth::alloy_compat::transport::test_support::install_log_capture();
    block_on(async {
        let ticker = "OOD5";
        let period = ms(600);
        // A block interval of 100 ms: the tolerance is the cap of 20 blocks, so the silent stub never lags that far
        // behind in the time of the test, and the stuck period is the larger of 3 intervals and the floor: 600 ms.
        let block_interval = Some(ms(100));
        let timings = ElectrumPoolTimings {
            stale_floor: period,
            ..quiet_timings()
        };
        let (a, b, c) = (
            stub_at(5, 1_000).await,
            stub_at(5, 1_000).await,
            stub_at(5, 1_000).await,
        );
        let client = checked_pool(
            &[&a, &b, &c],
            PoolSetup {
                order: ServerOrder::Random,
                block_interval,
                ..PoolSetup::new(ticker)
            },
            timings.clone(),
            script_draw(&[], 0.5),
        );
        let before = Instant::now();
        join_with_heights(&client, &[&a, &b, &c]).await;
        let mut height = 1_000;
        let stuck_after = loop {
            assert!(before.elapsed() < Duration::from_secs(8), "the stub never became stuck");
            height += 1;
            a.set_tip(height);
            b.set_tip(height);
            Timer::sleep(0.1).await;
            let state = view_of(&client, &c).out_of_date;
            // Read the clock after the state: a state read before the period cannot be stuck.
            let elapsed = before.elapsed();
            if state == Some("stuck") {
                break elapsed;
            }
            if elapsed < period {
                assert_eq!(state, None, "{elapsed:?}");
            }
        };
        assert!(stuck_after >= period, "{stuck_after:?}");
        assert!(view_of(&client, &a).out_of_date.is_none() && view_of(&client, &b).out_of_date.is_none());
        let answered = answered_by(&client, 20).await;
        assert!(answered.iter().all(|addr| *addr != c.addr()), "{answered:?}");
        assert_eq!(c.requests(READ), 0);
        if capturing {
            let lines = warn_lines(ticker);
            assert_eq!(lines.len(), 1, "{lines:?}");
            assert!(
                lines[0].starts_with(&format!(
                    "WARN {ticker}: Electrum server {} has sent no new block for ",
                    c.addr()
                )) && lines[0].ends_with(" s while another server did"),
                "{lines:?}"
            );
        }

        // Every stub silent for longer than the period: the chain may have stopped, none is stuck.
        let (a, b, c) = (
            stub_at(5, 1_000).await,
            stub_at(5, 1_000).await,
            stub_at(5, 1_000).await,
        );
        let client = checked_pool(
            &[&a, &b, &c],
            PoolSetup {
                block_interval,
                ..PoolSetup::new("OOD5B")
            },
            timings.clone(),
            script_draw(&[], 0.5),
        );
        join_with_heights(&client, &[&a, &b, &c]).await;
        Timer::sleep(0.9).await;
        answered_by(&client, 6).await;
        assert!(client
            .servers_view()
            .servers
            .iter()
            .all(|view| view.out_of_date.is_none()));
        assert!(warn_lines("OOD5B").is_empty());

        // A single maintained stub: item 4 does not apply, however long it is silent.
        let lone = stub_at(5, 1_000).await;
        let client = checked_pool(
            &[&lone],
            PoolSetup {
                block_interval,
                ..PoolSetup::new("OOD5C")
            },
            timings,
            script_draw(&[], 0.5),
        );
        join_with_heights(&client, &[&lone]).await;
        Timer::sleep(0.9).await;
        answered_by(&client, 2).await;
        assert_eq!(view_of(&client, &lone).out_of_date, None);
    });
}

/// T38.6.4o.6 (a liar does not steer): under `measured`, with three stubs at H + 1,000,000, H and H, the two at H are
/// eligible and rank before the liar, which is not eligible; no connection at H is out of date. With the three
/// maintained under `random` the liar is out of date as ahead, takes no reads and is named in one WARN line; a stub at
/// H + T stays up to date.
#[test]
fn t38_6_4o_6_a_liar_does_not_steer() {
    let capturing = crate::eth::alloy_compat::transport::test_support::install_log_capture();
    block_on(async {
        const H: u64 = 700_000;
        // The liar answers fastest, so that it would rank first if it were eligible.
        let liar = stub_at(20, H + 1_000_000).await;
        let (x, y) = (stub_at(150, H).await, stub_at(250, H).await);
        let client = checked_pool(
            &[&liar, &x, &y],
            PoolSetup {
                order: ServerOrder::Measured,
                spread_reads: false,
                ..PoolSetup::new("LIAR")
            },
            lenient(),
            script_draw(&identity(3), 0.5),
        );
        client.activation_probe().await.unwrap();
        wait_for_probe_end(&client).await;
        assert_eq!(rank_of(&client), vec![x.addr(), y.addr(), liar.addr()]);
        assert_eq!(client.servers_view().agreed_tip, Some(H));
        assert_eq!(view_of(&client, &x).out_of_date, None);
        assert_eq!(view_of(&client, &y).out_of_date, None);
        assert_eq!(view_of(&client, &liar).out_of_date, Some("ahead"));

        // Under `random`, with the tolerance of 3 blocks: the liar takes no reads and is named once.
        let ticker = "LIAR2";
        // The liar answers its header request last: while only it and one honest stub have a height, the honest one
        // is judged against the higher of the two (R38.6.4o item 2), which is not what this test is about.
        let liar = stub_at(200, H + 1_000_000).await;
        let (x, y) = (stub_at(5, H).await, stub_at(5, H).await);
        let client = checked_pool(
            &[&liar, &x, &y],
            PoolSetup {
                order: ServerOrder::Random,
                block_interval: Some(Duration::from_secs(60)),
                ..PoolSetup::new(ticker)
            },
            quiet_timings(),
            script_draw(&[], 0.5),
        );
        join_with_heights(&client, &[&liar, &x, &y]).await;
        let view = view_of(&client, &liar);
        assert_eq!(
            (view.out_of_date, view.lag),
            (Some("ahead"), Some(-1_000_000)),
            "{view:?}"
        );
        let answered = answered_by(&client, 30).await;
        assert!(answered.iter().all(|addr| *addr != liar.addr()), "{answered:?}");
        assert_eq!(liar.requests(READ), 0);
        if capturing {
            assert_eq!(warn_lines(ticker), vec![format!(
                "WARN {ticker}: Electrum server {} is 1000000 blocks ahead of the other servers (tip {H})",
                liar.addr()
            )]);
        }

        // A server at H + T (T = 3) stays up to date and takes reads.
        let ticker = "LIAR3";
        let near = stub_at(5, H + 3).await;
        let (x, y) = (stub_at(5, H).await, stub_at(5, H).await);
        let client = checked_pool(
            &[&near, &x, &y],
            PoolSetup {
                order: ServerOrder::Random,
                block_interval: Some(Duration::from_secs(60)),
                ..PoolSetup::new(ticker)
            },
            quiet_timings(),
            script_draw(&[], 0.5),
        );
        join_with_heights(&client, &[&near, &x, &y]).await;
        assert_eq!(view_of(&client, &near).out_of_date, None);
        assert!(answered_by(&client, 30).await.contains(&near.addr()));
        assert!(warn_lines(ticker).is_empty());
    });
}

/// T38.6.4o.11 (the parts about the header request and the read turn): with `electrum_tip_checks` off under
/// `random`, no stub receives a header request after joining and a stub 1000 blocks behind stays in the read turn; a
/// value that is not a boolean is taken as `true` with one warning, and an absent key and `null` log nothing.
#[test]
fn t38_6_4o_11_the_switch_turns_the_checks_off() {
    use super::electrum_pool::parse_tip_checks;
    assert_eq!(parse_tip_checks(&json!({})), (true, None));
    assert_eq!(parse_tip_checks(&json!({"electrum_tip_checks": null})), (true, None));
    assert_eq!(parse_tip_checks(&json!({"electrum_tip_checks": true})), (true, None));
    assert_eq!(parse_tip_checks(&json!({"electrum_tip_checks": false})), (false, None));
    for bad in [json!("false"), json!(0), json!(1), json!([]), json!({})] {
        let (checks, warning) = parse_tip_checks(&json!({ "electrum_tip_checks": bad }));
        assert!(checks, "{bad}: a value that is not a boolean is taken as true");
        let warning = warning.unwrap_or_default();
        assert!(warning.contains("electrum_tip_checks"), "{bad}: {warning}");
    }

    block_on(async {
        let (stale, good) = (stub_at(5, 1_000).await, stub_at(5, 2_000).await);
        let client = checked_pool(
            &[&stale, &good],
            PoolSetup {
                order: ServerOrder::Random,
                tip_checks: false,
                block_interval: Some(Duration::from_secs(60)),
                ..PoolSetup::new("OOD11")
            },
            quiet_timings(),
            script_draw(&[], 0.5),
        );
        client.pool.run_pass().await;
        assert_eq!(maintained(&client).len(), 2);
        Timer::sleep(0.2).await;
        assert_eq!(stale.requests(HEADERS) + good.requests(HEADERS), 0);
        // With no height known, nothing is out of date; the stale stub keeps its share of the reads.
        let answered = answered_by(&client, 20).await;
        assert!(answered.contains(&stale.addr()), "{answered:?}");
        assert!(client
            .servers_view()
            .servers
            .iter()
            .all(|view| view.out_of_date.is_none()));
    });
}

/// A verbose transaction with `confirmations`, as `blockchain.transaction.get` answers it.
fn verbose_tx(confirmations: u32) -> Json {
    json!({
        "hex": "00",
        "txid": "00".repeat(32),
        "version": 1,
        "locktime": 0,
        "vin": [],
        "vout": [],
        "confirmations": confirmations,
    })
}

/// A wait for one confirmation of a transaction that polls every second.
async fn wait_for_one_confirmation(client: &ElectrumClient) -> Result<(), String> {
    let wait_until = now_ms() / 1000 + 60;
    client
        .wait_for_confirmations_on_one_server(H256Json::default(), 0, 1, false, wait_until, 1)
        .await
}

const TX_GET: &str = "blockchain.transaction.get";

/// T38.6.4o.7: with two maintained stubs, A first in priority order answering 0 confirmations and B answering 3, a
/// wait for 1 confirmation does not end while A keeps answering, whatever the read counter, and every transaction
/// request of the wait reaches A. When A starts to fail the wait goes on with B and ends on its answer. With A out of
/// date the wait chooses B from the start.
#[test]
fn t38_6_4o_7_the_confirmation_wait_sticks_to_one_server() {
    use futures::future::{select, Either};
    block_on(async {
        const H: u64 = 1_000;
        let (a, b) = (stub_at(5, H).await, stub_at(5, H).await);
        a.set_result(TX_GET, verbose_tx(0));
        b.set_result(TX_GET, verbose_tx(3));
        let client = checked_pool(
            &[&a, &b],
            PoolSetup::new("WAIT7"),
            quiet_timings(),
            script_draw(&[], 0.5),
        );
        join_with_heights(&client, &[&a, &b]).await;
        let electrum = ElectrumClient(client.clone());
        // Move the read counter on: whatever its value, the wait polls A.
        for _ in 0..3 {
            ask(&client, READ).await.unwrap();
        }
        let mut wait = Box::pin(wait_for_one_confirmation(&electrum));
        match select(wait.as_mut(), Box::pin(Timer::sleep(2.5))).await {
            Either::Left((result, _)) => panic!("the wait ended on {result:?} while A answers 0 confirmations"),
            Either::Right(_) => (),
        }
        assert!(a.requests(TX_GET) >= 2, "{}", a.requests(TX_GET));
        assert_eq!(b.requests(TX_GET), 0);

        // A starts to fail: the wait goes on with B and ends on its answer.
        a.set_behavior(StubBehavior::Down).await;
        wait.await.unwrap();
        assert!(b.requests(TX_GET) >= 1);

        // With A out of date, the wait chooses B from the start. (The coin's tolerance is one block.)
        let (stale, good) = (stub_at(5, H - 1_000).await, stub_at(5, H).await);
        stale.set_result(TX_GET, verbose_tx(0));
        good.set_result(TX_GET, verbose_tx(3));
        let client = checked_pool(
            &[&stale, &good],
            PoolSetup::new("WAIT7B"),
            quiet_timings(),
            script_draw(&[], 0.5),
        );
        join_with_heights(&client, &[&stale, &good]).await;
        wait_for_one_confirmation(&ElectrumClient(client.clone()))
            .await
            .unwrap();
        assert_eq!(stale.requests(TX_GET), 0);
        assert_eq!(good.requests(TX_GET), 1);
    });
}

/// T38.6.4o.11 (the part about the confirmation wait): with `electrum_tip_checks` off the wait polls the first
/// maintained connection in priority order, even one that is far behind.
#[test]
fn t38_6_4o_11_with_the_checks_off_the_wait_polls_the_first_connection() {
    block_on(async {
        let (first, second) = (stub_at(5, 1_000).await, stub_at(5, 2_000).await);
        first.set_result(TX_GET, verbose_tx(3));
        second.set_result(TX_GET, verbose_tx(0));
        let client = checked_pool(
            &[&first, &second],
            PoolSetup {
                tip_checks: false,
                ..PoolSetup::new("WAIT11")
            },
            quiet_timings(),
            script_draw(&[], 0.5),
        );
        client.pool.run_pass().await;
        assert_eq!(maintained(&client).len(), 2);
        wait_for_one_confirmation(&ElectrumClient(client.clone()))
            .await
            .unwrap();
        assert_eq!(first.requests(TX_GET), 1);
        assert_eq!(second.requests(TX_GET), 0);
    });
}

/// T38.6.4o.8: in a measurement round under `measured`, with three stubs at the same height, two answering the same
/// header bytes and one other bytes, the third is not eligible and is neither suspended nor removed. With two stubs
/// at that height and different bytes and no third there, both stay eligible.
#[test]
fn t38_6_4o_8_a_header_that_differs_makes_a_server_not_eligible() {
    block_on(async {
        // The odd one out is the fastest, so that it would rank first if it were eligible.
        let x = stub_at(10, 100).await;
        x.set_header_hex("bb");
        let (y, z) = (stub_at(200, 100).await, stub_at(250, 100).await);
        y.set_header_hex("aa");
        z.set_header_hex("aa");
        let client = checked_pool(
            &[&x, &y, &z],
            PoolSetup {
                order: ServerOrder::Measured,
                spread_reads: false,
                ..PoolSetup::new("HDR8")
            },
            lenient(),
            script_draw(&identity(3), 0.5),
        );
        client.activation_probe().await.unwrap();
        wait_for_probe_end(&client).await;
        // The measured rank is applied when the probe ends, a moment after every server is in the set.
        wait_until("the measured rank", || rank_of(&client)[2] == x.addr()).await;
        let state = snap_of(&client, &x);
        assert!(
            !state.removed && state.suspended_until.is_none() && state.maintained,
            "{state:?}"
        );

        // Two stubs at the height with different bytes and no third: both are eligible.
        let x = stub_at(10, 100).await;
        x.set_header_hex("bb");
        let y = stub_at(200, 100).await;
        y.set_header_hex("aa");
        // Listed second, so that only the measured rank can put it first.
        let client = checked_pool(
            &[&y, &x],
            PoolSetup {
                order: ServerOrder::Measured,
                spread_reads: false,
                ..PoolSetup::new("HDR8B")
            },
            lenient(),
            script_draw(&identity(2), 0.5),
        );
        client.activation_probe().await.unwrap();
        wait_for_probe_end(&client).await;
        wait_until("the measured rank", || rank_of(&client)[0] == x.addr()).await;
    });
}

/// T38.6.4o.9 (the backoff): with `max_connected` 1, two listed stubs and every candidate not eligible, successive
/// re-measure rounds start 5, 10, 20, 40, 60 and 60 time units apart (the timings are scaled: the unit is 60 ms and the
/// cap 12 units). When the poor condition clears the next round waits the base again.
#[test]
fn t38_6_4o_9_rounds_without_a_replacement_back_off_and_the_poor_condition_resets_them() {
    block_on(async {
        // A is the only maintained server and goes silent, which makes a lone connection poor after the stale
        // floor. B is ten blocks behind it, so no round ever finds an eligible replacement.
        let a = stub_at(5, 100).await;
        let b = stub_at(5, 90).await;
        let unit = 0.06;
        let timings = ElectrumPoolTimings {
            stale_floor: ms(150),
            remeasure_limit: ms(60),
            remeasure_cap: ms(720),
            poor_demotion: ms(5_000),
            suspension_base: ms(30),
            suspension_cap: ms(60),
            ..lenient()
        };
        let client = checked_pool(
            &[&a, &b],
            PoolSetup {
                max_connected: Some(1),
                order: ServerOrder::Measured,
                spread_reads: false,
                ..PoolSetup::new("BACKOFF9")
            },
            timings,
            script_draw(&identity(2), 0.5),
        );
        client.activation_probe().await.unwrap();
        wait_for_probe_released(&client).await;
        client.start_pool(false);
        assert_eq!(maintained(&client), vec![0]);
        wait_until("seven rounds", || client.pool.remeasure_rounds().len() >= 7).await;
        let rounds = client.pool.remeasure_rounds();
        let gaps: Vec<f64> = rounds.windows(2).map(|pair| pair[1] - pair[0]).collect();
        for (gap, units) in gaps.iter().zip([1., 2., 4., 8., 12., 12.]) {
            let expected = units * unit;
            assert!(*gap >= expected - 0.005, "{gaps:?} against {units} units");
            assert!(*gap <= expected + 0.5, "{gaps:?} against {units} units");
        }
        // The poor condition clears: A notifies a new tip right after a round started. The next round waits the
        // base again once the connection is silent for the stale floor, instead of the 12 units it was at.
        let seen = rounds.len();
        wait_until("another round", || client.pool.remeasure_rounds().len() > seen).await;
        a.push_header(101);
        wait_until("the round after the reset", || {
            client.pool.remeasure_rounds().len() > seen + 1
        })
        .await;
        let rounds = client.pool.remeasure_rounds();
        let gap = rounds[seen + 1] - rounds[seen];
        assert!(gap >= unit - 0.005 && gap < 12. * unit - 0.1, "{gap}");
    });
}

/// T38.6.4o.9 (the width): a round connects at most 4 servers at a time. Six candidates take half a second to accept,
/// so the fifth and sixth are not started before one of the first four has finished.
#[test]
fn t38_6_4o_9_a_round_connects_at_most_four_servers_at_a_time() {
    block_on(async {
        // A answers fastest, so that it keeps the first rank and stays the maintained server; the six candidates answer
        // alike and slower.
        let a = stub_at(5, 100).await;
        let candidates = [
            stub_at(100, 100).await,
            stub_at(100, 100).await,
            stub_at(100, 100).await,
            stub_at(100, 100).await,
            stub_at(100, 100).await,
            stub_at(100, 100).await,
        ];
        let timings = ElectrumPoolTimings {
            stale_floor: ms(300),
            suspension_base: ms(30),
            suspension_cap: ms(60),
            ..lenient()
        };
        let all: Vec<&StubServer> = std::iter::once(&a).chain(candidates.iter()).collect();
        let client = checked_pool(
            &all,
            PoolSetup {
                max_connected: Some(1),
                order: ServerOrder::Measured,
                spread_reads: false,
                ..PoolSetup::new("WIDTH9")
            },
            timings,
            script_draw(&identity(7), 0.5),
        );
        client.activation_probe().await.unwrap();
        wait_for_probe_released(&client).await;
        client.start_pool(false);
        for candidate in &candidates {
            candidate.set_accept_delay(ms(600));
        }
        // A goes silent: the lone connection turns poor after the stale floor and a round starts.
        wait_until("the first round", || !client.pool.remeasure_rounds().is_empty()).await;
        Timer::sleep(0.2).await;
        let started = candidates.iter().filter(|candidate| candidate.accepted() >= 2).count();
        assert_eq!(started, 4, "four connections of the round are in flight, not all six");
        wait_until("every candidate to be measured", || {
            candidates.iter().all(|candidate| candidate.accepted() >= 2)
        })
        .await;
    });
}

/// T38.6.4o.10: the view lists every server of the pool in the coin's rank with host-only labels, the tolerance, the
/// agreed tip, each height and lag, and `behind` for a stub more than the tolerance behind. It sends no request to
/// any stub.
#[test]
fn t38_6_4o_10_the_view_lists_the_pool_and_sends_nothing() {
    block_on(async {
        const H: u64 = 1_000;
        let (a, b, c) = (stub_at(5, H).await, stub_at(5, H).await, stub_at(5, H - 10).await);
        let d = StubServer::start(StubBehavior::Down).await;
        let client = checked_pool(
            &[&a, &b, &c, &d],
            PoolSetup {
                block_interval: Some(Duration::from_secs(60)),
                ..PoolSetup::new("VIEW10")
            },
            quiet_timings(),
            script_draw(&[], 0.5),
        );
        join_with_heights(&client, &[&a, &b, &c]).await;
        client.pool.ping_maintained().await;
        let stubs = [&a, &b, &c, &d];
        let sent = || stubs.iter().map(|stub| stub.messages().len()).collect::<Vec<_>>();
        let before = sent();
        let view = client.servers_view();
        assert_eq!(sent(), before, "the view sends nothing to any server");

        assert_eq!(view.coin, "VIEW10");
        assert_eq!(view.server_order, "listed");
        assert_eq!(view.tolerance_blocks, 3);
        assert_eq!(view.agreed_tip, Some(H));
        let labels: Vec<&str> = view.servers.iter().map(|server| server.server.as_str()).collect();
        assert_eq!(labels, [a.addr(), b.addr(), c.addr(), d.addr()]);
        assert_eq!(view.servers.iter().map(|server| server.rank).collect::<Vec<_>>(), vec![
            0, 1, 2, 3
        ]);
        assert_eq!(
            view.servers.iter().map(|server| server.height).collect::<Vec<_>>(),
            vec![Some(H), Some(H), Some(H - 10), None]
        );
        assert_eq!(view.servers.iter().map(|server| server.lag).collect::<Vec<_>>(), vec![
            Some(0),
            Some(0),
            Some(10),
            None
        ]);
        assert_eq!(
            view.servers.iter().map(|server| server.out_of_date).collect::<Vec<_>>(),
            vec![None, None, Some("behind"), None]
        );
        assert_eq!(
            view.servers.iter().map(|server| server.maintained).collect::<Vec<_>>(),
            vec![true, true, true, false]
        );
        // The up-to-date maintained servers are near; the one behind and the one that is not maintained are not.
        assert_eq!(view.servers.iter().map(|server| server.near).collect::<Vec<_>>(), vec![
            true, true, false, false
        ]);
        assert!(view.servers[..3].iter().all(|server| server.latency_ms.is_some()));
        assert!(view.servers[3].latency_ms.is_none());
        assert!(view
            .servers
            .iter()
            .all(|server| !server.poor_demoted && !server.removed));

        // The wire form of the chapter.
        let wire = serde_json::to_value(&view).unwrap();
        assert_eq!(wire["coin"], "VIEW10");
        assert_eq!(wire["tolerance_blocks"], 3);
        assert_eq!(wire["agreed_tip"], H);
        assert_eq!(wire["servers"][2]["out_of_date"], "behind");
        assert!(wire["servers"][0]["out_of_date"].is_null());
        assert!(wire["servers"][3]["height"].is_null() && wire["servers"][3]["lag"].is_null());
    });
}

// ---- CRD ch.38 field fix: a dead and a refusing server at activation (plan step 0) ------------------------------

const BALANCE: &str = "blockchain.scripthash.get_balance";
const SCRIPT_HASH: &str = "00000000000000000000000000000000000000000000000000000000000000aa";

/// A good stub that answers its header request and a balance.
async fn good_stub() -> StubServer {
    let good = stub_at(5, 100).await;
    good.set_result(BALANCE, json!({"confirmed": 7, "unconfirmed": 0}));
    good
}

/// T38.6.4.14: with three stubs, one that never completes a connection (it accepts the TCP connection and never
/// answers: a listener that never accepts cannot be told from it in-process, and an address that black-holes depends
/// on the network of the host), one that refuses every connection at once and one good one, `max_connected` 2 and a
/// draw that ranks the good stub last, under `mixed` and `random`: when the build returns the maintained set holds the
/// good stub, and a balance read with the caller's budget (20 s, scaled to 400 ms) is answered by it within that
/// budget, in each of 20 runs.
#[test]
fn t38_6_4_14_a_dead_and_a_refusing_server_at_activation() {
    block_on(async {
        let (dead, refusing) = (
            StubServer::start(StubBehavior::Silent).await,
            StubServer::start(StubBehavior::Down).await,
        );
        let good = good_stub().await;
        let budget = ms(400);
        for order in [ServerOrder::Mixed, ServerOrder::Fixed(vec![0, 1, 2])] {
            for run in 0..20 {
                let client = checked_pool(
                    &[&dead, &refusing, &good],
                    PoolSetup {
                        max_connected: Some(2),
                        order: order.clone(),
                        ..PoolSetup::new(TICKER)
                    },
                    test_timings(),
                    // The first shuffle leaves the listed order: the good stub ranks last.
                    script_draw(&identity(3), 0.5),
                );
                assert_eq!(
                    ranked_addrs(&client).last(),
                    Some(&good.addr()),
                    "{order:?} run {run}: the good stub ranks last"
                );
                let started = Instant::now();
                client.activation_probe().await.unwrap();
                assert!(
                    snap_of(&client, &good).maintained,
                    "{order:?} run {run}: the set holds the good stub when the build returns"
                );
                client.start_pool(false);
                let electrum = ElectrumClient(client.clone());
                let balance = with_call_budget(budget, electrum.scripthash_get_balance(SCRIPT_HASH).compat())
                    .await
                    .unwrap();
                assert_eq!(balance.confirmed, 7, "{order:?} run {run}");
                assert!(
                    started.elapsed() < budget,
                    "{order:?} run {run}: {:?}",
                    started.elapsed()
                );
            }
        }
        assert!(good.requests(BALANCE) >= 40);
        assert_eq!(dead.requests(BALANCE) + refusing.requests(BALANCE), 0);
    });
}

/// R38.6.4k-R item 3 (the regression behind T38.6.4.14): under `mixed` and `measured` the maintained set is not empty
/// when `activation_probe` returns, although every header answer is still on its way.
#[test]
fn t38_6_4l_2_the_set_is_not_empty_when_the_build_returns_under_a_measured_order() {
    block_on(async {
        for order in [ServerOrder::Mixed, ServerOrder::Measured] {
            let servers = [
                stub_at(300, 100).await,
                stub_at(300, 100).await,
                stub_at(300, 100).await,
            ];
            let client = measured_pool(
                &[&servers[0], &servers[1], &servers[2]],
                Some(2),
                order.clone(),
                lenient(),
                None,
                script_draw(&identity(3), 0.5),
            );
            let started = Instant::now();
            client.activation_probe().await.unwrap();
            assert!(
                started.elapsed() < ms(300),
                "{order:?}: the build waited for a header answer"
            );
            assert!(!maintained(&client).is_empty(), "{order:?}: the set is empty");
            // Each stub still gets exactly one header request: the probe's, not one on joining as well.
            wait_for_probe_end(&client).await;
            for server in &servers {
                assert_eq!(server.requests(HEADERS), 1, "{order:?}");
            }
            // The kept connections were measured: a server that joined at establishment has its answer as a sample.
            assert!(maintained(&client).len() <= 2);
            for server in &servers {
                let state = snap_of(&client, server);
                assert!(state.activation_ms.is_some(), "{order:?}: {state:?}");
                if state.maintained {
                    assert_eq!(state.samples, 1, "{order:?}: {state:?}");
                }
            }
        }
    });
}

/// T38.6.4j.1: with an empty maintained set, `max_connected` 2 and three servers in the visiting order dead (never
/// answers), refusing (fails at once) and good, the good server is asked as soon as the refusing one has failed, not
/// after the dead one's attempt ends, and answers the request.
#[test]
fn t38_6_4j_1_the_retry_is_a_sliding_window() {
    block_on(async {
        let dead = StubServer::start(StubBehavior::Silent).await;
        let refusing = StubServer::start(StubBehavior::Down).await;
        let good = good_stub().await;
        let client = checked_pool(
            &[&dead, &refusing, &good],
            PoolSetup {
                max_connected: Some(2),
                tip_checks: false,
                ..PoolSetup::new(TICKER)
            },
            test_timings(),
            script_draw(&[], 0.5),
        );
        assert!(maintained(&client).is_empty());
        let started = Instant::now();
        let answered = ask(&client, READ).await.unwrap();
        let elapsed = started.elapsed();
        assert_eq!(answered, good.addr());
        // The dead server's attempt ends at the 400 ms establishment bound; the good one answers long before.
        assert!(elapsed < ms(250), "{elapsed:?}");

        // Every server is named once when all of them fail (R38.6.4i), and the call ends within its budget.
        good.set_behavior(StubBehavior::Down).await;
        let client = checked_pool(
            &[&dead, &refusing, &good],
            PoolSetup {
                max_connected: Some(2),
                tip_checks: false,
                ..PoolSetup::new(TICKER)
            },
            ElectrumPoolTimings {
                call_budget: ms(1_000),
                ..test_timings()
            },
            script_draw(&[], 0.5),
        );
        let started = Instant::now();
        let failure = ask(&client, READ).await.unwrap_err();
        assert!(started.elapsed() < ms(1_000), "{:?}", started.elapsed());
        for stub in [&dead, &refusing, &good] {
            assert_eq!(failure.matches(&stub.addr()).count(), 1, "{failure}");
        }
    });
}

/// T38.6.4a.9: with two maintained stubs, one with a smoothed latency and one joined whose header request on joining
/// is not answered yet, every read-only query goes to the first; once the second answers its header request it has a
/// figure and is judged by the limit and the floor of two as usual.
#[test]
fn t38_6_4a_9_a_session_that_has_answered_nothing_takes_no_read() {
    block_on(async {
        let (first, second) = (stub_at(5, 100).await, stub_at(800, 100).await);
        let client = checked_pool(
            &[&first, &second],
            PoolSetup {
                near_factor: super::electrum_measure::NEAR_FACTOR_DEFAULT,
                ..PoolSetup::new("NOFIGURE")
            },
            lenient(),
            script_draw(&[], 0.5),
        );
        client.pool.run_pass().await;
        assert_eq!(maintained(&client).len(), 2);
        wait_until("the first stub's figure", || snap_of(&client, &first).samples == 1).await;
        // The second stub's header request is still unanswered: no figure, no height.
        let state = snap_of(&client, &second);
        assert_eq!((state.samples, state.height), (0, None), "{state:?}");
        let answered = answered_by(&client, 10).await;
        assert!(answered.iter().all(|addr| *addr == first.addr()), "{answered:?}");
        assert_eq!(second.requests(READ), 0);

        // The answer is a sample, so the second stub has a figure at once. It is far, but with two maintained
        // sessions the floor of two keeps both near: reads take turns again.
        wait_until("the second stub's figure", || snap_of(&client, &second).samples == 1).await;
        let figure = snap_of(&client, &second).smoothed_ms.unwrap();
        assert!(figure >= 800., "{figure}");
        let answered = answered_by(&client, 10).await;
        assert!(answered.contains(&second.addr()), "{answered:?}");
    });
}
