//! `lightwalletd` as the sole backend of a Light-mode ZHTLC coin (CRD ch.39 §39.11).
//!
//! A Light-mode shielded coin is served by exactly one backend: the configured
//! `light_wallet_d_servers`, speaking the lightwalletd gRPC compact-transaction-streamer
//! service (R39.11.1). No Electrum connection is opened on behalf of such a coin.
//!
//! The module has four parts:
//!
//! * [`decide_admission`] -- endpoint admission (R39.11.7a), a pure function over what
//!   `GetLightdInfo` reported, so the decision that keeps a wallet on its own chain can be tested
//!   without a network.
//! * [`LightwalletdBackend`] -- the ordered-failover client over the *admitted* endpoints, the
//!   single chain-height authority (R39.11.9) with its monotonic floor (R39.11.10), and every
//!   function of the R39.11.1 table.
//! * [`read_history`] -- the windowed, bisecting transparent-history reader (R39.11.4b).
//! * [`LightwalletdClient`] -- the first-class implementation of [`UtxoRpcClientOps`]
//!   (R39.11.4g) that the shared UTXO swap helpers reach through the trait.
//!
//! The generic address methods still refuse a P2SH query with a typed error, because the serving
//! node's address index returns an empty success for a script-hash address forever (R39.11.4d) and an
//! empty result there would read as "not spent". Swap-path HTLC spend and already-sent detection
//! live in the `htlc_index` child module, which probes both address forms and decides from what they
//! return.

mod htlc_index;

pub(crate) use htlc_index::{p2sh_hash_of, DetectionLog, FundingFound, HtlcAddressing, HtlcIndexError, IndexEncoding,
                            OutputRef, SentOutcome, SpendOutcome};

use super::z_coin_wallet_db::{dedupe_lightwalletd_endpoints, scrub_lightwalletd_text, ZCoinShieldedHistory};
use super::z_rpc::z_coin_grpc;
use super::ZcoinConsensusParams;
use crate::big_decimal_from_sat_unsigned;
use crate::endpoint_suspension::{Suspension, SUSPENSION_BASE, SUSPENSION_CAP};
use crate::node_url::lightwalletd_endpoint_label;
use crate::utxo::rpc_clients::{BlockHashOrHeight, EstimateFeeMethod, EstimateFeeMode, SpentOutputInfo, UnspentInfo,
                               UnspentMap, UtxoRpcClientOps, UtxoRpcError, UtxoRpcFut, UtxoRpcResult};
use async_trait::async_trait;
use bigdecimal::BigDecimal;
use chain::{OutPoint, Transaction as UtxoTx};
use common::jsonrpc_client::{JsonRpcError, JsonRpcErrorType, JsonRpcRequest, JsonRpcRequestEnum, RpcRes};
use common::median;
use derive_more::Display;
use futures::compat::Future01CompatExt;
use futures::future::{join_all, BoxFuture};
use futures::lock::Mutex as AsyncMutex;
use futures::{FutureExt, StreamExt, TryFutureExt};
use keys::hash::H256;
use keys::Address;
use mm2_err_handle::prelude::*;
use parking_lot::Mutex;
use prost_14::Message as _;
use rpc::v1::types::{Bytes as BytesJson, Transaction as RpcTransaction, H256 as H256Json};
use serialization::{deserialize, serialize, CoinVariant};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::num::NonZeroU64;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tonic::Code;
use zcash_primitives::transaction::builder::DEFAULT_TX_EXPIRY_DELTA;
use zcash_primitives::transaction::Transaction as ZTransaction;
use zcash_protocol::consensus::{BlockHeight, BranchId, NetworkType, NetworkUpgrade, Parameters};

/// How long an admission decision is trusted before the endpoints are probed again (R39.11.7a(g)).
///
/// An endpoint can be upgraded, and a previously admitted one can fork away, so admission is never
/// cached for the coin's lifetime. Re-evaluation can only ever *narrow* a wallet's binding (see
/// [`decide_admission`]).
pub(crate) const ADMISSION_REEVALUATION_INTERVAL: Duration = Duration::from_secs(300);

/// Height window of one transparent-history request (R39.11.4b(a)). The probed server's failures
/// were confined to seven windows of this size over the whole test chain.
pub(crate) const TADDR_HISTORY_WINDOW_BLOCKS: u64 = 1_000;

/// Upper bound on a median-time-past window (R39.11.4f): each height is a separate request.
pub(crate) const MEDIAN_TIME_WINDOW_MAX: u64 = 64;

/// The builder's transaction expiry delta, re-exported so the staleness bound below cannot drift
/// from the dependency it describes (R39.11.8(e)).
pub(crate) const BUILDER_EXPIRY_DELTA: u32 = DEFAULT_TX_EXPIRY_DELTA;

const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const CALL_TIMEOUT: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------------

/// Why a lightwalletd operation failed. The variants that concern *which endpoints may be used*
/// are kept apart because activation reports them differently (R39.11.6).
#[derive(Clone, Debug, Display, PartialEq)]
pub enum LightwalletdError {
    #[display(fmt = "no lightwalletd endpoint is configured")]
    NoEndpoints,
    /// Every configured endpoint failed the server-information read.
    #[display(fmt = "every configured lightwalletd endpoint is unreachable: {}", _0)]
    AllUnreachable(String),
    /// At least one endpoint answered, and every one that did was excluded by admission.
    #[display(
        fmt = "every reachable lightwalletd endpoint was excluded by admission (they serve another chain or \
               branch, or are stale): {}",
        _0
    )]
    AllExcluded(String),
    /// A fresh wallet found no consensus-branch partition with a strict majority (R39.11.7a(e)).
    #[display(
        fmt = "fewer than two lightwalletd endpoints agree on a consensus branch, so none can be trusted as the \
               majority (observed branches: {})",
        _0
    )]
    NoBranchMajority(String),
    #[display(fmt = "{} failed on every admitted lightwalletd endpoint: {}", op, errors)]
    AllFailed { op: &'static str, errors: String },
    /// The service answered the broadcast with a failure; reproducing it elsewhere would hide the
    /// reason, so it is never retried on further endpoints (R39.11.4). `endpoint` is the host-only name of
    /// the endpoint that rejected it (R39.11.30).
    #[display(
        fmt = "lightwalletd endpoint {} rejected the transaction (code {}): {}",
        endpoint,
        code,
        message
    )]
    BroadcastRejected {
        endpoint: String,
        code: i32,
        message: String,
    },
    /// The tip read cannot be corroborated against what this activation already saw (R39.11.11).
    #[display(
        fmt = "the lightwalletd chain tip {} is more than {} blocks behind height {} already observed; refusing to \
               build against a height that cannot be corroborated",
        tip,
        BUILDER_EXPIRY_DELTA,
        observed
    )]
    StaleTip { tip: u64, observed: u64 },
    #[display(fmt = "{} is not supported over lightwalletd: {}", op, reason)]
    Unsupported { op: &'static str, reason: &'static str },
    #[display(fmt = "invalid lightwalletd request: {}", _0)]
    InvalidRequest(String),
    /// The service explicitly answered "no such transaction" (as opposed to failing).
    #[display(fmt = "lightwalletd has no such transaction: {}", _0)]
    NoSuchTransaction(String),
}

impl LightwalletdError {
    fn into_rpc_error(self, method: &str) -> MmError<UtxoRpcError> {
        match self {
            LightwalletdError::AllUnreachable(_) | LightwalletdError::AllFailed { .. } => MmError::new(
                UtxoRpcError::Transport(lightwalletd_json_rpc_error(method, self.to_string())),
            ),
            other => MmError::new(UtxoRpcError::Internal(other.to_string())),
        }
    }
}

fn lightwalletd_json_rpc_error(method: &str, message: String) -> JsonRpcError {
    JsonRpcError {
        client_info: "lightwalletd".to_owned(),
        request: JsonRpcRequestEnum::Single(JsonRpcRequest {
            jsonrpc: "2.0".to_owned(),
            id: "0".to_owned(),
            method: method.to_owned(),
            params: Vec::new(),
        }),
        error: JsonRpcErrorType::Transport(message),
    }
}

/// How one endpoint failed one call.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum EndpointError {
    /// Could not reach the endpoint, or it timed out.
    Transport(String),
    /// The endpoint answered with an error status.
    Service(String),
    /// The endpoint explicitly answered "not found".
    NotFound(String),
    /// A service-level broadcast failure (`SendResponse.errorCode != 0`).
    Rejected { code: i32, message: String },
    /// The server's node answered an address read with its no-address-index message (R39.11.4i(e)). It
    /// carries the status as received. Not a serve failure of one transaction (R39.11.4b(c)).
    NoAddressIndex(String),
}

impl fmt::Display for EndpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EndpointError::Transport(m) | EndpointError::Service(m) | EndpointError::NotFound(m) => f.write_str(m),
            EndpointError::Rejected { code, message } => write!(f, "rejected ({}): {}", code, message),
            EndpointError::NoAddressIndex(m) => write!(f, "the server's node keeps no address index: {}", m),
        }
    }
}

/// The node's message for a transparent-address read when it keeps no address index (R39.11.4i).
/// Matched case-insensitively, whatever the status code it arrives under.
pub(crate) const NO_ADDRESS_INDEX_MESSAGE: &str = "No information available for address";

pub(crate) fn mentions_no_address_index(text: &str) -> bool {
    text.to_lowercase().contains(&NO_ADDRESS_INDEX_MESSAGE.to_lowercase())
}

// ---------------------------------------------------------------------------------------------
// Byte order (R39.11.4c)
// ---------------------------------------------------------------------------------------------

/// `GetTransaction` takes the txid in **internal** (compact-block) byte order; every caller-facing
/// surface of this wallet uses the big-endian **display** order (R39.8.7a). These two functions are
/// the only conversion point.
pub(crate) fn txid_display_to_internal(display: &H256Json) -> [u8; 32] { display.reversed().0 }

pub(crate) fn txid_internal_to_display(internal: [u8; 32]) -> H256Json { H256Json::from(internal).reversed() }

// ---------------------------------------------------------------------------------------------
// Endpoint admission (R39.11.7a)
// ---------------------------------------------------------------------------------------------

/// What `GetLightdInfo` reported.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ServerInfo {
    pub chain_name: String,
    pub sapling_activation_height: u64,
    /// Hex, as the service sends it.
    pub consensus_branch_id: String,
    pub block_height: u64,
}

/// What admission learns about one reachable endpoint: its `GetLightdInfo` answer, and how far its
/// compact-block cache (`GetLatestBlock`) has got. The two come from different RPCs and disagree on
/// a lightwalletd whose cache is behind its node (R39.11.7a(j)).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AdmissionProbe {
    pub info: ServerInfo,
    /// The height `GetLatestBlock` reported, or the error it returned (an empty cache answers
    /// "Cache is empty" while `GetLightdInfo` succeeds).
    pub cache_height: Result<u64, String>,
}

/// How many blocks an endpoint may trail and still be admitted (R39.11.7a(j)): the builder's own
/// expiry delta, because a tip further behind than that is refused by the monotonic-tip rule
/// (R39.11.11) anyway. A tolerance below it would exclude endpoints the wallet can use; above it,
/// admit endpoints the wallet would then refuse.
pub(crate) const STALENESS_TOLERANCE_BLOCKS: u64 = BUILDER_EXPIRY_DELTA as u64;

