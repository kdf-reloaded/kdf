//! Evidence reads of the watch side (CRD ch.51 R100, ch.21 D21.B8): the solidified chain time and the solidified
//! `payments(id)` record, asked of each distinct node of the coin's node list on its own, and the agreement of
//! their answers.
//!
//! # Invariants
//!
//! - An answer is the answer of one named node: no failover stands in for a node that does not answer.
//! - The agreement is [`crate::swap_evidence::agree`]'s: this module supplies only what is asked of a node
//!   ([`read_node`]) and the decision an answer falls into. The distinct-host and no-outvoting rules, the trusted
//!   node and the "fewer than two answers" outcome are the engine's. No error is ever mapped to a state.

use super::REVEALING_CLAIM_LOCK_MARGIN_SECS;
use crate::eth::tron::api::{GetNowBlockResponse, TriggerConstantContractResponse, TriggerConstantDataRequest,
                            TronApiClient, TronApiError};
use crate::eth::tron::swap::{decode_payments_output, encode_payments_query, PaymentRecord, PaymentState, TronSwapError};
use crate::eth::tron::TronAddress;
use crate::swap_evidence::{self, node_name, Agreement, SwapEvidencePolicy, Undetermined, UndeterminedReason};
use common::log::debug;
use serde::Serialize;

/// The solidified head block of a node (CRD ch.21 D21.B8: only blocks the super-representative majority confirmed).
const SOLIDITY_NOW_BLOCK: &str = "/walletsolidity/getnowblock";
/// The constant-contract call on solidified state.
const SOLIDITY_TRIGGER: &str = "/walletsolidity/triggerconstantcontract";

#[derive(Serialize)]
struct Empty {}

/// What one node answered: its solidified head time in seconds and the solidified payment record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::eth::tron::swap) struct SolidifiedRead {
    pub chain_time_s: u64,
    pub record: PaymentRecord,
}

fn check_head(block: &GetNowBlockResponse) -> Result<(), TronApiError> {
    if block.block_header.raw_data.timestamp > 0 {
        Ok(())
    } else {
        Err(TronApiError::InvalidResponse(
            "getnowblock answer has a non-positive timestamp".to_owned(),
        ))
    }
}

fn check_read(answer: &TriggerConstantContractResponse) -> Result<(), TronApiError> {
    match answer.constant_result.as_ref().and_then(|results| results.first()) {
        Some(first) if !first.is_empty() && hex::decode(first).is_ok() => Ok(()),
        _ => Err(TronApiError::InvalidResponse(
            "constant_result is absent, empty or not hex".to_owned(),
        )),
    }
}

/// The payment record a `payments(id)` answer carries.
///
/// # Errors
///
/// [`TronSwapError::Node`] for an answer without a result, and the errors of [`decode_payments_output`].
pub(in crate::eth::tron::swap) fn record_of(
    answer: &TriggerConstantContractResponse,
) -> Result<PaymentRecord, TronSwapError> {
    let bad = |reason: &str| TronSwapError::Node(TronApiError::InvalidResponse(reason.to_owned()));
    let first = answer
        .constant_result
        .as_ref()
        .and_then(|results| results.first())
        .ok_or_else(|| bad("the payments answer carries no result"))?;
    let bytes = hex::decode(first).map_err(|_| bad("the payments answer is not hex"))?;
    decode_payments_output(&bytes)
}

/// The read of one node: its solidified head time and its solidified `payments(id)`.
async fn read_node(
    api: &TronApiClient,
    index: usize,
    owner: &TronAddress,
    contract: &TronAddress,
    id: &[u8; 32],
) -> Result<SolidifiedRead, TronSwapError> {
    let head: GetNowBlockResponse = api
        .post_to_node(index, SOLIDITY_NOW_BLOCK, &Empty {}, check_head)
        .await
        .map_err(TronSwapError::Node)?;
    let chain_time_s = u64::try_from(head.block_header.raw_data.timestamp / 1000).map_err(|_| {
        TronSwapError::Node(TronApiError::InvalidResponse(
            "getnowblock answer has a negative time".to_owned(),
        ))
    })?;
    let request = TriggerConstantDataRequest {
        owner_address: owner.to_base58(),
        contract_address: contract.to_base58(),
        data: hex::encode(encode_payments_query(id)?),
        call_value: 0,
        visible: true,
    };
    let answer: TriggerConstantContractResponse = api
        .post_to_node(index, SOLIDITY_TRIGGER, &request, check_read)
        .await
        .map_err(TronSwapError::Node)?;
    Ok(SolidifiedRead {
        chain_time_s,
        record: record_of(&answer)?,
    })
}

