// utxo_swap_agreement — the swap chain reads of an Electrum-backed UTXO, QTUM or BCH coin, judged by the R100
// agreement helper (CRD ch.51 §51.6G). A coin served by its own daemon, and a coin whose family has not adopted
// R100, read as before (one node).

use super::*;
use crate::swap_evidence::{agree, agree_with_tips, node_name, Agreement, SwapEvidencePolicy};
use crate::utxo::header_layout_fallback::HeaderReadPolicy;
use crate::utxo::rpc_clients::{ElectrumClient, SpentOutputInfo};

/// The servers of an Electrum client a read is agreed on, in the pool's own preference order.
pub(crate) struct EvidenceNodes {
    pub(crate) client: ElectrumClient,
    pub(crate) nodes: Vec<String>,
    /// The most blocks two servers' tips may differ by for their median time past to be compared: the pool's own
    /// tolerance for the coin (CRD ch.51 R100), the figure that also judges a server up to date.
    pub(crate) tip_gap: u64,
}

/// The servers to ask under `policy`, or `None` when the read goes the single-node way: a coin without an
/// Electrum client (own daemon, lightwalletd) and a policy that is not adopted or is the own daemon's.
pub(crate) fn evidence_nodes(coin: &UtxoCoinFields, policy: &SwapEvidencePolicy) -> Option<EvidenceNodes> {
    if matches!(policy, SwapEvidencePolicy::NotAdopted | SwapEvidencePolicy::OwnDaemon) {
        return None;
    }
    match &coin.rpc_client {
        UtxoRpcClientEnum::Electrum(client) => Some(EvidenceNodes {
            nodes: client.server_addrs(),
            tip_gap: client.tolerance_blocks(),
            client: client.clone(),
        }),
        _ => None,
    }
}

/// The readiness check before a swap starts (maintainer decision of 2026-11-25/26 under CRD ch.51 R100) of an
/// Electrum-backed or daemon-backed UTXO, QTUM or BCH coin under its effective `policy`. Over Electrum the servers are
/// judged by [`ElectrumClient::swap_evidence_ready`]; a coin read through the operator's own daemon is ready while the
/// daemon answers `getblockcount` within [`crate::swap_evidence::READY_PROBE_TIMEOUT_SECS`] (asked at most once per
/// window). A coin whose family has not adopted R100 is ready.
///
/// # Errors
///
/// The text of [`crate::swap_evidence::not_ready_text`], which names the coin and the kind only.
pub(crate) async fn swap_evidence_ready(coin: &UtxoCoinFields, policy: &SwapEvidencePolicy) -> Result<(), String> {
    swap_evidence_ready_as(&coin.conf.ticker, coin, policy).await
}

/// [`swap_evidence_ready`] of the nodes of `coin`, named in the text as `ticker` (an SLP token names itself while
/// its platform's servers are judged).
pub(crate) async fn swap_evidence_ready_as(
    ticker: &str,
    coin: &UtxoCoinFields,
    policy: &SwapEvidencePolicy,
) -> Result<(), String> {
    if matches!(policy, SwapEvidencePolicy::NotAdopted) {
        return Ok(());
    }
    match &coin.rpc_client {
        UtxoRpcClientEnum::Electrum(client) => client.swap_evidence_ready(ticker, policy, false).await,
        UtxoRpcClientEnum::Native(native) => {
            // The daemon is the node the operator trusts (R100 item 6), whatever form the stored policy takes.
            let policy = match policy {
                SwapEvidencePolicy::Insufficient { .. } => policy.clone(),
                _ => SwapEvidencePolicy::OwnDaemon,
            };
            own_daemon_ready(ticker, &policy, || {
                let native = native.clone();
                async move { native.get_block_count().compat().await.is_ok() }
            })
            .await
        },
        // A Light-mode ZHTLC coin judges its lightwalletd servers itself (`ZCoin::swap_evidence_ready`); no UTXO
        // coin is built over this backend.
        #[cfg(not(target_arch = "wasm32"))]
        UtxoRpcClientEnum::LightWalletD(_) => Ok(()),
    }
}

/// The readiness of a coin read through the operator's own daemon: `probe` asks the daemon a cheap call; its outcome
/// is kept for the window in the shared probe record under the coin's ticker.
pub(crate) async fn own_daemon_ready<F, Fut>(ticker: &str, policy: &SwapEvidencePolicy, probe: F) -> Result<(), String>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    crate::swap_evidence::evidence_ready(
        ticker,
        policy,
        &["own daemon"],
        |label| (*label).to_owned(),
        |_| crate::swap_evidence::HostReadiness::Unknown,
        crate::swap_evidence::shared_readiness_probes(),
        ticker,
        |_| probe(),
    )
    .await
}

