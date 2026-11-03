//! One resolver for every hostname the native daemon looks up (CRD chapter 45 section 45.6B).
//!
//! The node's start-up reads the `dns_resolver` key ([`parse_dns_resolver`], R45.6B.2) and installs the resolver of its
//! mode ([`install`], R45.6B.8). From then on every connection the node opens by name asks [`lookup`]:
//!
//! * the P2P transport (`mm2_p2p::resolving_transport`, R45.6B.9 item 1) and the peer-exchange names (item 2),
//! * the shared HTTP client and the Tendermint RPC client, through [`SharedDnsResolve`] (item 3),
//! * the Electrum servers, through [`connect_tcp`] (item 4),
//! * the lightwalletd servers, through the HTTP connector of [`crate::wio::http_connector`] (item 5).
//!
//! In the Google and the fixed-servers modes the lookups go to the listed DNS servers only, in their order, over UDP with
//! a TCP retry on truncation. The messages are built and read with the types of `hickory-proto` and sent over tokio
//! sockets; the cache, the hosts file, the `localhost` rule and the last known answers are the node's own (R45.6B.3 to
//! R45.6B.5, R45.6B.11, D45.8). There is no fallback to any other server or to the system resolver. In the system mode
//! every name goes to the operating system's own lookup, with nothing of the node's own around it.

use self::hosts::Hosts;
use self::wire::{absolute_name, Interpreted, Question};
use crate::wio::CORE;
use hickory_proto::rr::RecordType;
use hyper_util::client::legacy::connect::dns::Name;
use parking_lot::{Mutex, RwLock};
use serde_json::Value as Json;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

mod hosts;
mod wire;

/// The configuration key that selects the resolver (R45.6B.2).
pub const DNS_RESOLVER_KEY: &str = "dns_resolver";

/// The most DNS servers the fixed mode accepts (R45.6B.2).
pub const MAX_FIXED_SERVERS: usize = 8;

const DEFAULT_DNS_PORT: u16 = 53;

/// The servers of the Google mode, in the order they are preferred (R45.6B.2).
pub fn google_servers() -> Vec<SocketAddr> {
    vec![
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), DEFAULT_DNS_PORT),
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 4, 4)), DEFAULT_DNS_PORT),
        SocketAddr::new(
            IpAddr::V6(Ipv6Addr::new(0x2001, 0x4860, 0x4860, 0, 0, 0, 0, 0x8888)),
            DEFAULT_DNS_PORT,
        ),
        SocketAddr::new(
            IpAddr::V6(Ipv6Addr::new(0x2001, 0x4860, 0x4860, 0, 0, 0, 0, 0x8844)),
            DEFAULT_DNS_PORT,
        ),
    ]
}

/// The mode of the resolver (R45.6B.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DnsMode {
    /// Google Public DNS, the default.
    Google,
    /// The operating system's own lookup.
    System,
    /// Only the listed servers, preferred in the order given.
    Fixed(Vec<SocketAddr>),
}

impl fmt::Display for DnsMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DnsMode::Google => f.write_str("Google Public DNS"),
            DnsMode::System => f.write_str("the system resolver"),
            DnsMode::Fixed(servers) => write!(f, "the fixed servers {}", join_servers(servers)),
        }
    }
}

fn join_servers(servers: &[SocketAddr]) -> String {
    servers.iter().map(SocketAddr::to_string).collect::<Vec<_>>().join(", ")
}

/// The configuration error of an invalid `dns_resolver` value: the launch is refused (R45.6B.2, R45.7.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DnsResolverConfError {
    value: String,
    reason: String,
}

impl fmt::Display for DnsResolverConfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid value {} for the configuration key \"{}\": {}. Accepted forms: \"google\" (the default, also an \
             absent key or null), \"system\", or a non-empty array of at most {} DNS servers, each \"<IPv4>\", \
             \"<IPv4>:<port>\", \"<IPv6>\", \"[<IPv6>]\" or \"[<IPv6>]:<port>\" with the port 1 to 65535 (default {})",
            self.value, DNS_RESOLVER_KEY, self.reason, MAX_FIXED_SERVERS, DEFAULT_DNS_PORT
        )
    }
}

impl std::error::Error for DnsResolverConfError {}

