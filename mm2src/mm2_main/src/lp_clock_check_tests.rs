//! Tests of the clock check (CRD ch.45 §45.8, T45.23 to T45.31). None sends a packet to a real time server: they use a
//! stub time source, byte buffers, or a loopback socket.

use super::*;
use common::block_on;
use mm2_core::mm_ctx::MmCtxBuilder;
use serde_json::json;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

fn server(host: &str, port: u16) -> NtpServer {
    NtpServer {
        host: host.to_owned(),
        port,
    }
}

fn default_servers() -> Vec<NtpServer> { DEFAULT_NTP_HOSTS.iter().map(|host| server(host, 123)).collect() }

/// The NTP timestamp of a time in Unix seconds.
fn ts(unix_secs: f64) -> NtpTimestamp { NtpTimestamp::from_unix(Duration::from_secs_f64(unix_secs)) }

/// A stub time source: each host answers the offset given for it, or never answers. It counts its queries.
struct StubSource {
    offsets: HashMap<String, f64>,
    queries: AtomicUsize,
}

impl StubSource {
    fn new(offsets: &[(&str, f64)]) -> StubSource {
        StubSource {
            offsets: offsets
                .iter()
                .map(|(host, offset)| ((*host).to_owned(), *offset))
                .collect(),
            queries: AtomicUsize::new(0),
        }
    }

    fn silent() -> StubSource { StubSource::new(&[]) }

    fn queries(&self) -> usize { self.queries.load(Ordering::SeqCst) }
}

#[async_trait]
impl TimeSource for StubSource {
    async fn query(&self, server: &NtpServer) -> Option<f64> {
        self.queries.fetch_add(1, Ordering::SeqCst);
        self.offsets.get(&server.host).copied()
    }
}

fn conf_of(hosts: &[&str], required: bool) -> ClockConf {
    ClockConf {
        servers: hosts.iter().map(|host| server(host, 123)).collect(),
        required,
    }
}

fn new_ctx() -> MmArc { MmCtxBuilder::new().into_mm_arc() }

// ---- T45.23 ----------------------------------------------------------------------------------------------------

#[test]
fn t45_23_clock_check_keys_are_parsed() {
    let servers = |value: Json| parse_ntp_servers(Some(&value));
    assert_eq!(parse_ntp_servers(None).unwrap(), default_servers());
    assert_eq!(parse_ntp_servers(Some(&Json::Null)).unwrap(), default_servers());
    assert_eq!(servers(json!([])).unwrap(), Vec::<NtpServer>::new());
    for (value, expected) in [
        (json!(["time.example.com"]), server("time.example.com", 123)),
        (json!(["time.example.com:1123"]), server("time.example.com", 1123)),
        (json!(["192.0.2.1"]), server("192.0.2.1", 123)),
        (json!(["192.0.2.1:99"]), server("192.0.2.1", 99)),
        (json!(["2001:db8::1"]), server("2001:db8::1", 123)),
        (json!(["[2001:db8::1]"]), server("2001:db8::1", 123)),
        (json!(["[2001:db8::1]:1123"]), server("2001:db8::1", 1123)),
        (json!(["time.example.com:65535"]), server("time.example.com", 65535)),
    ] {
        assert_eq!(servers(value.clone()).unwrap(), vec![expected], "{value}");
    }
    assert_eq!(
        servers(json!(["b.example", "a.example:5", "[::1]"])).unwrap(),
        vec![server("b.example", 123), server("a.example", 5), server("::1", 123)],
        "the order given is kept"
    );
    assert_eq!(
        servers(json!(["1", "2", "3", "4", "5", "6", "7", "8"])).unwrap().len(),
        8
    );

    for value in [
        json!("pool.ntp.org"),
        json!([""]),
        json!(["time.example.com:0"]),
        json!(["time.example.com:65536"]),
        json!(["time.example.com:"]),
        json!(["time.example.com:12x"]),
        json!(["time.example.com:12:34"]),
        json!(["[2001:db8::1"]),
        json!(["[2001:db8::1]x"]),
        json!(["[192.0.2.1]"]),
        json!(["bad host"]),
        json!([123]),
        json!(["1", "2", "3", "4", "5", "6", "7", "8", "9"]),
        json!(true),
        json!(5),
        json!({}),
    ] {
        let error = servers(value.clone()).unwrap_err();
        assert_eq!(error.key, NTP_SERVERS_KEY, "{value}");
        let text = error.to_string();
        assert!(text.contains("\"ntp_servers\""), "{text}");
        assert!(text.contains(&value.to_string()), "the value is repeated: {text}");
    }

    assert!(!parse_ntp_required(None).unwrap());
    assert!(!parse_ntp_required(Some(&Json::Null)).unwrap());
    assert!(parse_ntp_required(Some(&json!(true))).unwrap());
    assert!(!parse_ntp_required(Some(&json!(false))).unwrap());
    for value in [json!("true"), json!(1), json!({})] {
        let error = parse_ntp_required(Some(&value)).unwrap_err();
        assert_eq!(error.key, NTP_REQUIRED_KEY);
        assert!(error.to_string().contains("\"ntp_required\""), "{error}");
    }
}

