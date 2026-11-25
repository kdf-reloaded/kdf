//! The QRC20 swap reads under the swap-evidence rule (CRD ch.51 §51.6G R100).
//!
//! # Purpose
//!
//! A refund or a claim of a QRC20 swap rests on three chain reads: the counterparty's payment state, whether a
//! payment is claimed, refunded or not spent, and the chain time of the refund gate. Over Electrum, with an adopted
//! policy, each of them is asked of single servers, addressed on their own, and decided by
//! [`crate::swap_evidence::agree_with_tips`]: two servers on distinct hosts must reach the same decision, or the one
//! server the operator trusts answers alone. A coin read through the operator's own daemon reads as before.
//!
//! # Public exports
//!
//! - `Qrc20Coin::agreed_swap_reads`: the policy and the client the reads are agreed under, `None` while the coin
//!   reads as before.
//! - `Qrc20Coin::agreed_payment_read`, `agreed_payment_of_tx`, `agreed_confirmation`, `agreed_payment_sent`: the
//!   payment-state reads.
//! - `Qrc20Coin::agreed_swap_tx_spend`, `wait_for_agreed_payment_spend`: the spend reads.
//! - `Qrc20Coin::evidence_policy`, `evidence_refusal`, `start_capability_probe`: the admission rule of the
//!   contract-event history.
//!
//! # Invariants
//!
//! - A directed read asks one server and never fails over.
//! - Every read asks each server at its own tip, and every answer carries that tip's height. Two answers whose tips
//!   are further apart than the pool's tolerance leave the attempt undetermined.
//! - The spend read is decided by the contract's state of the payment. The event history is read only on a server
//!   whose state is claimed or refunded, to name the transaction.
//! - A claim is taken only when its call carries a secret whose hash is the payment's secret hash and is bound to its
//!   own receipt; such a claim proves itself and is taken from one server.
//! - One attempt takes one pair of answers. Two answers that differ leave the attempt undetermined; no further
//!   server is asked to outvote them.
//! - An undetermined read never yields "valid", "not sent", "not spent" or an open refund gate.
//! - A server is named by host and port only, in every log line and error text.
//! - The text of an undetermined read is built in one place, [`undetermined_text`].

use super::rpc_clients::directed_error_text;
use super::swap::{bound_swap_event_output, find_swap_contract_call_with_swap_id, holds_swap_call, is_swap_event_log,
                  swap_calls_completed, Erc20PaymentDetails, ExpectedSwapEvent};
use super::*;
use crate::swap_evidence::{agree_with_tips, distinct_hosts, effective_policy, insufficient_refusal, node_name,
                           Agreement, SwapFamily, Undetermined, UndeterminedReason};
use crate::SwapEvidencePolicy;
use common::log::{debug, info};
use std::fmt;
use std::future::Future as StdFuture;

/// Seconds between two attempts of a read that waits while it is undetermined.
const READ_RETRY_SECS: f64 = 10.;

/// The least seconds between two probes of one server for the contract-event history.
const CAPABILITY_PROBE_GAP_SECS: f64 = 60.;

/// How many undetermined reads are remembered as already reported at WARN.
const REPORTED_CAP: usize = 256;

/// The undetermined reads already reported at WARN, oldest first.
static REPORTED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// `true` the first time `key` is seen.
fn first_report(key: String) -> bool {
    let mut reported = REPORTED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if reported.contains(&key) {
        return false;
    }
    if reported.len() >= REPORTED_CAP {
        reported.remove(0);
    }
    reported.push(key);
    true
}

/// The one place that builds the text of an undetermined QRC20 swap read. It starts with
/// `"swap evidence undetermined: "`, the marker the swap engine matches on, and names servers by `host:port` only.
pub(super) fn undetermined_text(undetermined: &Undetermined) -> String { undetermined.to_err_string() }

/// The two texts of an undetermined read of `subject`: the warning line and the error text. Both start with the
/// marker and the reason and end with which read waits and the two ways to fix it. The error text also says what
/// each server answered; the warning line names no server and no answer.
pub(super) fn undetermined_texts(
    ticker: &str,
    read: &str,
    subject: &str,
    undetermined: &Undetermined,
) -> (String, String) {
    let kind = Undetermined {
        reason: undetermined.reason,
        per_node: Vec::new(),
    };
    let tail = format!(
        "[{}: the {} of {} waits; to fix it, add an Electrum server on another host that serves the contract reads to \
         the server list, or set swap_evidence_node]",
        ticker, read, subject
    );
    (
        format!("{} {}", undetermined_text(&kind), tail),
        format!("{} {}", undetermined_text(undetermined), tail),
    )
}

/// A read that no server was asked for, because nothing could decide it.
fn too_few_answers<A, K>() -> Agreement<A, K> {
    Agreement::Undetermined(Undetermined {
        reason: UndeterminedReason::TooFewAnswers,
        per_node: Vec::new(),
    })
}

