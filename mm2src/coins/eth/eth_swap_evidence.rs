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
//! - `Web3Instance::node_name` and the directed reads of one node: `latest_block`, `payment_state_at`,
//!   `transaction_by_hash`, `transaction_block`, `swap_logs`, `block_timestamp`.
//! - `EthCoin::agreed_swap_reads`: the policy the reads are agreed under, `None` while the coin reads as before.
//! - `EthCoin::agreed_payment_read`, `EthCoin::agreed_swap_tx_spend`, `EthCoin::wait_for_agreed_payment_spend`,
//!   `EthCoin::agreed_refund_gate`: the three reads.
//!
//! # Invariants
//!
//! - A directed read asks one node, never fails over, and gives up after the per-node cutoff of the failover path.
//! - The payment-state read and the spend read ask each node at its own latest block, and every answer carries that
//!   block's height and timestamp. Two answers whose tip timestamps are more than [`MAX_TIP_GAP_SECS`] apart leave
//!   the attempt undetermined.
//! - A node that is a block behind and has not seen a claim, a refund or a payment yet disagrees with a node that
//!   has. The read is then undetermined until both nodes show it. This is intended: no answer is taken at a block
//!   below a node's own tip, so a claim that carries the swap's secret is never hidden by a lagging node.
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
use crate::swap_evidence::{agree, agree_with_tips, node_name, Agreement, Undetermined, UndeterminedReason};
use crate::{CanRefundHtlc, SwapEvidencePolicy};
use common::log::{debug, warn};
use std::future::Future as StdFuture;

/// A transaction as a node returns it.
pub(crate) type NodeTransaction = alloy::rpc::types::eth::Transaction;

/// Seconds between two attempts of the payment-state read while it is undetermined or the nodes do not show the
/// payment yet.
const PAYMENT_READ_RETRY_S: f64 = 10.;

