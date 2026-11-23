//! # Purpose
//!
//! The shared core of the swap-evidence rule (CRD ch.51 §51.6G, R100): the chain reads a refund or a claim rests
//! on must agree between two nodes of the coin's node pool, unless the operator trusts one named node
//! (`swap_evidence_node`) or the coin is served by the operator's own daemon.
//!
//! # Public exports
//!
//! - [`SwapEvidencePolicy`] — what a coin answers with, fixed at activation.
//! - [`node_host`] / [`swap_evidence_policy_from`] — the node identity rule and the policy from a node list.
//! - [`SwapEvidenceNodeNotInList`] — the one error of a `swap_evidence_node` outside the coin's node list.
//! - [`agree`] with [`Agreement`], [`Undetermined`], [`UndeterminedReason`] — the agreement helper the families
//!   wire into their swap reads.
//!
//! # Invariants
//!
//! - A node is a distinct host name (case-insensitive); two ports or two schemes of one host count once.
//! - A policy never invents a node: `Trusted` names an entry of the list the coin was activated with.
//! - [`agree`] never takes a third node's answer to outvote two that differ in one attempt.

use crate::node_url::electrum_host_port;
use futures::stream::{FuturesUnordered, StreamExt};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::future::Future;

/// How a coin's swap-evidence reads are judged (CRD ch.51 R100).
#[derive(Clone, PartialEq, Eq)]
pub enum SwapEvidencePolicy {
    /// At least two distinct hosts; two of them must agree. `hosts` lists the distinct hosts in list order.
    TwoNodes { hosts: Vec<String> },
    /// The operator named one node of the list; its answer alone is taken.
    Trusted { node: String },
    /// The coin is served by the operator's own daemon, which is trusted without a field.
    OwnDaemon,
    /// Fewer than two distinct hosts and no trusted node: the coin does not start a swap.
    Insufficient { hosts: usize },
    /// The coin's family has not adopted R100 yet: today's reads, no admission refusal. See [`R100_ADOPTED`].
    NotAdopted,
}

/// The swap-capable coin families R100 binds (CRD ch.51 R100 item 5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwapFamily {
    Utxo,
    Bch,
    Slp,
    Qtum,
    Qrc20,
    Evm,
    Zcoin,
    Sia,
    Tendermint,
    Tron,
    Solana,
}

/// Where a family stands with R100 (CRD ch.51 R100 item 5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum R100Adoption {
    /// The family's reads agree on two nodes; its `swap_evidence_policy()` answers the policy stored at activation.
    Adopted,
    /// Not adopted yet: today's reads, no admission refusal.
    NotAdopted,
    /// The family's swaps are switched off in this release, so no swap read exists to bind. Behaves as
    /// `NotAdopted`; the existing switches already refuse trading. The tie tests pin this entry to the real switches
    /// (`TENDERMINT_SWAPS_AVAILABLE`, `TRON_SWAPS_AVAILABLE`, the Solana untradable rule): a lane that switches swaps
    /// on must adopt R100, or CI fails.
    SwapsOff,
}

/// The one adoption table of R100 (maintainer decision of 2026-11-22). A family flips to
/// [`R100Adoption::Adopted`] once its reads agree on two nodes. The release gate
/// (`r100_release_gate_every_swap_family_adopted`, ignored in ordinary CI) passes when every entry is `Adopted` or
/// `SwapsOff`, so it binds only the families that swap in the release.
pub const R100_ADOPTED: &[(SwapFamily, R100Adoption)] = &[
    (SwapFamily::Utxo, R100Adoption::NotAdopted),
    (SwapFamily::Bch, R100Adoption::NotAdopted),
    (SwapFamily::Slp, R100Adoption::NotAdopted),
    (SwapFamily::Qtum, R100Adoption::NotAdopted),
    (SwapFamily::Qrc20, R100Adoption::NotAdopted),
    (SwapFamily::Evm, R100Adoption::NotAdopted),
    (SwapFamily::Zcoin, R100Adoption::NotAdopted),
    (SwapFamily::Sia, R100Adoption::NotAdopted),
    (SwapFamily::Tendermint, R100Adoption::SwapsOff),
    (SwapFamily::Tron, R100Adoption::Adopted),
    (SwapFamily::Solana, R100Adoption::SwapsOff),
];

/// The table entry of `family`.
pub fn adoption_of(family: SwapFamily) -> Option<R100Adoption> {
    R100_ADOPTED
        .iter()
        .find(|(candidate, _)| *candidate == family)
        .map(|(_, adoption)| *adoption)
}

/// Whether `family` has adopted R100 (true only for [`R100Adoption::Adopted`]).
pub fn family_adopted(family: SwapFamily) -> bool { adoption_of(family) == Some(R100Adoption::Adopted) }

/// What a coin of `family` answers with: its `stored` policy once the family has adopted R100, else
/// [`SwapEvidencePolicy::NotAdopted`] (also for a family whose swaps are off).
pub fn effective_policy(family: SwapFamily, stored: &SwapEvidencePolicy) -> SwapEvidencePolicy {
    if family_adopted(family) {
        stored.clone()
    } else {
        SwapEvidencePolicy::NotAdopted
    }
}

// A node address may carry a credential, so the debug form names the host only.
impl fmt::Debug for SwapEvidencePolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SwapEvidencePolicy::TwoNodes { hosts } => f.debug_struct("TwoNodes").field("hosts", hosts).finish(),
            SwapEvidencePolicy::Trusted { node } => f
                .debug_struct("Trusted")
                .field("node", &node_host(node).unwrap_or_else(|| "<unnamed>".to_owned()))
                .finish(),
            SwapEvidencePolicy::OwnDaemon => f.write_str("OwnDaemon"),
            SwapEvidencePolicy::NotAdopted => f.write_str("NotAdopted"),
            SwapEvidencePolicy::Insufficient { hosts } => f.debug_struct("Insufficient").field("hosts", hosts).finish(),
        }
    }
}

/// A `swap_evidence_node` that names no entry of the coin's node list (or a coin that has no list). `node` is the
/// field as [`node_name`] renders it (`host[:port]`), never the raw value, which may carry a credential.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct SwapEvidenceNodeNotInList {
    pub ticker: String,
    pub node: String,
}

impl fmt::Display for SwapEvidenceNodeNotInList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "swap_evidence_node {} is not in the node list of {}",
            self.node, self.ticker
        )
    }
}