/// Reads the `dns_resolver` key (R45.6B.2). `None` and `null` select the Google mode. Anything else that is not one of
/// the accepted forms is an error, never replaced by the default: a resolver the operator did not choose would send
/// the names to servers the operator meant to avoid.
pub fn parse_dns_resolver(value: Option<&Json>) -> Result<DnsMode, DnsResolverConfError> {
    let value = match value {
        None | Some(Json::Null) => return Ok(DnsMode::Google),
        Some(value) => value,
    };
    let error = |reason: &str| DnsResolverConfError {
        value: value.to_string(),
        reason: reason.to_owned(),
    };
    match value {
        Json::String(keyword) => match keyword.as_str() {
            "google" => Ok(DnsMode::Google),
            "system" => Ok(DnsMode::System),
            _ => Err(error("a string must be exactly \"google\" or \"system\", in lowercase")),
        },
        Json::Array(entries) => {
            if entries.is_empty() {
                return Err(error("the array of DNS servers is empty"));
            }
            if entries.len() > MAX_FIXED_SERVERS {
                return Err(error(&format!(
                    "the array has {} entries, at most {} are accepted",
                    entries.len(),
                    MAX_FIXED_SERVERS
                )));
            }
            let mut servers = Vec::with_capacity(entries.len());
            for (index, entry) in entries.iter().enumerate() {
                let entry = entry
                    .as_str()
                    .ok_or_else(|| error(&format!("entry {} is not a string", index + 1)))?;
                let server = parse_server(entry).map_err(|why| {
                    error(&format!(
                        "entry {} (\"{}\") is not a DNS server address: {}",
                        index + 1,
                        entry,
                        why
                    ))
                })?;
                servers.push(server);
            }
            Ok(DnsMode::Fixed(servers))
        },
        _ => Err(error("the value must be a string or an array of strings")),
    }
}

/// One entry of the fixed mode: `<IPv4>`, `<IPv4>:<port>`, `<IPv6>`, `[<IPv6>]` or `[<IPv6>]:<port>`.
fn parse_server(entry: &str) -> Result<SocketAddr, &'static str> {
    if let Some(rest) = entry.strip_prefix('[') {
        let (address, tail) = rest.split_once(']').ok_or("the closing ']' is missing")?;
        let address: Ipv6Addr = address
            .parse()
            .map_err(|_| "the text in brackets is not an IPv6 address")?;
        let port = if tail.is_empty() {
            DEFAULT_DNS_PORT
        } else {
            parse_port(tail.strip_prefix(':').ok_or("unexpected text after ']'")?)?
        };
        return Ok(SocketAddr::new(IpAddr::V6(address), port));
    }
    if let Ok(address) = entry.parse::<Ipv6Addr>() {
        return Ok(SocketAddr::new(IpAddr::V6(address), DEFAULT_DNS_PORT));
    }
    let (host, port) = match entry.split_once(':') {
        Some((host, port)) => (host, parse_port(port)?),
        None => (entry, DEFAULT_DNS_PORT),
    };
    let address: Ipv4Addr = host.parse().map_err(|_| "it is not an IPv4 or IPv6 address")?;
    Ok(SocketAddr::new(IpAddr::V4(address), port))
}

fn parse_port(port: &str) -> Result<u16, &'static str> {
    if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("the port is not a number");
    }
    match port.parse::<u16>() {
        Ok(0) | Err(_) => Err("the port must be 1 to 65535"),
        Ok(port) => Ok(port),
    }
}

/// The address family a lookup asks for (R45.6B.3 item 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DnsFamily {
    /// No family given: the IPv4 addresses of the name if it has any, and otherwise its IPv6 addresses (the system
    /// mode returns what the operating system returns).
    Any,
    /// `/dns4`: IPv4 only.
    V4,
    /// `/dns6`: IPv6 only.
    V6,
}

impl DnsFamily {
    fn index(self) -> usize {
        match self {
            DnsFamily::Any => 0,
            DnsFamily::V4 => 1,
            DnsFamily::V6 => 2,
        }
    }

    /// The addresses of this family out of `addresses`, in their order. With no family given: the IPv4 addresses if there
    /// are any, and otherwise the IPv6 ones (R45.6B.3 item 1).
    fn select(self, addresses: &[IpAddr]) -> Vec<IpAddr> {
        let of_type =
            |wanted: fn(&IpAddr) -> bool| -> Vec<IpAddr> { addresses.iter().copied().filter(wanted).collect() };
        match self {
            DnsFamily::V4 => of_type(IpAddr::is_ipv4),
            DnsFamily::V6 => of_type(IpAddr::is_ipv6),
            DnsFamily::Any => {
                let v4 = of_type(IpAddr::is_ipv4);
                if v4.is_empty() {
                    of_type(IpAddr::is_ipv6)
                } else {
                    v4
                }
            },
        }
    }

