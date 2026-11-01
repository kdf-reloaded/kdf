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
const LOG: DetectionLog<'static> = DetectionLog {
    coin: "ARRR",
    polling: false,
};

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
    /// Every history and UTXO request fails with this node message under an Unknown status, classified by
    /// the transport's own `status_error`, while the transaction lookup, the tip and the server
    /// information still answer: a node with no address index (R39.11.4i).
    refuse_index: Option<String>,
    /// Every history and UTXO request fails with this message under an Internal status: a refusal that
    /// is not the no-address-index message, so it is a serve failure of R39.11.4b(c).
    refuse_generic: Option<String>,
    /// Every history request is answered only after this long.
    history_delay: Option<Duration>,
}

/// The node's own reason, as the field report has it: the status message, and the status text the
/// wallet builds from it.
const NO_INDEX_STATUS_MESSAGE: &str = "-5: No information available for address";
const NO_INDEX_NODE_MESSAGE: &str = "Unknown error: -5: No information available for address";
const GENERIC_STATUS_MESSAGE: &str = "node trouble";
/// What a generic refusal reads like after the wallet's classification.
const GENERIC_REFUSAL_TEXT: &str = "Internal error: node trouble";

/// The error the real transport builds for an answer with this status.
fn classified(status: tonic::Status) -> EndpointError { status_error(status) }

