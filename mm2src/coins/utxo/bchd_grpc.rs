/// https://bchd.cash/
/// https://bchd.fountainhead.cash/
use super::bchd_pb::*;
use crate::swap_evidence::{agree, distinct_hosts, evidence_ready, node_name, relax_single_host,
                           shared_readiness_probes, Agreement, HostReadiness};
use crate::utxo::slp::SlpUnspent;
use crate::SwapEvidencePolicy;
use chain::OutPoint;
use derive_more::Display;
use futures::future::join_all;
use futures::FutureExt;
use get_slp_trusted_validation_response::validity_result::ValidityResultType;
use keys::hash::H256;
use mm2_err_handle::prelude::*;
use mm2_net::grpc_web::{post_grpc_web, PostGrpcWebErr};

#[derive(Debug, Display)]
#[display(fmt = "Error {} on request to the node {}", err, to_url)]
pub struct GrpcWebMultiUrlReqErr {
    /// The label of the node (`host[:port]`), never its URL.
    to_url: String,
    /// The kind of the failure only: the error itself may name the URL, which may carry a credential.
    err: String,
}

fn grpc_web_err_kind(err: &PostGrpcWebErr) -> &'static str {
    match err {
        PostGrpcWebErr::InvalidRequest(_) => "InvalidRequest",
        PostGrpcWebErr::EncodeBody(_) => "EncodeBody",
        PostGrpcWebErr::DecodeBody(_) => "DecodeBody",
        PostGrpcWebErr::Transport { .. } => "Transport",
        PostGrpcWebErr::Internal(_) => "Internal",
    }
}

/// This fn will simply return Ok() if urls are empty.
/// It is intended behaviour to make "unsafe" mode possible for BCH.
#[allow(clippy::needless_lifetimes)]
async fn grpc_web_multi_url_request<'a, Req, Res, Url>(
    urls: &'a [Url],
    req: &'a Req,
) -> Result<Vec<(&'a Url, Res)>, MmError<GrpcWebMultiUrlReqErr>>
where
    Req: prost::Message + Send + 'static,
    Res: prost::Message + Default + Send + 'static,
    Url: AsRef<str>,
{
    let futures = urls
        .iter()
        .map(|url| post_grpc_web::<_, Res>(url.as_ref(), req).map(move |res| (url, res)));

    join_all(futures)
        .await
        .into_iter()
        .map(|(url, response)| {
            Ok((
                url,
                response.mm_err(|err| GrpcWebMultiUrlReqErr {
                    to_url: node_name(url.as_ref()),
                    err: grpc_web_err_kind(&err).to_owned(),
                })?,
            ))
        })
        .collect()
}

#[derive(Debug, Display)]
pub enum ValidateSlpUtxosErrKind {
    MultiReqErr(GrpcWebMultiUrlReqErr),
    #[display(fmt = "{}", _0)]
    Request(String),
    #[display(fmt = "{}", _0)]
    Undetermined(String),
    #[display(fmt = "Expected {} token id, but got {}", expected, actual)]
    UnexpectedTokenId {
        expected: H256,
        actual: H256,
    },
    #[display(
        fmt = "Unexpected validity_result {:?} for unspent {:?}",
        validity_result,
        for_unspent
    )]
    UnexpectedValidityResultType {
        for_unspent: SlpUnspent,
        validity_result: Option<ValidityResultType>,
    },
    #[display(fmt = "Unexpected utxo {:?} in response", outpoint)]
    UnexpectedUtxoInResponse {
        outpoint: OutPoint,
    },
}

#[derive(Debug, Display)]
#[display(fmt = "Error {} on request to the node {}", kind, to_url)]
pub struct ValidateSlpUtxosErr {
    to_url: String,
    kind: ValidateSlpUtxosErrKind,
}

impl From<GrpcWebMultiUrlReqErr> for ValidateSlpUtxosErr {
    fn from(err: GrpcWebMultiUrlReqErr) -> Self {
        ValidateSlpUtxosErr {
            to_url: err.to_url.clone(),
            kind: ValidateSlpUtxosErrKind::MultiReqErr(err),
        }
    }
}

/// The label of a bchd node in an error or a log line: `host[:port]`, never the URL (it may carry a credential).
fn bchd_label(url: &str) -> String { node_name(url) }

fn validation_request(utxos: &[SlpUnspent]) -> GetSlpTrustedValidationRequest {
    let queries = utxos
        .iter()
        .map(|utxo| get_slp_trusted_validation_request::Query {
            prev_out_hash: utxo.bch_unspent.outpoint.hash.take().into(),
            prev_out_vout: utxo.bch_unspent.outpoint.index,
            graphsearch_valid_hashes: Vec::new(),
        })
        .collect();
    GetSlpTrustedValidationRequest {
        queries,
        include_graphsearch_count: false,
    }
}

