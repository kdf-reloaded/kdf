//! T39.11.g -- HTLC spend and already-sent detection over the transparent address index
//! (R39.11.4d, R39.11.4b(f), R39.11.17). The decisions are driven from hand-assembled evidence and the
//! orchestration from a scripted in-process transport: no network.

use super::*;
use serde_json::json;
use std::collections::VecDeque;

const REDEEM: [u8; 110] = [0x5a; 110];
const BRANCH: u32 = 0x37a5_165b;
const TIP: u64 = 4_167_700;
const FUNDING_HEIGHT: u64 = 4_167_535;
const SPEND_HEIGHT: u64 = 4_167_560;

fn hash() -> [u8; 20] {
    let h = dhash160(&REDEEM);
    let mut out = [0u8; 20];
    out.copy_from_slice(h.as_slice());
    out
}

fn htlc_script() -> Vec<u8> { p2sh_script_pubkey(&hash()) }

fn compact(len: usize) -> Vec<u8> {
    assert!(len < 0xfd);
    vec![len as u8]
}

/// A version-4 transaction with only transparent parts.
fn transparent_tx(inputs: &[([u8; 32], u32, Vec<u8>)], outputs: &[(u64, Vec<u8>)]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&0x8000_0004u32.to_le_bytes());
    b.extend_from_slice(&0x892F_2085u32.to_le_bytes());
    b.extend_from_slice(&compact(inputs.len()));
    for (txid, index, script) in inputs {
        b.extend_from_slice(txid);
        b.extend_from_slice(&index.to_le_bytes());
        b.extend_from_slice(&compact(script.len()));
        b.extend_from_slice(script);
        b.extend_from_slice(&0xffff_ffffu32.to_le_bytes());
    }
    b.extend_from_slice(&compact(outputs.len()));
    for (value, script) in outputs {
        b.extend_from_slice(&value.to_le_bytes());
        b.extend_from_slice(&compact(script.len()));
        b.extend_from_slice(script);
    }
    b.extend_from_slice(&0u32.to_le_bytes()); // lock time
    b.extend_from_slice(&1_000_000u32.to_le_bytes()); // expiry height
    b.extend_from_slice(&0i64.to_le_bytes()); // value balance
    b.extend_from_slice(&[0, 0, 0]); // no Sapling spends, outputs or joinsplits
    b
}

fn txid_of(bytes: &[u8]) -> [u8; 32] { *ZTransaction::read(bytes, BranchId::Sapling).unwrap().txid().as_ref() }

/// The funding shape the chain admits: no inputs, the P2SH output first, the reveal second.
fn funding_bytes() -> Vec<u8> {
    let mut reveal = vec![0x6a, REDEEM.len() as u8];
    reveal.extend_from_slice(&REDEEM);
    transparent_tx(&[], &[(100_000, htlc_script()), (0, reveal)])
}

/// A redeem of the HTLC: `<sig> <secret> OP_0 <redeem>`, paying nothing transparent.
fn spend_bytes(funding_txid: [u8; 32], vout: u32) -> Vec<u8> {
    let mut sig = vec![0x01, 0xaa, 0x20];
    sig.extend_from_slice(&[7u8; 32]);
    sig.push(0x00);
    sig.push(REDEEM.len() as u8);
    sig.extend_from_slice(&REDEEM);
    // A second, unrelated input first, so the matching input is not index 0.
    transparent_tx(&[([0x33; 32], 1, vec![0x51]), (funding_txid, vout, sig)], &[])
}

fn output() -> OutputRef {
    OutputRef {
        txid: txid_of(&funding_bytes()),
        vout: 0,
        script_pubkey: htlc_script(),
    }
}

fn raw(data: Vec<u8>, height: u64) -> RawTx {
    RawTx {
        data,
        height: Some(height),
    }
}

fn clean(txs: Vec<RawTx>) -> Result<HistoryRead, String> {
    Ok(HistoryRead {
        transactions: txs,
        ..HistoryRead::default()
    })
}

fn listed_row(script: Vec<u8>) -> TransparentUtxo {
    TransparentUtxo {
        txid: output().txid,
        index: 0,
        value_zat: 100_000,
        height: FUNDING_HEIGHT,
        script,
    }
}

