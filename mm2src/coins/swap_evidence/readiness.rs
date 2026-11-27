//! # Purpose
//!
//! The readiness check before a swap starts (maintainer decision of 2026-11-25/26, Reloaded-original, under CRD
//! ch.51 §51.6G R100). It relaxes nothing of R100: a coin whose swap-evidence reads cannot be agreed right now does
//! not start a swap, so a maker declines and a taker stops before any fee is paid, instead of a swap that waits on
//! an undetermined read later.
//!
//! # Public exports
//!
//! - [`evidence_ready`] — the shared decision over a coin's nodes under its effective policy, in one [`ReadyMode`].
//! - [`HostReadiness`], [`NotReadyKind`], [`not_ready_text`] — what a family knows of a node, and the one text of a
//!   coin that is not ready.
//! - [`ReadinessProbes`] — the record of the readiness probes, at most one series per host and [`READY_WINDOW_SECS`].
//! - [`ReadinessSettings`] — the operator's settings of the probes (`swap_evidence_probe_attempts`,
//!   `swap_evidence_probe_timeout_s`, `swap_evidence_host_log_interval_s`).
//! - [`READY_WINDOW_SECS`], [`READY_PROBE_TIMEOUT_SECS`].
//!
//! # Invariants
//!
//! - A host that answered within [`READY_WINDOW_SECS`] counts as answering and is never probed: the healthy case
//!   sends nothing extra.
//! - Probes are sent only when the answering hosts are not enough: one series per unknown host, each attempt bounded
//!   by the probe timeout, and a host is probed at most once per [`READY_WINDOW_SECS`] (one series).
//! - A probe still running is never read as "not answering": a check that depends on it waits for its current
//!   attempt (at most one attempt bound) and uses the outcome. It never starts a second probe of the same key.
//! - A host FAILS only after `attempts` consecutive failed attempts. Until then it does not count for a swap start
//!   ([`ReadyMode::Strict`]) but still counts for the maker's keep-alive pause ([`ReadyMode::Pause`]).
//! - The text of a coin that is not ready names the coin and the kind only, never a host or a URL; a silent host is
//!   named, as `host[:port]`, in its own INFO line.

use super::{node_host, node_name, SwapEvidencePolicy};
use common::custom_futures::FutureTimerExt;
use common::executor::Timer;
use futures::channel::oneshot;
use futures::future::join_all;
use serde_json::Value as Json;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock};

/// How long, in seconds, an answer of a host counts as recent for the readiness check; also the least gap between
/// two readiness probe series of one host.
pub const READY_WINDOW_SECS: f64 = 60.;

/// The default bound, in seconds, of one readiness probe attempt (`swap_evidence_probe_timeout_s`).
pub const READY_PROBE_TIMEOUT_SECS: f64 = 5.;

/// The default number of failed attempts in a row after which a host counts as failed
/// (`swap_evidence_probe_attempts`).
pub const READY_PROBE_ATTEMPTS: u32 = 3;

/// The default least gap, in seconds, between two INFO lines of one silent host (`swap_evidence_host_log_interval_s`).
pub const READY_HOST_LOG_INTERVAL_SECS: u64 = 3600;

/// The gap, in seconds, between two attempts of one series.
const READY_RETRY_GAP_SECS: f64 = 2.;

/// The MM2.json key of the number of attempts.
pub const PROBE_ATTEMPTS_KEY: &str = "swap_evidence_probe_attempts";
/// The MM2.json key of the bound of one attempt.
pub const PROBE_TIMEOUT_KEY: &str = "swap_evidence_probe_timeout_s";
/// The MM2.json key of the least gap between two INFO lines of one silent host.
pub const HOST_LOG_INTERVAL_KEY: &str = "swap_evidence_host_log_interval_s";

/// The marker every text of a coin that is not ready carries.
pub const NOT_READY_MARKER: &str = "swap evidence not available";

/// What a probe closure and its future must be to run as a background series: `Send` and `'static` where tasks are
/// spawned onto threads, `'static` in the browser.
#[cfg(not(target_arch = "wasm32"))]
pub trait ProbeBound: Send + 'static {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Send + 'static> ProbeBound for T {}
#[cfg(target_arch = "wasm32")]
pub trait ProbeBound: 'static {}
#[cfg(target_arch = "wasm32")]
impl<T: 'static> ProbeBound for T {}

/// For whom the readiness is judged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadyMode {
    /// A swap start: a host whose series has failed attempts, with retries pending, does NOT count as answering.
    Strict,
    /// The maker's keep-alive pause: such a host still counts; the pause follows the failure count alone.
    Pause,
}

/// The operator's settings of the readiness probes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReadinessSettings {
    /// Failed attempts in a row after which a host counts as failed.
    pub attempts: u32,
    /// The bound of one attempt, in seconds.
    pub probe_timeout_s: f64,
    /// The least gap, in seconds, between two INFO lines of one silent host.
    pub host_log_interval_s: f64,
    /// The gap between two attempts of one series, in seconds (not an operator setting).
    pub retry_gap_s: f64,
}

impl Default for ReadinessSettings {
    fn default() -> Self {
        ReadinessSettings {
            attempts: READY_PROBE_ATTEMPTS,
            probe_timeout_s: READY_PROBE_TIMEOUT_SECS,
            host_log_interval_s: READY_HOST_LOG_INTERVAL_SECS as f64,
            retry_gap_s: READY_RETRY_GAP_SECS,
        }
    }
}

/// Reads the three MM2.json settings; absent or `null` is the default of each. A value of the wrong kind is an error
/// that names the key (the start-up refuses it like the single-host switch).
///
/// - `swap_evidence_probe_attempts`: a whole number, 1 or more (default 3);
/// - `swap_evidence_probe_timeout_s`: a number above 0 (default 5);
/// - `swap_evidence_host_log_interval_s`: a whole number, 0 or more (default 3600).
pub fn readiness_settings_from_conf(conf: &Json) -> Result<ReadinessSettings, String> {
    let mut settings = ReadinessSettings::default();
    match conf.get(PROBE_ATTEMPTS_KEY) {
        None | Some(Json::Null) => {},
        Some(value) => match value.as_u64().filter(|n| *n >= 1).and_then(|n| u32::try_from(n).ok()) {
            Some(attempts) => settings.attempts = attempts,
            None => {
                return Err(format!(
                    "invalid '{PROBE_ATTEMPTS_KEY}' value {value}: the value must be a whole number of 1 or more"
                ))
            },
        },
    }
    match conf.get(PROBE_TIMEOUT_KEY) {
        None | Some(Json::Null) => {},
        Some(value) => match value.as_f64().filter(|secs| secs.is_finite() && *secs > 0.) {
            Some(secs) => settings.probe_timeout_s = secs,
            None => {
                return Err(format!(
                    "invalid '{PROBE_TIMEOUT_KEY}' value {value}: the value must be a number above 0"
                ))
            },
        },
    }
    match conf.get(HOST_LOG_INTERVAL_KEY) {
        None | Some(Json::Null) => {},
        Some(value) => match value.as_u64() {
            Some(secs) => settings.host_log_interval_s = secs as f64,
            None => {
                return Err(format!(
                    "invalid '{HOST_LOG_INTERVAL_KEY}' value {value}: the value must be a whole number of 0 or more"
                ))
            },
        },
    }
    Ok(settings)
}

fn settings_cell() -> &'static Mutex<ReadinessSettings> {
    static SETTINGS: OnceLock<Mutex<ReadinessSettings>> = OnceLock::new();
    SETTINGS.get_or_init(|| Mutex::new(ReadinessSettings::default()))
}

/// Sets the settings of the node (called once at start-up, after the MM2.json values were validated).
pub fn set_readiness_settings(settings: ReadinessSettings) {
    *settings_cell().lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = settings;
}