/// Judges the answer of one bchd node (`label`) about `utxos`.
fn judge_validation(
    label: &str,
    response: GetSlpTrustedValidationResponse,
    utxos: &[SlpUnspent],
    token_id: &H256,
) -> Result<(), MmError<ValidateSlpUtxosErr>> {
    for validation_result in response.results {
        let actual_token_id = validation_result.token_id.as_slice().into();
        if actual_token_id != *token_id {
            return MmError::err(ValidateSlpUtxosErr {
                to_url: label.to_owned(),
                kind: ValidateSlpUtxosErrKind::UnexpectedTokenId {
                    expected: *token_id,
                    actual: actual_token_id,
                },
            });
        }

        let outpoint = OutPoint {
            hash: validation_result.prev_out_hash.as_slice().into(),
            index: validation_result.prev_out_vout,
        };

        let initial_unspent = utxos
            .iter()
            .find(|unspent| unspent.bch_unspent.outpoint == outpoint)
            .or_mm_err(|| ValidateSlpUtxosErr {
                to_url: label.to_owned(),
                kind: ValidateSlpUtxosErrKind::UnexpectedUtxoInResponse { outpoint },
            })?;

        match validation_result.validity_result_type {
            Some(ValidityResultType::V1TokenAmount(slp_amount)) => {
                if slp_amount != initial_unspent.slp_amount {
                    return MmError::err(ValidateSlpUtxosErr {
                        to_url: label.to_owned(),
                        kind: ValidateSlpUtxosErrKind::UnexpectedValidityResultType {
                            for_unspent: initial_unspent.clone(),
                            validity_result: validation_result.validity_result_type,
                        },
                    });
                }
            },
            _ => {
                return MmError::err(ValidateSlpUtxosErr {
                    to_url: label.to_owned(),
                    kind: ValidateSlpUtxosErrKind::UnexpectedValidityResultType {
                        for_unspent: initial_unspent.clone(),
                        validity_result: validation_result.validity_result_type,
                    },
                })
            },
        }
    }
    Ok(())
}

/// Today's read: every bchd node is asked and every node must show the outputs valid.
pub async fn validate_slp_utxos(
    bchd_urls: &[impl AsRef<str>],
    utxos: &[SlpUnspent],
    token_id: &H256,
) -> Result<(), MmError<ValidateSlpUtxosErr>> {
    let request = validation_request(utxos);
    let urls: Vec<_> = bchd_urls
        .iter()
        .map(|url| url.as_ref().to_owned() + "/pb.bchrpc/GetSlpTrustedValidation")
        .collect();
    let responses: Vec<(_, GetSlpTrustedValidationResponse)> =
        grpc_web_multi_url_request(&urls, &request).await.mm_err(Into::into)?;
    for (url, response) in responses {
        judge_validation(&bchd_label(url), response, utxos, token_id)?;
    }
    Ok(())
}

/// The policy of an SLP validity read under the single-host switch (`swap_evidence_allow_single_host`, maintainer
/// decision of 2026-11-24): `Trusted` on the first entry of the one host when `allow_single_host` is set and the
/// CONFIGURED `bchd_urls` name exactly one distinct host (two ports or schemes of one host are one host), else `None`.
/// So two or more hosts stay strict, and a list with no host stays `Insufficient`. The one rule both the token's
/// stored policy and its validity read take.
pub(crate) fn single_bchd_host_policy(bchd_urls: &[String], allow_single_host: bool) -> Option<SwapEvidencePolicy> {
    let insufficient = SwapEvidencePolicy::Insufficient {
        hosts: distinct_hosts(bchd_urls).len(),
    };
    match relax_single_host(insufficient, None, bchd_urls, allow_single_host) {
        (trusted, true) => Some(trusted),
        (_, false) => None,
    }
}

/// How the validity of the counterparty's token output is read (CRD ch.51 R100): the legacy all-nodes read while the
/// family has not adopted R100; the one bchd host alone when the single-host switch relaxed the token
/// ([`single_bchd_host_policy`]); else two distinct bchd hosts must agree. An answer counts for the readiness check
/// of the bchd nodes of the BCH platform `platform`.
pub async fn validate_slp_utxos_under(
    platform: &str,
    policy: &SwapEvidencePolicy,
    bchd_urls: &[String],
    allow_single_host: bool,
    utxos: &[SlpUnspent],
    token_id: &H256,
) -> Result<(), MmError<ValidateSlpUtxosErr>> {
    let request = validation_request(utxos);
    let scope = bchd_readiness_scope(platform);
    validate_slp_utxos_with(policy, bchd_urls, allow_single_host, utxos, token_id, |url: &String| {
        let request = &request;
        let scope = &scope;
        let endpoint = url.clone() + "/pb.bchrpc/GetSlpTrustedValidation";
        let label = bchd_label(url);
        async move {
            let response = post_grpc_web::<_, GetSlpTrustedValidationResponse>(&endpoint, request)
                .await
                // The text of the transport error may name the URL; the node's label is added by `agree`.
                .map_err(|_| "the request failed".to_owned())?;
            // The readiness check before a swap starts counts this as an answer of the node.
            shared_readiness_probes().note_answer(scope, &label, common::now_float());
            Ok(response)
        }
    })
    .await
}