fn form(
    server: &str,
    encoding: IndexEncoding,
    utxos: Result<Vec<TransparentUtxo>, String>,
    history: Result<HistoryRead, String>,
) -> FormEvidence {
    FormEvidence {
        server: server.to_owned(),
        encoding,
        utxos,
        history,
    }
}

fn empty(server: &str, encoding: IndexEncoding) -> FormEvidence { form(server, encoding, Ok(vec![]), clean(vec![])) }

fn failed(server: &str, encoding: IndexEncoding) -> FormEvidence {
    form(
        server,
        encoding,
        Err("502 Bad Gateway".to_owned()),
        Err("502 Bad Gateway".to_owned()),
    )
}

fn assert_incomplete(result: Result<SpendOutcome, HtlcIndexError>) -> String {
    match result {
        Err(HtlcIndexError::Incomplete(message)) => message,
        other => panic!("expected an incomplete read, got {:?}", other),
    }
}

// ---------------------------------------------------------------------------------------------
// Pure decisions
// ---------------------------------------------------------------------------------------------

#[test]
fn a_spend_under_the_pubkey_hash_form_is_found_and_its_input_is_the_matching_one() {
    let funding = funding_bytes();
    let spend = spend_bytes(output().txid, 0);
    let evidence = vec![
        empty("a", IndexEncoding::ScriptHash),
        form(
            "a",
            IndexEncoding::PubkeyHash,
            Ok(vec![]),
            clean(vec![raw(funding, FUNDING_HEIGHT), raw(spend.clone(), SPEND_HEIGHT)]),
        ),
    ];
    match decide_spend(&output(), FundingState::Mined(FUNDING_HEIGHT), &evidence).unwrap() {
        SpendOutcome::Spent(found) => {
            assert_eq!(found.tx, spend);
            assert_eq!(found.input_index, 1, "the spender's second input is the HTLC input");
            assert_eq!(found.height, Some(SPEND_HEIGHT));
            assert_eq!(found.encoding, IndexEncoding::PubkeyHash);
        },
        other => panic!("{:?}", other),
    }
}

#[test]
fn a_spend_under_the_script_hash_form_is_found_too() {
    let evidence = vec![
        form(
            "a",
            IndexEncoding::ScriptHash,
            Ok(vec![]),
            clean(vec![raw(spend_bytes(output().txid, 0), SPEND_HEIGHT)]),
        ),
        empty("a", IndexEncoding::PubkeyHash),
    ];
    match decide_spend(&output(), FundingState::Mined(FUNDING_HEIGHT), &evidence).unwrap() {
        SpendOutcome::Spent(found) => assert_eq!(found.encoding, IndexEncoding::ScriptHash),
        other => panic!("{:?}", other),
    }
}

#[test]
fn a_transaction_that_spends_another_output_is_not_the_spend() {
    let other_vout = spend_bytes(output().txid, 1);
    let other_txid = spend_bytes([0x44; 32], 0);
    let evidence = vec![
        empty("a", IndexEncoding::ScriptHash),
        form(
            "a",
            IndexEncoding::PubkeyHash,
            Ok(vec![]),
            clean(vec![raw(other_vout, SPEND_HEIGHT), raw(other_txid, SPEND_HEIGHT)]),
        ),
    ];
    assert!(!matches!(
        decide_spend(&output(), FundingState::Mined(FUNDING_HEIGHT), &evidence),
        Ok(SpendOutcome::Spent(_))
    ));
}

#[test]
fn an_output_listed_as_unspent_under_either_form_is_the_negative_answer() {
    for listed in [IndexEncoding::PubkeyHash, IndexEncoding::ScriptHash] {
        let other = if listed == IndexEncoding::PubkeyHash {
            IndexEncoding::ScriptHash
        } else {
            IndexEncoding::PubkeyHash
        };
        let evidence = vec![
            empty("a", other),
            form(
                "a",
                listed,
                Ok(vec![listed_row(htlc_script())]),
                clean(vec![raw(funding_bytes(), FUNDING_HEIGHT)]),
            ),
        ];
        assert_eq!(
            decide_spend(&output(), FundingState::Mined(FUNDING_HEIGHT), &evidence).unwrap(),
            SpendOutcome::NotSpent(NotSpentProof::UnspentOutputListed {
                server: "a".to_owned(),
                encoding: listed
            })
        );
    }
}