impl std::error::Error for SwapEvidenceNodeNotInList {}

/// The reason text of a refused order or swap start for a coin whose policy is [`SwapEvidencePolicy::Insufficient`].
pub fn insufficient_refusal(ticker: &str) -> String {
    format!("{ticker} has fewer than two nodes for swap evidence; add a second node or set swap_evidence_node")
}

/// The host and the port as written of a node address: an Electrum `host:port`, an `https://...` / `wss://...` URL
/// or a bare host. The host is lowercased; an IPv6 host keeps its brackets and is never split at a colon inside them.
fn parse_node(addr: &str) -> Option<(String, Option<u16>)> {
    let addr = addr.trim();
    let rest = addr.split_once("://").map_or(addr, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, host_port)| host_port);
    if let Some(after) = host_port.strip_prefix('[') {
        let (inner, tail) = after.split_once(']')?;
        if inner.is_empty() || !inner.chars().all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.') {
            return None;
        }
        let port = match tail {
            "" => None,
            tail => Some(tail.strip_prefix(':')?.parse::<u16>().ok()?),
        };
        return Some((format!("[{}]", inner.to_ascii_lowercase()), port));
    }
    let name = electrum_host_port(addr)?;
    Some(match name.rsplit_once(':') {
        Some((host, port)) => (host.to_ascii_lowercase(), port.parse::<u16>().ok()),
        None => (name.to_ascii_lowercase(), None),
    })
}

/// [`parse_node`] with the default port of an `http`, `https`, `ws` or `wss` scheme when none is written.
fn host_and_port(addr: &str) -> Option<(String, Option<u16>)> {
    let (host, port) = parse_node(addr)?;
    let port = port.or_else(|| {
        match addr
            .trim()
            .split_once("://")
            .map(|(scheme, _)| scheme.to_ascii_lowercase())
        {
            Some(scheme) if scheme == "https" || scheme == "wss" => Some(443),
            Some(scheme) if scheme == "http" || scheme == "ws" => Some(80),
            _ => None,
        }
    });
    Some((host, port))
}

/// The form in which an error or a log line names a node, and in which a node is compared with the trusted node:
/// `host[:port]`, lowercased, with the port the address writes or else the default of its scheme (`https`/`wss` 443,
/// `http`/`ws` 80); an Electrum `host:port` or a bare host is unchanged. A scheme, userinfo, path and query (which may hold a credential) are dropped; an address with no host
/// is rendered as [`crate::node_url::UNNAMED_NODE`].
pub fn node_name(addr: &str) -> String {
    match host_and_port(addr) {
        Some((host, Some(port))) => format!("{host}:{port}"),
        Some((host, None)) => host,
        None => crate::node_url::UNNAMED_NODE.to_owned(),
    }
}

/// The host of one node address, lowercased (CRD ch.51 R100 item 1: a node is a distinct host name). `None` when
/// the address names no host.
pub fn node_host(url: &str) -> Option<String> { host_and_port(url).map(|(host, _)| host) }

/// Whether the `swap_evidence_node` value `field` names the list entry `entry`: the same host, and the same port only
/// when BOTH the field and the entry have one (written, or the default of an `http(s)`/`ws(s)` scheme); the scheme
/// itself is ignored. So a bare-host entry matches a field `host:443`, a field `host` matches an entry `host:50002`,
/// and an entry `https://a.example:8443` does not match a field `a.example:443`.
pub fn node_matches(field: &str, entry: &str) -> bool {
    let (Some((field_host, field_port)), Some((entry_host, entry_port))) = (host_and_port(field), host_and_port(entry))
    else {
        return false;
    };
    field_host == entry_host
        && match (field_port, entry_port) {
            (Some(field_port), Some(entry_port)) => field_port == entry_port,
            _ => true,
        }
}

/// The distinct hosts of `nodes`, in list order.
pub(crate) fn distinct_hosts(nodes: &[String]) -> Vec<String> {
    let mut hosts: Vec<String> = Vec::new();
    for host in nodes.iter().filter_map(|node| node_host(node)) {
        if !hosts.contains(&host) {
            hosts.push(host);
        }
    }
    hosts
}

/// The policy of a coin with the node list `nodes` and the optional `swap_evidence_node` value `field`.
pub fn swap_evidence_policy_from(
    ticker: &str,
    field: Option<&str>,
    nodes: &[String],
) -> Result<SwapEvidencePolicy, SwapEvidenceNodeNotInList> {
    if let Some(field) = field {
        return match nodes.iter().find(|entry| node_matches(field, entry)) {
            Some(entry) => Ok(SwapEvidencePolicy::Trusted { node: entry.clone() }),
            None => Err(SwapEvidenceNodeNotInList {
                ticker: ticker.to_owned(),
                node: node_name(field),
            }),
        };
    }
    let hosts = distinct_hosts(nodes);
    if hosts.len() >= 2 {
        Ok(SwapEvidencePolicy::TwoNodes { hosts })
    } else {
        Ok(SwapEvidencePolicy::Insufficient { hosts: hosts.len() })
    }
}

/// A `swap_evidence_node` set for a coin that has no node list (a coin served by its own daemon).
pub fn swap_evidence_field_without_list(ticker: &str, field: Option<&str>) -> Result<(), SwapEvidenceNodeNotInList> {
    match field {
        Some(node) => Err(SwapEvidenceNodeNotInList {
            ticker: ticker.to_owned(),
            node: node_name(node),
        }),
        None => Ok(()),
    }
}

/// Why a read is undetermined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UndeterminedReason {
    /// Two nodes answered with different decisions.
    Disagree,
    /// Fewer than two nodes answered (or the coin has no two nodes to ask).
    TooFewAnswers,
    /// The trusted node did not answer; no other node is taken in its place.
    TrustedNodeSilent,
    /// The two answering nodes gave the same decision, but their tips differ by more than the allowed gap
    /// ([`agree_with_tips`]).
    TipGap,
}

/// A read that cannot be decided: the step waits and must not refund or claim on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Undetermined {
    pub reason: UndeterminedReason,
    /// For each node asked, its label and a short summary of its answer or its error, for the log line.
    pub per_node: Vec<(String, String)>,
}

/// The start of the text of every undetermined swap read, the one marker the engine matches on (see
/// [`is_undetermined`]).
pub const UNDETERMINED_PREFIX: &str = "swap evidence undetermined: ";

