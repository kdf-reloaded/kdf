//! Acceptance tests for CRD ch.39 §39.11 (T39.11.a, c, d, f, h, i, j and the byte-order pinning of
//! R39.11.4c). Everything runs against a scripted in-process transport: no network.

use super::super::z_ironwood::{discover, IronwoodDiscoveryError, IronwoodStatus};
use super::*;
use serde_json::json;
use std::collections::VecDeque;

const SAPLING_BRANCH: u32 = 0x76b8_09bb;
const IRONWOOD_BRANCH: u32 = 0x37a5_165b;
const ARRR_TIP: u64 = 4_167_700;

fn arrr_params() -> ZcoinConsensusParams {
    serde_json::from_value(json!({
        "overwinter_activation_height": 152855,
        "sapling_activation_height": 152855,
        "blossom_activation_height": null,
        "heartwood_activation_height": null,
        "canopy_activation_height": null,
        "coin_type": 133,
        "hrp_sapling_extended_spending_key": "secret-extended-key-main",
        "hrp_sapling_extended_full_viewing_key": "zxviews",
        "hrp_sapling_payment_address": "zs",
        "b58_pubkey_address_prefix": [0x1c, 0xb8],
        "b58_script_address_prefix": [0x1c, 0xbd]
    }))
    .unwrap()
}

/// A configuration that declares every upgrade up to Canopy and nothing after it.
fn canopy_params() -> ZcoinConsensusParams {
    serde_json::from_value(json!({
        "overwinter_activation_height": 100,
        "sapling_activation_height": 200,
        "blossom_activation_height": 300,
        "heartwood_activation_height": 400,
        "canopy_activation_height": 500,
        "coin_type": 133,
        "hrp_sapling_extended_spending_key": "secret-extended-key-main",
        "hrp_sapling_extended_full_viewing_key": "zxviews",
        "hrp_sapling_payment_address": "zs",
        "b58_pubkey_address_prefix": [0x1c, 0xb8],
        "b58_script_address_prefix": [0x1c, 0xbd]
    }))
    .unwrap()
}

pub(crate) fn test_addressing() -> HtlcAddressing {
    HtlcAddressing {
        p2sh_prefix: 0xbd,
        p2sh_t_prefix: 0,
        pubkey_prefix: 0xb8,
        pubkey_t_prefix: 0,
        checksum_type: kdf_crypto::ChecksumType::DSHA256,
    }
}

pub(crate) fn info(branch: u32, height: u64) -> ServerInfo {
    ServerInfo {
        chain_name: "main".to_owned(),
        sapling_activation_height: 152855,
        consensus_branch_id: format!("{:08x}", branch),
        block_height: height,
    }
}

/// Endpoints whose compact-block cache is as current as their node.
fn probes(entries: Vec<(&str, Result<ServerInfo, &str>)>) -> Vec<(String, Result<AdmissionProbe, String>)> {
    entries
        .into_iter()
        .map(|(server, probe)| {
            let probe = probe.map_err(str::to_owned).map(|info| AdmissionProbe {
                cache_height: Ok(info.block_height),
                info,
            });
            (server.to_owned(), probe)
        })
        .collect()
}

/// An endpoint whose two heights come from different RPCs and disagree.
fn probe_with_cache(info: ServerInfo, cache_height: Result<u64, &str>) -> Result<AdmissionProbe, String> {
    Ok(AdmissionProbe {
        info,
        cache_height: cache_height.map_err(str::to_owned),
    })
}

fn fresh_wallet() -> WalletBranchState { WalletBranchState::default() }

fn scanned_wallet(recorded: u32) -> WalletBranchState {
    WalletBranchState {
        has_scanned_state: true,
        recorded: Some(recorded),
    }
}

// ---------------------------------------------------------------------------------------------
// T39.11.f -- endpoint admission
// ---------------------------------------------------------------------------------------------

#[test]
fn a_different_network_or_sapling_height_excludes_the_endpoint() {
    let policy = AdmissionPolicy::from_params(&arrr_params());
    let mut other_network = info(IRONWOOD_BRANCH, ARRR_TIP);
    other_network.chain_name = "test".to_owned();
    let mut other_sapling = info(IRONWOOD_BRANCH, ARRR_TIP);
    other_sapling.sapling_activation_height = 1;

    let outcome = decide_admission(
        probes(vec![
            ("net", Ok(other_network)),
            ("sapling", Ok(other_sapling)),
            ("a", Ok(info(IRONWOOD_BRANCH, ARRR_TIP))),
            ("b", Ok(info(IRONWOOD_BRANCH, ARRR_TIP))),
        ]),
        &policy,
        &fresh_wallet(),
    )
    .unwrap();

    assert_eq!(outcome.admitted, vec!["a", "b"]);
    assert!(matches!(
        outcome.excluded.iter().find(|(s, _)| s == "net"),
        Some((_, ExclusionReason::NetworkMismatch { .. }))
    ));
    assert!(matches!(
        outcome.excluded.iter().find(|(s, _)| s == "sapling"),
        Some((_, ExclusionReason::SaplingActivationMismatch { .. }))
    ));
}

/// The inversion case R39.11.7a exists for. ARRR's published configuration does not declare the
/// Ironwood upgrade, so the branch the *configuration* derives at today's tip is the pre-upgrade
/// Sapling value. A check that compares each endpoint with that expectation would admit the stale
/// fork and exclude every healthy endpoint.
#[test]
fn an_undeclared_upgrade_does_not_make_the_stale_fork_the_expected_chain() {
    let params = arrr_params();
    let policy = AdmissionPolicy::from_params(&params);

    // The naive local expectation really is the pre-upgrade value at the tip ...
    assert_eq!(
        u32::from(BranchId::for_height(&params, BlockHeight::from_u32(ARRR_TIP as u32))),
        SAPLING_BRANCH
    );
    // ... which is exactly why the configuration must decline to judge at that height.
    assert_eq!(policy.schedule.expected_at(ARRR_TIP), None);

    // Five endpoints on the healthy chain, three of cipig's on the stale fork (some of them with a
    // *higher* reported tip than the healthy ones).
    let mut entries = Vec::new();
    for healthy in ["h1", "h2", "h3", "h4", "h5"] {
        entries.push((healthy, Ok(info(IRONWOOD_BRANCH, ARRR_TIP))));
    }
    for stale in ["s1", "s2", "s3"] {
        entries.push((stale, Ok(info(SAPLING_BRANCH, ARRR_TIP + 10))));
    }
    let outcome = decide_admission(probes(entries), &policy, &fresh_wallet()).unwrap();

    // The assertion the step is built around: the healthy endpoints are admitted and the stale fork
    // is excluded -- never the other way round.
    assert_eq!(outcome.admitted, vec!["h1", "h2", "h3", "h4", "h5"]);
    assert_eq!(outcome.excluded.len(), 3);
    assert!(outcome.excluded.iter().all(|(server, reason)| server.starts_with('s')
        && matches!(reason, ExclusionReason::OutvotedBranch { majority, .. } if *majority == IRONWOOD_BRANCH)));
    assert_eq!(outcome.branch_to_record, Some(IRONWOOD_BRANCH));
}

/// Declaring the Ironwood height binds `Nu6_3` (Step E), so the configuration now determines the
/// branch at every height -- and determines it correctly on both sides of the height: Sapling's below,
/// Ironwood's from it. The inversion this guards against is a determination that is right below the
/// height and wrong above it; a configuration that declares *no* height must still decline to judge.
#[test]
fn carrying_the_ironwood_height_makes_the_expectation_follow_it_on_both_sides() {
    let mut params = serde_json::to_value(arrr_params()).unwrap();
    params["ironwood_activation_height"] = json!(4_161_185);
    let params: ZcoinConsensusParams = serde_json::from_value(params).unwrap();
    assert_eq!(params.ironwood_activation_height(), Some(4_161_185));
    let policy = AdmissionPolicy::from_params(&params);
    assert_eq!(policy.schedule.expected_at(4_161_184), Some(SAPLING_BRANCH));
    assert_eq!(policy.schedule.expected_at(4_161_185), Some(IRONWOOD_BRANCH));
    assert_eq!(policy.schedule.expected_at(ARRR_TIP), Some(IRONWOOD_BRANCH));

    // Without a declared height nothing is judged at the tip.
    let undeclared = AdmissionPolicy::from_params(&arrr_params());
    assert_eq!(undeclared.schedule.expected_at(ARRR_TIP), None);
}

/// A height *discovered* after the policy was built reaches the build through the shared parameters
/// but cannot re-arm the policy: its horizon was fixed when the upgrade was undeclared, so it keeps
/// declining to judge. That is the safe direction -- a late-learned height must never be able to
/// exclude the healthy endpoints it was learned from.
#[test]
fn a_discovered_height_does_not_re_arm_a_policy_built_before_it() {
    let params = arrr_params();
    let policy = AdmissionPolicy::from_params(&params);
    params.record_discovered_ironwood_height(4_161_185);
    assert_eq!(
        params.activation_height(NetworkUpgrade::Nu6_3),
        Some(BlockHeight::from_u32(4_161_185))
    );
    assert_eq!(policy.schedule.expected_at(ARRR_TIP), None);
}

#[test]
fn where_the_configuration_determines_the_branch_a_mismatch_excludes() {
    let policy = AdmissionPolicy::from_params(&canopy_params());
    let canopy = u32::from(BranchId::Canopy);
    let heartwood = u32::from(BranchId::Heartwood);
    // Below the horizon (the Canopy height) the configuration is authoritative.
    assert_eq!(policy.schedule.expected_at(450), Some(heartwood));

    let mut right = info(heartwood, 450);
    right.sapling_activation_height = 200;
    let mut wrong = info(canopy, 450);
    wrong.sapling_activation_height = 200;
    let outcome = decide_admission(
        probes(vec![("right", Ok(right)), ("wrong", Ok(wrong))]),
        &policy,
        &fresh_wallet(),
    )
    .unwrap();
    assert_eq!(outcome.admitted, vec!["right"]);
    assert!(matches!(
        outcome.excluded[0].1,
        ExclusionReason::ConfigBranchMismatch { reported, expected, .. } if reported == canopy && expected == heartwood
    ));
    // Determined by the configuration: nothing to record, no majority needed.
    assert_eq!(outcome.branch_to_record, None);
}