/// What a server shows at its own tip.
pub(super) struct AtTip<T> {
    height: u64,
    pub(super) read: T,
}

/// A decision with the height it was read at. Two of them are equal when the decisions are; the height is only
/// shown, in the text of an undetermined read.
pub(super) struct AtHeight<D> {
    decision: D,
    height: u64,
}

impl<D: PartialEq> PartialEq for AtHeight<D> {
    fn eq(&self, other: &Self) -> bool { self.decision == other.decision }
}

impl<D: fmt::Debug> fmt::Debug for AtHeight<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} at block {}", self.decision, self.height)
    }
}

/// The payment as one server shows it: the contract's state of the payment, and the `erc20Payment` call details
/// that the server's receipt of the payment transaction gives.
pub(super) struct PaymentRead {
    pub(super) state: U256,
    pub(super) details: Result<Erc20PaymentDetails, String>,
}

/// What two payment reads are compared on.
#[derive(PartialEq)]
pub(super) struct PaymentDecision {
    state: U256,
    details: Result<Erc20PaymentDetails, String>,
}

impl fmt::Debug for PaymentDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "payment state {}, ", self.state)?;
        match &self.details {
            Ok(details) => write!(f, "payment call of {:#02x} read", details.sender),
            Err(_) => f.write_str("payment call not readable"),
        }
    }
}

fn payment_decision(read: &PaymentRead) -> PaymentDecision {
    PaymentDecision {
        state: read.state,
        details: read.details.clone(),
    }
}

/// Whether a payment is claimed, refunded or not spent, as one server shows it.
pub(super) enum SpendRead {
    /// The contract holds the payment as sent.
    NotSpent,
    /// The receiver claimed the payment in this transaction, with a secret whose hash is the payment's secret hash.
    Claimed(UtxoTx),
    /// The sender refunded the payment in this transaction.
    Refunded(UtxoTx),
}

/// What two spend reads are compared on.
#[derive(Debug, PartialEq)]
pub(super) enum SpendDecision {
    NotSpent,
    ClaimedBy(H256Json),
    RefundedBy(H256Json),
}

fn tx_hash_of(tx: &UtxoTx) -> H256Json { tx.hash().reversed().into() }

fn spend_decision(read: &SpendRead) -> SpendDecision {
    match read {
        SpendRead::NotSpent => SpendDecision::NotSpent,
        SpendRead::Claimed(tx) => SpendDecision::ClaimedBy(tx_hash_of(tx)),
        SpendRead::Refunded(tx) => SpendDecision::RefundedBy(tx_hash_of(tx)),
    }
}

/// Whether this node's payment was sent, as one server shows it.
pub(super) enum SentRead {
    NotSent,
    Sent(UtxoTx),
    /// The contract knows the payment, but the server's event history holds no payment transaction of the swap.
    StateWithoutTransaction,
}

/// What two already-sent reads are compared on.
#[derive(Debug, PartialEq)]
pub(super) enum SentDecision {
    NotSent,
    SentBy(H256Json),
    StateWithoutTransaction,
}

fn sent_decision(read: &SentRead) -> SentDecision {
    match read {
        SentRead::NotSent => SentDecision::NotSent,
        SentRead::Sent(tx) => SentDecision::SentBy(tx_hash_of(tx)),
        SentRead::StateWithoutTransaction => SentDecision::StateWithoutTransaction,
    }
}

/// The decision of a confirmation wait as one server shows it: whether the transaction has the required
/// confirmations (not their number) and, when it has, whether its swap contract calls completed.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ConfirmRead {
    enough: bool,
    completed: Result<(), String>,
}

/// The servers the capability rule of the contract-event history is about.
enum CapabilityScope {
    /// Two distinct hosts of the client must have served the read.
    TwoHosts,
    /// The trusted server, by its `host:port`, must have served the read.
    Trusted(String),
}

/// Marks the probe of one server as ended when the probe ends or is dropped.
struct ProbeGuard {
    client: ElectrumClient,
    server: String,
}

impl Drop for ProbeGuard {
    fn drop(&mut self) { self.client.end_contract_history_probe(&self.server); }
}

impl Qrc20Coin {
    /// What the capability rule of the contract-event history asks of this token: `None` when it asks nothing (the
    /// family has not adopted the rule, the operator's own daemon, or a coin with too few hosts anyway).
    fn capability_scope(&self) -> Option<(&ElectrumClient, CapabilityScope)> {
        let UtxoRpcClientEnum::Electrum(client) = &self.utxo.rpc_client else {
            return None;
        };
        match effective_policy(SwapFamily::Qrc20, &self.utxo.swap_evidence) {
            SwapEvidencePolicy::TwoNodes { .. } => Some((client, CapabilityScope::TwoHosts)),
            SwapEvidencePolicy::Trusted { node } => Some((client, CapabilityScope::Trusted(node_name(&node)))),
            _ => None,
        }
    }

