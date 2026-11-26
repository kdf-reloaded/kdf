//! # Purpose
//!
//! The readiness check before a swap starts (maintainer decision of 2026-11-25/26, Reloaded-original, under CRD
//! ch.51 §51.6G R100). It relaxes nothing of R100: a coin whose swap-evidence reads cannot be agreed right now does
//! not start a swap, so a maker declines and a taker stops before any fee is paid, instead of a swap that waits on
//! an undetermined read later.
//!
//! # Public exports
//!
//! - [`evidence_ready`] — the shared decision over a coin's nodes under its effective policy.
//! - [`HostReadiness`], [`NotReadyKind`], [`not_ready_text`] — what a family knows of a node, and the one text of a
//!   coin that is not ready.
//! - [`ReadinessProbes`] — the record of the readiness probes, at most one per host and [`READY_WINDOW_SECS`].
//! - [`READY_WINDOW_SECS`], [`READY_PROBE_TIMEOUT_SECS`].
//!
//! # Invariants
//!
//! - A host that answered within [`READY_WINDOW_SECS`] counts as answering and is never probed: the healthy case
//!   sends nothing extra.
//! - Probes are sent only when the answering hosts are not enough: one per unknown host, all at once, each bounded
//!   by [`READY_PROBE_TIMEOUT_SECS`], and a host is probed at most once per [`READY_WINDOW_SECS`].
//! - The text of a coin that is not ready names the coin and the kind only, never a host or a URL.

use super::{node_host, node_name, SwapEvidencePolicy};
use common::custom_futures::FutureTimerExt;
use futures::future::join_all;
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Mutex, OnceLock};

/// How long, in seconds, an answer of a host counts as recent for the readiness check; also the least gap between
/// two readiness probes of one host.
pub const READY_WINDOW_SECS: f64 = 60.;

/// The bound, in seconds, of one readiness probe.
pub const READY_PROBE_TIMEOUT_SECS: f64 = 3.;

/// The marker every text of a coin that is not ready carries.
pub const NOT_READY_MARKER: &str = "swap evidence not available";

/// What a coin family knows of one of its nodes for the readiness check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostReadiness {
    /// The node answered a request within [`READY_WINDOW_SECS`], or holds a session that keeps answering.
    Answering,
    /// A readiness probe of the node failed within [`READY_WINDOW_SECS`] (or one is running): it is not probed again
    /// before that.
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

/// What a probe of one key left.
#[derive(Clone, Copy, Debug)]
struct ProbeRecord {
    /// When the probe was claimed.
    at: f64,
    /// `None` while it runs.
    answered: Option<bool>,
}

/// The readiness probes of a set of nodes: when each key (a host, under a scope) was last probed and how that ended.
/// A key is probed at most once per [`READY_WINDOW_SECS`]; a probe that answered counts as an answer of the host for
/// that long, one that failed or still runs as "not answering".
#[derive(Debug, Default)]
pub struct ReadinessProbes {
    records: Mutex<HashMap<String, ProbeRecord>>,
}

impl ReadinessProbes {
    fn records(&self) -> std::sync::MutexGuard<'_, HashMap<String, ProbeRecord>> {
        self.records.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// What a probe of `key` within the window before `now` left: `Answering` when it answered, `NotAnswering` when
    /// it failed or still runs, `None` when no probe of the key is recent.
    pub fn recent(&self, key: &str, now: f64) -> Option<HostReadiness> {
        let record = *self.records().get(key)?;
        if now - record.at >= READY_WINDOW_SECS {
            return None;
        }
        Some(match record.answered {
            Some(true) => HostReadiness::Answering,
            Some(false) | None => HostReadiness::NotAnswering,
        })
    }

    /// Claims a probe of `key` at `now`: `true` when no probe of the key was claimed within the window, which marks
    /// the key as being probed. Records older than the window are dropped here.
    pub fn claim(&self, key: &str, now: f64) -> bool {
        let mut records = self.records();
        records.retain(|_, record| now - record.at < READY_WINDOW_SECS);
        if records.contains_key(key) {
            return false;
        }
        records.insert(key.to_owned(), ProbeRecord {
            at: now,
            answered: None,
        });
        true
    }

    /// Records the end of the probe of `key` claimed at `claimed_at`.
    pub fn finish(&self, key: &str, claimed_at: f64, answered: bool) {
        self.records().insert(key.to_owned(), ProbeRecord {
            at: claimed_at,
            answered: Some(answered),
        });
    }