    fn admits(self, address: &IpAddr) -> bool {
        match self {
            DnsFamily::Any => true,
            DnsFamily::V4 => address.is_ipv4(),
            DnsFamily::V6 => address.is_ipv6(),
        }
    }
}

/// The four gauges of R45.6B.10, each since the resolver was installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DnsGauges {
    /// `dns.lookups.count`: the lookups of names (address literals make none).
    pub lookups: u64,
    /// `dns.failures.count`: the lookups that returned an error.
    pub failures: u64,
    /// `dns.stale_served.count`: the lookups answered with the last known answer.
    pub stale_served: u64,
    /// `dns.last_known.entries`: the names the last known answers are kept for.
    pub last_known_entries: u64,
}

/// The values the Google and the fixed mode run with (R45.6B.3 to R45.6B.6). Only the unit tests change them, to stand
/// in for real time.
#[derive(Clone, Debug)]
struct Options {
    /// How long one query waits for an answer (R45.6B.3).
    query_timeout: Duration,
    /// The bound of a lookup as a whole (R45.6B.3).
    lookup_bound: Duration,
    /// The entries the cache holds (R45.6B.4).
    cache_capacity: usize,
    positive_min_ttl: Duration,
    positive_max_ttl: Duration,
    negative_min_ttl: Duration,
    negative_max_ttl: Duration,
    /// The names the last known answers are kept for (R45.6B.5).
    last_known_capacity: usize,
    /// How long after its expiry the last known answer of a name is still served (R45.6B.5 item 1).
    last_known_window: Duration,
    /// The least time between two WARN lines about the same name (R45.6B.5 item 3, R45.6B.6).
    log_interval: Duration,
    /// The names the record of logged names holds (R45.6B.6).
    log_capacity: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            query_timeout: Duration::from_secs(5),
            lookup_bound: Duration::from_secs(15),
            cache_capacity: 1024,
            positive_min_ttl: Duration::from_secs(30),
            positive_max_ttl: Duration::from_secs(60 * 60),
            negative_min_ttl: Duration::from_secs(5),
            negative_max_ttl: Duration::from_secs(5 * 60),
            last_known_capacity: 1024,
            last_known_window: Duration::from_secs(24 * 60 * 60),
            log_interval: Duration::from_secs(60 * 60),
            log_capacity: 1024,
        }
    }
}

/// A map of at most `capacity` names that drops the oldest one when a new name finds it full.
struct OldestFirstMap<V> {
    capacity: usize,
    next_sequence: u64,
    by_name: HashMap<String, (u64, V)>,
    by_sequence: BTreeMap<u64, String>,
}

impl<V> OldestFirstMap<V> {
    fn new(capacity: usize) -> Self {
        OldestFirstMap {
            capacity,
            next_sequence: 0,
            by_name: HashMap::new(),
            by_sequence: BTreeMap::new(),
        }
    }

    fn len(&self) -> usize { self.by_name.len() }

    fn get(&self, name: &str) -> Option<&V> { self.by_name.get(name).map(|(_, value)| value) }

    /// Stores `value` as the newest entry of `name`. A new name drops the oldest entry when the map is full.
    fn insert(&mut self, name: String, value: V) {
        if let Some((old_sequence, _)) = self.by_name.remove(&name) {
            self.by_sequence.remove(&old_sequence);
        } else if self.by_name.len() >= self.capacity {
            if let Some((_, oldest)) = self.by_sequence.pop_first() {
                self.by_name.remove(&oldest);
            }
        }
        self.next_sequence += 1;
        self.by_sequence.insert(self.next_sequence, name.clone());
        self.by_name.insert(name, (self.next_sequence, value));
    }
}

impl<V: Default + Clone> OldestFirstMap<V> {
    /// Changes the value of `name` (a default one if there is none) and makes it the newest entry.
    fn update(&mut self, name: &str, change: impl FnOnce(&mut V)) {
        let mut value = self.get(name).cloned().unwrap_or_default();
        change(&mut value);
        self.insert(name.to_owned(), value);
    }
}

/// At most one WARN line per name per interval, for at most `capacity` names (R45.6B.5 item 3, R45.6B.6).
struct HourlyLog {
    interval: Duration,
    last_logged: OldestFirstMap<Instant>,
}

impl HourlyLog {
    fn new(interval: Duration, capacity: usize) -> Self {
        HourlyLog {
            interval,
            last_logged: OldestFirstMap::new(capacity),
        }
    }