/// An invalid value refuses the launch with a clean configuration error that names the key (R45.7.1).
#[test]
fn t45_23_an_invalid_key_refuses_the_launch() {
    for (conf, field) in [
        (json!({ "ntp_servers": "pool.ntp.org" }), NTP_SERVERS_KEY),
        (json!({ "ntp_servers": [""] }), NTP_SERVERS_KEY),
        (json!({ "ntp_required": "true" }), NTP_REQUIRED_KEY),
    ] {
        let error = read_clock_conf(&conf).unwrap_err().into_inner();
        match error {
            MmInitError::ErrorDeserializingConfig { field: named, error } => {
                assert_eq!(named, field);
                assert!(error.contains(field), "{error}");
            },
            other => panic!("{conf}: {other:?}"),
        }
    }
    let conf = read_clock_conf(&json!({ "ntp_servers": ["time.example.com"], "ntp_required": true })).unwrap();
    assert_eq!(conf.servers, vec![server("time.example.com", 123)]);
    assert!(conf.required);
    assert_eq!(read_clock_conf(&json!({})).unwrap().servers, default_servers());
}

// ---- T45.24 ----------------------------------------------------------------------------------------------------

/// An answer built from fixed times: version 4, the given leap indicator, mode and stratum.
fn answer(
    leap_indicator: u8,
    mode: u8,
    stratum: u8,
    originate: NtpTimestamp,
    received: NtpTimestamp,
    transmitted: NtpTimestamp,
) -> Vec<u8> {
    let mut packet = vec![0u8; NTP_PACKET_LEN];
    packet[0] = (leap_indicator << 6) | (4 << 3) | mode;
    packet[1] = stratum;
    packet[24..32].copy_from_slice(&originate.0.to_be_bytes());
    packet[32..40].copy_from_slice(&received.0.to_be_bytes());
    packet[40..48].copy_from_slice(&transmitted.0.to_be_bytes());
    packet
}

const NOW: f64 = 1_800_000_000.0;

#[test]
fn t45_24_the_request_is_a_version_4_client_packet() {
    let request = build_request(ts(NOW));
    assert_eq!(request.len(), 48);
    assert_eq!(request[0], 0x23, "version 4, client mode, no leap warning");
    assert_eq!(&request[40..48], &ts(NOW).0.to_be_bytes());
    assert!(request[1..40].iter().all(|byte| *byte == 0));
    // The NTP era offset: 1970-01-01 is 2,208,988,800 s after 1900-01-01.
    assert_eq!(
        NtpTimestamp::from_unix(Duration::ZERO),
        NtpTimestamp(2_208_988_800 << 32)
    );
}