    /// Records that the node `label` (a `host[:port]` form) of `scope` answered a request at `now`: its host counts
    /// as answering for the window, with no probe. For a family that keeps no answer time of its own.
    pub fn note_answer(&self, scope: &str, label: &str, now: f64) {
        self.records().insert(probe_key(scope, &host_of(label)), ProbeRecord {
            at: now,
            answered: Some(true),
        });
    }
}

/// The host a node label is grouped by: its host, or the whole label, lowercased, when it names none.
fn host_of(label: &str) -> String { node_host(label).unwrap_or_else(|| label.to_ascii_lowercase()) }

/// The key of a host of `scope` in a [`ReadinessProbes`].
fn probe_key(scope: &str, host: &str) -> String { format!("{scope}|{host}") }

/// The record of the readiness probes of the families that keep no client of their own to hold one (an own daemon,
/// the EVM, lightwalletd and walletd nodes). Callers scope the keys by the coin's ticker.
pub fn shared_readiness_probes() -> &'static ReadinessProbes {
    static SHARED: OnceLock<ReadinessProbes> = OnceLock::new();
    SHARED.get_or_init(ReadinessProbes::default)
}

/// Whether the coin `ticker` can start a swap now under its effective `policy`, over its `nodes`.
///
/// - `NotAdopted` (and a family whose swaps are off) is ready; `Insufficient` is not ([`NotReadyKind::TooFewNodes`]);
/// - `TwoNodes` is ready while at least two distinct hosts of `nodes` answer;
/// - `Trusted` while the node whose `label` names the trusted node answers;
/// - `OwnDaemon` while one of `nodes` (the daemon) answers.
///
/// `label` names a node in the `host[:port]` form ([`node_name`]); `known` is what the family knows of the node
/// (`Answering` for an answer within [`READY_WINDOW_SECS`], else `Unknown`). When the answering hosts are not enough,
/// one `probe` per host whose nodes are all unknown is sent, all at once, each bounded by
/// [`READY_PROBE_TIMEOUT_SECS`]; `probes` keeps a host from being probed twice within [`READY_WINDOW_SECS`], under
/// keys `"{scope}|{host}"`.
///
/// # Errors
///
/// The [`not_ready_text`] of the coin.
pub async fn evidence_ready<N, F, Fut>(
    ticker: &str,
    policy: &SwapEvidencePolicy,
    nodes: &[N],
    label: impl Fn(&N) -> String,
    known: impl Fn(&N) -> HostReadiness,
    probes: &ReadinessProbes,
    scope: &str,
    probe: F,
) -> Result<(), String>
where
    F: Fn(&N) -> Fut,
    Fut: Future<Output = bool>,
{
    evidence_ready_at(
        ticker,
        policy,
        nodes,
        label,
        known,
        probes,
        scope,
        probe,
        common::now_float(),
        READY_PROBE_TIMEOUT_SECS,
    )
    .await
}

