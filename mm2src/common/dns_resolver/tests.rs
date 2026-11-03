//! Tests of CRD ch.45 §45.6B (T45.16, T45.17, T45.20, and the common part of T45.19). The DNS server is a UDP socket and a
//! TCP listener on 127.0.0.1 that the test runs itself, built from the message types of `hickory-proto`. Real time is
//! replaced by the configured bounds ([`Options`]): the TTL clamps, the last known answer window and the lookup bound are
//! a second or two instead of an hour or a day, and `defaults_are_the_bounds_of_the_chapter` pins the real values. How
//! answers are read (CNAME chains, lifetimes, negative lifetimes) is tested on messages built by hand, with the real
//! bounds.

use super::hosts::Hosts as HostsFile;
use super::wire::{interpret, is_answer_to, MAX_CNAME_LINKS};
use super::*;
use crate::block_on;
use hickory_proto::op::{Message, MessageType, OpCode, Query, ResponseCode};
use hickory_proto::rr::rdata::{A, CNAME, SOA};
use hickory_proto::rr::{Name as DnsName, RData, Record};
use serde_json::json;
use std::io::{Read, Write};
use std::net::{TcpListener, UdpSocket};
use std::sync::atomic::AtomicUsize;

// ---- T45.16: the key ----

fn parse(value: serde_json::Value) -> Result<DnsMode, DnsResolverConfError> { parse_dns_resolver(Some(&value)) }

fn fixed(servers: &[&str]) -> DnsMode { DnsMode::Fixed(servers.iter().map(|s| s.parse().unwrap()).collect()) }

#[test]
fn absent_null_and_google_select_google_public_dns() {
    assert_eq!(parse_dns_resolver(None).unwrap(), DnsMode::Google);
    assert_eq!(parse(json!(null)).unwrap(), DnsMode::Google);
    assert_eq!(parse(json!("google")).unwrap(), DnsMode::Google);
}

#[test]
fn system_selects_the_system_resolver() {
    assert_eq!(parse(json!("system")).unwrap(), DnsMode::System);
}

#[test]
fn fixed_servers_keep_their_ports_and_their_order() {
    assert_eq!(parse(json!(["192.0.2.53"])).unwrap(), fixed(&["192.0.2.53:53"]));
    assert_eq!(parse(json!(["192.0.2.53:5353"])).unwrap(), fixed(&["192.0.2.53:5353"]));
    assert_eq!(parse(json!(["2001:db8::53"])).unwrap(), fixed(&["[2001:db8::53]:53"]));
    assert_eq!(parse(json!(["[2001:db8::53]"])).unwrap(), fixed(&["[2001:db8::53]:53"]));
    assert_eq!(
        parse(json!(["[2001:db8::53]:5353"])).unwrap(),
        fixed(&["[2001:db8::53]:5353"])
    );
    assert_eq!(
        parse(json!(["192.0.2.2", "[2001:db8::1]:5353", "192.0.2.1:1"])).unwrap(),
        fixed(&["192.0.2.2:53", "[2001:db8::1]:5353", "192.0.2.1:1"])
    );
    // Eight entries are the most.
    let eight: Vec<String> = (1..=8).map(|n| format!("192.0.2.{}", n)).collect();
    assert!(matches!(parse(json!(eight)).unwrap(), DnsMode::Fixed(servers) if servers.len() == 8));
}

#[test]
fn every_other_value_is_refused_with_an_error_that_names_the_key_and_the_value() {
    let nine: Vec<String> = (1..=9).map(|n| format!("192.0.2.{}", n)).collect();
    let invalid = [
        json!("Google"),
        json!("System"),
        json!(""),
        json!("192.0.2.53"),
        json!([]),
        json!([""]),
        json!(["dns.example.com"]),
        json!(["192.0.2.53:0"]),
        json!(["192.0.2.53:65536"]),
        json!(["192.0.2.53:"]),
        json!(["192.0.2.53:+53"]),
        json!(["[2001:db8::53"]),
        json!(["[2001:db8::53]x"]),
        json!(["[192.0.2.53]"]),
        json!([53]),
        json!(["192.0.2.53", 53]),
        json!(nine),
        json!(true),
        json!(false),
        json!(5),
        json!(1.5),
        json!({}),
        json!({"servers": ["192.0.2.53"]}),
    ];
    for value in invalid {
        let error = parse(value.clone()).expect_err(&format!("{} must be refused", value));
        let text = error.to_string();
        assert!(text.contains("\"dns_resolver\""), "{}: {}", value, text);
        assert!(text.contains(&value.to_string()), "{}: {}", value, text);
        assert!(
            text.contains("\"google\"") && text.contains("\"system\""),
            "{}: {}",
            value,
            text
        );
    }
}

// ---- the bounds ----

#[test]
fn defaults_are_the_bounds_of_the_chapter() {
    let options = Options::default();
    assert_eq!(options.query_timeout, Duration::from_secs(5));
    assert_eq!(options.lookup_bound, Duration::from_secs(15));
    assert_eq!(options.cache_capacity, 1024);
    assert_eq!(options.positive_min_ttl, Duration::from_secs(30));
    assert_eq!(options.positive_max_ttl, Duration::from_secs(3600));
    assert_eq!(options.negative_min_ttl, Duration::from_secs(5));
    assert_eq!(options.negative_max_ttl, Duration::from_secs(300));
    assert_eq!(options.last_known_capacity, 1024);
    assert_eq!(options.last_known_window, Duration::from_secs(24 * 60 * 60));
    assert_eq!(options.log_interval, Duration::from_secs(3600));
    assert_eq!(options.log_capacity, 1024);
}