    /// The policy this token's swap reads are judged by (CRD ch.51 R100), with the capability rule of the
    /// contract-event history (`blockchain.contract.event.get_history`): a token over two or more Electrum hosts is
    /// `Insufficient` until two distinct hosts of its client have served the read in this session, and a token with
    /// a trusted server until that server has. Only the operator's own daemon needs no such proof. While the proof is
    /// missing, a bounded probe of the servers concerned is started.
    pub(super) fn evidence_policy(&self) -> SwapEvidencePolicy {
        let stored = effective_policy(SwapFamily::Qrc20, &self.utxo.swap_evidence);
        let Some((client, scope)) = self.capability_scope() else {
            return stored;
        };
        let served = client.contract_history_servers().0;
        let hosts = match &scope {
            CapabilityScope::TwoHosts => {
                let hosts = distinct_hosts(&served).len();
                if hosts >= 2 {
                    return stored;
                }
                hosts
            },
            // The trusted server has not served the read: nothing can decide a swap read, as with no host at all.
            CapabilityScope::Trusted(trusted) => {
                if served.iter().any(|label| node_name(label) == *trusted) {
                    return stored;
                }
                0
            },
        };
        self.start_capability_probe();
        SwapEvidencePolicy::Insufficient { hosts }
    }

    /// The refusal text of an order or a swap start with this token, `None` when it may trade (CRD ch.51 R100 item
    /// 4). A token that is refused for the capability rule is told which hosts have served the contract-event history
    /// and which have not, or that its trusted server has not.
    pub(super) fn evidence_refusal(&self) -> Option<String> {
        let SwapEvidencePolicy::Insufficient { .. } = self.evidence_policy() else {
            return None;
        };
        let Some((client, scope)) = self.capability_scope() else {
            return Some(insufficient_refusal(&self.utxo.conf.ticker));
        };
        if let CapabilityScope::Trusted(trusted) = scope {
            return Some(format!(
                "{}: the trusted Electrum server {} of {} has not served blockchain.contract.event.get_history in \
                 this session; QRC20 swaps need the server named by swap_evidence_node to serve it: add a server \
                 that serves it to the server list, or name one of the list that does",
                self.utxo.conf.ticker, trusted, self.platform
            ));
        }
        let (served, not_served) = client.contract_history_servers();
        let list = |labels: Vec<String>| match labels.is_empty() {
            true => "none".to_owned(),
            false => labels
                .iter()
                .map(|label| node_name(label))
                .collect::<Vec<_>>()
                .join(", "),
        };
        Some(format!(
            "{}: fewer than two Electrum hosts of {} have served blockchain.contract.event.get_history in this \
             session (served: {}; not served: {}); QRC20 swaps need two such hosts: add one to the server list or \
             set swap_evidence_node",
            self.utxo.conf.ticker,
            self.platform,
            list(served),
            list(not_served)
        ))
    }

    /// Starts a probe of the contract-event history on the servers of this token's client that have not served it:
    /// one request per server, the token's own transfer history. A server is probed at most once a minute and never
    /// while a probe of it runs (the client keeps that, so it holds for every token that shares the client). The
    /// probe runs for a token over two or more hosts, and for a token with a trusted server on that server only. It
    /// ends when the token is disabled or dropped, and holds the client no longer than the token does. An answer is recorded by the client itself; one INFO line names the
    /// hosts by `host:port`.
    pub(crate) fn start_capability_probe(&self) {
        let Some((client, scope)) = self.capability_scope() else {
            return;
        };
        let only = match &scope {
            CapabilityScope::TwoHosts => None,
            CapabilityScope::Trusted(trusted) => Some(trusted.as_str()),
        };
        if self.capability_probes.ended() {
            return;
        }
        let Ok(my_address) = self.utxo.derivation_method.iguana_or_err() else {
            return;
        };
        let Ok(my_address) = qtum::contract_addr_from_utxo_addr(my_address.clone()) else {
            return;
        };
        let guards: Vec<ProbeGuard> = client
            .claim_contract_history_probes(CAPABILITY_PROBE_GAP_SECS, only)
            .into_iter()
            .map(|server| ProbeGuard {
                client: client.clone(),
                server,
            })
            .collect();
        if guards.is_empty() {
            return;
        }
        let client = client.clone();
        let platform = self.platform.clone();
        let address = contract_addr_into_rpc_format(&my_address);
        let token = contract_addr_into_rpc_format(&self.contract_address);
        let probe = async move {
            let requests = guards.iter().map(|guard| {
                client
                    .blockchain_contract_event_get_history_from(&guard.server, &address, &token, QRC20_TRANSFER_TOPIC)
                    .compat()
            });
            // The answers are recorded by the client where every answer of the method is.
            let _answers = futures::future::join_all(requests).await;
            let (served, not_served) = client.contract_history_servers();
            let names = |labels: &[String]| labels.iter().map(|label| node_name(label)).collect::<Vec<_>>();
            info!(
                "{}: Electrum hosts that have served blockchain.contract.event.get_history: {:?}; not served: {:?}",
                platform,
                names(&served),
                names(&not_served)
            );
        };
        let (probe, handle) = futures::future::abortable(probe);
        // A token disabled meanwhile starts nothing; the guards then release the servers.
        if self.capability_probes.add(handle) {
            common::executor::spawn(probe.map(|_| ()));
        }
    }

