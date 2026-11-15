//! Clock check against NTP time servers (CRD ch.45 §45.6C, Reloaded-original).
//!
//! The node measures its own clock against public time servers at start and while it runs, and refuses new orders and
//! swap starts while the clock is off by more than [`CLOCK_LIMIT_SECS`]. A browser build has no UDP: it behaves as if the
//! server list were empty (R45.6C.1, R45.6C.9).
//!
//! The pieces are kept pure and separately testable: the configuration parsing, the request and answer bytes of
//! SNTP (RFC 4330, RFC 5905), the median, the start decision, the state transitions with the log lines they produce and
//! the "check now?" decision of the run-time loop. Only [`UdpTimeSource`] sends packets.

use crate::mm2::lp_native_dex::{MmInitError, MmInitResult};
use async_trait::async_trait;
use common::log::{error, info, warn};
use futures::future::join_all;
use mm2_core::mm_ctx::{from_ctx, MmArc};
use mm2_err_handle::prelude::*;
use parking_lot::Mutex;
use serde_json::Value as Json;
use std::fmt;
use std::net::Ipv6Addr;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(test)] use mocktopus::macros::*;

#[cfg(not(target_arch = "wasm32"))]
use common::custom_futures::FutureTimerExt;
#[cfg(not(target_arch = "wasm32"))] use common::executor::Timer;
#[cfg(not(target_arch = "wasm32"))] use mm2_core::mm_ctx::MmWeak;

/// The configuration key that lists the time servers (R45.6C.2).
pub const NTP_SERVERS_KEY: &str = "ntp_servers";
/// The configuration key that makes "no server answered" a failed check (R45.6C.3).
pub const NTP_REQUIRED_KEY: &str = "ntp_required";

/// The most servers `ntp_servers` accepts (R45.6C.2).
pub const MAX_NTP_SERVERS: usize = 8;
/// The port of a server entry that names none (R45.6C.2).
pub const NTP_PORT: u16 = 123;
/// The default list of R45.6C.2 (decision D2, decided 2026-11-14: the check is on by default), one constant. An empty
/// list here would make the check off by default.
pub const DEFAULT_NTP_HOSTS: [&str; 4] = ["0.pool.ntp.org", "1.pool.ntp.org", "2.pool.ntp.org", "3.pool.ntp.org"];

/// The limit of R45.6C.5: a median offset above it fails the check. A constant, not a setting.
pub const CLOCK_LIMIT_SECS: f64 = 10.0;
/// What one server gets for the lookup and the answer together (R45.6C.4 item 1).
pub const PER_SERVER_TIMEOUT_SECS: f64 = 5.0;
/// The period of the run-time tick (R45.6C.7 item 1).
pub const TICK_SECS: f64 = 60.0;
/// A tick that came more than this much later than its period reports a pause of the process (R45.6C.7 item 1).
pub const LATE_TICK_SECS: f64 = 60.0;
/// The check interval while no refusal is in force.
pub const CHECK_INTERVAL_SECS: f64 = 3600.0;
/// The check interval while new swaps are refused.
pub const REFUSED_CHECK_INTERVAL_SECS: f64 = 300.0;
/// The no-answer WARN line of the run-time loop appears at most this often (R45.6C.7 item 3).
const NO_ANSWER_WARN_INTERVAL: Duration = Duration::from_secs(24 * 3600);

// ---------------------------------------------------------------------------------------------------------------
// Configuration (R45.6C.2, R45.6C.3)
// ---------------------------------------------------------------------------------------------------------------

/// One time server: a host name or an IP literal (an IPv6 literal without brackets) and a port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NtpServer {
    pub host: String,
    pub port: u16,
}

impl fmt::Display for NtpServer {
    /// Host and port only, an IPv6 literal in brackets (R45.6C.10).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.host.contains(':') {
            write!(f, "[{}]:{}", self.host, self.port)
        } else {
            write!(f, "{}:{}", self.host, self.port)
        }
    }
}