/// A wallet whose scanned state follows one chain admits only that partition, and an endpoint of
/// another partition is excluded however high its tip is. This is what stops the wallet rewinding
/// ~99 blocks and following a stale fork's chain.
#[test]
fn a_wallet_with_scanned_state_stays_on_its_recorded_branch() {
    let policy = AdmissionPolicy::from_params(&arrr_params());
    let outcome = decide_admission(
        probes(vec![
            ("fork", Ok(info(SAPLING_BRANCH, ARRR_TIP + 5_000))),
            ("mine", Ok(info(IRONWOOD_BRANCH, ARRR_TIP))),
        ]),
        &policy,
        &scanned_wallet(IRONWOOD_BRANCH),
    )
    .unwrap();
    assert_eq!(outcome.admitted, vec!["mine"]);
    assert!(matches!(
        outcome.excluded[0].1,
        ExclusionReason::OutsideWalletBranch { reported, bound }
            if reported == SAPLING_BRANCH && bound == IRONWOOD_BRANCH
    ));
    // The binding is already recorded; it is never rewritten by a later decision.
    assert_eq!(outcome.branch_to_record, None);

    // A single endpoint is enough when the wallet already knows its chain.
    assert!(decide_admission(
        probes(vec![("only", Ok(info(IRONWOOD_BRANCH, ARRR_TIP)))]),
        &policy,
        &scanned_wallet(IRONWOOD_BRANCH),
    )
    .is_ok());

    // And if every endpoint is on another branch the wallet stops rather than switching.
    let err = decide_admission(
        probes(vec![("fork", Ok(info(SAPLING_BRANCH, ARRR_TIP)))]),
        &policy,
        &scanned_wallet(IRONWOOD_BRANCH),
    )
    .unwrap_err();
    assert!(matches!(err, LightwalletdError::AllExcluded(_)), "{err:?}");
}

#[test]
fn a_fresh_wallet_needs_a_strict_majority_and_never_a_single_voice() {
    let policy = AdmissionPolicy::from_params(&arrr_params());

    // One reachable endpoint: no partition holds a majority, and every observed identifier is
    // reported.
    let err = decide_admission(
        probes(vec![("only", Ok(info(IRONWOOD_BRANCH, ARRR_TIP)))]),
        &policy,
        &fresh_wallet(),
    )
    .unwrap_err();
    match err {
        LightwalletdError::NoBranchMajority(observed) => assert!(observed.contains("37a5165b x1"), "{observed}"),
        other => panic!("{other:?}"),
    }

    // A tie.
    let err = decide_admission(
        probes(vec![
            ("a", Ok(info(IRONWOOD_BRANCH, ARRR_TIP))),
            ("b", Ok(info(SAPLING_BRANCH, ARRR_TIP))),
        ]),
        &policy,
        &fresh_wallet(),
    )
    .unwrap_err();
    match err {
        LightwalletdError::NoBranchMajority(observed) => {
            assert!(
                observed.contains("37a5165b x1") && observed.contains("76b809bb x1"),
                "{observed}"
            )
        },
        other => panic!("{other:?}"),
    }

    // A wallet with scanned state but no recorded identifier (it predates the binding) is decided
    // by majority and gets its identifier recorded.
    let legacy = WalletBranchState {
        has_scanned_state: true,
        recorded: None,
    };
    let outcome = decide_admission(
        probes(vec![
            ("a", Ok(info(IRONWOOD_BRANCH, ARRR_TIP))),
            ("b", Ok(info(IRONWOOD_BRANCH, ARRR_TIP))),
            ("c", Ok(info(SAPLING_BRANCH, ARRR_TIP))),
        ]),
        &policy,
        &legacy,
    )
    .unwrap();
    assert_eq!(outcome.admitted, vec!["a", "b"]);
    assert_eq!(outcome.branch_to_record, Some(IRONWOOD_BRANCH));
}

#[test]
fn unreachable_and_excluded_endpoint_sets_fail_with_different_errors() {
    let policy = AdmissionPolicy::from_params(&arrr_params());
    let unreachable = decide_admission(
        probes(vec![("a", Err("connection refused")), ("b", Err("timed out"))]),
        &policy,
        &fresh_wallet(),
    )
    .unwrap_err();
    assert!(
        matches!(unreachable, LightwalletdError::AllUnreachable(_)),
        "{unreachable:?}"
    );

    let mut other_network = info(IRONWOOD_BRANCH, ARRR_TIP);
    other_network.chain_name = "test".to_owned();
    let excluded = decide_admission(
        probes(vec![("a", Ok(other_network)), ("b", Err("timed out"))]),
        &policy,
        &fresh_wallet(),
    )
    .unwrap_err();
    assert!(matches!(excluded, LightwalletdError::AllExcluded(_)), "{excluded:?}");

    assert_eq!(
        decide_admission(Vec::new(), &policy, &fresh_wallet()).unwrap_err(),
        LightwalletdError::NoEndpoints
    );
}

#[test]
fn branch_identifiers_parse_as_the_service_sends_them() {
    assert_eq!(parse_branch_id("76b809bb"), Some(SAPLING_BRANCH));
    assert_eq!(parse_branch_id("0x37a5165b"), Some(IRONWOOD_BRANCH));
    assert_eq!(parse_branch_id(""), None);
    assert_eq!(parse_branch_id("not hex"), None);
    assert_eq!(parse_branch_id("1ffffffff"), None);
}

/// R39.11.4i(e): the node's no-address-index answer is its own condition, whatever the status code;
/// any other service answer stays a serve failure.
#[test]
fn status_error_classifies_the_no_address_index_message_apart_from_a_service_error() {
    let refused = status_error(tonic::Status::unknown("-5: No information available for address"));
    assert_eq!(
        refused,
        EndpointError::NoAddressIndex("Unknown error: -5: No information available for address".to_owned())
    );
    assert_eq!(
        refused.to_string(),
        "the server's node keeps no address index: Unknown error: -5: No information available for address"
    );
    assert_eq!(
        status_error(tonic::Status::internal("-5: NO INFORMATION AVAILABLE FOR ADDRESS")),
        EndpointError::NoAddressIndex("Internal error: -5: NO INFORMATION AVAILABLE FOR ADDRESS".to_owned())
    );
    assert!(matches!(
        status_error(tonic::Status::unknown("something else")),
        EndpointError::Service(_)
    ));
    // It ends a read instead of being bisected; a service error is bisected.
    assert!(!WindowError::from(&refused).serve_failure);
    assert!(WindowError::from(&status_error(tonic::Status::unknown("something else"))).serve_failure);
}

// ---------------------------------------------------------------------------------------------
// A scripted transport
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
pub(crate) struct Script {
    pub(crate) info: Option<Result<ServerInfo, EndpointError>>,
    /// What `GetLatestBlock` answers to admission; follows the info height when unset.
    pub(crate) cache: Option<Result<u64, EndpointError>>,
    pub(crate) tips: VecDeque<Result<u64, EndpointError>>,
    /// What `GetLatestBlock` answers once `tips` has run dry, instead of failing as unscripted. Lets a
    /// fixture hold the chain at a height for as many reads as the code under test makes.
    pub(crate) tip_floor: Option<u64>,
    /// Set as `tip_floor` the moment a broadcast is accepted: the chain mines the transaction.
    pub(crate) tip_after_send: Option<u64>,
    pub(crate) transaction: Option<Result<RawTx, EndpointError>>,
    pub(crate) send: Option<Result<(), EndpointError>>,
    /// How long a broadcast takes to be answered, to widen a race around it.
    pub(crate) send_delay: Option<std::time::Duration>,
    pub(crate) balance: Option<Result<i64, EndpointError>>,
    /// The transparent-history rows every address query answers with, those mined inside the asked window.
    pub(crate) history: Vec<RawTx>,
    /// How long a transparent-history request takes to be answered.
    pub(crate) history_delay: Option<std::time::Duration>,
    /// What every transparent-address read (history, unspent listing) fails with, when set: an endpoint
    /// whose node keeps no address index answers each of them with the same rejection.
    pub(crate) address_error: Option<EndpointError>,
    /// The block hash `GetBlock` answers per height; a height not listed is unscripted.
    pub(crate) block_hashes: HashMap<u64, [u8; 32]>,
}

/// Called with the bytes of every broadcast, before the scripted answer is returned.
pub(crate) type SendObserver = Arc<dyn Fn(&[u8]) + Send + Sync>;

#[derive(Default)]
pub(crate) struct FakeTransport {
    pub(crate) scripts: Mutex<HashMap<String, Script>>,
    pub(crate) calls: Mutex<Vec<(String, &'static str)>>,
    last_balance_request: Mutex<Option<Vec<String>>>,
    last_txid: Mutex<Option<[u8; 32]>>,
    /// The bytes handed to `send_transaction`, accepted or not, in call order.
    pub(crate) sent: Mutex<Vec<Vec<u8>>>,
    pub(crate) on_send: Mutex<Option<SendObserver>>,
}

impl fmt::Debug for FakeTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("FakeTransport") }
}

impl FakeTransport {
    pub(crate) fn serve(&self, server: &str, script: Script) { self.scripts.lock().insert(server.to_owned(), script); }

    fn record(&self, server: &str, method: &'static str) { self.calls.lock().push((server.to_owned(), method)); }

    pub(crate) fn called(&self, server: &str, method: &'static str) -> usize {
        self.calls
            .lock()
            .iter()
            .filter(|(s, m)| s == server && *m == method)
            .count()
    }

    fn unscripted() -> EndpointError { EndpointError::Transport("not scripted".to_owned()) }
}

#[async_trait]
impl LightwalletdTransport for FakeTransport {
    async fn light_info(&self, server: &str) -> Result<ServerInfo, EndpointError> {
        self.record(server, "info");
        self.scripts
            .lock()
            .get(server)
            .and_then(|s| s.info.clone())
            .unwrap_or_else(|| Err(Self::unscripted()))
    }