    /// The policy and the Electrum client this token's swap reads are agreed under (CRD ch.51 R100), or `None` when
    /// they are read as before: the family has not adopted the rule, or the coin is served by the operator's own
    /// daemon.
    pub(super) fn agreed_swap_reads(&self) -> Option<(SwapEvidencePolicy, ElectrumClient)> {
        let UtxoRpcClientEnum::Electrum(client) = &self.utxo.rpc_client else {
            return None;
        };
        match self.evidence_policy() {
            SwapEvidencePolicy::OwnDaemon | SwapEvidencePolicy::NotAdopted => None,
            policy => Some((policy, client.clone())),
        }
    }

    /// The servers a read under `policy` may ask: the client's usable servers, in its order. Under a trusted server
    /// and without `with_rest` only that server: the others could only be asked for an answer that proves itself,
    /// which such a read never has.
    fn evidence_servers(client: &ElectrumClient, policy: &SwapEvidencePolicy, with_rest: bool) -> Vec<String> {
        let servers = client.server_addrs();
        match policy {
            SwapEvidencePolicy::Trusted { node } if !with_rest => {
                let trusted = node_name(node);
                servers
                    .into_iter()
                    .filter(|server| node_name(server) == trusted)
                    .collect()
            },
            _ => servers,
        }
    }

    /// One attempt of a read that each server answers at its own tip: the server's tip is read first, then `read`
    /// runs on that server with the tip's height. The answers are compared by [`agree_with_tips`] with the heights of the tips, so two
    /// servers further apart than the pool's tolerance leave the attempt undetermined, and an answer that proves
    /// itself is taken from any one server.
    ///
    /// Without `with_rest` the read has no answer that proves itself: under an `Insufficient` policy no server is
    /// asked.
    async fn agree_at_own_tips<T, D, R, Fut>(
        client: &ElectrumClient,
        policy: &SwapEvidencePolicy,
        with_rest: bool,
        read: R,
        classify: impl Fn(&T) -> D,
        proves_itself: impl Fn(&T) -> bool,
    ) -> Agreement<AtTip<T>, AtHeight<D>>
    where
        D: PartialEq + fmt::Debug,
        R: Fn(String, u64) -> Fut,
        Fut: StdFuture<Output = Result<T, String>>,
    {
        if !with_rest && matches!(policy, SwapEvidencePolicy::Insufficient { .. }) {
            return too_few_answers();
        }
        let servers = Self::evidence_servers(client, policy, with_rest);
        let read = &read;
        agree_with_tips(
            policy,
            &servers,
            |server| node_name(server),
            |server| {
                let server = server.clone();
                async move {
                    let tip = client
                        .blockchain_headers_subscribe_from(&server)
                        .compat()
                        .await
                        .map_err(directed_error_text)?;
                    let height = tip.block_height();
                    let read = read(server, height).await?;
                    Ok(AtTip { height, read })
                }
            },
            |answer| AtHeight {
                decision: classify(&answer.read),
                height: answer.height,
            },
            |answer| proves_itself(&answer.read),
            |answer| Some(answer.height),
            client.tolerance_blocks(),
        )
        .await
    }

    /// The error text of an undetermined read of `subject`: the marker and what each server said
    /// ([`undetermined_text`]), then which read waits and the two ways to fix it (CRD ch.51 R100 item 9).
    ///
    /// The text is logged at DEBUG every time. The first time a read of `subject` is undetermined, one WARN line is
    /// written as well ([`undetermined_texts`]): it names the kind only and no server, since a refund wait's warning
    /// carries no node identity (CRD ch.51 R80, R81).
    fn report_undetermined(&self, read: &str, subject: &str, undetermined: &Undetermined) -> String {
        let ticker = &self.utxo.conf.ticker;
        let (warning, text) = undetermined_texts(ticker, read, subject, undetermined);
        if first_report(format!("{}:{}:{}", ticker, read, subject)) {
            warn!("{}", warning);
        }
        debug!("{}", text);
        text
    }