/// The two keys, read once at start. An empty `servers` means the check is off (R45.6C.9).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClockConf {
    pub servers: Vec<NtpServer>,
    pub required: bool,
}

impl ClockConf {
    /// The check off: no server, nothing required.
    pub fn off() -> ClockConf {
        ClockConf {
            servers: Vec::new(),
            required: false,
        }
    }

    /// Reads both keys of the node's configuration. A browser build ignores them (R45.6C.1).
    pub fn from_node_conf(conf: &Json) -> Result<ClockConf, ClockConfError> {
        if cfg!(target_arch = "wasm32") {
            return Ok(ClockConf::off());
        }
        Ok(ClockConf {
            servers: parse_ntp_servers(conf.get(NTP_SERVERS_KEY))?,
            required: parse_ntp_required(conf.get(NTP_REQUIRED_KEY))?,
        })
    }

    pub fn is_off(&self) -> bool { self.servers.is_empty() }
}

/// The configuration error of an invalid `ntp_servers` or `ntp_required` value: the launch is refused (R45.6C.2,
/// R45.6C.3, R45.7.1). It names the key and repeats the value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClockConfError {
    pub key: &'static str,
    value: String,
    reason: String,
}

impl fmt::Display for ClockConfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let accepted = if self.key == NTP_SERVERS_KEY {
            format!(
                "Accepted forms: an absent key or null (the default list of {} servers), [] (the clock check is off), or \
                 a non-empty array of at most {} time servers, each \"<host>\", \"<host>:<port>\", \"<IPv4>\", \
                 \"<IPv4>:<port>\", \"<IPv6>\", \"[<IPv6>]\" or \"[<IPv6>]:<port>\" with the port 1 to 65535 (default {})",
                DEFAULT_NTP_HOSTS.len(),
                MAX_NTP_SERVERS,
                NTP_PORT
            )
        } else {
            "Accepted forms: an absent key, null, true or false".to_owned()
        };
        write!(
            f,
            "invalid value {} for the configuration key \"{}\": {}. {}",
            self.value, self.key, self.reason, accepted
        )
    }
}

impl std::error::Error for ClockConfError {}

/// Reads `ntp_servers` (R45.6C.2). `None` and `null` select the default list; anything that is not an accepted form is
/// an error, never replaced by the default: a mistyped list would silently check against servers the operator did not
/// choose.
pub fn parse_ntp_servers(value: Option<&Json>) -> Result<Vec<NtpServer>, ClockConfError> {
    let value = match value {
        None | Some(Json::Null) => {
            return Ok(DEFAULT_NTP_HOSTS
                .iter()
                .map(|host| NtpServer {
                    host: (*host).to_owned(),
                    port: NTP_PORT,
                })
                .collect())
        },
        Some(value) => value,
    };
    let error = |reason: &str| ClockConfError {
        key: NTP_SERVERS_KEY,
        value: value.to_string(),
        reason: reason.to_owned(),
    };
    let entries = match value {
        Json::Array(entries) => entries,
        _ => return Err(error("the value must be an array of strings")),
    };
    if entries.len() > MAX_NTP_SERVERS {
        return Err(error(&format!(
            "the array has {} entries, at most {} are accepted",
            entries.len(),
            MAX_NTP_SERVERS
        )));
    }
    let mut servers = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let entry = entry
            .as_str()
            .ok_or_else(|| error(&format!("entry {} is not a string", index + 1)))?;
        let server = parse_server(entry).map_err(|why| {
            error(&format!(
                "entry {} (\"{}\") is not a time server: {}",
                index + 1,
                entry,
                why
            ))
        })?;
        servers.push(server);
    }
    Ok(servers)
}

/// Reads `ntp_required` (R45.6C.3): a boolean, `false` when absent or `null`.
pub fn parse_ntp_required(value: Option<&Json>) -> Result<bool, ClockConfError> {
    match value {
        None | Some(Json::Null) => Ok(false),
        Some(Json::Bool(required)) => Ok(*required),
        Some(other) => Err(ClockConfError {
            key: NTP_REQUIRED_KEY,
            value: other.to_string(),
            reason: "the value must be a boolean".to_owned(),
        }),
    }
}