/// R39.11.4d(c): twenty bytes are the whole pubkey-hash key, so a row under it can belong to another
/// output. The script decides, not the key.
#[test]
fn a_row_whose_script_is_not_the_expected_p2sh_script_is_discarded() {
    let mut p2pkh = vec![0x76, 0xa9, 0x14];
    p2pkh.extend_from_slice(&hash());
    p2pkh.extend_from_slice(&[0x88, 0xac]);
    let evidence = vec![
        empty("a", IndexEncoding::ScriptHash),
        form(
            "a",
            IndexEncoding::PubkeyHash,
            Ok(vec![listed_row(p2pkh)]),
            clean(vec![]),
        ),
    ];
    // The mismatching row is not accepted as the HTLC's: the index has returned nothing for it.
    assert!(matches!(
        decide_spend(&output(), FundingState::Mined(FUNDING_HEIGHT), &evidence),
        Err(HtlcIndexError::IndexCoverage { .. })
    ));
}

/// The decisive case of T39.11.g: the funding transaction is mined, both forms answered, neither
/// returned it. That is an index-coverage failure, never "not spent".
#[test]
fn a_healthy_but_empty_index_for_a_mined_funding_is_a_coverage_failure_and_not_a_negative() {
    let evidence = vec![
        empty("a", IndexEncoding::ScriptHash),
        empty("a", IndexEncoding::PubkeyHash),
    ];
    for funding in [FundingState::Mined(FUNDING_HEIGHT), FundingState::Unknown] {
        let result = decide_spend(&output(), funding, &evidence);
        assert!(
            matches!(result, Err(HtlcIndexError::IndexCoverage { .. })),
            "{:?}",
            result
        );
        assert!(result.unwrap_err().is_named_condition());
    }
}

#[test]
fn an_unmined_funding_with_a_clean_empty_index_is_not_spent() {
    let evidence = vec![
        empty("a", IndexEncoding::ScriptHash),
        empty("a", IndexEncoding::PubkeyHash),
    ];
    assert_eq!(
        decide_spend(&output(), FundingState::Unmined, &evidence).unwrap(),
        SpendOutcome::NotSpent(NotSpentProof::FundingNotMined)
    );
}

/// A failed call and an empty success reach different outcomes.
#[test]
fn a_failed_call_is_incomplete_and_differs_from_an_empty_success() {
    let funding = FundingState::Mined(FUNDING_HEIGHT);
    let failed_calls = vec![
        failed("a", IndexEncoding::ScriptHash),
        failed("a", IndexEncoding::PubkeyHash),
    ];
    let message = assert_incomplete(decide_spend(&output(), funding, &failed_calls));
    assert!(message.contains("502"), "{message}");

    // Even for an unmined funding, where an empty success would be a negative answer.
    assert_incomplete(decide_spend(&output(), FundingState::Unmined, &failed_calls));

    // One form failing taints the endpoint: the other form's empty answer is not enough.
    let half = vec![
        failed("a", IndexEncoding::ScriptHash),
        empty("a", IndexEncoding::PubkeyHash),
    ];
    assert_incomplete(decide_spend(&output(), FundingState::Unmined, &half));

    let empty_success = vec![
        empty("a", IndexEncoding::ScriptHash),
        empty("a", IndexEncoding::PubkeyHash),
    ];
    assert!(matches!(
        decide_spend(&output(), funding, &empty_success),
        Err(HtlcIndexError::IndexCoverage { .. })
    ));
}

/// D39.11.g / R39.11.4b(f): a skipped height can be exactly the one holding the spend.
#[test]
fn a_skipped_height_leaves_the_decision_undetermined() {
    let gapped = HistoryRead {
        transactions: vec![raw(funding_bytes(), FUNDING_HEIGHT)],
        gaps: vec![SPEND_HEIGHT],
        ..HistoryRead::default()
    };
    let evidence = vec![
        empty("a", IndexEncoding::ScriptHash),
        form("a", IndexEncoding::PubkeyHash, Ok(vec![]), Ok(gapped)),
    ];
    let message = assert_incomplete(decide_spend(&output(), FundingState::Mined(FUNDING_HEIGHT), &evidence));
    assert!(message.contains("transaction index"), "{message}");

    let failing = HistoryRead {
        failure: Some("stream reset".to_owned()),
        ..HistoryRead::default()
    };
    let evidence = vec![
        empty("a", IndexEncoding::ScriptHash),
        form("a", IndexEncoding::PubkeyHash, Ok(vec![]), Ok(failing)),
    ];
    assert_incomplete(decide_spend(&output(), FundingState::Unmined, &evidence));
}