#[test]
fn t45_24_a_valid_answer_gives_the_offset_of_rfc_5905() {
    let sent = ts(NOW);
    // The server's clock is 5 s ahead of ours, so ours is behind: the offset is positive. 0.1 s there, 0.1 s back.
    let received = ts(NOW + 0.3);
    let good = answer(0, 4, 2, sent, ts(NOW + 5.1), ts(NOW + 5.2));
    let offset = parse_answer(&good, sent, received).unwrap();
    assert!((offset - 5.0).abs() < 1e-6, "{offset}");
    // The server's clock is 3 s behind ours: the offset is negative.
    let behind = answer(0, 4, 1, sent, ts(NOW - 2.9), ts(NOW - 2.8));
    let offset = parse_answer(&behind, sent, ts(NOW + 0.3)).unwrap();
    assert!((offset + 3.0).abs() < 1e-6, "{offset}");
    // Stratum 15, the longest the rule accepts, and an answer longer than 48 bytes (extension fields).
    let mut long = answer(1, 4, 15, sent, ts(NOW + 5.1), ts(NOW + 5.2));
    long.extend_from_slice(&[0u8; 20]);
    assert!(parse_answer(&long, sent, received).is_some());
}

#[test]
fn t45_24_the_offset_is_right_across_an_era_change() {
    // Our clock reads the last second of an NTP era, the server's the first second of the next: the local clock is
    // 2 s behind.
    let before = NtpTimestamp(0xFFFF_FFFF_u64 << 32);
    let after = NtpTimestamp(1_u64 << 32);
    assert!((offset_seconds(before, after, after, before) - 2.0).abs() < 1e-9);
}

#[test]
fn t45_24_anything_but_a_valid_answer_is_no_answer() {
    let sent = ts(NOW);
    let received = ts(NOW + 0.3);
    let (t2, t3) = (ts(NOW + 5.1), ts(NOW + 5.2));
    let good = answer(0, 4, 2, sent, t2, t3);
    assert!(parse_answer(&good, sent, received).is_some());
    let bad: Vec<(&str, Vec<u8>)> = vec![
        ("47 bytes", good[..47].to_vec()),
        ("empty", Vec::new()),
        ("client mode", answer(0, 3, 2, sent, t2, t3)),
        ("broadcast mode", answer(0, 5, 2, sent, t2, t3)),
        ("symmetric passive mode", answer(0, 2, 2, sent, t2, t3)),
        ("stratum 0", answer(0, 4, 0, sent, t2, t3)),
        ("stratum 16", answer(0, 4, 16, sent, t2, t3)),
        ("not synchronized", answer(3, 4, 2, sent, t2, t3)),
        ("zero transmit time", answer(0, 4, 2, sent, t2, NtpTimestamp(0))),
        ("another originate", answer(0, 4, 2, ts(NOW + 1.0), t2, t3)),
        ("zero originate", answer(0, 4, 2, NtpTimestamp(0), t2, t3)),
    ];
    for (what, bytes) in bad {
        assert_eq!(parse_answer(&bytes, sent, received), None, "{what}");
    }
}

// ---- T45.25 ----------------------------------------------------------------------------------------------------

#[test]
fn t45_25_the_median_outvotes_one_outlier() {
    assert_eq!(median(&[0.1, 0.2, 100.0]), Some(0.2));
    assert_eq!(
        median(&[100.0, 0.2, 0.1]),
        Some(0.2),
        "the order of the answers does not matter"
    );
    let even = median(&[0.1, 0.2, 0.3, 100.0]).unwrap();
    assert!((even - 0.25).abs() < 1e-12, "{even}");
    assert_eq!(median(&[-4.5]), Some(-4.5));
    assert_eq!(median(&[]), None);
    assert_eq!(evaluate(&[0.1, 0.2, 100.0]), CheckResult::Passed {
        offset: 0.2,
        answers: 3
    });
}

// ---- T45.26 ----------------------------------------------------------------------------------------------------