#[test]
fn google_servers_are_the_four_addresses_in_order() {
    let servers: Vec<String> = google_servers().iter().map(SocketAddr::to_string).collect();
    assert_eq!(servers, [
        "8.8.8.8:53",
        "8.8.4.4:53",
        "[2001:4860:4860::8888]:53",
        "[2001:4860:4860::8844]:53"
    ]);
}

#[test]
fn the_oldest_name_is_dropped_first() {
    let mut map = OldestFirstMap::new(3);
    for name in ["a", "b", "c"] {
        map.insert(name.to_owned(), name.len());
    }
    // `a` is refreshed, so `b` is the oldest now.
    map.insert("a".to_owned(), 1);
    map.insert("d".to_owned(), 1);
    assert_eq!(map.len(), 3);
    assert!(map.get("b").is_none());
    assert!(map.get("a").is_some() && map.get("c").is_some() && map.get("d").is_some());
}

#[test]
fn a_warning_is_permitted_once_per_name_per_interval() {
    let start = Instant::now();
    let mut log = HourlyLog::new(Duration::from_secs(3600), 2);
    assert!(log.permit("a", start));
    assert!(!log.permit("a", start + Duration::from_secs(3599)));
    assert!(log.permit("b", start + Duration::from_secs(10)));
    assert!(log.permit("a", start + Duration::from_secs(3601)));
    // The record holds two names: `c` pushes out the oldest one, which may be logged again at once.
    assert!(log.permit("c", start + Duration::from_secs(3602)));
    assert!(log.permit("b", start + Duration::from_secs(3603)));
}

// ---- how an answer is read ----

fn name(text: &str) -> DnsName { DnsName::from_ascii(text).unwrap() }

fn a_record(owner: &str, ttl: u32, ip: &str) -> Record {
    Record::from_rdata(name(owner), ttl, RData::A(A(ip.parse().unwrap())))
}

fn cname_record(owner: &str, ttl: u32, target: &str) -> Record {
    Record::from_rdata(name(owner), ttl, RData::CNAME(CNAME(name(target))))
}

fn soa_record(ttl: u32, minimum: u32) -> Record {
    let soa = SOA::new(
        name("ns.example.test."),
        name("hostmaster.example.test."),
        1,
        3600,
        600,
        86_400,
        minimum,
    );
    Record::from_rdata(name("example.test."), ttl, RData::SOA(soa))
}

fn question(owner: &str) -> Question {
    Question {
        name: name(owner),
        record_type: RecordType::A,
    }
}

/// An answer to `question` with the given sections.
fn response(question: &Question, code: ResponseCode, answers: Vec<Record>, authorities: Vec<Record>) -> Message {
    let mut message = Message::response(77, OpCode::Query);
    message.metadata.response_code = code;
    message.queries = vec![Query::query(question.name.clone(), question.record_type)];
    message.answers = answers;
    message.authorities = authorities;
    message
}

fn read(question: &Question, message: &Message) -> Interpreted { interpret(message, question, &Options::default()) }

fn secs(n: u64) -> Duration { Duration::from_secs(n) }

#[test]
fn a_cname_chain_is_followed_inside_the_answer_and_the_least_lifetime_counts() {
    let asked = question("www.chain.test.");
    let message = response(
        &asked,
        ResponseCode::NoError,
        vec![
            cname_record("www.chain.test.", 300, "mid.chain.test."),
            cname_record("mid.chain.test.", 120, "final.chain.test."),
            a_record("final.chain.test.", 600, "192.0.2.1"),
            a_record("unrelated.test.", 5, "192.0.2.99"),
            a_record("final.chain.test.", 900, "192.0.2.2"),
        ],
        vec![],
    );
    assert_eq!(read(&asked, &message), Interpreted::Positive {
        addresses: vec!["192.0.2.1".parse().unwrap(), "192.0.2.2".parse().unwrap()],
        ttl: secs(120),
    });
}

#[test]
fn a_chain_of_more_than_eight_links_or_one_without_addresses_is_a_negative_answer() {
    let asked = question("c0.test.");
    // Nine links: the address belongs to the tenth name, which the eight links followed do not reach.
    let mut answers: Vec<Record> = (0..=MAX_CNAME_LINKS)
        .map(|n| cname_record(&format!("c{}.test.", n), 300, &format!("c{}.test.", n + 1)))
        .collect();
    answers.push(a_record(&format!("c{}.test.", MAX_CNAME_LINKS + 1), 300, "192.0.2.1"));
    let message = response(&asked, ResponseCode::NoError, answers, vec![]);
    assert!(matches!(read(&asked, &message), Interpreted::Negative {
        nxdomain: false,
        ..
    }));

    // Eight links are followed.
    let mut answers: Vec<Record> = (0..MAX_CNAME_LINKS)
        .map(|n| cname_record(&format!("c{}.test.", n), 300, &format!("c{}.test.", n + 1)))
        .collect();
    answers.push(a_record(&format!("c{}.test.", MAX_CNAME_LINKS), 300, "192.0.2.1"));
    let message = response(&asked, ResponseCode::NoError, answers, vec![]);
    assert!(matches!(read(&asked, &message), Interpreted::Positive { .. }));

    // A chain that ends in the answer section without addresses, and a loop.
    let message = response(
        &asked,
        ResponseCode::NoError,
        vec![cname_record("c0.test.", 300, "elsewhere.test.")],
        vec![],
    );
    assert!(matches!(read(&asked, &message), Interpreted::Negative {
        nxdomain: false,
        ..
    }));
    let message = response(
        &asked,
        ResponseCode::NoError,
        vec![
            cname_record("c0.test.", 300, "c1.test."),
            cname_record("c1.test.", 300, "c0.test."),
        ],
        vec![],
    );
    assert!(matches!(read(&asked, &message), Interpreted::Negative { .. }));
}

