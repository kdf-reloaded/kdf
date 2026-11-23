//! The legacy (V1) EVM swap reads under the swap-evidence rule (CRD ch.51 §51.6G R100).
//!
//! # Purpose
//!
//! A refund or a claim of a legacy EVM swap rests on three chain reads: the counterparty's payment state, whether a
//! payment is claimed, refunded or not spent, and the chain time of the refund gate. With an adopted policy each of
//! them is asked of single nodes, addressed on their own, and decided by [`crate::swap_evidence::agree`]: two nodes
//! on distinct hosts must reach the same decision, or the one node the operator trusts answers alone.
//!
//! # Public exports
//!
//! - `Web3Instance::node_name` and the directed reads of one node: `block_number`, `payment_state_at`,
//!   `transaction_by_hash`, `transaction_block`, `swap_logs`, `block_timestamp`.
//! - `EthCoin::agreed_swap_reads`: the policy the reads are agreed under, `None` while the coin reads as before.
//! - `EthCoin::agreed_payment_read`, `EthCoin::agreed_swap_tx_spend`, `EthCoin::wait_for_agreed_payment_spend`,
//!   `EthCoin::agreed_refund_gate`: the three reads.
//!
//! # Invariants
//!
//! - A directed read asks one node, never fails over, and gives up after the per-node cutoff of the failover path.
//! - The payment-state read and the spend read of one attempt are made at one common block: the lower tip of the
//!   attempt's pair (the trusted node's own tip under a trusted node). A node whose tip is below that block gives
//!   no answer in the attempt. Only the pair's nodes are asked for a tip before the read.
//! - One attempt takes one pair of answers. Two answers that differ leave the attempt undetermined; no further
//!   node is asked to outvote them.
//! - An undetermined read never yields "valid", "not spent" or an open refund gate.
//! - A node is named by host and port only, in every log line and error text.

use super::alloy_compat::assert_send_future;
use super::alloy_compat::transport::NODE_CUTOFF_S;
use super::eth_logs_window::{LogsQuerier, PaymentKey, REORG_MARGIN_BLOCKS};
use super::eth_swap_ops::{evm_refund_eligibility, secret_of_claim_call, secret_of_spent_logs};
use super::*;
use crate::node_url::{host_only, scrub_node_url, UNNAMED_NODE};
use crate::swap_evidence::{agree, node_host, node_matches, node_name, Agreement, Undetermined, UndeterminedReason};
use crate::{CanRefundHtlc, SwapEvidencePolicy};
use common::log::{debug, warn};
use std::future::Future as StdFuture;

/// A transaction as a node returns it.
pub(crate) type NodeTransaction = alloy::rpc::types::eth::Transaction;

/// Seconds between two attempts of the payment-state read while it is undetermined or the payment is not yet
/// visible at the common block.
const PAYMENT_READ_RETRY_S: f64 = 10.;

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

/// A lower-layer error text of the node `url` with the node named by host and port only.
fn node_error_text(url: &str, text: &str) -> String {
    let scrubbed = scrub_node_url(text, url);
    let label = host_only(url);
    if label == UNNAMED_NODE {
        scrubbed
    } else {
        scrubbed.replace(&label, &node_name(url))
    }
}

/// Runs one request of the node `url` for at most the per-node cutoff of the failover path.
async fn within_node_cutoff<T, E, F>(url: &str, method: &str, request: F) -> Result<T, String>
where
    E: fmt::Display,
    F: StdFuture<Output = Result<T, E>>,
{
    let request = Box::pin(request);
    match select(request, Timer::sleep(NODE_CUTOFF_S)).await {
        Either::Left((Ok(answer), _)) => Ok(answer),
        Either::Left((Err(e), _)) => Err(format!("{method} failed: {}", node_error_text(url, &e.to_string()))),
        Either::Right(_) => Err(format!("no answer to {method} within {NODE_CUTOFF_S} s")),
    }
}