/// Both keys, as the start-up reads them: an invalid value refuses the launch with a clean configuration error.
pub(crate) fn read_clock_conf(conf: &Json) -> MmInitResult<ClockConf> {
    ClockConf::from_node_conf(conf).map_to_mm(|e| MmInitError::ErrorDeserializingConfig {
        field: e.key.to_owned(),
        error: e.to_string(),
    })
}

/// One entry: `<host>`, `<host>:<port>`, `<IPv4>`, `<IPv4>:<port>`, `<IPv6>`, `[<IPv6>]` or `[<IPv6>]:<port>`.
fn parse_server(entry: &str) -> Result<NtpServer, &'static str> {
    if entry.is_empty() {
        return Err("the entry is empty");
    }
    if let Some(rest) = entry.strip_prefix('[') {
        let (address, tail) = rest.split_once(']').ok_or("the closing ']' is missing")?;
        let address: Ipv6Addr = address
            .parse()
            .map_err(|_| "the text in brackets is not an IPv6 address")?;
        let port = if tail.is_empty() {
            NTP_PORT
        } else {
            parse_port(tail.strip_prefix(':').ok_or("unexpected text after ']'")?)?
        };
        return Ok(NtpServer {
            host: address.to_string(),
            port,
        });
    }
    if let Ok(address) = entry.parse::<Ipv6Addr>() {
        return Ok(NtpServer {
            host: address.to_string(),
            port: NTP_PORT,
        });
    }
    let (host, port) = match entry.split_once(':') {
        Some((host, port)) => (host, parse_port(port)?),
        None => (entry, NTP_PORT),
    };
    check_host(host)?;
    Ok(NtpServer {
        host: host.to_owned(),
        port,
    })
}

fn check_host(host: &str) -> Result<(), &'static str> {
    if host.is_empty() {
        return Err("the host is empty");
    }
    if host.len() > 253
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.' || b == b'_')
    {
        return Err("it is not a host name or an IP address");
    }
    Ok(())
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

// ---------------------------------------------------------------------------------------------------------------
// SNTP packets (R45.6C.4 items 2 and 3)
// ---------------------------------------------------------------------------------------------------------------

/// The length of an NTP packet without extension fields.
pub const NTP_PACKET_LEN: usize = 48;
/// Seconds from the NTP era start (1900) to the Unix epoch (1970).
const NTP_UNIX_OFFSET_SECS: u64 = 2_208_988_800;

/// An NTP timestamp: seconds since 1900 in the high 32 bits, the fraction of a second in the low 32 bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NtpTimestamp(pub u64);

impl NtpTimestamp {
    /// The timestamp of a time given as the duration since the Unix epoch.
    pub fn from_unix(since_epoch: Duration) -> NtpTimestamp {
        let secs = since_epoch.as_secs().wrapping_add(NTP_UNIX_OFFSET_SECS) & 0xFFFF_FFFF;
        let fraction = (u64::from(since_epoch.subsec_nanos()) << 32) / 1_000_000_000;
        NtpTimestamp((secs << 32) | fraction)
    }

    /// The local clock now, or `None` if it is before the Unix epoch.
    pub fn now() -> Option<NtpTimestamp> {
        let since_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?;
        Some(NtpTimestamp::from_unix(since_epoch))
    }

    fn read(bytes: &[u8]) -> Option<NtpTimestamp> {
        let array: [u8; 8] = bytes.try_into().ok()?;
        Some(NtpTimestamp(u64::from_be_bytes(array)))
    }

    /// `self - earlier` in seconds. The wrapping subtraction keeps the result right across an era change.
    fn seconds_since(self, earlier: NtpTimestamp) -> f64 {
        self.0.wrapping_sub(earlier.0) as i64 as f64 / 4_294_967_296.0
    }
}