    async fn cache_height(&self, server: &str) -> Result<u64, EndpointError> {
        self.record(server, "cache");
        let scripts = self.scripts.lock();
        let script = scripts.get(server);
        match script.and_then(|s| s.cache.clone()) {
            Some(cache) => cache,
            None => match script.and_then(|s| s.info.clone()) {
                Some(Ok(info)) => Ok(info.block_height),
                _ => Err(Self::unscripted()),
            },
        }
    }

    async fn latest_block(&self, server: &str) -> Result<u64, EndpointError> {
        self.record(server, "tip");
        self.scripts
            .lock()
            .get_mut(server)
            .and_then(|s| s.tips.pop_front().or_else(|| s.tip_floor.map(Ok)))
            .unwrap_or_else(|| Err(Self::unscripted()))
    }

    async fn block_time(&self, server: &str, height: u64) -> Result<u32, EndpointError> {
        self.record(server, "time");
        Ok(1_000 + height as u32)
    }

    async fn block_hash(&self, server: &str, height: u64) -> Result<[u8; 32], EndpointError> {
        self.record(server, "block");
        self.scripts
            .lock()
            .get(server)
            .and_then(|s| s.block_hashes.get(&height).copied())
            .ok_or_else(Self::unscripted)
    }

    async fn transaction(&self, server: &str, txid: [u8; 32]) -> Result<RawTx, EndpointError> {
        self.record(server, "tx");
        *self.last_txid.lock() = Some(txid);
        self.scripts
            .lock()
            .get(server)
            .and_then(|s| s.transaction.clone())
            .unwrap_or_else(|| Err(Self::unscripted()))
    }

    async fn send_transaction(&self, server: &str, data: Vec<u8>) -> Result<(), EndpointError> {
        self.record(server, "send");
        let observer = self.on_send.lock().clone();
        if let Some(observer) = observer {
            observer(&data);
        }
        self.sent.lock().push(data);
        let (delay, answer) = {
            let scripts = self.scripts.lock();
            let script = scripts.get(server);
            (
                script.and_then(|s| s.send_delay),
                script
                    .and_then(|s| s.send.clone())
                    .unwrap_or_else(|| Err(Self::unscripted())),
            )
        };
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        if answer.is_ok() {
            if let Some(script) = self.scripts.lock().get_mut(server) {
                if let Some(tip) = script.tip_after_send {
                    script.tip_floor = Some(tip);
                }
            }
        }
        answer
    }

    async fn taddr_balance(&self, server: &str, addresses: Vec<String>) -> Result<i64, EndpointError> {
        self.record(server, "balance");
        *self.last_balance_request.lock() = Some(addresses);
        self.scripts
            .lock()
            .get(server)
            .and_then(|s| s.balance.clone())
            .unwrap_or_else(|| Err(Self::unscripted()))
    }