impl Web3Instance {
    /// The form in which a log line or an error text names this node: `host[:port]`.
    pub(crate) fn node_name(&self) -> String { node_name(&self.url) }

    /// `eth_blockNumber` of this node.
    pub(crate) async fn block_number(&self) -> Result<u64, String> {
        use alloy::providers::Provider;

        within_node_cutoff(&self.url, "eth_blockNumber", self.web3.get_block_number()).await
    }

    /// The state of the legacy swap contract's payment `id` at `block`, as this node reports it.
    pub(crate) async fn payment_state_at(
        &self,
        from: Address,
        swap_contract_address: Address,
        id: &[u8],
        block: u64,
    ) -> Result<U256, String> {
        use alloy::providers::Provider;

        let function = SWAP_CONTRACT.function("payments").map_err(|e| e.to_string())?;
        let data = function
            .encode_input(&[Token::FixedBytes(id.to_vec())])
            .map_err(|e| e.to_string())?;
        let request = CallRequest {
            from: Some(from),
            to: swap_contract_address,
            gas: None,
            gas_price: None,
            max_fee_per_gas: None,
            max_priority_fee_per_gas: None,
            value: None,
            data: Some(data.into()),
        };
        let call = self
            .web3
            .client()
            .request::<_, Bytes>("eth_call", (request, BlockNumber::Number(block)));
        let bytes = within_node_cutoff(&self.url, "eth_call", call).await?;
        let decoded = function.decode_output(&bytes.0).map_err(|e| e.to_string())?;
        match decoded.get(2) {
            Some(Token::Uint(state)) => Ok(*state),
            other => Err(format!("the payment state must be a uint, got {:?}", other)),
        }
    }

    /// `eth_getTransactionByHash` of this node.
    pub(crate) async fn transaction_by_hash(&self, hash: H256) -> Result<Option<NodeTransaction>, String> {
        use alloy::providers::Provider;

        let hash = alloy::primitives::B256::from_slice(&hash.0);
        within_node_cutoff(
            &self.url,
            "eth_getTransactionByHash",
            self.web3.get_transaction_by_hash(hash),
        )
        .await
    }

    /// The block of the receipt of transaction `hash` (`eth_getTransactionReceipt`), `None` while this node has no
    /// receipt of it.
    pub(crate) async fn transaction_block(&self, hash: H256) -> Result<Option<u64>, String> {
        use alloy::providers::Provider;

        let hash = alloy::primitives::B256::from_slice(&hash.0);
        let receipt = within_node_cutoff(
            &self.url,
            "eth_getTransactionReceipt",
            self.web3.get_transaction_receipt(hash),
        )
        .await?;
        Ok(receipt.and_then(|receipt| receipt.block_number))
    }

    /// The logs of `address` with the first topic `topic` in blocks `from` to `to`, both inclusive, as this node
    /// reports them. `querier` must be a querier over this node's provider.
    pub(crate) async fn swap_logs(
        &self,
        querier: LogsQuerier,
        topic: H256,
        address: Address,
        from: u64,
        to: u64,
    ) -> Result<Vec<Log>, String> {
        within_node_cutoff(&self.url, "eth_getLogs", querier.get_logs(topic, address, from, to)).await
    }

    /// The timestamp of block `block` of this node, or of its latest block (`eth_getBlockByNumber`).
    pub(crate) async fn block_timestamp(&self, block: Option<u64>) -> Result<u64, String> {
        use alloy::eips::BlockNumberOrTag;
        use alloy::providers::Provider;
        use std::future::IntoFuture;

        let tag = block.map_or(BlockNumberOrTag::Latest, BlockNumberOrTag::Number);
        let request = self.web3.get_block_by_number(tag).into_future();
        match within_node_cutoff(&self.url, "eth_getBlockByNumber", request).await? {
            Some(block) => Ok(block.header.timestamp),
            None => Err("the node returned no block".to_owned()),
        }
    }
}

