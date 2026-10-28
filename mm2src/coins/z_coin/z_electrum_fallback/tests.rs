//! T39.11.p -- the Electrum fallback of three swap-path questions (CRD ch.39 R39.11.25), driven from a
//! scripted lightwalletd transport and a scripted Electrum reader: no network. The reads over the real
//! shared client are checked against a loopback stub server.

use super::*;
use crate::utxo::rpc_clients::electrum_stub_server::{StubBehavior, StubServer};
use crate::utxo::utxo_common::extract_secret_from_script_sigs;
use crate::z_coin::z_coin_wallet_db::tests::test_params_with_zip212;
use crate::z_coin::z_lightwalletd::tests::{info, test_addressing, FakeTransport, Script};
use crate::z_coin::z_lightwalletd::{EndpointError, LightwalletdClient};
use crate::z_coin::z_tx_read_fixtures::V6_HEIGHT_68370_HEX;
use kdf_crypto::dhash160;
use mm2_core::mm_ctx::MmCtxBuilder;
use script::Script as UtxoScript;
use serde_json::json;
use std::collections::HashMap;
use zcash_protocol::consensus::BranchId;

const TIP: u64 = 4_167_700;
/// Admission needs two endpoints that agree on the branch.
const LWD_SERVERS: [&str; 2] = ["lwd1", "lwd2"];
const REDEEM: [u8; 110] = [0x5a; 110];
const SECRET: [u8; 32] = [7; 32];
const LOG: DetectionLog<'static> = DetectionLog {
    coin: "ARRR",
    polling: false,
};

// ---------------------------------------------------------------------------------------------
// A scripted Electrum reader
// ---------------------------------------------------------------------------------------------

/// What one scripted Electrum server answers. A read that is not scripted fails.
#[derive(Default)]
pub(crate) struct ServerScript {
    pub(crate) tip: Option<Result<u64, String>>,
    pub(crate) header: Option<Result<Vec<u8>, String>>,
    pub(crate) history: Option<Result<Vec<HistoryItem>, String>>,
    pub(crate) unspent: Option<Result<Vec<UnspentItem>, String>>,
    /// Raw transactions by display-order identifier in hex.
    pub(crate) transactions: HashMap<String, Result<Vec<u8>, String>>,
    /// How long a history read takes to be answered.
    pub(crate) history_delay: Option<Duration>,
}

#[derive(Default)]
pub(crate) struct ScriptedElectrum {
    listed: Mutex<Vec<String>>,
    build_error: Mutex<Option<String>>,
    scripts: Mutex<HashMap<String, ServerScript>>,
    /// Every read made, as `(server, method)`; a header read is `header@<height>`.
    calls: Mutex<Vec<(String, String)>>,
}

impl fmt::Debug for ScriptedElectrum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("ScriptedElectrum") }
}

impl ScriptedElectrum {
    /// No server listed: the fallback is not configured.
    pub(crate) fn none() -> Arc<ScriptedElectrum> { Arc::new(ScriptedElectrum::default()) }

    /// Servers that pass the vetting for a chain at `tip` whose header at `tip - IDENTITY_DEPTH` is `header`.
    pub(crate) fn healthy(servers: &[&str], tip: u64, header: &[u8]) -> Arc<ScriptedElectrum> {
        let electrum = ScriptedElectrum::default();
        *electrum.listed.lock() = servers.iter().map(|s| (*s).to_owned()).collect();
        for server in servers {
            electrum.scripts.lock().insert((*server).to_owned(), ServerScript {
                tip: Some(Ok(tip)),
                header: Some(Ok(header.to_vec())),
                ..ServerScript::default()
            });
        }
        Arc::new(electrum)
    }

    pub(crate) fn edit(&self, server: &str, change: impl FnOnce(&mut ServerScript)) {
        change(self.scripts.lock().entry(server.to_owned()).or_default());
    }

    pub(crate) fn serve_transaction(&self, server: &str, raw: &[u8]) {
        let key = hex::encode(txid_display(&read_tx(raw).expect("a readable transaction")).0);
        self.edit(server, |s| {
            s.transactions.insert(key, Ok(raw.to_vec()));
        });
    }

    pub(crate) fn fail_build(&self, message: &str) { *self.build_error.lock() = Some(message.to_owned()); }

    pub(crate) fn calls(&self) -> Vec<(String, String)> { self.calls.lock().clone() }

    pub(crate) fn asked(&self, server: &str, method: &str) -> usize {
        self.calls
            .lock()
            .iter()
            .filter(|(s, m)| s == server && m.starts_with(method))
            .count()
    }

    fn answer<T: Clone>(
        &self,
        server: &str,
        method: String,
        pick: impl FnOnce(&ServerScript) -> Option<Result<T, String>>,
    ) -> Result<T, String> {
        self.calls.lock().push((server.to_owned(), method));
        let scripts = self.scripts.lock();
        scripts
            .get(server)
            .and_then(pick)
            .unwrap_or_else(|| Err("not scripted".to_owned()))
    }
}

#[async_trait]
impl ElectrumReads for ScriptedElectrum {
    async fn servers(&self) -> Result<Vec<String>, String> {
        match self.build_error.lock().clone() {
            Some(error) => Err(error),
            None => Ok(self.listed.lock().clone()),
        }
    }

    async fn tip_height(&self, server: &str) -> Result<u64, String> {
        self.answer(server, "tip".to_owned(), |s| s.tip.clone())
    }

    async fn block_header(&self, server: &str, height: u64) -> Result<Vec<u8>, String> {
        self.answer(server, format!("header@{}", height), |s| s.header.clone())
    }

    async fn history(&self, server: &str, _script_hash: &str) -> Result<Vec<HistoryItem>, String> {
        let delay = self.scripts.lock().get(server).and_then(|s| s.history_delay);
        let answer = self.answer(server, "history".to_owned(), |s| s.history.clone());
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        answer
    }

    async fn unspent(&self, server: &str, _script_hash: &str) -> Result<Vec<UnspentItem>, String> {
        self.answer(server, "unspent".to_owned(), |s| s.unspent.clone())
    }