/// The 48-byte client request: version 4, client mode, the transmit timestamp `transmit` and nothing else.
pub fn build_request(transmit: NtpTimestamp) -> [u8; NTP_PACKET_LEN] {
    let mut packet = [0u8; NTP_PACKET_LEN];
    packet[0] = (4 << 3) | 3;
    packet[40..48].copy_from_slice(&transmit.0.to_be_bytes());
    packet
}

/// Validates an answer and computes the offset of the local clock in seconds (R45.6C.4 items 2 and 3). `sent` is the
/// transmit time the request carried, `received` the local time the answer arrived. The sender address is checked by the
/// caller. A positive offset means the local clock is behind.
///
/// An answer counts only when it is at least 48 bytes long, is in the server mode, carries `sent` in its originate field,
/// has a non-zero transmit time, a stratum from 1 to 15 and a leap indicator other than "not synchronized".
pub fn parse_answer(answer: &[u8], sent: NtpTimestamp, received: NtpTimestamp) -> Option<f64> {
    if answer.len() < NTP_PACKET_LEN {
        return None;
    }
    let leap_indicator = answer[0] >> 6;
    let mode = answer[0] & 0b111;
    let stratum = answer[1];
    if mode != 4 || leap_indicator == 3 || !(1..=15).contains(&stratum) {
        return None;
    }
    if NtpTimestamp::read(&answer[24..32])? != sent {
        return None;
    }
    let server_received = NtpTimestamp::read(&answer[32..40])?;
    let server_transmitted = NtpTimestamp::read(&answer[40..48])?;
    if server_transmitted.0 == 0 {
        return None;
    }
    Some(offset_seconds(sent, server_received, server_transmitted, received))
}

/// The offset of RFC 5905: `((T2 - T1) + (T3 - T4)) / 2` with T1 the query's send time, T2 the server's receive time,
/// T3 the server's transmit time and T4 the answer's receive time.
pub fn offset_seconds(t1: NtpTimestamp, t2: NtpTimestamp, t3: NtpTimestamp, t4: NtpTimestamp) -> f64 {
    (t2.seconds_since(t1) + t3.seconds_since(t4)) / 2.0
}

/// The median of the offsets, the mean of the two middle ones for an even count (R45.6C.4 item 4).
pub fn median(offsets: &[f64]) -> Option<f64> {
    if offsets.is_empty() {
        return None;
    }
    let mut sorted = offsets.to_vec();
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted.get(middle).copied()
    } else {
        Some((sorted.get(middle - 1).copied()? + sorted.get(middle).copied()?) / 2.0)
    }
}

// ---------------------------------------------------------------------------------------------------------------
// One check (R45.6C.4)
// ---------------------------------------------------------------------------------------------------------------

/// What asks one time server. The real one sends a UDP packet; the tests use a stub.
#[async_trait]
pub trait TimeSource: Send + Sync {
    /// The offset of the local clock in seconds from one valid answer of `server`, or `None` when it gave none within
    /// the time it has (R45.6C.4 item 1).
    async fn query(&self, server: &NtpServer) -> Option<f64>;
}

/// The result of one check (R45.6C.4 item 5).
#[derive(Clone, Debug, PartialEq)]
pub enum CheckResult {
    /// The absolute median offset is at most the limit.
    Passed { offset: f64, answers: usize },
    /// The absolute median offset is above the limit.
    Failed { offset: f64, answers: usize },
    /// No server gave a valid answer.
    NoAnswer,
}

/// The result of the offsets of the valid answers.
pub fn evaluate(offsets: &[f64]) -> CheckResult {
    match median(offsets) {
        None => CheckResult::NoAnswer,
        Some(offset) if offset.abs() <= CLOCK_LIMIT_SECS => CheckResult::Passed {
            offset,
            answers: offsets.len(),
        },
        Some(offset) => CheckResult::Failed {
            offset,
            answers: offsets.len(),
        },
    }
}

/// Asks every server at once and evaluates the answers.
pub async fn run_check(source: &dyn TimeSource, servers: &[NtpServer]) -> CheckResult {
    let offsets: Vec<f64> = join_all(servers.iter().map(|server| source.query(server)))
        .await
        .into_iter()
        .flatten()
        .collect();
    evaluate(&offsets)
}