#[test]
fn a_spend_is_found_even_when_the_stream_that_delivered_it_failed_afterwards() {
    let partial = HistoryRead {
        transactions: vec![raw(spend_bytes(output().txid, 0), SPEND_HEIGHT)],
        failure: Some("stream reset".to_owned()),
        ..HistoryRead::default()
    };
    let evidence = vec![
        failed("a", IndexEncoding::ScriptHash),
        form("a", IndexEncoding::PubkeyHash, Ok(vec![]), Ok(partial)),
    ];
    assert!(matches!(
        decide_spend(&output(), FundingState::Unknown, &evidence),
        Ok(SpendOutcome::Spent(_))
    ));
}

/// The service drops the newest height group of a stream, and that is where an HTLC's spend lives, so a
/// history that shows the funding and no spender is not "not spent".
#[test]
fn a_history_showing_the_funding_and_no_spender_is_not_a_negative_answer() {
    let evidence = vec![
        empty("a", IndexEncoding::ScriptHash),
        form(
            "a",
            IndexEncoding::PubkeyHash,
            Ok(vec![]),
            clean(vec![raw(funding_bytes(), FUNDING_HEIGHT)]),
        ),
    ];
    assert_eq!(
        decide_spend(&output(), FundingState::Mined(FUNDING_HEIGHT), &evidence),
        Err(HtlcIndexError::SpenderNotDelivered)
    );
}

#[test]
fn an_undecodable_delivered_transaction_leaves_the_decision_undetermined() {
    let evidence = vec![
        empty("a", IndexEncoding::ScriptHash),
        form(
            "a",
            IndexEncoding::PubkeyHash,
            Ok(vec![]),
            clean(vec![raw(vec![1, 2, 3], SPEND_HEIGHT)]),
        ),
    ];
    let message = assert_incomplete(decide_spend(&output(), FundingState::Unmined, &evidence));
    assert!(message.contains("cannot read"), "{message}");
}

#[test]
fn a_healthy_endpoint_answers_for_a_failing_one() {
    let evidence = vec![
        failed("bad", IndexEncoding::PubkeyHash),
        failed("bad", IndexEncoding::ScriptHash),
        empty("good", IndexEncoding::ScriptHash),
        empty("good", IndexEncoding::PubkeyHash),
    ];
    assert_eq!(
        decide_spend(&output(), FundingState::Unmined, &evidence).unwrap(),
        SpendOutcome::NotSpent(NotSpentProof::FundingNotMined)
    );
}

#[test]
fn sent_detection_distinguishes_found_not_sent_and_could_not_tell() {
    let script = htlc_script();
    let funding = funding_bytes();

    let from_history = vec![
        empty("a", IndexEncoding::ScriptHash),
        form(
            "a",
            IndexEncoding::PubkeyHash,
            Ok(vec![]),
            clean(vec![raw(funding.clone(), FUNDING_HEIGHT)]),
        ),
    ];
    assert_eq!(
        decide_sent(&script, &from_history).unwrap(),
        SentOutcome::Sent(FundingFound::Transaction {
            tx: funding,
            height: Some(FUNDING_HEIGHT)
        })
    );

    let from_listing = vec![
        form(
            "a",
            IndexEncoding::ScriptHash,
            Ok(vec![listed_row(script.clone())]),
            clean(vec![]),
        ),
        empty("a", IndexEncoding::PubkeyHash),
    ];
    assert_eq!(
        decide_sent(&script, &from_listing).unwrap(),
        SentOutcome::Sent(FundingFound::Listed { txid: output().txid })
    );

    let nothing = vec![
        empty("a", IndexEncoding::ScriptHash),
        empty("a", IndexEncoding::PubkeyHash),
    ];
    assert_eq!(decide_sent(&script, &nothing).unwrap(), SentOutcome::NotSent);

    let broken = vec![
        failed("a", IndexEncoding::ScriptHash),
        failed("a", IndexEncoding::PubkeyHash),
    ];
    assert!(matches!(
        decide_sent(&script, &broken),
        Err(HtlcIndexError::Incomplete(_))
    ));

    // A gap is not an empty window.
    let gapped = vec![
        empty("a", IndexEncoding::ScriptHash),
        form(
            "a",
            IndexEncoding::PubkeyHash,
            Ok(vec![]),
            Ok(HistoryRead {
                gaps: vec![FUNDING_HEIGHT],
                ..HistoryRead::default()
            }),
        ),
    ];
    assert!(matches!(
        decide_sent(&script, &gapped),
        Err(HtlcIndexError::Incomplete(_))
    ));
}