/// The settings of the node: the defaults until [`set_readiness_settings`] ran.
pub fn readiness_settings() -> ReadinessSettings {
    *settings_cell().lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What a coin family knows of one of its nodes for the readiness check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostReadiness {
    /// The node answered a request within [`READY_WINDOW_SECS`], or holds a session that keeps answering.
    Answering,
    /// A readiness probe of the node failed within [`READY_WINDOW_SECS`]: it is not probed again before that.
    NotAnswering,
    /// Nothing recent is known of the node: a probe may ask it.
    Unknown,
}

/// Why a coin cannot start a swap right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotReadyKind {
    /// The coin has fewer than two hosts and no trusted node (R100 item 4); admission refuses it as well.
    TooFewNodes,
    /// Fewer than two distinct hosts of the coin answer.
    TooFewHostsAnswering,
    /// The trusted node (`swap_evidence_node`, or the one host of a coin relaxed to it) does not answer.
    TrustedNodeSilent,
    /// The operator's own daemon does not answer.
    OwnDaemonSilent,
    /// A QRC20 token over two or more hosts: fewer than two distinct hosts have served the contract-event history.
    TooFewContractHistoryHosts,
    /// A QRC20 token with a trusted node: that node has not served the contract-event history.
    TrustedNodeNoContractHistory,
}

impl NotReadyKind {
    /// The fixed phrase of the kind, free of any node name.
    pub fn phrase(&self) -> &'static str {
        match self {
            NotReadyKind::TooFewNodes => "fewer than two nodes listed",
            NotReadyKind::TooFewHostsAnswering => "fewer than two hosts answering",
            NotReadyKind::TrustedNodeSilent => "the trusted node is not answering",
            NotReadyKind::OwnDaemonSilent => "the own daemon is not answering",
            NotReadyKind::TooFewContractHistoryHosts => "fewer than two hosts serve the contract-event history",
            NotReadyKind::TrustedNodeNoContractHistory => "the trusted node does not serve the contract-event history",
        }
    }
}

/// The one text of a coin that is not ready: `"{ticker}: swap evidence not available ({kind})"`. It names the coin and
/// the kind only.
pub fn not_ready_text(ticker: &str, kind: NotReadyKind) -> String {
    format!("{ticker}: {NOT_READY_MARKER} ({})", kind.phrase())
}

/// The label of the operator's own daemon, which has no host: its lines name the daemon instead.
pub const OWN_DAEMON_LABEL: &str = "own daemon";

/// The INFO line of a host that does not answer: `"swap evidence: host {host[:port]} of {platform} is not answering
/// ({n} coins affected)"`.
fn silent_host_line(label: &str, platform: &str, coins: usize) -> String {
    if label == OWN_DAEMON_LABEL {
        return format!("swap evidence: the own daemon of {platform} is not answering");
    }
    let noun = if coins == 1 { "coin" } else { "coins" };
    format!("swap evidence: host {label} of {platform} is not answering ({coins} {noun} affected)")
}

/// The INFO line of a host logged as silent that answers again.
fn answering_again_line(label: &str, platform: &str) -> String {
    if label == OWN_DAEMON_LABEL {
        return format!("swap evidence: the own daemon of {platform} answers again");
    }
    format!("swap evidence: host {label} of {platform} answers again")
}

/// Where a check records its probes and its host lines: the `key` groups the hosts that are probed once for all the
/// coins that share them (a platform and its tokens), `platform` names that group in the host lines.
#[derive(Clone, Copy, Debug)]
pub struct ReadyScope<'a> {
    pub key: &'a str,
    pub platform: &'a str,
}

/// How a check is run: for whom, and with which settings.
#[derive(Clone, Copy, Debug)]
pub struct ReadyRun {
    pub mode: ReadyMode,
    pub settings: ReadinessSettings,
}

impl ReadyRun {
    /// The settings of the node, for `mode`.
    pub fn of_node(mode: ReadyMode) -> ReadyRun {
        ReadyRun {
            mode,
            settings: readiness_settings(),
        }
    }
}

/// Where a series of a key stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// An attempt is running.
    Attempting,
    /// Between two attempts, after a failed one.
    Waiting,
    /// An attempt answered.
    Answered,
    /// `attempts` attempts in a row failed.
    Failed,
}

impl Phase {
    fn is_final(self) -> bool { matches!(self, Phase::Answered | Phase::Failed) }
}

/// What a probe series of one key left.
#[derive(Debug)]
struct ProbeRecord {
    /// When the series was claimed: its identity.
    at: f64,
    /// When the series became final (answered or failed): the anchor of the window of a final record.
    ended_at: f64,
    /// How long a series that has not ended may run before it counts as lost.
    max_age: f64,
    phase: Phase,
    /// Failed attempts so far.
    failures: u32,
    /// Attempts that ended so far.
    attempts_done: u32,
    /// The failed attempts after which the host counts as failed.
    limit: u32,
    /// The checks that wait for the end of the current attempt.
    waiters: Vec<oneshot::Sender<()>>,
}

impl ProbeRecord {
    /// Whether the record no longer counts at `now`: a final one [`READY_WINDOW_SECS`] after the series ended, one
    /// that has not ended (a lost task) once it is older than the longest a series can take.
    fn expired(&self, now: f64) -> bool {
        if self.phase.is_final() {
            now - self.ended_at >= READY_WINDOW_SECS
        } else {
            now - self.at >= self.max_age
        }
    }
}

/// How one host stands for one check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HostView {
    Yes,
    No,
    /// An attempt is running and its outcome decides; `seen` is the number of attempts that had ended.
    InFlight {
        seen: u32,
    },
}

/// What is logged of the silent hosts of one scope.
#[derive(Debug, Default)]
struct ScopeLog {
    platform: String,
    /// The coins judged over each host, by host.
    coins: HashMap<String, BTreeSet<String>>,
    /// The hosts logged as silent: label and the time of the last line.
    silent: BTreeMap<String, (String, f64)>,
}

#[derive(Debug, Default)]
struct ProbesInner {
    records: Mutex<HashMap<String, ProbeRecord>>,
    logs: Mutex<HashMap<String, ScopeLog>>,
    /// The keys whose last series failed whole and that have not answered since: such a host does not count as
    /// answering, in any view, while a new series of it is pending. It survives the expiry of the failed record.
    failed_since_answer: Mutex<HashSet<String>>,
    /// The INFO host lines logged, in order (tests).
    #[cfg(test)]
    lines: Mutex<Vec<String>>,
}