#[test]
fn only_the_records_of_the_type_asked_for_are_used() {
    let asked = Question {
        name: name("v6.test."),
        record_type: RecordType::AAAA,
    };
    let aaaa = Record::from_rdata(
        name("v6.test."),
        100,
        RData::AAAA(hickory_proto::rr::rdata::AAAA("2001:db8::1".parse().unwrap())),
    );
    let message = response(
        &asked,
        ResponseCode::NoError,
        vec![a_record("v6.test.", 100, "192.0.2.1"), aaaa],
        vec![],
    );
    assert_eq!(read(&asked, &message), Interpreted::Positive {
        addresses: vec!["2001:db8::1".parse().unwrap()],
        ttl: secs(100),
    });
}

#[test]
fn positive_lifetimes_are_clamped_to_30_seconds_and_an_hour() {
    let asked = question("ttl.test.");
    for (wire, kept) in [(0, 30), (29, 30), (31, 31), (3600, 3600), (86_400, 3600)] {
        let message = response(
            &asked,
            ResponseCode::NoError,
            vec![a_record("ttl.test.", wire, "192.0.2.1")],
            vec![],
        );
        assert_eq!(
            read(&asked, &message),
            Interpreted::Positive {
                addresses: vec!["192.0.2.1".parse().unwrap()],
                ttl: secs(kept),
            },
            "wire lifetime {}",
            wire
        );
    }
}

#[test]
fn a_negative_lifetime_is_the_least_of_the_soa_lifetime_and_minimum_clamped_to_5_seconds_and_5_minutes() {
    let asked = question("gone.test.");
    let negative = |code, authorities| match read(&asked, &response(&asked, code, vec![], authorities)) {
        Interpreted::Negative { nxdomain, ttl } => (nxdomain, ttl),
        other => panic!("not a negative answer: {:?}", other),
    };
    assert_eq!(
        negative(ResponseCode::NXDomain, vec![soa_record(100, 7)]),
        (true, secs(7))
    );
    assert_eq!(
        negative(ResponseCode::NXDomain, vec![soa_record(9, 1000)]),
        (true, secs(9))
    );
    assert_eq!(
        negative(ResponseCode::NXDomain, vec![soa_record(3, 1000)]),
        (true, secs(5))
    );
    assert_eq!(
        negative(ResponseCode::NXDomain, vec![soa_record(1000, 1000)]),
        (true, secs(300))
    );
    // No SOA: the floor. NOERROR with no record is NODATA.
    assert_eq!(negative(ResponseCode::NXDomain, vec![]), (true, secs(5)));
    assert_eq!(
        negative(ResponseCode::NoError, vec![soa_record(60, 40)]),
        (false, secs(40))
    );
}

#[test]
fn a_server_failure_or_a_refusal_goes_to_the_next_server() {
    let asked = question("fail.test.");
    for code in [
        ResponseCode::ServFail,
        ResponseCode::Refused,
        ResponseCode::FormErr,
        ResponseCode::NotImp,
    ] {
        assert!(
            matches!(
                read(&asked, &response(&asked, code, vec![], vec![])),
                Interpreted::ServerFailure(_)
            ),
            "{:?}",
            code
        );
    }
}

#[test]
fn an_answer_is_accepted_only_for_the_id_and_the_question_asked() {
    let asked = question("Mixed.Case.test.");
    let message = response(&asked, ResponseCode::NoError, vec![], vec![]);
    assert!(is_answer_to(&message, 77, &asked));
    assert!(!is_answer_to(&message, 78, &asked), "another ID");
    assert!(!is_answer_to(&message, 77, &question("other.test.")), "another name");
    let other_type = Question {
        name: asked.name.clone(),
        record_type: RecordType::AAAA,
    };
    assert!(!is_answer_to(&message, 77, &other_type), "another type");
    // The comparison of names ignores case.
    assert!(is_answer_to(&message, 77, &question("mixed.case.test.")));
    let mut query = message.clone();
    query.metadata.message_type = MessageType::Query;
    assert!(!is_answer_to(&query, 77, &asked), "a query is not an answer");
    let mut two = message;
    two.queries.push(Query::query(name("second.test."), RecordType::A));
    assert!(!is_answer_to(&two, 77, &asked), "two questions");
}

#[test]
fn names_are_absolute_and_non_ascii_names_go_through_idna() {
    let plain = absolute_name("seed.example.test").unwrap();
    assert!(plain.is_fqdn());
    assert_eq!(plain.to_ascii(), "seed.example.test.");
    assert_eq!(
        absolute_name("seed.example.test.").unwrap().to_ascii(),
        "seed.example.test."
    );
    assert_eq!(
        absolute_name("bücher.example").unwrap().to_ascii(),
        "xn--bcher-kva.example."
    );
    let too_long_label = "a".repeat(64);
    let too_long_name = format!("{}.test", "a".repeat(300));
    for invalid in ["", "a..b", too_long_label.as_str(), too_long_name.as_str()] {
        assert!(absolute_name(invalid).is_err(), "{:?}", invalid);
    }
}

// ---- the hosts file and localhost ----