#[test]
fn sent_detection_ignores_a_transaction_paying_another_script_and_a_listing_with_another_script() {
    let other = p2sh_script_pubkey(&[9u8; 20]);
    let elsewhere = transparent_tx(&[], &[(5, other.clone())]);
    let evidence = vec![
        form(
            "a",
            IndexEncoding::ScriptHash,
            Ok(vec![listed_row(other)]),
            clean(vec![raw(elsewhere, FUNDING_HEIGHT)]),
        ),
        empty("a", IndexEncoding::PubkeyHash),
    ];
    assert_eq!(decide_sent(&htlc_script(), &evidence).unwrap(), SentOutcome::NotSent);
}

#[test]
fn the_script_shape_check_accepts_only_pay_to_script_hash() {
    assert_eq!(p2sh_hash_of(&htlc_script()), Some(hash()));
    assert_eq!(p2sh_hash_of(&[0xa9, 0x14, 0x00, 0x87]), None);
    let mut p2pkh = vec![0x76, 0xa9, 0x14];
    p2pkh.extend_from_slice(&hash());
    p2pkh.extend_from_slice(&[0x88, 0xac]);
    assert_eq!(p2sh_hash_of(&p2pkh), None);
    let mut long = htlc_script();
    long.push(0);
    assert_eq!(p2sh_hash_of(&long), None);
}

/// The two forms are the same twenty bytes under the two version bytes of the chain, so the probe
/// really asks two different questions.
#[test]
fn the_two_forms_encode_the_same_twenty_bytes_under_the_two_version_bytes() {
    let addressing = HtlcAddressing {
        p2sh_prefix: 85,
        p2sh_t_prefix: 0,
        pubkey_prefix: 60,
        pubkey_t_prefix: 0,
        checksum_type: ChecksumType::DSHA256,
    };
    let p2sh = addressing.address(IndexEncoding::ScriptHash, hash()).to_string();
    let pubkey = addressing.address(IndexEncoding::PubkeyHash, hash()).to_string();
    assert!(p2sh.starts_with('b'), "{p2sh}");
    assert!(pubkey.starts_with('R'), "{pubkey}");
    assert_ne!(p2sh, pubkey);

    // The live fixture of the 2026-10-12 probe.
    let live = hex::decode("978654dd2ac745a1a83e58795125dc53f5686a34").unwrap();
    let mut live_hash = [0u8; 20];
    live_hash.copy_from_slice(&live);
    assert_eq!(
        addressing.address(IndexEncoding::PubkeyHash, live_hash).to_string(),
        "RP6P2zsq8scUKdzHjvZSrxMzfoZzmcpGtc"
    );
    assert_eq!(
        addressing.address(IndexEncoding::ScriptHash, live_hash).to_string(),
        "bSYTeiL2tPCNmUUTMQtQz6AfQR1aWwzhNK"
    );
}

// ---------------------------------------------------------------------------------------------
// Orchestration over a scripted transport
// ---------------------------------------------------------------------------------------------

/// What one endpoint answers.
#[derive(Clone, Default)]
struct Endpoint {
    /// `None` fails every index method with a gateway error (D39.11.g: admitted, then unusable).
    index_ok: bool,
    funding_lookup: Option<Result<RawTx, EndpointError>>,
    /// Rows by address string.
    utxos: HashMap<String, Vec<TransparentUtxo>>,
    history: HashMap<String, Vec<RawTx>>,
    /// Heights whose window fails with a serve failure.
    unservable: Vec<u64>,
    tips: VecDeque<u64>,
}

#[derive(Debug, Default)]
struct Scripted {
    endpoints: Mutex<HashMap<String, Endpoint>>,
    requests: Mutex<Vec<(String, String, String)>>,
    history_ranges: Mutex<Vec<(u64, u64)>>,
}

impl fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("Endpoint") }
}

impl Scripted {
    fn serve(&self, server: &str, endpoint: Endpoint) { self.endpoints.lock().insert(server.to_owned(), endpoint); }