/// The lower median of the node heights the endpoints report: the height most of them have reached.
/// With three or more endpoints a minority cannot move it; with two it is the lower one, so a lone
/// stale endpoint is never the reference that condemns a healthy one.
fn consensus_height(mut heights: Vec<u64>) -> Option<u64> {
    heights.sort_unstable();
    heights.get(heights.len().checked_sub(1)? / 2).copied()
}

/// Parses the service's hex consensus branch identifier (`76b809bb`, optionally `0x`-prefixed).
pub(crate) fn parse_branch_id(raw: &str) -> Option<u32> {
    let trimmed = raw.trim().trim_start_matches("0x");
    if trimmed.is_empty() || trimmed.len() > 8 {
        return None;
    }
    u32::from_str_radix(trimmed, 16).ok()
}

/// The consensus branch identifier the coin's configuration determines, and where that stops.
///
/// The expectation derived from configured upgrade heights is the *pre-upgrade* value for any
/// upgrade the configuration does not declare, so a check that applies it at every height admits
/// the stale fork and excludes the healthy chain (R39.11.7a). [`BranchSchedule::expected_at`]
/// therefore answers `None` at and above the **horizon** -- the activation height of the last
/// declared upgrade, when any later upgrade this build knows of is undeclared -- and the caller must
/// not exclude on a local expectation it does not have.
#[derive(Clone, Debug)]
pub(crate) struct BranchSchedule {
    params: ZcoinConsensusParams,
    /// `None`: the configuration declares every upgrade this build knows of, so it determines the
    /// branch at every height.
    horizon: Option<u32>,
}

/// Every upgrade this build maps, in activation order.
const KNOWN_UPGRADES: [NetworkUpgrade; 10] = [
    NetworkUpgrade::Overwinter,
    NetworkUpgrade::Sapling,
    NetworkUpgrade::Blossom,
    NetworkUpgrade::Heartwood,
    NetworkUpgrade::Canopy,
    NetworkUpgrade::Nu5,
    NetworkUpgrade::Nu6,
    NetworkUpgrade::Nu6_1,
    NetworkUpgrade::Nu6_2,
    NetworkUpgrade::Nu6_3,
];

impl BranchSchedule {
    pub(crate) fn from_params(params: &ZcoinConsensusParams) -> Self {
        let declared: Vec<Option<u32>> = KNOWN_UPGRADES
            .iter()
            .map(|nu| params.activation_height(*nu).map(u32::from))
            .collect();
        let last_declared = declared.iter().rposition(Option::is_some);
        let horizon = last_declared.and_then(|idx| {
            let undeclared_after = declared[idx + 1..].iter().any(Option::is_none);
            undeclared_after.then(|| declared[idx].unwrap_or(0))
        });
        // A configuration declaring nothing at all cannot determine anything.
        let horizon = if last_declared.is_none() { Some(0) } else { horizon };
        BranchSchedule {
            params: params.clone(),
            horizon,
        }
    }

    /// The branch identifier the configuration determines at `height`, or `None` when it does not.
    pub(crate) fn expected_at(&self, height: u64) -> Option<u32> {
        let height = u32::try_from(height).ok()?;
        if self.horizon.is_some_and(|horizon| height >= horizon) {
            return None;
        }
        Some(u32::from(BranchId::for_height(
            &self.params,
            BlockHeight::from_u32(height),
        )))
    }
}

/// The branch identifier of the last declared upgrade before Ironwood, or `None` when the
/// configuration declares no upgrade at all.
fn pre_ironwood_branch(params: &ZcoinConsensusParams) -> Option<u32> {
    let height = KNOWN_UPGRADES
        .iter()
        .filter(|nu| **nu != NetworkUpgrade::Nu6_3)
        .filter_map(|nu| params.activation_height(*nu))
        .max()?;
    Some(u32::from(BranchId::for_height(params, height)))
}

/// What the coin expects of an endpoint, from its own configuration.
#[derive(Clone, Debug)]
pub(crate) struct AdmissionPolicy {
    pub network: &'static str,
    pub sapling_activation_height: u64,
    pub schedule: BranchSchedule,
    /// The wall-clock time from which the Ironwood upgrade is in force, when this build or the
    /// coin configuration knows one. The authority for "an endpoint still on the pre-upgrade branch
    /// is stale" (R39.11.7a(h)); `None` means no such judgement can be made.
    pub ironwood_activation_time: Option<u32>,
    /// The branch identifier in force just before Ironwood: the one the last declared pre-Ironwood
    /// upgrade determines.
    pub pre_ironwood_branch: Option<u32>,
}

impl AdmissionPolicy {
    pub(crate) fn from_params(params: &ZcoinConsensusParams) -> Self {
        let network = match params.network_type_hint() {
            NetworkType::Main => "main",
            NetworkType::Test => "test",
            NetworkType::Regtest => "regtest",
        };
        AdmissionPolicy {
            network,
            sapling_activation_height: u64::from(params.sapling_activation_height),
            schedule: BranchSchedule::from_params(params),
            ironwood_activation_time: None,
            pre_ironwood_branch: pre_ironwood_branch(params),
        }
    }

    pub(crate) fn with_ironwood_activation_time(mut self, time: Option<u32>) -> Self {
        self.ironwood_activation_time = time;
        self
    }

    /// Whether the Ironwood activation time is known and has passed at `now_sec`.
    fn ironwood_active_at(&self, now_sec: u64) -> bool {
        self.ironwood_activation_time
            .is_some_and(|time| now_sec >= u64::from(time))
    }

    fn network_matches(&self, reported: &str) -> bool {
        let reported = reported.trim().to_ascii_lowercase();
        match self.network {
            "main" => reported == "main" || reported == "mainnet",
            "test" => reported == "test" || reported == "testnet",
            other => reported == other,
        }
    }
}

/// What the wallet database says about the chain the wallet follows.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct WalletBranchState {
    /// The wallet database holds scanned blocks.
    pub has_scanned_state: bool,
    /// The consensus branch identifier recorded alongside that scanned state.
    pub recorded: Option<u32>,
}