/// The counterparty's payment as one node shows it: the contract's state of the payment and the payment
/// transaction.
pub(crate) struct PaymentRead {
    pub(crate) state: U256,
    pub(crate) tx: Option<NodeTransaction>,
}

/// What two payment reads are compared on: the contract's state and what the transaction says (its sender, its
/// receiver, its value and its call input).
#[derive(PartialEq)]
pub(crate) struct PaymentDecision {
    state: U256,
    tx: Option<(Address, Option<Address>, U256, Vec<u8>)>,
}

impl fmt::Debug for PaymentDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "payment state {}, ", self.state)?;
        match &self.tx {
            Some((from, _, _, input)) => {
                write!(f, "transaction from {:?} with {} input bytes", from, input.len())
            },
            None => f.write_str("transaction unknown"),
        }
    }
}

fn payment_decision(read: &PaymentRead) -> PaymentDecision {
    use alloy::consensus::Transaction as _;

    let tx = read.tx.as_ref().map(|tx| {
        let envelope = tx.inner.inner();
        let value: [u8; 32] = envelope.value().to_be_bytes();
        (
            Address::from_slice(tx.inner.signer().as_slice()),
            envelope.to().map(|to| Address::from_slice(to.as_slice())),
            U256::from_big_endian(&value),
            envelope.input().to_vec(),
        )
    });
    PaymentDecision { state: read.state, tx }
}

/// Whether a payment is claimed, refunded or not spent, as one node shows it at one block.
#[derive(Debug)]
pub(crate) enum SpendRead {
    /// The contract holds the payment as sent, or does not know it.
    NotSpent,
    /// The receiver claimed the payment in `tx`. `proven` when the claim carries a secret whose hash is the secret
    /// hash of the payment.
    Claimed { tx: Box<SignedEthTx>, proven: bool },
    /// The sender refunded the payment in `tx`.
    Refunded { tx: Box<SignedEthTx> },
}

/// What two spend reads are compared on.
#[derive(Debug, PartialEq)]
pub(crate) enum SpendDecision {
    NotSpent,
    ClaimedBy(H256),
    RefundedBy(H256),
}

fn spend_decision(read: &SpendRead) -> SpendDecision {
    match read {
        SpendRead::NotSpent => SpendDecision::NotSpent,
        SpendRead::Claimed { tx, .. } => SpendDecision::ClaimedBy(tx.tx_hash()),
        SpendRead::Refunded { tx } => SpendDecision::RefundedBy(tx.tx_hash()),
    }
}

/// What two chain times are compared on (CRD ch.51 R100 item 1).
#[derive(Debug, PartialEq)]
enum ChainTime {
    PastTheLock,
    NotPastTheLock,
}

/// One node of an attempt. `tip` is the tip the node reported when the attempt began, or its failure to report one;
/// `None` for a node outside the attempt's pair, which is asked for its tip only when it is asked at all.
struct EvidenceNode {
    name: String,
    instance: Web3Instance,
    tip: Option<Result<u64, String>>,
}

/// The indexes of the nodes whose tips make the common block of an attempt, in the order they are asked, and how
/// many tips are needed: under two nodes the first node of every distinct host and two tips; under a trusted node
/// that node and its tip; otherwise none.
fn tip_candidates(policy: &SwapEvidencePolicy, names: &[String]) -> (Vec<usize>, usize) {
    match policy {
        SwapEvidencePolicy::TwoNodes { .. } => {
            let mut hosts: Vec<String> = Vec::new();
            let mut candidates = Vec::new();
            for (index, name) in names.iter().enumerate() {
                let host = node_host(name).unwrap_or_else(|| name.to_ascii_lowercase());
                if !hosts.contains(&host) {
                    hosts.push(host);
                    candidates.push(index);
                }
            }
            (candidates, 2)
        },
        SwapEvidencePolicy::Trusted { node } => (
            (0..names.len())
                .filter(|index| node_matches(node, &names[*index]))
                .take(1)
                .collect(),
            1,
        ),
        _ => (Vec::new(), 0),
    }
}