    async fn taddr_txids(&self, server: &str, _address: &str, start: u64, end: u64) -> WindowRead {
        self.record(server, "history");
        let delay = self.scripts.lock().get(server).and_then(|s| s.history_delay);
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        let refused = self.scripts.lock().get(server).and_then(|s| s.address_error.clone());
        if let Some(error) = refused {
            return WindowRead {
                txs: Vec::new(),
                error: Some(WindowError::from(&error)),
            };
        }
        let txs = self
            .scripts
            .lock()
            .get(server)
            .map(|s| {
                s.history
                    .iter()
                    .filter(|t| t.height.map_or(false, |h| (start..=end).contains(&h)))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        WindowRead { txs, error: None }
    }

    async fn address_utxos(&self, server: &str, _address: &str) -> Result<Vec<TransparentUtxo>, EndpointError> {
        self.record(server, "utxos");
        match self.scripts.lock().get(server).and_then(|s| s.address_error.clone()) {
            Some(error) => Err(error),
            None => Ok(Vec::new()),
        }
    }
}

fn backend_over(transport: &Arc<FakeTransport>, servers: &[&str]) -> LightwalletdBackend {
    LightwalletdBackend::new(
        transport.clone(),
        servers.iter().map(|s| (*s).to_owned()).collect(),
        AdmissionPolicy::from_params(&arrr_params()),
        test_addressing(),
    )
}

fn healthy(tips: &[u64]) -> Script {
    Script {
        info: Some(Ok(info(IRONWOOD_BRANCH, ARRR_TIP))),
        tips: tips.iter().map(|t| Ok(*t)).collect(),
        ..Script::default()
    }
}

fn stale_fork() -> Script {
    Script {
        info: Some(Ok(info(SAPLING_BRANCH, ARRR_TIP + 5_000))),
        tips: VecDeque::from([Ok(ARRR_TIP + 5_000)]),
        ..Script::default()
    }
}

#[tokio::test]
async fn an_excluded_endpoint_is_never_asked_for_anything() {
    let transport = Arc::new(FakeTransport::default());
    transport.serve("fork", stale_fork());
    transport.serve("a", healthy(&[ARRR_TIP]));
    transport.serve("b", healthy(&[ARRR_TIP]));
    let backend = backend_over(&transport, &["fork", "a", "b"]);

    assert_eq!(backend.chain_tip().await.unwrap(), ARRR_TIP);
    assert_eq!(
        transport.called("fork", "tip"),
        0,
        "an off-branch endpoint must not be used"
    );
    assert_eq!(backend.excluded_endpoints().len(), 1);
    assert_eq!(backend.excluded_endpoints()[0].0, "fork");
}

/// D39.11.g: an endpoint can pass admission and still fail the method that is actually needed.
#[tokio::test]
async fn an_admitted_endpoint_that_fails_the_needed_method_is_skipped() {
    let transport = Arc::new(FakeTransport::default());
    let mut broken = healthy(&[]);
    broken.tips = VecDeque::from([Err(EndpointError::Service("502 Bad Gateway".to_owned()))]);
    transport.serve("broken", broken);
    transport.serve("good", healthy(&[ARRR_TIP]));
    let backend = backend_over(&transport, &["broken", "good"]);

    assert_eq!(backend.chain_tip().await.unwrap(), ARRR_TIP);
    assert_eq!(transport.called("broken", "tip"), 1);
    assert_eq!(transport.called("good", "tip"), 1);
}

/// T39.11.i: a lagging read neither lowers the coin's height nor is reported as it.
#[tokio::test]
async fn the_reported_height_never_goes_backwards() {
    let transport = Arc::new(FakeTransport::default());
    transport.serve("a", healthy(&[ARRR_TIP, ARRR_TIP - 7, ARRR_TIP + 1]));
    transport.serve("b", healthy(&[]));
    let backend = backend_over(&transport, &["a", "b"]);

    assert_eq!(backend.chain_tip().await.unwrap(), ARRR_TIP);
    let lagging = backend.fresh_tip().await.unwrap();
    assert_eq!(lagging, FreshTip {
        read: ARRR_TIP - 7,
        highest_before: ARRR_TIP
    });
    assert_eq!(lagging.monotonic(), ARRR_TIP);
    assert_eq!(backend.chain_tip().await.unwrap(), ARRR_TIP + 1);
}

/// T39.11.c / T39.11.d: a failing tip read is an error, never a fallback value.
#[tokio::test]
async fn a_failed_tip_read_is_an_error_and_not_a_default() {
    let transport = Arc::new(FakeTransport::default());
    transport.serve("a", healthy(&[]));
    let backend = backend_over(&transport, &["a"]);
    let err = backend.fresh_tip().await.unwrap_err();
    // (a single endpoint cannot be admitted by majority on a fresh wallet, so the failure is the
    // admission one; with a recorded branch it is the read itself)
    assert!(matches!(err, LightwalletdError::NoBranchMajority(_)), "{err:?}");

    struct Bound;
    impl WalletBranchStore for Bound {
        fn state(&self) -> Result<WalletBranchState, String> { Ok(scanned_wallet(IRONWOOD_BRANCH)) }

        fn record(&self, _: u32) -> Result<(), String> { Ok(()) }
    }
    backend.bind_wallet(Arc::new(Bound));
    let err = backend.fresh_tip().await.unwrap_err();
    assert!(
        matches!(err, LightwalletdError::AllFailed {
            op: "GetLatestBlock",
            ..
        }),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_rejected_broadcast_is_reported_and_not_retried_elsewhere() {
    struct Bound;
    impl WalletBranchStore for Bound {
        fn state(&self) -> Result<WalletBranchState, String> { Ok(scanned_wallet(IRONWOOD_BRANCH)) }

        fn record(&self, _: u32) -> Result<(), String> { Ok(()) }
    }
    let transport = Arc::new(FakeTransport::default());
    let mut first = healthy(&[]);
    first.send = Some(Err(EndpointError::Rejected {
        code: -26,
        message: "tx-expiring-soon".to_owned(),
    }));
    let mut second = healthy(&[]);
    second.send = Some(Ok(()));
    transport.serve("first", first);
    transport.serve("second", second);
    let backend = backend_over(&transport, &["first", "second"]);
    backend.bind_wallet(Arc::new(Bound));

    let err = backend.send_transaction(vec![1, 2, 3]).await.unwrap_err();
    assert_eq!(err, LightwalletdError::BroadcastRejected {
        code: -26,
        message: "tx-expiring-soon".to_owned()
    });
    assert_eq!(
        transport.called("second", "send"),
        0,
        "a consensus rejection is not retried"
    );
}

#[tokio::test]
async fn a_transport_failure_on_broadcast_does_move_on_to_the_next_endpoint() {
    struct Bound;
    impl WalletBranchStore for Bound {
        fn state(&self) -> Result<WalletBranchState, String> { Ok(scanned_wallet(IRONWOOD_BRANCH)) }

        fn record(&self, _: u32) -> Result<(), String> { Ok(()) }
    }
    let transport = Arc::new(FakeTransport::default());
    let mut first = healthy(&[]);
    first.send = Some(Err(EndpointError::Transport("reset".to_owned())));
    let mut second = healthy(&[]);
    second.send = Some(Ok(()));
    transport.serve("first", first);
    transport.serve("second", second);
    let backend = backend_over(&transport, &["first", "second"]);
    backend.bind_wallet(Arc::new(Bound));

    backend.send_transaction(vec![1]).await.unwrap();
    assert_eq!(transport.called("second", "send"), 1);
}

// ---------------------------------------------------------------------------------------------
// T39.11.c -- one builder height
// ---------------------------------------------------------------------------------------------

#[test]
fn every_construction_path_derives_the_same_target_and_expiry_from_one_tip() {
    let tip = FreshTip {
        read: ARRR_TIP,
        highest_before: ARRR_TIP,
    };
    // The ordinary send path and the HTLC spend/refund path both call this one function, so one
    // injected tip gives both the same target height and therefore the same expiry height.
    let ordinary_send = builder_target_height(tip, ARRR_TIP).unwrap();
    let htlc_spend_or_refund = builder_target_height(tip, ARRR_TIP).unwrap();
    assert_eq!(ordinary_send, htlc_spend_or_refund);
    assert_eq!(u64::from(ordinary_send), ARRR_TIP + 1);
    assert_eq!(
        u64::from(ordinary_send) + u64::from(BUILDER_EXPIRY_DELTA),
        ARRR_TIP + 1 + 40
    );
}

#[test]
fn a_tip_more_than_the_expiry_delta_behind_what_was_observed_refuses_to_build() {
    let behind = FreshTip {
        read: ARRR_TIP - 6_000,
        highest_before: ARRR_TIP,
    };
    assert_eq!(
        builder_target_height(behind, ARRR_TIP).unwrap_err(),
        LightwalletdError::StaleTip {
            tip: ARRR_TIP - 6_000,
            observed: ARRR_TIP
        }
    );

    // The wallet's own scanned height corroborates too, even with no earlier tip read.
    let first_read = FreshTip {
        read: ARRR_TIP - 100,
        highest_before: 0,
    };
    assert!(matches!(
        builder_target_height(first_read, ARRR_TIP),
        Err(LightwalletdError::StaleTip { .. })
    ));

    // Within the delta it is ordinary propagation lag, and the height used does not go backwards.
    let lagging = FreshTip {
        read: ARRR_TIP - BUILDER_EXPIRY_DELTA as u64,
        highest_before: ARRR_TIP,
    };
    assert_eq!(u64::from(builder_target_height(lagging, 0).unwrap()), ARRR_TIP + 1);
}

// ---------------------------------------------------------------------------------------------
// T39.11.d -- confirmations and the uncorroborated expiry
// ---------------------------------------------------------------------------------------------

#[test]
fn confirmation_depth_is_derived_from_the_mined_height_and_the_tip() {
    assert_eq!(confirmation_depth(100, Some(100)), 1);
    assert_eq!(confirmation_depth(100, Some(91)), 10);
    assert_eq!(confirmation_depth(100, None), 0, "unmined");
    assert_eq!(confirmation_depth(100, Some(101)), 0, "above the tip");
    assert_eq!(confirmation_depth(100, Some(0)), 0);
}

#[tokio::test]
async fn an_unfetchable_transaction_past_its_expiry_is_not_declared_expired() {
    struct Bound;
    impl WalletBranchStore for Bound {
        fn state(&self) -> Result<WalletBranchState, String> { Ok(scanned_wallet(IRONWOOD_BRANCH)) }

        fn record(&self, _: u32) -> Result<(), String> { Ok(()) }
    }
    let txid = H256Json::from([7u8; 32]);

    // The backend merely fails the lookup.
    let transport = Arc::new(FakeTransport::default());
    let mut failing = healthy(&[ARRR_TIP]);
    failing.transaction = Some(Err(EndpointError::Service("502".to_owned())));
    transport.serve("a", failing);
    let client = LightwalletdClient::new(
        transport.clone(),
        vec!["a".to_owned()],
        &arrr_params(),
        test_addressing(),
    );
    client.0.bind_wallet(Arc::new(Bound));
    let err = client
        .wait_for_confirmations(txid, (ARRR_TIP - 10) as u32, 1, false, u64::MAX, 1)
        .await
        .unwrap_err();
    assert!(err.contains("does not prove it expired"), "{err}");
    assert!(!err.contains("has expired"), "{err}");

    // The service explicitly answers "no such transaction": that is the corroborated expiry.
    let transport = Arc::new(FakeTransport::default());
    let mut missing = healthy(&[ARRR_TIP]);
    missing.transaction = Some(Err(EndpointError::NotFound("unknown txid".to_owned())));
    transport.serve("a", missing);
    let client = LightwalletdClient::new(
        transport.clone(),
        vec!["a".to_owned()],
        &arrr_params(),
        test_addressing(),
    );
    client.0.bind_wallet(Arc::new(Bound));
    let err = client
        .wait_for_confirmations(txid, (ARRR_TIP - 10) as u32, 1, false, u64::MAX, 1)
        .await
        .unwrap_err();
    assert!(err.contains("has expired"), "{err}");
}

#[tokio::test]
async fn a_confirmed_transaction_satisfies_the_wait_without_any_server_side_decode() {
    struct Bound;
    impl WalletBranchStore for Bound {
        fn state(&self) -> Result<WalletBranchState, String> { Ok(scanned_wallet(IRONWOOD_BRANCH)) }

        fn record(&self, _: u32) -> Result<(), String> { Ok(()) }
    }
    let transport = Arc::new(FakeTransport::default());
    let mut script = healthy(&[ARRR_TIP]);
    script.transaction = Some(Ok(RawTx {
        data: vec![1, 2, 3],
        height: Some(ARRR_TIP - 2),
    }));
    transport.serve("a", script);
    let client = LightwalletdClient::new(transport, vec!["a".to_owned()], &arrr_params(), test_addressing());
    client.0.bind_wallet(Arc::new(Bound));
    client
        .wait_for_confirmations(H256Json::from([9u8; 32]), 0, 3, false, u64::MAX, 1)
        .await
        .unwrap();
}

/// R39.11.10a fixture: the highest observed tip is `HIGHEST_SEEN` and the following reads are the
/// scripted `fresh` ones; `tx` (when given) is the transaction the service returns.
const HIGHEST_SEEN: u64 = 110;

struct BoundBranch;
impl WalletBranchStore for BoundBranch {
    fn state(&self) -> Result<WalletBranchState, String> { Ok(scanned_wallet(IRONWOOD_BRANCH)) }

    fn record(&self, _: u32) -> Result<(), String> { Ok(()) }
}

fn client_after_highest_seen(
    fresh: &[u64],
    tx: Result<RawTx, EndpointError>,
) -> (Arc<FakeTransport>, LightwalletdClient) {
    let transport = Arc::new(FakeTransport::default());
    let mut script = healthy(&[HIGHEST_SEEN]);
    script.tips.extend(fresh.iter().map(|t| Ok(*t)));
    script.tip_floor = fresh.last().copied();
    script.transaction = Some(tx);
    transport.serve("a", script);
    let client = LightwalletdClient::new(
        transport.clone(),
        vec!["a".to_owned()],
        &arrr_params(),
        test_addressing(),
    );
    client.0.bind_wallet(Arc::new(BoundBranch));
    (transport, client)
}

/// T39.11.10a: highest observed 110, fresh read 105, mined at 105 -> 1 confirmation, not 6.
#[tokio::test]
async fn confirmations_use_fresh_tip_not_highest_seen() {
    let (_transport, client) = client_after_highest_seen(&[105], Ok(tx_at(105, 1)));
    assert_eq!(client.0.chain_tip().await.unwrap(), HIGHEST_SEEN);

    let tip = client.0.confirmation_tip().await.unwrap();
    assert_eq!(tip, 105, "the fresh read, not the highest observed");
    assert_eq!(confirmation_depth(tip, Some(105)), 1);
    assert_eq!(
        confirmation_depth(HIGHEST_SEEN, Some(105)),
        6,
        "the monotonic height would overcount"
    );
}

/// T39.11.10a: a wait for 2 confirmations is not done at fresh 105, and is done at a later fresh 106.
#[tokio::test]
async fn wait_for_confirmations_waits_for_fresh_tip() {
    let (transport, client) = client_after_highest_seen(&[105, 106], Ok(tx_at(105, 1)));
    assert_eq!(client.0.chain_tip().await.unwrap(), HIGHEST_SEEN);

    client
        .wait_for_confirmations(H256Json::from([9u8; 32]), 0, 2, false, u64::MAX, 1)
        .await
        .unwrap();
    // One read to establish the highest tip, then 105 (depth 1, keep waiting), then 106 (depth 2).
    assert_eq!(transport.called("a", "tip"), 3);
    assert_eq!(transport.called("a", "tx"), 2);
}

/// T39.11.10a: expiry 107, fresh read 105, highest observed 110 -> not expired.
#[tokio::test]
async fn expiry_not_declared_on_lowered_tip() {
    let (_transport, client) =
        client_after_highest_seen(&[105], Err(EndpointError::NotFound("unknown txid".to_owned())));
    assert_eq!(client.0.chain_tip().await.unwrap(), HIGHEST_SEEN);

    let wait_until = common::now_ms() / 1000 + 1;
    let err = client
        .wait_for_confirmations(H256Json::from([9u8; 32]), 107, 1, false, wait_until, 1)
        .await
        .unwrap_err();
    assert!(err.contains("Waited too long"), "{err}");
    assert!(!err.contains("expired"), "{err}");
}

/// T39.11.10a: the reported height stays monotonic while confirmations read the fresh tip.
#[tokio::test]
async fn reported_height_stays_monotonic() {
    let (_transport, client) = client_after_highest_seen(&[105, 105], Ok(tx_at(105, 1)));
    assert_eq!(client.0.chain_tip().await.unwrap(), HIGHEST_SEEN);
    assert_eq!(client.0.confirmation_tip().await.unwrap(), 105);
    assert_eq!(client.0.chain_tip().await.unwrap(), HIGHEST_SEEN);
    assert_eq!(client.get_block_count().compat().await.unwrap(), HIGHEST_SEEN);
}

#[tokio::test]
async fn notarized_depth_is_refused_and_not_substituted() {
    let transport = Arc::new(FakeTransport::default());
    let client = LightwalletdClient::new(transport, vec!["a".to_owned()], &arrr_params(), test_addressing());
    let err = client
        .wait_for_confirmations(H256Json::from([9u8; 32]), 0, 1, true, u64::MAX, 1)
        .await
        .unwrap_err();
    assert!(err.contains("notarized"), "{err}");
}

// ---------------------------------------------------------------------------------------------
// R39.11.4c -- byte order
// ---------------------------------------------------------------------------------------------

#[test]
fn the_txid_conversion_point_is_explicit_and_works_in_both_directions() {
    // Not byte-order symmetric.
    let mut internal = [0u8; 32];
    for (i, byte) in internal.iter_mut().enumerate() {
        *byte = i as u8;
    }
    let display = txid_internal_to_display(internal);
    assert_eq!(display.0[0], 31);
    assert_eq!(display.0[31], 0);
    assert_eq!(txid_display_to_internal(&display), internal);
    assert_ne!(display.0, internal);
}

#[tokio::test]
async fn the_lookup_sends_the_internal_byte_order() {
    struct Bound;
    impl WalletBranchStore for Bound {
        fn state(&self) -> Result<WalletBranchState, String> { Ok(scanned_wallet(IRONWOOD_BRANCH)) }

        fn record(&self, _: u32) -> Result<(), String> { Ok(()) }
    }
    let transport = Arc::new(FakeTransport::default());
    let mut script = healthy(&[]);
    script.transaction = Some(Ok(RawTx {
        data: vec![0],
        height: None,
    }));
    transport.serve("a", script);
    let client = LightwalletdClient::new(
        transport.clone(),
        vec!["a".to_owned()],
        &arrr_params(),
        test_addressing(),
    );
    client.0.bind_wallet(Arc::new(Bound));

    let mut display_bytes = [0u8; 32];
    for (i, byte) in display_bytes.iter_mut().enumerate() {
        *byte = i as u8;
    }
    let display = H256Json::from(display_bytes);
    client.raw_transaction(&display).await.unwrap();
    let sent = transport.last_txid.lock().unwrap();
    let mut expected = display_bytes;
    expected.reverse();
    assert_eq!(sent, expected);
}

// ---------------------------------------------------------------------------------------------
// T39.11.j -- the balance request
// ---------------------------------------------------------------------------------------------

#[test]
fn the_balance_request_is_a_flat_repeated_string_field_on_the_wire() {
    let request = balance_request(&["RM6uy".to_owned(), "RXyz".to_owned()]);
    // field 1, wire type 2, length, bytes -- once per address, nothing nested.
    let mut expected = Vec::new();
    for address in ["RM6uy", "RXyz"] {
        expected.push(0x0a);
        expected.push(address.len() as u8);
        expected.extend_from_slice(address.as_bytes());
    }
    assert_eq!(request.encode_to_vec(), expected);

    // The malformed nesting the probes tripped over: field 1 containing a submessage whose field 1
    // is the address. It encodes differently, and is what the node rejects as an invalid list.
    let mut nested = Vec::new();
    let inner = [&[0x0a, 5][..], b"RM6uy"].concat();
    nested.push(0x0a);
    nested.push(inner.len() as u8);
    nested.extend_from_slice(&inner);
    assert_ne!(request.encode_to_vec(), nested);
}

#[tokio::test]
async fn several_addresses_go_out_as_one_call() {
    struct Bound;
    impl WalletBranchStore for Bound {
        fn state(&self) -> Result<WalletBranchState, String> { Ok(scanned_wallet(IRONWOOD_BRANCH)) }

        fn record(&self, _: u32) -> Result<(), String> { Ok(()) }
    }
    let transport = Arc::new(FakeTransport::default());
    let mut script = healthy(&[]);
    script.balance = Some(Ok(179_356_160_014));
    transport.serve("a", script);
    let backend = backend_over(&transport, &["a"]);
    backend.bind_wallet(Arc::new(Bound));

    let address = |hash: u8| Address {
        prefix: 0x3c,
        t_addr_prefix: 0,
        hrp: None,
        hash: keys::AddressHashEnum::AddressHash([hash; 20].into()),
        checksum_type: kdf_crypto::ChecksumType::DSHA256,
        addr_format: keys::AddressFormat::Standard,
    };
    let total = backend
        .taddr_balance(vec![address(1), address(2), address(3)])
        .await
        .unwrap();
    assert_eq!(total, 179_356_160_014);
    assert_eq!(transport.called("a", "balance"), 1);
    assert_eq!(transport.last_balance_request.lock().as_ref().unwrap().len(), 3);
}

#[tokio::test]
async fn a_script_hash_address_is_refused_and_never_answered_with_an_empty_success() {
    let transport = Arc::new(FakeTransport::default());
    let backend = backend_over(&transport, &["a"]);
    let p2sh = Address {
        prefix: 0xbd,
        t_addr_prefix: 0,
        hrp: None,
        hash: keys::AddressHashEnum::AddressHash([5u8; 20].into()),
        checksum_type: kdf_crypto::ChecksumType::DSHA256,
        addr_format: keys::AddressFormat::Standard,
    };
    assert!(matches!(
        backend.taddr_balance(vec![p2sh.clone()]).await,
        Err(LightwalletdError::Unsupported { .. })
    ));
    assert!(matches!(
        backend.address_utxos(&p2sh).await,
        Err(LightwalletdError::Unsupported { .. })
    ));
    assert_eq!(transport.called("a", "balance"), 0);
    assert_eq!(transport.called("a", "utxos"), 0);
}

// ---------------------------------------------------------------------------------------------
// T39.11.h -- the transparent history reader
// ---------------------------------------------------------------------------------------------

fn tx_at(height: u64, tag: u8) -> RawTx {
    RawTx {
        data: vec![tag; 4],
        height: Some(height),
    }
}

fn serve_failure() -> WindowError {
    WindowError {
        serve_failure: true,
        message: "No information available about transaction".to_owned(),
    }
}

/// A server model: transactions by height, an unservable height that aborts any stream containing
/// it, and the real server's habit of dropping the last height group of every stream.
fn history_server(
    chain: Vec<RawTx>,
    unservable: Option<u64>,
    requests: Arc<Mutex<Vec<(u64, u64)>>>,
) -> impl FnMut(u64, u64) -> futures::future::Ready<WindowRead> {
    move |start, end| {
        requests.lock().push((start, end));
        let mut groups: Vec<u64> = chain
            .iter()
            .filter_map(|tx| tx.height)
            .filter(|h| *h >= start && *h <= end)
            .collect();
        groups.sort_unstable();
        groups.dedup();
        let mut delivered = Vec::new();
        for (idx, height) in groups.iter().enumerate() {
            let is_last_group = idx + 1 == groups.len();
            if Some(*height) == unservable {
                return futures::future::ready(WindowRead {
                    txs: delivered,
                    error: Some(serve_failure()),
                });
            }
            if is_last_group {
                // dropped
                continue;
            }
            delivered.extend(chain.iter().filter(|tx| tx.height == Some(*height)).cloned());
        }
        futures::future::ready(WindowRead {
            txs: delivered,
            error: None,
        })
    }
}

#[tokio::test]
async fn a_failing_stream_is_not_no_history_and_its_deliveries_are_retained() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let chain = vec![tx_at(10, 1), tx_at(20, 2), tx_at(30, 3), tx_at(40, 4)];
    let read = read_history(
        1,
        100,
        100,
        HistoryLimits::UNBOUNDED,
        history_server(chain, Some(30), requests),
    )
    .await
    .unwrap();
    assert!(!read.is_clean());
    assert_eq!(read.gaps, vec![30]);
    assert!(read.transactions.iter().any(|tx| tx.height == Some(10)));
    assert!(read.transactions.iter().any(|tx| tx.height == Some(20)));
}

#[tokio::test]
async fn the_failing_window_is_bisected_and_the_offending_height_skipped_and_recorded() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let chain = vec![
        tx_at(10, 1),
        tx_at(20, 2),
        tx_at(30, 3),
        tx_at(40, 4),
        tx_at(45, 5),
        tx_at(90, 9),
    ];
    let read = read_history(
        1,
        100,
        100,
        HistoryLimits::UNBOUNDED,
        history_server(chain, Some(30), requests.clone()),
    )
    .await
    .unwrap();
    assert_eq!(
        read.gaps,
        vec![30],
        "the skipped height is a recorded gap, not forgotten"
    );
    assert!(read.failure.is_none(), "a serve failure does not fail the pass");
    // Everything servable around the gap arrived.
    for height in [10, 20, 40, 45] {
        assert!(read.transactions.iter().any(|tx| tx.height == Some(height)), "{height}");
    }
    // Bisection narrowed the failing range down to the single height.
    assert!(requests.lock().iter().any(|(s, e)| s == e && *s == 30));
    // A swap-path negative answer over this range is refused while sync/history still advanced.
    assert!(!read.is_clean());
    assert!(!read.transactions.is_empty());
}

#[tokio::test]
async fn resumption_starts_at_the_last_received_height_and_duplicates_are_suppressed() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    // Windows of 50: the first window's last group (height 45) is dropped by the server, so the
    // next window has to start at the last *received* height (20), not at 51.
    let chain = vec![tx_at(10, 1), tx_at(20, 2), tx_at(45, 3), tx_at(70, 4), tx_at(95, 5)];
    let read = read_history(
        1,
        100,
        50,
        HistoryLimits::UNBOUNDED,
        history_server(chain, None, requests.clone()),
    )
    .await
    .unwrap();
    let starts: Vec<u64> = requests.lock().iter().map(|(s, _)| *s).collect();
    assert_eq!(starts[0], 1);
    assert_eq!(
        starts[1], 20,
        "resumed from the last received height, not one past the window"
    );
    let heights: Vec<Option<u64>> = read.transactions.iter().map(|tx| tx.height).collect();
    // The height-45 group, dropped as the tail of the first two windows, arrives once the window is
    // widened to contain a later group; height 20 was delivered several times on the wire and
    // appears once.
    assert_eq!(heights.iter().filter(|h| **h == Some(20)).count(), 1);
    assert!(heights.contains(&Some(45)));
}

#[tokio::test]
async fn no_request_is_open_ended_or_below_one() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let chain = vec![tx_at(5, 1), tx_at(2_500, 2)];
    read_history(
        0,
        3_000,
        1_000,
        HistoryLimits::UNBOUNDED,
        history_server(chain, None, requests.clone()),
    )
    .await
    .unwrap();
    let requests = requests.lock();
    assert!(!requests.is_empty());
    for (start, end) in requests.iter() {
        assert!(*start >= 1 && *end >= 1, "{start}..{end}");
        assert!(
            end - start < 1_000 * HISTORY_MAX_WIDEN_FACTOR,
            "{start}..{end} is not bounded by the widest window"
        );
    }