#[derive(Clone, Debug, Display, PartialEq)]
pub(crate) enum ExclusionReason {
    #[display(fmt = "reports network '{}', expected '{}'", reported, expected)]
    NetworkMismatch { reported: String, expected: String },
    #[display(fmt = "reports Sapling activation height {}, expected {}", reported, expected)]
    SaplingActivationMismatch { reported: u64, expected: u64 },
    #[display(fmt = "reports an unparseable consensus branch identifier '{}'", _0)]
    UnparseableBranchId(String),
    #[display(
        fmt = "reports branch {:08x} at height {}, the coin configuration requires {:08x}",
        reported,
        at_height,
        expected
    )]
    ConfigBranchMismatch {
        reported: u32,
        expected: u32,
        at_height: u64,
    },
    #[display(
        fmt = "reports branch {:08x}, the wallet is bound to branch {:08x} (its scanned state follows that chain)",
        reported,
        bound
    )]
    OutsideWalletBranch { reported: u32, bound: u32 },
    #[display(
        fmt = "reports the pre-Ironwood branch {:08x} although Ironwood activated at {} (it has not followed the upgrade)",
        reported,
        activation_time
    )]
    StalePreActivationBranch { reported: u32, activation_time: u32 },
    #[display(
        fmt = "reports branch {:08x}, outvoted by branch {:08x} reported by a strict majority of the endpoints",
        reported,
        majority
    )]
    OutvotedBranch { reported: u32, majority: u32 },
    #[display(
        fmt = "answers GetLightdInfo but its compact-block cache does not answer GetLatestBlock: {}",
        _0
    )]
    CacheUnavailable(String),
    #[display(
        fmt = "its compact-block cache is at height {} while its own node reports {} (more than {} blocks behind)",
        cache,
        node,
        tolerance
    )]
    CacheBehindNode { cache: u64, node: u64, tolerance: u64 },
    #[display(
        fmt = "it can serve blocks only up to {} while the endpoints' consensus height is {} (more than {} blocks \
               behind)",
        served,
        consensus,
        tolerance
    )]
    BehindConsensus {
        served: u64,
        consensus: u64,
        tolerance: u64,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AdmissionOutcome {
    /// In configured order.
    pub admitted: Vec<String>,
    pub excluded: Vec<(String, ExclusionReason)>,
    pub unreachable: Vec<(String, String)>,
    /// The branch identifier to persist alongside the wallet's sync state, when this decision
    /// established it (a majority decision on a wallet that carries none).
    pub branch_to_record: Option<u32>,
}

fn describe_excluded(excluded: &[(String, ExclusionReason)]) -> String {
    excluded
        .iter()
        .map(|(server, reason)| format!("{} {}", server, reason))
        .collect::<Vec<_>>()
        .join("; ")
}

fn describe_probes(probes: &[(String, Result<AdmissionProbe, String>)]) -> String {
    probes
        .iter()
        .map(|(server, probe)| match probe {
            Ok(AdmissionProbe { info, cache_height }) => format!(
                "{} [chain {}, branch {}, height {}, cache {}]",
                server,
                info.chain_name,
                info.consensus_branch_id,
                info.block_height,
                match cache_height {
                    Ok(height) => height.to_string(),
                    Err(e) => format!("unavailable: {}", e),
                }
            ),
            Err(e) => format!("{} [unreachable: {}]", server, e),
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Decides which endpoints may be used (R39.11.7a). `probes` holds, per configured endpoint and in
/// configured order, what its server-information read returned.
///
/// 1. An endpoint on another network, or with another Sapling activation height, is excluded.
/// 2. Where the coin configuration *determines* the branch at the endpoint's reported height, a
///    mismatch excludes it ((d)).
/// 2a. Once the Ironwood activation time has passed, an endpoint reporting the pre-Ironwood branch
///    is excluded outright ((h)), before any vote, and a wallet binding to that branch is released.
/// 2b. An endpoint whose compact-block cache is unavailable or materially behind its own node, or
///    that trails the endpoints' consensus height, is excluded before any vote ((j)).
/// 3. Where it does not ((e)) the local expectation is **not** consulted. A wallet carrying scanned
///    state with a recorded identifier admits only that partition; any other wallet admits the
///    partition a strict majority of at least two endpoints reports, and fails with every observed
///    identifier when there is none -- including the single-endpoint case.
#[cfg(test)]
pub(crate) fn decide_admission(
    probes: Vec<(String, Result<AdmissionProbe, String>)>,
    policy: &AdmissionPolicy,
    wallet: &WalletBranchState,
) -> Result<AdmissionOutcome, LightwalletdError> {
    decide_admission_at(probes, policy, wallet, 0)
}

/// [`decide_admission`] at the wall-clock time `now_sec`, which only the stale-fork rule (h) reads.
pub(crate) fn decide_admission_at(
    probes: Vec<(String, Result<AdmissionProbe, String>)>,
    policy: &AdmissionPolicy,
    wallet: &WalletBranchState,
    now_sec: u64,
) -> Result<AdmissionOutcome, LightwalletdError> {
    if probes.is_empty() {
        return Err(LightwalletdError::NoEndpoints);
    }

    let mut excluded: Vec<(String, ExclusionReason)> = Vec::new();
    let mut unreachable: Vec<(String, String)> = Vec::new();
    let mut admitted_flags: HashMap<String, bool> = HashMap::new();
    // Endpoints the configuration cannot judge, with their reported branch.
    let mut undetermined: Vec<(String, u32)> = Vec::new();
    let mut reachable = 0usize;
    // The height most endpoints have reached, among those on this coin's network (R39.11.7a(j)).
    let consensus = consensus_height(
        probes
            .iter()
            .filter_map(|(_, probe)| probe.as_ref().ok())
            .filter(|p| {
                policy.network_matches(&p.info.chain_name)
                    && p.info.sapling_activation_height == policy.sapling_activation_height
            })
            .map(|p| p.info.block_height)
            .collect(),
    );

    for (server, probe) in &probes {
        let (info, cache_height) = match probe {
            Ok(AdmissionProbe { info, cache_height }) => (info, cache_height),
            Err(e) => {
                unreachable.push((server.clone(), e.clone()));
                continue;
            },
        };
        reachable += 1;

        if !policy.network_matches(&info.chain_name) {
            excluded.push((server.clone(), ExclusionReason::NetworkMismatch {
                reported: info.chain_name.clone(),
                expected: policy.network.to_owned(),
            }));
            continue;
        }
        if info.sapling_activation_height != policy.sapling_activation_height {
            excluded.push((server.clone(), ExclusionReason::SaplingActivationMismatch {
                reported: info.sapling_activation_height,
                expected: policy.sapling_activation_height,
            }));
            continue;
        }
        let Some(reported_branch) = parse_branch_id(&info.consensus_branch_id) else {
            excluded.push((
                server.clone(),
                ExclusionReason::UnparseableBranchId(info.consensus_branch_id.clone()),
            ));
            continue;
        };

        // (h) Positive information, not a vote: once the Ironwood activation time has passed, an
        // endpoint reporting the pre-Ironwood branch has not followed the chain, whatever the other
        // endpoints say and however many of them there are.
        if let (Some(time), Some(pre)) = (policy.ironwood_activation_time, policy.pre_ironwood_branch) {
            if policy.ironwood_active_at(now_sec) && reported_branch == pre {
                excluded.push((server.clone(), ExclusionReason::StalePreActivationBranch {
                    reported: reported_branch,
                    activation_time: time,
                }));
                continue;
            }
        }

        // (j) Staleness: judged on what the endpoint can actually serve, before the vote so that a
        // stale endpoint neither counts in a majority nor is mistaken for the healthy branch.
        let tolerance = STALENESS_TOLERANCE_BLOCKS;
        let cache = match cache_height {
            Ok(cache) => *cache,
            Err(e) => {
                excluded.push((server.clone(), ExclusionReason::CacheUnavailable(e.clone())));
                continue;
            },
        };
        if cache.saturating_add(tolerance) < info.block_height {
            excluded.push((server.clone(), ExclusionReason::CacheBehindNode {
                cache,
                node: info.block_height,
                tolerance,
            }));
            continue;
        }
        let served = cache.min(info.block_height);
        if let Some(consensus) = consensus.filter(|c| served.saturating_add(tolerance) < *c) {
            excluded.push((server.clone(), ExclusionReason::BehindConsensus {
                served,
                consensus,
                tolerance,
            }));
            continue;
        }

        match policy.schedule.expected_at(info.block_height) {
            Some(expected) if expected != reported_branch => {
                excluded.push((server.clone(), ExclusionReason::ConfigBranchMismatch {
                    reported: reported_branch,
                    expected,
                    at_height: info.block_height,
                }));
            },
            Some(_) => {
                admitted_flags.insert(server.clone(), true);
            },
            None => undetermined.push((server.clone(), reported_branch)),
        }
    }

    let mut branch_to_record = None;
    if !undetermined.is_empty() {
        let bound = match wallet {
            // A binding to the pre-Ironwood branch is superseded once Ironwood is in force: that
            // chain is a prefix of the current one, and keeping the binding would lock the wallet
            // out of every current endpoint (R39.11.7a(h)).
            WalletBranchState {
                has_scanned_state: true,
                recorded: Some(recorded),
            } if !(policy.ironwood_active_at(now_sec) && Some(*recorded) == policy.pre_ironwood_branch) => {
                Some(*recorded)
            },
            _ => None,
        };
        match bound {
            // The wallet's scanned state follows one chain: never leave it, whatever the endpoint's
            // tip height. This is what stops an off-branch endpoint above the wallet's anchor from
            // making the wallet rewind and follow that endpoint's chain.
            Some(bound) => {
                for (server, branch) in &undetermined {
                    if *branch == bound {
                        admitted_flags.insert(server.clone(), true);
                    } else {
                        excluded.push((server.clone(), ExclusionReason::OutsideWalletBranch {
                            reported: *branch,
                            bound,
                        }));
                    }
                }
            },
            None => {
                let mut counts: Vec<(u32, usize)> = Vec::new();
                for (_, branch) in &undetermined {
                    match counts.iter_mut().find(|(b, _)| b == branch) {
                        Some((_, n)) => *n += 1,
                        None => counts.push((*branch, 1)),
                    }
                }
                // A strict majority of the endpoints the configuration could not judge, and never a
                // single endpoint: one voice corroborates nothing.
                let majority = counts
                    .iter()
                    .find(|(_, n)| *n >= 2 && *n * 2 > undetermined.len())
                    .map(|(branch, _)| *branch);
                let Some(majority) = majority else {
                    let observed = counts
                        .iter()
                        .map(|(branch, n)| format!("{:08x} x{}", branch, n))
                        .collect::<Vec<_>>()
                        .join(", ");
                    // Name what else was dropped, so the failure says why so few endpoints voted.
                    let mut detail = observed;
                    if !excluded.is_empty() {
                        detail.push_str(&format!("; excluded: {}", describe_excluded(&excluded)));
                    }
                    if !unreachable.is_empty() {
                        detail.push_str(&format!(
                            "; unreachable: {}",
                            unreachable
                                .iter()
                                .map(|(server, e)| format!("{}: {}", server, e))
                                .collect::<Vec<_>>()
                                .join("; ")
                        ));
                    }
                    return Err(LightwalletdError::NoBranchMajority(detail));
                };
                for (server, branch) in &undetermined {
                    if *branch == majority {
                        admitted_flags.insert(server.clone(), true);
                    } else {
                        excluded.push((server.clone(), ExclusionReason::OutvotedBranch {
                            reported: *branch,
                            majority,
                        }));
                    }
                }
                if wallet.recorded != Some(majority) {
                    branch_to_record = Some(majority);
                }
            },
        }
    }

    let admitted: Vec<String> = probes
        .iter()
        .filter(|(server, _)| admitted_flags.contains_key(server))
        .map(|(server, _)| server.clone())
        .collect();
    if admitted.is_empty() {
        return Err(if reachable == 0 {
            LightwalletdError::AllUnreachable(
                unreachable
                    .iter()
                    .map(|(server, e)| format!("{}: {}", server, e))
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        } else {
            LightwalletdError::AllExcluded(describe_excluded(&excluded))
        });
    }
    Ok(AdmissionOutcome {
        admitted,
        excluded,
        unreachable,
        branch_to_record,
    })
}

/// Where a wallet's recorded consensus branch lives and whether it holds scanned state.
pub(crate) trait WalletBranchStore: Send + Sync {
    fn state(&self) -> Result<WalletBranchState, String>;
    fn record(&self, branch: u32) -> Result<(), String>;
}

/// The recorded identifier is a sidecar of the wallet database. The wallet database itself cannot
/// carry it: its schema is fingerprinted and an unknown table makes the file unrecognizable.
pub(crate) struct FileBranchStore {
    path: PathBuf,
    history: ZCoinShieldedHistory,
}

impl FileBranchStore {
    pub(crate) fn new(history: ZCoinShieldedHistory) -> Self {
        let mut name = history.wallet_db_path().as_os_str().to_os_string();
        name.push(".lightwalletd-branch");
        FileBranchStore {
            path: PathBuf::from(name),
            history,
        }
    }
}

impl WalletBranchStore for FileBranchStore {
    fn state(&self) -> Result<WalletBranchState, String> {
        let has_scanned_state = self.history.has_scanned_blocks()?;
        let recorded = match std::fs::read_to_string(&self.path) {
            Ok(text) => parse_branch_id(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("cannot read {}: {}", self.path.display(), e)),
        };
        Ok(WalletBranchState {
            has_scanned_state,
            recorded,
        })
    }

    fn record(&self, branch: u32) -> Result<(), String> {
        std::fs::write(&self.path, format!("{:08x}\n", branch))
            .map_err(|e| format!("cannot write {}: {}", self.path.display(), e))
    }
}

// ---------------------------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------------------------

/// A transaction as the service returns it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RawTx {
    pub data: Vec<u8>,
    /// `None` while the transaction is unmined.
    pub height: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TransparentUtxo {
    /// Internal (wire) byte order.
    pub txid: [u8; 32],
    pub index: u32,
    pub value_zat: i64,
    pub height: u64,
    /// The output's script-pubkey as the index returned it. The pubkey-hash key type is keyed by
    /// twenty bytes alone, so a row is only the HTLC's once this matches (R39.11.4d(c)).
    pub script: Vec<u8>,
}

/// What one transparent-history window request delivered. A stream that fails part-way still
/// carries what it delivered before failing (R39.11.4b(e)).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct WindowRead {
    pub txs: Vec<RawTx>,
    pub error: Option<WindowError>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WindowError {
    /// The node could not serve a transaction in the window (R39.11.4b): bisectable.
    pub serve_failure: bool,
    /// The attempt failed at transport level (R39.11.31(a)): the endpoint is suspended.
    pub transport: bool,
    pub message: String,
}

impl From<&EndpointError> for WindowError {
    /// Only a service answer or a "not found" is the node failing to serve a transaction of the
    /// window. A transport failure, and the node's no-address-index answer (R39.11.4i(e)), end the
    /// read instead of bisecting it.
    fn from(error: &EndpointError) -> Self {
        WindowError {
            serve_failure: matches!(error, EndpointError::Service(_) | EndpointError::NotFound(_)),
            transport: matches!(error, EndpointError::Transport(_)),
            message: error.to_string(),
        }
    }
}

/// The wire to one endpoint. A trait so the logic above it can be driven without a network.
#[async_trait]
pub(crate) trait LightwalletdTransport: Send + Sync + fmt::Debug + 'static {
    async fn light_info(&self, server: &str) -> Result<ServerInfo, EndpointError>;
    async fn latest_block(&self, server: &str) -> Result<u64, EndpointError>;
    /// How far the endpoint's compact-block cache has got (`GetLatestBlock`), read by admission
    /// (R39.11.7a(j)); a separate method so a fixture can make it disagree with `light_info`.
    async fn cache_height(&self, server: &str) -> Result<u64, EndpointError>;
    /// The compact block's block-time field (R39.11.4f).
    async fn block_time(&self, server: &str, height: u64) -> Result<u32, EndpointError>;
    /// The compact block's block-hash field (`GetBlock`), the 32 bytes in the order the service returns
    /// them, which is the order of the double SHA-256 of the full block header (R39.11.25(a)).
    async fn block_hash(&self, server: &str, height: u64) -> Result<[u8; 32], EndpointError>;
    /// `txid` is in internal byte order (R39.11.4c).
    async fn transaction(&self, server: &str, txid: [u8; 32]) -> Result<RawTx, EndpointError>;
    async fn send_transaction(&self, server: &str, data: Vec<u8>) -> Result<(), EndpointError>;
    async fn taddr_balance(&self, server: &str, addresses: Vec<String>) -> Result<i64, EndpointError>;
    async fn taddr_txids(&self, server: &str, address: &str, start: u64, end: u64) -> WindowRead;
    async fn address_utxos(&self, server: &str, address: &str) -> Result<Vec<TransparentUtxo>, EndpointError>;
}

/// The balance request: the address list is the dictated **flat repeated** string field. A nested
/// submessage is accepted by the transport and rejected by the node for every address (R39.11.4a).
pub(crate) fn balance_request(addresses: &[String]) -> z_coin_grpc::AddressList {
    z_coin_grpc::AddressList {
        addresses: addresses.to_vec(),
    }
}

fn mined_height(raw: u64) -> Option<u64> {
    // 0 and the unsigned reading of -1 both mean "not mined".
    if raw == 0 || raw > i64::MAX as u64 {
        None
    } else {
        Some(raw)
    }
}

/// The gRPC status codes that count as a transport-level failure of the attempt: the endpoint is suspended
/// after one (R39.11.31(a)).
pub(crate) fn is_transport_code(code: Code) -> bool {
    matches!(
        code,
        Code::Unavailable | Code::DeadlineExceeded | Code::Cancelled | Code::Aborted | Code::ResourceExhausted
    )
}

fn status_error(status: tonic::Status) -> EndpointError {
    let message = format!("{}: {}", status.code(), status.message());
    if mentions_no_address_index(status.message()) {
        return EndpointError::NoAddressIndex(message);
    }
    if is_transport_code(status.code()) {
        return EndpointError::Transport(message);
    }
    match status.code() {
        Code::NotFound => EndpointError::NotFound(message),
        _ => EndpointError::Service(message),
    }
}

/// What the callers that walk the endpoints themselves (the compact-block download, the mempool poll) tell the
/// backend about each endpoint, so that the suspension of R39.11.31 sees every attempt.
pub(crate) trait EndpointHealth: Send + Sync {
    /// The name a text uses for `server` (R39.11.29).
    fn name(&self, server: &str) -> String;
    /// An attempt on `server` failed at transport level (R39.11.31(a)).
    fn transport_failed(&self, server: &str, failure: &str);
    /// `server` answered (R39.11.31(b)).
    fn answered(&self, server: &str);
}

impl EndpointHealth for LightwalletdBackend {
    fn name(&self, server: &str) -> String { LightwalletdBackend::name(self, server) }

    fn transport_failed(&self, server: &str, failure: &str) { self.suspend_on_failure(server, failure) }

    fn answered(&self, server: &str) { self.reset_suspension(server) }
}

type GrpcClient = super::z_coin_wallet_db::LightwalletdClient;

/// The real gRPC transport. Channels are kept per endpoint and dropped after a failure.
#[derive(Default)]
pub(crate) struct GrpcTransport {
    clients: Mutex<HashMap<String, GrpcClient>>,
}

impl fmt::Debug for GrpcTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("GrpcTransport") }
}

impl GrpcTransport {
    async fn client(&self, server: &str) -> Result<GrpcClient, EndpointError> {
        if let Some(client) = self.clients.lock().get(server).cloned() {
            return Ok(client);
        }
        // `connect_lightwalletd` already scrubs the endpoint's URL out of its error text.
        let client = ZCoinShieldedHistory::connect_lightwalletd(server)
            .await
            .map_err(EndpointError::Transport)?;
        self.clients.lock().insert(server.to_owned(), client.clone());
        Ok(client)
    }

    fn evict_on_transport_failure<T>(
        &self,
        server: &str,
        result: Result<T, EndpointError>,
    ) -> Result<T, EndpointError> {
        if matches!(result, Err(EndpointError::Transport(_))) {
            self.clients.lock().remove(server);
        }
        result
    }

    async fn timed<T>(
        fut: impl std::future::Future<Output = Result<tonic::Response<T>, tonic::Status>>,
        what: &'static str,
    ) -> Result<T, EndpointError> {
        tokio::time::timeout(CALL_TIMEOUT, fut)
            .await
            .map_err(|_| EndpointError::Transport(format!("{} timed out", what)))?
            .map(tonic::Response::into_inner)
            .map_err(status_error)
    }
}

#[async_trait]
impl LightwalletdTransport for GrpcTransport {
    async fn light_info(&self, server: &str) -> Result<ServerInfo, EndpointError> {
        let mut client = self.client(server).await?;
        let result = Self::timed(client.get_lightd_info(z_coin_grpc::Empty {}), "GetLightdInfo")
            .await
            .map(|info| ServerInfo {
                chain_name: info.chain_name,
                sapling_activation_height: info.sapling_activation_height,
                consensus_branch_id: info.consensus_branch_id,
                block_height: info.block_height,
            });
        self.evict_on_transport_failure(server, result)
    }

    async fn latest_block(&self, server: &str) -> Result<u64, EndpointError> {
        let mut client = self.client(server).await?;
        let result = Self::timed(client.get_latest_block(z_coin_grpc::ChainSpec {}), "GetLatestBlock")
            .await
            .map(|block| block.height);
        self.evict_on_transport_failure(server, result)
    }

    async fn cache_height(&self, server: &str) -> Result<u64, EndpointError> { self.latest_block(server).await }

    async fn block_time(&self, server: &str, height: u64) -> Result<u32, EndpointError> {
        let mut client = self.client(server).await?;
        let request = z_coin_grpc::BlockId {
            height,
            hash: Vec::new(),
        };
        let result = Self::timed(client.get_block(request), "GetBlock")
            .await
            .map(|block| block.time);
        self.evict_on_transport_failure(server, result)
    }

    async fn block_hash(&self, server: &str, height: u64) -> Result<[u8; 32], EndpointError> {
        let mut client = self.client(server).await?;
        let request = z_coin_grpc::BlockId {
            height,
            hash: Vec::new(),
        };
        let result = Self::timed(client.get_block(request), "GetBlock")
            .await
            .and_then(|block| {
                <[u8; 32]>::try_from(block.hash.as_slice()).map_err(|_| {
                    EndpointError::Service(format!(
                        "GetBlock of height {} returned a {}-byte block hash, not 32 bytes",
                        height,
                        block.hash.len()
                    ))
                })
            });
        self.evict_on_transport_failure(server, result)
    }

    async fn transaction(&self, server: &str, txid: [u8; 32]) -> Result<RawTx, EndpointError> {
        let mut client = self.client(server).await?;
        // Only the hash selector is served; {block, index} is not (R39.11.4c).
        let filter = z_coin_grpc::TxFilter {
            block: None,
            index: 0,
            hash: txid.to_vec(),
        };
        let result = Self::timed(client.get_transaction(filter), "GetTransaction")
            .await
            .map(|raw| RawTx {
                data: raw.data,
                height: mined_height(raw.height),
            });
        self.evict_on_transport_failure(server, result)
    }

    async fn send_transaction(&self, server: &str, data: Vec<u8>) -> Result<(), EndpointError> {
        let mut client = self.client(server).await?;
        let result = Self::timed(
            client.send_transaction(z_coin_grpc::RawTransaction { data, height: 0 }),
            "SendTransaction",
        )
        .await
        .and_then(|response| {
            if response.error_code == 0 {
                Ok(())
            } else {
                Err(EndpointError::Rejected {
                    code: response.error_code,
                    message: response.error_message,
                })
            }
        });
        self.evict_on_transport_failure(server, result)
    }

    async fn taddr_balance(&self, server: &str, addresses: Vec<String>) -> Result<i64, EndpointError> {
        let mut client = self.client(server).await?;
        let result = Self::timed(
            client.get_taddress_balance(balance_request(&addresses)),
            "GetTaddressBalance",
        )
        .await
        .map(|balance| balance.value_zat);
        self.evict_on_transport_failure(server, result)
    }

    async fn taddr_txids(&self, server: &str, address: &str, start: u64, end: u64) -> WindowRead {
        // `start = 0` silently means the whole chain (R39.11.4b(b)): never send it.
        if start == 0 || end == 0 || start > end {
            return WindowRead {
                txs: Vec::new(),
                error: Some(WindowError {
                    serve_failure: false,
                    transport: false,
                    message: format!("refusing the history range {}..={}", start, end),
                }),
            };
        }
        let mut client = match self.client(server).await {
            Ok(client) => client,
            Err(e) => {
                return WindowRead {
                    txs: Vec::new(),
                    error: Some(WindowError {
                        serve_failure: false,
                        transport: matches!(e, EndpointError::Transport(_)),
                        message: e.to_string(),
                    }),
                }
            },
        };
        let filter = z_coin_grpc::TransparentAddressBlockFilter {
            address: address.to_owned(),
            range: Some(z_coin_grpc::BlockRange {
                start: Some(z_coin_grpc::BlockId {
                    height: start,
                    hash: Vec::new(),
                }),
                end: Some(z_coin_grpc::BlockId {
                    height: end,
                    hash: Vec::new(),
                }),
            }),
        };
        let mut stream = match Self::timed(client.get_taddress_txids(filter), "GetTaddressTxids").await {
            Ok(stream) => stream,
            Err(e) => {
                let error = WindowError::from(&e);
                if !error.serve_failure {
                    self.clients.lock().remove(server);
                }
                return WindowRead {
                    txs: Vec::new(),
                    error: Some(error),
                };
            },
        };
        let mut txs = Vec::new();
        loop {
            match tokio::time::timeout(CALL_TIMEOUT, stream.next()).await {
                Err(_) => {
                    self.clients.lock().remove(server);
                    return WindowRead {
                        txs,
                        error: Some(WindowError {
                            serve_failure: false,
                            transport: true,
                            message: "GetTaddressTxids stream timed out".to_owned(),
                        }),
                    };
                },
                Ok(None) => return WindowRead { txs, error: None },
                Ok(Some(Ok(raw))) => txs.push(RawTx {
                    data: raw.data,
                    height: mined_height(raw.height),
                }),
                Ok(Some(Err(status))) => {
                    // The stream ends on the first transaction the node cannot serve; the
                    // transactions delivered before it are kept (R39.11.4b(e)).
                    return WindowRead {
                        txs,
                        error: Some(WindowError::from(&status_error(status))),
                    };
                },
            }
        }
    }

    async fn address_utxos(&self, server: &str, address: &str) -> Result<Vec<TransparentUtxo>, EndpointError> {
        let mut client = self.client(server).await?;
        let arg = z_coin_grpc::GetAddressUtxosArg {
            address: address.to_owned(),
            start_height: 0,
            max_entries: 0,
        };
        let result = Self::timed(client.get_address_utxos(arg), "GetAddressUtxos")
            .await
            .and_then(|reply| {
                reply
                    .address_utxos
                    .into_iter()
                    .map(|utxo| {
                        let txid: [u8; 32] = utxo.txid.as_slice().try_into().map_err(|_| {
                            EndpointError::Service(format!("GetAddressUtxos returned a {}-byte txid", utxo.txid.len()))
                        })?;
                        let index = u32::try_from(utxo.index).map_err(|_| {
                            EndpointError::Service(format!("GetAddressUtxos returned output index {}", utxo.index))
                        })?;
                        Ok(TransparentUtxo {
                            txid,
                            index,
                            value_zat: utxo.value_zat,
                            height: utxo.height,
                            script: utxo.script,
                        })
                    })
                    .collect::<Result<Vec<_>, EndpointError>>()
            });
        self.evict_on_transport_failure(server, result)
    }
}

// ---------------------------------------------------------------------------------------------
// Transparent history (R39.11.4b)
// ---------------------------------------------------------------------------------------------

/// What a windowed history read established.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct HistoryRead {
    /// Deduplicated by transaction content, in delivery order.
    pub transactions: Vec<RawTx>,
    /// Heights the node could not serve and the read skipped. A skipped height is recorded, never
    /// forgotten.
    pub gaps: Vec<u64>,
    /// A failure that ended the read before `to`. The transactions delivered before it are kept.
    pub failure: Option<String>,
    pub highest_received_height: Option<u64>,
    /// The number of window requests the read made (R39.11.4h).
    pub requests: usize,
    /// The read ended under the unservable-heights limit of R39.11.4h(b): its gaps are artifacts of an
    /// endpoint that refuses requests, not transactions the node cannot serve.
    pub stopped_on_unservable_heights: bool,
}

impl HistoryRead {
    /// Only a read that completed with no gap and no failure may be reported as a negative answer
    /// ("no match in this window"). Anything else leaves a swap-path decision undetermined
    /// (R39.11.4b(f)); skipping a height is acceptable for keeping history and sync moving, never as
    /// the basis of "not spent" over a range containing it.
    pub(crate) fn is_clean(&self) -> bool { self.gaps.is_empty() && self.failure.is_none() }
}

/// How far a window may be widened when it delivers nothing beyond what the read already holds,
/// as a multiple of the base window. See [`read_history`].
const HISTORY_MAX_WIDEN_FACTOR: u64 = 16;

/// R39.11.4h(b): a window holding more than this many heights the endpoint could not serve ends the
/// read. Bisection assumes a serve failure belongs to one transaction; this many failing heights in
/// one window says it does not.
pub(crate) const HTLC_HISTORY_MAX_UNSERVABLE_HEIGHTS: usize = 4;

/// R39.11.4h(b): a read ends with a named failure after this many requests.
pub(crate) const HTLC_HISTORY_MAX_REQUESTS: usize = 1_024;

/// R39.11.4h(c): the time one endpoint may take per HTLC detection, both address forms together.
pub(crate) const HTLC_DETECTION_ENDPOINT_BUDGET: Duration = Duration::from_secs(120);

/// The limits that end a [`read_history`] with a named failure.
#[derive(Clone, Copy, Debug)]
pub(crate) struct HistoryLimits {
    /// The read ends once one window holds more than this many unservable heights.
    pub max_unservable_heights_per_window: usize,
    /// The read ends after this many requests.
    pub max_requests: usize,
}

impl HistoryLimits {
    /// No limit: the reader's behaviour of R39.11.4b alone.
    pub(crate) const UNBOUNDED: HistoryLimits = HistoryLimits {
        max_unservable_heights_per_window: usize::MAX,
        max_requests: usize::MAX,
    };
    /// The limits of an HTLC detection (R39.11.4h(b)).
    pub(crate) const HTLC: HistoryLimits = HistoryLimits {
        max_unservable_heights_per_window: HTLC_HISTORY_MAX_UNSERVABLE_HEIGHTS,
        max_requests: HTLC_HISTORY_MAX_REQUESTS,
    };
}

/// Reads `from..=to` of an address's transparent history in bounded windows.
///
/// * Every request is bounded by `window` (or a bounded widening of it) and both its bounds are at
///   least 1 -- `start = 0` silently means the whole chain.
/// * When the node cannot serve a transaction the window is **bisected** and the offending height
///   skipped and recorded, rather than failing the pass.
/// * The service drops the last height group of every stream, so the next window resumes from the
///   last received height, **not one past it**; transactions are deduplicated across windows. A
///   window that delivers nothing beyond what the read already holds is widened (up to
///   [`HISTORY_MAX_WIDEN_FACTOR`] times) from the same cursor, because the group it dropped can only
///   be delivered by a window that also contains a later group.
/// * Whatever a failing stream delivered is retained.
///
/// * `limits` end the read with a named failure (R39.11.4h(b)): bisection assumes a serve failure
///   belongs to one transaction, so a window with more unservable heights than the limit, or a read
///   that has made the permitted number of requests, stops instead of walking the range one height
///   at a time. What was delivered so far is retained and the read is not clean.
///
/// Two residues cannot be repaired from here, because the service never delivers the data: the
/// newest height group of the whole range, and a group isolated from any later group by more than
/// the widest window. Neither is an answer; a caller that needs the newest group has to corroborate
/// it elsewhere (the funding-transaction proof of R39.11.4d). `highest_received_height` is the
/// ceiling of what was actually delivered.
pub(crate) async fn read_history<F, Fut>(
    from: u64,
    to: u64,
    window: u64,
    limits: HistoryLimits,
    mut fetch: F,
) -> Result<HistoryRead, LightwalletdError>
where
    F: FnMut(u64, u64) -> Fut,
    Fut: std::future::Future<Output = WindowRead>,
{
    if window == 0 {
        return Err(LightwalletdError::InvalidRequest(
            "history window must be non-zero".to_owned(),
        ));
    }
    let from = from.max(1);
    if to < from {
        return Err(LightwalletdError::InvalidRequest(format!(
            "history range {}..={} is empty or open-ended",
            from, to
        )));
    }

    let max_width = window.saturating_mul(HISTORY_MAX_WIDEN_FACTOR);
    let mut width = window;
    let mut out = HistoryRead::default();
    let mut seen: HashSet<[u8; 32]> = HashSet::new();
    let mut cursor = from;
    while cursor <= to {
        let window_end = cursor.saturating_add(width - 1).min(to);
        let mut window_highest: Option<u64> = None;
        let mut window_gap_max: Option<u64> = None;
        let mut window_unservable: usize = 0;
        let mut pending = vec![(cursor, window_end)];
        while let Some((start, end)) = pending.pop() {
            if out.requests >= limits.max_requests {
                out.failure = Some(format!(
                    "the read made {} requests without completing the range {}..={}; stopped at {}..={}",
                    out.requests, from, to, start, end
                ));
                return Ok(out);
            }
            out.requests += 1;
            let read = fetch(start, end).await;
            for tx in read.txs {
                if let Some(height) = tx.height {
                    window_highest = Some(window_highest.map_or(height, |h| h.max(height)));
                    out.highest_received_height = Some(out.highest_received_height.map_or(height, |h| h.max(height)));
                }
                let digest: [u8; 32] = Sha256::digest(&tx.data).into();
                if seen.insert(digest) {
                    out.transactions.push(tx);
                }
            }
            match read.error {
                None => {},
                Some(error) if error.serve_failure => {
                    if start == end {
                        out.gaps.push(start);
                        window_gap_max = Some(window_gap_max.map_or(start, |g| g.max(start)));
                        window_unservable += 1;
                        if window_unservable > limits.max_unservable_heights_per_window {
                            out.stopped_on_unservable_heights = true;
                            out.failure = Some(format!(
                                "could not serve {} heights of window {}..={} (last error: {}); the failure is not \
                                 confined to single transactions",
                                window_unservable, cursor, window_end, error.message
                            ));
                            return Ok(out);
                        }
                    } else {
                        let mid = start + (end - start) / 2;
                        // Lower half first.
                        pending.push((mid + 1, end));
                        pending.push((start, mid));
                    }
                },
                Some(error) => {
                    out.failure = Some(error.message);
                    return Ok(out);
                },
            }
        }

        // Resume from the last received height, never one past it; always move beyond a height
        // that was skipped as a gap, or the same failure would be found again.
        let received = window_highest.filter(|received| *received > cursor);
        let past_gap = window_gap_max.map(|gap| gap + 1).filter(|next| *next > cursor);
        match received.into_iter().chain(past_gap).max() {
            Some(next) => {
                cursor = next;
                width = window;
            },
            None if window_end >= to => break,
            None if width < max_width => width = width.saturating_mul(2).min(max_width),
            None => {
                cursor = window_end + 1;
                width = window;
            },
        }
    }
    out.gaps.sort_unstable();
    out.gaps.dedup();
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// The single chain-height authority (R39.11.9 - R39.11.11)
// ---------------------------------------------------------------------------------------------

/// A tip read together with the highest tip this activation had observed before it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FreshTip {
    pub read: u64,
    pub highest_before: u64,
}

impl FreshTip {
    /// The height the coin reports and scans towards: never lower than one already observed
    /// (R39.11.10).
    pub(crate) fn monotonic(&self) -> u64 { self.read.max(self.highest_before) }

    /// The height confirmations and expiry are judged against: the tip just read, not the highest
    /// ever observed (R39.11.10a). A lagging or reorganized endpoint then delays those decisions
    /// instead of overcounting a payment's confirmations.
    pub(crate) fn confirmation_height(&self) -> u64 { self.read }
}

/// The target height every shielded construction path hands its builder (R39.11.11): the
/// authority's tip plus one, refused when it cannot be corroborated.
///
/// A target more than the builder's expiry delta behind the chain yields a transaction that is
/// expired the instant it is broadcast, and the network reports that as an opaque broadcast
/// failure. `observed_floor` is the highest height this activation has seen from any source
/// (earlier tip reads, the wallet's scanned height). There is deliberately no fallback: not to the
/// wallet database's own target height, not to a last known tip, not to the scanned height.
pub(crate) fn builder_target_height(tip: FreshTip, scanned_through: u64) -> Result<u32, LightwalletdError> {
    let observed = tip.highest_before.max(scanned_through);
    if observed.saturating_sub(tip.read) > u64::from(BUILDER_EXPIRY_DELTA) {
        return Err(LightwalletdError::StaleTip {
            tip: tip.read,
            observed,
        });
    }
    tip.monotonic()
        .checked_add(1)
        .and_then(|target| u32::try_from(target).ok())
        .ok_or_else(|| LightwalletdError::InvalidRequest(format!("chain tip {} does not fit a block height", tip.read)))
}

/// Confirmations derived locally (R39.11.4e): `tip + 1 - mined height`, zero when unmined or when
/// the height exceeds the tip.
pub(crate) fn confirmation_depth(tip: u64, mined_height: Option<u64>) -> u64 {
    match mined_height {
        Some(height) if height >= 1 && height <= tip => tip + 1 - height,
        _ => 0,
    }
}

// ---------------------------------------------------------------------------------------------
// The backend
// ---------------------------------------------------------------------------------------------

struct CachedAdmission {
    at: Instant,
    outcome: AdmissionOutcome,
}

/// Reads the wall clock, in seconds since the epoch; a test injects its own (R39.11.31(b)).
type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

pub(crate) struct LightwalletdBackend {
    transport: Arc<dyn LightwalletdTransport>,
    /// The configured endpoints, in configured order, as the transport connects to them. They are credentials:
    /// a text names an endpoint through [`Self::name`].
    servers: Vec<String>,
    /// Per endpoint of [`Self::servers`], the host-only name every log line, error text and status message
    /// uses (R39.11.29). Unique within the list.
    labels: Vec<String>,
    policy: AdmissionPolicy,
    /// How the coin encodes the two address forms of an HTLC; script-hash queries from the generic
    /// address methods are refused, the HTLC probe is the only caller that issues them (R39.11.4d).
    addressing: HtlcAddressing,
    /// Which address form last returned the HTLC's rows, per endpoint (R39.11.4d(b)). Informative:
    /// both forms are probed on every query.
    index_encodings: Mutex<HashMap<String, htlc_index::IndexEncoding>>,
    wallet: Mutex<Option<Arc<dyn WalletBranchStore>>>,
    cached: Mutex<Option<CachedAdmission>>,
    evaluating: AsyncMutex<()>,
    highest_tip: AtomicU64,
    /// The time one endpoint may take per HTLC detection (R39.11.4h(c)).
    endpoint_budget: Duration,
    /// Endpoints whose node keeps no address index, with the node's message (R39.11.4i). Kept for
    /// the life of this backend; admission re-evaluation does not clear it.
    no_address_index: Mutex<HashMap<String, String>>,
    /// What the HTLC detection in flight for a redeem-script hash read last (R39.5.5).
    detection_progress: Mutex<HashMap<[u8; 20], String>>,
    /// The suspension of each endpoint that failed at transport level (R39.11.31). Held in memory for the life
    /// of this backend: a new activation starts with none, and re-evaluating admission clears none.
    suspensions: Mutex<HashMap<String, Suspension>>,
    clock: Clock,
}

impl fmt::Debug for LightwalletdBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LightwalletdBackend")
            .field("servers", &self.labels)
            .finish()
    }
}