/// The spend read of one node at block `upto`. The contract's state decides; for a claimed or a refunded payment
/// the node's own logs name the transaction, searched from `upto` downwards.
async fn spend_read_on(
    coin: EthCoin,
    node: Web3Instance,
    key: PaymentKey,
    payment_hash: H256,
    secret_hash: Vec<u8>,
    search_from_block: u64,
    upto: u64,
) -> Result<SpendRead, String> {
    let (swap_contract_address, id) = (key.0, key.1.as_slice());
    let state = node
        .payment_state_at(coin.my_address, swap_contract_address, id, upto)
        .await?;
    let claimed = if state == U256::from(PAYMENT_STATE_UNINITIALIZED) || state == U256::from(PAYMENT_STATE_SENT) {
        return Ok(SpendRead::NotSpent);
    } else if state == U256::from(PAYMENT_STATE_SPENT) {
        true
    } else if state == U256::from(_PAYMENT_STATE_REFUNDED) {
        false
    } else {
        return Err(format!("unknown payment state {state}"));
    };
    let event = if claimed { "ReceiverSpent" } else { "SenderRefunded" };
    let topic = SWAP_CONTRACT.event(event).map_err(|e| e.to_string())?.signature();

    // The spend cannot precede the payment; the low end is 12 blocks below the payment's block, as a
    // reorganization can move the payment.
    let floor = match node.transaction_block(payment_hash).await? {
        Some(block) => search_from_block.max(block.saturating_sub(REORG_MARGIN_BLOCKS)),
        None => search_from_block,
    };
    let mut top = upto;
    let found = loop {
        if top < floor {
            break None;
        }
        let from = floor.max(top.saturating_add(1).saturating_sub(coin.logs_window()));
        let querier = coin.logs_querier_on(node.web3.clone());
        let logs = node.swap_logs(querier, topic, swap_contract_address, from, top).await?;
        if let Some(log) = logs.into_iter().find(|log| log.data.0.get(..32) == Some(id)) {
            break Some(log);
        }
        match from.checked_sub(1) {
            Some(below) => top = below,
            None => break None,
        }
    };
    let Some(log) = found else {
        return Err(format!(
            "the swap contract reports the payment as {} at block {upto}, but no {event} log of it is in blocks \
             {floor} to {upto}",
            if claimed { "claimed" } else { "refunded" }
        ));
    };
    let Some(spend_hash) = log.transaction_hash else {
        return Err(format!("the {event} log of the payment has no transaction hash"));
    };
    let Some(transaction) = node.transaction_by_hash(spend_hash).await? else {
        return Err(format!("the {event} transaction {:02x} is missing", spend_hash));
    };
    let tx = Box::new(signed_tx_from_alloy_tx(transaction)?);
    if !claimed {
        return Ok(SpendRead::Refunded { tx });
    }
    let spent_log = (
        log.topics.first().map(|topic| topic.as_bytes().to_vec()),
        log.data.0.clone(),
    );
    let proven = secret_of_claim_call(&tx.data, &secret_hash)?.is_some()
        || secret_of_spent_logs(&[spent_log], &secret_hash)?.is_some();
    Ok(SpendRead::Claimed { tx, proven })
}

impl EthCoin {
    /// The policy this coin's legacy swap reads are agreed under (CRD ch.51 R100), or `None` when they are read
    /// as before: a family that has not adopted the rule, and a coin served by the operator's own daemon.
    pub(crate) fn agreed_swap_reads(&self) -> Option<SwapEvidencePolicy> {
        match self.swap_evidence_policy() {
            SwapEvidencePolicy::OwnDaemon | SwapEvidencePolicy::NotAdopted => None,
            policy => Some(policy),
        }
    }