    /// Whether the line about `name` may be logged at `now`. A permitted line is recorded.
    fn permit(&mut self, name: &str, now: Instant) -> bool {
        if let Some(previous) = self.last_logged.get(name) {
            if now.saturating_duration_since(*previous) < self.interval {
                return false;
            }
        }
        self.last_logged.insert(name.to_owned(), now);
        true
    }
}

/// Why a lookup failed, as far as R45.6B.5 needs to tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FailureKind {
    /// A server answered that the name does not exist or has no address: authoritative, never replaced by the last
    /// known answer.
    NotFound,
    /// No server answered (no answer in time, connection refused, network unreachable) or a server reported its own
    /// failure (a server failure or a refusal): the last known answer may be served.
    NoServer,
    /// Anything else (a malformed name or answer, for example).
    Other,
}

struct Failure {
    kind: FailureKind,
    text: String,
}

impl Failure {
    fn into_io_error(self) -> io::Error {
        let kind = match self.kind {
            FailureKind::NotFound => io::ErrorKind::NotFound,
            FailureKind::NoServer => io::ErrorKind::TimedOut,
            FailureKind::Other => io::ErrorKind::Other,
        };
        io::Error::new(kind, self.text)
    }
}

/// The successful answer of a name, kept as the last known answer (R45.6B.5).
#[derive(Clone)]
struct Answer {
    addresses: Vec<IpAddr>,
    /// When the cache entry the answer came from expires.
    valid_until: Instant,
    /// When the answer was received.
    stored_at: Instant,
}

/// The last known answers of one name, one per family asked for.
#[derive(Clone, Default)]
struct KnownAnswers([Option<Answer>; 3]);

struct Found {
    addresses: Vec<IpAddr>,
    /// The addresses are the last known answer (R45.6B.5).
    stale: bool,
}

/// What the cache keeps for a name and a record type (R45.6B.4).
#[derive(Clone)]
enum Cached {
    Positive(Vec<IpAddr>),
    /// A negative answer: the name does not exist (`nxdomain`) or has no address of the type.
    Negative {
        nxdomain: bool,
    },
}

#[derive(Clone)]
struct CacheEntry {
    result: Cached,
    valid_until: Instant,
}

/// The localhost addresses of the family (R45.6B.3 item 3).
fn localhost_addresses(name: &str, family: DnsFamily) -> Option<Vec<IpAddr>> {
    if name == "localhost" || name.ends_with(".localhost") {
        Some(family.select(&[IpAddr::V4(Ipv4Addr::LOCALHOST), IpAddr::V6(Ipv6Addr::LOCALHOST)]))
    } else {
        None
    }
}

fn negative_failure(nxdomain: bool) -> Failure {
    Failure {
        kind: FailureKind::NotFound,
        text: if nxdomain {
            "the name does not exist (NXDOMAIN)".to_owned()
        } else {
            "the name has no address of the family asked for (NODATA)".to_owned()
        },
    }
}

/// The Google and the fixed modes: the servers in order, the node's own cache, hosts file and last known answers
/// (R45.6B.3 to R45.6B.5).
struct ServerBackend {
    servers: Vec<SocketAddr>,
    options: Options,
    hosts: Hosts,
    /// By the lowercase name and the record type, positive and negative entries alike, the oldest dropped first.
    cache: Mutex<OldestFirstMap<CacheEntry>>,
    last_known: Mutex<OldestFirstMap<KnownAnswers>>,
    stale_log: Mutex<HourlyLog>,
    /// How many stale-answer WARN lines were logged (read by the unit tests).
    stale_warnings: AtomicU64,
}

impl ServerBackend {
    fn new(servers: &[SocketAddr], options: Options, hosts: Hosts) -> Self {
        ServerBackend {
            servers: servers.to_vec(),
            hosts,
            cache: Mutex::new(OldestFirstMap::new(options.cache_capacity)),
            last_known: Mutex::new(OldestFirstMap::new(options.last_known_capacity)),
            stale_log: Mutex::new(HourlyLog::new(options.log_interval, options.log_capacity)),
            stale_warnings: AtomicU64::new(0),
            options,
        }
    }

    fn last_known_entries(&self) -> u64 { self.last_known.lock().len() as u64 }