#[test]
fn t45_26_a_start_within_the_limit_goes_on_with_the_info_line() {
    for offset in [3.0, -3.0, 10.0, -10.0, 0.0] {
        for variant in [StartFailureOutcome::Stop, StartFailureOutcome::RestrictedStart] {
            let ctx = new_ctx();
            let source = StubSource::new(&[("a.example", offset)]);
            let lines = block_on(start_check(&ctx, &conf_of(&["a.example"], false), &source, variant)).unwrap();
            assert_eq!(lines.len(), 1, "{offset} {variant:?}");
            assert_eq!(lines[0].level, LineLevel::Info);
            assert!(lines[0].text.contains("passed"), "{}", lines[0].text);
            assert_eq!(clock_refusal(&ctx), None, "{offset} {variant:?}");
        }
    }
}

#[test]
fn t45_26_a_start_above_the_limit_stops_under_variant_a() {
    for (offset, direction) in [(10.5, "behind"), (-10.5, "ahead of")] {
        let ctx = new_ctx();
        let source = StubSource::new(&[("a.example", offset), ("b.example", offset)]);
        let conf = conf_of(&["a.example", "b.example", "c.example"], false);
        let error = block_on(start_check(&ctx, &conf, &source, StartFailureOutcome::Stop))
            .unwrap_err()
            .into_inner();
        match error {
            MmInitError::ClockCheckFailed(text) => {
                assert!(text.contains("10.5 s"), "the offset: {text}");
                assert!(text.contains(direction), "the direction: {text}");
                assert!(text.contains("limit is 10 s"), "the limit: {text}");
                assert!(text.contains("2 answer(s)"), "the number of answers: {text}");
                assert!(text.contains("must be set right"), "{text}");
            },
            other => panic!("{other:?}"),
        }
        assert_eq!(source.queries(), 3, "every server is asked");
        assert_eq!(clock_refusal(&ctx), None, "a stopped launch leaves no refusal behind");
    }
}

#[test]
fn t45_26_a_start_above_the_limit_goes_on_restricted_under_variant_b() {
    for (offset, direction) in [(10.5, "behind"), (-10.5, "ahead of")] {
        let ctx = new_ctx();
        let source = StubSource::new(&[("a.example", offset)]);
        let lines = block_on(start_check(
            &ctx,
            &conf_of(&["a.example"], false),
            &source,
            StartFailureOutcome::RestrictedStart,
        ))
        .unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].level, LineLevel::Error);
        let refusal = clock_refusal(&ctx).unwrap();
        assert!(refusal.contains("10.5 s") && refusal.contains(direction), "{refusal}");
        assert!(lines[0].text.contains(&refusal), "{}", lines[0].text);
        // The run-time loop then checks every 5 minutes, because the refusal is in force.
        let state = ClockCheckCtx::from_ctx(&ctx).unwrap();
        let (refused, last_check) = state.refused_and_last_check();
        assert!(refused && last_check.is_some());
    }
}

#[test]
fn t45_26_the_production_variant_is_stop_until_decision_d1() {
    assert_eq!(START_FAILURE_OUTCOME, StartFailureOutcome::Stop);
}

// ---- T45.27 ----------------------------------------------------------------------------------------------------

#[test]
fn t45_27_no_answer_with_ntp_required_false_warns_and_goes_on() {
    for variant in [StartFailureOutcome::Stop, StartFailureOutcome::RestrictedStart] {
        let ctx = new_ctx();
        let source = StubSource::silent();
        let lines = block_on(start_check(
            &ctx,
            &conf_of(&["a.example", "b.example"], false),
            &source,
            variant,
        ))
        .unwrap();
        assert_eq!(lines.len(), 1, "one WARN line");
        assert_eq!(lines[0].level, LineLevel::Warn);
        assert!(lines[0].text.contains("No time server answered"), "{}", lines[0].text);
        assert!(lines[0].text.contains("ntp_servers") && lines[0].text.contains("ntp_required"));
        assert_eq!(clock_refusal(&ctx), None);
    }
}