    /// The nodes a read under `policy` may ask, and the policy in the form [`agree`] matches node names with.
    ///
    /// Under a trusted node the list holds that node (the instance of exactly the trusted address) and, when
    /// `with_rest`, the nodes of other addresses: they are asked only for an answer that proves itself. The trusted
    /// node is absent when it did not pass the activation probe; the read is then undetermined.
    ///
    /// The trusted node is found by its exact address here, not by [`agree`]'s own match of the policy's node
    /// against a node name: an address without a written port takes its scheme's default port in that match, while
    /// `node_name` writes no port for it, so the two would not match; and a node of the same host on another port
    /// could be taken for the trusted one. [`agree`] is therefore given the trusted node in its `node_name` form
    /// and a list without any other node that this form would match.
    fn evidence_instances(
        &self,
        policy: &SwapEvidencePolicy,
        with_rest: bool,
    ) -> (SwapEvidencePolicy, Vec<Web3Instance>) {
        match policy {
            SwapEvidencePolicy::Trusted { node } => {
                let trusted_name = node_name(node);
                let instances = self
                    .web3_instances
                    .iter()
                    .filter(|instance| {
                        instance.url == *node || (with_rest && !node_matches(&trusted_name, &instance.node_name()))
                    })
                    .cloned()
                    .collect();
                (SwapEvidencePolicy::Trusted { node: trusted_name }, instances)
            },
            other => (other.clone(), self.web3_instances.clone()),
        }
    }

    /// One attempt of a read that is made at a common block.
    ///
    /// The tips of the attempt's pair are read first: the first two distinct hosts that report one, walking past a
    /// node that does not (under a trusted node: that node's tip). The common block is the lower of the two tips.
    /// `read` then runs on the nodes [`agree`] asks, at the common block. A node outside the pair is asked for its
    /// own tip when [`agree`] turns to it, and gives no answer when that tip is below the common block. Without a
    /// common block (too few tips, or a policy without one) a node reads at its own tip, and [`agree`] takes such an
    /// answer only when it proves itself.
    async fn agree_at_common_block<A, K, R, Fut>(
        &self,
        policy: &SwapEvidencePolicy,
        with_rest: bool,
        read: R,
        classify: impl Fn(&A) -> K,
        proves_itself: impl Fn(&A) -> bool,
    ) -> Agreement<A, K>
    where
        K: PartialEq + fmt::Debug,
        R: Fn(Web3Instance, u64) -> Fut,
        Fut: StdFuture<Output = Result<A, String>>,
    {
        let (policy, instances) = self.evidence_instances(policy, with_rest);
        let names: Vec<String> = instances.iter().map(Web3Instance::node_name).collect();
        let (candidates, needed) = tip_candidates(&policy, &names);
        let mut tips: Vec<Option<Result<u64, String>>> = vec![None; instances.len()];
        let (mut reported, mut asked) = (Vec::new(), 0);
        while reported.len() < needed && asked < candidates.len() {
            let batch: Vec<usize> = candidates[asked..]
                .iter()
                .copied()
                .take(needed - reported.len())
                .collect();
            asked += batch.len();
            let answers = assert_send_future(join_all(batch.iter().map(|index| {
                let instance = instances[*index].clone();
                async move { instance.block_number().await }
            })))
            .await;
            for (index, tip) in batch.into_iter().zip(answers) {
                if let Ok(tip) = &tip {
                    reported.push(*tip);
                }
                tips[index] = Some(tip);
            }
        }
        let common = if needed > 0 && reported.len() == needed {
            reported.iter().copied().min()
        } else {
            None
        };
        let nodes: Vec<EvidenceNode> = instances
            .into_iter()
            .zip(names)
            .zip(tips)
            .map(|((instance, name), tip)| EvidenceNode { name, instance, tip })
            .collect();
        let read = &read;
        agree(
            &policy,
            &nodes,
            |node| node.name.clone(),
            |node| {
                let (known_tip, instance) = (node.tip.clone(), node.instance.clone());
                async move {
                    let tip = match known_tip {
                        Some(tip) => tip?,
                        None => instance.block_number().await?,
                    };
                    let at = match common {
                        Some(common) if tip < common => {
                            return Err(format!("its tip {tip} is below the common block {common}"))
                        },
                        Some(common) => common,
                        None => tip,
                    };
                    read(instance, at).await
                }
            },
            classify,
            proves_itself,
        )
        .await
    }