#[test]
fn the_hosts_file_is_read_with_comments_aliases_case_and_both_address_families() {
    let hosts = HostsFile::parse(
        "# a comment\n\
         127.0.0.1\tlocalhost  Loopback.Local # trailing comment\n\
         \n\
         192.0.2.10 Web.Test www.web.test\n\
         192.0.2.11 web.test\n\
         2001:db8::10 web.test v6.web.test\r\n\
         fe80::1%lo0 zoned.test\n\
         not-an-address ignored.test\n\
         192.0.2.12\n\
            # indented comment\n\
         192.0.2.13 dotted.test.\n",
    );
    let ip = |s: &str| s.parse::<IpAddr>().unwrap();
    assert_eq!(
        hosts.lookup("loopback.local", DnsFamily::Any),
        Some(vec![ip("127.0.0.1")])
    );
    // Names are case-insensitive, an address is listed once per name, in the order of the file.
    assert_eq!(
        hosts.lookup("web.test", DnsFamily::V4),
        Some(vec![ip("192.0.2.10"), ip("192.0.2.11")])
    );
    assert_eq!(hosts.lookup("web.test", DnsFamily::V6), Some(vec![ip("2001:db8::10")]));
    // With no family given: IPv4 if there is any.
    assert_eq!(
        hosts.lookup("web.test", DnsFamily::Any),
        Some(vec![ip("192.0.2.10"), ip("192.0.2.11")])
    );
    assert_eq!(
        hosts.lookup("www.web.test", DnsFamily::V4),
        Some(vec![ip("192.0.2.10")])
    );
    assert_eq!(
        hosts.lookup("v6.web.test", DnsFamily::Any),
        Some(vec![ip("2001:db8::10")])
    );
    assert_eq!(hosts.lookup("zoned.test", DnsFamily::V6), Some(vec![ip("fe80::1")]));
    assert_eq!(hosts.lookup("dotted.test", DnsFamily::V4), Some(vec![ip("192.0.2.13")]));
    // A name the file lists for another family only goes to the servers.
    assert_eq!(hosts.lookup("www.web.test", DnsFamily::V6), None);
    assert_eq!(hosts.lookup("ignored.test", DnsFamily::Any), None);
    assert_eq!(hosts.lookup("unknown.test", DnsFamily::Any), None);
}

#[test]
fn the_system_hosts_file_loads_and_an_empty_one_lists_nothing() {
    // `load` never fails: whatever the host has, the result is usable (the file may be empty or absent).
    let _ = HostsFile::load().lookup("localhost", DnsFamily::Any);
    assert!(HostsFile::parse("").lookup("localhost", DnsFamily::Any).is_none());
}

// ---- T45.17: against a DNS server on a loopback address ----

#[derive(Clone, Copy, Debug)]
enum Reply {
    /// An A record with this lifetime for every A query; no answer (NODATA) to any other query.
    Answer {
        ttl: u32,
    },
    /// `name` CNAME `mid` CNAME `final`, the address on `final`, and an unrelated record that must be ignored.
    Cname {
        ttl: u32,
    },
    NxDomain,
    ServFail,
    /// The packet is read and not answered.
    Silent,
    /// Over UDP an empty answer with the TC bit; over TCP the full answer.
    Truncated,
    /// Over UDP three answers for each query: one with another ID, one for another question, then the right one.
    Bogus,
}

struct TestDns {
    addr: SocketAddr,
    reply: Arc<Mutex<Reply>>,
    udp_queries: Arc<AtomicUsize>,
    tcp_queries: Arc<AtomicUsize>,
    source_ports: Arc<Mutex<Vec<u16>>>,
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

const WRONG: Ipv4Addr = Ipv4Addr::new(203, 0, 113, 66);

fn base_reply(request: &Message) -> Message {
    let mut response = Message::response(request.metadata.id, OpCode::Query);
    response.metadata.recursion_desired = request.metadata.recursion_desired;
    response.metadata.recursion_available = true;
    response.queries = request.queries.clone();
    response
}

fn address_reply(request: &Message, ttl: u32, address: Ipv4Addr) -> Message {
    let mut response = base_reply(request);
    if let Some(query) = request.queries.first() {
        if query.query_type == RecordType::A {
            response
                .answers
                .push(Record::from_rdata(query.name.clone(), ttl, RData::A(A(address))));
        }
    }
    response
}

/// The datagrams (or TCP messages) the server sends for `request`.
fn build_replies(request: &Message, reply: Reply, address: Ipv4Addr, tcp: bool) -> Vec<Vec<u8>> {
    let mut messages = Vec::new();
    match reply {
        Reply::Silent => {},
        Reply::ServFail => {
            let mut response = base_reply(request);
            response.metadata.response_code = ResponseCode::ServFail;
            messages.push(response);
        },
        Reply::NxDomain => {
            let mut response = base_reply(request);
            response.metadata.response_code = ResponseCode::NXDomain;
            messages.push(response);
        },
        Reply::Answer { ttl } => messages.push(address_reply(request, ttl, address)),
        Reply::Cname { ttl } => {
            let mut response = base_reply(request);
            if let Some(query) = request.queries.first() {
                if query.query_type == RecordType::A {
                    response.answers = vec![
                        cname_record_of(query.name.clone(), ttl, "mid.cname.test."),
                        cname_record("mid.cname.test.", ttl, "final.cname.test."),
                        Record::from_rdata(name("final.cname.test."), ttl, RData::A(A(address))),
                        Record::from_rdata(name("unrelated.cname.test."), ttl, RData::A(A(WRONG))),
                    ];
                }
            }
            messages.push(response);
        },
        Reply::Truncated => {
            if tcp {
                messages.push(address_reply(request, 60, address));
            } else {
                let mut response = base_reply(request);
                response.metadata.truncation = true;
                messages.push(response);
            }
        },
        Reply::Bogus => {
            if !tcp {
                let mut wrong_id = address_reply(request, 60, WRONG);
                wrong_id.metadata.id = request.metadata.id.wrapping_add(1);
                messages.push(wrong_id);
                let mut wrong_question = address_reply(request, 60, WRONG);
                wrong_question.queries = vec![Query::query(name("another.question.test."), RecordType::A)];
                messages.push(wrong_question);
            }
            messages.push(address_reply(request, 60, address));
        },
    }
    messages.iter().filter_map(|message| message.to_vec().ok()).collect()
}

fn cname_record_of(owner: DnsName, ttl: u32, target: &str) -> Record {
    Record::from_rdata(owner, ttl, RData::CNAME(CNAME(name(target))))
}

impl TestDns {
    fn start(address: Ipv4Addr, reply: Reply) -> TestDns {
        // The UDP socket and the TCP listener share the port number, as a DNS server's do.
        let (udp, tcp) = (0..100)
            .find_map(|_| {
                let udp = UdpSocket::bind("127.0.0.1:0").ok()?;
                let tcp = TcpListener::bind(("127.0.0.1", udp.local_addr().ok()?.port())).ok()?;
                Some((udp, tcp))
            })
            .expect("a free port for both UDP and TCP");
        udp.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
        tcp.set_nonblocking(true).unwrap();
        let addr = udp.local_addr().unwrap();
        let reply = Arc::new(Mutex::new(reply));
        let udp_queries = Arc::new(AtomicUsize::new(0));
        let tcp_queries = Arc::new(AtomicUsize::new(0));
        let source_ports = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));