/// [`validate_slp_utxos_under`] with the per-node request `ask` given by the caller (a scripted node in tests).
pub(crate) async fn validate_slp_utxos_with<F, Fut>(
    policy: &SwapEvidencePolicy,
    bchd_urls: &[String],
    allow_single_host: bool,
    utxos: &[SlpUnspent],
    token_id: &H256,
    ask: F,
) -> Result<(), MmError<ValidateSlpUtxosErr>>
where
    F: Fn(&String) -> Fut,
    Fut: std::future::Future<Output = Result<GetSlpTrustedValidationResponse, String>>,
{
    // The bchd nodes are read by their own list: a platform node (an Electrum entry) or the platform's daemon is not
    // a bchd node, so every adopted policy but `Insufficient` is read as two nodes, unless the single-host switch
    // trusts the one configured bchd host (matched by its label, as `agree` matches a trusted node).
    let bchd_policy = match (policy, single_bchd_host_policy(bchd_urls, allow_single_host)) {
        (SwapEvidencePolicy::NotAdopted, _) => {
            return validate_slp_utxos_legacy(bchd_urls, utxos, token_id, &ask).await
        },
        (_, Some(trusted)) => trusted,
        (SwapEvidencePolicy::Insufficient { hosts }, None) => SwapEvidencePolicy::Insufficient { hosts: *hosts },
        (_, None) => SwapEvidencePolicy::TwoNodes {
            hosts: distinct_hosts(bchd_urls),
        },
    };
    let agreement = agree(
        &bchd_policy,
        bchd_urls,
        |url| bchd_label(url),
        |url| {
            let answer = ask(url);
            let label = bchd_label(url);
            async move {
                let response = answer.await?;
                Ok(judge_validation(&label, response, utxos, token_id).map_err(MmError::into_inner))
            }
        },
        |answer: &Result<(), ValidateSlpUtxosErr>| match answer {
            Ok(()) => ValidityDecision::Valid,
            Err(_) => ValidityDecision::Invalid,
        },
        |_| false,
    )
    .await;
    match agreement {
        Agreement::Agreed { answer: Ok(()), .. } => Ok(()),
        Agreement::Agreed { answer: Err(err), .. } => MmError::err(err),
        Agreement::Undetermined(undetermined) => MmError::err(ValidateSlpUtxosErr {
            to_url: "the bchd nodes".to_owned(),
            kind: ValidateSlpUtxosErrKind::Undetermined(undetermined.to_err_string()),
        }),
    }
}

/// The scope of the bchd nodes of the BCH platform `platform` in the shared readiness probe record: two platforms
/// whose bchd nodes share a host keep apart records.
pub(crate) fn bchd_readiness_scope(platform: &str) -> String { format!("bchd:{platform}") }

/// One readiness probe of the bchd node `url`: a `GetBlockchainInfo` whose whole exchange is bounded by
/// [`crate::swap_evidence::READY_PROBE_TIMEOUT_SECS`] inside the task that runs it, so a node that accepts and never
/// answers keeps no task and no socket past the bound. `true` when the node answered.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) async fn bchd_readiness_probe(url: &str) -> bool {
    use crate::swap_evidence::READY_PROBE_TIMEOUT_SECS;
    use mm2_net::grpc_web::post_grpc_web_bounded;

    let endpoint = url.to_owned() + "/pb.bchrpc/GetBlockchainInfo";
    post_grpc_web_bounded::<_, GetBlockchainInfoResponse>(
        &endpoint,
        &GetBlockchainInfoRequest {},
        READY_PROBE_TIMEOUT_SECS,
    )
    .await
    .is_ok()
}

/// One readiness probe of the bchd node `url` in the browser: the fetch ends with the check's bound around it.
#[cfg(target_arch = "wasm32")]
pub(crate) async fn bchd_readiness_probe(url: &str) -> bool {
    let endpoint = url.to_owned() + "/pb.bchrpc/GetBlockchainInfo";
    post_grpc_web::<_, GetBlockchainInfoResponse>(&endpoint, &GetBlockchainInfoRequest {})
        .await
        .is_ok()
}

/// The readiness check before a swap starts (maintainer decision of 2026-11-25/26 under CRD ch.51 R100) of the bchd
/// nodes of the BCH platform `platform` for an SLP token `ticker` whose effective policy is `policy`: the bchd nodes
/// are judged under the policy their reads take ([`validate_slp_utxos_with`]): two distinct hosts answering, or the
/// one configured host the single-host switch trusts. A node counts as answering when it served a validation within
/// the window; otherwise, and only while the answering hosts are not enough, it gets one probe
/// ([`bchd_readiness_probe`]), at most once per window per host of the platform.
///
/// # Errors
///
/// The text of [`crate::swap_evidence::not_ready_text`], which names the token and the kind only.
pub(crate) async fn bchd_ready(
    ticker: &str,
    platform: &str,
    policy: &SwapEvidencePolicy,
    bchd_urls: &[String],
    allow_single_host: bool,
) -> Result<(), String> {
    bchd_ready_with(
        ticker,
        platform,
        policy,
        bchd_urls,
        allow_single_host,
        |url: &String| {
            let url = url.clone();
            async move { bchd_readiness_probe(&url).await }
        },
    )
    .await
}

/// [`bchd_ready`] with the probe of one node given by the caller (a scripted node in tests).
pub(crate) async fn bchd_ready_with<F, Fut>(
    ticker: &str,
    platform: &str,
    policy: &SwapEvidencePolicy,
    bchd_urls: &[String],
    allow_single_host: bool,
    probe: F,
) -> Result<(), String>
where
    F: Fn(&String) -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let bchd_policy = match (policy, single_bchd_host_policy(bchd_urls, allow_single_host)) {
        (SwapEvidencePolicy::NotAdopted, _) => return Ok(()),
        (_, Some(trusted)) => trusted,
        (SwapEvidencePolicy::Insufficient { hosts }, None) => SwapEvidencePolicy::Insufficient { hosts: *hosts },
        (_, None) => SwapEvidencePolicy::TwoNodes {
            hosts: distinct_hosts(bchd_urls),
        },
    };
    evidence_ready(
        ticker,
        &bchd_policy,
        bchd_urls,
        |url| bchd_label(url),
        |_| HostReadiness::Unknown,
        shared_readiness_probes(),
        &bchd_readiness_scope(platform),
        probe,
    )
    .await
}