    assert!(matches!(
        read_history(10, 5, 100, HistoryLimits::UNBOUNDED, |_, _| futures::future::ready(
            WindowRead::default()
        ))
        .await,
        Err(LightwalletdError::InvalidRequest(_))
    ));
}

#[tokio::test]
async fn a_transport_failure_ends_the_read_without_claiming_a_clean_answer() {
    let read = read_history(1, 100, 100, HistoryLimits::UNBOUNDED, |_, _| {
        futures::future::ready(WindowRead {
            txs: vec![tx_at(3, 1)],
            error: Some(WindowError {
                serve_failure: false,
                message: "connection reset".to_owned(),
            }),
        })
    })
    .await
    .unwrap();
    assert!(!read.is_clean());
    assert_eq!(read.failure.as_deref(), Some("connection reset"));
    assert_eq!(read.transactions.len(), 1);
}

#[tokio::test]
async fn a_complete_read_with_no_gap_is_clean_even_when_it_finds_nothing() {
    let read = read_history(1, 100, 100, HistoryLimits::UNBOUNDED, |_, _| {
        futures::future::ready(WindowRead::default())
    })
    .await
    .unwrap();
    assert!(read.is_clean());
    assert!(read.transactions.is_empty());
}

// ---------------------------------------------------------------------------------------------
// R39.11.4h -- limits on a history read
// ---------------------------------------------------------------------------------------------

fn refusing_every_request(calls: Arc<Mutex<usize>>) -> impl FnMut(u64, u64) -> futures::future::Ready<WindowRead> {
    move |_, _| {
        *calls.lock() += 1;
        futures::future::ready(WindowRead {
            txs: Vec::new(),
            error: Some(WindowError {
                serve_failure: true,
                message: "Internal error: the node could not serve a transaction".to_owned(),
            }),
        })
    }
}

/// The same script, with and without the limits of R39.11.4h(b), over 30,001 blocks: the unbounded
/// reader bisects every 1,000-block window down to single heights (2 * 1,000 - 1 requests each, and one
/// request for the last window of a single block).
#[tokio::test]
async fn a_refusing_endpoint_costs_about_sixty_thousand_requests_unbounded_and_eighteen_bounded() {
    let (from, to) = (4_137_700, 4_167_700);
    let unbounded_calls = Arc::new(Mutex::new(0));
    let read = read_history(
        from,
        to,
        1_000,
        HistoryLimits::UNBOUNDED,
        refusing_every_request(unbounded_calls.clone()),
    )
    .await
    .unwrap();
    assert_eq!(*unbounded_calls.lock(), 30 * 1_999 + 1);
    assert_eq!(read.gaps.len(), 30_001);
    assert!(read.failure.is_none(), "the unbounded reader never says why");

    let bounded_calls = Arc::new(Mutex::new(0));
    let read = read_history(
        from,
        to,
        1_000,
        HistoryLimits::HTLC,
        refusing_every_request(bounded_calls.clone()),
    )
    .await
    .unwrap();
    assert_eq!(*bounded_calls.lock(), 18);
    assert_eq!(read.requests, 18);
    let failure = read.failure.clone().expect("named failure");
    assert!(
        failure.starts_with("could not serve 5 heights of window 4137700..=4138699 (last error: Internal error: the node could not serve a transaction); the failure is not confined to single transactions"),
        "{failure}"
    );
    assert!(!read.is_clean());
}

/// A window with exactly four unservable heights is still bisected; the fifth ends the read.
#[tokio::test]
async fn four_unservable_heights_in_a_window_are_tolerated_and_a_fifth_is_not() {
    for (unservable, stopped) in [(vec![3, 5, 7, 9], false), (vec![3, 5, 7, 9, 11], true)] {
        let read = read_history(1, 100, 100, HistoryLimits::HTLC, move |start, end| {
            let failing = unservable.iter().any(|h| (start..=end).contains(h));
            futures::future::ready(WindowRead {
                txs: Vec::new(),
                error: failing.then(|| WindowError {
                    serve_failure: true,
                    message: "no such mempool or blockchain transaction".to_owned(),
                }),
            })
        })
        .await
        .unwrap();
        assert_eq!(read.failure.is_some(), stopped, "{read:?}");
        assert_eq!(read.gaps.len(), if stopped { 5 } else { 4 });
    }
}

#[tokio::test]
async fn a_read_is_capped_at_1024_requests() {
    let calls = Arc::new(Mutex::new(0usize));
    let counter = calls.clone();
    // Every window delivers one transaction one height above its start, so the read advances one
    // height per request and never completes the range.
    let read = read_history(1, 1_000_000, 1_000, HistoryLimits::HTLC, move |start, _end| {
        *counter.lock() += 1;
        futures::future::ready(WindowRead {
            txs: vec![RawTx {
                data: start.to_le_bytes().to_vec(),
                height: Some(start + 1),
            }],
            error: None,
        })
    })
    .await
    .unwrap();
    assert_eq!(*calls.lock(), 1_024);
    assert_eq!(read.requests, 1_024);
    let failure = read.failure.clone().expect("named failure");
    assert!(failure.contains("1024 requests"), "{failure}");
    assert!(!read.is_clean());
    assert!(!read.transactions.is_empty(), "what was delivered is kept");
}

// ---------------------------------------------------------------------------------------------
// R39.11.1 -- the median time past
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn median_time_past_is_computed_locally_from_compact_block_times() {
    struct Bound;
    impl WalletBranchStore for Bound {
        fn state(&self) -> Result<WalletBranchState, String> { Ok(scanned_wallet(IRONWOOD_BRANCH)) }

        fn record(&self, _: u32) -> Result<(), String> { Ok(()) }
    }
    let transport = Arc::new(FakeTransport::default());
    transport.serve("a", healthy(&[]));
    let backend = backend_over(&transport, &["a"]);
    backend.bind_wallet(Arc::new(Bound));
    // times are 1000 + height; the median of heights 90..=100 is 95.
    assert_eq!(backend.median_time_past(100, 11).await.unwrap(), 1_095);
    assert_eq!(transport.called("a", "time"), 11);
    assert!(matches!(
        backend.median_time_past(100, MEDIAN_TIME_WINDOW_MAX + 1).await,
        Err(LightwalletdError::InvalidRequest(_))
    ));
}

// ---------------------------------------------------------------------------------------------
// Ironwood activation discovery (R39.6.4e) against the scripted transport, whose block at height
// `h` carries time `1000 + h`.
// ---------------------------------------------------------------------------------------------

/// The activation time under which block `transition` is the first one over it.
fn time_of_transition(transition: u64) -> u32 { 1_000 + transition as u32 - 1 }

async fn discovered(
    branch: u32,
    tip: u64,
    activation_time: Option<u32>,
) -> Result<IronwoodStatus, IronwoodDiscoveryError> {
    let transport = Arc::new(FakeTransport::default());
    for server in ["a", "b"] {
        transport.serve(server, Script {
            info: Some(Ok(info(branch, tip))),
            ..Script::default()
        });
    }
    discover(&backend_over(&transport, &["a", "b"]), activation_time).await
}

#[tokio::test]
async fn a_server_on_ironwood_with_a_known_time_gives_the_derived_height() {
    // Transition at 4_000_000, tip far beyond: height is the transition plus the settling margin.
    let status = discovered(IRONWOOD_BRANCH, ARRR_TIP, Some(time_of_transition(4_000_000))).await;
    assert_eq!(status, Ok(IronwoodStatus::ActiveFrom(4_000_060)));
}

#[tokio::test]
async fn a_server_on_ironwood_without_a_time_is_taken_at_its_word() {
    // The test chain's case: no built-in time applies, the server says Ironwood is active.
    assert_eq!(
        discovered(IRONWOOD_BRANCH, ARRR_TIP, None).await,
        Ok(IronwoodStatus::ActiveFrom(ARRR_TIP as u32))
    );
}

#[tokio::test]
async fn a_server_before_ironwood_stays_not_activated_until_the_height_is_known() {
    let t = time_of_transition(4_167_000);
    // The tip has not reached the activation time at all.
    assert_eq!(
        discovered(SAPLING_BRANCH, 4_166_999, Some(t)).await,
        Ok(IronwoodStatus::NotActivated)
    );
    // The transition block exists but is not yet 30 deep: the chain itself does not know the height.
    assert_eq!(
        discovered(SAPLING_BRANCH, 4_167_020, Some(t)).await,
        Ok(IronwoodStatus::NotActivated)
    );
    // Buried: the upcoming height is known, and the tip is still below it.
    assert_eq!(
        discovered(SAPLING_BRANCH, 4_167_030, Some(t)).await,
        Ok(IronwoodStatus::ActiveFrom(4_167_060))
    );
}

#[tokio::test]
async fn a_server_that_disagrees_with_the_timestamp_rule_is_refused() {
    let t = time_of_transition(4_167_000);
    let err = discovered(SAPLING_BRANCH, ARRR_TIP, Some(t)).await.unwrap_err();
    assert!(matches!(err, IronwoodDiscoveryError::Inconsistent(_)), "{err}");
}

#[tokio::test]
async fn an_unreachable_server_is_an_error_and_not_a_default() {
    let transport = Arc::new(FakeTransport::default());
    let err = discover(&backend_over(&transport, &["a", "b"]), Some(1))
        .await
        .unwrap_err();
    assert!(matches!(err, IronwoodDiscoveryError::Unavailable(_)), "{err}");
}

// ---------------------------------------------------------------------------------------------
// R39.11.7a(h): a pre-Ironwood endpoint is stale once the activation time has passed
// ---------------------------------------------------------------------------------------------

const ARRR_ACTIVATION_TIME: u32 = 1_791_054_000;
const AFTER_ACTIVATION: u64 = ARRR_ACTIVATION_TIME as u64 + 86_400;
const STALE_TIP: u64 = 4_161_184;

fn timed_policy() -> AdmissionPolicy {
    AdmissionPolicy::from_params(&arrr_params()).with_ironwood_activation_time(Some(ARRR_ACTIVATION_TIME))
}

fn stale_three_with_healthy_unreachable() -> Vec<(String, Result<AdmissionProbe, String>)> {
    probes(vec![
        ("lightd1", Err("timed out")),
        ("piratelightd1", Err("timed out")),
        ("piratelightd2", Err("timed out")),
        ("piratelightd3", Err("timed out")),
        ("piratelightd4", Err("timed out")),
        ("cipig1", Ok(info(SAPLING_BRANCH, STALE_TIP))),
        ("cipig2", Ok(info(SAPLING_BRANCH, STALE_TIP))),
        ("cipig3", Ok(info(SAPLING_BRANCH, STALE_TIP))),
    ])
}

#[test]
fn the_pre_ironwood_branch_is_derived_from_the_configuration() {
    assert_eq!(timed_policy().pre_ironwood_branch, Some(SAPLING_BRANCH));
}

/// The live scenario: only the stale operator's three endpoints answer, all on the pre-upgrade
/// branch, after activation. They form a majority, and must still not be admitted.
#[test]
fn three_stale_endpoints_are_not_admitted_after_activation_even_as_the_only_ones_reachable() {
    let err = decide_admission_at(
        stale_three_with_healthy_unreachable(),
        &timed_policy(),
        &fresh_wallet(),
        AFTER_ACTIVATION,
    )
    .unwrap_err();
    match err {
        LightwalletdError::AllExcluded(text) => {
            assert!(text.contains("cipig1") && text.contains("pre-Ironwood"), "{text}")
        },
        other => panic!("{other:?}"),
    }
}

/// Before the activation time the pre-upgrade branch is the correct answer, and the boundary is the
/// activation second itself.
#[test]
fn the_same_three_endpoints_are_admitted_before_activation() {
    for now in [0, ARRR_ACTIVATION_TIME as u64 - 1] {
        let outcome = decide_admission_at(
            stale_three_with_healthy_unreachable(),
            &timed_policy(),
            &fresh_wallet(),
            now,
        )
        .unwrap();
        assert_eq!(outcome.admitted, vec!["cipig1", "cipig2", "cipig3"]);
        assert_eq!(outcome.branch_to_record, Some(SAPLING_BRANCH));
    }
    assert!(decide_admission_at(
        stale_three_with_healthy_unreachable(),
        &timed_policy(),
        &fresh_wallet(),
        ARRR_ACTIVATION_TIME as u64
    )
    .is_err());
}

/// Exclusion is information, not a vote: three stale endpoints outnumbering two healthy ones lose.
#[test]
fn a_stale_majority_cannot_outvote_the_current_chain() {
    let entries = || {
        probes(vec![
            ("a", Ok(info(SAPLING_BRANCH, STALE_TIP))),
            ("b", Ok(info(SAPLING_BRANCH, STALE_TIP))),
            ("c", Ok(info(SAPLING_BRANCH, STALE_TIP))),
            ("d", Ok(info(IRONWOOD_BRANCH, ARRR_TIP))),
            ("e", Ok(info(IRONWOOD_BRANCH, ARRR_TIP))),
        ])
    };
    let outcome = decide_admission_at(entries(), &timed_policy(), &fresh_wallet(), AFTER_ACTIVATION).unwrap();
    assert_eq!(outcome.admitted, vec!["d", "e"]);
    assert_eq!(outcome.excluded.len(), 3);
    assert_eq!(outcome.branch_to_record, Some(IRONWOOD_BRANCH));

    // Without a known activation time the earlier behaviour is unchanged: the stale three win.
    let untimed = AdmissionPolicy::from_params(&arrr_params());
    let outcome = decide_admission_at(entries(), &untimed, &fresh_wallet(), AFTER_ACTIVATION).unwrap();
    assert_eq!(outcome.admitted, vec!["a", "b", "c"]);
}

/// A wallet bound to the pre-upgrade branch by an earlier run (every wallet that synced before
/// activation is) must not stay bound once activation has passed: the binding would exclude every
/// current endpoint and leave only the stale fork.
#[test]
fn a_wallet_bound_to_the_pre_ironwood_branch_is_released_after_activation() {
    let entries_with = |cipig_height: u64| {
        probes(vec![
            ("healthy1", Ok(info(IRONWOOD_BRANCH, ARRR_TIP))),
            ("healthy2", Ok(info(IRONWOOD_BRANCH, ARRR_TIP))),
            ("cipig1", Ok(info(SAPLING_BRANCH, cipig_height))),
        ])
    };
    // After activation the frozen node sits at its old tip; before it, the chain is still one
    // chain, so its node is level with the others (a frozen tip would be stale, R39.11.7a(j)).
    let entries = || entries_with(STALE_TIP);
    let bound = scanned_wallet(SAPLING_BRANCH);

    let after = decide_admission_at(entries(), &timed_policy(), &bound, AFTER_ACTIVATION).unwrap();
    assert_eq!(after.admitted, vec!["healthy1", "healthy2"]);
    assert_eq!(after.branch_to_record, Some(IRONWOOD_BRANCH));

    // Before activation the binding holds exactly as before.
    let before = decide_admission_at(
        entries_with(ARRR_TIP),
        &timed_policy(),
        &bound,
        ARRR_ACTIVATION_TIME as u64 - 1,
    )
    .unwrap();
    assert_eq!(before.admitted, vec!["cipig1"]);

    // A binding to any other branch is never released by this rule.
    let other = scanned_wallet(IRONWOOD_BRANCH);
    let outcome = decide_admission_at(entries(), &timed_policy(), &other, AFTER_ACTIVATION).unwrap();
    assert_eq!(outcome.admitted, vec!["healthy1", "healthy2"]);
    assert_eq!(outcome.branch_to_record, None);
}

/// Through the backend, which reads the clock itself: a stale-only set fails closed and the
/// failure is not cached as an admission.
#[tokio::test]
async fn the_backend_fails_closed_when_only_stale_endpoints_answer() {
    let transport = Arc::new(FakeTransport::default());
    for name in ["cipig1", "cipig2", "cipig3"] {
        transport.serve(name, stale_fork());
    }
    let backend = LightwalletdBackend::new(
        transport.clone(),
        ["lightd1", "cipig1", "cipig2", "cipig3"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect(),
        AdmissionPolicy::from_params(&arrr_params()).with_ironwood_activation_time(Some(1)),
        test_addressing(),
    );
    assert!(matches!(
        backend.admitted().await.unwrap_err(),
        LightwalletdError::AllExcluded(_)
    ));
    for name in ["cipig1", "cipig2", "cipig3"] {
        assert_eq!(transport.called(name, "tip"), 0);
    }
    // Every configured endpoint is probed, including the unreachable one.
    assert_eq!(transport.called("lightd1", "info"), 1);
}

// ---------------------------------------------------------------------------------------------
// T39.11.k -- staleness (R39.11.7a(j))
// ---------------------------------------------------------------------------------------------

const RECOVERED_TIP: u64 = 4_181_383;
const MATHNODES_CACHE: u64 = 840_761;

fn at_tip(height: u64) -> ServerInfo { info(IRONWOOD_BRANCH, height) }

fn healthy_probe(height: u64) -> Result<AdmissionProbe, String> { probe_with_cache(at_tip(height), Ok(height)) }

fn excluded_reason<'a>(outcome: &'a AdmissionOutcome, server: &str) -> Option<&'a ExclusionReason> {
    outcome.excluded.iter().find(|(s, _)| s == server).map(|(_, r)| r)
}