    fn asked(&self, server: &str, method: &str) -> Vec<String> {
        self.requests
            .lock()
            .iter()
            .filter(|(s, m, _)| s == server && m == method)
            .map(|(_, _, a)| a.clone())
            .collect()
    }
}

#[async_trait]
impl LightwalletdTransport for Scripted {
    async fn light_info(&self, server: &str) -> Result<ServerInfo, EndpointError> {
        if self.endpoints.lock().contains_key(server) {
            Ok(ServerInfo {
                chain_name: "main".to_owned(),
                sapling_activation_height: 152855,
                consensus_branch_id: format!("{:08x}", BRANCH),
                block_height: TIP,
            })
        } else {
            Err(EndpointError::Transport("unreachable".to_owned()))
        }
    }

    async fn cache_height(&self, _server: &str) -> Result<u64, EndpointError> { Ok(TIP) }

    async fn latest_block(&self, server: &str) -> Result<u64, EndpointError> {
        Ok(self
            .endpoints
            .lock()
            .get_mut(server)
            .and_then(|e| e.tips.pop_front())
            .unwrap_or(TIP))
    }

    async fn block_time(&self, _server: &str, height: u64) -> Result<u32, EndpointError> { Ok(height as u32) }

    async fn transaction(&self, server: &str, _txid: [u8; 32]) -> Result<RawTx, EndpointError> {
        self.endpoints
            .lock()
            .get(server)
            .and_then(|e| e.funding_lookup.clone())
            .unwrap_or_else(|| Err(EndpointError::Service("502 Bad Gateway".to_owned())))
    }

    async fn send_transaction(&self, _server: &str, _data: Vec<u8>) -> Result<(), EndpointError> {
        Err(EndpointError::Transport("not scripted".to_owned()))
    }

    async fn taddr_balance(&self, _server: &str, _addresses: Vec<String>) -> Result<i64, EndpointError> {
        Err(EndpointError::Transport("not scripted".to_owned()))
    }