    /// The text of an undetermined read: what each node said and the two ways to fix it (CRD ch.51 R100 item 9).
    /// The first time a read of `subject` is undetermined the text is logged at WARN, afterwards at DEBUG.
    fn report_undetermined(&self, read: &str, subject: &str, undetermined: &Undetermined) -> String {
        let text = format!(
            "[{}] the {} of {} is undetermined and the step waits: {}. To fix it, add a node on another host to \
             the coin's node list or set swap_evidence_node",
            self.ticker(),
            read,
            subject,
            undetermined
        );
        if first_report(format!("{}:{}:{}", self.ticker(), read, subject)) {
            warn!("{}", text);
        } else {
            debug!("{}", text);
        }
        text
    }

    /// One attempt of the payment-state read: the contract's state of payment `id` and the payment transaction.
    async fn payment_read_once(
        &self,
        policy: &SwapEvidencePolicy,
        swap_contract_address: Address,
        id: &[u8],
        payment_hash: H256,
    ) -> Agreement<PaymentRead, PaymentDecision> {
        // Nothing can decide this read without two nodes or a trusted one, so no node is asked.
        if matches!(policy, SwapEvidencePolicy::Insufficient { .. }) {
            return Agreement::Undetermined(Undetermined {
                reason: UndeterminedReason::TooFewAnswers,
                per_node: Vec::new(),
            });
        }
        let (from, id) = (self.my_address, id.to_vec());
        self.agree_at_common_block(
            policy,
            false,
            move |node: Web3Instance, at: u64| {
                let id = id.clone();
                assert_send_future(async move {
                    let state = node.payment_state_at(from, swap_contract_address, &id, at).await?;
                    let tx = node.transaction_by_hash(payment_hash).await?;
                    Ok::<_, String>(PaymentRead { state, tx })
                })
            },
            payment_decision,
            |_| false,
        )
        .await
    }

    /// The counterparty's payment under `policy` (CRD ch.51 R100): the state of payment `id` and the payment
    /// transaction, as two nodes agree on them at a common block, or as the trusted node shows them.
    ///
    /// The read is repeated until `wait_until` (UNIX seconds) while it is undetermined, and while the nodes agree
    /// that the contract does not know the payment yet (a node one block behind makes the common block the block
    /// before the payment's). It is made at least once.
    ///
    /// # Errors
    ///
    /// The read is still undetermined at `wait_until`. The text names what each node said.
    pub(crate) async fn agreed_payment_read(
        &self,
        policy: &SwapEvidencePolicy,
        swap_contract_address: Address,
        id: &[u8],
        payment_hash: H256,
        wait_until: u64,
    ) -> Result<PaymentRead, String> {
        loop {
            let attempt = assert_send_future(self.payment_read_once(policy, swap_contract_address, id, payment_hash));
            match attempt.await {
                Agreement::Agreed { answer, .. } => {
                    let not_visible_yet = answer.state == U256::from(PAYMENT_STATE_UNINITIALIZED);
                    if !not_visible_yet || now_ms() / 1000 >= wait_until {
                        return Ok(answer);
                    }
                },
                Agreement::Undetermined(undetermined) => {
                    let subject = format!("payment {}", hex::encode(id));
                    let text = self.report_undetermined("payment state", &subject, &undetermined);
                    if now_ms() / 1000 >= wait_until {
                        return Err(text);
                    }
                },
            }
            Timer::sleep(PAYMENT_READ_RETRY_S).await;
        }
    }