/// The decision two bchd answers are compared on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ValidityDecision {
    Valid,
    Invalid,
}

/// Today's read through the same `ask`: every node, the first failure of any node is the answer.
async fn validate_slp_utxos_legacy<F, Fut>(
    bchd_urls: &[String],
    utxos: &[SlpUnspent],
    token_id: &H256,
    ask: &F,
) -> Result<(), MmError<ValidateSlpUtxosErr>>
where
    F: Fn(&String) -> Fut,
    Fut: std::future::Future<Output = Result<GetSlpTrustedValidationResponse, String>>,
{
    let answers = join_all(bchd_urls.iter().map(|url| ask(url))).await;
    for (url, answer) in bchd_urls.iter().zip(answers) {
        let label = bchd_label(url);
        let response = answer.map_err(|e| {
            MmError::new(ValidateSlpUtxosErr {
                to_url: label.clone(),
                kind: ValidateSlpUtxosErrKind::Request(e),
            })
        })?;
        judge_validation(&label, response, utxos, token_id)?;
    }
    Ok(())
}

#[derive(Debug, Display)]
pub enum CheckSlpTransactionErrKind {
    MultiReqErr(GrpcWebMultiUrlReqErr),
    #[display(fmt = "Transaction {:?} is not valid with reason {}", transaction, reason)]
    InvalidTransaction {
        transaction: Vec<u8>,
        reason: String,
    },
}

#[derive(Debug, Display)]
#[display(fmt = "Error {} on request to the node {}", kind, to_url)]
pub struct CheckSlpTransactionErr {
    to_url: String,
    kind: CheckSlpTransactionErrKind,
}

impl From<GrpcWebMultiUrlReqErr> for CheckSlpTransactionErr {
    fn from(err: GrpcWebMultiUrlReqErr) -> Self {
        CheckSlpTransactionErr {
            to_url: err.to_url.clone(),
            kind: CheckSlpTransactionErrKind::MultiReqErr(err),
        }
    }
}