/// The error of an undetermined evidence read: the step waits. Its text is the core's text for the read
/// ([`Undetermined::to_err_string`]: the marker, the reason and what each node said, nodes named as `host[:port]`),
/// unchanged and first, so the swap engine's `is_undetermined` recognizes it (CRD ch.51 R100 item 9, ch.21
/// R21.11D.1), followed by the fix for the reason. The engine warns once per step; the per-node answers stay at DEBUG.
fn undetermined(why: Undetermined) -> TronSwapError {
    debug!("{why}");
    let fix = match why.reason {
        UndeterminedReason::TrustedNodeSilent => {
            "the node named by swap_evidence_node did not answer; check that node, or remove swap_evidence_node to \
             use two nodes on distinct hosts"
        },
        UndeterminedReason::TooFewAnswers | UndeterminedReason::Disagree | UndeterminedReason::TipGap => {
            "two TRON nodes on distinct hosts must agree; add a node on another host to the coin's node list or set \
             swap_evidence_node to the node to trust"
        },
    };
    TronSwapError::Undetermined(format!("{}: {fix}", why.to_err_string()))
}

/// The answer the nodes of the policy agree on, by the decision `classify` reduces each answer to (CRD ch.51 R100
/// item 1: each answer on its own satisfies the step's condition). `ask` reads the node at an index.
///
/// # Errors
///
/// [`TronSwapError::Undetermined`], whose text carries the core's marker and names the fix.
async fn agreed_answer<A, K, Fut>(
    api: &TronApiClient,
    policy: &SwapEvidencePolicy,
    ask: impl Fn(usize) -> Fut,
    classify: impl Fn(&A) -> K,
) -> Result<A, TronSwapError>
where
    K: PartialEq + std::fmt::Debug,
    Fut: std::future::Future<Output = Result<A, TronSwapError>>,
{
    let nodes = api.node_urls();
    let ask = |url: &String| {
        let pending = api.index_of_url(url).map(&ask);
        async move {
            match pending {
                Some(pending) => pending.await.map_err(|e| e.to_string()),
                None => Err("no such node".to_owned()),
            }
        }
    };
    // A solidified read never proves itself.
    match swap_evidence::agree(policy, &nodes, |url: &String| node_name(url), ask, classify, |_| false).await {
        Agreement::Agreed { answer, .. } => Ok(answer),
        Agreement::Undetermined(why) => Err(undetermined(why)),
    }
}

/// The payment record the nodes agree on: the same state, the same stored hash and the same lock time.
pub(in crate::eth::tron::swap) async fn agreed_payment(
    api: &TronApiClient,
    policy: &SwapEvidencePolicy,
    owner: &TronAddress,
    contract: &TronAddress,
    id: &[u8; 32],
) -> Result<PaymentRecord, TronSwapError> {
    let read = agreed_answer(
        api,
        policy,
        |index| read_node(api, index, owner, contract, id),
        |read: &SolidifiedRead| (read.record.state, read.record.payment_hash, read.record.lock_time),
    )
    .await?;
    Ok(read.record)
}

/// The payment state the nodes agree on.
pub(in crate::eth::tron::swap) async fn agreed_state(
    api: &TronApiClient,
    policy: &SwapEvidencePolicy,
    owner: &TronAddress,
    contract: &TronAddress,
    id: &[u8; 32],
) -> Result<PaymentState, TronSwapError> {
    let read = agreed_answer(
        api,
        policy,
        |index| read_node(api, index, owner, contract, id),
        |read: &SolidifiedRead| read.record.state,
    )
    .await?;
    Ok(read.record.state)
}

/// The solidified head time of one node, in seconds.
async fn head_time_of_node(api: &TronApiClient, index: usize) -> Result<u64, TronSwapError> {
    let head: GetNowBlockResponse = api
        .post_to_node(index, SOLIDITY_NOW_BLOCK, &Empty {}, check_head)
        .await
        .map_err(TronSwapError::Node)?;
    u64::try_from(head.block_header.raw_data.timestamp / 1000).map_err(|_| {
        TronSwapError::Node(TronApiError::InvalidResponse(
            "getnowblock answer has a negative time".to_owned(),
        ))
    })
}

/// The solidified head time, in seconds, of the nodes that agree on whether it is at or past `lock_time` (the
/// refund gate's read: CRD ch.51 R88, R100 item 1).
pub(in crate::eth::tron::swap) async fn agreed_chain_time(
    api: &TronApiClient,
    policy: &SwapEvidencePolicy,
    lock_time: u64,
) -> Result<u64, TronSwapError> {
    agreed_answer(
        api,
        policy,
        |index| head_time_of_node(api, index),
        |time: &u64| *time >= lock_time,
    )
    .await
}

/// The read of the claim that reveals the secret: the nodes must agree on the payment state and on whether the
/// solidified chain time plus [`REVEALING_CLAIM_LOCK_MARGIN_SECS`] is below the stored lock time. The answer carries
/// both the chain time and the record.
pub(in crate::eth::tron::swap) async fn agreed_claim_read(
    api: &TronApiClient,
    policy: &SwapEvidencePolicy,
    owner: &TronAddress,
    contract: &TronAddress,
    id: &[u8; 32],
) -> Result<SolidifiedRead, TronSwapError> {
    agreed_answer(
        api,
        policy,
        |index| read_node(api, index, owner, contract, id),
        |read: &SolidifiedRead| {
            (
                read.record.state,
                read.chain_time_s.saturating_add(REVEALING_CLAIM_LOCK_MARGIN_SECS) < read.record.lock_time,
            )
        },
    )
    .await
}