    async fn taddr_txids(&self, server: &str, address: &str, start: u64, end: u64) -> WindowRead {
        self.requests
            .lock()
            .push((server.to_owned(), "history".to_owned(), address.to_owned()));
        self.history_ranges.lock().push((start, end));
        let endpoints = self.endpoints.lock();
        let Some(endpoint) = endpoints.get(server) else {
            return WindowRead::default();
        };
        if !endpoint.index_ok {
            return WindowRead {
                txs: Vec::new(),
                error: Some(WindowError {
                    serve_failure: false,
                    message: "502 Bad Gateway".to_owned(),
                }),
            };
        }
        if endpoint.unservable.iter().any(|h| (start..=end).contains(h)) {
            return WindowRead {
                txs: Vec::new(),
                error: Some(WindowError {
                    serve_failure: true,
                    message: "no such mempool or blockchain transaction".to_owned(),
                }),
            };
        }
        let txs = endpoint
            .history
            .get(address)
            .map(|txs| {
                txs.iter()
                    .filter(|t| t.height.map_or(false, |h| (start..=end).contains(&h)))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        WindowRead { txs, error: None }
    }

    async fn address_utxos(&self, server: &str, address: &str) -> Result<Vec<TransparentUtxo>, EndpointError> {
        self.requests
            .lock()
            .push((server.to_owned(), "utxos".to_owned(), address.to_owned()));
        let endpoints = self.endpoints.lock();
        match endpoints.get(server) {
            Some(e) if e.index_ok => Ok(e.utxos.get(address).cloned().unwrap_or_default()),
            _ => Err(EndpointError::Service("502 Bad Gateway".to_owned())),
        }
    }
}

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

fn addressing() -> HtlcAddressing {
    HtlcAddressing {
        p2sh_prefix: 85,
        p2sh_t_prefix: 0,
        pubkey_prefix: 60,
        pubkey_t_prefix: 0,
        checksum_type: ChecksumType::DSHA256,
    }
}

struct Bound;
impl WalletBranchStore for Bound {
    fn state(&self) -> Result<WalletBranchState, String> {
        Ok(WalletBranchState {
            has_scanned_state: true,
            recorded: Some(BRANCH),
        })
    }

    fn record(&self, _: u32) -> Result<(), String> { Ok(()) }
}

fn backend(transport: &Arc<Scripted>, servers: &[&str]) -> LightwalletdBackend {
    let backend = LightwalletdBackend::new(
        transport.clone(),
        servers.iter().map(|s| (*s).to_owned()).collect(),
        AdmissionPolicy::from_params(&arrr_params()),
        addressing(),
    );
    backend.bind_wallet(Arc::new(Bound));
    backend
}

fn form_address(encoding: IndexEncoding) -> String { addressing().address(encoding, hash()).to_string() }

fn healthy_endpoint() -> Endpoint {
    Endpoint {
        index_ok: true,
        ..Endpoint::default()
    }
}

#[tokio::test]
async fn both_forms_are_queried_with_bounded_windows_and_the_spend_is_found() {
    let transport = Arc::new(Scripted::default());
    let mut endpoint = healthy_endpoint();
    endpoint.funding_lookup = Some(Ok(raw(funding_bytes(), FUNDING_HEIGHT)));
    endpoint.history.insert(form_address(IndexEncoding::PubkeyHash), vec![
        raw(funding_bytes(), FUNDING_HEIGHT),
        raw(spend_bytes(output().txid, 0), SPEND_HEIGHT),
    ]);
    transport.serve("a", endpoint);
    let backend = backend(&transport, &["a"]);

    let outcome = backend.detect_output_spend(&output(), FUNDING_HEIGHT).await.unwrap();
    assert!(matches!(outcome, SpendOutcome::Spent(ref s) if s.encoding == IndexEncoding::PubkeyHash));

    for encoding in [IndexEncoding::PubkeyHash, IndexEncoding::ScriptHash] {
        let address = form_address(encoding);
        assert_eq!(transport.asked("a", "utxos"), vec![
            form_address(IndexEncoding::PubkeyHash),
            form_address(IndexEncoding::ScriptHash)
        ]);
        assert!(
            transport.asked("a", "history").contains(&address),
            "{encoding:?} was not queried"
        );
    }
    assert!(
        transport.history_ranges.lock().iter().all(|(s, e)| *s >= 1 && *e >= *s),
        "a request bound below 1 or an inverted range was sent"
    );
    assert_eq!(backend.answering_encoding("a"), Some(IndexEncoding::PubkeyHash));
}

#[tokio::test]
async fn an_older_build_that_filed_the_rows_under_script_hash_is_handled() {
    let transport = Arc::new(Scripted::default());
    let mut endpoint = healthy_endpoint();
    endpoint.funding_lookup = Some(Ok(raw(funding_bytes(), FUNDING_HEIGHT)));
    endpoint
        .utxos
        .insert(form_address(IndexEncoding::ScriptHash), vec![listed_row(htlc_script())]);
    transport.serve("a", endpoint);
    let backend = backend(&transport, &["a"]);
    let outcome = backend.detect_output_spend(&output(), FUNDING_HEIGHT).await.unwrap();
    assert_eq!(
        outcome,
        SpendOutcome::NotSpent(NotSpentProof::UnspentOutputListed {
            server: "a".to_owned(),
            encoding: IndexEncoding::ScriptHash
        })
    );
    assert_eq!(backend.answering_encoding("a"), Some(IndexEncoding::ScriptHash));
}

/// D39.11.g: an endpoint admitted on `GetLightdInfo` that then fails the methods needed.
#[tokio::test]
async fn an_endpoint_that_passes_admission_and_then_fails_the_index_is_skipped_not_believed() {
    let transport = Arc::new(Scripted::default());
    transport.serve("broken", Endpoint::default());
    let mut good = healthy_endpoint();
    good.funding_lookup = Some(Ok(raw(funding_bytes(), FUNDING_HEIGHT)));
    good.utxos
        .insert(form_address(IndexEncoding::PubkeyHash), vec![listed_row(htlc_script())]);
    transport.serve("good", good);
    let backend = backend(&transport, &["broken", "good"]);
    let outcome = backend.detect_output_spend(&output(), FUNDING_HEIGHT).await.unwrap();
    assert!(matches!(
        outcome,
        SpendOutcome::NotSpent(NotSpentProof::UnspentOutputListed { .. })
    ));
    assert!(
        !transport.asked("broken", "utxos").is_empty(),
        "the broken endpoint was tried first"
    );
}

#[tokio::test]
async fn when_every_endpoint_fails_the_index_the_detection_fails_and_does_not_answer() {
    let transport = Arc::new(Scripted::default());
    transport.serve("broken", Endpoint::default());
    let backend = backend(&transport, &["broken"]);
    let error = backend
        .detect_output_spend(&output(), FUNDING_HEIGHT)
        .await
        .unwrap_err();
    assert!(matches!(error, HtlcIndexError::Incomplete(_)), "{error:?}");

    let error = backend
        .detect_funding(&htlc_script(), FUNDING_HEIGHT)
        .await
        .unwrap_err();
    assert!(matches!(error, HtlcIndexError::Incomplete(_)), "{error:?}");
}

#[tokio::test]
async fn a_healthy_empty_index_for_a_mined_funding_fails_with_the_coverage_condition() {
    let transport = Arc::new(Scripted::default());
    let mut endpoint = healthy_endpoint();
    endpoint.funding_lookup = Some(Ok(raw(funding_bytes(), FUNDING_HEIGHT)));
    transport.serve("a", endpoint);
    let backend = backend(&transport, &["a"]);
    let error = backend
        .detect_output_spend(&output(), FUNDING_HEIGHT)
        .await
        .unwrap_err();
    assert!(matches!(error, HtlcIndexError::IndexCoverage { .. }), "{error:?}");
    assert!(error.to_string().contains("mined at height 4167535"), "{error}");
}

/// A node without its transaction index cannot serve the spend or the spent funding transaction.
#[tokio::test]
async fn an_unservable_height_is_reported_as_a_gap_condition_and_not_as_no_spend() {
    let transport = Arc::new(Scripted::default());
    let mut endpoint = healthy_endpoint();
    endpoint.funding_lookup = Some(Err(EndpointError::Service(
        "no such mempool or blockchain transaction".to_owned(),
    )));
    endpoint.unservable = vec![FUNDING_HEIGHT, SPEND_HEIGHT];
    transport.serve("a", endpoint);
    let backend = backend(&transport, &["a"]);
    let error = backend
        .detect_output_spend(&output(), FUNDING_HEIGHT)
        .await
        .unwrap_err();
    match error {
        HtlcIndexError::Incomplete(message) => assert!(message.contains("skipped height"), "{message}"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn the_search_starts_no_lower_than_the_sapling_floor_and_takes_in_the_funding_height() {
    let transport = Arc::new(Scripted::default());
    let mut endpoint = healthy_endpoint();
    endpoint.funding_lookup = Some(Ok(raw(funding_bytes(), 160_000)));
    endpoint.tips = VecDeque::from([153_500]);
    transport.serve("a", endpoint);
    let backend = backend(&transport, &["a"]);
    let _ = backend.detect_output_spend(&output(), 0).await;
    let ranges = transport.history_ranges.lock().clone();
    assert_eq!(ranges.first().map(|r| r.0), Some(152855), "{ranges:?}");
    assert!(ranges.iter().all(|(s, _)| *s >= 1));
    // The funding height is above the endpoint's tip, and the window reaches it anyway.
    assert!(ranges.iter().any(|(_, e)| *e >= 160_000), "{ranges:?}");

    // A funding below the caller's start widens the window instead of excluding it.
    let (from, to) = backend.history_window(FUNDING_HEIGHT + 100, TIP, Some(FundingState::Mined(FUNDING_HEIGHT)));
    assert_eq!((from, to), (FUNDING_HEIGHT, TIP));
}

#[tokio::test]
async fn already_sent_detection_finds_the_payment_and_reports_nothing_only_from_a_healthy_index() {
    let transport = Arc::new(Scripted::default());
    let mut sent = healthy_endpoint();
    sent.history.insert(form_address(IndexEncoding::PubkeyHash), vec![raw(
        funding_bytes(),
        FUNDING_HEIGHT,
    )]);
    transport.serve("sent", sent);
    let found = backend(&transport, &["sent"])
        .detect_funding(&htlc_script(), FUNDING_HEIGHT)
        .await
        .unwrap();
    assert!(matches!(found, SentOutcome::Sent(FundingFound::Transaction { .. })));

    let transport = Arc::new(Scripted::default());
    transport.serve("quiet", healthy_endpoint());
    let none = backend(&transport, &["quiet"])
        .detect_funding(&htlc_script(), FUNDING_HEIGHT)
        .await
        .unwrap();
    assert_eq!(none, SentOutcome::NotSent);
}