    /// The `bytes20` secret hash the payment call of `payment_tx` carries (CRD ch.51 R72A).
    fn payment_secret_hash_arg(&self, payment_tx: &SignedEthTx) -> Result<Vec<u8>, String> {
        let (function, index) = match self.coin_type {
            EthCoinType::Eth => ("ethPayment", 2),
            EthCoinType::Erc20 { .. } => ("erc20Payment", 4),
            EthCoinType::Tron | EthCoinType::Trc20 { .. } => {
                return Err("a TRON coin has no legacy EVM payment call".to_owned())
            },
        };
        let function = SWAP_CONTRACT.function(function).map_err(|e| e.to_string())?;
        let Some(args) = payment_tx.data.get(4..) else {
            return Err("the payment's call input is shorter than a selector".to_owned());
        };
        let decoded = function.decode_input(args).map_err(|e| e.to_string())?;
        match decoded.get(index) {
            Some(Token::FixedBytes(secret_hash)) => Ok(secret_hash.clone()),
            other => Err(format!("the payment's secret hash argument is {:?}", other)),
        }
    }

    /// One attempt of the spend read of payment `id`.
    async fn spend_read_once(
        &self,
        policy: &SwapEvidencePolicy,
        payment_tx: &SignedEthTx,
        id: &[u8],
        swap_contract_address: Address,
        search_from_block: u64,
    ) -> Result<Agreement<SpendRead, SpendDecision>, String> {
        let secret_hash = self.payment_secret_hash_arg(payment_tx)?;
        let coin = self.clone();
        let key: PaymentKey = (swap_contract_address, id.to_vec());
        let payment_hash = payment_tx.tx_hash();
        // The nodes outside the pair (or beside the trusted node) are passed too: a claim whose secret matches the
        // payment's secret hash proves itself and is taken from any one of them (CRD ch.51 R100). This holds under
        // an `Insufficient` policy too: `agree` then asks the coin's nodes for such a claim only and ignores every
        // other answer, so "not spent" and a claim without the secret stay undetermined.
        Ok(self
            .agree_at_common_block(
                policy,
                true,
                move |node: Web3Instance, at: u64| {
                    assert_send_future(spend_read_on(
                        coin.clone(),
                        node,
                        key.clone(),
                        payment_hash,
                        secret_hash.clone(),
                        search_from_block,
                        at,
                    ))
                },
                spend_decision,
                |read| matches!(read, SpendRead::Claimed { proven: true, .. }),
            )
            .await)
    }

    /// Whether the payment `payment_tx` (swap id `id`) is claimed, refunded or not spent, under `policy`
    /// (CRD ch.51 R100). "Not spent" is answered only when two nodes agree on it at a common block, or the trusted
    /// node says so.
    ///
    /// # Errors
    ///
    /// The read is undetermined: the nodes disagree, too few of them answer, or the trusted node is silent. The
    /// text names what each node said. An undetermined read is never answered as "not spent".
    pub(crate) async fn agreed_swap_tx_spend(
        &self,
        policy: &SwapEvidencePolicy,
        payment_tx: &SignedEthTx,
        id: &[u8],
        swap_contract_address: Address,
        search_from_block: u64,
    ) -> Result<Option<FoundSwapTxSpend>, String> {
        let attempt =
            assert_send_future(self.spend_read_once(policy, payment_tx, id, swap_contract_address, search_from_block));
        match attempt.await? {
            Agreement::Agreed { answer, .. } => Ok(match answer {
                SpendRead::NotSpent => None,
                SpendRead::Claimed { tx, .. } => Some(FoundSwapTxSpend::Spent(TransactionEnum::from(*tx))),
                SpendRead::Refunded { tx } => Some(FoundSwapTxSpend::Refunded(TransactionEnum::from(*tx))),
            }),
            Agreement::Undetermined(undetermined) => {
                let subject = format!("payment {}", hex::encode(id));
                Err(self.report_undetermined("spend state", &subject, &undetermined))
            },
        }
    }