impl LightwalletdBackend {
    pub(crate) fn new(
        transport: Arc<dyn LightwalletdTransport>,
        servers: Vec<String>,
        policy: AdmissionPolicy,
        addressing: HtlcAddressing,
    ) -> Self {
        // Every construction path ends here, so each endpoint votes once everywhere (R39.11.7b).
        let configured = servers;
        let (servers, duplicates) = dedupe_lightwalletd_endpoints(configured.clone());
        let position = |spelling: &str| configured.iter().position(|s| s == spelling).unwrap_or_default();
        for (dropped, kept) in &duplicates {
            common::log::warn!(
                "lightwalletd endpoint {} names the same endpoint as {} and is ignored",
                lightwalletd_endpoint_label(dropped, position(dropped)),
                lightwalletd_endpoint_label(kept, position(kept))
            );
        }
        let mut labels: Vec<String> = Vec::with_capacity(servers.len());
        for server in &servers {
            let mut label = lightwalletd_endpoint_label(server, position(server));
            // Two endpoints on one host differ only by a path; the label stays unique within the list.
            if labels.contains(&label) {
                label = format!("{} (entry {})", label, position(server) + 1);
            }
            labels.push(label);
        }
        LightwalletdBackend {
            transport,
            servers,
            labels,
            policy,
            addressing,
            index_encodings: Mutex::new(HashMap::new()),
            wallet: Mutex::new(None),
            cached: Mutex::new(None),
            evaluating: AsyncMutex::new(()),
            highest_tip: AtomicU64::new(0),
            endpoint_budget: HTLC_DETECTION_ENDPOINT_BUDGET,
            no_address_index: Mutex::new(HashMap::new()),
            detection_progress: Mutex::new(HashMap::new()),
            suspensions: Mutex::new(HashMap::new()),
            clock: Arc::new(common::now_float),
        }
    }