/// The only constructor of the error text of an undetermined read: [`UNDETERMINED_PREFIX`], the `reason` and the
/// node labels (`host[:port]` forms, see [`node_name`], never URLs).
pub fn undetermined(reason: &str, nodes: &[&str]) -> String {
    if nodes.is_empty() {
        format!("{UNDETERMINED_PREFIX}{reason}")
    } else {
        format!("{UNDETERMINED_PREFIX}{reason} ({})", nodes.join(", "))
    }
}

/// The only test of an error text for an undetermined read. It looks for the prefix anywhere in the text, because
/// `try_s!` and `ERR!` put a `file:line]` in front of it. It matches the prefix anywhere, so a lower-layer error
/// that quotes it counts as undetermined; that errs toward waiting, the safe side.
pub fn is_undetermined(err: &str) -> bool { err.contains(UNDETERMINED_PREFIX) }

impl Undetermined {
    /// The error text of this read: [`UNDETERMINED_PREFIX`], the reason, and what each node said, as
    /// `(label: summary; label: summary)`. A summary is the decision or the fixed error text of the node's `ask`,
    /// which carries no URL or credential by the contract of [`agree`]. [`undetermined`] builds the labels-only form
    /// for callers without an `Undetermined` value; both start with the prefix.
    pub fn to_err_string(&self) -> String {
        let reason = match self.reason {
            UndeterminedReason::Disagree => "the nodes disagree",
            UndeterminedReason::TooFewAnswers => "fewer than two nodes answered",
            UndeterminedReason::TrustedNodeSilent => "the trusted node did not answer",
            UndeterminedReason::TipGap => "the tips of the nodes differ too much",
        };
        if self.per_node.is_empty() {
            return format!("{UNDETERMINED_PREFIX}{reason}");
        }
        let nodes: Vec<String> = self
            .per_node
            .iter()
            .map(|(label, summary)| format!("{label}: {summary}"))
            .collect();
        format!("{UNDETERMINED_PREFIX}{reason} ({})", nodes.join("; "))
    }
}

impl fmt::Display for Undetermined {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.to_err_string()) }
}

/// The outcome of [`agree`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Agreement<A, K> {
    /// The answer taken (the first of the agreeing answers in node order, or the one that proves itself, or the
    /// trusted or own node's) and the decision it carries.
    Agreed {
        answer: A,
        decision: K,
    },
    Undetermined(Undetermined),
}

enum Asked<A, K> {
    Answer { index: usize, answer: A, decision: K },
    Failed { index: usize, error: String },
}

/// Asks `nodes` the same read under `policy` and compares the decisions (CRD ch.51 R100).
///
/// - `ask` asks one node and returns its answer or an error text;
/// - `classify` reduces an answer to the step's decision, which two answers are compared on;
/// - `proves_itself` is `true` for an answer the node cannot be wrong about (a secret whose hash is the swap's
///   secret hash): it is taken alone, at once;
/// - `label` names a node for the log line and for matching the trusted node.
///
/// `label` must return a `host[:port]` form of the node (see [`node_name`]), never a raw URL: it is matched against
/// the trusted node and it appears in the log line of an undetermined read. The per-node texts of an
/// [`Undetermined`] carry what `ask` returns as its error, so an `ask` error must be free of URLs and credentials,
/// as `label` must be.
///
/// A secret that proves itself needs no agreeing read (R100): whenever the read would otherwise end undetermined
/// because too few nodes answered (an `Insufficient` policy, a silent trusted node, fewer than two answers), the nodes
/// not asked yet are asked and an answer that proves itself is taken from any of them. Any other answer of those
/// nodes is ignored and the result stays undetermined with the original reason; a disagreement of two answers is not
/// reopened.
///
/// Each call asks one pair and decides only when that pair agrees: when two answers differ, no third node is asked.
///
/// `ask` must read each node at that node's own tip: comparing the nodes at a common lower height is not allowed for
/// a read that can prove itself, because a node that is behind could hide a claim the other node already shows. Use
/// [`agree_with_tips`] to refuse a pair whose tips are far apart.
pub async fn agree<N, A, K, F, Fut>(
    policy: &SwapEvidencePolicy,
    nodes: &[N],
    label: impl Fn(&N) -> String,
    ask: F,
    classify: impl Fn(&A) -> K,
    proves_itself: impl Fn(&A) -> bool,
) -> Agreement<A, K>
where
    K: PartialEq + fmt::Debug,
    F: Fn(&N) -> Fut,
    Fut: Future<Output = Result<A, String>>,
{
    agree_with_tips(policy, nodes, label, ask, classify, proves_itself, |_| None, 0).await
}

