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
    let mut client = ElectrumClientImpl::new(TICKER.into(), vec![]);
    client.configure_pool(ElectrumPoolOptions {
        min_connected,
        max_connected,
        client_name: Some(CLIENT_NAME.into()),
        timings,
        order,
    });
    // A server that cannot be used stays in the pool as removed; the tests look at the snapshot.
    let _ = client.add_servers(&requests);
    Arc::new(client)
}

fn rpc(method: &str, params: Vec<Json>) -> JsonRpcRequestEnum {
    static IDS: TestCounter = TestCounter::new(1_000_000);
    JsonRpcRequestEnum::Single(JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: format!("t{}", IDS.fetch_add(1, AtomicOrdering::Relaxed)),
        method: method.into(),
        params,
    })
}

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

/// R38.6.4b-R item 1: `random` is the default, `listed` keeps the listed order, anything else is `random` with
/// one warning naming the switch and the value.
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

    for (value, shown) in [
        (json!("mixed"), "mixed"),
        (json!("measured"), "measured"),
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