/// The readiness probes of a set of nodes: when each key (a host, under a scope) was last probed and how that ended.
/// A key is probed by at most one series per [`READY_WINDOW_SECS`]; a series that answered counts as an answer of the
/// host for that long, one that failed as "not answering" for that long. Cloning shares the record.
#[derive(Clone, Debug, Default)]
pub struct ReadinessProbes {
    inner: Arc<ProbesInner>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl ReadinessProbes {
    /// How a check in `mode` sees `key` at `now`; `None` when nothing recent is known of it.
    fn view(&self, key: &str, now: f64, mode: ReadyMode) -> Option<HostView> {
        let sticky = lock(&self.inner.failed_since_answer).contains(key);
        let records = lock(&self.inner.records);
        let record = records.get(key)?;
        if record.expired(now) {
            return None;
        }
        // What a failed attempt leaves: the host fails after the last one, and until then only a swap start doubts it.
        let after_failure = |failures: u32| {
            if sticky || failures >= record.limit || mode == ReadyMode::Strict {
                HostView::No
            } else {
                HostView::Yes
            }
        };
        Some(match record.phase {
            Phase::Answered => HostView::Yes,
            Phase::Failed => HostView::No,
            Phase::Waiting => after_failure(record.failures),
            Phase::Attempting => match after_failure(record.failures + 1) {
                // The attempt may fail or answer: the host counts either way.
                HostView::Yes => HostView::Yes,
                _ => HostView::InFlight {
                    seen: record.attempts_done,
                },
            },
        })
    }

    /// Claims a series of `key` at `now`: `true` when none was claimed within the window (and none still runs), which
    /// marks the key as being probed. Final records older than the window are dropped here.
    fn claim(&self, key: &str, now: f64, settings: ReadinessSettings) -> bool {
        let mut records = lock(&self.inner.records);
        records.retain(|_, record| !record.expired(now));
        if records.contains_key(key) {
            return false;
        }
        records.insert(key.to_owned(), ProbeRecord {
            at: now,
            ended_at: now,
            // Every attempt and every gap, and one more bound as a margin.
            max_age: f64::from(settings.attempts) * (settings.probe_timeout_s + settings.retry_gap_s)
                + settings.probe_timeout_s,
            phase: Phase::Attempting,
            failures: 0,
            attempts_done: 0,
            limit: settings.attempts,
            waiters: Vec::new(),
        });
        true
    }

    fn begin_attempt(&self, key: &str, claimed_at: f64) {
        if let Some(record) = lock(&self.inner.records).get_mut(key).filter(|r| r.at == claimed_at) {
            record.phase = Phase::Attempting;
        }
    }

    /// Records the end of an attempt of the series of `key` claimed at `claimed_at` and wakes the checks that wait
    /// for it. Returns the phase it left.
    fn end_attempt(&self, key: &str, claimed_at: f64, ended_at: f64, answered: bool) -> Option<Phase> {
        let (phase, waiters) = {
            let mut records = lock(&self.inner.records);
            let record = records.get_mut(key).filter(|r| r.at == claimed_at)?;
            record.attempts_done += 1;
            if answered {
                record.phase = Phase::Answered;
            } else {
                record.failures += 1;
                record.phase = if record.failures >= record.limit {
                    Phase::Failed
                } else {
                    Phase::Waiting
                };
            }
            if record.phase.is_final() {
                record.ended_at = ended_at;
            }
            (record.phase, std::mem::take(&mut record.waiters))
        };
        match phase {
            Phase::Failed => {
                lock(&self.inner.failed_since_answer).insert(key.to_owned());
            },
            Phase::Answered => {
                lock(&self.inner.failed_since_answer).remove(key);
            },
            _ => {},
        }
        for waiter in waiters {
            let _ = waiter.send(());
        }
        Some(phase)
    }

    /// Waits for the end of the attempt that runs on `key` (the one after `seen` ended attempts), at most `bound`
    /// seconds.
    async fn wait_attempt(&self, key: &str, seen: u32, bound: f64) {
        let receiver = {
            let mut records = lock(&self.inner.records);
            match records.get_mut(key) {
                Some(record) if record.attempts_done == seen && !record.phase.is_final() => {
                    let (sender, receiver) = oneshot::channel();
                    record.waiters.push(sender);
                    Some(receiver)
                },
                _ => None,
            }
        };
        if let Some(receiver) = receiver {
            let _ = receiver.timeout_secs(bound).await;
        }
    }

    /// Records that the node `label` (a `host[:port]` form) of `scope` answered a request at `now`: its host counts
    /// as answering for the window, with no probe. For a family that keeps no answer time of its own.
    pub fn note_answer(&self, scope: &str, label: &str, now: f64) {
        let host = host_of(label);
        {
            let mut records = lock(&self.inner.records);
            let key = probe_key(scope, &host);
            if !records.get(&key).is_some_and(|record| !record.phase.is_final()) {
                records.insert(key, ProbeRecord {
                    at: now,
                    ended_at: now,
                    max_age: 0.,
                    phase: Phase::Answered,
                    failures: 0,
                    attempts_done: 1,
                    limit: 1,
                    waiters: Vec::new(),
                });
            }
        }
        self.host_answers(scope, &host, now);
    }

    /// Notes that `ticker` is judged over the hosts of `scope`: the coins a host line counts.
    fn note_coin(&self, scope: ReadyScope<'_>, host: &str, ticker: &str) {
        let mut logs = lock(&self.inner.logs);
        let log = logs.entry(scope.key.to_owned()).or_default();
        if log.platform != scope.platform {
            log.platform = scope.platform.to_owned();
        }
        log.coins.entry(host.to_owned()).or_default().insert(ticker.to_owned());
    }

    /// A series on `host` of the scope ended in failure at `at`: logs its line when the host was not logged as
    /// silent (then the lines of every silent host of the scope, the set having changed), else after
    /// `interval` seconds since its last line.
    fn host_failed(&self, scope_key: &str, host: &str, label: &str, at: f64, interval: f64) {
        let mut logs = lock(&self.inner.logs);
        let Some(log) = logs.get_mut(scope_key) else {
            return;
        };
        match log.silent.get_mut(host) {
            None => {
                log.silent.insert(host.to_owned(), (label.to_owned(), at));
                self.log_silent_hosts(log, at);
            },
            Some((_, last)) if at - *last >= interval => {
                *last = at;
                let line = silent_host_line(label, &log.platform, log.coins.get(host).map_or(0, BTreeSet::len));
                self.log_line(line);
            },
            Some(_) => {},
        }
    }

    /// `host` of the scope answers at `at`: when it was logged as silent, logs that it answers again (then the lines
    /// of the hosts still silent, the set having changed).
    fn host_answers(&self, scope_key: &str, host: &str, at: f64) {
        lock(&self.inner.failed_since_answer).remove(&probe_key(scope_key, host));
        let mut logs = lock(&self.inner.logs);
        let Some(log) = logs.get_mut(scope_key) else {
            return;
        };
        if let Some((label, _)) = log.silent.remove(host) {
            self.log_line(answering_again_line(&label, &log.platform));
            self.log_silent_hosts(log, at);
        }
    }

    fn log_silent_hosts(&self, log: &mut ScopeLog, at: f64) {
        let platform = log.platform.clone();
        for (host, (label, last)) in log.silent.iter_mut() {
            *last = at;
            self.log_line(silent_host_line(
                label,
                &platform,
                log.coins.get(host).map_or(0, BTreeSet::len),
            ));
        }
    }

    fn log_line(&self, line: String) {
        common::log::info!("{line}");
        #[cfg(test)]
        lock(&self.inner.lines).push(line);
    }

    /// The host INFO lines logged so far (tests).
    #[cfg(test)]
    pub(crate) fn lines(&self) -> Vec<String> { lock(&self.inner.lines).clone() }

    /// When the series of `key` ended, if it has (tests).
    #[cfg(test)]
    fn records_ended_at(&self, key: &str) -> Option<f64> {
        lock(&self.inner.records)
            .get(key)
            .filter(|r| r.phase.is_final())
            .map(|r| r.ended_at)
    }

    /// The failed attempts and the phase of the series of `key` (tests).
    #[cfg(test)]
    fn series_state(&self, key: &str) -> Option<(u32, Phase)> {
        lock(&self.inner.records).get(key).map(|r| (r.failures, r.phase))
    }
}

/// The host a node label is grouped by: its host, or the whole label, lowercased, when it names none.
fn host_of(label: &str) -> String { node_host(label).unwrap_or_else(|| label.to_ascii_lowercase()) }

/// The key of a host of `scope` in a [`ReadinessProbes`].
fn probe_key(scope: &str, host: &str) -> String { format!("{scope}|{host}") }

/// The record of the readiness probes of the families that keep no client of their own to hold one (an own daemon,
/// the EVM, lightwalletd and walletd nodes). Callers scope the keys by the PLATFORM's ticker, so a platform and its
/// tokens share one probe and one host line per host.
pub fn shared_readiness_probes() -> &'static ReadinessProbes {
    static SHARED: OnceLock<ReadinessProbes> = OnceLock::new();
    SHARED.get_or_init(ReadinessProbes::default)
}

/// The series of attempts on one host: runs as a task of its own, so a check never waits for more than one attempt.
/// The first attempt is built by the check that claimed the series (in its own context, before the task starts); the
/// retries are built by `probe`.
struct Series<G, Fut> {
    probes: ReadinessProbes,
    key: String,
    scope_key: String,
    host: String,
    label: String,
    claimed_at: f64,
    settings: ReadinessSettings,
    probe: G,
    first: Option<Fut>,
}

impl<G, Fut> Series<G, Fut>
where
    G: Fn(f64) -> Fut,
    Fut: Future<Output = bool>,
{
    async fn run(mut self) {
        let started = common::now_float();
        let bound = self.settings.probe_timeout_s;
        for attempt in 1..=self.settings.attempts {
            if attempt > 1 {
                self.probes.begin_attempt(&self.key, self.claimed_at);
            }
            let attempt_future = Box::pin(match self.first.take() {
                Some(first) => first,
                None => (self.probe)(bound),
            });
            let answered = matches!(attempt_future.timeout_secs(bound).await, Ok(true));
            let last = answered || attempt >= self.settings.attempts;
            // The end of the attempt on the clock of the claim.
            let at = self.claimed_at + (common::now_float() - started);
            if last {
                // The line is written before the checks that wait for this attempt are woken.
                if answered {
                    self.probes.host_answers(&self.scope_key, &self.host, at);
                } else {
                    self.probes.host_failed(
                        &self.scope_key,
                        &self.host,
                        &self.label,
                        at,
                        self.settings.host_log_interval_s,
                    );
                }
            }
            self.probes.end_attempt(&self.key, self.claimed_at, at, answered);
            if last {
                return;
            }
            Timer::sleep(self.settings.retry_gap_s).await;
        }
    }
}