/// The error text of a failed ask: never the server's own text, which may name an address.
fn ask_error(what: &str, label: &str, detail: impl std::fmt::Display) -> String {
    debug!("swap evidence: {what} on {label} failed: {detail}");
    format!("{what} failed")
}

/// What a server shows of the spend of a swap output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SpendDecision {
    NotSpent,
    Claimed(H256),
    Refunded(H256),
    /// Spent by a script of neither shape.
    Unclassified(H256),
}

pub(crate) fn spend_decision(spend: &Option<SpentOutputInfo>, tx_hash_algo: TxHashAlgo) -> SpendDecision {
    let Some(info) = spend else {
        return SpendDecision::NotSpent;
    };
    let mut tx = info.spending_tx.clone();
    tx.tx_hash_algo = tx_hash_algo;
    let txid = tx.hash();
    let kind = tx
        .inputs
        .get(info.input_index)
        .and_then(|input| classify_swap_spend(&input.script_sig.clone().into()));
    match kind {
        Some(SwapSpendKind::Spent) => SpendDecision::Claimed(txid),
        Some(SwapSpendKind::Refunded) => SpendDecision::Refunded(txid),
        None => SpendDecision::Unclassified(txid),
    }
}

/// A claim whose secret hashes to the swap's secret hash proves itself (R100).
fn spend_proves_itself(spend: &Option<SpentOutputInfo>, secret_hash: Option<&[u8]>) -> bool {
    let (Some(info), Some(secret_hash)) = (spend, secret_hash) else {
        return false;
    };
    extract_secret_from_script_sigs(
        secret_hash,
        info.spending_tx
            .inputs
            .iter()
            .map(|input| input.script_sig.clone().into()),
    )
    .is_ok()
}

/// A claim proves itself from the payment alone, with no secret hash known (R100 item 3): the input spends the
/// payment's HTLC outpoint, the redeem script it pushes hashes (hash160) to the payment output's P2SH, and the
/// secret it pushes hashes to the hash the redeem script commits to (`OP_HASH160 <hash> OP_EQUALVERIFY`).
fn claim_proves_itself_from_payment(
    spend: &Option<SpentOutputInfo>,
    payment: &PaymentOutputs,
    output_index: usize,
) -> bool {
    let Some(info) = spend else {
        return false;
    };
    let Some(input) = info.spending_tx.inputs.get(info.input_index) else {
        return false;
    };
    if input.previous_output.hash != payment.tx_hash || input.previous_output.index != output_index as u32 {
        return false;
    }
    let script: Script = input.script_sig.clone().into();
    // `<sig> <secret> OP_0 <redeem script>`: the claim path.
    if classify_swap_spend(&script) != Some(SwapSpendKind::Spent) {
        return false;
    }
    let push = |index: usize| match script.get_instruction(index) {
        Some(Ok(instruction)) => instruction.data.map(|data| data.to_vec()),
        _ => None,
    };
    let (Some(secret), Some(redeem)) = (push(1), push(3)) else {
        return false;
    };
    if secret.len() != 32 {
        return false;
    }
    let redeem_p2sh = Builder::build_p2sh(&dhash160(&redeem).into()).to_bytes();
    if redeem_p2sh != payment.spent_script_pubkey {
        return false;
    }
    // The hash the redeem script commits to: the push that follows `OP_HASH160`.
    let redeem_script = Script::from(redeem);
    let mut committed = None;
    let mut previous_was_hash160 = false;
    for instruction in redeem_script.iter() {
        let Ok(instruction) = instruction else {
            return false;
        };
        if previous_was_hash160 {
            committed = instruction.data.map(|data| data.to_vec());
            break;
        }
        previous_was_hash160 = instruction.opcode == Opcode::OP_HASH160;
    }
    committed.map_or(false, |hash| hash.as_slice() == dhash160(&secret).as_slice())
}