fn failure_text(offset: f64, answers: usize) -> String {
    let direction = if offset > 0.0 { "behind" } else { "ahead of" };
    format!(
        "The computer's clock is {:.2} s {} the time servers; the limit is {} s (median of {} answer(s)). The \
         computer's clock must be set right",
        offset.abs(),
        direction,
        CLOCK_LIMIT_SECS,
        answers
    )
}

fn no_answer_text() -> String {
    format!(
        "No time server answered, so the computer's clock could not be checked, and \"{}\" is true. The computer's \
         clock must be set right, or the time servers must be reachable (configuration keys \"{}\" and \"{}\")",
        NTP_REQUIRED_KEY, NTP_SERVERS_KEY, NTP_REQUIRED_KEY
    )
}

fn no_answer_warning() -> String {
    format!(
        "No time server answered, so the computer's clock was not checked (configuration keys \"{}\" and \"{}\")",
        NTP_SERVERS_KEY, NTP_REQUIRED_KEY
    )
}

/// The level of a [`LogLine`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineLevel {
    Info,
    Warn,
    Error,
}

/// A log line the pure parts produce, so that the tests see it as data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogLine {
    pub level: LineLevel,
    pub text: String,
}

impl LogLine {
    fn new(level: LineLevel, text: String) -> LogLine { LogLine { level, text } }

    pub fn emit(&self) {
        match self.level {
            LineLevel::Info => info!("{}", self.text),
            LineLevel::Warn => warn!("{}", self.text),
            LineLevel::Error => error!("{}", self.text),
        }
    }
}

fn emit_all(lines: &[LogLine]) { lines.iter().for_each(LogLine::emit); }

// ---------------------------------------------------------------------------------------------------------------
// At start (R45.6C.6)
// ---------------------------------------------------------------------------------------------------------------

/// What the start-up does after its check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartDecision {
    /// The start goes on.
    Go,
    /// The launch is refused with this error (R45.6C.6 items 2 and 3).
    Refuse { error: String },
}

/// The decision and the lines to log about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartReport {
    pub decision: StartDecision,
    pub lines: Vec<LogLine>,
}

/// R45.6C.6 items 2 to 4: a failed check, or no answer with `ntp_required` true, refuses the launch.
pub fn decide_start(result: &CheckResult, required: bool) -> StartReport {
    let refuse = |reason: String| StartReport {
        decision: StartDecision::Refuse {
            error: start_error_text(&reason),
        },
        lines: Vec::new(),
    };
    let go = |line: LogLine| StartReport {
        decision: StartDecision::Go,
        lines: vec![line],
    };
    match result {
        CheckResult::Passed { offset, answers } => go(LogLine::new(
            LineLevel::Info,
            format!(
                "Clock check passed: the computer's clock is {:+.3} s off the time servers (median of {} answer(s), limit {} s)",
                offset, answers, CLOCK_LIMIT_SECS
            ),
        )),
        CheckResult::NoAnswer if !required => go(LogLine::new(LineLevel::Warn, no_answer_warning())),
        CheckResult::Failed { offset, answers } => refuse(failure_text(*offset, *answers)),
        CheckResult::NoAnswer => refuse(no_answer_text()),
    }
}

/// The error that refuses the launch (R45.6C.6 item 2): the reason, and what the refusal means for swaps in progress.
/// Only the start uses it; at run time swaps in progress continue, so [`entering_refusal_text`] says nothing of the kind.
fn start_error_text(reason: &str) -> String {
    format!(
        "{}. The node does not start: swaps in progress are not resumed until it starts with a correct clock, and \
         their deadlines keep running, so setting the computer's clock right is urgent",
        reason
    )
}

/// The ERROR line on entering the refusal at run time (R45.6C.7 item 2).
fn entering_refusal_text(reason: &str) -> String {
    format!(
        "{}. New orders and swap starts are refused until a check passes",
        reason
    )
}