    /// The name a text uses for `server`, one of the endpoints this backend was built with (R39.11.29): its
    /// host-only form, or a placeholder with its position in the configured list.
    pub(crate) fn name(&self, server: &str) -> String {
        match self.servers.iter().position(|s| s == server) {
            Some(index) => self.labels[index].clone(),
            None => lightwalletd_endpoint_label(server, 0),
        }
    }

    /// Replaces the clock the suspensions are measured by.
    #[cfg(test)]
    pub(crate) fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// Removes the endpoint's URL from a lower layer's `text` before it is logged or returned (R39.11.29).
    pub(crate) fn scrub(&self, text: &str, server: &str) -> String { scrub_lightwalletd_text(text, server) }

    // ---- suspension (R39.11.31) ----------------------------------------------------------------

    fn now(&self) -> f64 { (self.clock)() }

    /// Whether `server` is suspended right now.
    pub(crate) fn is_suspended(&self, server: &str) -> bool {
        let now = self.now();
        self.suspensions
            .lock()
            .get(server)
            .map_or(false, |suspension| suspension.is_suspended(now))
    }

    /// Suspends `server` after a transport-level failure (R39.11.31(a), (b)): 20 s the first time, doubling with
    /// each further failure before its next answer, up to 12 hours.
    pub(crate) fn suspend_on_failure(&self, server: &str, failure: &str) {
        let secs = self
            .suspensions
            .lock()
            .entry(server.to_owned())
            .or_default()
            .on_failure(self.now(), SUSPENSION_BASE, SUSPENSION_CAP);
        common::log::debug!(
            "lightwalletd endpoint {} suspended for {:.0} s after a transport-level failure: {}",
            self.name(server),
            secs,
            self.scrub(failure, server)
        );
    }