/// The largest difference, in seconds, between the timestamps of the tip blocks of two nodes whose answers are
/// compared. A gap in time, not in blocks, fits every EVM chain whatever its block time: a node that is this far
/// behind is not reading the chain the other node reads.
pub(crate) const MAX_TIP_GAP_SECS: u64 = 60;

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

    /// The height and the timestamp of this node's latest block (`eth_getBlockByNumber`).
    pub(crate) async fn latest_block(&self) -> Result<TipBlock, String> {
        use alloy::eips::BlockNumberOrTag;
        use alloy::providers::Provider;
        use std::future::IntoFuture;

        let request = self.web3.get_block_by_number(BlockNumberOrTag::Latest).into_future();
        match within_node_cutoff(&self.url, "eth_getBlockByNumber", request).await? {
            Some(block) => Ok(TipBlock {
                height: block.header.number,
                time: block.header.timestamp,
            }),
            None => Err("the node returned no latest block".to_owned()),
        }
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

/// The latest block of a node when it answered.
#[derive(Clone, Copy)]
pub(crate) struct TipBlock {
    pub(crate) height: u64,
    /// The block's timestamp, in seconds.
    pub(crate) time: u64,
}

/// What a node shows at its own latest block.
pub(crate) struct AtTip<T> {
    pub(crate) tip: TipBlock,
    pub(crate) read: T,
}

/// A decision with the height it was read at. Two of them are equal when the decisions are; the height is only
/// shown, in the text of an undetermined read.
struct AtHeight<D> {
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

/// [`EthCoin::agreed_swap_reads`] for a coin that answers `policy`; `tron_family` is whether it is a TRX or TRC20
/// coin, which is checked before the policy.
fn agreed_reads_policy(tron_family: bool, policy: SwapEvidencePolicy) -> Option<SwapEvidencePolicy> {
    if tron_family {
        return None;
    }
    match policy {
        SwapEvidencePolicy::OwnDaemon | SwapEvidencePolicy::NotAdopted => None,
        policy => Some(policy),
    }
}

impl EthCoin {
    /// The policy this coin's legacy swap reads are agreed under (CRD ch.51 R100), or `None` when they are read
    /// as before: a family that has not adopted the rule, and a coin served by the operator's own daemon.
    ///
    /// A TRX or TRC20 coin always gets `None`: TRON reads its swap evidence through its own TRON swap operations,
    /// never through the EVM agreed reads, whatever TRON's adoption state.
    pub(crate) fn agreed_swap_reads(&self) -> Option<SwapEvidencePolicy> {
        agreed_reads_policy(self.is_tron_family(), self.swap_evidence_policy())
    }

    /// The nodes a read under `policy` may ask: the coin's probe-passed nodes. Under a trusted node and without
    /// `with_rest` only that node, found as [`agree`] finds it (by its `node_name`): the other nodes could only be
    /// asked for an answer that proves itself, which such a read never has. The trusted node is absent when it did
    /// not pass the activation probe; the read is then undetermined.
    fn evidence_instances(&self, policy: &SwapEvidencePolicy, with_rest: bool) -> Vec<Web3Instance> {
        match policy {
            SwapEvidencePolicy::Trusted { node } if !with_rest => {
                let trusted_name = node_name(node);
                self.web3_instances
                    .iter()
                    .filter(|instance| instance.node_name() == trusted_name)
                    .cloned()
                    .collect()
            },
            _ => self.web3_instances.clone(),
        }
    }

    /// One attempt of a read that each node answers at its own latest block: the node's latest block is read
    /// first, then `read` runs on that node at that block's height. The answers are compared by
    /// [`agree_with_tips`] with the timestamps of the tip blocks, so two nodes more than [`MAX_TIP_GAP_SECS`] apart
    /// leave the attempt undetermined, and an answer that proves itself is taken from any one node.
    async fn agree_at_own_tips<T, D, R, Fut>(
        &self,
        policy: &SwapEvidencePolicy,
        with_rest: bool,
        read: R,
        classify: impl Fn(&T) -> D,
        proves_itself: impl Fn(&T) -> bool,
    ) -> Agreement<AtTip<T>, AtHeight<D>>
    where
        D: PartialEq + fmt::Debug,
        R: Fn(Web3Instance, u64) -> Fut,
        Fut: StdFuture<Output = Result<T, String>>,
    {
        let instances = self.evidence_instances(policy, with_rest);
        let read = &read;
        agree_with_tips(
            policy,
            &instances,
            |instance| instance.node_name(),
            |instance| {
                let instance = instance.clone();
                async move {
                    let tip = instance.latest_block().await?;
                    let read = read(instance, tip.height).await?;
                    Ok(AtTip { tip, read })
                }
            },
            |answer| AtHeight {
                decision: classify(&answer.read),
                height: answer.tip.height,
            },
            |answer| proves_itself(&answer.read),
            |answer| Some(answer.tip.time),
            MAX_TIP_GAP_SECS,
        )
        .await
    }

    /// The error text of an undetermined read: the core's text of it ([`Undetermined::to_err_string`], which the
    /// swap engine recognizes by `swap_evidence::is_undetermined`), with the coin, the read, its subject and the
    /// two ways to fix it (CRD ch.51 R100 item 9).
    ///
    /// The text, which names what each node said, is logged at DEBUG every time. The first time a read of `subject`
    /// is undetermined, one WARN line is written as well. It names the kind only: the coin, the read, its subject,
    /// the reason and the two fixes, and no node, since a refund wait's warning carries no node identity
    /// (CRD ch.51 R80, R81).
    fn report_undetermined(&self, read: &str, subject: &str, undetermined: &Undetermined) -> String {
        const FIXES: &str = "To fix it, add a node on another host to the coin's node list or set swap_evidence_node";
        let head = format!(
            "[{}] the {} of {} is undetermined and the step waits",
            self.ticker(),
            read,
            subject
        );
        let text = format!("{head}: {}. {FIXES}", undetermined.to_err_string());
        if first_report(format!("{}:{}:{}", self.ticker(), read, subject)) {
            let kind = Undetermined {
                reason: undetermined.reason,
                per_node: Vec::new(),
            };
            warn!("{head}: {}. {FIXES}", kind.to_err_string());
        }
        debug!("{}", text);
        text
    }

    /// One attempt of the payment-state read: the contract's state of payment `id` and the payment transaction.
    async fn payment_read_once(
        &self,
        policy: &SwapEvidencePolicy,
        swap_contract_address: Address,
        id: &[u8],
        payment_hash: H256,
    ) -> Agreement<AtTip<PaymentRead>, AtHeight<PaymentDecision>> {
        // Nothing can decide this read without two nodes or a trusted one, so no node is asked.
        if matches!(policy, SwapEvidencePolicy::Insufficient { .. }) {
            return Agreement::Undetermined(Undetermined {
                reason: UndeterminedReason::TooFewAnswers,
                per_node: Vec::new(),
            });
        }
        let (from, id) = (self.my_address, id.to_vec());
        self.agree_at_own_tips(
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
    /// transaction, as two nodes agree on them, each at its own latest block, or as the trusted node shows them.
    ///
    /// The read is repeated until `wait_until` (UNIX seconds) while it is undetermined, and while the nodes agree
    /// that the contract does not know the payment yet. It is made at least once, and no attempt starts at or after
    /// `wait_until`.
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
            // What the read answers when the deadline has passed.
            let at_deadline = match attempt.await {
                Agreement::Agreed { answer, .. } => {
                    if answer.read.state != U256::from(PAYMENT_STATE_UNINITIALIZED) {
                        return Ok(answer.read);
                    }
                    Ok(answer.read)
                },
                Agreement::Undetermined(undetermined) => {
                    let subject = format!("payment {}", hex::encode(id));
                    Err(self.report_undetermined("payment state", &subject, &undetermined))
                },
            };
            // The wait never runs past the deadline: it is cut to the time that is left, and the deadline is
            // checked again after it, so no attempt starts at or after `wait_until`.
            let remaining = wait_until.saturating_sub(now_ms() / 1000);
            if remaining == 0 {
                return at_deadline;
            }
            Timer::sleep(PAYMENT_READ_RETRY_S.min(remaining as f64)).await;
            if now_ms() / 1000 >= wait_until {
                return at_deadline;
            }
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
    ) -> Result<Agreement<AtTip<SpendRead>, AtHeight<SpendDecision>>, String> {
        let secret_hash = self.payment_secret_hash_arg(payment_tx)?;
        let coin = self.clone();
        let key: PaymentKey = (swap_contract_address, id.to_vec());
        let payment_hash = payment_tx.tx_hash();
        // The nodes outside the pair (or beside the trusted node) are passed too: a claim whose secret matches the
        // payment's secret hash proves itself and is taken from any one of them (CRD ch.51 R100). This holds under
        // an `Insufficient` policy too: `agree` then asks the coin's nodes for such a claim only and ignores every
        // other answer, so "not spent" and a claim without the secret stay undetermined.
        Ok(self
            .agree_at_own_tips(
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
    /// (CRD ch.51 R100). "Not spent" is answered only when two nodes agree on it, each at its own tip, or the trusted
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
            Agreement::Agreed { answer, .. } => Ok(match answer.read {
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
            // The text of this attempt when it was undetermined.
            let mut undetermined_text = None;
            match attempt.await {
                Ok(Agreement::Agreed {
                    answer:
                        AtTip {
                            read: SpendRead::Claimed { tx, .. },
                            ..
                        },
                    ..
                }) => return Ok(TransactionEnum::from(*tx)),
                Ok(Agreement::Agreed { .. }) => (),
                Ok(Agreement::Undetermined(undetermined)) => {
                    let subject = format!("payment {}", hex::encode(id));
                    undetermined_text = Some(self.report_undetermined("spend state", &subject, &undetermined));
                },
                Err(e) => return TX_PLAIN_ERR!("{}", e),
            }

            if now_ms() / 1000 > wait_until {
                // A wait whose last read was undetermined says so, in the form the swap engine recognizes.
                return match undetermined_text {
                    Some(text) => TX_PLAIN_ERR!(
                        "Waited too long until {} for transaction {:?} to be spent; {}",
                        wait_until,
                        payment_tx,
                        text
                    ),
                    None => TX_PLAIN_ERR!(
                        "Waited too long until {} for transaction {:?} to be spent",
                        wait_until,
                        payment_tx,
                    ),
                };
            }
            Timer::sleep(poll_every).await;
        }
    }

    /// The refund gate under `policy` once the local clock `now` is past `locktime` (CRD ch.51 R100, §51.6E R88):
    /// each node's latest block timestamp is read, and the gate opens when two nodes (or the trusted node) show a
    /// time past the lock, whatever the two times are. Nodes that agree on a time not past the lock leave the gate
    /// closed with a wait of at most `EVM_TIP_RECHECK_MAX_SECS`.
    ///
    /// # Errors
    ///
    /// The read is undetermined. The swap engine recognizes the text, reads the gate again and holds the refund
    /// until the read is determined or the engine's own cap ends the step; the gate is never open on it.
    pub(crate) async fn agreed_refund_gate(
        &self,
        policy: &SwapEvidencePolicy,
        now: u64,
        locktime: u64,
    ) -> Result<CanRefundHtlc, String> {
        let outcome = if matches!(policy, SwapEvidencePolicy::Insufficient { .. }) {
            // Nothing can decide this read without two nodes or a trusted one, so no node is asked.
            Agreement::Undetermined(Undetermined {
                reason: UndeterminedReason::TooFewAnswers,
                per_node: Vec::new(),
            })
        } else {
            let instances = self.evidence_instances(policy, false);
            let reading = agree(
                policy,
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
            Agreement::Agreed { answer, .. } => Ok(evm_refund_eligibility(now, locktime, Some(answer))),
            Agreement::Undetermined(undetermined) => {
                let subject = format!("lock time {locktime}");
                Err(self.report_undetermined("chain time of the refund gate", &subject, &undetermined))
            },
        }
    }
}

#[cfg(test)]
mod agreed_reads_policy_tests {
    use super::*;

    /// A TRX or TRC20 coin never takes the EVM agreed reads, also when it answers an adopted policy.
    #[test]
    fn a_tron_family_coin_never_takes_the_evm_agreed_reads() {
        let policies = [
            SwapEvidencePolicy::TwoNodes {
                hosts: vec!["a.example".to_owned(), "b.example".to_owned()],
            },
            SwapEvidencePolicy::Trusted {
                node: "https://a.example".to_owned(),
            },
            SwapEvidencePolicy::Insufficient { hosts: 1 },
        ];
        for policy in policies {
            assert_eq!(agreed_reads_policy(true, policy.clone()), None);
            assert_eq!(agreed_reads_policy(false, policy.clone()), Some(policy));
        }
        assert_eq!(agreed_reads_policy(false, SwapEvidencePolicy::NotAdopted), None);
        assert_eq!(agreed_reads_policy(false, SwapEvidencePolicy::OwnDaemon), None);
    }
}