        let udp_thread = {
            let (reply, queries, ports, stop) =
                (reply.clone(), udp_queries.clone(), source_ports.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut buffer = [0u8; 1500];
                while !stop.load(Ordering::SeqCst) {
                    let Ok((size, peer)) = udp.recv_from(&mut buffer) else {
                        continue;
                    };
                    let Ok(request) = Message::from_vec(&buffer[..size]) else {
                        continue;
                    };
                    queries.fetch_add(1, Ordering::SeqCst);
                    ports.lock().push(peer.port());
                    let current = *reply.lock();
                    for bytes in build_replies(&request, current, address, false) {
                        let _ = udp.send_to(&bytes, peer);
                    }
                }
            })
        };
        let tcp_thread = {
            let (reply, queries, stop) = (reply.clone(), tcp_queries.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let Ok((mut stream, _)) = tcp.accept() else {
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    };
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
                    let mut prefix = [0u8; 2];
                    if stream.read_exact(&mut prefix).is_err() {
                        continue;
                    }
                    let mut body = vec![0u8; usize::from(u16::from_be_bytes(prefix))];
                    if stream.read_exact(&mut body).is_err() {
                        continue;
                    }
                    let Ok(request) = Message::from_vec(&body) else {
                        continue;
                    };
                    queries.fetch_add(1, Ordering::SeqCst);
                    let current = *reply.lock();
                    for bytes in build_replies(&request, current, address, true) {
                        let mut framed = (bytes.len() as u16).to_be_bytes().to_vec();
                        framed.extend_from_slice(&bytes);
                        let _ = stream.write_all(&framed);
                    }
                }
            })
        };
        TestDns {
            addr,
            reply,
            udp_queries,
            tcp_queries,
            source_ports,
            stop,
            threads: vec![udp_thread, tcp_thread],
        }
    }

    fn set_reply(&self, reply: Reply) { *self.reply.lock() = reply; }

    /// The UDP queries received.
    fn queries(&self) -> usize { self.udp_queries.load(Ordering::SeqCst) }

    fn tcp_queries(&self) -> usize { self.tcp_queries.load(Ordering::SeqCst) }
}

impl Drop for TestDns {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

/// The bounds a test runs with: a second or two stand in for an hour or a day.
fn scaled() -> Options {
    Options {
        query_timeout: Duration::from_millis(300),
        lookup_bound: Duration::from_secs(2),
        positive_min_ttl: Duration::from_secs(1),
        positive_max_ttl: Duration::from_secs(2),
        negative_min_ttl: Duration::from_secs(1),
        negative_max_ttl: Duration::from_secs(2),
        last_known_window: Duration::from_secs(3),
        ..Options::default()
    }
}

fn servers_installed(servers: &[SocketAddr], options: Options, hosts: HostsFile) -> Installed {
    let backend = ServerBackend::new(servers, options.clone(), hosts);
    Installed::new(
        DnsMode::Fixed(servers.to_vec()),
        Backend::Servers(Arc::new(backend)),
        &options,
    )
}

fn installed_for(server: &TestDns, options: Options) -> Installed {
    servers_installed(&[server.addr], options, HostsFile::default())
}

fn resolve(installed: &Installed, name: &str) -> io::Result<Vec<SocketAddr>> {
    block_on(installed.resolve(name, 80, DnsFamily::V4))
}

fn sleep(millis: u64) { std::thread::sleep(Duration::from_millis(millis)); }

const ANSWER: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 7);

fn answer_at(port: u16) -> Vec<SocketAddr> { vec![SocketAddr::new(IpAddr::V4(ANSWER), port)] }

fn backend(installed: &Installed) -> &Arc<ServerBackend> {
    match &installed.backend {
        Backend::Servers(backend) => backend,
        _ => panic!("not a servers backend"),
    }
}

#[test]
fn an_answer_with_a_lifetime_of_zero_is_cached_for_the_minimum() {
    let server = TestDns::start(ANSWER, Reply::Answer { ttl: 0 });
    let installed = installed_for(&server, scaled());
    assert_eq!(resolve(&installed, "zero.test").unwrap(), answer_at(80));
    assert_eq!(resolve(&installed, "zero.test").unwrap(), answer_at(80));
    assert_eq!(server.queries(), 1, "the second lookup is served from the cache");
    // The minimum (here 1 s) has passed: the name is asked again.
    sleep(1300);
    assert_eq!(resolve(&installed, "zero.test").unwrap(), answer_at(80));
    assert_eq!(server.queries(), 2);
}