/// Whether the coin `ticker` can start a swap now (`run.mode`) under its effective `policy`, over its `nodes`.
///
/// - `NotAdopted` (and a family whose swaps are off) is ready; `Insufficient` is not ([`NotReadyKind::TooFewNodes`]);
/// - `TwoNodes` is ready while at least two distinct hosts of `nodes` answer;
/// - `Trusted` while the node whose `label` names the trusted node answers;
/// - `OwnDaemon` while one of `nodes` (the daemon) answers.
///
/// `label` names a node in the `host[:port]` form ([`node_name`]); `known` is what the family knows of the node
/// (`Answering` for an answer within [`READY_WINDOW_SECS`], else `Unknown`). When the answering hosts are not enough,
/// one series of `probe` attempts per host whose nodes are all unknown starts in the background; `probe(node)` gives
/// the closure that runs one attempt, with the attempt's bound in seconds. `probes` keeps a host from being probed
/// twice within [`READY_WINDOW_SECS`], under keys `"{scope.key}|{host}"`. The check waits for the current attempt of a
/// running series only, and uses its outcome.
///
/// # Errors
///
/// The [`not_ready_text`] of the coin.
#[allow(clippy::too_many_arguments)]
pub async fn evidence_ready<N, F, G, Fut>(
    ticker: &str,
    policy: &SwapEvidencePolicy,
    nodes: &[N],
    label: impl Fn(&N) -> String,
    known: impl Fn(&N) -> HostReadiness,
    probes: &ReadinessProbes,
    scope: ReadyScope<'_>,
    mode: ReadyMode,
    probe: F,
) -> Result<(), String>
where
    F: Fn(&N) -> G,
    G: Fn(f64) -> Fut + ProbeBound,
    Fut: Future<Output = bool> + ProbeBound,
{
    evidence_ready_at(
        ticker,
        policy,
        nodes,
        label,
        known,
        probes,
        scope,
        ReadyRun::of_node(mode),
        probe,
        common::now_float(),
    )
    .await
}