#[test]
fn t45_27_no_answer_with_ntp_required_true_gives_the_outcome_of_a_failed_check() {
    let conf = conf_of(&["a.example"], true);
    let ctx = new_ctx();
    let error = block_on(start_check(
        &ctx,
        &conf,
        &StubSource::silent(),
        StartFailureOutcome::Stop,
    ))
    .unwrap_err()
    .into_inner();
    match error {
        MmInitError::ClockCheckFailed(text) => assert!(text.contains("No time server answered"), "{text}"),
        other => panic!("{other:?}"),
    }
    let ctx = new_ctx();
    let lines = block_on(start_check(
        &ctx,
        &conf,
        &StubSource::silent(),
        StartFailureOutcome::RestrictedStart,
    ))
    .unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].level, LineLevel::Error);
    assert!(clock_refusal(&ctx).unwrap().contains("No time server answered"));
}

// ---- T45.28 ----------------------------------------------------------------------------------------------------

#[test]
fn t45_28_with_the_check_off_no_query_is_sent() {
    for required in [false, true] {
        for variant in [StartFailureOutcome::Stop, StartFailureOutcome::RestrictedStart] {
            let ctx = new_ctx();
            let source = StubSource::new(&[("a.example", 500.0)]);
            let conf = ClockConf {
                servers: Vec::new(),
                required,
            };
            let lines = block_on(start_check(&ctx, &conf, &source, variant)).unwrap();
            assert_eq!(lines.len(), 1, "the INFO line appears once");
            assert_eq!(lines[0].level, LineLevel::Info);
            assert!(lines[0].text.contains("clock check is off"), "{}", lines[0].text);
            assert_eq!(source.queries(), 0, "no query at start");
            assert_eq!(clock_refusal(&ctx), None);

            // Nor later: the run-time loop ends at once.
            block_on(clock_check_loop(
                ctx.weak(),
                Arc::new(StubSource::new(&[("a.example", 500.0)])),
            ));
            assert_eq!(clock_refusal(&ctx), None);
        }
    }
    // `[]` in the configuration is the off state, and with it `ntp_required` has no effect.
    let conf = read_clock_conf(&json!({ "ntp_servers": [], "ntp_required": true })).unwrap();
    assert!(conf.is_off() && conf.required);
}

#[test]
fn t45_28_the_run_time_loop_sends_no_query_when_off() {
    let ctx = new_ctx();
    let source = Arc::new(StubSource::silent());
    ClockCheckCtx::from_ctx(&ctx).unwrap().configure(&ClockConf::off());
    block_on(clock_check_loop(ctx.weak(), source.clone()));
    assert_eq!(source.queries(), 0);
}

// ---- T45.29 (the state; the gate at the order and swap starts is tested in ordermatch_tests) ----------------------

fn failed(offset: f64) -> CheckResult { CheckResult::Failed { offset, answers: 2 } }

fn passed(offset: f64) -> CheckResult { CheckResult::Passed { offset, answers: 2 } }

#[test]
fn t45_29_a_failed_check_starts_the_refusal_once_and_a_passing_check_ends_it_once() {
    let ctx = new_ctx();
    let state = ClockCheckCtx::from_ctx(&ctx).unwrap();
    let now = Instant::now();
    assert!(
        state.apply_runtime_result(&passed(0.5), false, now).is_empty(),
        "a pass logs nothing"
    );
    assert_eq!(clock_refusal(&ctx), None);

    let lines = state.apply_runtime_result(&failed(12.0), false, now);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].level, LineLevel::Error);
    let refusal = clock_refusal(&ctx).unwrap();
    assert!(
        refusal.contains("12.0 s") && refusal.contains("limit is 10 s"),
        "{refusal}"
    );

    assert!(
        state.apply_runtime_result(&failed(13.0), false, now).is_empty(),
        "a check during the refusal that fails again logs nothing new"
    );
    assert!(
        clock_refusal(&ctx).unwrap().contains("13.0 s"),
        "the refusal names the latest offset"
    );

    let lines = state.apply_runtime_result(&passed(1.25), false, now);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].level, LineLevel::Info);
    assert!(
        lines[0].text.contains("allowed again") && lines[0].text.contains("+1.250"),
        "{}",
        lines[0].text
    );
    assert_eq!(clock_refusal(&ctx), None);
    assert!(
        state.apply_runtime_result(&passed(1.0), false, now).is_empty(),
        "the INFO line appears once"
    );
}