#[test]
fn an_answer_with_a_long_lifetime_is_asked_again_after_the_maximum() {
    let server = TestDns::start(ANSWER, Reply::Answer { ttl: 86_400 });
    let installed = installed_for(&server, scaled());
    resolve(&installed, "day.test").unwrap();
    sleep(1300);
    resolve(&installed, "day.test").unwrap();
    assert_eq!(server.queries(), 1, "still cached before the maximum (here 2 s)");
    sleep(1000);
    resolve(&installed, "day.test").unwrap();
    assert_eq!(server.queries(), 2, "asked again after the maximum");
}

#[test]
fn a_negative_answer_is_cached_too() {
    let server = TestDns::start(ANSWER, Reply::NxDomain);
    let installed = installed_for(&server, scaled());
    assert_eq!(
        resolve(&installed, "none.test").unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(
        resolve(&installed, "none.test").unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(server.queries(), 1);
    sleep(1300);
    assert!(resolve(&installed, "none.test").is_err());
    assert_eq!(server.queries(), 2);
}

#[test]
fn the_cache_drops_the_oldest_entry_when_it_is_full() {
    // At the size of the chapter.
    let mut map = OldestFirstMap::new(Options::default().cache_capacity);
    for n in 0..1025 {
        map.insert(format!("name{}.test|a", n), n);
    }
    assert_eq!(map.len(), 1024);
    assert!(map.get("name0.test|a").is_none());
    assert!(map.get("name1.test|a").is_some() && map.get("name1024.test|a").is_some());

    // Through the lookups, with two entries.
    let server = TestDns::start(ANSWER, Reply::Answer { ttl: 60 });
    let installed = installed_for(&server, Options {
        cache_capacity: 2,
        ..scaled()
    });
    for name in ["one.test", "two.test", "three.test"] {
        resolve(&installed, name).unwrap();
    }
    assert_eq!(server.queries(), 3);
    resolve(&installed, "three.test").unwrap();
    resolve(&installed, "two.test").unwrap();
    assert_eq!(server.queries(), 3, "the newest two are cached");
    resolve(&installed, "one.test").unwrap();
    assert_eq!(server.queries(), 4, "the oldest was dropped");
}

#[test]
fn a_name_in_the_hosts_file_is_answered_without_a_query() {
    let server = TestDns::start(ANSWER, Reply::Answer { ttl: 60 });
    let hosts = HostsFile::parse("192.0.2.99 hosts.test alias.test\n");
    let installed = servers_installed(&[server.addr], scaled(), hosts);
    let expected = vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 99)), 80)];
    assert_eq!(resolve(&installed, "hosts.test").unwrap(), expected);
    assert_eq!(resolve(&installed, "ALIAS.test").unwrap(), expected);
    assert_eq!(server.queries(), 0);
    // An answer from the hosts file is not kept as a last known answer.
    assert_eq!(installed.gauges().last_known_entries, 0);
}

#[test]
fn an_address_literal_makes_no_query() {
    let server = TestDns::start(ANSWER, Reply::Answer { ttl: 60 });
    let installed = installed_for(&server, scaled());
    for literal in ["127.0.0.9", "[2001:db8::1]", "2001:db8::1"] {
        let found = block_on(installed.resolve(literal, 80, DnsFamily::Any)).unwrap();
        assert_eq!(found.len(), 1, "{}", literal);
        assert_eq!(found[0].port(), 80);
    }
    // A literal of the other family is not an address of the family asked for.
    assert!(block_on(installed.resolve("127.0.0.9", 80, DnsFamily::V6)).is_err());
    assert_eq!(server.queries(), 0);
    assert_eq!(installed.gauges().lookups, 0);
}

#[test]
fn localhost_and_its_subdomains_resolve_to_loopback_without_a_query() {
    let server = TestDns::start(ANSWER, Reply::Answer { ttl: 60 });
    // The hosts file lists no localhost, as the Windows default.
    let installed = installed_for(&server, scaled());
    let loopback = |ip: IpAddr| vec![SocketAddr::new(ip, 80)];
    let (v4, v6) = (IpAddr::V4(Ipv4Addr::LOCALHOST), IpAddr::V6(Ipv6Addr::LOCALHOST));
    for name in ["localhost", "LOCALHOST", "a.localhost", "x.y.Localhost.", "localhost."] {
        assert_eq!(
            block_on(installed.resolve(name, 80, DnsFamily::Any)).unwrap(),
            loopback(v4),
            "{}",
            name
        );
        assert_eq!(
            block_on(installed.resolve(name, 80, DnsFamily::V4)).unwrap(),
            loopback(v4),
            "{}",
            name
        );
        assert_eq!(
            block_on(installed.resolve(name, 80, DnsFamily::V6)).unwrap(),
            loopback(v6),
            "{}",
            name
        );
    }
    assert_eq!(server.queries(), 0);
    // Other names that merely contain the word are asked.
    resolve(&installed, "localhost.example.test").unwrap();
    resolve(&installed, "notlocalhost").unwrap();
    assert_eq!(server.queries(), 2);
}

#[test]
fn a_truncated_udp_answer_is_repeated_over_tcp() {
    let server = TestDns::start(ANSWER, Reply::Truncated);
    let installed = installed_for(&server, scaled());
    assert_eq!(resolve(&installed, "big.test").unwrap(), answer_at(80));
    assert_eq!(server.queries(), 1);
    assert_eq!(server.tcp_queries(), 1);
}