    async fn lookup(self: &Arc<Self>, name: &str, family: DnsFamily) -> Result<Found, Failure> {
        let key = normalize(name);
        // `localhost` first, then the hosts file: neither makes a query, and neither is kept as a last known answer.
        if let Some(addresses) = localhost_addresses(&key, family).or_else(|| self.hosts.lookup(&key, family)) {
            return Ok(Found {
                addresses,
                stale: false,
            });
        }
        let question_name = absolute_name(name).map_err(|text| Failure {
            kind: FailureKind::Other,
            text,
        })?;
        let this = self.clone();
        let bound = self.options.lookup_bound;
        let task_key = key.clone();
        let outcome =
            on_core(
                async move { tokio::time::timeout(bound, this.query_family(&task_key, &question_name, family)).await },
            )
            .await;
        let failure = match outcome {
            Ok(Ok(Ok((addresses, valid_until)))) => {
                self.remember(&key, family, &addresses, valid_until);
                return Ok(Found {
                    addresses,
                    stale: false,
                });
            },
            Ok(Ok(Err(failure))) => failure,
            Ok(Err(_elapsed)) => Failure {
                kind: FailureKind::NoServer,
                text: format!("no answer within {} s", bound.as_secs_f64()),
            },
            Err(error) => Failure {
                kind: FailureKind::Other,
                text: error.to_string(),
            },
        };
        if failure.kind == FailureKind::NoServer {
            if let Some(found) = self.serve_last_known(&key, name, family) {
                return Ok(found);
            }
        }
        Err(failure)
    }

    /// The addresses of the family and the instant they stop being current (R45.6B.3 item 1): with no family given the
    /// A records, and the AAAA records only when the name has no A record.
    async fn query_family(
        &self,
        key: &str,
        name: &hickory_proto::rr::Name,
        family: DnsFamily,
    ) -> Result<(Vec<IpAddr>, Instant), Failure> {
        match family {
            DnsFamily::V4 => self.resolve_type(key, name, RecordType::A).await,
            DnsFamily::V6 => self.resolve_type(key, name, RecordType::AAAA).await,
            DnsFamily::Any => match self.resolve_type(key, name, RecordType::A).await {
                Err(failure) if failure.kind == FailureKind::NotFound => {
                    self.resolve_type(key, name, RecordType::AAAA).await
                },
                other => other,
            },
        }
    }

    /// One record type of one name: from the cache while the entry is current, otherwise from the servers, which are
    /// asked in their order. The first answer that is not a server problem decides (R45.6B.3).
    async fn resolve_type(
        &self,
        key: &str,
        name: &hickory_proto::rr::Name,
        record_type: RecordType,
    ) -> Result<(Vec<IpAddr>, Instant), Failure> {
        let cache_key = format!("{}|{}", key, if record_type == RecordType::A { "a" } else { "aaaa" });
        let cached = self.cache.lock().get(&cache_key).cloned();
        if let Some(entry) = cached {
            if entry.valid_until > Instant::now() {
                return match entry.result {
                    Cached::Positive(addresses) => Ok((addresses, entry.valid_until)),
                    Cached::Negative { nxdomain } => Err(negative_failure(nxdomain)),
                };
            }
        }
        let question = Question {
            name: name.clone(),
            record_type,
        };
        let mut last_problem = "no DNS server is configured".to_owned();
        for server in &self.servers {
            match wire::ask(*server, &question, &self.options).await {
                Interpreted::Positive { addresses, ttl } => {
                    let valid_until = Instant::now() + ttl;
                    self.cache.lock().insert(cache_key, CacheEntry {
                        result: Cached::Positive(addresses.clone()),
                        valid_until,
                    });
                    return Ok((addresses, valid_until));
                },
                Interpreted::Negative { nxdomain, ttl } => {
                    self.cache.lock().insert(cache_key, CacheEntry {
                        result: Cached::Negative { nxdomain },
                        valid_until: Instant::now() + ttl,
                    });
                    return Err(negative_failure(nxdomain));
                },
                Interpreted::ServerFailure(problem) => last_problem = problem,
            }
        }
        Err(Failure {
            kind: FailureKind::NoServer,
            text: format!("no server answered: {}", last_problem),
        })
    }

    /// Keeps the answer as the last known one (R45.6B.5).
    fn remember(&self, key: &str, family: DnsFamily, addresses: &[IpAddr], valid_until: Instant) {
        let answer = Answer {
            addresses: addresses.to_vec(),
            valid_until,
            stored_at: Instant::now(),
        };
        self.last_known
            .lock()
            .update(key, |known| known.0[family.index()] = Some(answer));
    }