/// The spend of the output `output_index` of the payment, agreed on the servers under `policy`. `secret_hash` is
/// given for the reads that serve the secret.
pub(crate) async fn agreed_spend(
    tx_hash_algo: TxHashAlgo,
    policy: &SwapEvidencePolicy,
    servers: &EvidenceNodes,
    payment: &PaymentOutputs,
    output_index: usize,
    secret_hash: Option<&[u8]>,
) -> Result<Option<SpentOutputInfo>, String> {
    let client = &servers.client;
    let agreement = agree(
        policy,
        &servers.nodes,
        |addr| node_name(addr),
        |addr| {
            let addr = addr.clone();
            async move {
                client
                    .find_output_spend_from(&addr, payment.tx_hash, &payment.spent_script_pubkey, output_index)
                    .await
                    .map_err(|e| ask_error("the search for a spend", &node_name(&addr), e))
            }
        },
        |spend| spend_decision(spend, tx_hash_algo),
        |spend| match secret_hash {
            // The secret hash of the swap is known: the claim's secret must hash to it.
            Some(_) => spend_proves_itself(spend, secret_hash),
            // Not known (the wait for the spend): the claim proves itself from the payment alone.
            None => claim_proves_itself_from_payment(spend, payment, output_index),
        },
    )
    .await;
    match agreement {
        Agreement::Agreed { answer, .. } => Ok(answer),
        Agreement::Undetermined(undetermined) => Err(undetermined.to_err_string()),
    }
}

/// The spend of the output `output_index` of `payment`, agreed on the servers under `policy`, for a swap output
/// that is not an HTLC of the legacy shape (the version 2 funding and the version 2 taker payment). The decision of a
/// node is the id of the spending transaction, or none; two nodes agree when both show no spend or both show the same
/// spending transaction. `proves_itself` is `true` for a spend a server cannot be wrong about (a claim whose secret
/// hashes to the swap's secret hash); it is taken alone, at once.
pub(crate) async fn agreed_output_spend(
    tx_hash_algo: TxHashAlgo,
    policy: &SwapEvidencePolicy,
    servers: &EvidenceNodes,
    payment: &PaymentOutputs,
    output_index: usize,
    proves_itself: impl Fn(&Option<SpentOutputInfo>) -> bool,
) -> Result<Option<SpentOutputInfo>, String> {
    let client = &servers.client;
    let agreement = agree(
        policy,
        &servers.nodes,
        |addr| node_name(addr),
        |addr| {
            let addr = addr.clone();
            async move {
                client
                    .find_output_spend_from(&addr, payment.tx_hash, &payment.spent_script_pubkey, output_index)
                    .await
                    .map_err(|e| ask_error("the search for a spend", &node_name(&addr), e))
            }
        },
        |spend: &Option<SpentOutputInfo>| {
            spend.as_ref().map(|info| {
                let mut tx = info.spending_tx.clone();
                tx.tx_hash_algo = tx_hash_algo;
                tx.hash()
            })
        },
        proves_itself,
    )
    .await;
    match agreement {
        Agreement::Agreed { answer, .. } => Ok(answer),
        Agreement::Undetermined(undetermined) => Err(undetermined.to_err_string()),
    }
}

/// The look on every usable server (CRD ch.51 R31a with R100): the agreed pair search of [`agreed_spend`], and then
/// every server of the list is asked for a PROOF ONLY, a claim whose secret hashes to the swap's secret hash. A
/// proof found on any server is returned; every other answer of the sweep is ignored. Without a proof the result of
/// the pair search stands, whatever it is (an agreed spend, none, or an undetermined read).
pub(crate) async fn agreed_spend_with_proof_sweep(
    tx_hash_algo: TxHashAlgo,
    policy: &SwapEvidencePolicy,
    servers: &EvidenceNodes,
    payment: &PaymentOutputs,
    output_index: usize,
    secret_hash: &[u8],
) -> Result<Option<SpentOutputInfo>, String> {
    let pair = agreed_spend(tx_hash_algo, policy, servers, payment, output_index, Some(secret_hash)).await;
    if matches!(&pair, Ok(spend) if spend_proves_itself(spend, Some(secret_hash))) {
        return pair;
    }
    let client = &servers.client;
    let mut sweep: futures::stream::FuturesUnordered<_> = servers
        .nodes
        .iter()
        .map(|addr| async move {
            match client
                .find_output_spend_from(addr, payment.tx_hash, &payment.spent_script_pubkey, output_index)
                .await
            {
                Ok(spend) => Some(spend).filter(|spend| spend_proves_itself(spend, Some(secret_hash))),
                Err(e) => {
                    debug!("swap evidence: the proof sweep on {} failed: {}", node_name(addr), e);
                    None
                },
            }
        })
        .collect();
    while let Some(found) = futures::StreamExt::next(&mut sweep).await {
        if let Some(proof) = found {
            return Ok(proof);
        }
    }
    pair
}