    /// The payment read of one server: its receipt of `payment_tx`, and its state of the payment `swap_id`. Without
    /// `swap_id` the id of the payment call is taken, and a receipt that gives no payment call is an error.
    async fn payment_read_on(
        &self,
        client: &ElectrumClient,
        server: &str,
        payment_tx: &UtxoTx,
        swap_contract: &H160,
        swap_id: Option<&[u8]>,
    ) -> Result<PaymentRead, String> {
        let receipts = client
            .blockchain_transaction_get_receipt_from(server, &tx_hash_of(payment_tx))
            .compat()
            .await
            .map_err(directed_error_text)?;
        let details = self.erc20_payment_details_from_receipts(payment_tx, receipts);
        let swap_id = match (swap_id, &details) {
            (Some(swap_id), _) => swap_id.to_vec(),
            (None, Ok(details)) => details.swap_id.clone(),
            (None, Err(e)) => return Err(e.clone()),
        };
        let state = client.payment_state_from(server, swap_contract, &swap_id).await?;
        Ok(PaymentRead { state, details })
    }

    /// One attempt of the payment read.
    async fn payment_read_once(
        &self,
        policy: &SwapEvidencePolicy,
        client: &ElectrumClient,
        payment_tx: &UtxoTx,
        swap_contract: &H160,
        swap_id: Option<&[u8]>,
    ) -> Agreement<AtTip<PaymentRead>, AtHeight<PaymentDecision>> {
        Self::agree_at_own_tips(
            client,
            policy,
            false,
            |server: String, _tip: u64| async move {
                self.payment_read_on(client, &server, payment_tx, swap_contract, swap_id)
                    .await
            },
            payment_decision,
            |_| false,
        )
        .await
    }

    /// The counterparty's payment under `policy` (CRD ch.51 R100): the contract's state of the payment `swap_id` and
    /// the payment call details, as two servers agree on them, each at its own tip, or as the trusted server shows
    /// them.
    ///
    /// The read is repeated until `wait_until` (UNIX seconds) while it is undetermined, and while the servers agree
    /// that the contract does not know the payment yet. It is made at least once, and no attempt starts at or after
    /// `wait_until`.
    ///
    /// # Errors
    ///
    /// The read is still undetermined at `wait_until`. The text names what each server said.
    pub(super) async fn agreed_payment_read(
        &self,
        policy: &SwapEvidencePolicy,
        client: &ElectrumClient,
        payment_tx: &UtxoTx,
        swap_contract: &H160,
        swap_id: &[u8],
        wait_until: u64,
    ) -> Result<PaymentRead, String> {
        loop {
            let attempt = self
                .payment_read_once(policy, client, payment_tx, swap_contract, Some(swap_id))
                .await;
            // What the read answers when the deadline has passed.
            let at_deadline = match attempt {
                Agreement::Agreed { answer, .. } => {
                    if answer.read.state != eth::PAYMENT_STATE_UNINITIALIZED.into() {
                        return Ok(answer.read);
                    }
                    Ok(answer.read)
                },
                Agreement::Undetermined(undetermined) => {
                    let subject = format!("payment {}", hex::encode(swap_id));
                    Err(self.report_undetermined("payment state", &subject, &undetermined))
                },
            };
            // The wait never runs past the deadline: it is cut to the time that is left, and the deadline is
            // checked again after it, so no attempt starts at or after `wait_until`.
            let remaining = wait_until.saturating_sub(now_ms() / 1000);
            if remaining == 0 {
                return at_deadline;
            }
            Timer::sleep(READ_RETRY_SECS.min(remaining as f64)).await;
            if now_ms() / 1000 >= wait_until {
                return at_deadline;
            }
        }
    }

    /// The payment call details of `payment_tx` and the contract's state of that payment under `policy`, in one
    /// attempt: what a claim that reveals a secret, and a refund, check before they are sent.
    ///
    /// # Errors
    ///
    /// The read is undetermined, or the servers agree that the receipt gives no payment call.
    pub(super) async fn agreed_payment_of_tx(
        &self,
        policy: &SwapEvidencePolicy,
        client: &ElectrumClient,
        payment_tx: &UtxoTx,
        swap_contract: &H160,
    ) -> Result<(Erc20PaymentDetails, U256), String> {
        match self
            .payment_read_once(policy, client, payment_tx, swap_contract, None)
            .await
        {
            Agreement::Agreed { answer, .. } => Ok((answer.read.details?, answer.read.state)),
            Agreement::Undetermined(undetermined) => {
                let subject = format!("payment transaction {:?}", tx_hash_of(payment_tx));
                Err(self.report_undetermined("payment state", &subject, &undetermined))
            },
        }
    }