    async fn transaction(&self, server: &str, txid: &H256Json) -> Result<Vec<u8>, String> {
        let key = hex::encode(txid.0);
        self.answer(server, "transaction".to_owned(), |s| s.transactions.get(&key).cloned())
    }
}

// ---------------------------------------------------------------------------------------------
// Transactions
// ---------------------------------------------------------------------------------------------

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

fn p2sh_script(redeem: &[u8]) -> Vec<u8> {
    let mut s = vec![0xa9, 0x14];
    s.extend_from_slice(dhash160(redeem).as_slice());
    s.push(0x87);
    s
}

fn htlc_script() -> Vec<u8> { p2sh_script(&REDEEM) }

/// The funding shape the chain admits: no inputs, the P2SH output first, the reveal second.
fn funding_bytes() -> Vec<u8> {
    let mut reveal = vec![0x6a, REDEEM.len() as u8];
    reveal.extend_from_slice(&REDEEM);
    transparent_tx(&[], &[(100_000, htlc_script()), (0, reveal)])
}

/// A redeem of the HTLC: `<sig> <secret> OP_0 <redeem>`, behind an unrelated input.
fn spend_bytes(funding_txid: [u8; 32], vout: u32) -> Vec<u8> {
    let mut sig = vec![0x01, 0xaa, 0x20];
    sig.extend_from_slice(&SECRET);
    sig.push(0x00);
    sig.push(REDEEM.len() as u8);
    sig.extend_from_slice(&REDEEM);
    transparent_tx(&[([0x33; 32], 1, vec![0x51]), (funding_txid, vout, sig)], &[])
}

fn tx_of(bytes: &[u8]) -> ZTransaction { read_tx(bytes).expect("a readable transaction") }

fn output() -> OutputRef {
    OutputRef {
        txid: *tx_of(&funding_bytes()).txid().as_ref(),
        vout: 0,
        script_pubkey: htlc_script(),
    }
}

pub(crate) fn item(bytes: &[u8], height: i64) -> HistoryItem {
    HistoryItem {
        txid: txid_display(&tx_of(bytes)),
        height,
    }
}

fn funding_unspent() -> UnspentItem {
    UnspentItem {
        txid: txid_internal_to_display(output().txid),
        vout: 0,
    }
}

// ---------------------------------------------------------------------------------------------
// The rig
// ---------------------------------------------------------------------------------------------

/// A 1,487-byte header, the size of an Equihash header on the coin this fallback serves.
pub(crate) fn full_header() -> Vec<u8> { (0..1_487u32).map(|i| (i % 251) as u8).collect() }

pub(crate) fn hash_of(header: &[u8]) -> [u8; 32] {
    <[u8; 32]>::try_from(dhash256(header).as_slice()).expect("32 bytes")
}

fn lightwalletd_endpoint(tip: u64, reported_hash: [u8; 32]) -> Script {
    let mut server_info = info(u32::from(BranchId::Canopy), tip);
    server_info.sapling_activation_height = 2;
    let mut script = Script {
        info: Some(Ok(server_info)),
        tip_floor: Some(tip),
        ..Script::default()
    };
    script.block_hashes.insert(tip - IDENTITY_DEPTH, reported_hash);
    script
}

/// A lightwalletd client over two healthy scripted endpoints that report `hash` for the identity height.
fn lightwalletd_over(transport: &Arc<FakeTransport>, hash: [u8; 32]) -> LightwalletdClient {
    for server in LWD_SERVERS {
        transport.serve(server, lightwalletd_endpoint(TIP, hash));
    }
    LightwalletdClient::new(
        transport.clone(),
        LWD_SERVERS.iter().map(|s| (*s).to_owned()).collect(),
        &test_params_with_zip212(),
        test_addressing(),
    )
}

struct Rig {
    lwd: LightwalletdClient,
    transport: Arc<FakeTransport>,
    electrum: Arc<ScriptedElectrum>,
    fallback: ElectrumFallback,
}

impl Rig {
    /// Healthy servers `servers`, all on the chain lightwalletd reports.
    fn new(servers: &[&str]) -> Rig {
        let transport = Arc::new(FakeTransport::default());
        let lwd = lightwalletd_over(&transport, hash_of(&full_header()));
        let electrum = ScriptedElectrum::healthy(servers, TIP, &full_header());
        Rig {
            lwd,
            transport,
            fallback: ElectrumFallback::new("ARRR", electrum.clone()),
            electrum,
        }
    }

    fn lwd_calls(&self, method: &'static str) -> usize {
        LWD_SERVERS.iter().map(|s| self.transport.called(s, method)).sum()
    }

    /// Changes what every scripted lightwalletd endpoint answers.
    fn edit_lwd(&self, change: impl Fn(&mut Script)) {
        for script in self.transport.scripts.lock().values_mut() {
            change(script);
        }
    }

    async fn sent(&self) -> Result<SentAnswer, FallbackError> {
        self.fallback
            .already_sent(self.lwd.backend(), &htlc_script(), &LOG)
            .await
    }

    async fn spent(&self) -> Result<SpendAnswer, FallbackError> {
        self.fallback.spend(self.lwd.backend(), &output(), &LOG).await
    }
}

