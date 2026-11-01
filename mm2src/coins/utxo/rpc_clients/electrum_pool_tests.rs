//! Tests of the Electrum connection pool against scripted stub servers (CRD ch.38 T38.6.4.1 - T38.6.4.13-R).
//!
//! The pool is built with millisecond timings, so no test waits real seconds. The production figures are
//! pinned in `electrum_pool::budget_tests`; the 12-hour suspension cap is tested there with a clock stub.

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

/// Like [`pool_client_ordered`], with `electrum_spread_reads` (R38.6.4a item 5) set as given.
fn pool_client_configured(
    requests: Vec<ElectrumRpcRequest>,
    min_connected: usize,
    max_connected: Option<usize>,
    timings: ElectrumPoolTimings,
    order: ServerOrder,
    spread_reads: bool,
) -> Arc<ElectrumClientImpl> {
    let mut client = ElectrumClientImpl::new(TICKER.into(), vec![]);
    client.configure_pool(ElectrumPoolOptions {
        min_connected,
        max_connected,
        client_name: Some(CLIENT_NAME.into()),
        timings,
        order,
        spread_reads,
        block_interval: None,
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
            timeout_sec: None,
        };
        let unparseable = ElectrumRpcRequest {
            url: "not-an-address".into(),
            protocol: Default::default(),
            disable_cert_verification: false,
            timeout_sec: None,
        };
        let unresolvable = ElectrumRpcRequest {
            url: "no-such-host.invalid:50001".into(),
            protocol: Default::default(),
            disable_cert_verification: false,
            timeout_sec: None,
        };
        // A TLS address needs a host name; an IP literal has none.
        let tls_without_a_name = ElectrumRpcRequest {
            url: "127.0.0.1:50002".into(),
            protocol: ElectrumProtocol::SSL,
            disable_cert_verification: false,
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
    wait_until("the probe to end", || {
        client.pool_snapshot().iter().all(|server| {
            server.maintained || (!server.connected && (server.removed || server.suspended_until.is_some()))
        })
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

/// A pool over `servers` with a measured (or other) `order`, whose random choices come from `draw`.
fn measured_pool(
    servers: &[&StubServer],
    max_connected: Option<usize>,
    order: ServerOrder,
    timings: ElectrumPoolTimings,
    block_interval: Option<Duration>,
    draw: impl FnMut() -> f64 + Send + 'static,
) -> Arc<ElectrumClientImpl> {
    let mut client = ElectrumClientImpl::new(TICKER.into(), vec![]);
    client.configure_pool(ElectrumPoolOptions {
        min_connected: 1,
        max_connected,
        client_name: Some(CLIENT_NAME.into()),
        timings,
        order,
        spread_reads: false,
        block_interval,
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

/// T38.6.4l.1: `mixed` and `measured` parse as themselves; under `random` and `listed` no stub gets a header
/// request and no trigger runs.
#[test]
fn t38_6_4l_1_random_and_listed_measure_nothing() {
    block_on(async {
        for order in [ServerOrder::Random, ServerOrder::Listed] {
            let servers = [stub_at(5, 100).await, stub_at(5, 100).await, stub_at(5, 100).await];
            let client = measured_pool(
                &[&servers[0], &servers[1], &servers[2]],
                None,
                order.clone(),
                test_timings(),
                None,
                script_draw(&[], 0.5),
            );
            activate_and_start(&client).await;
            // A slow request is not a sample, and a re-measure round does not run.
            servers[0].set_reply_delay("blockchain.block.header", ms(150));
            for _ in 0..6 {
                ask(&client, "blockchain.block.header").await.unwrap();
            }
            client.pool.run_remeasure_round().await;
            Timer::sleep(0.2).await;
            for server in &servers {
                assert_eq!(server.requests(HEADERS), 0, "{order:?}");
            }
            assert!(client.pool.remeasure_rounds().is_empty(), "{order:?}");
            assert!(client
                .pool_snapshot()
                .iter()
                .all(|state| state.samples == 0 && !state.poor));
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

/// T38.6.4l.3: a server two blocks behind ranks after every eligible one.
#[test]
fn t38_6_4l_3_a_server_two_blocks_behind_ranks_last() {
    block_on(async {
        // The lagging server is the fastest.
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
        let rank = rank_of(&client);
        assert_eq!(rank[2], servers[2].addr(), "{rank:?}");
        assert!(rank[..2].contains(&servers[0].addr()) && rank[..2].contains(&servers[1].addr()));
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
/// measuring connection, gets exactly one header request right after joining, and P1 then covers it; under
/// `random` the same join sends none.
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
            // The request is not a sample of the smoothed latency, and no re-measure round ran.
            assert!(client.pool_snapshot().iter().all(|state| state.samples == 0));
            assert!(client.pool.remeasure_rounds().is_empty());

            // P1 covers the joined connection: two consecutive notifications 2 blocks behind trigger it.
            b.push_header(98);
            Timer::sleep(0.15).await;
            assert!(!snap_of(&client, &b).poor, "{order:?}");
            b.push_header(98);
            wait_until("the lagging connection to be marked poor", || snap_of(&client, &b).poor).await;
            assert!(!snap_of(&client, &a).poor, "{order:?}");
        }

        // Under `random` the same join sends no header request.
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
        // Four slow samples (asked at once, so they take as long as one): not enough.
        ask_together(&client, "blockchain.block.header", 4).await;
        wait_until("four samples", || snap_of(&client, &a).samples == 4).await;
        let state = snap_of(&client, &a);
        assert!(!state.poor, "{state:?}");
        // The fifth makes it poor.
        ask(&client, "blockchain.block.header").await.unwrap();
        wait_until("the slow connection to be marked poor", || snap_of(&client, &a).poor).await;
        let first = snap_of(&client, &a);
        assert!(first.smoothed_ms.unwrap() > 1_000.);

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

/// T38.6.4l.13 and T38.6.4l.11: with every candidate two or more blocks behind, the poor server stays
/// maintained, the rank is unchanged, and the next round starts no earlier than the limit later.
#[test]
fn t38_6_4l_13_without_an_eligible_candidate_the_poor_server_stays() {
    block_on(async {
        let a = stub_at(10, 100).await;
        let (b, c) = (stub_at(10, 95).await, stub_at(10, 95).await);
        // A re-measure limit of 2 s: the second round starts that long after the first, long after the first
        // round's connections were made, so the counts below are not read while a round is under way.
        let client = measured_pool(
            &[&a, &b, &c],
            Some(1),
            ServerOrder::Measured,
            ElectrumPoolTimings {
                remeasure_limit: ms(2_000),
                ..lenient()
            },
            None,
            script_draw(&identity(3), 0.5),
        );
        client.activation_probe().await.unwrap();
        wait_for_probe_end(&client).await;
        let rank = rank_of(&client);
        assert_eq!(rank[0], a.addr());
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
        assert_eq!(maintained(&client), vec![0]);
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