#[test]
fn t45_29_no_answer_refuses_only_with_ntp_required_true() {
    let ctx = new_ctx();
    let state = ClockCheckCtx::from_ctx(&ctx).unwrap();
    let now = Instant::now();

    // ntp_required false: no state change, the WARN line at most once per 24 hours.
    let lines = state.apply_runtime_result(&CheckResult::NoAnswer, false, now);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].level, LineLevel::Warn);
    assert_eq!(clock_refusal(&ctx), None);
    assert!(state
        .apply_runtime_result(&CheckResult::NoAnswer, false, now + Duration::from_secs(3600))
        .is_empty());
    assert!(state
        .apply_runtime_result(&CheckResult::NoAnswer, false, now + Duration::from_secs(24 * 3600 - 1))
        .is_empty());
    assert_eq!(
        state
            .apply_runtime_result(&CheckResult::NoAnswer, false, now + Duration::from_secs(24 * 3600))
            .len(),
        1,
        "the WARN line comes again after 24 hours"
    );
    assert_eq!(clock_refusal(&ctx), None);

    // ntp_required true: the refusal begins and ends like that of a failed check.
    let lines = state.apply_runtime_result(&CheckResult::NoAnswer, true, now);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].level, LineLevel::Error);
    assert!(clock_refusal(&ctx).unwrap().contains("No time server answered"));
    assert!(state.apply_runtime_result(&CheckResult::NoAnswer, true, now).is_empty());
    // No answer with ntp_required false does not end a refusal that is in force.
    state.apply_runtime_result(&CheckResult::NoAnswer, false, now);
    assert!(clock_refusal(&ctx).is_some());
    assert_eq!(state.apply_runtime_result(&passed(0.0), true, now).len(), 1);
    assert_eq!(clock_refusal(&ctx), None);
}

#[test]
fn t45_29_each_node_has_its_own_state() {
    let (first, second) = (new_ctx(), new_ctx());
    ClockCheckCtx::from_ctx(&first)
        .unwrap()
        .apply_runtime_result(&failed(30.0), false, Instant::now());
    assert!(clock_refusal(&first).is_some());
    assert_eq!(clock_refusal(&second), None);
}

// ---- T45.30 ----------------------------------------------------------------------------------------------------

fn tick(wall_elapsed: f64, mono_elapsed: f64, since_last_check: f64, refused: bool) -> TickTimes {
    TickTimes {
        wall_elapsed,
        mono_elapsed,
        since_last_check,
        refused,
    }
}

#[test]
fn t45_30_when_the_node_checks() {
    // The two clocks moved by the same amount, nothing else due: no check.
    assert!(!check_due(&tick(60.0, 60.0, 100.0, false)));
    assert!(!check_due(&tick(60.3, 60.1, 100.0, false)));
    // A wall-clock jump of 11 s (forward or back) checks at once; one of exactly 10 s does not.
    assert!(check_due(&tick(71.0, 60.0, 100.0, false)));
    assert!(check_due(&tick(49.0, 60.0, 100.0, false)));
    assert!(!check_due(&tick(70.0, 60.0, 100.0, false)));
    // A tick that came 61 s late checks at once (the clocks agree); 60 s late does not.
    assert!(check_due(&tick(121.0, 121.0, 100.0, false)));
    assert!(!check_due(&tick(120.0, 120.0, 100.0, false)));
    // An hour after the last check a check runs.
    assert!(!check_due(&tick(60.0, 60.0, 3599.0, false)));
    assert!(check_due(&tick(60.0, 60.0, 3600.0, false)));
    // During a refusal the interval is 5 minutes.
    assert!(!check_due(&tick(60.0, 60.0, 299.0, true)));
    assert!(check_due(&tick(60.0, 60.0, 300.0, true)));
    assert!(!check_due(&tick(60.0, 60.0, 299.0, false)));
}