/// The raw payment transaction of `txid`, agreed on the servers: the decision of a node is whether its bytes are
/// the bytes of `expected`.
pub(crate) async fn agreed_payment_bytes(
    policy: &SwapEvidencePolicy,
    servers: &EvidenceNodes,
    txid: &H256Json,
    matches_expected: impl Fn(&[u8]) -> bool,
) -> Result<BytesJson, String> {
    let client = &servers.client;
    let agreement = agree(
        policy,
        &servers.nodes,
        |addr| node_name(addr),
        |addr| {
            let addr = addr.clone();
            async move {
                client
                    .transaction_get_from(&addr, txid)
                    .compat()
                    .await
                    .map_err(|e| ask_error("the payment fetch", &node_name(&addr), e))
            }
        },
        |bytes: &BytesJson| matches_expected(&bytes.0),
        |_| false,
    )
    .await;
    match agreement {
        Agreement::Agreed { answer, .. } => Ok(answer),
        Agreement::Undetermined(undetermined) => Err(undetermined.to_err_string()),
    }
}

/// The first transaction paying `script_hash` (hex), agreed on the servers: the decision is its id, or none.
pub(crate) async fn agreed_payment_sent(
    policy: &SwapEvidencePolicy,
    servers: &EvidenceNodes,
    script_hash: &str,
) -> Result<Option<(H256Json, BytesJson)>, String> {
    let client = &servers.client;
    let agreement = agree(
        policy,
        &servers.nodes,
        |addr| node_name(addr),
        |addr| {
            let addr = addr.clone();
            async move {
                client
                    .first_history_tx_from(&addr, script_hash)
                    .await
                    .map_err(|e| ask_error("the already-sent read", &node_name(&addr), e))
            }
        },
        |found: &Option<(H256Json, BytesJson)>| found.as_ref().map(|(txid, _)| *txid),
        |_| false,
    )
    .await;
    match agreement {
        Agreement::Agreed { answer, .. } => Ok(answer),
        Agreement::Undetermined(undetermined) => Err(undetermined.to_err_string()),
    }
}

/// Whether the transaction has at least `required` confirmations, agreed on the servers.
pub(crate) async fn agreed_confirmed(
    policy: &SwapEvidencePolicy,
    servers: &EvidenceNodes,
    txid: &H256Json,
    required: u32,
    requires_notarization: bool,
) -> Result<bool, String> {
    let client = &servers.client;
    let agreement = agree(
        policy,
        &servers.nodes,
        |addr| node_name(addr),
        |addr| {
            let addr = addr.clone();
            async move {
                client
                    .transaction_get_verbose_from(&addr, txid)
                    .compat()
                    .await
                    .map(|t| {
                        if requires_notarization {
                            t.confirmations
                        } else {
                            t.rawconfirmations.unwrap_or(t.confirmations)
                        }
                    })
                    .map_err(|e| ask_error("the confirmation count", &node_name(&addr), e))
            }
        },
        |confirmations| *confirmations >= required,
        |_| false,
    )
    .await;
    match agreement {
        Agreement::Agreed { decision, .. } => Ok(decision),
        Agreement::Undetermined(undetermined) => Err(undetermined.to_err_string()),
    }
}

/// The median time past agreed on the servers, each read at its server's own tip; two tips more than
/// [`servers.tip_gap`] apart are undetermined. The decision of a node is `is_past(mtp)`; the answer is the
/// first agreeing node's tip and median time past.
pub(crate) async fn agreed_mtp(
    coin: &UtxoCoinFields,
    policy: &SwapEvidencePolicy,
    servers: &EvidenceNodes,
    is_past: impl Fn(u32) -> bool,
) -> Result<u32, String> {
    let client = &servers.client;
    let header_policy = HeaderReadPolicy::for_conf(&coin.conf);
    let header_policy = &header_policy;
    let count = coin.conf.mtp_block_count;
    let agreement = agree_with_tips(
        policy,
        &servers.nodes,
        |addr| node_name(addr),
        |addr| {
            let addr = addr.clone();
            async move {
                client
                    .median_time_past_from(&addr, count, header_policy)
                    .await
                    .map_err(|e| ask_error("the median time past", &node_name(&addr), e))
            }
        },
        |read| is_past(read.mtp),
        |_| false,
        |read| Some(read.tip),
        servers.tip_gap,
    )
    .await;
    match agreement {
        Agreement::Agreed { answer, .. } => Ok(answer.mtp),
        Agreement::Undetermined(undetermined) => Err(undetermined.to_err_string()),
    }
}