fn off_line() -> LogLine {
    LogLine::new(
        LineLevel::Info,
        format!(
            "The clock check is off: no time server is queried (configuration key \"{}\")",
            NTP_SERVERS_KEY
        ),
    )
}

/// The start check (R45.6C.6): with the check off, returns the INFO line and sends no query; otherwise asks the servers
/// of `conf`, records the outcome in the node's state and returns the lines to log per R45.6C.6. Returns the error that
/// refuses the launch after a failed check, or with no answer and `ntp_required` true.
pub(crate) async fn start_check(ctx: &MmArc, conf: &ClockConf, source: &dyn TimeSource) -> MmInitResult<Vec<LogLine>> {
    let state = ClockCheckCtx::from_ctx(ctx).map_to_mm(MmInitError::Internal)?;
    state.configure(conf);
    if conf.is_off() {
        return Ok(vec![off_line()]);
    }
    let result = run_check(source, &conf.servers).await;
    let report = decide_start(&result, conf.required);
    match report.decision {
        StartDecision::Go => {
            let now = Instant::now();
            state.record_check(now);
            if result == CheckResult::NoAnswer {
                // The start WARN line counts as the one of the 24 hours (R45.6C.7 item 3).
                state.record_no_answer_warning(now);
            }
            Ok(report.lines)
        },
        StartDecision::Refuse { error } => MmError::err(MmInitError::ClockCheckFailed(error)),
    }
}

/// The check `lp_init` runs after the resolver is installed and before the wallet is opened (R45.6C.6 item 1).
pub(crate) async fn startup_clock_check(ctx: &MmArc, conf: &ClockConf) -> MmInitResult<()> {
    let lines = start_check(ctx, conf, &default_time_source()).await?;
    emit_all(&lines);
    Ok(())
}

// ---------------------------------------------------------------------------------------------------------------
// The node's state (R45.6C.11) and the gate (R45.6C.8)
// ---------------------------------------------------------------------------------------------------------------

struct ClockState {
    conf: ClockConf,
    /// The refusal text while new orders and swap starts are refused.
    refusal: Option<String>,
    last_check: Option<Instant>,
    last_no_answer_warning: Option<Instant>,
}

/// The state of the clock check of one node: the keys it started with, the refusal and the last check time. It lives in
/// the node's context, so a later node start in the same process has its own.
pub struct ClockCheckCtx {
    state: Mutex<ClockState>,
}

impl ClockCheckCtx {
    /// The clock-check state of the node, created (check off, no refusal) on first use.
    pub fn from_ctx(ctx: &MmArc) -> Result<Arc<ClockCheckCtx>, String> {
        from_ctx(&ctx.clock_check_ctx, || {
            Ok(ClockCheckCtx {
                state: Mutex::new(ClockState {
                    conf: ClockConf::off(),
                    refusal: None,
                    last_check: None,
                    last_no_answer_warning: None,
                }),
            })
        })
    }

    fn configure(&self, conf: &ClockConf) { self.state.lock().conf = conf.clone(); }

    fn conf(&self) -> ClockConf { self.state.lock().conf.clone() }

    fn record_check(&self, now: Instant) { self.state.lock().last_check = Some(now); }

    fn record_no_answer_warning(&self, now: Instant) { self.state.lock().last_no_answer_warning = Some(now); }

    /// The refusal text while new orders and swap starts are refused.
    pub fn refusal(&self) -> Option<String> { self.state.lock().refusal.clone() }

    fn refused_and_last_check(&self) -> (bool, Option<Instant>) {
        let state = self.state.lock();
        (state.refusal.is_some(), state.last_check)
    }