pub async fn check_slp_transaction(
    bchd_urls: &[impl AsRef<str>],
    transaction: Vec<u8>,
) -> Result<(), MmError<CheckSlpTransactionErr>> {
    let request = CheckSlpTransactionRequest {
        transaction,
        required_slp_burns: Vec::new(),
        use_spec_validity_judgement: false,
    };

    let urls: Vec<_> = bchd_urls
        .iter()
        .map(|url| url.as_ref().to_owned() + "/pb.bchrpc/CheckSlpTransaction")
        .collect();

    let responses: Vec<(_, CheckSlpTransactionResponse)> =
        grpc_web_multi_url_request(&urls, &request).await.mm_err(Into::into)?;
    for (url, response) in responses {
        if !response.is_valid {
            return MmError::err(CheckSlpTransactionErr {
                to_url: node_name(url),
                kind: CheckSlpTransactionErrKind::InvalidTransaction {
                    transaction: request.transaction,
                    reason: response.invalid_reason,
                },
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod bchd_grpc_tests {
    use super::*;
    use crate::utxo::rpc_clients::UnspentInfo;
    use common::block_on;

    #[test]
    #[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
    fn test_validate_slp_utxos_valid() {
        let tx_hash = H256::from_reversed_str("0ba1b91abbfceaa0777424165edb2928dace87d59669c913989950da31968032");

        let slp_utxos = [
            SlpUnspent {
                bch_unspent: UnspentInfo {
                    outpoint: OutPoint {
                        hash: tx_hash,
                        index: 1,
                    },
                    value: 0,
                    height: None,
                },
                slp_amount: 1000,
            },
            SlpUnspent {
                bch_unspent: UnspentInfo {
                    outpoint: OutPoint {
                        hash: tx_hash,
                        index: 2,
                    },
                    value: 0,
                    height: None,
                },
                slp_amount: 8999,
            },
        ];

        let url = "https://bchd-testnet.electroncash.de:18335";
        let token_id = H256::from("bb309e48930671582bea508f9a1d9b491e49b69be3d6f372dc08da2ac6e90eb7");
        block_on(validate_slp_utxos(&[url], &slp_utxos, &token_id)).unwrap();
    }

    #[test]
    #[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
    fn test_validate_slp_utxos_non_slp_input() {
        let tx_hash = H256::from_reversed_str("0ba1b91abbfceaa0777424165edb2928dace87d59669c913989950da31968032");

        let slp_utxos = [
            SlpUnspent {
                bch_unspent: UnspentInfo {
                    outpoint: OutPoint {
                        hash: tx_hash,
                        index: 1,
                    },
                    value: 0,
                    height: None,
                },
                slp_amount: 1000,
            },
            SlpUnspent {
                bch_unspent: UnspentInfo {
                    outpoint: OutPoint {
                        hash: tx_hash,
                        index: 2,
                    },
                    value: 0,
                    height: None,
                },
                slp_amount: 8999,
            },
            SlpUnspent {
                bch_unspent: UnspentInfo {
                    outpoint: OutPoint {
                        hash: tx_hash,
                        index: 3,
                    },
                    value: 0,
                    height: None,
                },
                slp_amount: 8999,
            },
        ];

        let url = "https://bchd-testnet.electroncash.de:18335";
        let token_id = H256::from("bb309e48930671582bea508f9a1d9b491e49b69be3d6f372dc08da2ac6e90eb7");
        let err = block_on(validate_slp_utxos(&[url], &slp_utxos, &token_id)).unwrap_err();
        match err.into_inner().kind {
            ValidateSlpUtxosErrKind::MultiReqErr { .. } => (),
            err @ _ => panic!("Unexpected error {:?}", err),
        }
    }

    #[test]
    #[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
    fn test_validate_slp_utxos_invalid_amount() {
        let tx_hash = H256::from_reversed_str("0ba1b91abbfceaa0777424165edb2928dace87d59669c913989950da31968032");
        let invalid_utxo = SlpUnspent {
            bch_unspent: UnspentInfo {
                outpoint: OutPoint {
                    hash: tx_hash,
                    index: 1,
                },
                value: 0,
                height: None,
            },
            slp_amount: 999,
        };

        let slp_utxos = [invalid_utxo.clone(), SlpUnspent {
            bch_unspent: UnspentInfo {
                outpoint: OutPoint {
                    hash: tx_hash,
                    index: 2,
                },
                value: 0,
                height: None,
            },
            slp_amount: 8999,
        }];

        let url = "https://bchd-testnet.electroncash.de:18335";
        let token_id = H256::from("bb309e48930671582bea508f9a1d9b491e49b69be3d6f372dc08da2ac6e90eb7");
        let err = block_on(validate_slp_utxos(&[url], &slp_utxos, &token_id)).unwrap_err();
        match err.into_inner().kind {
            ValidateSlpUtxosErrKind::UnexpectedValidityResultType {
                for_unspent,
                validity_result,
            } => {
                let expected_validity = Some(ValidityResultType::V1TokenAmount(1000));
                assert_eq!(invalid_utxo, for_unspent);
                assert_eq!(expected_validity, validity_result);
            },
            err @ _ => panic!("Unexpected error {:?}", err),
        }
    }

    #[test]
    #[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
    fn test_validate_slp_utxos_unexpected_token_id() {
        let tx_hash = H256::from_reversed_str("0ba1b91abbfceaa0777424165edb2928dace87d59669c913989950da31968032");

        let slp_utxos = [
            SlpUnspent {
                bch_unspent: UnspentInfo {
                    outpoint: OutPoint {
                        hash: tx_hash,
                        index: 1,
                    },
                    value: 0,
                    height: None,
                },
                slp_amount: 1000,
            },
            SlpUnspent {
                bch_unspent: UnspentInfo {
                    outpoint: OutPoint {
                        hash: tx_hash,
                        index: 2,
                    },
                    value: 0,
                    height: None,
                },
                slp_amount: 8999,
            },
        ];

        let url = "https://bchd-testnet.electroncash.de:18335";
        let valid_token_id = H256::from("bb309e48930671582bea508f9a1d9b491e49b69be3d6f372dc08da2ac6e90eb7");
        let invalid_token_id = H256::from("bb309e48930671582bea508f9a1d9b491e49b69be3d6f372dc08da2ac6e90eb8");
        let err = block_on(validate_slp_utxos(&[url], &slp_utxos, &invalid_token_id)).unwrap_err();
        match err.into_inner().kind {
            ValidateSlpUtxosErrKind::UnexpectedTokenId { expected, actual } => {
                assert_eq!(invalid_token_id, expected);
                assert_eq!(valid_token_id, actual);
            },
            err @ _ => panic!("Unexpected error {:?}", err),
        }
    }

    #[test]
    #[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
    fn test_check_slp_transaction_valid() {
        let url = "https://bchd-testnet.electroncash.de:18335";
        // https://testnet.simpleledger.info/tx/c5f46ccc5431687154335d5b6526f1b9cfa961c44b97956b7bec77f884f56c73
        let tx = hex::decode("010000000232809631da50999813c96996d587ceda2829db5e16247477a0eafcbb1ab9a10b020000006a473044022057c88d815fa563eda8ef7d0dd5c522f4501ffa6110df455b151b31609f149c22022048fecfc9b16e983fbfd05b0d2b7c011c3dbec542577fa00cd9bd192b81961f8e4121036879df230663db4cd083c8eeb0f293f46abc460ad3c299b0089b72e6d472202cffffffff32809631da50999813c96996d587ceda2829db5e16247477a0eafcbb1ab9a10b030000006a4730440220539e1204d2805c0474111a1f233ff82c0ab06e6e2bfc0cbe4975eacae64a0b1f02200ec83d32c2180f5567d0f760e85f1efc99d9341cfebd86c9a334310f6d4381494121036879df230663db4cd083c8eeb0f293f46abc460ad3c299b0089b72e6d472202cffffffff040000000000000000406a04534c500001010453454e4420bb309e48930671582bea508f9a1d9b491e49b69be3d6f372dc08da2ac6e90eb7080000000000000001080000000000002326e8030000000000001976a914ca1e04745e8ca0c60d8c5881531d51bec470743f88ace8030000000000001976a9148cfffc2409d063437d6aa8b75a009b9ba51b71fc88ac9f694801000000001976a9148cfffc2409d063437d6aa8b75a009b9ba51b71fc88ac8983d460").unwrap();
        block_on(check_slp_transaction(&[url], tx)).unwrap();
    }

    #[test]
    #[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
    fn test_check_slp_transaction_invalid() {
        let url = "https://bchd-testnet.electroncash.de:18335";
        // https://www.blockchain.com/bch-testnet/tx/d76723c092b64bc598d5d2ceafd6f0db37dce4032db569d6f26afb35491789a7
        let tx = hex::decode("010000000190e35c09c83b5818b441c18a2d5ec54734851e5581fb21bde7936e77c6c3dca8030000006b483045022100e6b1415cbd81f2d04360597fba65965bc77ab5a972f5b8f8d5c0f1b1912923c402206a63f305f03e9c49ffba6c71c7a76ef60631f67dce7631f673a0e8485b86898d4121036879df230663db4cd083c8eeb0f293f46abc460ad3c299b0089b72e6d472202cffffffff020000000000000000376a04534c500001010453454e4420bb309e48930671582bea508f9a1d9b491e49b69be3d6f372dc08da2ac6e90eb70800000000000003e82500ae00000000001976a9148cfffc2409d063437d6aa8b75a009b9ba51b71fc88ac62715161").unwrap();
        let err = block_on(check_slp_transaction(&[url], tx)).unwrap_err();
        match err.into_inner().kind {
            CheckSlpTransactionErrKind::InvalidTransaction { reason, .. } => {
                println!("{}", reason);
            },
            err @ _ => panic!("Unexpected error {:?}", err),
        }
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod wasm_tests {
    use super::*;
    use wasm_bindgen_test::*;

    wasm_bindgen_test_configure!(run_in_browser);

    #[wasm_bindgen_test]
    async fn test_check_slp_transaction_valid() {
        let url = "https://bchd-testnet.electroncash.de:18335";
        // https://testnet.simpleledger.info/tx/c5f46ccc5431687154335d5b6526f1b9cfa961c44b97956b7bec77f884f56c73
        let tx = hex::decode("010000000232809631da50999813c96996d587ceda2829db5e16247477a0eafcbb1ab9a10b020000006a473044022057c88d815fa563eda8ef7d0dd5c522f4501ffa6110df455b151b31609f149c22022048fecfc9b16e983fbfd05b0d2b7c011c3dbec542577fa00cd9bd192b81961f8e4121036879df230663db4cd083c8eeb0f293f46abc460ad3c299b0089b72e6d472202cffffffff32809631da50999813c96996d587ceda2829db5e16247477a0eafcbb1ab9a10b030000006a4730440220539e1204d2805c0474111a1f233ff82c0ab06e6e2bfc0cbe4975eacae64a0b1f02200ec83d32c2180f5567d0f760e85f1efc99d9341cfebd86c9a334310f6d4381494121036879df230663db4cd083c8eeb0f293f46abc460ad3c299b0089b72e6d472202cffffffff040000000000000000406a04534c500001010453454e4420bb309e48930671582bea508f9a1d9b491e49b69be3d6f372dc08da2ac6e90eb7080000000000000001080000000000002326e8030000000000001976a914ca1e04745e8ca0c60d8c5881531d51bec470743f88ace8030000000000001976a9148cfffc2409d063437d6aa8b75a009b9ba51b71fc88ac9f694801000000001976a9148cfffc2409d063437d6aa8b75a009b9ba51b71fc88ac8983d460").unwrap();
        check_slp_transaction(&[url], tx).await.unwrap();
    }
}

#[cfg(test)]
mod r100_validity_tests {
    use super::*;
    use crate::utxo::rpc_clients::UnspentInfo;
    use common::block_on;
    use std::collections::HashMap;

    const HASH_BYTES: [u8; 32] = [7; 32];

    fn token() -> H256 { H256::from([9; 32]) }

    fn unspent() -> SlpUnspent {
        SlpUnspent {
            bch_unspent: UnspentInfo {
                outpoint: OutPoint {
                    hash: H256::from(HASH_BYTES),
                    index: 1,
                },
                value: 0,
                height: None,
            },
            slp_amount: 1000,
        }
    }

    fn answer(amount: u64) -> GetSlpTrustedValidationResponse {
        GetSlpTrustedValidationResponse {
            results: vec![get_slp_trusted_validation_response::ValidityResult {
                prev_out_hash: HASH_BYTES.to_vec(),
                prev_out_vout: 1,
                token_id: token().take().to_vec(),
                validity_result_type: Some(ValidityResultType::V1TokenAmount(amount)),
                ..Default::default()
            }],
        }
    }

    #[test]
    fn a_transport_failure_names_the_node_label_and_never_the_url() {
        let err = GrpcWebMultiUrlReqErr {
            to_url: node_name("https://user:secret@bchd1.example:8335/x"),
            err: grpc_web_err_kind(&PostGrpcWebErr::Transport {
                uri: "https://user:secret@bchd1.example:8335/x".to_owned(),
                error: "refused".to_owned(),
            })
            .to_owned(),
        };
        let text = err.to_string();
        assert!(
            text.contains("bchd1.example:8335") && text.contains("Transport"),
            "{text}"
        );
        assert!(!text.contains("secret") && !text.contains("://"), "{text}");
    }

    type Script = HashMap<&'static str, Result<GetSlpTrustedValidationResponse, String>>;

    fn urls(entries: &[&str]) -> Vec<String> { entries.iter().map(|e| (*e).to_owned()).collect() }

    /// The read with the single-host switch off, the strict rule.
    fn run(policy: &SwapEvidencePolicy, list: &[&str], script: &Script) -> Result<(), MmError<ValidateSlpUtxosErr>> {
        run_switch(policy, false, list, script).0
    }

    /// The read with the single-host switch `allow`, and the entries that were asked, in the order asked.
    fn run_switch(
        policy: &SwapEvidencePolicy,
        allow: bool,
        list: &[&str],
        script: &Script,
    ) -> (Result<(), MmError<ValidateSlpUtxosErr>>, Vec<String>) {
        let list = urls(list);
        let asked = std::sync::Mutex::new(Vec::new());
        let result = block_on(validate_slp_utxos_with(
            policy,
            &list,
            allow,
            &[unspent()],
            &token(),
            |url: &String| {
                asked.lock().unwrap().push(url.clone());
                let reply = script
                    .iter()
                    .find(|(host, _)| url.contains(*host))
                    .map(|(_, reply)| reply.clone())
                    .unwrap_or_else(|| Err("no script".to_owned()));
                async move { reply }
            },
        ));
        (result, asked.into_inner().unwrap())
    }

    fn two(list: &[&str]) -> SwapEvidencePolicy {
        SwapEvidencePolicy::TwoNodes {
            hosts: distinct_hosts(&urls(list)),
        }
    }

    fn undetermined_text(result: Result<(), MmError<ValidateSlpUtxosErr>>) -> String {
        let text = result.unwrap_err().to_string();
        assert!(crate::swap_evidence::is_undetermined(&text), "{text}");
        assert!(!text.contains("://"), "{text}");
        text
    }

    #[test]
    fn two_agreeing_hosts_decide_valid() {
        let list = ["https://a.example", "https://b.example"];
        let script: Script = [("a.example", Ok(answer(1000))), ("b.example", Ok(answer(1000)))].into();
        assert!(run(&two(&list), &list, &script).is_ok());
    }

    #[test]
    fn two_hosts_that_both_say_invalid_agree_on_invalid() {
        let list = ["https://a.example", "https://b.example"];
        let script: Script = [("a.example", Ok(answer(5))), ("b.example", Ok(answer(6)))].into();
        let err = run(&two(&list), &list, &script).unwrap_err().to_string();
        assert!(!crate::swap_evidence::is_undetermined(&err), "{err}");
    }

    #[test]
    fn hosts_that_differ_are_undetermined() {
        let list = ["https://a.example", "https://b.example"];
        let script: Script = [("a.example", Ok(answer(1000))), ("b.example", Ok(answer(5)))].into();
        let text = undetermined_text(run(&two(&list), &list, &script));
        assert!(
            text.contains("a.example:443") && text.contains("b.example:443"),
            "{text}"
        );
    }

    #[test]
    fn one_answering_host_is_undetermined() {
        let list = ["https://a.example", "https://b.example"];
        let script: Script = [("a.example", Ok(answer(1000))), ("b.example", Err("down".to_owned()))].into();
        undetermined_text(run(&two(&list), &list, &script));
    }

    #[test]
    fn tcp_and_ssl_entries_on_one_host_are_one_node_and_insufficient() {
        let list = ["https://a.example:8335", "http://A.example:8336"];
        let policy = crate::swap_evidence::swap_evidence_policy_from("T", None, &urls(&list)).unwrap();
        assert_eq!(policy, SwapEvidencePolicy::Insufficient { hosts: 1 });
        let script: Script = [("a.example", Ok(answer(1000)))].into();
        undetermined_text(run(&policy, &list, &script));
    }

    /// The single-host switch (maintainer decision of 2026-11-24): off, one configured bchd host is `Insufficient`
    /// and never reads valid; on, the token takes the platform's policy and its one bchd host is trusted alone. A
    /// TCP+SSL pair of one host is still one host: only the first entry of it is asked.
    #[test]
    fn one_bchd_host_is_trusted_only_with_the_switch_on() {
        let one = ["https://a.example:8335"];
        let pair = ["https://a.example:8335", "http://A.example:8336"];
        let platform = SwapEvidencePolicy::TwoNodes {
            hosts: vec!["e1.example".to_owned(), "e2.example".to_owned()],
        };
        let script: Script = [("a.example", Ok(answer(1000)))].into();

        // Off: the token is `Insufficient`, and so is the read.
        assert_eq!(single_bchd_host_policy(&urls(&pair), false), None);
        let insufficient = SwapEvidencePolicy::Insufficient { hosts: 1 };
        undetermined_text(run_switch(&insufficient, false, &one, &script).0);
        undetermined_text(run_switch(&insufficient, false, &pair, &script).0);

        // On: `Trusted` on the first entry of the one host, which decides alone.
        for list in [&one[..], &pair[..]] {
            assert_eq!(
                single_bchd_host_policy(&urls(list), true),
                Some(SwapEvidencePolicy::Trusted {
                    node: "https://a.example:8335".to_owned()
                })
            );
            let (result, asked) = run_switch(&platform, true, list, &script);
            assert!(result.is_ok(), "{result:?}");
            assert_eq!(asked, vec!["https://a.example:8335".to_owned()]);
        }

        // On: an invalid answer of the trusted host is decided as invalid, not undetermined.
        let invalid: Script = [("a.example", Ok(answer(5)))].into();
        let err = run_switch(&platform, true, &one, &invalid).0.unwrap_err().to_string();
        assert!(!crate::swap_evidence::is_undetermined(&err), "{err}");

        // On: a silent trusted host is undetermined, and the read names its label only.
        let silent: Script = [("a.example", Err("down".to_owned()))].into();
        let text = undetermined_text(run_switch(&platform, true, &pair, &silent).0);
        assert!(text.contains("a.example:8335"), "{text}");

        // No host configured stays `Insufficient` with the switch on.
        assert_eq!(single_bchd_host_policy(&[], true), None);
    }

    #[test]
    fn two_hosts_of_one_operator_are_two_nodes() {
        let list = ["https://bchd1.op.example", "https://bchd2.op.example"];
        let policy = crate::swap_evidence::swap_evidence_policy_from("T", None, &urls(&list)).unwrap();
        let script: Script = [
            ("bchd1.op.example", Ok(answer(1000))),
            ("bchd2.op.example", Ok(answer(1000))),
        ]
        .into();
        assert!(run(&policy, &list, &script).is_ok());
    }

    #[test]
    fn a_disagreement_is_not_outvoted_and_a_new_pair_decides() {
        let list = ["https://a.example", "https://b.example", "https://c.example"];
        let policy = two(&list);
        // First attempt: a and b differ; c would agree with a, but no third node is asked.
        let script: Script = [
            ("a.example", Ok(answer(1000))),
            ("b.example", Ok(answer(5))),
            ("c.example", Ok(answer(1000))),
        ]
        .into();
        undetermined_text(run(&policy, &list, &script));
        // A later attempt whose pair agrees decides.
        let script: Script = [
            ("a.example", Ok(answer(1000))),
            ("b.example", Ok(answer(1000))),
            ("c.example", Ok(answer(5))),
        ]
        .into();
        assert!(run(&policy, &list, &script).is_ok());
    }

    #[test]
    fn an_insufficient_coin_never_reads_valid() {
        let list = ["https://a.example"];
        let script: Script = [("a.example", Ok(answer(1000)))].into();
        undetermined_text(run(&SwapEvidencePolicy::Insufficient { hosts: 1 }, &list, &script));
    }

    #[test]
    fn a_platform_trusted_node_does_not_replace_the_two_bchd_hosts() {
        let list = ["https://a.example", "https://b.example"];
        let policy = SwapEvidencePolicy::Trusted {
            node: "electrum.example:50002".to_owned(),
        };
        let script: Script = [("a.example", Ok(answer(1000))), ("b.example", Ok(answer(1000)))].into();
        assert!(run(&policy, &list, &script).is_ok());
    }

    /// Two or more configured bchd hosts stay strict with the single-host switch on: two hosts must agree, one
    /// answering host is undetermined, and a platform trusted node does not replace them.
    #[test]
    fn two_bchd_hosts_stay_strict_with_the_switch_on() {
        let two_hosts = ["https://a.example", "https://b.example"];
        let three_hosts = ["https://a.example", "https://b.example", "https://c.example"];
        assert_eq!(single_bchd_host_policy(&urls(&two_hosts), true), None);
        assert_eq!(single_bchd_host_policy(&urls(&three_hosts), true), None);
        let trusted_platform = SwapEvidencePolicy::Trusted {
            node: "electrum.example:50002".to_owned(),
        };
        for policy in [two(&two_hosts), trusted_platform] {
            let agreeing: Script = [("a.example", Ok(answer(1000))), ("b.example", Ok(answer(1000)))].into();
            let (result, asked) = run_switch(&policy, true, &two_hosts, &agreeing);
            assert!(result.is_ok(), "{result:?}");
            assert_eq!(asked.len(), 2);
            let one_down: Script = [("a.example", Ok(answer(1000))), ("b.example", Err("down".to_owned()))].into();
            undetermined_text(run_switch(&policy, true, &two_hosts, &one_down).0);
            let differing: Script = [("a.example", Ok(answer(1000))), ("b.example", Ok(answer(5)))].into();
            undetermined_text(run_switch(&policy, true, &two_hosts, &differing).0);
        }
    }

    #[test]
    fn not_adopted_keeps_the_all_hosts_read() {
        let list = ["https://a.example", "https://b.example"];
        let policy = SwapEvidencePolicy::NotAdopted;
        let good: Script = [("a.example", Ok(answer(1000))), ("b.example", Ok(answer(1000)))].into();
        assert!(run(&policy, &list, &good).is_ok());
        // One invalid host fails the read, as today, and it is not an undetermined read.
        let one_bad: Script = [("a.example", Ok(answer(1000))), ("b.example", Ok(answer(5)))].into();
        let err = run(&policy, &list, &one_bad).unwrap_err().to_string();
        assert!(
            !crate::swap_evidence::is_undetermined(&err) && !err.contains("://"),
            "{err}"
        );
        // One host down fails the read, as today.
        let one_down: Script = [("a.example", Ok(answer(1000))), ("b.example", Err("down".to_owned()))].into();
        assert!(run(&policy, &list, &one_down).is_err());
    }
}