    /// A successful answer of any method resets `server` (R39.11.31(b)).
    pub(crate) fn reset_suspension(&self, server: &str) {
        if let Some(suspension) = self.suspensions.lock().get_mut(server) {
            suspension.on_success();
        }
    }

    /// `endpoints` with the suspended ones moved behind the others: the unsuspended ones first, in configured
    /// order, then the suspended ones, in configured order (R39.11.31(c), (d)).
    pub(crate) fn unsuspended_first(&self, endpoints: Vec<String>) -> Vec<String> {
        let (suspended, ready): (Vec<String>, Vec<String>) = endpoints.into_iter().partition(|s| self.is_suspended(s));
        ready.into_iter().chain(suspended).collect()
    }

    #[cfg(test)]
    pub(crate) fn with_endpoint_budget(mut self, budget: Duration) -> Self {
        self.endpoint_budget = budget;
        self
    }

    pub(crate) fn bind_wallet(&self, store: Arc<dyn WalletBranchStore>) {
        *self.wallet.lock() = Some(store);
        *self.cached.lock() = None;
    }

    /// The cached admitted endpoints (configured spelling), in configured order.
    fn fresh_admission(&self) -> Option<Vec<String>> {
        self.cached
            .lock()
            .as_ref()
            .filter(|cached| cached.at.elapsed() < ADMISSION_REEVALUATION_INTERVAL)
            .map(|cached| self.to_servers(&cached.outcome.admitted))
    }

    /// The configured spelling of each endpoint named by one of `labels`.
    fn to_servers(&self, labels: &[String]) -> Vec<String> {
        labels
            .iter()
            .filter_map(|label| self.labels.iter().position(|l| l == label))
            .map(|index| self.servers[index].clone())
            .collect()
    }

    /// The endpoints that may be used, in configured order. Re-evaluated every
    /// [`ADMISSION_REEVALUATION_INTERVAL`], never cached for the coin's lifetime.
    ///
    /// The probe asks every configured endpoint, suspended or not (R39.11.31(e)), so that a suspension never
    /// changes the branch vote, the consensus height or the failure reported.
    pub(crate) async fn admitted(&self) -> Result<Vec<String>, LightwalletdError> {
        if let Some(admitted) = self.fresh_admission() {
            return Ok(admitted);
        }
        let _evaluating = self.evaluating.lock().await;
        if let Some(admitted) = self.fresh_admission() {
            return Ok(admitted);
        }

        // Admission works on the endpoints' names, which are unique within the list (R39.11.29).
        let probes = join_all(
            self.servers
                .iter()
                .cloned()
                .zip(self.labels.iter().cloned())
                .map(|(server, label)| {
                    let transport = self.transport.clone();
                    async move {
                        let probe = match tokio::time::timeout(PROBE_TIMEOUT, transport.light_info(&server)).await {
                            Ok(Ok(info)) => {
                                self.reset_suspension(&server);
                                let cache_height =
                                    match tokio::time::timeout(PROBE_TIMEOUT, transport.cache_height(&server)).await {
                                        Ok(result) => result.map_err(|e| self.probe_failure(&server, e)),
                                        Err(_) => {
                                            self.suspend_on_failure(&server, "GetLatestBlock timed out");
                                            Err("GetLatestBlock timed out".to_owned())
                                        },
                                    };
                                Ok(AdmissionProbe { info, cache_height })
                            },
                            Ok(Err(e)) => Err(self.probe_failure(&server, e)),
                            Err(_) => {
                                self.suspend_on_failure(&server, "GetLightdInfo timed out");
                                Err("GetLightdInfo timed out".to_owned())
                            },
                        };
                        (label, probe)
                    }
                }),
        )
        .await;

        let store = self.wallet.lock().clone();
        let wallet_state = match &store {
            Some(store) => store.state().map_err(|e| {
                LightwalletdError::InvalidRequest(format!("cannot read the wallet's consensus branch binding: {}", e))
            })?,
            None => WalletBranchState::default(),
        };

        common::log::info!(
            "lightwalletd admission: probed {} endpoint(s): {}",
            probes.len(),
            describe_probes(&probes)
        );
        let outcome = decide_admission_at(probes, &self.policy, &wallet_state, common::now_ms() / 1000);
        match outcome {
            Ok(outcome) => {
                for (server, reason) in &outcome.excluded {
                    common::log::warn!("lightwalletd endpoint {} excluded: {}", server, reason);
                }
                for (server, error) in &outcome.unreachable {
                    common::log::warn!("lightwalletd endpoint {} unreachable: {}", server, error);
                }
                common::log::info!(
                    "lightwalletd admission: admitted {} of {} endpoint(s): {}",
                    outcome.admitted.len(),
                    self.servers.len(),
                    outcome.admitted.join(", ")
                );
                if let (Some(branch), Some(store)) = (outcome.branch_to_record, &store) {
                    if let Err(e) = store.record(branch) {
                        common::log::warn!("could not record consensus branch {:08x}: {}", branch, e);
                    }
                }
                let admitted = self.to_servers(&outcome.admitted);
                *self.cached.lock() = Some(CachedAdmission {
                    at: Instant::now(),
                    outcome,
                });
                Ok(admitted)
            },
            Err(e) => {
                common::log::error!("lightwalletd admission failed, no endpoint is usable: {}", e);
                *self.cached.lock() = None;
                Err(e)
            },
        }
    }

    /// What one failed admission read of `server` becomes: a text free of the endpoint's URL, and a suspension
    /// when the failure is at transport level (R39.11.31(e)).
    fn probe_failure(&self, server: &str, error: EndpointError) -> String {
        let text = self.scrub(&error.to_string(), server);
        if matches!(error, EndpointError::Transport(_)) {
            self.suspend_on_failure(server, &text);
        }
        text
    }