// ---- T45.31 ----------------------------------------------------------------------------------------------------

#[test]
fn t45_31_the_default_list_is_the_four_pool_hosts() {
    assert_eq!(DEFAULT_NTP_HOSTS, [
        "0.pool.ntp.org",
        "1.pool.ntp.org",
        "2.pool.ntp.org",
        "3.pool.ntp.org"
    ]);
    assert_eq!(NTP_PORT, 123);
    assert_eq!(CLOCK_LIMIT_SECS, 10.0);
}

#[test]
fn t45_31_a_check_asks_every_server_and_takes_the_median() {
    let source = StubSource::new(&[("a.example", 0.1), ("b.example", 0.2), ("c.example", 100.0)]);
    let servers = conf_of(&["a.example", "b.example", "c.example", "d.example"], false).servers;
    let result = block_on(run_check(&source, &servers));
    assert_eq!(source.queries(), 4);
    assert_eq!(result, CheckResult::Passed {
        offset: 0.2,
        answers: 3
    });
    assert_eq!(
        block_on(run_check(&StubSource::silent(), &servers)),
        CheckResult::NoAnswer
    );
}

/// The real source against a server on the loopback interface: no packet leaves the machine. A junk answer from the right
/// address is ignored, and so is an answer from another socket; the valid answer then gives the offset.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn t45_31_the_udp_source_talks_to_a_loopback_server() {
    use std::net::UdpSocket;

    let listener = UdpSocket::bind("127.0.0.1:0").unwrap();
    listener.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let port = listener.local_addr().unwrap().port();
    let stranger = UdpSocket::bind("127.0.0.1:0").unwrap();
    let thread = std::thread::spawn(move || {
        let mut buffer = [0u8; 512];
        let (len, from) = listener.recv_from(&mut buffer).unwrap();
        assert_eq!(len, NTP_PACKET_LEN);
        assert_eq!(buffer[0], 0x23);
        let sent = NtpTimestamp::read(&buffer[40..48]).unwrap();
        // From another port: not the address the query went to.
        stranger
            .send_to(&answer(0, 4, 2, sent, ts(NOW), ts(NOW)), from)
            .unwrap();
        // From the right address but not an answer: the originate field is wrong.
        listener
            .send_to(&answer(0, 4, 2, NtpTimestamp(7), ts(NOW), ts(NOW)), from)
            .unwrap();
        // The answer: the server's clock is 4 s ahead of the one that sent the query.
        let server_time = NtpTimestamp::now().unwrap().0.wrapping_add(4_u64 << 32);
        let server_time = NtpTimestamp(server_time);
        listener
            .send_to(&answer(0, 4, 2, sent, server_time, server_time), from)
            .unwrap();
    });
    let offset = block_on(UdpTimeSource.query(&server("127.0.0.1", port))).unwrap();
    thread.join().unwrap();
    assert!((offset - 4.0).abs() < 0.5, "{offset}");
}

/// A host that does not answer gives no answer within the time it has; it never blocks the check longer.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn t45_31_a_silent_loopback_server_gives_no_answer() {
    use std::net::UdpSocket;

    let silent = UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = silent.local_addr().unwrap().port();
    let started = Instant::now();
    assert_eq!(block_on(UdpTimeSource.query(&server("127.0.0.1", port))), None);
    let waited = started.elapsed().as_secs_f64();
    assert!(
        (PER_SERVER_TIMEOUT_SECS - 0.5..PER_SERVER_TIMEOUT_SECS + 2.0).contains(&waited),
        "{waited}"
    );
}