/// [`evidence_ready`] at the time `now`, with the probe bound `probe_timeout_secs`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn evidence_ready_at<N, F, Fut>(
    ticker: &str,
    policy: &SwapEvidencePolicy,
    nodes: &[N],
    label: impl Fn(&N) -> String,
    known: impl Fn(&N) -> HostReadiness,
    probes: &ReadinessProbes,
    scope: &str,
    probe: F,
    now: f64,
    probe_timeout_secs: f64,
) -> Result<(), String>
where
    F: Fn(&N) -> Fut,
    Fut: Future<Output = bool>,
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
    let key_of = |host: &str| probe_key(scope, host);
    let state_of = |host: &str, index: usize| match known(&nodes[index]) {
        HostReadiness::Answering => HostReadiness::Answering,
        own => probes.recent(&key_of(host), now).unwrap_or(own),
    };

    let mut answering = 0;
    let mut unknown: Vec<(String, usize)> = Vec::new();
    for (host, members) in hosts.iter() {
        let states: Vec<HostReadiness> = members.iter().map(|index| state_of(host, *index)).collect();
        if states.contains(&HostReadiness::Answering) {
            answering += 1;
        } else if let Some(position) = states.iter().position(|state| *state == HostReadiness::Unknown) {
            unknown.push((host.clone(), members[position]));
        }
    }
    if answering >= needed {
        return Ok(());
    }

    // Not enough: one probe per unknown host, all at once, each host at most once per window.
    let claimed: Vec<(String, usize)> = unknown
        .into_iter()
        .filter(|(host, _)| probes.claim(&key_of(host), now))
        .collect();
    let outcomes = join_all(claimed.iter().map(|(_, index)| {
        let request = Box::pin(probe(&nodes[*index]));
        async move { matches!(request.timeout_secs(probe_timeout_secs).await, Ok(true)) }
    }))
    .await;
    for ((host, _), answered) in claimed.iter().zip(outcomes) {
        probes.finish(&key_of(host), now, answered);
        if answered {
            answering += 1;
        }
    }
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
    use std::time::{Duration, Instant};

    /// A scripted node: its label, what the family knows of it, and how a probe of it ends (`None`: never).
    #[derive(Clone)]
    struct Node {
        label: &'static str,
        known: HostReadiness,
        probe: Option<bool>,
    }

    fn node(label: &'static str, known: HostReadiness, probe: Option<bool>) -> Node { Node { label, known, probe } }

    fn two_nodes() -> SwapEvidencePolicy {
        SwapEvidencePolicy::TwoNodes {
            hosts: vec!["a".to_owned(), "b".to_owned()],
        }
    }

    /// Runs the check over `nodes` at `now` and returns the outcome and the labels probed.
    fn check(
        policy: &SwapEvidencePolicy,
        nodes: &[Node],
        probes: &ReadinessProbes,
        now: f64,
        timeout: f64,
    ) -> (Result<(), String>, Vec<&'static str>) {
        let probed = Mutex::new(Vec::new());
        let outcome = block_on(evidence_ready_at(
            "T",
            policy,
            nodes,
            |node| node.label.to_owned(),
            |node| node.known,
            probes,
            "T",
            |node: &Node| {
                probed.lock().unwrap().push(node.label);
                let answer = node.probe;
                async move {
                    match answer {
                        Some(answer) => answer,
                        None => futures::future::pending().await,
                    }
                }
            },
            now,
            timeout,
        ));
        (outcome, probed.into_inner().unwrap())
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
        let (outcome, probed) = check(&two_nodes(), &short, &probes, 1000., 0.05);
        assert_eq!(outcome, Ok(()));
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
        let (_, probed) = check(&two_nodes(), &failing, &probes, 1060., 0.05);
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

    /// A probe that does not answer within its bound is a failed probe, and the check ends at the bound.
    #[test]
    fn the_probe_bound_is_honored() {
        let probes = ReadinessProbes::default();
        let silent = [node("a.example:1", Answering, None), node("b.example:1", Unknown, None)];
        let started = Instant::now();
        let (outcome, probed) = check(&two_nodes(), &silent, &probes, 1000., 0.2);
        let elapsed = started.elapsed();
        assert!(outcome.is_err());
        assert_eq!(probed.len(), 1);
        assert!(
            elapsed >= Duration::from_millis(200),
            "ended before the bound: {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "the bound was not honored: {elapsed:?}"
        );
        assert_eq!(probes.recent("T|b.example", 1001.), Some(NotAnswering));
    }

    /// The production bound is 3 s and the window 60 s; the public check uses them.
    #[test]
    fn the_production_bound_is_three_seconds() {
        assert_eq!(READY_PROBE_TIMEOUT_SECS, 3.);
        assert_eq!(READY_WINDOW_SECS, 60.);
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
            "T",
            |_: &Node| futures::future::pending::<bool>(),
        ));
        let elapsed = started.elapsed();
        assert!(outcome.is_err());
        assert!(
            elapsed >= Duration::from_millis(2900) && elapsed < Duration::from_secs(6),
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

    /// Two concurrent checks never probe one host twice: the second sees the running probe as not answering.
    #[test]
    fn a_running_probe_is_not_doubled() {
        let probes = ReadinessProbes::default();
        assert!(probes.claim("T|b", 1000.));
        assert!(!probes.claim("T|b", 1000.5));
        assert_eq!(probes.recent("T|b", 1000.5), Some(NotAnswering));
        probes.finish("T|b", 1000., true);
        assert_eq!(probes.recent("T|b", 1001.), Some(Answering));
        assert_eq!(probes.recent("T|b", 1060.), None);
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