    /// The confirmation read of one server: whether `qtum_tx` has `confirmations` there and, when it has, whether
    /// the server's receipt shows its swap contract calls as completed.
    async fn confirm_read_on(
        client: &ElectrumClient,
        server: &str,
        qtum_tx: &UtxoTx,
        confirmations: u64,
        requires_nota: bool,
    ) -> Result<ConfirmRead, String> {
        let tx_hash = tx_hash_of(qtum_tx);
        let verbose = client
            .transaction_get_verbose_from(server, &tx_hash)
            .compat()
            .await
            .map_err(directed_error_text)?;
        let seen = match requires_nota {
            true => verbose.confirmations,
            false => verbose.rawconfirmations.unwrap_or(verbose.confirmations),
        };
        if u64::from(seen) < confirmations {
            return Ok(ConfirmRead {
                enough: false,
                completed: Ok(()),
            });
        }
        let receipts = client
            .blockchain_transaction_get_receipt_from(server, &tx_hash)
            .compat()
            .await
            .map_err(directed_error_text)?;
        Ok(ConfirmRead {
            enough: true,
            completed: swap_calls_completed(qtum_tx, &receipts),
        })
    }

    /// The decision of a confirmation wait under `policy` (CRD ch.51 R100), made after the wait on one server ended:
    /// `qtum_tx` is confirmed when two servers (or the trusted one) show it with `confirmations`, each at its own tip,
    /// and agree on whether its swap contract calls completed. Every `check_every` seconds one attempt is made until
    /// `wait_until` (UNIX seconds); at least one is made.
    ///
    /// # Errors
    ///
    /// The servers agree that a swap contract call failed; or at `wait_until` the read is undetermined or the servers
    /// agree that the confirmations are not there.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn agreed_confirmation(
        &self,
        policy: &SwapEvidencePolicy,
        client: &ElectrumClient,
        qtum_tx: &UtxoTx,
        confirmations: u64,
        requires_nota: bool,
        wait_until: u64,
        check_every: u64,
    ) -> Result<(), String> {
        let subject = format!("transaction {:?}", tx_hash_of(qtum_tx));
        loop {
            let attempt = Self::agree_at_own_tips(
                client,
                policy,
                false,
                |server: String, _tip: u64| async move {
                    Self::confirm_read_on(client, &server, qtum_tx, confirmations, requires_nota).await
                },
                ConfirmRead::clone,
                |_| false,
            )
            .await;
            let at_deadline = match attempt {
                Agreement::Agreed { answer, .. } if answer.read.enough => return answer.read.completed,
                Agreement::Agreed { .. } => ERR!(
                    "Waited too long until {} for {} to be confirmed {} times on two servers",
                    wait_until,
                    subject,
                    confirmations
                ),
                Agreement::Undetermined(undetermined) => {
                    Err(self.report_undetermined("confirmation", &subject, &undetermined))
                },
            };
            let remaining = wait_until.saturating_sub(now_ms() / 1000);
            if remaining == 0 {
                return at_deadline;
            }
            Timer::sleep((check_every.max(1) as f64).min(remaining as f64)).await;
            if now_ms() / 1000 >= wait_until {
                return at_deadline;
            }
        }
    }

    /// The transactions of `server`'s transfer history of `caller` from `from_block` whose receipts hold a swap event
    /// of `event_topic`, each with the receipts `server` returned for it.
    async fn swap_event_candidates_from(
        &self,
        client: &ElectrumClient,
        server: &str,
        event_topic: &str,
        caller: H160,
        from_block: u64,
    ) -> Result<Vec<(UtxoTx, Vec<TxReceipt>)>, String> {
        let address = contract_addr_into_rpc_format(&caller);
        let token = contract_addr_into_rpc_format(&self.contract_address);
        let history = client
            .blockchain_contract_event_get_history_from(server, &address, &token, QRC20_TRANSFER_TOPIC)
            .compat()
            .await
            .map_err(directed_error_text)?;
        let mut asked: Vec<H256Json> = Vec::new();
        let mut candidates = Vec::new();
        for item in history {
            if item.height < from_block || asked.contains(&item.tx_hash) {
                continue;
            }
            asked.push(item.tx_hash);
            let receipts = client
                .blockchain_transaction_get_receipt_from(server, &item.tx_hash)
                .compat()
                .await
                .map_err(directed_error_text)?;
            let emitted = receipts
                .iter()
                .any(|receipt| receipt.log.iter().any(|log| is_swap_event_log(event_topic, log)));
            if !emitted {
                continue;
            }
            let verbose = client
                .transaction_get_verbose_from(server, &item.tx_hash)
                .compat()
                .await
                .map_err(directed_error_text)?;
            let tx: UtxoTx = deserialize(verbose.hex.as_slice()).map_err(|e| format!("{:?}", e))?;
            candidates.push((tx, receipts));
        }
        Ok(candidates)
    }

    /// The spend read of one server at its tip. The contract's state decides; for a claimed or a refunded payment
    /// the server's own event history names the transaction, bound to its receipt as the single-server search binds
    /// it (CRD ch.51 §51.6E R89).
    async fn spend_read_on(
        &self,
        client: &ElectrumClient,
        server: &str,
        payment_tx: &UtxoTx,
        expected_swap_id: Option<&[u8]>,
        secret_hash: Option<&[u8]>,
        from_block: u64,
    ) -> Result<SpendRead, String> {
        let receipts = client
            .blockchain_transaction_get_receipt_from(server, &tx_hash_of(payment_tx))
            .compat()
            .await
            .map_err(directed_error_text)?;
        let details = self.erc20_payment_details_from_receipts(payment_tx, receipts)?;
        if matches!(expected_swap_id, Some(expected) if expected != details.swap_id.as_slice()) {
            return Err(format!("unexpected swap_id {}", hex::encode(&details.swap_id)));
        }
        let state = client
            .payment_state_from(server, &details.swap_contract_address, &details.swap_id)
            .await?;
        let claimed = if state == eth::PAYMENT_STATE_SENT.into() {
            return Ok(SpendRead::NotSpent);
        } else if state == eth::PAYMENT_STATE_SPENT.into() {
            true
        } else if state == eth::_PAYMENT_STATE_REFUNDED.into() {
            false
        } else if state == eth::PAYMENT_STATE_UNINITIALIZED.into() {
            return Err("the swap contract does not know the payment".to_owned());
        } else {
            return Err(format!("unknown payment state {state}"));
        };
        let secret_hash = secret_hash.unwrap_or(&details.secret_hash);
        let (expected, caller) = match claimed {
            true => (ExpectedSwapEvent::ReceiverSpent { secret_hash }, details.receiver),
            false => (ExpectedSwapEvent::SenderRefunded, details.sender),
        };
        let candidates = self
            .swap_event_candidates_from(client, server, expected.topic(), caller, from_block)
            .await?;
        let found = candidates.into_iter().find(|(candidate, receipts)| {
            holds_swap_call(candidate, &expected, &details.swap_id)
                && bound_swap_event_output(
                    candidate,
                    receipts,
                    &expected,
                    &details.swap_id,
                    &details.swap_contract_address,
                )
                .is_ok()
        });
        match (found, claimed) {
            (Some((tx, _)), true) => Ok(SpendRead::Claimed(tx)),
            (Some((tx, _)), false) => Ok(SpendRead::Refunded(tx)),
            (None, true) => Err(format!(
                "the swap contract reports the payment as claimed, but no claim with the swap's secret is in the \
                 event history from block {from_block}"
            )),
            (None, false) => Err(format!(
                "the swap contract reports the payment as refunded, but no refund of it is in the event history from \
                 block {from_block}"
            )),
        }
    }

    /// One attempt of the spend read. The servers outside the pair (or beside the trusted server) are passed too: a
    /// claim proves itself and is taken from any one of them. This holds under an `Insufficient` policy too: the
    /// servers are then asked for such a claim only, so "not spent" and a refund stay undetermined.
    async fn spend_read_once(
        &self,
        policy: &SwapEvidencePolicy,
        client: &ElectrumClient,
        payment_tx: &UtxoTx,
        expected_swap_id: Option<&[u8]>,
        secret_hash: Option<&[u8]>,
        from_block: u64,
    ) -> Agreement<AtTip<SpendRead>, AtHeight<SpendDecision>> {
        Self::agree_at_own_tips(
            client,
            policy,
            true,
            |server: String, _tip: u64| async move {
                self.spend_read_on(client, &server, payment_tx, expected_swap_id, secret_hash, from_block)
                    .await
            },
            spend_decision,
            |read| matches!(read, SpendRead::Claimed(_)),
        )
        .await
    }

    /// Whether the payment `payment_tx` is claimed, refunded or not spent, under `policy` (CRD ch.51 R100). "Not
    /// spent" is answered only when two servers agree on it, each at its own tip, or the trusted server says so.
    ///
    /// # Errors
    ///
    /// The read is undetermined: the servers disagree, too few of them answer, or the trusted server is silent. An
    /// undetermined read is never answered as "not spent".
    pub(super) async fn agreed_swap_tx_spend(
        &self,
        policy: &SwapEvidencePolicy,
        client: &ElectrumClient,
        payment_tx: &UtxoTx,
        expected_swap_id: Option<&[u8]>,
        secret_hash: &[u8],
        from_block: u64,
    ) -> Result<Option<FoundSwapTxSpend>, String> {
        let attempt = self
            .spend_read_once(
                policy,
                client,
                payment_tx,
                expected_swap_id,
                Some(secret_hash),
                from_block,
            )
            .await;
        match attempt {
            Agreement::Agreed { answer, .. } => Ok(match answer.read {
                SpendRead::NotSpent => None,
                SpendRead::Claimed(tx) => Some(FoundSwapTxSpend::Spent(TransactionEnum::UtxoTx(tx))),
                SpendRead::Refunded(tx) => Some(FoundSwapTxSpend::Refunded(TransactionEnum::UtxoTx(tx))),
            }),
            Agreement::Undetermined(undetermined) => {
                let subject = format!("payment transaction {:?}", tx_hash_of(payment_tx));
                Err(self.report_undetermined("spend state", &subject, &undetermined))
            },
        }
    }

    /// Waits until the payment `payment_tx` is claimed, or `wait_until` (UNIX seconds) passes, under `policy` (CRD
    /// ch.51 R100). Every ten seconds one attempt of the spend read is made. A claim carries the swap's secret and
    /// is taken on one server's answer. An undetermined attempt, like "not spent" and a refund, leaves the wait going
    /// to its deadline.
    pub(super) async fn wait_for_agreed_payment_spend(
        &self,
        policy: &SwapEvidencePolicy,
        client: &ElectrumClient,
        payment_tx: &UtxoTx,
        wait_until: u64,
        from_block: u64,
    ) -> Result<TransactionEnum, String> {
        loop {
            match self
                .spend_read_once(policy, client, payment_tx, None, None, from_block)
                .await
            {
                Agreement::Agreed {
                    answer:
                        AtTip {
                            read: SpendRead::Claimed(tx),
                            ..
                        },
                    ..
                } => return Ok(TransactionEnum::UtxoTx(tx)),
                Agreement::Agreed { .. } => (),
                Agreement::Undetermined(undetermined) => {
                    let subject = format!("payment transaction {:?}", tx_hash_of(payment_tx));
                    self.report_undetermined("spend state", &subject, &undetermined);
                },
            }

            if now_ms() / 1000 > wait_until {
                return ERR!("Waited too long until {} for {:?} to be spent ", wait_until, payment_tx);
            }
            Timer::sleep(READ_RETRY_SECS).await;
        }
    }

    /// The already-sent read of one server: "not sent" while the contract does not know the payment `swap_id`, else
    /// this node's payment transaction of the server's event history.
    async fn sent_read_on(
        &self,
        client: &ElectrumClient,
        server: &str,
        swap_contract: &H160,
        swap_id: &[u8],
        from_block: u64,
    ) -> Result<SentRead, String> {
        let state = client.payment_state_from(server, swap_contract, swap_id).await?;
        if state == eth::PAYMENT_STATE_UNINITIALIZED.into() {
            return Ok(SentRead::NotSent);
        }
        let my_address = self
            .utxo
            .derivation_method
            .iguana_or_err()
            .map_err(|e| e.to_string())?
            .clone();
        let sender = qtum::contract_addr_from_utxo_addr(my_address).map_err(|e| e.to_string())?;
        let candidates = self
            .swap_event_candidates_from(client, server, QRC20_PAYMENT_SENT_TOPIC, sender, from_block)
            .await?;
        Ok(candidates
            .into_iter()
            .map(|(tx, _)| tx)
            .find(|tx| find_swap_contract_call_with_swap_id(MutContractCallType::Erc20Payment, tx, swap_id).is_some())
            .map_or(SentRead::StateWithoutTransaction, SentRead::Sent))
    }

    /// Whether this node's payment `swap_id` was sent, under `policy` (CRD ch.51 R100): "not sent" and the payment
    /// transaction are what two servers agree on, each at its own tip, or what the trusted server shows.
    ///
    /// # Errors
    ///
    /// The read is undetermined. It is never answered as "not sent".
    pub(super) async fn agreed_payment_sent(
        &self,
        policy: &SwapEvidencePolicy,
        client: &ElectrumClient,
        swap_contract: &H160,
        swap_id: &[u8],
        from_block: u64,
    ) -> Result<Option<TransactionEnum>, String> {
        let attempt = Self::agree_at_own_tips(
            client,
            policy,
            false,
            |server: String, _tip: u64| async move {
                self.sent_read_on(client, &server, swap_contract, swap_id, from_block)
                    .await
            },
            sent_decision,
            |_| false,
        )
        .await;
        match attempt {
            Agreement::Agreed { answer, .. } => match answer.read {
                SentRead::NotSent => Ok(None),
                SentRead::Sent(tx) => Ok(Some(TransactionEnum::UtxoTx(tx))),
                // CRD ch.39 R39.11.5A.7, D39.11.5A.1 (a): a history without the payment contradicts the state.
                SentRead::StateWithoutTransaction => ERR!(
                    "Undetermined whether the payment was sent: the swap contract reports a payment for this swap, \
                     but no payment transaction was found in the token history from block {}",
                    from_block
                ),
            },
            Agreement::Undetermined(undetermined) => {
                let subject = format!("payment {}", hex::encode(swap_id));
                Err(self.report_undetermined("already-sent check", &subject, &undetermined))
            },
        }
    }
}