    /// The admitted endpoints in the order an operation tries them: those that are not suspended in configured
    /// order, then the suspended ones in configured order (R39.11.31(c), (d)). Admission itself is unchanged by
    /// suspension.
    pub(crate) async fn admitted_ordered(&self) -> Result<Vec<String>, LightwalletdError> {
        self.admitted().await.map(|admitted| self.unsuspended_first(admitted))
    }

    /// The endpoints excluded by the latest admission decision, each with its reason
    /// (R39.11.7a(f)). The endpoint is named host-only (R39.11.29).
    pub(crate) fn excluded_endpoints(&self) -> Vec<(String, String)> {
        self.cached
            .lock()
            .as_ref()
            .map(|cached| {
                cached
                    .outcome
                    .excluded
                    .iter()
                    .map(|(server, reason)| (server.clone(), reason.to_string()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Runs `call` on the admitted endpoints: those that are not suspended first, in configured order, then
    /// the suspended ones (R39.11.31). The operation fails only after every admitted endpoint has failed
    /// (R39.11.3); a service-level broadcast rejection stops at once, because every honest endpoint would
    /// reproduce it (R39.11.4), and suspends nothing (R39.11.31(h)).
    ///
    /// Passing admission does not make an endpoint usable for every method -- one endpoint answers
    /// `GetLightdInfo` and then fails the method actually needed -- so a failure here moves on to the
    /// next endpoint, and a total failure forces the next call to re-evaluate admission.
    async fn call<T>(
        &self,
        op: &'static str,
        call: impl Fn(Arc<dyn LightwalletdTransport>, String) -> BoxFuture<'static, Result<T, EndpointError>>,
    ) -> Result<T, LightwalletdError> {
        self.call_for(op, None, call).await
    }

    /// [`Self::call`] for a broadcast: `txid` is the identifier of the transaction, computed locally, that a
    /// rejection names in its log line (R39.11.30).
    async fn call_for<T>(
        &self,
        op: &'static str,
        txid: Option<&str>,
        call: impl Fn(Arc<dyn LightwalletdTransport>, String) -> BoxFuture<'static, Result<T, EndpointError>>,
    ) -> Result<T, LightwalletdError> {
        let admitted = self.admitted_ordered().await?;
        let mut errors = Vec::new();
        let mut explicitly_missing = false;
        for server in admitted {
            match call(self.transport.clone(), server.clone()).await {
                Ok(value) => {
                    self.reset_suspension(&server);
                    return Ok(value);
                },
                Err(EndpointError::Rejected { code, message }) => {
                    return Err(self.rejected(&server, txid, code, message));
                },
                Err(e) => {
                    explicitly_missing |= matches!(e, EndpointError::NotFound(_));
                    let text = self.scrub(&e.to_string(), &server);
                    if matches!(e, EndpointError::Transport(_)) {
                        self.suspend_on_failure(&server, &text);
                    }
                    common::log::debug!("lightwalletd {} on {} failed: {}", op, self.name(&server), text);
                    errors.push(format!("{}: {}", self.name(&server), text));
                },
            }
        }
        *self.cached.lock() = None;
        let errors = errors.join("; ");
        if explicitly_missing {
            return Err(LightwalletdError::NoSuchTransaction(errors));
        }
        Err(LightwalletdError::AllFailed { op, errors })
    }

    /// The error of a broadcast that `server` rejected, and the one `warn` line that says so (R39.11.30). The
    /// rejection does not suspend the endpoint.
    fn rejected(&self, server: &str, txid: Option<&str>, code: i32, message: String) -> LightwalletdError {
        let endpoint = self.name(server);
        let message = self.scrub(&message, server);
        common::log::warn!(
            "lightwalletd endpoint {} rejected transaction {}: code {}, message {}",
            endpoint,
            txid.unwrap_or("(identifier not computed)"),
            code,
            message
        );
        LightwalletdError::BroadcastRejected {
            endpoint,
            code,
            message,
        }
    }

    /// A tip read plus the highest tip seen before it. This is the only height source of the coin.
    pub(crate) async fn fresh_tip(&self) -> Result<FreshTip, LightwalletdError> {
        let read = self
            .call("GetLatestBlock", |t, s| async move { t.latest_block(&s).await }.boxed())
            .await?;
        let highest_before = self.highest_tip.fetch_max(read, AtomicOrdering::SeqCst);
        Ok(FreshTip { read, highest_before })
    }

    /// The coin's current block: monotonic non-decreasing within an activation (R39.11.10).
    pub(crate) async fn chain_tip(&self) -> Result<u64, LightwalletdError> {
        self.fresh_tip().await.map(|tip| tip.monotonic())
    }

    /// The tip confirmations and expiry are judged against: the fresh read, never the monotonic
    /// height (R39.11.10a).
    pub(crate) async fn confirmation_tip(&self) -> Result<u64, LightwalletdError> {
        self.fresh_tip().await.map(|tip| tip.confirmation_height())
    }

    /// What `GetLightdInfo` reports, from the first admitted endpoint that answers.
    pub(crate) async fn server_info(&self) -> Result<ServerInfo, LightwalletdError> {
        self.call("GetLightdInfo", |t, s| async move { t.light_info(&s).await }.boxed())
            .await
    }

    /// The Sapling activation height admission holds every endpoint to: the floor of any block read.
    pub(crate) fn sapling_activation_height(&self) -> u64 { self.policy.sapling_activation_height }

    pub(crate) async fn block_time(&self, height: u64) -> Result<u32, LightwalletdError> {
        self.call("GetBlock", move |t, s| {
            async move { t.block_time(&s, height).await }.boxed()
        })
        .await
    }

    /// The block hash `GetBlock` reports for `height`, from the first admitted endpoint that answers: the
    /// reference the chain-identity check of the Electrum fallback compares a server's header with
    /// (R39.11.25(a)).
    pub(crate) async fn block_hash(&self, height: u64) -> Result<[u8; 32], LightwalletdError> {
        self.call("GetBlock", move |t, s| {
            async move { t.block_hash(&s, height).await }.boxed()
        })
        .await
    }

    pub(crate) async fn median_time_past(&self, starting_block: u64, count: u64) -> Result<u32, LightwalletdError> {
        if count == 0 || count > MEDIAN_TIME_WINDOW_MAX {
            return Err(LightwalletdError::InvalidRequest(format!(
                "median-time window of {} blocks is outside 1..={}",
                count, MEDIAN_TIME_WINDOW_MAX
            )));
        }
        let first = starting_block.saturating_sub(count - 1).max(1);
        let mut times = Vec::with_capacity(count as usize);
        for height in first..=starting_block {
            times.push(self.block_time(height).await?);
        }
        median(times.as_mut_slice())
            .ok_or_else(|| LightwalletdError::InvalidRequest("empty median-time window".to_owned()))
    }

    pub(crate) async fn transaction(&self, txid_internal: [u8; 32]) -> Result<RawTx, LightwalletdError> {
        self.call("GetTransaction", move |t, s| {
            async move { t.transaction(&s, txid_internal).await }.boxed()
        })
        .await
    }

    pub(crate) async fn send_transaction(&self, data: Vec<u8>) -> Result<(), LightwalletdError> {
        // The identifier is computed from the bytes, as the broadcast path does (the service answers with a
        // status only); a rejection names it (R39.11.30).
        let txid = ZTransaction::read(data.as_slice(), BranchId::Sapling)
            .map(|parsed| format!("{:?}", txid_internal_to_display(*parsed.txid().as_ref())))
            .unwrap_or_else(|_| "(unreadable transaction)".to_owned());
        self.call_for("SendTransaction", Some(&txid), move |t, s| {
            let data = data.clone();
            async move { t.send_transaction(&s, data).await }.boxed()
        })
        .await
    }

    fn refuse_script_hash(&self, address: &Address, op: &'static str) -> Result<(), LightwalletdError> {
        if address.prefix == self.addressing.p2sh_prefix {
            return Err(LightwalletdError::Unsupported {
                op,
                reason: "the serving node's address index files every output under the pubkey-hash key type, so a \
                         script-hash address returns an empty success forever (R39.11.4d); an empty result would \
                         read as 'not spent' or 'zero balance'",
            });
        }
        Ok(())
    }

    pub(crate) async fn taddr_balance(&self, addresses: Vec<Address>) -> Result<i64, LightwalletdError> {
        for address in &addresses {
            self.refuse_script_hash(address, "GetTaddressBalance")?;
        }
        let addresses: Vec<String> = addresses.iter().map(|a| a.to_string()).collect();
        self.call("GetTaddressBalance", move |t, s| {
            let addresses = addresses.clone();
            async move { t.taddr_balance(&s, addresses).await }.boxed()
        })
        .await
    }

    pub(crate) async fn address_utxos(&self, address: &Address) -> Result<Vec<TransparentUtxo>, LightwalletdError> {
        self.refuse_script_hash(address, "GetAddressUtxos")?;
        let address = address.to_string();
        self.call("GetAddressUtxos", move |t, s| {
            let address = address.clone();
            async move { t.address_utxos(&s, &address).await }.boxed()
        })
        .await
    }
}

// ---------------------------------------------------------------------------------------------
// The UTXO RPC client
// ---------------------------------------------------------------------------------------------

/// The Light-mode ZHTLC coin's UTXO RPC client (R39.11.4g). Cheap to clone.
#[derive(Clone, Debug)]
pub struct LightwalletdClient(pub(crate) Arc<LightwalletdBackend>);

impl LightwalletdClient {
    pub(crate) fn new(
        transport: Arc<dyn LightwalletdTransport>,
        servers: Vec<String>,
        params: &ZcoinConsensusParams,
        addressing: HtlcAddressing,
    ) -> Self {
        LightwalletdClient(Arc::new(LightwalletdBackend::new(
            transport,
            servers,
            AdmissionPolicy::from_params(params),
            addressing,
        )))
    }

    pub(crate) fn connect(
        servers: Vec<String>,
        params: &ZcoinConsensusParams,
        ironwood_activation_time: Option<u32>,
        addressing: HtlcAddressing,
    ) -> Self {
        LightwalletdClient(Arc::new(LightwalletdBackend::new(
            Arc::new(GrpcTransport::default()),
            servers,
            AdmissionPolicy::from_params(params).with_ironwood_activation_time(ironwood_activation_time),
            addressing,
        )))
    }

    pub(crate) fn backend(&self) -> &Arc<LightwalletdBackend> { &self.0 }

    /// Raw transaction bytes and mined height by txid in display order.
    pub(crate) async fn raw_transaction(&self, txid_display: &H256Json) -> Result<RawTx, LightwalletdError> {
        self.0.transaction(txid_display_to_internal(txid_display)).await
    }

    /// Waits until `tx_hash` (display order) has `confirmations` confirmations, derived locally from
    /// the transaction's mined height and the tip (R39.11.4e). There is no notarized depth to wait
    /// for: a ZHTLC coin has no notarized confirmation count, so `requires_notarization` has no
    /// effect on it and is not taken here (R39.11.4e, D39.11.f).
    ///
    /// A transaction that cannot be fetched while the tip has passed its expiry height is **not**
    /// declared expired unless the service explicitly answered "no such transaction": a backend that
    /// has not indexed it looks identical, and the two are reported differently (R39.11.12).
    pub(crate) async fn wait_for_confirmations(
        &self,
        tx_hash: H256Json,
        expiry_height: u32,
        confirmations: u32,
        wait_until: u64,
        check_every: u64,
    ) -> Result<(), String> {
        loop {
            if common::now_ms() / 1000 > wait_until {
                return Err(format!(
                    "Waited too long until {} for transaction {:?} to be confirmed {} times",
                    wait_until, tx_hash, confirmations
                ));
            }

            match self.raw_transaction(&tx_hash).await {
                Ok(raw) => match self.0.confirmation_tip().await {
                    Ok(tip) => {
                        let depth = confirmation_depth(tip, raw.height);
                        if depth >= u64::from(confirmations) {
                            return Ok(());
                        }
                        common::log::info!(
                            "Waiting for tx {:?} confirmations, now {}, required {}",
                            tx_hash,
                            depth,
                            confirmations
                        );
                    },
                    Err(e) => common::log::error!("could not read the tip while waiting for {:?}: {}", tx_hash, e),
                },
                Err(fetch_error) => {
                    if expiry_height > 0 {
                        match self.0.confirmation_tip().await {
                            Ok(tip) if tip > u64::from(expiry_height) => {
                                return Err(match fetch_error {
                                    LightwalletdError::NoSuchTransaction(_) => {
                                        format!("The transaction {:?} has expired, current block {}", tx_hash, tip)
                                    },
                                    other => format!(
                                        "The transaction {:?} could not be fetched ({}) and the chain tip {} has \
                                         passed its expiry height {}; this does not prove it expired, the backend \
                                         may simply not have indexed it",
                                        tx_hash, other, tip, expiry_height
                                    ),
                                });
                            },
                            Ok(_) => {},
                            Err(e) => {
                                common::log::error!("could not read the tip while waiting for {:?}: {}", tx_hash, e)
                            },
                        }
                    }
                    common::log::error!("could not fetch transaction {:?}: {}, retrying", tx_hash, fetch_error);
                },
            }
            common::executor::Timer::sleep(check_every as f64).await;
        }
    }
}

fn unsupported_fut<T: Send + 'static>(op: &'static str, reason: &'static str) -> UtxoRpcFut<T> {
    let err = LightwalletdError::Unsupported { op, reason };
    Box::new(futures::future::ready(Err(err.into_rpc_error(op))).compat())
}

fn sat_from_zat(value_zat: i64, what: &str) -> Result<u64, MmError<UtxoRpcError>> {
    u64::try_from(value_zat).map_err(|_| {
        MmError::new(UtxoRpcError::InvalidResponse(format!(
            "negative {} {}",
            what, value_zat
        )))
    })
}

#[async_trait]
impl UtxoRpcClientOps for LightwalletdClient {
    fn list_unspent(&self, address: &Address, _decimals: u8) -> UtxoRpcFut<Vec<UnspentInfo>> {
        let this = self.clone();
        let address = address.clone();
        let fut = async move {
            let utxos = this
                .0
                .address_utxos(&address)
                .await
                .map_err(|e| e.into_rpc_error("GetAddressUtxos"))?;
            utxos
                .into_iter()
                .map(|utxo| {
                    Ok(UnspentInfo {
                        outpoint: OutPoint {
                            hash: H256::from(utxo.txid),
                            index: utxo.index,
                        },
                        value: sat_from_zat(utxo.value_zat, "output value")?,
                        height: (utxo.height > 0).then_some(utxo.height),
                    })
                })
                .collect::<UtxoRpcResult<Vec<_>>>()
        };
        Box::new(fut.boxed().compat())
    }

    fn list_unspent_group(&self, addresses: Vec<Address>, decimals: u8) -> UtxoRpcFut<UnspentMap> {
        // The service definition carries no multi-address form: one call per address (R39.11.1).
        let this = self.clone();
        let fut = async move {
            let mut map = UnspentMap::new();
            for address in addresses {
                let unspents = this.list_unspent(&address, decimals).compat().await?;
                map.insert(address, unspents);
            }
            Ok(map)
        };
        Box::new(fut.boxed().compat())
    }

    fn send_transaction(&self, tx: &UtxoTx) -> UtxoRpcFut<H256Json> {
        self.send_raw_transaction(BytesJson::from(serialize(tx).take()))
    }

    fn send_raw_transaction(&self, tx: BytesJson) -> UtxoRpcFut<H256Json> {
        let this = self.clone();
        let fut = async move {
            // The service answers with a status only, so the identifier comes from the bytes. The
            // Zcash reader is used because the shared UTXO parser misreads version 5 and 6.
            let parsed = ZTransaction::read(tx.0.as_slice(), BranchId::Sapling).map_err(|e| {
                MmError::new(UtxoRpcError::InvalidResponse(format!(
                    "cannot read the transaction: {}",
                    e
                )))
            })?;
            let txid = txid_internal_to_display(*parsed.txid().as_ref());
            this.0
                .send_transaction(tx.0)
                .await
                .map_err(|e| e.into_rpc_error("SendTransaction"))?;
            Ok(txid)
        };
        Box::new(fut.boxed().compat())
    }

    fn get_transaction_bytes(&self, txid: &H256Json) -> UtxoRpcFut<BytesJson> {
        let this = self.clone();
        let txid = *txid;
        let fut = async move {
            this.raw_transaction(&txid)
                .await
                .map(|raw| BytesJson::from(raw.data))
                .map_err(|e| e.into_rpc_error("GetTransaction"))
        };
        Box::new(fut.boxed().compat())
    }

    fn get_verbose_transaction(&self, _txid: &H256Json) -> UtxoRpcFut<RpcTransaction> {
        // A verbose transaction carries decoded inputs and outputs, and nothing on this coin's path
        // asks the backend to decode (R39.11.4e). Decoding here would go through the shared UTXO
        // parser, which misreads version 5 and 6.
        unsupported_fut(
            "get_verbose_transaction",
            "lightwalletd relays transaction bytes and a mined height; it decodes nothing",
        )
    }

    fn get_verbose_transactions(&self, _tx_ids: &[H256Json]) -> UtxoRpcFut<Vec<RpcTransaction>> {
        unsupported_fut(
            "get_verbose_transactions",
            "lightwalletd relays transaction bytes and a mined height; it decodes nothing",
        )
    }

    fn get_block_count(&self) -> UtxoRpcFut<u64> {
        let this = self.clone();
        let fut = async move { this.0.chain_tip().await.map_err(|e| e.into_rpc_error("GetLatestBlock")) };
        Box::new(fut.boxed().compat())
    }

    fn display_balance(&self, address: Address, decimals: u8) -> RpcRes<BigDecimal> {
        let this = self.clone();
        let fut = async move {
            let zat = this
                .0
                .taddr_balance(vec![address])
                .await
                .map_err(|e| lightwalletd_json_rpc_error("GetTaddressBalance", e.to_string()))?;
            let sat = u64::try_from(zat)
                .map_err(|_| lightwalletd_json_rpc_error("GetTaddressBalance", format!("negative balance {}", zat)))?;
            Ok(big_decimal_from_sat_unsigned(sat, decimals))
        };
        Box::new(fut.boxed().compat())
    }

    fn display_balances(&self, addresses: Vec<Address>, decimals: u8) -> UtxoRpcFut<Vec<(Address, BigDecimal)>> {
        // The reply is a single summed value with no per-address breakdown (R39.11.4a), so a
        // per-address result takes one call per address.
        let this = self.clone();
        let fut = async move {
            let mut balances = Vec::with_capacity(addresses.len());
            for address in addresses {
                let zat = this
                    .0
                    .taddr_balance(vec![address.clone()])
                    .await
                    .map_err(|e| e.into_rpc_error("GetTaddressBalance"))?;
                let sat = sat_from_zat(zat, "balance")?;
                balances.push((address, big_decimal_from_sat_unsigned(sat, decimals)));
            }
            Ok(balances)
        };
        Box::new(fut.boxed().compat())
    }

    fn estimate_fee_sat(
        &self,
        _decimals: u8,
        _fee_method: &EstimateFeeMethod,
        _mode: &Option<EstimateFeeMode>,
        _n_blocks: u32,
    ) -> UtxoRpcFut<u64> {
        unsupported_fut(
            "estimate_fee_sat",
            "this service exposes no fee estimation; a Light-mode shielded coin must declare a fixed fee",
        )
    }

    fn get_relay_fee(&self) -> RpcRes<BigDecimal> {
        Box::new(
            futures::future::ready(Err(lightwalletd_json_rpc_error(
                "get_relay_fee",
                "this service exposes no relay fee".to_owned(),
            )))
            .compat(),
        )
    }

    fn find_output_spend(
        &self,
        tx_hash: H256,
        script_pubkey: &[u8],
        vout: usize,
        from_block: BlockHashOrHeight,
    ) -> Box<dyn futures01::Future<Item = Option<SpentOutputInfo>, Error = String> + Send> {
        let this = self.clone();
        let output = OutputRef {
            txid: tx_hash.take(),
            vout: vout as u32,
            script_pubkey: script_pubkey.to_vec(),
        };
        let fut = async move {
            let from = match from_block {
                BlockHashOrHeight::Height(height) => u64::try_from(height).unwrap_or(0),
                BlockHashOrHeight::Hash(_) => {
                    return ERR!("HTLC spend detection over lightwalletd searches from a height, not a block hash")
                },
            };
            let log = DetectionLog {
                coin: "an unnamed coin",
                polling: false,
            };
            match this.0.detect_output_spend(&output, from, &log).await {
                Ok(SpendOutcome::NotSpent(_)) => Ok(None),
                Ok(SpendOutcome::Spent(spend)) => {
                    // The shared UTXO transaction type cannot read version 5 or 6, so a ZHTLC coin's
                    // swap path does not come through here (z_tx_read); this trait form is for the
                    // shared callers and fails loudly rather than guessing at a transaction it cannot read.
                    let spending_tx: UtxoTx = deserialize(spend.tx.as_slice()).map_err(|e| {
                        format!(
                            "the spend of the HTLC output was found, but the shared UTXO parser cannot read it ({:?})",
                            e
                        )
                    })?;
                    let height = spend
                        .height
                        .ok_or_else(|| "the spending transaction has no mined height".to_owned())?;
                    Ok(Some(SpentOutputInfo {
                        spending_tx,
                        input_index: spend.input_index,
                        spent_in_block: BlockHashOrHeight::Height(height as i64),
                    }))
                },
                Err(e) => Err(e.to_string()),
            }
        };
        Box::new(fut.boxed().compat())
    }

    fn get_median_time_past(
        &self,
        starting_block: u64,
        count: NonZeroU64,
        _coin_variant: CoinVariant,
    ) -> UtxoRpcFut<u32> {
        let this = self.clone();
        let fut = async move {
            this.0
                .median_time_past(starting_block, count.get())
                .await
                .map_err(|e| e.into_rpc_error("GetBlock"))
        };
        Box::new(fut.boxed().compat())
    }

    async fn get_block_timestamp(&self, height: u64) -> Result<u64, MmError<UtxoRpcError>> {
        self.0
            .block_time(height)
            .await
            .map(u64::from)
            .map_err(|e| e.into_rpc_error("GetBlock"))
    }
}

#[cfg(test)] pub(crate) mod tests;