#[test]
fn a_first_server_that_does_not_answer_is_followed_by_the_second() {
    let silent = TestDns::start(ANSWER, Reply::Silent);
    let second = TestDns::start(ANSWER, Reply::Answer { ttl: 60 });
    let installed = servers_installed(&[silent.addr, second.addr], scaled(), HostsFile::default());
    assert_eq!(resolve(&installed, "two.test").unwrap(), answer_at(80));
    assert_eq!(silent.queries(), 1);
    assert_eq!(second.queries(), 1);
    // A server failure is followed by the next server too, an authoritative negative answer is not.
    silent.set_reply(Reply::ServFail);
    assert!(resolve(&installed, "three.test").is_ok());
    silent.set_reply(Reply::NxDomain);
    assert_eq!(
        resolve(&installed, "four.test").unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(
        second.queries(),
        2,
        "the second server was not asked for the name that does not exist"
    );
}

#[test]
fn a_cname_chain_in_the_answer_gives_the_address_of_its_end() {
    let server = TestDns::start(ANSWER, Reply::Cname { ttl: 60 });
    let installed = installed_for(&server, scaled());
    assert_eq!(resolve(&installed, "www.cname.test").unwrap(), answer_at(80));
    assert_eq!(server.queries(), 1, "the chain is not chased with new queries");
}

#[test]
fn an_answer_with_the_wrong_id_or_the_wrong_question_is_ignored() {
    let server = TestDns::start(ANSWER, Reply::Bogus);
    let installed = installed_for(&server, scaled());
    // The three datagrams per query arrive in this order; only the last one is for the query.
    assert_eq!(resolve(&installed, "bogus.test").unwrap(), answer_at(80));
}

#[test]
fn every_udp_query_comes_from_a_fresh_source_port() {
    let server = TestDns::start(ANSWER, Reply::Answer { ttl: 60 });
    let installed = installed_for(&server, scaled());
    for n in 0..8 {
        resolve(&installed, &format!("port{}.test", n)).unwrap();
    }
    let ports = server.source_ports.lock().clone();
    assert_eq!(ports.len(), 8);
    // The operating system draws the ephemeral port of each new socket at random.
    let distinct: std::collections::HashSet<u16> = ports.iter().copied().collect();
    assert!(distinct.len() >= 6, "source ports: {:?}", ports);
}

#[test]
fn the_last_known_answer_is_served_when_no_server_answers_and_is_logged_once() {
    let server = TestDns::start(ANSWER, Reply::Answer { ttl: 1 });
    let installed = installed_for(&server, scaled());
    assert_eq!(resolve(&installed, "stale.test").unwrap(), answer_at(80));
    assert_eq!(installed.gauges().last_known_entries, 1);

    // The server is gone and the answer has expired (1 s), at most 3 s ago.
    server.set_reply(Reply::Silent);
    sleep(1300);
    assert_eq!(resolve(&installed, "stale.test").unwrap(), answer_at(80));
    assert_eq!(resolve(&installed, "stale.test").unwrap(), answer_at(80));
    assert_eq!(installed.gauges().stale_served, 2);
    assert_eq!(installed.gauges().failures, 0);
    assert_eq!(
        backend(&installed).stale_warnings.load(Ordering::SeqCst),
        1,
        "one WARN line per name per hour"
    );
}

#[test]
fn a_server_failure_also_serves_the_last_known_answer() {
    let server = TestDns::start(ANSWER, Reply::Answer { ttl: 1 });
    let installed = installed_for(&server, scaled());
    assert_eq!(resolve(&installed, "servfail.test").unwrap(), answer_at(80));
    server.set_reply(Reply::ServFail);
    sleep(1300);
    assert_eq!(resolve(&installed, "servfail.test").unwrap(), answer_at(80));
    assert_eq!(installed.gauges().stale_served, 1);
}

#[test]
fn a_name_that_does_not_exist_is_never_replaced_by_the_last_known_answer() {
    let server = TestDns::start(ANSWER, Reply::Answer { ttl: 1 });
    let installed = installed_for(&server, scaled());
    resolve(&installed, "gone.test").unwrap();
    server.set_reply(Reply::NxDomain);
    sleep(1300);
    let error = resolve(&installed, "gone.test").unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert_eq!(installed.gauges().stale_served, 0);
    assert_eq!(installed.gauges().failures, 1);
    assert_eq!(backend(&installed).stale_warnings.load(Ordering::SeqCst), 0);
}

#[test]
fn an_answer_that_expired_more_than_the_window_ago_is_not_served() {
    let server = TestDns::start(ANSWER, Reply::Answer { ttl: 1 });
    let options = Options {
        last_known_window: Duration::from_millis(300),
        ..scaled()
    };
    let installed = installed_for(&server, options);
    resolve(&installed, "old.test").unwrap();
    server.set_reply(Reply::Silent);
    // Expired after 1 s, and 0.3 s later it is too old.
    sleep(1700);
    let error = resolve(&installed, "old.test").unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(installed.gauges().stale_served, 0);
}

#[test]
fn a_server_that_never_answers_fails_the_lookup_within_the_bound_and_is_logged_once() {
    let server = TestDns::start(ANSWER, Reply::Silent);
    let options = Options {
        lookup_bound: Duration::from_millis(600),
        ..scaled()
    };
    let installed = installed_for(&server, options);
    for _ in 0..3 {
        let started = Instant::now();
        let error = resolve(&installed, "silent.test").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
    }
    let gauges = installed.gauges();
    assert_eq!((gauges.lookups, gauges.failures, gauges.stale_served), (3, 3, 0));
    assert_eq!(
        installed.failure_warnings.load(Ordering::SeqCst),
        1,
        "the failure line appears once per name per hour"
    );
    // Another name has its own line.
    resolve(&installed, "silent-too.test").unwrap_err();
    assert_eq!(installed.failure_warnings.load(Ordering::SeqCst), 2);
}

#[test]
fn an_invalid_name_is_a_lookup_error_and_not_a_panic() {
    let server = TestDns::start(ANSWER, Reply::Answer { ttl: 60 });
    let installed = installed_for(&server, scaled());
    assert!(resolve(&installed, "bad..name").is_err());
    assert!(resolve(&installed, &"a".repeat(80)).is_err());
    assert_eq!(server.queries(), 0);
}

#[test]
fn only_the_family_asked_for_is_returned() {
    let server = TestDns::start(ANSWER, Reply::Answer { ttl: 60 });
    let installed = installed_for(&server, scaled());
    // The server has an A record and no AAAA record.
    assert_eq!(
        block_on(installed.resolve("fam.test", 80, DnsFamily::Any))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        server.queries(),
        1,
        "the AAAA record is asked for only when there is no A record"
    );
    let v6 = block_on(installed.resolve("fam.test", 80, DnsFamily::V6)).unwrap_err();
    assert_eq!(v6.kind(), io::ErrorKind::NotFound);
}

// ---- the system mode ----

#[test]
fn the_system_mode_asks_the_operating_system() {
    let installed = Installed::new(DnsMode::System, Backend::System, &Options::default());
    let found = block_on(installed.resolve("localhost", 80, DnsFamily::V4)).unwrap();
    assert!(
        found.contains(&SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 80)),
        "{:?}",
        found
    );
    assert_eq!(installed.gauges().lookups, 1);
    assert_eq!(installed.gauges().last_known_entries, 0);
}