/// [`agree`] with a tip-gap rule. After `proves_itself` (an answer that proves itself is taken at once, whatever the
/// other node says) the two decisions are compared first: different decisions are `Undetermined(Disagree)`, even when
/// the tips are also far apart (the gap is only logged at DEBUG, naming the two node labels). Equal decisions from a
/// pair that both report a tip, the tips differing by more than `max_gap`, are `Undetermined(TipGap)`. So `TipGap`
/// always means "the same answer, tips too far apart", and a caller that keeps a second source for the cases that are
/// not a disagreement cannot use it to outvote a pair that disagreed.
///
/// `tip_of` returns a monotonic tip measure in the same unit as `max_gap`: a block height, or the timestamp of the tip
/// block in seconds, at the caller's choice. `ask` must read each node at that node's own tip; comparing at a common
/// lower height is not allowed for a read that can prove itself.
///
/// Otherwise as [`agree`].
///
/// # Arguments
///
/// - `ask` asks one node and returns its answer or an error text;
/// - `classify` reduces an answer to the step's decision, which two answers are compared on;
/// - `proves_itself` is `true` for an answer the node cannot be wrong about (a secret whose hash is the swap's
///   secret hash): it is taken alone, at once;
/// - `label` names a node for the log line and for matching the trusted node.
///
/// `label` must return a `host[:port]` form of the node (see [`node_name`]), never a raw URL: it is matched against
/// the trusted node and it appears in the log line of an undetermined read. The per-node texts of an
/// [`Undetermined`] carry what `ask` returns as its error, so an `ask` error must be free of URLs and credentials,
/// as `label` must be.
///
/// A secret that proves itself needs no agreeing read (R100): whenever the read would otherwise end undetermined
/// because too few nodes answered (an `Insufficient` policy, a silent trusted node, fewer than two answers), the nodes
/// not asked yet are asked and an answer that proves itself is taken from any of them. Any other answer of those
/// nodes is ignored and the result stays undetermined with the original reason; a disagreement of two answers is not
/// reopened.
///
/// Each call asks one pair and decides only when that pair agrees: when two answers differ, no third node is asked.
pub async fn agree_with_tips<N, A, K, F, Fut>(
    policy: &SwapEvidencePolicy,
    nodes: &[N],
    label: impl Fn(&N) -> String,
    ask: F,
    classify: impl Fn(&A) -> K,
    proves_itself: impl Fn(&A) -> bool,
    tip_of: impl Fn(&A) -> Option<u64>,
    max_gap: u64,
) -> Agreement<A, K>
where
    K: PartialEq + fmt::Debug,
    F: Fn(&N) -> Fut,
    Fut: Future<Output = Result<A, String>>,
{
    let labels: Vec<String> = nodes.iter().map(&label).collect();
    let classify = &classify;
    let proves_itself = &proves_itself;
    // The nodes not in `asked`, all at once: the first answer that proves itself, any other answer ignored.
    let proof_from_rest = |asked: Vec<usize>| {
        let pending: FuturesUnordered<_> = (0..nodes.len())
            .filter(|index| !asked.contains(index))
            .map(|index| {
                let future = ask(&nodes[index]);
                async move { future.await.ok() }
            })
            .collect();
        async move {
            let mut pending = pending;
            while let Some(answer) = pending.next().await {
                if let Some(answer) = answer.filter(|answer| proves_itself(answer)) {
                    let decision = classify(&answer);
                    return Some((answer, decision));
                }
            }
            None
        }
    };
    let mut per_node: Vec<(usize, String, String)> = Vec::new();
    let finish = |reason: UndeterminedReason, mut per_node: Vec<(usize, String, String)>| {
        per_node.sort_by_key(|(index, _, _)| *index);
        Agreement::Undetermined(Undetermined {
            reason,
            per_node: per_node
                .into_iter()
                .map(|(_, label, summary)| (label, summary))
                .collect(),
        })
    };

    // The candidate indexes in the order they are asked, and how many answers decide.
    let (candidates, needed, silent_reason): (Vec<usize>, usize, UndeterminedReason) = match policy {
        SwapEvidencePolicy::Insufficient { .. } => {
            return match proof_from_rest(Vec::new()).await {
                Some((answer, decision)) => Agreement::Agreed { answer, decision },
                None => finish(UndeterminedReason::TooFewAnswers, per_node),
            };
        },
        // A family that has not adopted R100 reads as today: one routed node, its answer taken.
        SwapEvidencePolicy::OwnDaemon | SwapEvidencePolicy::NotAdopted => {
            ((0..nodes.len()).take(1).collect(), 1, UndeterminedReason::TooFewAnswers)
        },
        SwapEvidencePolicy::Trusted { node } => (
            (0..nodes.len())
                .filter(|index| node_name(node) == node_name(&labels[*index]))
                .collect(),
            1,
            UndeterminedReason::TrustedNodeSilent,
        ),
        SwapEvidencePolicy::TwoNodes { .. } => {
            let mut seen: Vec<String> = Vec::new();
            let mut candidates = Vec::new();
            for (index, node_label) in labels.iter().enumerate() {
                let host = node_host(node_label).unwrap_or_else(|| node_label.to_ascii_lowercase());
                if !seen.contains(&host) {
                    seen.push(host);
                    candidates.push(index);
                }
            }
            (candidates, 2, UndeterminedReason::TooFewAnswers)
        },
    };

    let ask_one = |index: usize| {
        let future = ask(&nodes[index]);
        async move {
            match future.await {
                Ok(answer) => {
                    let decision = classify(&answer);
                    Asked::Answer {
                        index,
                        answer,
                        decision,
                    }
                },
                Err(error) => Asked::Failed { index, error },
            }
        }
    };

    let mut next = candidates.into_iter();
    let mut in_flight = FuturesUnordered::new();
    for index in next.by_ref().take(needed) {
        in_flight.push(ask_one(index));
    }
    let mut answers: Vec<(usize, A, K, Option<u64>)> = Vec::new();
    while let Some(asked) = in_flight.next().await {
        match asked {
            Asked::Answer {
                index,
                answer,
                decision,
            } => {
                if proves_itself(&answer) {
                    return Agreement::Agreed { answer, decision };
                }
                per_node.push((index, labels[index].clone(), format!("{decision:?}")));
                let tip = tip_of(&answer);
                answers.push((index, answer, decision, tip));
                if answers.len() == needed {
                    break;
                }
            },
            Asked::Failed { index, error } => {
                per_node.push((index, labels[index].clone(), format!("no answer: {error}")));
                if let Some(index) = next.next() {
                    in_flight.push(ask_one(index));
                }
            },
        }
    }

    if answers.len() < needed {
        let asked: Vec<usize> = per_node.iter().map(|(index, _, _)| *index).collect();
        return match proof_from_rest(asked).await {
            Some((answer, decision)) => Agreement::Agreed { answer, decision },
            None => finish(silent_reason, per_node),
        };
    }
    answers.sort_by_key(|(index, _, _, _)| *index);
    // The decisions are compared first: two different decisions are a disagreement, whatever the tips are, so
    // `TipGap` always means "the same answer, tips too far apart".
    let tip_gap = match answers.as_slice() {
        [(first, _, _, Some(first_tip)), (second, _, _, Some(second_tip))] => {
            let gap = first_tip.abs_diff(*second_tip);
            (gap > max_gap).then(|| {
                common::log::debug!(
                    "swap evidence: tips differ by {gap}, more than {max_gap}: {} and {}",
                    labels[*first],
                    labels[*second]
                );
            })
        },
        _ => None,
    };
    let (_, answer, decision, _) = answers.remove(0);
    if !answers.iter().all(|(_, _, other, _)| *other == decision) {
        return finish(UndeterminedReason::Disagree, per_node);
    }
    if tip_gap.is_some() {
        // The same answer from nodes whose tips are far apart: undetermined, and the nodes not asked yet may still
        // show a proof.
        let asked: Vec<usize> = per_node.iter().map(|(index, _, _)| *index).collect();
        return match proof_from_rest(asked).await {
            Some((answer, decision)) => Agreement::Agreed { answer, decision },
            None => finish(UndeterminedReason::TipGap, per_node),
        };
    }
    Agreement::Agreed { answer, decision }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use std::cell::RefCell;

    fn list(entries: &[&str]) -> Vec<String> { entries.iter().map(|entry| (*entry).to_owned()).collect() }

    #[test]
    fn undetermined_marker_survives_wrapping() {
        let text = undetermined("disagree", &["a:1", "b:2"]);
        assert!(is_undetermined(&format!("x.rs:1] {text}")));
        assert!(is_undetermined(&text));
        assert!(!is_undetermined("x.rs:1] connection refused"));
        assert!(!text.contains("://"));
        assert_eq!(text, "swap evidence undetermined: disagree (a:1, b:2)");
        let read = Undetermined {
            reason: UndeterminedReason::TipGap,
            per_node: vec![("a:1".to_owned(), "x".to_owned())],
        };
        assert!(is_undetermined(&read.to_err_string()));
        assert_eq!(read.to_string(), read.to_err_string());
        // the long form carries what each node said
        let disagree = Undetermined {
            reason: UndeterminedReason::Disagree,
            per_node: vec![
                ("a:1".to_owned(), "Claimed".to_owned()),
                ("b:2".to_owned(), "NotSpent".to_owned()),
            ],
        };
        let long = disagree.to_err_string();
        assert_eq!(
            long,
            "swap evidence undetermined: the nodes disagree (a:1: Claimed; b:2: NotSpent)"
        );
        assert_eq!(disagree.to_string(), long);
        assert!(is_undetermined(&long));
        assert!(is_undetermined(&format!("x.rs:1] {long}")));
    }

    #[test]
    fn node_host_forms() {
        assert_eq!(
            node_host("Electrum.Example.com:50002").as_deref(),
            Some("electrum.example.com")
        );
        assert_eq!(
            node_host("https://Node.Example.com:8545/path?key=1").as_deref(),
            Some("node.example.com")
        );
        assert_eq!(
            node_host("wss://node.example.com/ws").as_deref(),
            Some("node.example.com")
        );
        assert_eq!(node_host("node.example.com").as_deref(), Some("node.example.com"));
        assert_eq!(node_host("[2001:DB8::1]:50002").as_deref(), Some("[2001:db8::1]"));
        assert_eq!(node_host("https://[::1]:8545").as_deref(), Some("[::1]"));
        assert_eq!(
            node_host("https://user:secret@node.example.com/x").as_deref(),
            Some("node.example.com")
        );
        assert_eq!(node_host(""), None);
    }

    #[test]
    fn field_match_rule() {
        let nodes = list(&[
            "a.example.com:50001",
            "a.example.com:50002",
            "wss://B.example.com/ws",
            "https://c.example.com:8545",
        ]);
        // a port in the field must match; no port matches by host
        assert!(node_matches("a.example.com:50002", &nodes[1]));
        assert!(!node_matches("a.example.com:50003", &nodes[1]));
        assert!(node_matches("A.EXAMPLE.COM", &nodes[0]));
        // the scheme is ignored
        assert!(node_matches("https://b.example.com", &nodes[2]));
        assert!(node_matches("b.example.com:443", &nodes[2]));
        assert!(!node_matches("b.example.com:444", &nodes[2]));
        assert!(node_matches("c.example.com:8545", &nodes[3]));
        assert!(node_matches("wss://c.example.com:8545", &nodes[3]));
        let policy = swap_evidence_policy_from("T", Some("a.example.com:50002"), &nodes).unwrap();
        assert_eq!(policy, SwapEvidencePolicy::Trusted { node: nodes[1].clone() });
        // a field outside the list is an error, with the wire shape
        let error = swap_evidence_policy_from("T", Some("z.example.com"), &nodes).unwrap_err();
        assert_eq!(
            error.to_string(),
            "swap_evidence_node z.example.com is not in the node list of T"
        );
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            serde_json::json!({"ticker": "T", "node": "z.example.com"})
        );
        // IPv6
        let v6 = list(&["[::1]:50001", "[::2]:50001"]);
        assert!(swap_evidence_policy_from("T", Some("[::2]:50001"), &v6).is_ok());
        assert!(swap_evidence_policy_from("T", Some("[::2]:50002"), &v6).is_err());
    }

    /// A field that carries a credential is named `host[:port]` only, in the error and in its text.
    #[test]
    fn a_field_outside_the_list_is_named_without_its_credential() {
        let nodes = list(&["a.example:50002"]);
        let error = swap_evidence_policy_from("T", Some("https://user:secret@evil.example:8545/v3/KEY?x=1"), &nodes)
            .unwrap_err();
        assert_eq!(error.node, "evil.example:8545", "the node is not host[:port]");
        let text = error.to_string();
        assert!(!text.contains("secret"), "the text carries the userinfo");
        assert!(!text.contains("KEY"), "the text carries the path");
        assert!(!text.contains("x=1"), "the text carries the query");
        let wire = serde_json::to_string(&error).unwrap();
        assert!(
            !wire.contains("secret") && !wire.contains("KEY"),
            "the wire form carries a credential"
        );
        assert_eq!(node_name("HTTPS://Node.Example.com/v3/KEY"), "node.example.com:443");
    }

    /// One rule for the field match and the Trusted selection: the scheme's default port is written by `node_name`.
    #[test]
    fn node_name_writes_the_default_port_of_a_scheme() {
        assert_eq!(node_name("https://a.example/x"), "a.example:443");
        assert_eq!(node_name("wss://a.example/ws"), "a.example:443");
        assert_eq!(node_name("http://a.example"), "a.example:80");
        assert_eq!(node_name("https://a.example:8545"), "a.example:8545");
        assert_eq!(node_name("a.example:50002"), "a.example:50002");
        assert_eq!(node_name("A.Example"), "a.example");
    }

    /// A trusted https node without a port is found by its own label.
    #[test]
    fn agree_trusted_node_without_a_port_matches_its_label() {
        let entry = "https://walletd.example/";
        let policy = SwapEvidencePolicy::Trusted { node: entry.to_owned() };
        let label = node_name(entry);
        assert_eq!(label, "walletd.example:443");
        let script: Script = vec![("other.example:443", Ok("none")), ("walletd.example:443", Ok("spent"))];
        let (outcome, asked) = run(policy, &script, &[]);
        assert_eq!(outcome, Agreement::Agreed {
            answer: "spent",
            decision: "spent".to_owned()
        });
        assert_eq!(asked, vec![label]);
    }

    /// Ports are compared only when both sides have one.
    #[test]
    fn ports_are_compared_only_when_both_sides_have_one() {
        let bare = list(&["lwd.example"]);
        assert_eq!(
            swap_evidence_policy_from("T", Some("lwd.example:443"), &bare).unwrap(),
            SwapEvidencePolicy::Trusted {
                node: "lwd.example".to_owned()
            }
        );
        assert!(node_matches("lwd.example", "lwd.example:50002"));
        assert!(node_matches("https://lwd.example", "lwd.example"));
        let https = list(&["https://a.example:8443"]);
        assert!(swap_evidence_policy_from("T", Some("a.example:443"), &https).is_err());
        assert!(swap_evidence_policy_from("T", Some("a.example:8443"), &https).is_ok());
        // two explicit ports of one host are still one node for counting
        let policy = swap_evidence_policy_from("T", None, &list(&["a.example:1", "a.example:2"])).unwrap();
        assert_eq!(policy, SwapEvidencePolicy::Insufficient { hosts: 1 });
    }

    #[test]
    fn policy_from_node_lists() {
        let one = swap_evidence_policy_from("T", None, &list(&["a.example.com:50001"])).unwrap();
        assert_eq!(one, SwapEvidencePolicy::Insufficient { hosts: 1 });
        let two_ports =
            swap_evidence_policy_from("T", None, &list(&["a.example.com:50001", "A.example.com:50002"])).unwrap();
        assert_eq!(two_ports, SwapEvidencePolicy::Insufficient { hosts: 1 });
        let schemes =
            swap_evidence_policy_from("T", None, &list(&["https://a.example.com", "wss://a.example.com/ws"])).unwrap();
        assert_eq!(schemes, SwapEvidencePolicy::Insufficient { hosts: 1 });
        let two = swap_evidence_policy_from("T", None, &list(&["a.example.com:50001", "b.example.com:50001"])).unwrap();
        assert_eq!(two, SwapEvidencePolicy::TwoNodes {
            hosts: list(&["a.example.com", "b.example.com"])
        });
        let none = swap_evidence_policy_from("T", None, &[]).unwrap();
        assert_eq!(none, SwapEvidencePolicy::Insufficient { hosts: 0 });
        assert!(swap_evidence_field_without_list("T", None).is_ok());
        assert!(swap_evidence_field_without_list("T", Some("x")).is_err());
    }

    #[test]
    fn debug_form_names_the_host_only() {
        let policy = SwapEvidencePolicy::Trusted {
            node: "https://node.example.com/v3/APIKEY".to_owned(),
        };
        let text = format!("{policy:?}");
        assert!(text.contains("node.example.com") && !text.contains("APIKEY"), "{text}");
    }

    type Script = Vec<(&'static str, Result<&'static str, &'static str>)>;

    /// Runs `agree` over scripted nodes; answers are `(label, result)`. Returns the outcome and the labels asked.
    fn run(
        policy: SwapEvidencePolicy,
        script: &Script,
        proves: &'static [&'static str],
    ) -> (Agreement<&'static str, String>, Vec<String>) {
        let asked = RefCell::new(Vec::new());
        let outcome = block_on(agree(
            &policy,
            script,
            |(label, _)| (*label).to_owned(),
            |(label, result)| {
                asked.borrow_mut().push((*label).to_owned());
                let result = result.map_err(str::to_owned);
                async move { result }
            },
            |answer: &&'static str| answer.split(':').next().unwrap().to_owned(),
            |answer| proves.contains(answer),
        ));
        (outcome, asked.into_inner())
    }

    fn two() -> SwapEvidencePolicy {
        SwapEvidencePolicy::TwoNodes {
            hosts: list(&["a", "b", "c"]),
        }
    }

    #[test]
    fn agree_two_agreeing() {
        let script: Script = vec![("a:1", Ok("spent:1")), ("b:1", Ok("spent:2")), ("c:1", Ok("none"))];
        let (outcome, asked) = run(two(), &script, &[]);
        assert_eq!(outcome, Agreement::Agreed {
            answer: "spent:1",
            decision: "spent".to_owned()
        });
        assert_eq!(asked, vec!["a:1", "b:1"]);
    }

    #[test]
    fn agree_two_differing_asks_no_third() {
        let script: Script = vec![("a:1", Ok("spent")), ("b:1", Ok("none")), ("c:1", Ok("spent"))];
        let (outcome, asked) = run(two(), &script, &[]);
        match outcome {
            Agreement::Undetermined(u) => {
                assert_eq!(u.reason, UndeterminedReason::Disagree);
                assert_eq!(u.per_node.len(), 2);
            },
            other => panic!("{other:?}"),
        }
        assert_eq!(asked, vec!["a:1", "b:1"]);
    }

    #[test]
    fn agree_skips_a_host_already_asked_and_walks_past_a_failure() {
        let script: Script = vec![
            ("a:1", Err("down")),
            ("a:2", Ok("x")),
            ("b:1", Ok("x")),
            ("c:1", Ok("x")),
        ];
        let (outcome, asked) = run(two(), &script, &[]);
        assert_eq!(outcome, Agreement::Agreed {
            answer: "x",
            decision: "x".to_owned()
        });
        // a:2 is the same host as the failed a:1 and is not asked; b and c answer
        assert_eq!(asked, vec!["a:1", "b:1", "c:1"]);
    }

    #[test]
    fn agree_one_answering_is_undetermined() {
        let script: Script = vec![("a:1", Ok("x")), ("b:1", Err("down")), ("c:1", Err("down"))];
        let (outcome, _) = run(two(), &script, &[]);
        match outcome {
            Agreement::Undetermined(u) => {
                assert_eq!(u.reason, UndeterminedReason::TooFewAnswers);
                assert_eq!(u.per_node.len(), 3);
            },
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn agree_trusted_answering_and_silent() {
        let policy = SwapEvidencePolicy::Trusted { node: "b:1".to_owned() };
        let script: Script = vec![("a:1", Ok("none")), ("b:1", Ok("spent")), ("c:1", Ok("none"))];
        let (outcome, asked) = run(policy.clone(), &script, &[]);
        assert_eq!(outcome, Agreement::Agreed {
            answer: "spent",
            decision: "spent".to_owned()
        });
        assert_eq!(asked, vec!["b:1"]);

        let script: Script = vec![("a:1", Ok("none")), ("b:1", Err("down")), ("c:1", Ok("none"))];
        let (outcome, asked) = run(policy, &script, &[]);
        match outcome {
            Agreement::Undetermined(u) => assert_eq!(u.reason, UndeterminedReason::TrustedNodeSilent),
            other => panic!("{other:?}"),
        }
        // the trusted node is asked first; the others are asked only for a self-proving answer
        assert_eq!(asked.first().map(String::as_str), Some("b:1"));
    }

    #[test]
    fn agree_an_answer_that_proves_itself_is_taken_alone() {
        let script: Script = vec![("a:1", Ok("secret:s")), ("b:1", Err("down"))];
        let (outcome, _) = run(two(), &script, &["secret:s"]);
        assert_eq!(outcome, Agreement::Agreed {
            answer: "secret:s",
            decision: "secret".to_owned()
        });
    }

    #[test]
    fn agree_chain_times_both_past_the_lock_agree() {
        // The decision is "past the lock", not the time itself.
        let script: Script = vec![("a:1", Ok("past:1000")), ("b:1", Ok("past:1003"))];
        let (outcome, _) = run(two(), &script, &[]);
        assert_eq!(outcome, Agreement::Agreed {
            answer: "past:1000",
            decision: "past".to_owned()
        });
        let script: Script = vec![("a:1", Ok("past:1000")), ("b:1", Ok("before:990"))];
        let (outcome, _) = run(two(), &script, &[]);
        assert!(matches!(outcome, Agreement::Undetermined(u) if u.reason == UndeterminedReason::Disagree));
    }

    #[test]
    #[ignore = "release gate R100: must pass before tagging 0.5.0"]
    fn r100_release_gate_every_swap_family_adopted() {
        let open: Vec<_> = R100_ADOPTED
            .iter()
            .filter(|(_, adoption)| *adoption == R100Adoption::NotAdopted)
            .map(|(family, _)| *family)
            .collect();
        assert!(
            open.is_empty(),
            "families that swap and have not adopted R100: {open:?}"
        );
    }

    /// `SwapsOff` is tied to the real switches in both directions (CRD ch.51 R100 item 5): a family marked
    /// `SwapsOff` must have its swaps off, and a family whose swaps are on must not be `SwapsOff`, so a lane that
    /// switches swaps on without adopting R100 fails CI. The Solana entry is tied in mm2_main, where its untradable
    /// rule lives.
    #[test]
    fn swaps_off_entries_follow_the_real_switches() {
        for (family, adoption) in R100_ADOPTED {
            let swaps_on = match family {
                SwapFamily::Tendermint => crate::TENDERMINT_SWAPS_AVAILABLE,
                SwapFamily::Tron => crate::TRON_SWAPS_AVAILABLE,
                // Solana: mm2_main `r100_swaps_off_for_solana_follows_the_untradable_rule`.
                SwapFamily::Solana => continue,
                // every other family swaps in the release
                SwapFamily::Utxo
                | SwapFamily::Bch
                | SwapFamily::Slp
                | SwapFamily::Qtum
                | SwapFamily::Qrc20
                | SwapFamily::Evm
                | SwapFamily::Zcoin
                | SwapFamily::Sia => true,
            };
            if *adoption == R100Adoption::SwapsOff {
                assert!(!swaps_on, "a family marked SwapsOff has its swaps switched on");
            }
            if swaps_on {
                assert!(
                    *adoption != R100Adoption::SwapsOff,
                    "a family whose swaps are on is marked SwapsOff"
                );
            }
        }
    }

    #[test]
    fn adoption_table_gates_the_stored_policy() {
        let stored = SwapEvidencePolicy::Insufficient { hosts: 1 };
        for (family, adoption) in R100_ADOPTED {
            let effective = effective_policy(*family, &stored);
            let adopted = *adoption == R100Adoption::Adopted;
            assert_eq!(effective == stored, adopted, "{family:?}");
            assert_eq!(family_adopted(*family), adopted, "{family:?}");
            assert!(adopted || effective == SwapEvidencePolicy::NotAdopted);
        }
    }

    #[test]
    fn agree_not_adopted_takes_the_one_routed_node() {
        let script: Script = vec![("a:1", Ok("spent")), ("b:1", Ok("none"))];
        let (outcome, asked) = run(SwapEvidencePolicy::NotAdopted, &script, &[]);
        assert_eq!(outcome, Agreement::Agreed {
            answer: "spent",
            decision: "spent".to_owned()
        });
        assert_eq!(asked, vec!["a:1"]);
    }

    /// R100: a secret that proves itself is taken from any node when the read would otherwise be undetermined.
    #[test]
    fn agree_a_self_proving_answer_of_another_node_is_taken_when_the_read_is_undetermined() {
        let proves: &'static [&'static str] = &["secret:s"];
        // Insufficient policy, one node answers a self-proving answer
        let script: Script = vec![("a:1", Ok("none")), ("b:1", Ok("secret:s"))];
        let (outcome, _) = run(SwapEvidencePolicy::Insufficient { hosts: 1 }, &script, proves);
        assert_eq!(outcome, Agreement::Agreed {
            answer: "secret:s",
            decision: "secret".to_owned()
        });
        // Trusted node silent, another node proves
        let trusted = SwapEvidencePolicy::Trusted { node: "b:1".to_owned() };
        let script: Script = vec![("a:1", Ok("secret:s")), ("b:1", Err("down"))];
        let (outcome, _) = run(trusted.clone(), &script, proves);
        assert_eq!(outcome, Agreement::Agreed {
            answer: "secret:s",
            decision: "secret".to_owned()
        });
        // Trusted node silent, another node answers a plain answer: it is ignored
        let script: Script = vec![("a:1", Ok("none")), ("b:1", Err("down"))];
        let (outcome, _) = run(trusted, &script, proves);
        match outcome {
            Agreement::Undetermined(u) => {
                assert_eq!(u.reason, UndeterminedReason::TrustedNodeSilent);
                assert_eq!(u.per_node.len(), 1, "the plain answer of the other node is not used");
            },
            other => panic!("{other:?}"),
        }
        // Two nodes of a pair, one silent, a third proves
        let script: Script = vec![("a:1", Ok("none")), ("b:1", Err("down")), ("c:1", Ok("secret:s"))];
        let (outcome, _) = run(two(), &script, proves);
        assert!(matches!(outcome, Agreement::Agreed { .. }));
    }

    type TipScript = Vec<(&'static str, Result<(&'static str, u64), &'static str>)>;

    fn run_tips(
        script: &TipScript,
        max_gap: u64,
        proves: &'static [&'static str],
    ) -> Agreement<(&'static str, u64), String> {
        block_on(agree_with_tips(
            &two(),
            script,
            |(label, _)| (*label).to_owned(),
            |(_, result)| {
                let result = result.map_err(str::to_owned);
                async move { result }
            },
            |answer: &(&'static str, u64)| answer.0.to_owned(),
            |answer| proves.contains(&answer.0),
            |answer| Some(answer.1),
            max_gap,
        ))
    }

    /// A tip gap above the maximum is undetermined, within it the normal comparison runs, and an answer that proves
    /// itself wins whatever the gap.
    #[test]
    fn agree_with_tips_refuses_a_large_gap_and_lets_a_proof_win() {
        // block heights
        let script: TipScript = vec![("a:1", Ok(("none", 1000))), ("b:1", Ok(("none", 1010)))];
        assert!(matches!(run_tips(&script, 5, &[]),
            Agreement::Undetermined(u) if u.reason == UndeterminedReason::TipGap));
        assert!(matches!(run_tips(&script, 10, &[]), Agreement::Agreed { .. }));
        // different decisions within the gap: the normal comparison
        let script: TipScript = vec![("a:1", Ok(("none", 1000))), ("b:1", Ok(("spent", 1001)))];
        assert!(matches!(run_tips(&script, 5, &[]),
            Agreement::Undetermined(u) if u.reason == UndeterminedReason::Disagree));
        // timestamps in seconds, max gap 60
        let script: TipScript = vec![
            ("a:1", Ok(("none", 1_780_000_000))),
            ("b:1", Ok(("none", 1_780_000_045))),
        ];
        assert!(matches!(run_tips(&script, 60, &[]), Agreement::Agreed { .. }));
        let script: TipScript = vec![
            ("a:1", Ok(("none", 1_780_000_000))),
            ("b:1", Ok(("none", 1_780_000_300))),
        ];
        assert!(matches!(run_tips(&script, 60, &[]),
            Agreement::Undetermined(u) if u.reason == UndeterminedReason::TipGap));
        // one node shows a self-proving answer and the gap is huge: the proof is taken
        let script: TipScript = vec![("a:1", Ok(("secret:s", 5))), ("b:1", Ok(("none", 5_000_000)))];
        assert!(matches!(run_tips(&script, 5, &["secret:s"]), Agreement::Agreed {
            answer: ("secret:s", 5),
            ..
        }));
    }

    /// Different decisions with far-apart tips are a disagreement, equal decisions with far-apart tips are a tip gap,
    /// and a self-proving answer wins over both.
    #[test]
    fn agree_with_tips_compares_the_decisions_before_the_gap() {
        let far_apart_different: TipScript = vec![("a:1", Ok(("none", 1000))), ("b:1", Ok(("spent", 9000)))];
        assert!(matches!(run_tips(&far_apart_different, 5, &[]),
            Agreement::Undetermined(u) if u.reason == UndeterminedReason::Disagree));
        let far_apart_equal: TipScript = vec![("a:1", Ok(("none", 1000))), ("b:1", Ok(("none", 9000)))];
        assert!(matches!(run_tips(&far_apart_equal, 5, &[]),
            Agreement::Undetermined(u) if u.reason == UndeterminedReason::TipGap));
        let proof: TipScript = vec![("a:1", Ok(("secret:s", 1000))), ("b:1", Ok(("none", 9000)))];
        assert!(matches!(run_tips(&proof, 5, &["secret:s"]), Agreement::Agreed { .. }));
        let proof_vs_spent: TipScript = vec![("a:1", Ok(("spent", 1000))), ("b:1", Ok(("secret:s", 9000)))];
        assert!(matches!(
            run_tips(&proof_vs_spent, 5, &["secret:s"]),
            Agreement::Agreed { .. }
        ));
    }

    /// A bracketed IPv6 host is never split at a colon inside the brackets.
    #[test]
    fn ipv6_hosts_that_differ_in_the_last_group_stay_two_nodes() {
        let nodes = list(&["[fe80::1:2]:8080", "[fe80::1:3]:8080"]);
        assert_eq!(node_host(&nodes[0]).as_deref(), Some("[fe80::1:2]"));
        assert!(matches!(
            swap_evidence_policy_from("T", None, &nodes).unwrap(),
            SwapEvidencePolicy::TwoNodes { hosts } if hosts.len() == 2
        ));
        assert!(node_matches("[fe80::1:2]:8080", &nodes[0]));
        assert!(!node_matches("[fe80::1:2]:9090", &nodes[0]));
        assert!(node_matches("[fe80::1:2]", &nodes[0]));
        assert!(!node_matches("[fe80::1:2]", &nodes[1]));
        assert_eq!(node_host("[fe80::1:2]").as_deref(), Some("[fe80::1:2]"));
        assert_eq!(node_name("https://[FE80::1:2]:8545/x"), "[fe80::1:2]:8545");
    }

    #[test]
    fn agree_own_daemon_and_insufficient() {
        let script: Script = vec![("d:1", Ok("spent"))];
        let (outcome, _) = run(SwapEvidencePolicy::OwnDaemon, &script, &[]);
        assert_eq!(outcome, Agreement::Agreed {
            answer: "spent",
            decision: "spent".to_owned()
        });
        let script: Script = vec![("d:1", Err("down"))];
        let (outcome, _) = run(SwapEvidencePolicy::OwnDaemon, &script, &[]);
        assert!(matches!(outcome, Agreement::Undetermined(_)));
        let script: Script = vec![("a:1", Ok("x")), ("b:1", Ok("x"))];
        let (outcome, asked) = run(SwapEvidencePolicy::Insufficient { hosts: 1 }, &script, &[]);
        assert!(matches!(outcome, Agreement::Undetermined(u) if u.reason == UndeterminedReason::TooFewAnswers));
        // plain answers are ignored: the read stays undetermined whoever answers
        assert_eq!(asked.len(), 2);
    }
}