/// The live 2026-10-23 specimen: the node is at the tip and on the right branch, the compact-block
/// cache stopped at 840,761. Admission read only `GetLightdInfo`, so it admitted this endpoint.
#[test]
fn an_endpoint_with_a_current_node_and_a_stale_cache_is_excluded() {
    let policy = AdmissionPolicy::from_params(&arrr_params());
    let mut entries: Vec<(String, Result<AdmissionProbe, String>)> = ["a", "b", "c", "d", "e"]
        .into_iter()
        .map(|s| (s.to_owned(), healthy_probe(RECOVERED_TIP)))
        .collect();
    entries.insert(
        2,
        (
            "mathnodes".to_owned(),
            probe_with_cache(at_tip(RECOVERED_TIP), Ok(MATHNODES_CACHE)),
        ),
    );

    let outcome = decide_admission(entries, &policy, &fresh_wallet()).unwrap();
    assert_eq!(outcome.admitted, vec!["a", "b", "c", "d", "e"]);
    assert_eq!(
        excluded_reason(&outcome, "mathnodes"),
        Some(&ExclusionReason::CacheBehindNode {
            cache: MATHNODES_CACHE,
            node: RECOVERED_TIP,
            tolerance: STALENESS_TOLERANCE_BLOCKS
        })
    );
}