    /// The last known answer of the name, if it expired at most [`Options::last_known_window`] ago (R45.6B.5 items 1
    /// and 3).
    fn serve_last_known(&self, key: &str, name: &str, family: DnsFamily) -> Option<Found> {
        let now = Instant::now();
        let answer = self.last_known.lock().get(key)?.0[family.index()].clone()?;
        let servable_until = answer.valid_until.checked_add(self.options.last_known_window)?;
        if now > servable_until {
            return None;
        }
        if self.stale_log.lock().permit(key, now) {
            self.stale_warnings.fetch_add(1, Ordering::Relaxed);
            log::warn!(
                "DNS: no server answered for {}, so the last known answer is served (received {} s ago)",
                name,
                now.saturating_duration_since(answer.stored_at).as_secs()
            );
        }
        Some(Found {
            addresses: answer.addresses,
            stale: true,
        })
    }
}

/// The names are compared in lowercase and without a trailing dot.
fn normalize(name: &str) -> String { name.trim_end_matches('.').to_ascii_lowercase() }

/// Aborts the task when the future that awaits it is dropped, so an abandoned lookup does not run on.
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) { self.0.abort(); }
}

/// Runs the future on the shared reactor [`CORE`], whatever runtime the caller runs on: the connections of the DNS
/// library and the blocking pool of the system lookup then live as long as the process, not as long as the first runtime
/// that asked.
async fn on_core<T, F>(future: F) -> io::Result<T>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let mut task = AbortOnDrop(CORE.0.spawn(future));
    (&mut task.0)
        .await
        .map_err(|e| io::Error::other(format!("the lookup task failed: {}", e)))
}

/// The system mode: the operating system's own lookup, as an application asks it (R45.6B.2).
async fn system_lookup(name: &str, port: u16, family: DnsFamily) -> io::Result<Vec<SocketAddr>> {
    let name = name.to_owned();
    let found = on_core(async move {
        tokio::net::lookup_host((name.as_str(), port))
            .await
            .map(|addresses| addresses.collect::<Vec<_>>())
    })
    .await??;
    let found: Vec<SocketAddr> = found
        .into_iter()
        .filter(|address| family.admits(&address.ip()))
        .collect();
    if found.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "the system resolver returned no address of the family asked for",
        ));
    }
    Ok(found)
}

enum Backend {
    System,
    Servers(Arc<ServerBackend>),
    /// A fixed table, for tests ([`install_test_map`]). A name it does not list goes to the system lookup.
    Map(HashMap<String, Vec<IpAddr>>),
}

/// The resolver one node start installed, with the counters of the gauges (R45.6B.10).
struct Installed {
    mode: DnsMode,
    backend: Backend,
    lookups: AtomicU64,
    failures: AtomicU64,
    stale_served: AtomicU64,
    failure_log: Mutex<HourlyLog>,
    /// How many failure WARN lines were logged (read by the unit tests).
    failure_warnings: AtomicU64,
}

impl Installed {
    fn new(mode: DnsMode, backend: Backend, options: &Options) -> Self {
        Installed {
            mode,
            backend,
            lookups: AtomicU64::new(0),
            failures: AtomicU64::new(0),
            stale_served: AtomicU64::new(0),
            failure_log: Mutex::new(HourlyLog::new(options.log_interval, options.log_capacity)),
            failure_warnings: AtomicU64::new(0),
        }
    }

    async fn resolve(&self, name: &str, port: u16, family: DnsFamily) -> io::Result<Vec<SocketAddr>> {
        if let Some(literal) = resolve_literal(name, port, family) {
            return literal;
        }
        self.lookups.fetch_add(1, Ordering::Relaxed);
        let result = match &self.backend {
            Backend::System => system_lookup(name, port, family).await,
            Backend::Map(table) => match table.get(&normalize(name)) {
                Some(addresses) => {
                    let found: Vec<SocketAddr> = addresses
                        .iter()
                        .filter(|address| family.admits(address))
                        .map(|address| SocketAddr::new(*address, port))
                        .collect();
                    if found.is_empty() {
                        Err(io::Error::new(
                            io::ErrorKind::NotFound,
                            "the name has no address of the family asked for",
                        ))
                    } else {
                        Ok(found)
                    }
                },
                None => system_lookup(name, port, family).await,
            },
            Backend::Servers(backend) => match backend.lookup(name, family).await {
                Ok(found) => {
                    if found.stale {
                        self.stale_served.fetch_add(1, Ordering::Relaxed);
                    }
                    Ok(found
                        .addresses
                        .into_iter()
                        .map(|address| SocketAddr::new(address, port))
                        .collect())
                },
                Err(failure) => Err(failure.into_io_error()),
            },
        };
        if let Err(error) = &result {
            self.failures.fetch_add(1, Ordering::Relaxed);
            self.warn_failure(name, error);
        }
        result
    }