// ---- T45.20: installation ----

#[test]
fn the_startup_line_names_the_mode() {
    let google = startup_line(&DnsMode::Google);
    assert!(
        google.contains("Google Public DNS") && google.contains("\"system\""),
        "{}",
        google
    );
    assert!(google.contains("8.8.8.8:53"), "{}", google);
    let system = startup_line(&DnsMode::System);
    assert!(system.contains("operating system"), "{}", system);
    let servers = startup_line(&fixed(&["192.0.2.2:53", "[2001:db8::1]:5353"]));
    assert!(servers.contains("192.0.2.2:53, [2001:db8::1]:5353"), "{}", servers);
}

#[test]
fn a_lookup_outside_the_node_uses_the_system_resolver() {
    let _guard = lock_resolver_for_test();
    assert!(gauges().is_none());
    let found = block_on(lookup("localhost", 80, DnsFamily::V4)).unwrap();
    assert!(found.contains(&SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 80)));
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "before the resolver was installed")]
fn a_lookup_in_the_node_before_the_installation_fails_the_assertion() {
    let _guard = lock_resolver_for_test();
    mark_node_started();
    let _ = block_on(lookup("localhost", 80, DnsFamily::V4));
}

#[test]
fn a_start_logs_its_line_once_and_a_second_start_replaces_the_resolver() {
    let _guard = lock_resolver_for_test();
    mark_node_started();
    let lines_before = STARTUP_LINES.load(Ordering::SeqCst);

    install(&DnsMode::System).unwrap();
    assert_eq!(STARTUP_LINES.load(Ordering::SeqCst), lines_before + 1);
    block_on(lookup("localhost", 80, DnsFamily::V4)).unwrap();
    assert_eq!(gauges().unwrap().lookups, 1);

    // The second start installs its own resolver, with counters and last known answers of its own.
    install(&DnsMode::Google).unwrap();
    assert_eq!(STARTUP_LINES.load(Ordering::SeqCst), lines_before + 2);
    assert_eq!(gauges().unwrap(), DnsGauges {
        lookups: 0,
        failures: 0,
        stale_served: 0,
        last_known_entries: 0
    });
    install(&fixed(&["127.0.0.1:9"])).unwrap();
    assert_eq!(STARTUP_LINES.load(Ordering::SeqCst), lines_before + 3);
}

#[test]
fn a_later_installation_takes_effect_for_the_lookups_after_it() {
    let first = install_test_map(&[("moves.test", &[IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1))])]);
    assert_eq!(block_on(lookup("moves.test", 80, DnsFamily::Any)).unwrap(), vec![
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 80)
    ]);
    drop(first);
    let _second = install_test_map(&[("moves.test", &[IpAddr::V4(Ipv4Addr::new(192, 0, 2, 2))])]);
    assert_eq!(block_on(lookup("moves.test", 80, DnsFamily::Any)).unwrap(), vec![
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 2)), 80)
    ]);
}

// ---- T45.19, the common part: the shared HTTP client and the TCP connection ----

#[test]
fn an_http_request_through_the_shared_client_resolves_the_name_through_the_installed_resolver() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        use std::io::{Read, Write};
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        let mut chunk = [0u8; 256];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let size = stream.read(&mut chunk).unwrap();
            assert!(size > 0, "the client closed the connection");
            request.extend_from_slice(&chunk[..size]);
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok")
            .unwrap();
        String::from_utf8_lossy(&request).into_owned()
    });

    let _guard = install_test_map(&[("web.test", &[IpAddr::V4(Ipv4Addr::LOCALHOST)])]);
    let request = hyper::Request::builder()
        .uri(format!("http://web.test:{}/ping", port))
        .body(http_body_util::Full::new(hyper::body::Bytes::new()))
        .unwrap();
    let response = block_on(crate::wio::HYPER.request(request)).unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(gauges().unwrap().lookups, 1);
    let received = server.join().unwrap();
    assert!(received.contains("host: web.test"), "{}", received);
}

#[test]
fn a_tcp_connection_tries_every_address_of_the_name_in_turn() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    // 127.0.0.2 is a loopback address too, and nothing listens on it: the first address refuses the connection.
    let _guard = install_test_map(&[("two.test", &[
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2)),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
    ])]);
    let stream = block_on(connect_tcp("two.test", port)).unwrap();
    assert_eq!(
        stream.peer_addr().unwrap(),
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    );
    assert_eq!(gauges().unwrap().lookups, 1);
}