    /// Applies the result of a run-time check (R45.6C.7 items 2 to 4) and returns the lines to log.
    pub fn apply_runtime_result(&self, result: &CheckResult, required: bool, now: Instant) -> Vec<LogLine> {
        let mut state = self.state.lock();
        state.last_check = Some(now);
        let enter = |state: &mut ClockState, reason: String| {
            let entering = state.refusal.is_none();
            let lines = if entering {
                vec![LogLine::new(LineLevel::Error, entering_refusal_text(&reason))]
            } else {
                // A check during the refusal that fails again logs nothing new.
                Vec::new()
            };
            state.refusal = Some(reason);
            lines
        };
        match result {
            CheckResult::Failed { offset, answers } => enter(&mut state, failure_text(*offset, *answers)),
            CheckResult::NoAnswer if required => enter(&mut state, no_answer_text()),
            CheckResult::Passed { offset, answers } => {
                if state.refusal.take().is_some() {
                    vec![LogLine::new(
                        LineLevel::Info,
                        format!(
                            "The computer's clock is within the limit again ({:+.3} s, median of {} answer(s)); new \
                             orders and swaps are allowed again",
                            offset, answers
                        ),
                    )]
                } else {
                    Vec::new()
                }
            },
            // No answer with `ntp_required` false changes no state; the WARN line appears at most once per 24 hours.
            CheckResult::NoAnswer => {
                let due = state
                    .last_no_answer_warning
                    .is_none_or(|last| now.saturating_duration_since(last) >= NO_ANSWER_WARN_INTERVAL);
                if due {
                    state.last_no_answer_warning = Some(now);
                    vec![LogLine::new(LineLevel::Warn, no_answer_warning())]
                } else {
                    Vec::new()
                }
            },
        }
    }
}

/// The gate: the text of the refusal while the node's clock check refuses new orders and swap starts, else `None`
/// (R45.6C.8). Consulted at the places of the Electrum disagreement gate and nowhere else. If the state cannot be read
/// the gate refuses, so a broken state never lets a new swap through unchecked.
#[cfg_attr(test, mockable)]
pub(crate) fn clock_refusal(ctx: &MmArc) -> Option<String> {
    match ClockCheckCtx::from_ctx(ctx) {
        Ok(state) => state.refusal(),
        Err(e) => Some(format!("The clock check state is unavailable: {}", e)),
    }
}

// ---------------------------------------------------------------------------------------------------------------
// While the node runs (R45.6C.7 item 1)
// ---------------------------------------------------------------------------------------------------------------

/// What one tick of the run-time loop knows, all in seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TickTimes {
    /// How far the wall clock moved since the previous tick.
    pub wall_elapsed: f64,
    /// How far the monotonic clock moved since the previous tick.
    pub mono_elapsed: f64,
    /// The monotonic time since the last check.
    pub since_last_check: f64,
    /// Whether new swaps are refused.
    pub refused: bool,
}

/// Whether a check runs at this tick: the clocks moved apart by more than the limit (the clock was set, or the computer
/// slept), or the tick came more than [`LATE_TICK_SECS`] late (the process was paused), or the interval since the last
/// check is over (one hour, five minutes while refused).
pub fn check_due(tick: &TickTimes) -> bool {
    let clock_jumped = (tick.wall_elapsed - tick.mono_elapsed).abs() > CLOCK_LIMIT_SECS;
    let tick_late = tick.mono_elapsed - TICK_SECS > LATE_TICK_SECS;
    let interval = if tick.refused {
        REFUSED_CHECK_INTERVAL_SECS
    } else {
        CHECK_INTERVAL_SECS
    };
    clock_jumped || tick_late || tick.since_last_check >= interval
}