fn conditions(result: Result<impl fmt::Debug, FallbackError>) -> Vec<String> {
    match result {
        Err(FallbackError::Undetermined(conditions)) => conditions,
        other => panic!("expected an undetermined answer, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------------
// R39.11.25(a), (b): vetting
// ---------------------------------------------------------------------------------------------

/// The comparison is the double SHA-256 of the full header, read at the fresh tip minus ten, against the
/// hash lightwalletd reports for that height.
#[tokio::test]
async fn the_identity_check_hashes_the_full_header_at_the_fresh_tip_minus_ten() {
    let rig = Rig::new(&["a"]);
    rig.electrum.edit("a", |s| s.history = Some(Ok(vec![])));

    assert!(matches!(rig.sent().await, Ok(SentAnswer::NotSent)));

    let height = TIP - 10;
    assert_eq!(rig.electrum.asked("a", &format!("header@{}", height)), 1);
    assert_eq!(rig.electrum.asked("a", "header"), 1, "no other height is read");
    assert_eq!(rig.lwd_calls("block"), 1, "lightwalletd's hash of the same height");
}

/// A server whose header hashes to something else is not used, nothing is asked of it, and the reason names
/// both hashes.
#[tokio::test]
async fn a_server_whose_header_hash_differs_is_not_used_and_the_reason_is_named() {
    let rig = Rig::new(&["a"]);
    let mut other = full_header();
    other[700] ^= 1;
    rig.electrum.edit("a", |s| s.header = Some(Ok(other.clone())));
    rig.electrum.edit("a", |s| s.history = Some(Ok(vec![])));

    let reasons = conditions(rig.sent().await);

    assert_eq!(reasons.len(), 1, "{reasons:?}");
    assert!(reasons[0].starts_with("a: "), "{reasons:?}");
    assert!(reasons[0].contains("not on this coin's chain"), "{reasons:?}");
    let shown = |bytes: [u8; 32]| {
        let mut bytes = bytes.to_vec();
        bytes.reverse();
        hex::encode(bytes)
    };
    assert!(reasons[0].contains(&shown(hash_of(&other))), "{reasons:?}");
    assert!(reasons[0].contains(&shown(hash_of(&full_header()))), "{reasons:?}");
    assert_eq!(
        rig.electrum.asked("a", "history"),
        0,
        "a server that fails the check is not asked"
    );
}

/// The hash covers every byte of the header: a hash of only its first bytes does not match.
#[tokio::test]
async fn the_header_hash_covers_the_whole_header_not_a_prefix() {
    let rig = Rig::new(&["a"]);
    rig.edit_lwd(|script| {
        script
            .block_hashes
            .insert(TIP - IDENTITY_DEPTH, hash_of(&full_header()[..140]));
    });

    let reasons = conditions(rig.sent().await);

    assert!(reasons[0].contains("not on this coin's chain"), "{reasons:?}");
}

#[tokio::test]
async fn a_server_that_cannot_serve_the_header_is_not_used() {
    let rig = Rig::new(&["a"]);
    rig.electrum
        .edit("a", |s| s.header = Some(Err("height out of range".to_owned())));

    let reasons = conditions(rig.sent().await);

    assert!(
        reasons[0].contains("could not serve the header of block"),
        "{reasons:?}"
    );
    assert!(reasons[0].contains("height out of range"), "{reasons:?}");
    assert_eq!(rig.electrum.asked("a", "history"), 0);
}

/// A tip ten blocks behind is allowed; eleven is not, and the server is named with its lag. Its header is
/// not even read.
#[tokio::test]
async fn a_server_whose_tip_lags_by_more_than_ten_blocks_is_not_used_and_the_reason_is_named() {
    let rig = Rig::new(&["lagging"]);
    rig.electrum.edit("lagging", |s| s.tip = Some(Ok(TIP - 11)));

    let reasons = conditions(rig.sent().await);

    assert!(reasons[0].starts_with("lagging: "), "{reasons:?}");
    assert!(reasons[0].contains("11 blocks below the fresh tip"), "{reasons:?}");
    assert_eq!(rig.electrum.asked("lagging", "history"), 0);
    assert_eq!(rig.electrum.asked("lagging", "header"), 0);

    rig.electrum.edit("lagging", |s| {
        s.tip = Some(Ok(TIP - 10));
        s.history = Some(Ok(vec![]));
    });
    assert!(
        matches!(rig.sent().await, Ok(SentAnswer::NotSent)),
        "a lag of exactly ten blocks is within the bound"
    );
}

/// A server that is not used is skipped for the next one, in the client's order.
#[tokio::test]
async fn a_server_that_fails_the_check_is_skipped_for_the_next_in_order() {
    let rig = Rig::new(&["bad", "good"]);
    rig.electrum
        .edit("bad", |s| s.tip = Some(Err("connection refused".to_owned())));
    rig.electrum.edit("good", |s| s.history = Some(Ok(vec![])));

    assert!(matches!(rig.sent().await, Ok(SentAnswer::NotSent)));

    assert_eq!(rig.electrum.asked("bad", "history"), 0);
    assert_eq!(rig.electrum.asked("good", "history"), 1);
}

/// A server that is never reached is never asked for anything: the first usable server answers.
#[tokio::test]
async fn servers_after_the_answering_one_are_not_touched() {
    let rig = Rig::new(&["first", "second"]);
    rig.electrum.edit("first", |s| s.history = Some(Ok(vec![])));

    assert!(matches!(rig.sent().await, Ok(SentAnswer::NotSent)));

    assert!(rig.electrum.calls().iter().all(|(server, _)| server == "first"));
}

/// A passed identity check is kept per server for the admission re-evaluation interval; the tip is read
/// for every question, and the check is repeated once the interval has passed.
#[tokio::test]
async fn a_passed_identity_check_is_cached_and_repeated_after_the_interval() {
    let rig = Rig::new(&["a"]);
    rig.electrum.edit("a", |s| s.history = Some(Ok(vec![])));

    assert!(matches!(rig.sent().await, Ok(SentAnswer::NotSent)));
    assert!(matches!(rig.sent().await, Ok(SentAnswer::NotSent)));

    assert_eq!(rig.electrum.asked("a", "header"), 1, "the identity is checked once");
    assert_eq!(rig.electrum.asked("a", "tip"), 2, "the lag is read for each question");
    assert_eq!(rig.lwd_calls("block"), 1);

    rig.fallback.age_identity("a", ADMISSION_REEVALUATION_INTERVAL);
    assert!(matches!(rig.sent().await, Ok(SentAnswer::NotSent)));
    assert_eq!(
        rig.electrum.asked("a", "header"),
        2,
        "repeated after the re-evaluation interval"
    );
}

/// A server that changes chain stops being used, though it passed before.
#[tokio::test]
async fn a_server_that_passed_and_then_fails_the_repeated_check_is_not_used() {
    let rig = Rig::new(&["a"]);
    rig.electrum.edit("a", |s| s.history = Some(Ok(vec![])));
    assert!(matches!(rig.sent().await, Ok(SentAnswer::NotSent)));

    rig.fallback.age_identity("a", ADMISSION_REEVALUATION_INTERVAL);
    let mut other = full_header();
    other[0] ^= 0xff;
    rig.electrum.edit("a", |s| s.header = Some(Ok(other)));

    let reasons = conditions(rig.sent().await);
    assert!(reasons[0].contains("not on this coin's chain"), "{reasons:?}");
}

/// Without a way to compare, the server is not trusted: lightwalletd's hash being unavailable is a named
/// condition, never a pass.
#[tokio::test]
async fn an_identity_that_cannot_be_compared_is_not_a_pass() {
    let rig = Rig::new(&["a"]);
    rig.edit_lwd(|script| script.block_hashes.clear());

    let reasons = conditions(rig.sent().await);

    assert!(reasons[0].contains("could not be read to compare with"), "{reasons:?}");
    assert_eq!(rig.electrum.asked("a", "history"), 0);
}

// ---------------------------------------------------------------------------------------------
// R39.11.25(c), (d): the questions
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn already_sent_is_sent_with_a_history_transaction_that_has_the_htlc_output() {
    let rig = Rig::new(&["a"]);
    let funding = funding_bytes();
    rig.electrum
        .edit("a", |s| s.history = Some(Ok(vec![item(&funding, 5)])));
    rig.electrum.serve_transaction("a", &funding);

    match rig.sent().await {
        Ok(SentAnswer::Sent(tx)) => assert_eq!(txid_display(&tx), txid_display(&tx_of(&funding))),
        other => panic!("expected sent, got {other:?}"),
    }
}

/// "Not sent" is a complete, empty script history and nothing less.
#[tokio::test]
async fn already_sent_not_sent_needs_a_complete_empty_history() {
    let rig = Rig::new(&["a"]);
    rig.electrum.edit("a", |s| s.history = Some(Ok(vec![])));
    assert!(matches!(rig.sent().await, Ok(SentAnswer::NotSent)));

    // A history that failed to come is not an empty one.
    rig.electrum
        .edit("a", |s| s.history = Some(Err("history too large".to_owned())));
    let reasons = conditions(rig.sent().await);
    assert!(reasons[0].contains("history too large"), "{reasons:?}");
}

/// A history that names transactions none of which pays exactly the HTLC script is neither "sent" nor "not
/// sent".
#[tokio::test]
async fn already_sent_with_a_history_that_does_not_pay_the_script_is_undetermined() {
    let rig = Rig::new(&["a"]);
    let other_script = transparent_tx(&[], &[(1, p2sh_script(&[1u8; 20]))]);
    rig.electrum
        .edit("a", |s| s.history = Some(Ok(vec![item(&other_script, 5)])));
    rig.electrum.serve_transaction("a", &other_script);

    let reasons = conditions(rig.sent().await);

    assert!(
        reasons[0].contains("none has an output of exactly the HTLC script"),
        "{reasons:?}"
    );
}

/// Every failure of a read leaves the question undetermined, never negative: a transaction that cannot be
/// fetched, bytes that cannot be decoded, and a transaction other than the one asked for.
#[tokio::test]
async fn already_sent_failures_of_the_transaction_read_are_undetermined() {
    let rig = Rig::new(&["a"]);
    let funding = funding_bytes();
    let asked = item(&funding, 5);
    rig.electrum.edit("a", |s| s.history = Some(Ok(vec![asked.clone()])));
    let key = hex::encode(asked.txid.0);

    // Not fetchable.
    rig.electrum.edit("a", |s| {
        s.transactions = HashMap::from([(key.clone(), Err("not found".to_owned()))])
    });
    let reasons = conditions(rig.sent().await);
    assert!(reasons[0].contains("could not be fetched: not found"), "{reasons:?}");

    // Not decodable.
    rig.electrum.edit("a", |s| {
        s.transactions = HashMap::from([(key.clone(), Ok(vec![1, 2, 3]))])
    });
    let reasons = conditions(rig.sent().await);
    assert!(reasons[0].contains("cannot read the transaction"), "{reasons:?}");

    // Another transaction than the one asked for.
    let other = transparent_tx(&[], &[(1, htlc_script()), (2, vec![0x51])]);
    rig.electrum
        .edit("a", |s| s.transactions = HashMap::from([(key.clone(), Ok(other))]));
    let reasons = conditions(rig.sent().await);
    assert!(reasons[0].contains("was answered with transaction"), "{reasons:?}");
}

/// A spend found through Electrum is the transaction, with the input that spends the HTLC outpoint, and the
/// secret can be taken from it.
#[tokio::test]
async fn a_spend_found_through_electrum_yields_the_secret() {
    let rig = Rig::new(&["a"]);
    let out = output();
    let spend = spend_bytes(out.txid, out.vout);
    // The funding transaction is in the history too, and is not fetched: it cannot spend its own output.
    rig.electrum.edit("a", |s| {
        s.history = Some(Ok(vec![item(&funding_bytes(), 5), item(&spend, 9)]));
    });
    rig.electrum.serve_transaction("a", &spend);

    match rig.spent().await {
        Ok(SpendAnswer::Spent { tx, input_index }) => {
            assert_eq!(input_index, 1, "the second input is the HTLC's");
            assert_eq!(txid_display(&tx), txid_display(&tx_of(&spend)));
            let script_sigs: Vec<UtxoScript> = tx
                .transparent_bundle()
                .unwrap()
                .vin
                .iter()
                .map(|input| UtxoScript::from(input.script_sig().0 .0.clone()))
                .collect();
            let secret =
                extract_secret_from_script_sigs(dhash160(&SECRET).as_slice(), script_sigs).expect("the secret");
            assert_eq!(secret, SECRET.to_vec());
        },
        other => panic!("expected a spend, got {other:?}"),
    }
    assert_eq!(
        rig.electrum.asked("a", "transaction"),
        1,
        "only the spender was fetched"
    );
    assert_eq!(
        rig.electrum.asked("a", "unspent"),
        0,
        "a spend needs no unspent listing"
    );
}

/// "Not spent" needs the unspent listing to list the HTLC outpoint; a history without a spender is not
/// enough on its own.
#[tokio::test]
async fn not_spent_needs_the_unspent_listing() {
    let rig = Rig::new(&["a"]);
    rig.electrum
        .edit("a", |s| s.history = Some(Ok(vec![item(&funding_bytes(), 5)])));

    // The listing is not scripted: undetermined.
    let reasons = conditions(rig.spent().await);
    assert!(reasons[0].contains("not scripted"), "{reasons:?}");

    // The listing does not list the outpoint: undetermined, not "not spent".
    rig.electrum.edit("a", |s| s.unspent = Some(Ok(vec![])));
    let reasons = conditions(rig.spent().await);
    assert!(reasons[0].contains("is not in its unspent listing"), "{reasons:?}");

    // It lists another output of the transaction: still not the HTLC outpoint.
    rig.electrum.edit("a", |s| {
        s.unspent = Some(Ok(vec![UnspentItem {
            vout: 1,
            ..funding_unspent()
        }]))
    });
    assert!(conditions(rig.spent().await)[0].contains("is not in its unspent listing"));

    // It lists the outpoint: not spent.
    rig.electrum
        .edit("a", |s| s.unspent = Some(Ok(vec![funding_unspent()])));
    assert!(matches!(rig.spent().await, Ok(SpendAnswer::NotSpent)));

    // A listing that failed to come is not "not spent".
    rig.electrum
        .edit("a", |s| s.unspent = Some(Err("server busy".to_owned())));
    assert!(conditions(rig.spent().await)[0].contains("server busy"));
}

/// A history transaction that cannot be read might be the spender: even a listing that lists the outpoint
/// does not make the answer "not spent".
#[tokio::test]
async fn an_unreadable_history_transaction_leaves_the_spend_question_undetermined() {
    let rig = Rig::new(&["a"]);
    let out = output();
    let spend = spend_bytes(out.txid, out.vout);
    rig.electrum.edit("a", |s| {
        s.history = Some(Ok(vec![item(&funding_bytes(), 5), item(&spend, 9)]));
        s.unspent = Some(Ok(vec![funding_unspent()]));
        s.transactions = HashMap::from([(hex::encode(item(&spend, 9).txid.0), Err("connection reset".to_owned()))]);
    });

    let reasons = conditions(rig.spent().await);

    assert!(reasons[0].contains("connection reset"), "{reasons:?}");
}

/// A transaction that spends another output of the same script is not the spend.
#[tokio::test]
async fn a_history_transaction_spending_another_outpoint_is_not_the_spend() {
    let rig = Rig::new(&["a"]);
    let out = output();
    let elsewhere = spend_bytes(out.txid, 1);
    rig.electrum.edit("a", |s| {
        s.history = Some(Ok(vec![item(&funding_bytes(), 5), item(&elsewhere, 9)]));
        s.unspent = Some(Ok(vec![funding_unspent()]));
    });
    rig.electrum.serve_transaction("a", &elsewhere);

    assert!(matches!(rig.spent().await, Ok(SpendAnswer::NotSpent)));
}

/// A version-6 transaction in a history is read with the Zcash reader and identified by its own rule. The
/// shared UTXO parser cannot read it at all, so were it used the transaction would be unreadable and the
/// answer would be undetermined instead of "not spent".
#[tokio::test]
async fn a_version_6_transaction_is_read_with_the_zcash_reader() {
    let raw = hex::decode(V6_HEIGHT_68370_HEX).unwrap();
    assert!(
        serialization::deserialize::<_, chain::Transaction>(raw.as_slice()).is_err(),
        "the shared parser refuses a version-6 transaction"
    );
    let rig = Rig::new(&["a"]);
    rig.electrum.edit("a", |s| {
        s.history = Some(Ok(vec![item(&funding_bytes(), 5), item(&raw, 9)]));
        s.unspent = Some(Ok(vec![funding_unspent()]));
    });
    rig.electrum.serve_transaction("a", &raw);

    assert!(matches!(rig.spent().await, Ok(SpendAnswer::NotSpent)));
    assert_eq!(
        rig.electrum.asked("a", "transaction"),
        1,
        "the version-6 transaction was read"
    );
}

// ---------------------------------------------------------------------------------------------
// R39.11.25(d): failures
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn no_listed_server_means_no_fallback_and_nothing_is_asked() {
    let transport = Arc::new(FakeTransport::default());
    let lwd = lightwalletd_over(&transport, hash_of(&full_header()));
    let fallback = ElectrumFallback::new("ARRR", ScriptedElectrum::none());

    let answer = fallback.already_sent(lwd.backend(), &htlc_script(), &LOG).await;

    assert!(matches!(answer, Err(FallbackError::NotConfigured)), "{answer:?}");
    assert_eq!(
        LWD_SERVERS.iter().map(|s| transport.called(s, "tip")).sum::<usize>(),
        0,
        "not even the fresh tip is read"
    );
    assert_eq!(
        FallbackError::NotConfigured.appended_to("lightwalletd text".to_owned()),
        "lightwalletd text"
    );
}

#[tokio::test]
async fn a_client_that_cannot_be_built_is_a_named_condition() {
    let rig = Rig::new(&["a"]);
    rig.electrum
        .fail_build("no Electrum server could be connected: a: connection refused");

    let reasons = conditions(rig.spent().await);

    assert_eq!(reasons.len(), 1);
    assert!(reasons[0].contains("no Electrum client"), "{reasons:?}");
    assert!(reasons[0].contains("a: connection refused"), "{reasons:?}");
}

/// When lightwalletd also gave no answer the error carries both sources' conditions.
#[tokio::test]
async fn the_error_names_the_conditions_of_both_sources() {
    let rig = Rig::new(&["a", "b"]);
    rig.electrum
        .edit("a", |s| s.tip = Some(Err("connection refused".to_owned())));
    rig.electrum
        .edit("b", |s| s.history = Some(Err("history too large".to_owned())));

    let fallback = rig.sent().await.expect_err("neither server answers");
    let text = fallback.appended_to("no address index on: lwd".to_owned());

    assert!(
        text.starts_with("no address index on: lwd; the Electrum fallback"),
        "{text}"
    );
    assert!(
        text.contains("a: its tip could not be read: connection refused"),
        "{text}"
    );
    assert!(text.contains("b: history too large"), "{text}");
}

/// The overall budget of a question (CRD ch.38 R38.6.4j) ends a question that a server never answers, and
/// the server not reached is named.
#[tokio::test]
async fn the_time_budget_ends_a_question_and_the_unreached_server_is_named() {
    let mut rig = Rig::new(&["slow", "unreached"]);
    rig.fallback = ElectrumFallback::new("ARRR", rig.electrum.clone()).with_budget(Duration::from_millis(300));
    rig.electrum.edit("slow", |s| {
        s.history = Some(Ok(vec![]));
        s.history_delay = Some(Duration::from_secs(30));
    });

    let reasons = conditions(rig.sent().await);

    assert!(
        reasons
            .iter()
            .any(|r| r.starts_with("slow: ") && r.contains("time budget")),
        "{reasons:?}"
    );
    assert!(
        reasons
            .iter()
            .any(|r| r.contains("used up before unreached could be asked")),
        "{reasons:?}"
    );
    assert_eq!(rig.electrum.asked("unreached", "tip"), 0);
}

/// The R39.5.5 progress record says what the fallback is reading while it runs, and is gone when it ends.
#[tokio::test]
async fn the_progress_record_names_the_electrum_read_in_flight() {
    let rig = Rig::new(&["a"]);
    rig.electrum.edit("a", |s| {
        s.history = Some(Ok(vec![]));
        s.history_delay = Some(Duration::from_millis(600));
    });
    let hash = p2sh_hash_of(&htlc_script()).unwrap();

    let (answer, progress) = tokio::join!(rig.sent(), async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        rig.lwd.backend().detection_progress(&hash)
    });

    assert!(matches!(answer, Ok(SentAnswer::NotSent)));
    assert_eq!(progress.as_deref(), Some("Electrum a, script history"));
    assert_eq!(
        rig.lwd.backend().detection_progress(&hash),
        None,
        "removed when the question ends"
    );
}

/// The capability the taker refusal of R39.11.28 builds on: the first server that passes the vetting and
/// answers one script-history read.
#[tokio::test]
async fn the_first_usable_server_is_the_first_that_passes_the_vetting_and_answers_a_history_read() {
    let rig = Rig::new(&["lagging", "silent", "good"]);
    rig.electrum.edit("lagging", |s| s.tip = Some(Ok(TIP - 50)));
    rig.electrum.edit("good", |s| s.history = Some(Ok(Vec::new())));

    assert_eq!(
        rig.fallback
            .first_usable_server(rig.lwd.backend(), &htlc_script())
            .await
            .unwrap(),
        "good"
    );
    assert_eq!(
        rig.electrum.asked("lagging", "history"),
        0,
        "a server that fails vetting is not asked"
    );
    assert_eq!(
        rig.electrum.asked("silent", "history"),
        1,
        "a vetted server is asked once"
    );

    rig.electrum.edit("good", |s| s.tip = Some(Ok(TIP - 50)));
    let reasons = conditions(
        rig.fallback
            .first_usable_server(rig.lwd.backend(), &htlc_script())
            .await,
    );
    assert_eq!(reasons.len(), 3, "{reasons:?}");
    assert!(reasons[1].starts_with("silent: "), "{reasons:?}");
}

/// The fresh tip is the single height authority's read: when lightwalletd cannot give it, no server is
/// vetted against a guess.
#[tokio::test]
async fn without_a_fresh_tip_no_server_is_vetted() {
    let rig = Rig::new(&["a"]);
    rig.edit_lwd(|script| {
        script.tip_floor = None;
        script.info = Some(Err(EndpointError::Transport("unreachable".to_owned())));
    });

    let reasons = conditions(rig.sent().await);

    assert!(reasons[0].contains("the fresh tip could not be read"), "{reasons:?}");
    assert_eq!(rig.electrum.asked("a", "tip"), 0);
}

// ---------------------------------------------------------------------------------------------
// The reads over the shared Electrum client
// ---------------------------------------------------------------------------------------------

/// Constructing the reads connects nothing; the client is built, with the shared builder, by the first
/// read that needs it, and then speaks to the named server (CRD ch.38 R38.6.4g).
#[tokio::test]
async fn the_shared_client_is_built_on_the_first_read_and_not_before() {
    let ctx = MmCtxBuilder::default().into_mm_arc();
    let stub = StubServer::start(StubBehavior::Answer).await;
    let txid = "11".repeat(32);
    stub.set_result(
        "blockchain.headers.subscribe",
        json!({ "height": 4_167_700, "hex": "00" }),
    );
    stub.set_result("blockchain.block.header", json!("0a0b0c"));
    stub.set_result(
        "blockchain.scripthash.get_history",
        json!([{ "height": 5, "tx_hash": txid }, { "height": 0, "tx_hash": "22".repeat(32) }]),
    );
    stub.set_result(
        "blockchain.scripthash.listunspent",
        json!([{ "height": 5, "tx_hash": txid, "tx_pos": 1, "value": 10 }]),
    );
    stub.set_result("blockchain.transaction.get", json!("010203"));

    let reads = ElectrumClientReads::new(ctx.weak(), "ARRR", vec![stub.request()]);
    assert!(!reads.is_built());
    assert_eq!(stub.accepted(), 0, "constructing the reads opens no connection");

    let servers = reads.servers().await.expect("the stub is reachable");
    assert_eq!(servers, vec![stub.addr()]);
    assert!(reads.is_built());
    assert!(stub.accepted() >= 1);

    let server = stub.addr();
    assert_eq!(reads.tip_height(&server).await.unwrap(), 4_167_700);
    assert_eq!(reads.block_header(&server, 4_167_690).await.unwrap(), vec![
        0x0a, 0x0b, 0x0c
    ]);
    let history = reads.history(&server, "aa").await.unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!((history[0].height, history[1].height), (5, 0));
    assert_eq!(hex::encode(history[0].txid.0), txid);
    let unspent = reads.unspent(&server, "aa").await.unwrap();
    assert_eq!((unspent[0].vout, hex::encode(unspent[0].txid.0)), (1, txid.clone()));
    assert_eq!(reads.transaction(&server, &history[0].txid).await.unwrap(), vec![
        1, 2, 3
    ]);

    // The header was asked for at the height given, and the transaction without the verbose flag.
    assert_eq!(
        stub.last_request("blockchain.block.header").unwrap()["params"][0],
        4_167_690
    );
    assert_eq!(
        stub.last_request("blockchain.transaction.get").unwrap()["params"][1],
        false
    );
    // An address that is not in the pool is refused without any connection attempt.
    assert!(reads
        .tip_height("127.0.0.1:2")
        .await
        .unwrap_err()
        .contains("unknown Electrum server"));
}

/// No server listed: nothing is built, whatever is asked.
#[tokio::test]
async fn an_empty_server_list_builds_no_client() {
    let ctx = MmCtxBuilder::default().into_mm_arc();
    let reads = ElectrumClientReads::new(ctx.weak(), "ARRR", Vec::new());

    assert_eq!(reads.servers().await.unwrap(), Vec::<String>::new());
    assert!(!reads.is_built());
}

/// Unreachable servers fail the first question with each server named, and the failure is not remembered:
/// the activation itself never touched them, and a later question tries again.
#[tokio::test]
async fn unreachable_servers_fail_the_question_not_the_activation() {
    let ctx = MmCtxBuilder::default().into_mm_arc();
    let refused = ElectrumRpcRequest {
        url: "127.0.0.1:1".to_owned(),
        protocol: Default::default(),
        disable_cert_verification: false,
        timeout_sec: None,
    };
    let fallback = ElectrumFallback::for_activation(ctx.weak(), "ARRR", vec![refused]);
    let transport = Arc::new(FakeTransport::default());
    let lwd = lightwalletd_over(&transport, hash_of(&full_header()));

    let reasons = conditions(fallback.already_sent(lwd.backend(), &htlc_script(), &LOG).await);

    assert_eq!(reasons.len(), 1);
    assert!(reasons[0].contains("no Electrum client"), "{reasons:?}");
    assert!(reasons[0].contains("127.0.0.1:1"), "{reasons:?}");
    // A second question is a second attempt, with the same named failure.
    let again = conditions(fallback.already_sent(lwd.backend(), &htlc_script(), &LOG).await);
    assert!(again[0].contains("127.0.0.1:1"), "{again:?}");
}

// ---------------------------------------------------------------------------------------------
// The identity check against a real header
// ---------------------------------------------------------------------------------------------

/// The full header of ARRR block 4,161,185 (Ironwood activation) as an Electrum server serves it
/// (`blockchain.block.header`), 1,487 bytes, and the `hash` bytes lightwalletd's `GetBlock` returns for
/// the same height, not reversed. Public chain data, both captured live on 2026-10-28.
const REAL_HEADER_HEIGHT: u64 = 4_161_185;
const REAL_HEADER_HEX: &str = include_str!("../test-fixtures/arrr-header-4161185.hex");
const REAL_GETBLOCK_HASH_HEX: &str = include_str!("../test-fixtures/arrr-header-4161185-getblock-hash.hex");

/// A rig whose fresh tip is ten blocks above the real header: lightwalletd reports the captured hash at
/// that height, and the one Electrum server serves `header`.
fn real_header_rig(header: Vec<u8>) -> (Rig, [u8; 32]) {
    let tip = REAL_HEADER_HEIGHT + IDENTITY_DEPTH;
    let reported = <[u8; 32]>::try_from(hex::decode(REAL_GETBLOCK_HASH_HEX.trim()).unwrap().as_slice())
        .expect("the captured hash is 32 bytes");
    let transport = Arc::new(FakeTransport::default());
    for server in LWD_SERVERS {
        transport.serve(server, lightwalletd_endpoint(tip, reported));
    }
    let lwd = LightwalletdClient::new(
        transport.clone(),
        LWD_SERVERS.iter().map(|s| (*s).to_owned()).collect(),
        &test_params_with_zip212(),
        test_addressing(),
    );
    let electrum = ScriptedElectrum::healthy(&["a"], tip, &header);
    electrum.edit("a", |s| s.history = Some(Ok(vec![])));
    let rig = Rig {
        lwd,
        transport,
        fallback: ElectrumFallback::new("ARRR", electrum.clone()),
        electrum,
    };
    (rig, reported)
}

/// R39.11.25(a) on live data: the double SHA-256 of the real 1,487-byte header equals the hash the real
/// lightwalletd reported, byte for byte, so the server passes and answers.
#[tokio::test]
async fn a_real_arrr_header_passes_the_identity_check_against_the_real_getblock_hash() {
    let header = hex::decode(REAL_HEADER_HEX.trim()).unwrap();
    assert_eq!(header.len(), 1_487);
    let (rig, reported) = real_header_rig(header.clone());
    assert_eq!(
        hash_of(&header),
        reported,
        "the captured pair is consistent, unreversed"
    );

    assert!(matches!(rig.sent().await, Ok(SentAnswer::NotSent)));

    assert_eq!(rig.electrum.asked("a", &format!("header@{}", REAL_HEADER_HEIGHT)), 1);
    assert_eq!(rig.lwd_calls("block"), 1);
}

/// One flipped byte of the real header fails the check with the named mismatch, and the server is not asked
/// anything else.
#[tokio::test]
async fn a_real_arrr_header_with_one_byte_flipped_fails_the_identity_check() {
    let mut header = hex::decode(REAL_HEADER_HEX.trim()).unwrap();
    header[1_000] ^= 0x01;
    let (rig, reported) = real_header_rig(header.clone());

    let reasons = conditions(rig.sent().await);

    assert!(reasons[0].contains("not on this coin's chain"), "{reasons:?}");
    let shown = |bytes: [u8; 32]| {
        let mut bytes = bytes.to_vec();
        bytes.reverse();
        hex::encode(bytes)
    };
    assert!(reasons[0].contains(&shown(hash_of(&header))), "{reasons:?}");
    assert!(reasons[0].contains(&shown(reported)), "{reasons:?}");
    assert!(
        reasons[0].contains("0000000b4ea249ba4c7c57724e0abdd8abf6fb719ef0806fe6c9ae1ee0534099"),
        "the reported hash is shown in display order: {reasons:?}"
    );
    assert_eq!(rig.electrum.asked("a", "history"), 0);
}

// ---------------------------------------------------------------------------------------------
// T39.11.s -- the taker refusal's capability check (R39.11.28)
// ---------------------------------------------------------------------------------------------

/// The wallet's own transparent address the probes read.
fn own_address() -> Address {
    test_addressing().address(crate::z_coin::z_lightwalletd::IndexEncoding::PubkeyHash, [9; 20])
}

fn no_index() -> EndpointError {
    EndpointError::NoAddressIndex("Unknown: No information available for address".to_owned())
}

impl Rig {
    async fn watch_source(&self) -> Result<String, String> {
        self.fallback
            .ensure_watch_source(self.lwd.backend(), &own_address())
            .await
    }

    /// Changes what one scripted lightwalletd endpoint answers.
    fn edit_endpoint(&self, server: &str, change: impl FnOnce(&mut Script)) {
        change(
            self.transport
                .scripts
                .lock()
                .get_mut(server)
                .expect("a scripted endpoint"),
        );
    }
}

/// An endpoint that answers the address read is the source; neither the second endpoint nor Electrum is
/// asked.
#[tokio::test]
async fn an_endpoint_that_answers_an_address_read_is_the_watch_source() {
    let rig = Rig::new(&["e"]);

    assert_eq!(rig.watch_source().await.unwrap(), "lightwalletd lwd1");

    assert_eq!(rig.transport.called("lwd1", "utxos"), 1);
    assert_eq!(
        rig.transport.called("lwd2", "utxos"),
        0,
        "the first capable endpoint ends the probe"
    );
    assert!(
        rig.electrum.calls().is_empty(),
        "Electrum is not asked when lightwalletd can answer"
    );
}

/// One capable endpoint is enough: the one without the index is marked, and the other answers.
#[tokio::test]
async fn one_capable_endpoint_among_marked_ones_lets_the_swap_proceed() {
    let rig = Rig::new(&[]);
    rig.edit_endpoint("lwd1", |s| s.address_error = Some(no_index()));

    assert_eq!(rig.watch_source().await.unwrap(), "lightwalletd lwd2");

    // The mark is kept: the second probe does not send the marked endpoint a request again.
    assert_eq!(rig.watch_source().await.unwrap(), "lightwalletd lwd2");
    assert_eq!(rig.transport.called("lwd1", "utxos"), 1);
    assert_eq!(rig.transport.called("lwd2", "utxos"), 2);
}

/// Every endpoint marked and no Electrum server listed: refused with the named error, which names the coin,
/// every endpoint with its condition and the Electrum state, and says the taker could not watch.
#[tokio::test]
async fn every_endpoint_marked_and_no_electrum_server_is_refused_with_the_named_error() {
    let rig = Rig::new(&[]);
    rig.edit_lwd(|s| s.address_error = Some(no_index()));

    let error = rig.watch_source().await.unwrap_err();

    assert_eq!(
        error,
        "ARRR: the taker could not watch its payment for the maker's spend, because no source can answer an \
         address read. lightwalletd endpoints: lwd1: no address index (node message: Unknown: No information \
         available for address); lwd2: no address index (node message: Unknown: No information available for \
         address). Electrum servers: no server is listed in the activation's electrum_servers"
    );
    // The marks outlive the probe: a second refusal reads the marks and sends the endpoints nothing.
    let again = rig.watch_source().await.unwrap_err();
    assert!(again.contains("lwd1: no address index, skipped"), "{again}");
    assert_eq!(rig.transport.called("lwd1", "utxos"), 1);
    assert_eq!(rig.transport.called("lwd2", "utxos"), 1);
}

/// An endpoint that fails the read some other way is a named condition and is not marked: it is asked again.
#[tokio::test]
async fn an_endpoint_that_fails_the_read_otherwise_is_named_and_not_marked() {
    let rig = Rig::new(&[]);
    rig.edit_lwd(|s| s.address_error = Some(EndpointError::Transport("connection reset".to_owned())));

    let error = rig.watch_source().await.unwrap_err();
    assert!(
        error.contains("lwd1: the address read failed: connection reset"),
        "{error}"
    );
    assert!(
        error.contains("lwd2: the address read failed: connection reset"),
        "{error}"
    );

    let _ = rig.watch_source().await;
    assert_eq!(
        rig.transport.called("lwd1", "utxos"),
        2,
        "a failure that is not the rejection is no mark"
    );
}

/// With every endpoint marked, one identity-checked Electrum server that answers a script-history read lets
/// the swap proceed.
#[tokio::test]
async fn one_identity_checked_electrum_server_lets_the_swap_proceed() {
    let rig = Rig::new(&["lagging", "good"]);
    rig.edit_lwd(|s| s.address_error = Some(no_index()));
    rig.electrum.edit("lagging", |s| s.tip = Some(Ok(TIP - 50)));
    rig.electrum.edit("good", |s| s.history = Some(Ok(Vec::new())));

    assert_eq!(rig.watch_source().await.unwrap(), "Electrum good");
    assert_eq!(rig.electrum.asked("good", "history"), 1);
}

/// Electrum servers that fail vetting or the history read are each named, next to every endpoint.
#[tokio::test]
async fn electrum_servers_that_cannot_answer_are_each_named_in_the_refusal() {
    let rig = Rig::new(&["lagging", "silent", "foreign"]);
    rig.edit_lwd(|s| s.address_error = Some(no_index()));
    rig.electrum.edit("lagging", |s| s.tip = Some(Ok(TIP - 50)));
    rig.electrum.edit("foreign", |s| s.header = Some(Ok(vec![1, 2, 3])));

    let error = rig.watch_source().await.unwrap_err();

    assert!(
        error.starts_with("ARRR: the taker could not watch its payment for the maker's spend"),
        "{error}"
    );
    assert!(error.contains("lwd1: no address index"), "{error}");
    assert!(error.contains("lwd2: no address index"), "{error}");
    assert!(error.contains("lagging: its tip"), "{error}");
    assert!(error.contains("silent: not scripted"), "{error}");
    assert!(error.contains("foreign: it is not on this coin's chain"), "{error}");
}