    /// At most one WARN line per name per hour (R45.6B.6).
    fn warn_failure(&self, name: &str, error: &io::Error) {
        if self.failure_log.lock().permit(&normalize(name), Instant::now()) {
            self.failure_warnings.fetch_add(1, Ordering::Relaxed);
            log::warn!(
                "DNS: could not resolve {} through {}: {}. The \"{}\" configuration key selects the resolver (\"google\", \
                 \"system\" or a list of DNS servers)",
                name,
                self.mode,
                error,
                DNS_RESOLVER_KEY
            );
        }
    }

    fn gauges(&self) -> DnsGauges {
        DnsGauges {
            lookups: self.lookups.load(Ordering::Relaxed),
            failures: self.failures.load(Ordering::Relaxed),
            stale_served: self.stale_served.load(Ordering::Relaxed),
            last_known_entries: match &self.backend {
                Backend::Servers(backend) => backend.last_known_entries(),
                _ => 0,
            },
        }
    }
}

/// An address literal needs no lookup, in any mode (R45.6B.3 item 3).
fn resolve_literal(name: &str, port: u16, family: DnsFamily) -> Option<io::Result<Vec<SocketAddr>>> {
    let literal = name
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(name);
    let address = literal.parse::<IpAddr>().ok()?;
    Some(if family.admits(&address) {
        Ok(vec![SocketAddr::new(address, port)])
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not an address of the family asked for", address),
        ))
    })
}

/// The resolver the latest node start installed (R45.6B.8 item 1).
static CURRENT: RwLock<Option<Arc<Installed>>> = RwLock::new(None);
/// Set at the beginning of a node's start-up (R45.6B.8 item 3).
static NODE_STARTED: AtomicBool = AtomicBool::new(false);
/// The release build logs the lookup-before-installation error once.
static UNINSTALLED_LOGGED: AtomicBool = AtomicBool::new(false);
/// How many start-up lines were logged (read by the unit tests, R45.6B.7).
static STARTUP_LINES: AtomicU64 = AtomicU64::new(0);

/// Marks that a node is starting: from now on a lookup with no resolver installed is a programming error (R45.6B.8 item
/// 3). The node's start-up calls this first, then [`install`].
pub fn mark_node_started() { NODE_STARTED.store(true, Ordering::SeqCst); }

/// Installs the resolver of `mode`, replacing the one a former node start in this process installed together with its
/// cache and last known answers, and logs the start-up line of R45.6B.7.
pub fn install(mode: &DnsMode) -> Result<(), String> {
    let options = Options::default();
    let backend = match mode {
        DnsMode::System => Backend::System,
        DnsMode::Google => Backend::Servers(Arc::new(ServerBackend::new(
            &google_servers(),
            options.clone(),
            Hosts::load(),
        ))),
        DnsMode::Fixed(servers) => {
            Backend::Servers(Arc::new(ServerBackend::new(servers, options.clone(), Hosts::load())))
        },
    };
    publish(Installed::new(mode.clone(), backend, &options));
    log_startup_line(mode);
    Ok(())
}

fn publish(installed: Installed) -> Arc<Installed> {
    let installed = Arc::new(installed);
    *CURRENT.write() = Some(installed.clone());
    installed
}

/// The one INFO line of R45.6B.7.
fn startup_line(mode: &DnsMode) -> String {
    match mode {
        DnsMode::Google => format!(
            "DNS: the names of every seed, peer, coin server and service this node connects to are sent to Google Public \
             DNS ({}). The \"{}\" key set to \"system\" uses the operating system's resolver instead",
            join_servers(&google_servers()),
            DNS_RESOLVER_KEY
        ),
        DnsMode::System => format!(
            "DNS: the operating system's resolver is used for every name this node looks up (\"{}\": \"system\")",
            DNS_RESOLVER_KEY
        ),
        DnsMode::Fixed(servers) => format!(
            "DNS: every name this node looks up is sent to the servers {}, in this order (\"{}\")",
            join_servers(servers),
            DNS_RESOLVER_KEY
        ),
    }
}

fn log_startup_line(mode: &DnsMode) {
    STARTUP_LINES.fetch_add(1, Ordering::SeqCst);
    log::info!("{}", startup_line(mode));
}

/// The mode of the installed resolver, or `None` when none is installed (for the tests of the node's start-up).
#[doc(hidden)]
pub fn installed_mode() -> Option<DnsMode> { CURRENT.read().as_ref().map(|installed| installed.mode.clone()) }

/// How many start-up lines were logged in this process (for the tests of the node's start-up, R45.6B.7).
#[doc(hidden)]
pub fn startup_lines_logged() -> u64 { STARTUP_LINES.load(Ordering::SeqCst) }