    /// Waits until the payment `payment_tx` (swap id `id`) is claimed, or `wait_until` (UNIX seconds) passes, under
    /// `policy` (CRD ch.51 R100). Every `poll_every` seconds one attempt of the spend read is made. A claim that
    /// carries the secret of the payment is taken on one node's answer; any other claim needs the agreement. An
    /// undetermined attempt, like "not spent", leaves the wait going to its deadline.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn wait_for_agreed_payment_spend(
        &self,
        policy: &SwapEvidencePolicy,
        payment_tx: &SignedEthTx,
        id: &[u8],
        swap_contract_address: Address,
        wait_until: u64,
        from_block: u64,
        poll_every: f64,
    ) -> Result<TransactionEnum, crate::TransactionErr> {
        loop {
            let attempt =
                assert_send_future(self.spend_read_once(policy, payment_tx, id, swap_contract_address, from_block));
            match attempt.await {
                Ok(Agreement::Agreed {
                    answer: SpendRead::Claimed { tx, .. },
                    ..
                }) => return Ok(TransactionEnum::from(*tx)),
                Ok(Agreement::Agreed { .. }) => (),
                Ok(Agreement::Undetermined(undetermined)) => {
                    let subject = format!("payment {}", hex::encode(id));
                    self.report_undetermined("spend state", &subject, &undetermined);
                },
                Err(e) => return TX_PLAIN_ERR!("{}", e),
            }

            if now_ms() / 1000 > wait_until {
                return TX_PLAIN_ERR!(
                    "Waited too long until {} for transaction {:?} to be spent",
                    wait_until,
                    payment_tx,
                );
            }
            Timer::sleep(poll_every).await;
        }
    }

    /// The refund gate under `policy` once the local clock `now` is past `locktime` (CRD ch.51 R100, §51.6E R88):
    /// each node's latest block timestamp is read, and the gate opens when two nodes (or the trusted node) show a
    /// time past the lock, whatever the two times are. Nodes that agree on a time not past the lock, and an
    /// undetermined read, leave the gate closed with a wait of at most `EVM_TIP_RECHECK_MAX_SECS`.
    pub(crate) async fn agreed_refund_gate(
        &self,
        policy: &SwapEvidencePolicy,
        now: u64,
        locktime: u64,
    ) -> CanRefundHtlc {
        let outcome = if matches!(policy, SwapEvidencePolicy::Insufficient { .. }) {
            // Nothing can decide this read without two nodes or a trusted one, so no node is asked.
            Agreement::Undetermined(Undetermined {
                reason: UndeterminedReason::TooFewAnswers,
                per_node: Vec::new(),
            })
        } else {
            let (policy, instances) = self.evidence_instances(policy, false);
            let reading = agree(
                &policy,
                &instances,
                |instance| instance.node_name(),
                |instance| {
                    let instance = instance.clone();
                    assert_send_future(async move { instance.block_timestamp(None).await })
                },
                // Strictly past the lock, as the gate has judged a block timestamp since CRD ch.51 §51.6E R88.
                |timestamp: &u64| {
                    if *timestamp > locktime {
                        ChainTime::PastTheLock
                    } else {
                        ChainTime::NotPastTheLock
                    }
                },
                |_| false,
            );
            assert_send_future(reading).await
        };
        match outcome {
            Agreement::Agreed { answer, .. } => evm_refund_eligibility(now, locktime, Some(answer)),
            Agreement::Undetermined(undetermined) => {
                let subject = format!("lock time {locktime}");
                self.report_undetermined("chain time of the refund gate", &subject, &undetermined);
                evm_refund_eligibility(now, locktime, None)
            },
        }
    }
}