/// The run-time loop: a tick every 60 s, a check when [`check_due`] says so, one check at a time. It holds a weak pointer
/// to the context and ends with the node (R45.6C.11).
#[cfg(not(target_arch = "wasm32"))]
pub async fn clock_check_loop(ctx_weak: MmWeak, source: Arc<dyn TimeSource>) {
    let conf = match MmArc::from_weak(&ctx_weak).map(|ctx| ClockCheckCtx::from_ctx(&ctx)) {
        Some(Ok(state)) => state.conf(),
        Some(Err(e)) => {
            error!("The clock check loop did not start: {}", e);
            return;
        },
        None => return,
    };
    if conf.is_off() {
        return;
    }
    let mut previous_wall_ms = common::now_ms() as i64;
    let mut previous_mono = Instant::now();
    loop {
        Timer::sleep(TICK_SECS).await;
        let ctx = match MmArc::from_weak(&ctx_weak) {
            Some(ctx) if !ctx.is_stopping() => ctx,
            _ => return,
        };
        let state = match ClockCheckCtx::from_ctx(&ctx) {
            Ok(state) => state,
            Err(e) => {
                error!("The clock check loop stopped: {}", e);
                return;
            },
        };
        let wall_ms = common::now_ms() as i64;
        let mono = Instant::now();
        let (refused, last_check) = state.refused_and_last_check();
        let tick = TickTimes {
            wall_elapsed: (wall_ms - previous_wall_ms) as f64 / 1000.0,
            mono_elapsed: mono.saturating_duration_since(previous_mono).as_secs_f64(),
            since_last_check: last_check
                .map_or(f64::INFINITY, |last| mono.saturating_duration_since(last).as_secs_f64()),
            refused,
        };
        if check_due(&tick) {
            let result = run_check(source.as_ref(), &conf.servers).await;
            emit_all(&state.apply_runtime_result(&result, conf.required, Instant::now()));
        }
        previous_wall_ms = common::now_ms() as i64;
        previous_mono = Instant::now();
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The real time source
// ---------------------------------------------------------------------------------------------------------------

/// Asks a server over UDP, the host name looked up through the node's resolver (R45.6C.4 item 1).
#[cfg(not(target_arch = "wasm32"))]
pub struct UdpTimeSource;

#[cfg(not(target_arch = "wasm32"))]
#[async_trait]
impl TimeSource for UdpTimeSource {
    async fn query(&self, server: &NtpServer) -> Option<f64> {
        match Box::pin(query_udp(server)).timeout_secs(PER_SERVER_TIMEOUT_SECS).await {
            Ok(offset) => offset,
            Err(_) => {
                common::log::debug!("Clock check: {} gave no answer in time", server);
                None
            },
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
async fn query_udp(server: &NtpServer) -> Option<f64> {
    use common::dns_resolver::{self, DnsFamily};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use tokio::net::UdpSocket;

    let target: SocketAddr = match dns_resolver::lookup(&server.host, server.port, DnsFamily::Any).await {
        Ok(addresses) => *addresses.first()?,
        Err(e) => {
            common::log::debug!("Clock check: the lookup of {} failed: {}", server, e);
            return None;
        },
    };
    let any: IpAddr = if target.is_ipv4() {
        Ipv4Addr::UNSPECIFIED.into()
    } else {
        Ipv6Addr::UNSPECIFIED.into()
    };
    let socket = UdpSocket::bind((any, 0)).await.ok()?;
    let sent = NtpTimestamp::now()?;
    socket.send_to(&build_request(sent), target).await.ok()?;
    let mut buffer = [0u8; 512];
    loop {
        // A receive error (for example an ICMP port-unreachable that Windows reports here) ends the query for this server
        // as "no answer".
        let (len, from) = socket.recv_from(&mut buffer).await.ok()?;
        let received = NtpTimestamp::now()?;
        // An answer counts only from the address the query went to; anything else is ignored until the time is up.
        if from.ip() != target.ip() || from.port() != target.port() {
            continue;
        }
        if let Some(offset) = parse_answer(buffer.get(..len)?, sent, received) {
            return Some(offset);
        }
    }
}

/// A browser build sends no time query; its list is always empty, so this is never asked.
#[cfg(target_arch = "wasm32")]
pub struct UdpTimeSource;

#[cfg(target_arch = "wasm32")]
#[async_trait]
impl TimeSource for UdpTimeSource {
    async fn query(&self, _server: &NtpServer) -> Option<f64> { None }
}

fn default_time_source() -> UdpTimeSource { UdpTimeSource }

#[cfg(test)]
#[path = "lp_clock_check_tests.rs"]
mod tests;