/// The four gauges of R45.6B.10, or `None` when no resolver is installed.
pub fn gauges() -> Option<DnsGauges> { CURRENT.read().as_ref().map(|installed| installed.gauges()) }

/// Looks `name` up through the resolver the node installed (R45.6B.8) and returns its addresses with `port`, in the
/// order of the answer. An address literal is returned as it is.
///
/// A lookup inside the node before [`install`] is a programming error: a debug build fails an assertion, a release build
/// logs one ERROR line and uses the system lookup. A lookup outside the node (the unit tests and helper programs never
/// call [`mark_node_started`]) uses the system lookup.
pub async fn lookup(name: &str, port: u16, family: DnsFamily) -> io::Result<Vec<SocketAddr>> {
    if let Some(literal) = resolve_literal(name, port, family) {
        return literal;
    }
    let installed = CURRENT.read().clone();
    match installed {
        Some(installed) => installed.resolve(name, port, family).await,
        None => {
            let started = NODE_STARTED.load(Ordering::SeqCst);
            debug_assert!(
                !started,
                "a hostname was looked up before the resolver was installed (CRD ch.45 R45.6B.8)"
            );
            if started && !UNINSTALLED_LOGGED.swap(true, Ordering::SeqCst) {
                log::error!(
                    "DNS: a hostname was looked up before the resolver was installed; the system resolver is used. This is \
                     a programming error (CRD ch.45 R45.6B.8)"
                );
            }
            system_lookup(name, port, family).await
        },
    }
}

/// Opens a TCP connection to `host:port`: the name goes through [`lookup`], and every address it resolves to is tried in
/// turn until one connects (R45.6B.9 item 4). An address literal makes no lookup.
pub async fn connect_tcp(host: &str, port: u16) -> io::Result<tokio::net::TcpStream> {
    let addresses = lookup(host, port, DnsFamily::Any).await?;
    let mut last_error = None;
    for address in addresses {
        match tokio::net::TcpStream::connect(address).await {
            Ok(stream) => return Ok(stream),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the name resolved to no address")))
}

/// The resolver service of the HTTP connectors (R45.6B.9 items 3 and 5): every resolution asks [`lookup`] at that
/// moment, so a resolver a later node start installed takes effect for the connections made afterwards.
#[derive(Clone, Copy, Debug, Default)]
pub struct SharedDnsResolve;

impl tower_service::Service<Name> for SharedDnsResolve {
    type Response = std::vec::IntoIter<SocketAddr>;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = io::Result<Self::Response>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> { Poll::Ready(Ok(())) }

    fn call(&mut self, name: Name) -> Self::Future {
        let host = name.as_str().to_owned();
        Box::pin(async move { lookup(&host, 0, DnsFamily::Any).await.map(Vec::into_iter) })
    }
}

/// Test support: the tests that change the process-wide resolver hold this lock, so parallel tests do not interfere.
static TEST_LOCK: Mutex<()> = Mutex::new(());

/// Holds the lock of the tests that change the process-wide resolver. Dropping it uninstalls the resolver and clears the
/// "node started" flag.
#[doc(hidden)]
pub struct TestResolverGuard {
    _lock: parking_lot::MutexGuard<'static, ()>,
}

fn reset_state() {
    *CURRENT.write() = None;
    NODE_STARTED.store(false, Ordering::SeqCst);
    UNINSTALLED_LOGGED.store(false, Ordering::SeqCst);
}

impl Drop for TestResolverGuard {
    fn drop(&mut self) { reset_state(); }
}

/// Takes the test lock with nothing installed and the "node started" flag cleared.
#[doc(hidden)]
pub fn lock_resolver_for_test() -> TestResolverGuard {
    let guard = TestResolverGuard {
        _lock: TEST_LOCK.lock(),
    };
    reset_state();
    guard
}

/// Takes the test lock and installs a test backend that maps the listed names to the listed addresses. A name it does
/// not list goes to the system lookup. The lookups are counted in the gauges like those of a real mode.
#[doc(hidden)]
pub fn install_test_map(entries: &[(&str, &[IpAddr])]) -> TestResolverGuard {
    let guard = lock_resolver_for_test();
    let table = entries
        .iter()
        .map(|(name, addresses)| (normalize(name), addresses.to_vec()))
        .collect();
    publish(Installed::new(
        DnsMode::System,
        Backend::Map(table),
        &Options::default(),
    ));
    guard
}

#[cfg(test)] mod tests;