/// [`evidence_ready`] at the time `now`, with the settings of `run`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn evidence_ready_at<N, F, G, Fut>(
    ticker: &str,
    policy: &SwapEvidencePolicy,
    nodes: &[N],
    label: impl Fn(&N) -> String,
    known: impl Fn(&N) -> HostReadiness,
    probes: &ReadinessProbes,
    scope: ReadyScope<'_>,
    run: ReadyRun,
    probe: F,
    now: f64,
) -> Result<(), String>
where
    F: Fn(&N) -> G,
    G: Fn(f64) -> Fut + ProbeBound,
    Fut: Future<Output = bool> + ProbeBound,
{
    let labels: Vec<String> = nodes.iter().map(&label).collect();
    let (needed, kind, candidates): (usize, NotReadyKind, Vec<usize>) = match policy {
        SwapEvidencePolicy::NotAdopted => return Ok(()),
        SwapEvidencePolicy::Insufficient { .. } => return Err(not_ready_text(ticker, NotReadyKind::TooFewNodes)),
        SwapEvidencePolicy::TwoNodes { .. } => (2, NotReadyKind::TooFewHostsAnswering, (0..nodes.len()).collect()),
        SwapEvidencePolicy::Trusted { node } => {
            let trusted = node_name(node);
            (
                1,
                NotReadyKind::TrustedNodeSilent,
                (0..nodes.len())
                    .filter(|index| node_name(&labels[*index]) == trusted)
                    .collect(),
            )
        },
        SwapEvidencePolicy::OwnDaemon => (1, NotReadyKind::OwnDaemonSilent, (0..nodes.len()).collect()),
    };

    // The candidates by host, in list order.
    let mut hosts: Vec<(String, Vec<usize>)> = Vec::new();
    for index in candidates {
        let host = host_of(&labels[index]);
        match hosts.iter_mut().find(|(known_host, _)| *known_host == host) {
            Some((_, members)) => members.push(index),
            None => hosts.push((host, vec![index])),
        }
    }
    let key_of = |host: &str| probe_key(scope.key, host);
    let ReadyRun { mode, settings } = run;

    // What the family knows: a host with a node that answered is answering, whatever a probe left.
    let mut known_answering = Vec::with_capacity(hosts.len());
    for (host, members) in hosts.iter() {
        probes.note_coin(scope, host, ticker);
        let answering = members
            .iter()
            .any(|index| known(&nodes[*index]) == HostReadiness::Answering);
        if answering {
            probes.host_answers(scope.key, host, now);
        }
        known_answering.push(answering);
    }

    // How the hosts stand now: the answering ones, the ones whose attempt runs, the ones nothing is known of.
    let evaluate = || {
        let mut answering = 0;
        let mut in_flight: Vec<(usize, u32)> = Vec::new();
        let mut unknown: Vec<usize> = Vec::new();
        for (position, (host, members)) in hosts.iter().enumerate() {
            if known_answering[position] {
                answering += 1;
                continue;
            }
            match probes.view(&key_of(host), now, mode) {
                Some(HostView::Yes) => answering += 1,
                Some(HostView::No) => {},
                Some(HostView::InFlight { seen }) => in_flight.push((position, seen)),
                None => {
                    if members
                        .iter()
                        .any(|index| known(&nodes[*index]) == HostReadiness::Unknown)
                    {
                        unknown.push(position);
                    }
                },
            }
        }
        (answering, in_flight, unknown)
    };

    let (answering, _, unknown) = evaluate();
    if answering >= needed {
        return Ok(());
    }

    // Not enough: one series per unknown host, each host at most once per window.
    for position in unknown {
        let (host, members) = &hosts[position];
        let key = key_of(host);
        if !probes.claim(&key, now, settings) {
            continue;
        }
        let member = members
            .iter()
            .find(|index| known(&nodes[**index]) == HostReadiness::Unknown)
            .copied()
            .unwrap_or(members[0]);
        let attempt = probe(&nodes[member]);
        let series = Series {
            probes: probes.clone(),
            key,
            scope_key: scope.key.to_owned(),
            host: host.clone(),
            label: if labels[member] == OWN_DAEMON_LABEL {
                OWN_DAEMON_LABEL.to_owned()
            } else {
                node_name(&labels[member])
            },
            claimed_at: now,
            settings,
            first: Some(attempt(settings.probe_timeout_s)),
            probe: attempt,
        };
        common::executor::spawn(series.run());
    }

    // The hosts whose attempt decides: the check waits for that attempt only, then judges again.
    let (answering, in_flight, _) = evaluate();
    if answering >= needed {
        return Ok(());
    }
    // The attempt is bounded by its own timer; the margin only keeps a lost task from holding the check.
    join_all(in_flight.iter().map(|(position, seen)| {
        let key = key_of(&hosts[*position].0);
        async move { probes.wait_attempt(&key, *seen, settings.probe_timeout_s + 1.).await }
    }))
    .await;
    let (answering, _, _) = evaluate();
    if answering >= needed {
        Ok(())
    } else {
        Err(not_ready_text(ticker, kind))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::block_on;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    /// How one probe attempt of a scripted node ends.
    #[derive(Clone, Copy)]
    enum Step {
        Answer(bool),
        /// Answers `bool` after the given milliseconds.
        After(u64, bool),
        Never,
    }
    use Step::{After, Answer, Never};

    /// A scripted node: its label, what the family knows of it, and how its attempts end (the last step repeats).
    #[derive(Clone)]
    struct Node {
        label: &'static str,
        known: HostReadiness,
        script: Vec<Step>,
    }

    fn node(label: &'static str, known: HostReadiness, probe: Option<bool>) -> Node {
        Node {
            label,
            known,
            script: vec![probe.map_or(Never, Answer)],
        }
    }

    fn scripted(label: &'static str, script: &[Step]) -> Node {
        Node {
            label,
            known: Unknown,
            script: script.to_vec(),
        }
    }

    /// Small gaps, `attempts` attempts per series.
    fn fast(attempts: u32) -> ReadinessSettings {
        ReadinessSettings {
            attempts,
            probe_timeout_s: 1.,
            host_log_interval_s: 3600.,
            retry_gap_s: 0.01,
        }
    }

    fn two_nodes() -> SwapEvidencePolicy {
        SwapEvidencePolicy::TwoNodes {
            hosts: vec!["a".to_owned(), "b".to_owned()],
        }
    }

    /// The attempts made on each label.
    type Calls = Arc<Mutex<Vec<&'static str>>>;

    /// Runs the check of coin `ticker` over `nodes` at `now`.
    async fn check_async(
        ticker: &str,
        policy: &SwapEvidencePolicy,
        nodes: &[Node],
        probes: &ReadinessProbes,
        now: f64,
        run: ReadyRun,
        calls: &Calls,
    ) -> Result<(), String> {
        let counters: Arc<Mutex<HashMap<&'static str, usize>>> = Arc::default();
        evidence_ready_at(
            ticker,
            policy,
            nodes,
            |node| node.label.to_owned(),
            |node| node.known,
            probes,
            ReadyScope {
                key: "T",
                platform: "T",
            },
            run,
            |node: &Node| {
                let node = node.clone();
                let calls = calls.clone();
                let counters = counters.clone();
                move |_bound| {
                    calls.lock().unwrap().push(node.label);
                    let attempt = {
                        let mut counters = counters.lock().unwrap();
                        let entry = counters.entry(node.label).or_default();
                        *entry += 1;
                        *entry - 1
                    };
                    let step = *node.script.get(attempt).or(node.script.last()).unwrap();
                    async move {
                        match step {
                            Answer(answer) => answer,
                            After(ms, answer) => {
                                Timer::sleep_ms(ms as u32).await;
                                answer
                            },
                            Never => futures::future::pending().await,
                        }
                    }
                }
            },
            now,
        )
        .await
    }

    /// Runs the check over `nodes` at `now` (strict, `attempts` attempt) and returns the outcome and the labels
    /// probed, once the series has ended.
    fn check(
        policy: &SwapEvidencePolicy,
        nodes: &[Node],
        probes: &ReadinessProbes,
        now: f64,
        timeout: f64,
    ) -> (Result<(), String>, Vec<&'static str>) {
        let mut settings = fast(1);
        settings.probe_timeout_s = timeout;
        let calls: Calls = Arc::default();
        let outcome = block_on(check_async(
            "T",
            policy,
            nodes,
            probes,
            now,
            ReadyRun {
                mode: ReadyMode::Strict,
                settings,
            },
            &calls,
        ));
        block_on(settle(probes, nodes, now));
        let probed = calls.lock().unwrap().clone();
        (outcome, probed)
    }

    /// Waits until every series of `nodes` that runs has ended.
    async fn settle(probes: &ReadinessProbes, nodes: &[Node], _now: f64) {
        for node in nodes {
            let key = probe_key("T", &host_of(node.label));
            for _ in 0..1000 {
                match probes.series_state(&key) {
                    Some((_, phase)) if !phase.is_final() => Timer::sleep_ms(5).await,
                    _ => break,
                }
            }
        }
    }

    fn run_of(mode: ReadyMode, attempts: u32) -> ReadyRun {
        ReadyRun {
            mode,
            settings: fast(attempts),
        }
    }

    use HostReadiness::{Answering, NotAnswering, Unknown};

    /// TwoNodes: one of three hosts answering is not ready; two of three are.
    #[test]
    fn two_nodes_needs_two_answering_hosts() {
        let probes = ReadinessProbes::default();
        let one_of_three = [
            node("a.example:50001", Answering, None),
            node("b.example:50001", NotAnswering, None),
            node("c.example:50001", NotAnswering, None),
        ];
        let (outcome, probed) = check(&two_nodes(), &one_of_three, &probes, 1000., 0.05);
        assert_eq!(
            outcome,
            Err("T: swap evidence not available (fewer than two hosts answering)".to_owned())
        );
        assert!(probed.is_empty(), "a host known as not answering is not probed");

        let two_of_three = [
            node("a.example:50001", Answering, None),
            node("b.example:50001", NotAnswering, None),
            node("c.example:50001", Answering, None),
        ];
        assert_eq!(check(&two_nodes(), &two_of_three, &probes, 1000., 0.05).0, Ok(()));
    }

    /// Two ports of one host are one host: they never make two answering hosts.
    #[test]
    fn two_ports_of_one_host_answer_as_one() {
        let probes = ReadinessProbes::default();
        let nodes = [
            node("a.example:50001", Answering, None),
            node("a.example:50002", Answering, None),
            node("b.example:50001", Unknown, Some(false)),
        ];
        let (outcome, probed) = check(&two_nodes(), &nodes, &probes, 1000., 0.05);
        assert!(outcome.is_err());
        assert_eq!(probed, vec!["b.example:50001"]);
    }

    /// The healthy case sends no probe; the unknown host is probed only when the answering ones are not enough.
    #[test]
    fn probes_go_only_where_needed() {
        let probes = ReadinessProbes::default();
        let healthy = [
            node("a.example:1", Answering, None),
            node("b.example:1", Answering, None),
            node("c.example:1", Unknown, Some(true)),
        ];
        let (outcome, probed) = check(&two_nodes(), &healthy, &probes, 1000., 0.05);
        assert_eq!(outcome, Ok(()));
        assert!(probed.is_empty(), "nothing extra is sent in the healthy case");

        let short = [
            node("a.example:1", Answering, None),
            node("b.example:1", Unknown, Some(true)),
            node("c.example:1", Unknown, Some(true)),
        ];
        let (outcome, mut probed) = check(&two_nodes(), &short, &probes, 1000., 0.05);
        assert_eq!(outcome, Ok(()));
        probed.sort_unstable();
        assert_eq!(
            probed,
            vec!["b.example:1", "c.example:1"],
            "one probe per unknown host, at once"
        );
    }

    /// A host is probed at most once per window; a probe that answered counts as an answer for the window.
    #[test]
    fn a_host_is_probed_at_most_once_per_window() {
        let probes = ReadinessProbes::default();
        let failing = [
            node("a.example:1", Answering, None),
            node("b.example:1", Unknown, Some(false)),
        ];
        let (outcome, probed) = check(&two_nodes(), &failing, &probes, 1000., 0.05);
        assert!(outcome.is_err());
        assert_eq!(probed.len(), 1);
        let (outcome, probed) = check(&two_nodes(), &failing, &probes, 1059., 0.05);
        assert!(outcome.is_err());
        assert!(probed.is_empty(), "no second probe within 60 s");
        let (_, probed) = check(&two_nodes(), &failing, &probes, 1062., 0.05);
        assert_eq!(probed.len(), 1, "probed again once the window has passed");

        let probes = ReadinessProbes::default();
        let answering = [
            node("a.example:1", Answering, None),
            node("b.example:1", Unknown, Some(true)),
        ];
        assert_eq!(
            check(&two_nodes(), &answering, &probes, 1000., 0.05),
            (Ok(()), vec!["b.example:1"])
        );
        assert_eq!(
            check(&two_nodes(), &answering, &probes, 1030., 0.05),
            (Ok(()), vec![]),
            "the probe's answer counts for the window"
        );
    }

    /// A probe that does not answer within its bound is a failed attempt, and the check ends at the bound.
    #[test]
    fn the_probe_bound_is_honored() {
        let probes = ReadinessProbes::default();
        let silent = [node("a.example:1", Answering, None), node("b.example:1", Unknown, None)];
        let calls: Calls = Arc::default();
        let mut settings = fast(1);
        settings.probe_timeout_s = 0.2;
        let started = Instant::now();
        let outcome = block_on(check_async(
            "T",
            &two_nodes(),
            &silent,
            &probes,
            1000.,
            ReadyRun {
                mode: ReadyMode::Strict,
                settings,
            },
            &calls,
        ));
        let elapsed = started.elapsed();
        assert!(outcome.is_err());
        assert_eq!(calls.lock().unwrap().len(), 1);
        assert!(
            elapsed >= Duration::from_millis(200),
            "ended before the bound: {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "the bound was not honored: {elapsed:?}"
        );
        assert_eq!(probes.series_state("T|b.example"), Some((1, Phase::Failed)));
    }

    /// The production defaults: bound 5 s, 3 attempts, window 60 s, interval 3600 s; the public check ends at the
    /// bound of its first attempt.
    #[test]
    fn the_production_defaults() {
        assert_eq!(READY_PROBE_TIMEOUT_SECS, 5.);
        assert_eq!(READY_WINDOW_SECS, 60.);
        let defaults = ReadinessSettings::default();
        assert_eq!(defaults.attempts, 3);
        assert_eq!(defaults.probe_timeout_s, 5.);
        assert_eq!(defaults.host_log_interval_s, 3600.);
        assert_eq!(defaults.retry_gap_s, 2.);
        assert_eq!(
            readiness_settings(),
            defaults,
            "the node's settings start as the defaults"
        );
        let probes = ReadinessProbes::default();
        let silent = [node("a.example:1", Unknown, None)];
        let trusted = SwapEvidencePolicy::Trusted {
            node: "a.example:1".to_owned(),
        };
        let started = Instant::now();
        let outcome = block_on(evidence_ready(
            "T",
            &trusted,
            &silent,
            |node| node.label.to_owned(),
            |node| node.known,
            &probes,
            ReadyScope {
                key: "T",
                platform: "T",
            },
            ReadyMode::Strict,
            |_: &Node| |_bound: f64| futures::future::pending::<bool>(),
        ));
        let elapsed = started.elapsed();
        assert!(outcome.is_err());
        assert!(
            elapsed >= Duration::from_millis(4900) && elapsed < Duration::from_secs(8),
            "{elapsed:?}"
        );
    }

    /// Trusted: ready only while the trusted node answers; another node never stands in for it.
    #[test]
    fn trusted_up_and_down() {
        let trusted = SwapEvidencePolicy::Trusted {
            node: "https://user:pw@b.example:443/path".to_owned(),
        };
        let probes = ReadinessProbes::default();
        let up = [
            node("a.example:443", Answering, None),
            node("b.example:443", Answering, None),
        ];
        assert_eq!(check(&trusted, &up, &probes, 1000., 0.05).0, Ok(()));
        let down = [
            node("a.example:443", Answering, None),
            node("b.example:443", Unknown, Some(false)),
        ];
        let (outcome, probed) = check(&trusted, &down, &probes, 1000., 0.05);
        assert_eq!(
            outcome,
            Err("T: swap evidence not available (the trusted node is not answering)".to_owned())
        );
        assert_eq!(probed, vec!["b.example:443"]);
        let missing = [node("a.example:443", Answering, None)];
        assert!(check(&trusted, &missing, &probes, 1000., 0.05).0.is_err());
    }

    /// OwnDaemon: ready while the daemon answers its probe; Insufficient is never ready; NotAdopted always is.
    #[test]
    fn own_daemon_insufficient_and_not_adopted() {
        let probes = ReadinessProbes::default();
        let daemon_up = [node("own daemon", Unknown, Some(true))];
        assert_eq!(
            check(&SwapEvidencePolicy::OwnDaemon, &daemon_up, &probes, 1000., 0.05),
            (Ok(()), vec!["own daemon"])
        );
        let probes = ReadinessProbes::default();
        let daemon_down = [node("own daemon", Unknown, Some(false))];
        assert_eq!(
            check(&SwapEvidencePolicy::OwnDaemon, &daemon_down, &probes, 1000., 0.05).0,
            Err("T: swap evidence not available (the own daemon is not answering)".to_owned())
        );
        let any = [node("a.example:1", Answering, None)];
        assert_eq!(
            check(
                &SwapEvidencePolicy::Insufficient { hosts: 1 },
                &any,
                &probes,
                1000.,
                0.05
            )
            .0,
            Err("T: swap evidence not available (fewer than two nodes listed)".to_owned())
        );
        let none: [Node; 0] = [];
        assert_eq!(
            check(&SwapEvidencePolicy::NotAdopted, &none, &probes, 1000., 0.05),
            (Ok(()), vec![])
        );
    }

    /// C1: a probe in flight is waited for, not read as "not answering": the second check uses the answer of the
    /// probe of the first one, and no second probe is started.
    #[test]
    fn a_probe_in_flight_is_waited_for() {
        let probes = ReadinessProbes::default();
        let nodes = [
            node("a.example:1", Answering, None),
            scripted("b.example:1", &[After(100, true)]),
        ];
        let calls: Calls = Arc::default();
        let run = run_of(ReadyMode::Strict, 3);
        let policy = two_nodes();
        let (first, second) = block_on(async {
            futures::join!(check_async("T", &policy, &nodes, &probes, 1000., run, &calls), async {
                // Starts while the first check's probe runs.
                Timer::sleep_ms(30).await;
                check_async("T", &policy, &nodes, &probes, 1000.03, run, &calls).await
            })
        });
        assert_eq!(first, Ok(()));
        assert_eq!(second, Ok(()), "the running probe's answer is used");
        assert_eq!(calls.lock().unwrap().len(), 1, "one probe of the key, not two");
        // Pause judges see the same.
        let (pause, strict) = block_on(async {
            (
                check_async(
                    "T",
                    &two_nodes(),
                    &nodes,
                    &probes,
                    1001.,
                    run_of(ReadyMode::Pause, 3),
                    &calls,
                )
                .await,
                check_async("T", &two_nodes(), &nodes, &probes, 1001., run, &calls).await,
            )
        });
        assert_eq!((pause, strict), (Ok(()), Ok(())));
    }

    /// C2/C4: a host fails after 3 failed attempts. Before that, a swap start refuses it at once while the pause
    /// still counts it; the pause follows the 3rd failure.
    #[test]
    fn a_host_fails_after_three_attempts() {
        let probes = ReadinessProbes::default();
        let nodes = [
            node("a.example:1", Answering, None),
            scripted("b.example:1", &[Answer(false), After(150, false), After(150, false)]),
        ];
        let calls: Calls = Arc::default();
        let strict = run_of(ReadyMode::Strict, 3);
        let pause = run_of(ReadyMode::Pause, 3);
        // The first attempt failed, retries pending.
        let started = Instant::now();
        assert_eq!(
            block_on(check_async("T", &two_nodes(), &nodes, &probes, 1000., pause, &calls)),
            Ok(()),
            "the pause counts the host: no pause after 0 failures"
        );
        assert!(
            block_on(check_async("T", &two_nodes(), &nodes, &probes, 1000.1, strict, &calls)).is_err(),
            "a swap start is refused while the series has failed attempts or runs"
        );
        // After one failure and after two: still counted by the pause; the swap start stays refused.
        for failed in [1, 2] {
            block_on(async {
                for _ in 0..1000 {
                    if probes.series_state("T|b.example").is_some_and(|s| s.0 >= failed) {
                        break;
                    }
                    Timer::sleep_ms(5).await;
                }
            });
            assert_eq!(
                block_on(check_async("T", &two_nodes(), &nodes, &probes, 1000.2, pause, &calls)),
                Ok(()),
                "pause view after {failed} failed attempts"
            );
            assert!(block_on(check_async("T", &two_nodes(), &nodes, &probes, 1000.2, strict, &calls)).is_err());
        }
        // The 3rd failure: the pause follows.
        block_on(settle(&probes, &nodes, 1000.));
        assert_eq!(probes.series_state("T|b.example"), Some((3, Phase::Failed)));
        assert!(block_on(check_async("T", &two_nodes(), &nodes, &probes, 1000.5, pause, &calls)).is_err());
        assert_eq!(calls.lock().unwrap().len(), 3, "three attempts, no more");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    /// A pause judge that finds the last attempt in flight waits for it and uses its outcome.
    #[test]
    fn the_pause_waits_for_the_last_attempt() {
        let probes = ReadinessProbes::default();
        let nodes = [
            node("a.example:1", Answering, None),
            scripted("b.example:1", &[Answer(false), Answer(false), After(100, false)]),
        ];
        let calls: Calls = Arc::default();
        let pause = run_of(ReadyMode::Pause, 3);
        assert_eq!(
            block_on(check_async("T", &two_nodes(), &nodes, &probes, 1000., pause, &calls)),
            Ok(())
        );
        block_on(async {
            for _ in 0..1000 {
                if probes.series_state("T|b.example") == Some((2, Phase::Attempting)) {
                    break;
                }
                Timer::sleep_ms(2).await;
            }
        });
        assert_eq!(
            block_on(check_async("T", &two_nodes(), &nodes, &probes, 1000.1, pause, &calls)),
            Err("T: swap evidence not available (fewer than two hosts answering)".to_owned())
        );
    }

    /// A success after one failure: the host answers, nothing is paused, and no host line is logged.
    #[test]
    fn a_success_after_one_failure_ends_the_series() {
        let probes = ReadinessProbes::default();
        let nodes = [
            node("a.example:1", Answering, None),
            scripted("b.example:1", &[Answer(false), Answer(true)]),
        ];
        let calls: Calls = Arc::default();
        let pause = run_of(ReadyMode::Pause, 3);
        assert_eq!(
            block_on(check_async("T", &two_nodes(), &nodes, &probes, 1000., pause, &calls)),
            Ok(())
        );
        block_on(settle(&probes, &nodes, 1000.));
        assert_eq!(probes.series_state("T|b.example"), Some((1, Phase::Answered)));
        for mode in [ReadyMode::Pause, ReadyMode::Strict] {
            assert_eq!(
                block_on(check_async(
                    "T",
                    &two_nodes(),
                    &nodes,
                    &probes,
                    1001.,
                    run_of(mode, 3),
                    &calls
                )),
                Ok(())
            );
        }
        assert_eq!(calls.lock().unwrap().len(), 2, "the series stopped at the answer");
        assert!(probes.lines().is_empty(), "no host line: {:?}", probes.lines());
    }

    /// C5: one line per silent host, for all the coins of its platform, naming `host[:port]` only.
    #[test]
    fn one_host_line_for_the_tokens_of_a_platform() {
        let probes = ReadinessProbes::default();
        let urls = [
            "https://user:pw@a.example:8545/rpc?key=SECRET",
            "https://b.example/path",
        ];
        let nodes: Vec<Node> = urls.iter().map(|url| scripted(url, &[After(30, false)])).collect();
        let run = run_of(ReadyMode::Pause, 2);
        let calls: Calls = Arc::default();
        let judge = |ticker: &'static str, now: f64| {
            let (probes, nodes, calls) = (probes.clone(), nodes.clone(), calls.clone());
            async move {
                evidence_ready_at(
                    ticker,
                    &two_nodes(),
                    &nodes,
                    |node| node_name(node.label),
                    |node| node.known,
                    &probes,
                    ReadyScope {
                        key: "evm:ETH",
                        platform: "ETH",
                    },
                    run,
                    |node: &Node| {
                        let (node, calls) = (node.clone(), calls.clone());
                        move |_bound| {
                            calls.lock().unwrap().push(node.label);
                            async move {
                                Timer::sleep_ms(30).await;
                                false
                            }
                        }
                    },
                    now,
                )
                .await
            }
        };
        block_on(async {
            let _ = futures::join!(
                judge("ETH", 1000.),
                judge("USDT-ERC20", 1000.),
                judge("DAI-ERC20", 1000.)
            );
        });
        block_on(async {
            for _ in 0..1000 {
                if probes.series_state("evm:ETH|a.example").is_some_and(|s| s.1.is_final())
                    && probes.series_state("evm:ETH|b.example").is_some_and(|s| s.1.is_final())
                {
                    break;
                }
                Timer::sleep_ms(5).await;
            }
        });
        let lines = probes.lines();
        assert!(
            lines.contains(&"swap evidence: host a.example:8545 of ETH is not answering (3 coins affected)".to_owned()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"swap evidence: host b.example:443 of ETH is not answering (3 coins affected)".to_owned()),
            "{lines:?}"
        );
        for line in &lines {
            assert!(
                !line.contains("://") && !line.contains('?') && !line.contains('@'),
                "{line}"
            );
            assert!(
                !line.contains("/rpc") && !line.contains("/path") && !line.contains("SECRET"),
                "{line}"
            );
        }
        assert_eq!(
            calls.lock().unwrap().len(),
            4,
            "each host is probed once per attempt for all the tokens"
        );
    }

    /// The line of a host repeats only when the set of silent hosts changes or after the interval.
    #[test]
    fn the_host_line_repeats_on_a_change_or_after_the_interval() {
        let probes = ReadinessProbes::default();
        let mut settings = fast(1);
        settings.host_log_interval_s = 100.;
        let run = ReadyRun {
            mode: ReadyMode::Strict,
            settings,
        };
        let calls: Calls = Arc::default();
        let b_down = [
            node("a.example:1", Answering, None),
            node("b.example:1", Unknown, Some(false)),
        ];
        let judge = |nodes: &[Node], now: f64| {
            let outcome = block_on(check_async("T", &two_nodes(), nodes, &probes, now, run, &calls));
            block_on(settle(&probes, nodes, now));
            outcome
        };
        let b_line = "swap evidence: host b.example:1 of T is not answering (1 coin affected)".to_owned();
        assert!(judge(&b_down, 1000.).is_err());
        assert_eq!(probes.lines(), vec![b_line.clone()], "logged at the first failure");
        assert!(judge(&b_down, 1030.).is_err());
        assert!(judge(&b_down, 1062.).is_err(), "probed again after the window");
        assert_eq!(probes.lines().len(), 1, "within the interval: no repeat");
        assert!(judge(&b_down, 1125.).is_err());
        assert_eq!(
            probes.lines(),
            vec![b_line.clone(), b_line.clone()],
            "after the interval"
        );
        // A second host fails: the set changes, both lines are written.
        let both_down = [
            node("a.example:1", Unknown, Some(false)),
            node("b.example:1", Unknown, Some(false)),
        ];
        assert!(judge(&both_down, 1130.).is_err());
        let lines = probes.lines();
        assert_eq!(lines.len(), 4, "{lines:?}");
        assert!(lines[2..].contains(&b_line));
        assert!(
            lines[2..].contains(&"swap evidence: host a.example:1 of T is not answering (1 coin affected)".to_owned())
        );
    }

    /// The recovery line: once, when a host logged as silent answers again.
    #[test]
    fn the_recovery_line() {
        let probes = ReadinessProbes::default();
        let run = run_of(ReadyMode::Strict, 1);
        let calls: Calls = Arc::default();
        let judge = |nodes: &[Node], now: f64| {
            let outcome = block_on(check_async("T", &two_nodes(), nodes, &probes, now, run, &calls));
            block_on(settle(&probes, nodes, now));
            outcome
        };
        let down = [
            node("a.example:1", Answering, None),
            node("b.example:1", Unknown, Some(false)),
        ];
        assert!(judge(&down, 1000.).is_err());
        let up = [
            node("a.example:1", Answering, None),
            node("b.example:1", Unknown, Some(true)),
        ];
        assert_eq!(judge(&up, 1062.), Ok(()));
        assert_eq!(probes.lines(), vec![
            "swap evidence: host b.example:1 of T is not answering (1 coin affected)".to_owned(),
            "swap evidence: host b.example:1 of T answers again".to_owned(),
        ]);
        assert_eq!(judge(&up, 1110.), Ok(()));
        assert_eq!(probes.lines().len(), 2, "once");

        // The family seeing an answer (no probe) is a recovery as well.
        let probes = ReadinessProbes::default();
        assert!(block_on(check_async("T", &two_nodes(), &down, &probes, 1000., run, &calls)).is_err());
        block_on(settle(&probes, &down, 1000.));
        probes.note_answer("T", "b.example:1", 1010.);
        assert_eq!(probes.lines().len(), 2);
        assert!(probes.lines()[1].ends_with("answers again"));
    }

    /// F1: the window of a final record counts from the end of the series. A series longer than the window still
    /// pauses, and its host is not claimed again until the window after its end has passed.
    #[test]
    fn the_window_counts_from_the_end_of_the_series() {
        let probes = ReadinessProbes::default();
        // Three attempts of 0.4 s: the series lasts over 1.2 s, so the end is later than the claim.
        let nodes = [
            node("a.example:1", Answering, None),
            scripted("b.example:1", &[After(400, false)]),
        ];
        let calls: Calls = Arc::default();
        let mut settings = fast(3);
        settings.probe_timeout_s = 1.;
        let pause = ReadyRun {
            mode: ReadyMode::Pause,
            settings,
        };
        let policy = two_nodes();
        assert_eq!(
            block_on(check_async("T", &policy, &nodes, &probes, 1000., pause, &calls)),
            Ok(())
        );
        block_on(settle(&probes, &nodes, 1000.));
        let ended_at = probes.records_ended_at("T|b.example").expect("final record");
        assert!(ended_at >= 1000. + 1.2, "the record is anchored at the end: {ended_at}");
        // The checks run on the injected clock. More than the window after the claim but less than the window after
        // the end, the host stays failed and is not claimed again (counted from the claim it would have expired).
        let at = ended_at + READY_WINDOW_SECS - 0.1;
        assert!(at - 1000. > READY_WINDOW_SECS);
        assert!(block_on(check_async("T", &policy, &nodes, &probes, at, pause, &calls)).is_err());
        assert_eq!(
            calls.lock().unwrap().len(),
            3,
            "no new series within the window after the end"
        );
        // After the window after the end, the host is probed again.
        let at = ended_at + READY_WINDOW_SECS + 1.;
        let _ = block_on(check_async("T", &policy, &nodes, &probes, at, pause, &calls));
        block_on(settle(&probes, &nodes, at));
        assert_eq!(calls.lock().unwrap().len(), 3 + 3, "a new series of three attempts");
    }

    /// F2: a series that never ended (a lost task) counts as lost once it is older than the longest a series can
    /// take, and is claimed again.
    #[test]
    fn a_lost_series_is_claimed_again() {
        let probes = ReadinessProbes::default();
        let settings = fast(3);
        assert!(probes.claim("T|b.example", 1000., settings));
        // The longest a series takes: 3 * (1 + 0.01) + 1 s.
        assert!(
            !probes.claim("T|b.example", 1004., settings),
            "still within its longest run"
        );
        assert_eq!(
            probes.view("T|b.example", 1004., ReadyMode::Strict),
            Some(HostView::InFlight { seen: 0 })
        );
        assert_eq!(probes.view("T|b.example", 1005., ReadyMode::Strict), None, "lost");
        assert!(probes.claim("T|b.example", 1005., settings), "claimed again");
    }

    /// The lab sequence: a host that failed a whole series stays not answering, in both views, across later windows
    /// and their new series; an answer clears it, with one resume and one "answers again" line. A host that never
    /// failed still counts in its first series (`a_host_fails_after_three_attempts`).
    #[test]
    fn a_failed_host_stays_failed_until_it_answers() {
        let probes = ReadinessProbes::default();
        let policy = two_nodes();
        let calls: Calls = Arc::default();
        let a = node("a.example:1", Answering, None);
        let down = [a.clone(), scripted("b.example:1", &[Answer(false)])];
        let pause = run_of(ReadyMode::Pause, 3);
        let strict = run_of(ReadyMode::Strict, 3);
        // The first series fails whole.
        assert!(block_on(check_async("T", &policy, &down, &probes, 1000., strict, &calls)).is_err());
        block_on(settle(&probes, &down, 1000.));
        assert_eq!(probes.series_state("T|b.example"), Some((3, Phase::Failed)));
        // Four later windows: at the start of each, a new series is claimed. Both views refuse, never accept.
        let mut now = 1000.;
        for _ in 0..4 {
            now += READY_WINDOW_SECS + 2.;
            assert!(
                block_on(check_async("T", &policy, &down, &probes, now, pause, &calls)).is_err(),
                "the pause holds at {now}"
            );
            // While the new series has retries pending, both views still refuse.
            assert!(block_on(check_async("T", &policy, &down, &probes, now + 0.01, pause, &calls)).is_err());
            assert!(block_on(check_async("T", &policy, &down, &probes, now + 0.01, strict, &calls)).is_err());
            block_on(settle(&probes, &down, now));
        }
        assert_eq!(
            calls.lock().unwrap().len(),
            15,
            "five series of three attempts, no extra probe"
        );
        assert_eq!(
            probes.lines().len(),
            1,
            "no repeated line within the interval: {:?}",
            probes.lines()
        );
        // An answer in a later series clears the mark: one "answers again" line, and the host counts again.
        now += READY_WINDOW_SECS + 2.;
        let up = [a, scripted("b.example:1", &[Answer(true)])];
        assert_eq!(
            block_on(check_async("T", &policy, &up, &probes, now, pause, &calls)),
            Ok(())
        );
        block_on(settle(&probes, &up, now));
        assert_eq!(probes.lines().len(), 2);
        assert!(probes.lines()[1].ends_with("host b.example:1 of T answers again"));
        for run in [pause, strict] {
            assert_eq!(
                block_on(check_async("T", &policy, &up, &probes, now + 1., run, &calls)),
                Ok(())
            );
        }
    }

    /// The own daemon has no host: its lines name the daemon and the platform.
    #[test]
    fn the_own_daemon_lines_name_no_host() {
        let probes = ReadinessProbes::default();
        let run = run_of(ReadyMode::Strict, 1);
        let calls: Calls = Arc::default();
        let policy = SwapEvidencePolicy::OwnDaemon;
        let down = [node(OWN_DAEMON_LABEL, Unknown, Some(false))];
        assert!(block_on(check_async("T", &policy, &down, &probes, 1000., run, &calls)).is_err());
        block_on(settle(&probes, &down, 1000.));
        let up = [node(OWN_DAEMON_LABEL, Unknown, Some(true))];
        assert!(block_on(check_async("T", &policy, &up, &probes, 1062., run, &calls)).is_ok());
        block_on(settle(&probes, &up, 1062.));
        assert_eq!(probes.lines(), vec![
            "swap evidence: the own daemon of T is not answering".to_owned(),
            "swap evidence: the own daemon of T answers again".to_owned(),
        ]);
    }

    /// The settings: defaults, values, and refused values.
    #[test]
    fn the_settings_of_the_conf() {
        use serde_json::json;
        let defaults = ReadinessSettings::default();
        assert_eq!(readiness_settings_from_conf(&json!({})), Ok(defaults));
        assert_eq!(
            readiness_settings_from_conf(&json!({
                "swap_evidence_probe_attempts": null,
                "swap_evidence_probe_timeout_s": null,
                "swap_evidence_host_log_interval_s": null,
            })),
            Ok(defaults)
        );
        let set = readiness_settings_from_conf(&json!({
            "swap_evidence_probe_attempts": 5,
            "swap_evidence_probe_timeout_s": 2.5,
            "swap_evidence_host_log_interval_s": 0,
        }))
        .unwrap();
        assert_eq!(
            (set.attempts, set.probe_timeout_s, set.host_log_interval_s),
            (5, 2.5, 0.)
        );
        assert_eq!(
            readiness_settings_from_conf(&json!({ "swap_evidence_probe_timeout_s": 7 }))
                .unwrap()
                .probe_timeout_s,
            7.
        );
        for (key, bad) in [
            ("swap_evidence_probe_attempts", json!(0)),
            ("swap_evidence_probe_attempts", json!(-1)),
            ("swap_evidence_probe_attempts", json!(2.5)),
            ("swap_evidence_probe_attempts", json!("3")),
            ("swap_evidence_probe_timeout_s", json!(0)),
            ("swap_evidence_probe_timeout_s", json!(-1.5)),
            ("swap_evidence_probe_timeout_s", json!("5")),
            ("swap_evidence_host_log_interval_s", json!(-1)),
            ("swap_evidence_host_log_interval_s", json!(1.5)),
            ("swap_evidence_host_log_interval_s", json!(true)),
        ] {
            let err = readiness_settings_from_conf(&json!({ key: bad })).unwrap_err();
            assert!(err.contains(key), "{key} {bad}: {err}");
        }
    }

    /// The texts name the coin and the kind only.
    #[test]
    fn the_texts_name_no_host() {
        for kind in [
            NotReadyKind::TooFewNodes,
            NotReadyKind::TooFewHostsAnswering,
            NotReadyKind::TrustedNodeSilent,
            NotReadyKind::OwnDaemonSilent,
            NotReadyKind::TooFewContractHistoryHosts,
            NotReadyKind::TrustedNodeNoContractHistory,
        ] {
            let text = not_ready_text("KMD", kind);
            assert!(text.starts_with("KMD: swap evidence not available ("));
            assert!(!text.contains("://"), "{text}");
            assert_eq!(text.matches(':').count(), 1, "only the ticker's colon: {text}");
        }
    }
}
