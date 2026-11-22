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

/// The one adoption table of R100 (decision LFV b): `true` once the family's reads agree on two nodes and its
/// `swap_evidence_policy()` answers its stored policy; until then it answers [`SwapEvidencePolicy::NotAdopted`].
/// Every entry must be `true` before 0.5.0 is tagged (`r100_release_gate_every_swap_family_adopted`).
pub const R100_ADOPTED: &[(SwapFamily, bool)] = &[
    (SwapFamily::Utxo, false),
    (SwapFamily::Bch, false),
    (SwapFamily::Slp, false),
    (SwapFamily::Qtum, false),
    (SwapFamily::Qrc20, false),
    (SwapFamily::Evm, false),
    (SwapFamily::Zcoin, false),
    (SwapFamily::Sia, false),
    (SwapFamily::Tendermint, false),
    (SwapFamily::Tron, true),
    (SwapFamily::Solana, false),
];

/// Whether `family` has adopted R100.
pub fn family_adopted(family: SwapFamily) -> bool {
    R100_ADOPTED
        .iter()
        .any(|(candidate, adopted)| *candidate == family && *adopted)
}

/// What a coin of `family` answers with: its `stored` policy once the family has adopted R100, else
/// [`SwapEvidencePolicy::NotAdopted`].
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

/// Host and optional port of a node address: an Electrum `host:port`, an `https://...` / `wss://...` URL or a bare
/// host. The host is lowercased; an IPv6 host keeps its brackets. The port is the one written, else the default
/// of an `http`, `https`, `ws` or `wss` scheme.
fn host_and_port(addr: &str) -> Option<(String, Option<u16>)> {
    let name = electrum_host_port(addr.trim())?;
    let (host, port) = match name.rsplit_once(':') {
        Some((host, port)) if !host.ends_with(':') => (host, port.parse::<u16>().ok()),
        _ => (name.as_str(), None),
    };
    let port = port.or_else(|| match addr.trim().split_once("://").map(|(scheme, _)| scheme) {
        Some("https") | Some("wss") => Some(443),
        Some("http") | Some("ws") => Some(80),
        _ => None,
    });
    Some((host.to_ascii_lowercase(), port))
}

/// The form in which an error or a log line names a node: `host[:port]`, lowercased, the port only when the address
/// writes one. A scheme, userinfo, path and query (which may hold a credential) are dropped; an address with no host
/// is rendered as [`crate::node_url::UNNAMED_NODE`].
pub fn node_name(addr: &str) -> String {
    electrum_host_port(addr.trim()).map_or_else(
        || crate::node_url::UNNAMED_NODE.to_owned(),
        |name| name.to_ascii_lowercase(),
    )
}

/// The host of one node address, lowercased (CRD ch.51 R100 item 1: a node is a distinct host name). `None` when
/// the address names no host.
pub fn node_host(url: &str) -> Option<String> { host_and_port(url).map(|(host, _)| host) }

/// Whether the `swap_evidence_node` value `field` names the list entry `entry`: the same host, and the same port
/// when the field names one (explicitly, or by an `http(s)`/`ws(s)` scheme); the scheme itself is ignored.
pub fn node_matches(field: &str, entry: &str) -> bool {
    let (Some((field_host, field_port)), Some((entry_host, entry_port))) = (host_and_port(field), host_and_port(entry))
    else {
        return false;
    };
    field_host == entry_host && (field_port.is_none() || field_port == entry_port)
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
}

/// A read that cannot be decided: the step waits and must not refund or claim on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Undetermined {
    pub reason: UndeterminedReason,
    /// For each node asked, its label and a short summary of its answer or its error, for the log line.
    pub per_node: Vec<(String, String)>,
}

impl fmt::Display for Undetermined {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = match self.reason {
            UndeterminedReason::Disagree => "the nodes disagree",
            UndeterminedReason::TooFewAnswers => "fewer than two nodes answered",
            UndeterminedReason::TrustedNodeSilent => "the trusted node did not answer",
        };
        write!(f, "swap evidence undetermined: {reason}")?;
        for (index, (label, summary)) in self.per_node.iter().enumerate() {
            write!(f, "{} {label}: {summary}", if index == 0 { " (" } else { "; " })?;
        }
        if !self.per_node.is_empty() {
            f.write_str(")")?;
        }
        Ok(())
    }
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
/// the trusted node and it appears in the log line of an undetermined read.
///
/// Each call asks one pair and decides only when that pair agrees: when two answers differ, no third node is asked.
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
    let labels: Vec<String> = nodes.iter().map(&label).collect();
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
            return finish(UndeterminedReason::TooFewAnswers, per_node);
        },
        // A family that has not adopted R100 reads as today: one routed node, its answer taken.
        SwapEvidencePolicy::OwnDaemon | SwapEvidencePolicy::NotAdopted => {
            ((0..nodes.len()).take(1).collect(), 1, UndeterminedReason::TooFewAnswers)
        },
        SwapEvidencePolicy::Trusted { node } => (
            (0..nodes.len())
                .filter(|index| node_matches(node, &labels[*index]))
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

    let classify = &classify;
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
    let mut answers: Vec<(usize, A, K)> = Vec::new();
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
                answers.push((index, answer, decision));
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
        return finish(silent_reason, per_node);
    }
    answers.sort_by_key(|(index, _, _)| *index);
    let (_, answer, decision) = answers.remove(0);
    if answers.iter().all(|(_, _, other)| *other == decision) {
        Agreement::Agreed { answer, decision }
    } else {
        finish(UndeterminedReason::Disagree, per_node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use std::cell::RefCell;

    fn list(entries: &[&str]) -> Vec<String> { entries.iter().map(|entry| (*entry).to_owned()).collect() }

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
        assert_eq!(node_name("HTTPS://Node.Example.com/v3/KEY"), "node.example.com");
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
        assert_eq!(asked, vec!["b:1"]);
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
    #[ignore = "release gate R100 (LFV b): must pass before tagging 0.5.0"]
    fn r100_release_gate_every_swap_family_adopted() {
        let open: Vec<_> = R100_ADOPTED
            .iter()
            .filter(|(_, adopted)| !adopted)
            .map(|(family, _)| *family)
            .collect();
        assert!(open.is_empty(), "families that have not adopted R100: {open:?}");
    }

    #[test]
    fn adoption_table_gates_the_stored_policy() {
        let stored = SwapEvidencePolicy::Insufficient { hosts: 1 };
        for (family, adopted) in R100_ADOPTED {
            let effective = effective_policy(*family, &stored);
            assert_eq!(effective == stored, *adopted, "{family:?}");
            assert!(*adopted || effective == SwapEvidencePolicy::NotAdopted);
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
        assert!(asked.is_empty());
    }
}