/// `GetLightdInfo` answers while `GetLatestBlock` says "Cache is empty. Server is probably not yet
/// ready".
#[test]
fn an_endpoint_whose_cache_is_empty_or_errors_is_excluded() {
    let policy = AdmissionPolicy::from_params(&arrr_params());
    let outcome = decide_admission(
        vec![
            ("a".to_owned(), healthy_probe(RECOVERED_TIP)),
            ("b".to_owned(), healthy_probe(RECOVERED_TIP)),
            (
                "empty".to_owned(),
                probe_with_cache(
                    at_tip(RECOVERED_TIP),
                    Err("Unknown: Cache is empty. Server is probably not yet ready"),
                ),
            ),
        ],
        &policy,
        &fresh_wallet(),
    )
    .unwrap();
    assert_eq!(outcome.admitted, vec!["a", "b"]);
    match excluded_reason(&outcome, "empty") {
        Some(ExclusionReason::CacheUnavailable(e)) => assert!(e.contains("Cache is empty"), "{e}"),
        other => panic!("{other:?}"),
    }
}

/// A node that is itself far behind the other endpoints is excluded, with its cache in step.
#[test]
fn an_endpoint_behind_the_consensus_of_the_others_is_excluded() {
    let policy = AdmissionPolicy::from_params(&arrr_params());
    let lagging = RECOVERED_TIP - STALENESS_TOLERANCE_BLOCKS - 1;
    let outcome = decide_admission(
        probes(vec![
            ("a", Ok(at_tip(RECOVERED_TIP))),
            ("b", Ok(at_tip(RECOVERED_TIP))),
            ("c", Ok(at_tip(RECOVERED_TIP))),
            ("slow", Ok(at_tip(lagging))),
        ]),
        &policy,
        &fresh_wallet(),
    )
    .unwrap();
    assert_eq!(outcome.admitted, vec!["a", "b", "c"]);
    assert!(matches!(
        excluded_reason(&outcome, "slow"),
        Some(ExclusionReason::BehindConsensus { served, .. }) if *served == lagging
    ));
}