fn refusal_of(endpoint: &Endpoint) -> Option<EndpointError> {
    endpoint
        .refuse_index
        .as_deref()
        .map(|message| classified(tonic::Status::unknown(message)))
        .or_else(|| {
            endpoint
                .refuse_generic
                .as_deref()
                .map(|message| classified(tonic::Status::internal(message)))
        })
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

    async fn block_hash(&self, _server: &str, _height: u64) -> Result<[u8; 32], EndpointError> {
        Err(EndpointError::Transport("not scripted".to_owned()))
    }

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
        let (delay, refusal) = self
            .endpoints
            .lock()
            .get(server)
            .map(|e| (e.history_delay, refusal_of(e)))
            .unwrap_or_default();
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        if let Some(refusal) = refusal {
            return WindowRead {
                txs: Vec::new(),
                error: Some(WindowError::from(&refusal)),
            };
        }
        let endpoints = self.endpoints.lock();
        let Some(endpoint) = endpoints.get(server) else {
            return WindowRead::default();
        };
        if !endpoint.index_ok {
            return WindowRead {
                txs: Vec::new(),
                error: Some(WindowError {
                    serve_failure: false,
                    transport: false,
                    message: "502 Bad Gateway".to_owned(),
                }),
            };
        }
        if endpoint.unservable.iter().any(|h| (start..=end).contains(h)) {
            return WindowRead {
                txs: Vec::new(),
                error: Some(WindowError {
                    serve_failure: true,
                    transport: false,
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
        if let Some(refusal) = endpoints.get(server).and_then(refusal_of) {
            return Err(refusal);
        }
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

    let outcome = backend
        .detect_output_spend(&output(), FUNDING_HEIGHT, &LOG)
        .await
        .unwrap();
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
    let outcome = backend
        .detect_output_spend(&output(), FUNDING_HEIGHT, &LOG)
        .await
        .unwrap();
    assert_eq!(
        outcome,
        SpendOutcome::NotSpent(NotSpentProof::UnspentOutputListed {
            server: "https://a".to_owned(),
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
    let outcome = backend
        .detect_output_spend(&output(), FUNDING_HEIGHT, &LOG)
        .await
        .unwrap();
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
        .detect_output_spend(&output(), FUNDING_HEIGHT, &LOG)
        .await
        .unwrap_err();
    assert!(matches!(error, HtlcIndexError::Incomplete(_)), "{error:?}");

    let error = backend
        .detect_funding(&htlc_script(), FUNDING_HEIGHT, &LOG)
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
        .detect_output_spend(&output(), FUNDING_HEIGHT, &LOG)
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
        .detect_output_spend(&output(), FUNDING_HEIGHT, &LOG)
        .await
        .unwrap_err();
    match error {
        HtlcIndexError::Incomplete(message) => {
            // The genuine case (R39.11.4b(c)) keeps its explanation.
            assert!(message.contains("skipped height"), "{message}");
            assert!(message.contains("transaction index"), "{message}");
        },
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn the_search_starts_at_the_funding_height_and_without_one_no_lower_than_the_sapling_floor() {
    let transport = Arc::new(Scripted::default());
    let mut endpoint = healthy_endpoint();
    endpoint.funding_lookup = Some(Ok(raw(funding_bytes(), 160_000)));
    endpoint.tips = VecDeque::from([153_500]);
    transport.serve("a", endpoint);
    let backend = backend(&transport, &["a"]);
    let _ = backend.detect_output_spend(&output(), 0, &LOG).await;
    let ranges = transport.history_ranges.lock().clone();
    // R39.11.4h(a): a spend cannot precede its funding, so the caller's start (here the floor) is not read.
    assert_eq!(ranges.first().map(|r| r.0), Some(160_000), "{ranges:?}");
    assert!(ranges.iter().all(|(s, _)| *s >= 160_000));
    // The funding height is above the endpoint's tip, and the window reaches it anyway.
    assert!(ranges.iter().any(|(_, e)| *e >= 160_000), "{ranges:?}");

    // Whatever the caller's start, a mined funding fixes the start of the window.
    let (from, to) = backend.history_window(FUNDING_HEIGHT + 100, TIP, Some(FundingState::Mined(FUNDING_HEIGHT)));
    assert_eq!((from, to), (FUNDING_HEIGHT, TIP));
    let (from, to) = backend.history_window(1, TIP, Some(FundingState::Mined(FUNDING_HEIGHT)));
    assert_eq!((from, to), (FUNDING_HEIGHT, TIP));
    // A funding below the Sapling floor is read from the floor.
    let (from, _) = backend.history_window(1, TIP, Some(FundingState::Mined(10)));
    assert_eq!(from, 152855);
    // Unknown or unmined funding, and already-sent detection: the caller's start applies, floored.
    for funding in [Some(FundingState::Unknown), Some(FundingState::Unmined), None] {
        assert_eq!(backend.history_window(0, TIP, funding), (152855, TIP));
        assert_eq!(
            backend.history_window(FUNDING_HEIGHT, TIP, funding),
            (FUNDING_HEIGHT, TIP)
        );
    }
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
        .detect_funding(&htlc_script(), FUNDING_HEIGHT, &LOG)
        .await
        .unwrap();
    assert!(matches!(found, SentOutcome::Sent(FundingFound::Transaction { .. })));

    let transport = Arc::new(Scripted::default());
    transport.serve("quiet", healthy_endpoint());
    let none = backend(&transport, &["quiet"])
        .detect_funding(&htlc_script(), FUNDING_HEIGHT, &LOG)
        .await
        .unwrap();
    assert_eq!(none, SentOutcome::NotSent);
}

// ---------------------------------------------------------------------------------------------
// T39.11.4h -- bounded HTLC detection
// ---------------------------------------------------------------------------------------------

/// A funding 30,000 blocks below the tip, spent 25 blocks later, on a clean endpoint.
const FAR_FUNDING_HEIGHT: u64 = TIP - 30_000;

fn clean_endpoint_with_spend(funding_height: u64) -> Endpoint {
    let mut endpoint = healthy_endpoint();
    endpoint.funding_lookup = Some(Ok(raw(funding_bytes(), funding_height)));
    endpoint.history.insert(form_address(IndexEncoding::PubkeyHash), vec![
        raw(funding_bytes(), funding_height),
        raw(spend_bytes(output().txid, 0), funding_height + 25),
    ]);
    endpoint
}

fn endpoint_without_address_index(funding_height: u64) -> Endpoint {
    Endpoint {
        index_ok: true,
        funding_lookup: Some(Ok(raw(funding_bytes(), funding_height))),
        refuse_index: Some(NO_INDEX_STATUS_MESSAGE.to_owned()),
        ..Endpoint::default()
    }
}

/// An endpoint that refuses every history and UTXO request with a generic service error, one that is not
/// the no-address-index message: the storm of R39.11.4h(b).
fn endpoint_refusing_every_read(funding_height: u64) -> Endpoint {
    Endpoint {
        index_ok: true,
        funding_lookup: Some(Ok(raw(funding_bytes(), funding_height))),
        refuse_generic: Some(GENERIC_STATUS_MESSAGE.to_owned()),
        ..Endpoint::default()
    }
}

/// An endpoint that answers every history and UTXO request with a generic service error is stopped after 18
/// history requests per address form: the first window of 1,000 blocks is bisected down one level at a
/// time to height 1 (11 requests: 1,000 wide, then 500, 250, 125, 63, 32, 16, 8, 4, 2, 1), and the
/// fifth unservable height ends the read after 7 more (2, 3-4, 3, 4, 5-8, 5-6, 5). The range is 30,000
/// blocks. The second endpoint then decides.
#[tokio::test]
async fn every_history_request_failing_is_stopped_after_few_requests() {
    let transport = Arc::new(Scripted::default());
    transport.serve("a", endpoint_refusing_every_read(FAR_FUNDING_HEIGHT));
    transport.serve("b", clean_endpoint_with_spend(FAR_FUNDING_HEIGHT));
    let backend = backend(&transport, &["a", "b"]);

    let outcome = backend.detect_output_spend(&output(), 0, &LOG).await.unwrap();
    assert!(
        matches!(outcome, SpendOutcome::Spent(ref s) if s.server == "https://b"),
        "{outcome:?}"
    );
    assert_eq!(
        transport.asked("a", "history").len(),
        2 * 18,
        "18 history requests per address form, not tens of thousands"
    );
    assert_eq!(transport.asked("a", "utxos").len(), 2);
    assert!(!transport.asked("b", "history").is_empty());
}

/// The endpoint's own reason is in the named failure, with the endpoint, the form and the address.
#[tokio::test]
async fn the_named_failure_carries_the_nodes_last_error_text_verbatim() {
    let transport = Arc::new(Scripted::default());
    transport.serve("a", endpoint_refusing_every_read(FAR_FUNDING_HEIGHT));
    let backend = backend(&transport, &["a"]);
    let evidence = backend.form_evidence("a", hash(), FAR_FUNDING_HEIGHT, TIP, &LOG).await;
    assert_eq!(evidence.len(), 2);
    let address = form_address(IndexEncoding::PubkeyHash);
    let failure = evidence[0].history.as_ref().unwrap().failure.clone().unwrap();
    assert!(
        failure.starts_with(&format!(
            "lightwalletd https://a: PubkeyHash form, history of {address} stopped: could not serve 5 heights of window "
        )),
        "{failure}"
    );
    assert!(
        failure.contains(&format!(
            "(last error: {GENERIC_REFUSAL_TEXT}); the failure is not confined to single transactions"
        )),
        "{failure}"
    );
    assert!(evidence[0].utxos.as_ref().unwrap_err().contains(GENERIC_REFUSAL_TEXT));
}

/// An endpoint stopped under the limits, with no other endpoint, is undetermined and never "not spent".
#[tokio::test]
async fn a_stopped_endpoint_alone_leaves_the_detection_undetermined() {
    let transport = Arc::new(Scripted::default());
    transport.serve("a", endpoint_refusing_every_read(FAR_FUNDING_HEIGHT));
    let backend = backend(&transport, &["a"]);
    let error = backend.detect_output_spend(&output(), 0, &LOG).await.unwrap_err();
    match error {
        HtlcIndexError::Incomplete(message) => {
            assert!(message.contains("could not serve 5 heights of window"), "{message}");
            assert!(message.contains(GENERIC_REFUSAL_TEXT), "{message}");
            // The skipped heights are artifacts of the refusal: no pointer at the transaction index.
            assert!(!message.contains("transaction index"), "{message}");
            assert!(!message.contains("skipped height"), "{message}");
        },
        other => panic!("{other:?}"),
    }
    let error = backend.detect_funding(&htlc_script(), 0, &LOG).await.unwrap_err();
    assert!(
        matches!(error, HtlcIndexError::Incomplete(_) | HtlcIndexError::NoAddressIndex(_)),
        "{error:?}"
    );
}

/// An endpoint that takes longer than its budget counts as failed, naming the endpoint, the address
/// form and the read it was in; the next endpoint decides.
#[tokio::test]
async fn an_endpoint_over_its_time_budget_counts_as_failed() {
    let transport = Arc::new(Scripted::default());
    let mut slow = clean_endpoint_with_spend(FUNDING_HEIGHT);
    slow.history_delay = Some(Duration::from_secs(600));
    transport.serve("slow", slow);
    transport.serve("fast", clean_endpoint_with_spend(FUNDING_HEIGHT));

    // The budget is shortened for the test; the constant is 120 s (R39.11.4h(c)).
    assert_eq!(HTLC_DETECTION_ENDPOINT_BUDGET, Duration::from_secs(120));
    let budget = Duration::from_millis(250);
    let alone = backend(&transport, &["slow"]).with_endpoint_budget(budget);
    let started = tokio::time::Instant::now();
    let error = alone
        .detect_output_spend(&output(), FUNDING_HEIGHT, &LOG)
        .await
        .unwrap_err();
    let elapsed = started.elapsed();
    assert!(elapsed >= budget && elapsed < Duration::from_secs(20), "{elapsed:?}");
    match error {
        HtlcIndexError::Incomplete(message) => {
            assert!(
                message.contains("lightwalletd https://slow: PubkeyHash form, history of"),
                "{message}"
            );
            assert!(message.contains("time budget"), "{message}");
            // The second form never gets a request once the budget is used up, and says so.
            assert!(
                message.contains("lightwalletd https://slow: ScriptHash form"),
                "{message}"
            );
        },
        other => panic!("{other:?}"),
    }

    let both = backend(&transport, &["slow", "fast"]).with_endpoint_budget(budget);
    let outcome = both.detect_output_spend(&output(), FUNDING_HEIGHT, &LOG).await.unwrap();
    assert!(
        matches!(outcome, SpendOutcome::Spent(ref s) if s.server == "https://fast"),
        "{outcome:?}"
    );
}

/// A funding far above the caller's start, and one far below it, are read from their own height.
#[tokio::test]
async fn funding_far_above_search_start_is_read_from_its_height() {
    let transport = Arc::new(Scripted::default());
    transport.serve("a", clean_endpoint_with_spend(FUNDING_HEIGHT));
    let backend = backend(&transport, &["a"]);
    // The caller's start is the Sapling floor, millions of blocks below the funding.
    let outcome = backend.detect_output_spend(&output(), 1, &LOG).await.unwrap();
    assert!(matches!(outcome, SpendOutcome::Spent(_)));
    assert_eq!(
        transport.history_ranges.lock().first().map(|r| r.0),
        Some(FUNDING_HEIGHT)
    );
}

#[tokio::test]
async fn funding_far_below_search_start_is_read_from_its_height() {
    let transport = Arc::new(Scripted::default());
    transport.serve("a", clean_endpoint_with_spend(FUNDING_HEIGHT));
    let backend = backend(&transport, &["a"]);
    // The caller's start is above the funding and above its spend.
    let outcome = backend.detect_output_spend(&output(), TIP - 5, &LOG).await.unwrap();
    assert!(matches!(outcome, SpendOutcome::Spent(_)));
    assert_eq!(
        transport.history_ranges.lock().first().map(|r| r.0),
        Some(FUNDING_HEIGHT)
    );
}

// ---------------------------------------------------------------------------------------------
// T39.11.4i -- no address index
// ---------------------------------------------------------------------------------------------

fn history_and_utxo_requests(transport: &Scripted, server: &str) -> usize {
    transport.asked(server, "history").len() + transport.asked(server, "utxos").len()
}

#[tokio::test]
async fn an_endpoint_without_an_address_index_is_marked_and_the_next_endpoint_decides() {
    let transport = Arc::new(Scripted::default());
    transport.serve("noindex", endpoint_without_address_index(FUNDING_HEIGHT));
    transport.serve("good", clean_endpoint_with_spend(FUNDING_HEIGHT));
    let backend = backend(&transport, &["noindex", "good"]);
    assert!(backend.no_address_index_message("noindex").is_none());

    let outcome = backend
        .detect_output_spend(&output(), FUNDING_HEIGHT, &LOG)
        .await
        .unwrap();
    assert!(
        matches!(outcome, SpendOutcome::Spent(ref s) if s.server == "https://good"),
        "{outcome:?}"
    );
    let node_said = backend.no_address_index_message("noindex").expect("marked");
    assert!(
        node_said.contains("No information available for address"),
        "{node_said}"
    );
    assert!(backend.no_address_index_message("good").is_none());
}

/// The match is case-insensitive and does not depend on the status class.
#[test]
fn the_node_message_matches_case_insensitively() {
    assert!(mentions_no_address_index(
        "Unknown error: -5: No information available for address"
    ));
    assert!(mentions_no_address_index("-5: NO INFORMATION AVAILABLE FOR ADDRESS"));
    assert!(!mentions_no_address_index("no such mempool or blockchain transaction"));
}

#[tokio::test]
async fn a_marked_endpoint_is_sent_no_request_by_later_detections_and_listed_as_skipped() {
    let transport = Arc::new(Scripted::default());
    transport.serve("noindex", endpoint_without_address_index(FUNDING_HEIGHT));
    // A second endpoint that cannot decide either, so the error text lists both.
    transport.serve("broken", Endpoint::default());
    let backend = backend(&transport, &["noindex", "broken"]);
    let _ = backend.detect_output_spend(&output(), FUNDING_HEIGHT, &LOG).await;
    let sent = history_and_utxo_requests(&transport, "noindex");
    assert!(sent > 0);

    let error = backend
        .detect_output_spend(&output(), FUNDING_HEIGHT, &LOG)
        .await
        .unwrap_err();
    match error {
        HtlcIndexError::Incomplete(message) => {
            assert!(message.contains("noindex"), "{message}");
            assert!(message.contains("skipped: no address index"), "{message}");
        },
        other => panic!("{other:?}"),
    }
    let error = backend
        .detect_funding(&htlc_script(), FUNDING_HEIGHT, &LOG)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("skipped: no address index"), "{error}");
    assert_eq!(
        history_and_utxo_requests(&transport, "noindex"),
        sent,
        "no further request"
    );
}

#[tokio::test]
async fn with_every_admitted_endpoint_marked_a_detection_fails_at_once_with_the_named_error() {
    let transport = Arc::new(Scripted::default());
    transport.serve("a", endpoint_without_address_index(FUNDING_HEIGHT));
    transport.serve("b", endpoint_without_address_index(FUNDING_HEIGHT));
    let backend = backend(&transport, &["a", "b"]);
    let first = backend
        .detect_output_spend(&output(), FUNDING_HEIGHT, &LOG)
        .await
        .unwrap_err();
    assert!(matches!(first, HtlcIndexError::Incomplete(_)), "{first:?}");
    let sent = (
        history_and_utxo_requests(&transport, "a"),
        history_and_utxo_requests(&transport, "b"),
    );

    for error in [
        backend
            .detect_output_spend(&output(), FUNDING_HEIGHT, &LOG)
            .await
            .unwrap_err(),
        backend
            .detect_funding(&htlc_script(), FUNDING_HEIGHT, &LOG)
            .await
            .unwrap_err(),
    ] {
        match &error {
            HtlcIndexError::NoAddressIndex(servers) => {
                assert!(servers.contains('a') && servers.contains('b'), "{servers}");
                assert!(error.to_string().contains("keeps an address index"), "{error}");
                assert!(error.is_named_condition());
            },
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(
        (
            history_and_utxo_requests(&transport, "a"),
            history_and_utxo_requests(&transport, "b")
        ),
        sent,
        "no request after the endpoints were marked"
    );
}

#[tokio::test]
async fn a_backend_built_anew_starts_with_no_endpoint_marked() {
    let transport = Arc::new(Scripted::default());
    transport.serve("a", endpoint_without_address_index(FUNDING_HEIGHT));
    let first = backend(&transport, &["a"]);
    let _ = first.detect_output_spend(&output(), FUNDING_HEIGHT, &LOG).await;
    assert!(first.no_address_index_message("a").is_some());
    let sent = history_and_utxo_requests(&transport, "a");

    let second = backend(&transport, &["a"]);
    assert!(second.no_address_index_message("a").is_none());
    let _ = second.detect_output_spend(&output(), FUNDING_HEIGHT, &LOG).await;
    assert!(
        history_and_utxo_requests(&transport, "a") > sent,
        "the new activation asked again"
    );
}

#[tokio::test]
async fn a_marked_endpoint_still_serves_the_tip_and_transaction_lookups_and_re_admission_keeps_the_mark() {
    let transport = Arc::new(Scripted::default());
    transport.serve("a", endpoint_without_address_index(FUNDING_HEIGHT));
    let backend = backend(&transport, &["a"]);
    let _ = backend.detect_output_spend(&output(), FUNDING_HEIGHT, &LOG).await;
    assert!(backend.no_address_index_message("a").is_some());

    // Binding the wallet drops the cached admission, so the next call evaluates it again.
    backend.bind_wallet(Arc::new(Bound));
    assert_eq!(backend.admitted().await.unwrap(), vec!["a".to_owned()]);
    assert!(
        backend.no_address_index_message("a").is_some(),
        "admission does not clear the mark"
    );

    assert_eq!(backend.chain_tip().await.unwrap(), TIP);
    let tx = backend.transaction(output().txid).await.unwrap();
    assert_eq!(tx.height, Some(FUNDING_HEIGHT));
}

/// R39.11.4i(e): a fresh swap's already-sent check reads one block. The refusal ends each read at its
/// first request, records no gap, and is explained as a missing address index, never a transaction index.
#[tokio::test]
async fn a_one_block_read_refused_for_no_address_index_costs_one_request_and_no_gap() {
    let transport = Arc::new(Scripted::default());
    transport.serve("a", endpoint_without_address_index(TIP));
    let backend = backend(&transport, &["a"]);

    let evidence = backend.form_evidence("a", hash(), TIP, TIP, &LOG).await;
    assert_eq!(evidence.len(), 2);
    for form in &evidence {
        let read = form.history.as_ref().expect("the read itself returned");
        assert!(read.gaps.is_empty(), "{read:?}");
        assert_eq!(read.requests, 1);
        assert!(!read.stopped_on_unservable_heights);
        let failure = read.failure.as_deref().expect("the read's failure is set");
        assert!(failure.contains("keeps no address index"), "{failure}");
        assert!(failure.contains(NO_INDEX_NODE_MESSAGE), "{failure}");
        assert!(!failure.contains("transaction index"), "{failure}");
        let utxos = form.utxos.as_ref().unwrap_err();
        assert!(utxos.contains("keeps no address index"), "{utxos}");
        assert!(utxos.contains(NO_INDEX_NODE_MESSAGE), "{utxos}");
        assert!(!utxos.contains("transaction index"), "{utxos}");
    }
    assert_eq!(transport.asked("a", "history").len(), 2, "one history request per form");
    assert_eq!(transport.asked("a", "utxos").len(), 2, "one UTXO request per form");

    // The same through the detection: its error names the missing address index.
    let transport = Arc::new(Scripted::default());
    transport.serve("a", endpoint_without_address_index(TIP));
    let backend = self::backend(&transport, &["a"]);
    let error = backend.detect_funding(&htlc_script(), TIP, &LOG).await.unwrap_err();
    let text = error.to_string();
    assert!(matches!(error, HtlcIndexError::Incomplete(_)), "{text}");
    assert!(text.contains("keeps no address index"), "{text}");
    assert!(text.contains(NO_INDEX_NODE_MESSAGE), "{text}");
    assert!(!text.contains("transaction index"), "{text}");
    assert!(!text.contains("skipped height"), "{text}");
    assert_eq!(transport.asked("a", "history").len(), 2);
    assert_eq!(transport.asked("a", "utxos").len(), 2);
}

#[tokio::test]
async fn a_no_address_index_refusal_ends_a_long_read_after_its_first_request() {
    let transport = Arc::new(Scripted::default());
    transport.serve("a", endpoint_without_address_index(FAR_FUNDING_HEIGHT));
    let backend = backend(&transport, &["a"]);
    let evidence = backend.form_evidence("a", hash(), FAR_FUNDING_HEIGHT, TIP, &LOG).await;
    for form in &evidence {
        let read = form.history.as_ref().expect("the read itself returned");
        assert_eq!(read.requests, 1);
        assert!(read.gaps.is_empty(), "{read:?}");
        assert!(read
            .failure
            .as_deref()
            .unwrap_or_default()
            .contains("keeps no address index"));
    }
    assert_eq!(
        transport.asked("a", "history").len(),
        2,
        "one history request per address form over 30,000 blocks"
    );
}

/// A refusal without the node's message stays a serve failure: bisected, its gaps recorded, and the
/// transaction-index explanation kept.
#[tokio::test]
async fn a_genuine_serve_failure_is_still_bisected_and_explained_as_a_transaction_index_gap() {
    let transport = Arc::new(Scripted::default());
    let mut endpoint = healthy_endpoint();
    endpoint.funding_lookup = Some(Ok(raw(funding_bytes(), FUNDING_HEIGHT)));
    endpoint.unservable = vec![FUNDING_HEIGHT + 10];
    transport.serve("a", endpoint);
    let backend = backend(&transport, &["a"]);

    let evidence = backend
        .form_evidence("a", hash(), FUNDING_HEIGHT, FUNDING_HEIGHT + 100, &LOG)
        .await;
    let read = evidence[0].history.as_ref().expect("the read itself returned");
    assert_eq!(read.gaps, vec![FUNDING_HEIGHT + 10]);
    assert!(read.requests > 1, "the window was bisected");
    assert!(!read.stopped_on_unservable_heights);

    let error = backend
        .detect_funding(&htlc_script(), FUNDING_HEIGHT, &LOG)
        .await
        .unwrap_err();
    let text = error.to_string();
    assert!(text.contains("history skipped height(s)"), "{text}");
    assert!(text.contains("transaction index"), "{text}");
    assert!(!text.contains("keeps no address index"), "{text}");
}

// ---------------------------------------------------------------------------------------------
// R39.5.5 -- what a detection read last, for the recovery bound's error
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_dropped_detection_leaves_the_endpoint_and_form_it_read_last_and_a_finished_one_leaves_nothing() {
    let transport = Arc::new(Scripted::default());
    let mut slow = clean_endpoint_with_spend(FUNDING_HEIGHT);
    slow.history_delay = Some(Duration::from_secs(600));
    transport.serve("slow", slow);
    let backend = backend(&transport, &["slow"]);
    // The caller's own bound fires long before the endpoint's budget would.
    let bounded = tokio::time::timeout(
        Duration::from_millis(250),
        backend.detect_output_spend(&output(), FUNDING_HEIGHT, &LOG),
    )
    .await;
    assert!(bounded.is_err(), "the detection was still running");
    let progress = backend.detection_progress(&hash()).expect("the last read is kept");
    assert!(
        progress.starts_with("https://slow, PubkeyHash form, history of "),
        "{progress}"
    );

    let transport = Arc::new(Scripted::default());
    transport.serve("a", clean_endpoint_with_spend(FUNDING_HEIGHT));
    let backend = self::backend(&transport, &["a"]);
    backend
        .detect_output_spend(&output(), FUNDING_HEIGHT, &LOG)
        .await
        .unwrap();
    assert_eq!(backend.detection_progress(&hash()), None);
}