/// Both sides of the threshold, and the normal lag a wallet must tolerate.
#[test]
fn ordinary_lag_is_tolerated_and_the_threshold_is_exact() {
    let policy = AdmissionPolicy::from_params(&arrr_params());
    let tol = STALENESS_TOLERANCE_BLOCKS;
    let decide = |node_lag: u64, cache_lag: u64| {
        decide_admission(
            vec![
                ("a".to_owned(), healthy_probe(RECOVERED_TIP)),
                ("b".to_owned(), healthy_probe(RECOVERED_TIP)),
                (
                    "x".to_owned(),
                    probe_with_cache(
                        at_tip(RECOVERED_TIP - node_lag),
                        Ok(RECOVERED_TIP - node_lag - cache_lag),
                    ),
                ),
            ],
            &policy,
            &fresh_wallet(),
        )
        .unwrap()
    };
    // A cache a few blocks behind its node, and a node a few blocks behind the others: healthy.
    assert!(decide(0, 3).admitted.contains(&"x".to_owned()));
    assert!(decide(3, 0).admitted.contains(&"x".to_owned()));
    assert!(decide(2, 2).admitted.contains(&"x".to_owned()));
    // Exactly the tolerance is still admitted; one block more is not.
    assert!(decide(0, tol).admitted.contains(&"x".to_owned()));
    assert!(!decide(0, tol + 1).admitted.contains(&"x".to_owned()));
    assert!(decide(tol, 0).admitted.contains(&"x".to_owned()));
    assert!(!decide(tol + 1, 0).admitted.contains(&"x".to_owned()));
}

/// A stale endpoint is excluded before the vote: it neither counts toward the majority nor hides
/// that only one current endpoint remains, and the failure names every endpoint that was dropped.
#[test]
fn a_stale_endpoint_does_not_vote_and_the_failure_names_it() {
    let policy = AdmissionPolicy::from_params(&arrr_params());
    let err = decide_admission(
        vec![
            ("current".to_owned(), healthy_probe(RECOVERED_TIP)),
            (
                "stalecache".to_owned(),
                probe_with_cache(at_tip(RECOVERED_TIP), Ok(MATHNODES_CACHE)),
            ),
            ("down".to_owned(), Err("timed out".to_owned())),
        ],
        &policy,
        &fresh_wallet(),
    )
    .unwrap_err();
    match err {
        LightwalletdError::NoBranchMajority(detail) => {
            assert!(detail.contains("37a5165b x1"), "{detail}");
            assert!(
                detail.contains("stalecache") && detail.contains("compact-block cache"),
                "{detail}"
            );
            assert!(detail.contains("down: timed out"), "{detail}");
        },
        other => panic!("{other:?}"),
    }
}

/// When every endpoint is stale, the failure says so.
#[test]
fn all_endpoints_stale_fails_with_every_reason() {
    let policy = AdmissionPolicy::from_params(&arrr_params());
    let err = decide_admission(
        vec![
            (
                "a".to_owned(),
                probe_with_cache(at_tip(RECOVERED_TIP), Ok(MATHNODES_CACHE)),
            ),
            (
                "b".to_owned(),
                probe_with_cache(at_tip(RECOVERED_TIP), Err("Cache is empty")),
            ),
        ],
        &policy,
        &fresh_wallet(),
    )
    .unwrap_err();
    match err {
        LightwalletdError::AllExcluded(text) => assert!(text.contains("a ") && text.contains("b "), "{text}"),
        other => panic!("{other:?}"),
    }
}

/// Through the backend and the transport trait: the two heights come from different RPCs and the
/// fixture lets them disagree. The stale-cache endpoint is never asked for anything.
#[tokio::test]
async fn the_backend_probes_the_cache_and_never_uses_a_stale_cache_endpoint() {
    let transport = Arc::new(FakeTransport::default());
    let mut stale = healthy(&[MATHNODES_CACHE]);
    stale.info = Some(Ok(at_tip(RECOVERED_TIP)));
    stale.cache = Some(Ok(MATHNODES_CACHE));
    transport.serve("mathnodes", stale);
    for name in ["a", "b"] {
        let mut ok = healthy(&[RECOVERED_TIP]);
        ok.info = Some(Ok(at_tip(RECOVERED_TIP)));
        transport.serve(name, ok);
    }
    let backend = backend_over(&transport, &["mathnodes", "a", "b"]);

    assert_eq!(backend.chain_tip().await.unwrap(), RECOVERED_TIP);
    assert_eq!(transport.called("mathnodes", "cache"), 1);
    assert_eq!(transport.called("mathnodes", "tip"), 0);
    let excluded = backend.excluded_endpoints();
    assert_eq!(excluded.len(), 1);
    assert_eq!(excluded[0].0, "mathnodes");
}

/// T39.11.7b / R39.11.7b: `[A, A, B]` with A and B on different branches is two endpoints with one
/// vote each, so a fresh wallet fails with no majority instead of admitting A 2-to-1.
#[tokio::test]
async fn admission_counts_duplicate_endpoint_once() {
    let transport = Arc::new(FakeTransport::default());
    transport.serve("a", healthy(&[ARRR_TIP]));
    let mut other_branch = healthy(&[ARRR_TIP]);
    other_branch.info = Some(Ok(info(SAPLING_BRANCH, ARRR_TIP)));
    transport.serve("b", other_branch);
    let backend = backend_over(&transport, &["a", "a", "b"]);

    assert_eq!(backend.servers, vec!["a".to_owned(), "b".to_owned()]);
    match backend.admitted().await.unwrap_err() {
        LightwalletdError::NoBranchMajority(observed) => assert!(
            observed.contains("37a5165b x1") && observed.contains("76b809bb x1"),
            "{observed}"
        ),
        other => panic!("{other:?}"),
    }
    assert_eq!(transport.called("a", "info"), 1);
}

/// T39.11.7b: spellings of one endpoint collapse to the first one, in configured order.
#[test]
fn endpoint_spellings_normalize_to_one() {
    let (kept, dropped) = dedupe_lightwalletd_endpoints(vec![
        "https://a.example:443/".to_owned(),
        "a.example".to_owned(),
        "B".to_owned(),
    ]);
    assert_eq!(kept, vec!["https://a.example:443/".to_owned(), "B".to_owned()]);
    assert_eq!(dropped, vec![(
        "a.example".to_owned(),
        "https://a.example:443/".to_owned()
    )]);

    // Host case, an explicit default port and a trailing slash do not make a different endpoint;
    // another port, another scheme or another host does.
    let (kept, dropped) = dedupe_lightwalletd_endpoints(
        [
            "https://lightd.example",
            "https://LIGHTD.EXAMPLE:443/",
            "lightd.example:9067",
            "http://lightd.example",
            "lightd2.example",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect(),
    );
    assert_eq!(kept, vec![
        "https://lightd.example".to_owned(),
        "lightd.example:9067".to_owned(),
        "http://lightd.example".to_owned(),
        "lightd2.example".to_owned(),
    ]);
    assert_eq!(dropped.len(), 1);
}

/// T39.11.7b: a list made only of one endpoint's spellings is a single endpoint, and admission
/// treats it as it treats any single endpoint (a fresh wallet has no majority of one).
#[tokio::test]
async fn only_duplicates_is_one_endpoint() {
    let transport = Arc::new(FakeTransport::default());
    transport.serve("a", healthy(&[ARRR_TIP]));
    let backend = backend_over(&transport, &["a", "a"]);

    assert_eq!(backend.servers, vec!["a".to_owned()]);
    match backend.admitted().await.unwrap_err() {
        LightwalletdError::NoBranchMajority(observed) => assert!(observed.contains("37a5165b x1"), "{observed}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(transport.called("a", "info"), 1);
}
