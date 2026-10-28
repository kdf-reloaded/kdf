//! In-flight note exclusion on the Light-mode send path (CRD ch.39 §39.8.0.7, T39.8.0h / D39.8.0f).
//!
//! The harness drives `ZCoin::send_outputs` and `ZCoin::gen_tx` of a real Light-mode coin over a real
//! `zcash_client_sqlite` wallet database in a temporary directory. The backend is the scripted
//! lightwalletd transport of `z_lightwalletd::tests`: it admits its endpoints, reports a chain tip the
//! test controls, accepts or rejects a broadcast as scripted, and keeps the bytes it was handed. A
//! "scan" is a compact block built from a real transaction and fed to the real scanner; a "restart" is a
//! new coin over the same wallet database directory with a fresh in-memory in-flight map.
//!
//! The tests make real Sapling spend proofs and need the Sapling parameter files; where they are absent
//! a test says so and returns (the same arrangement as the withdraw-side tests in `z_coin_wallet_db`).

use super::z_coin_errors::{GenTxError, SendOutputsErr};
use super::z_coin_wallet_db::tests::{compact_block_with_received_notes, empty_compact_block, empty_sapling_tree_bytes,
                                     test_db_dir, test_params_with_zip212};
use super::z_coin_wallet_db::ZCoinTxHistoryPage;
use super::z_lightwalletd::tests::{info, test_addressing, FakeTransport, Script as WireScript};
use super::z_lightwalletd::{EndpointError, FileBranchStore, LightwalletdClient, RawTx};
use super::*;
use crate::utxo::utxo_tests::utxo_coin_fields_for_test;
use crate::utxo::{TxFee, UtxoArc};
use sapling::value::NoteValue;
use sapling::Rseed;
use std::collections::BTreeSet;
use std::time::{Duration, Instant};
use zcash_client_backend::proto::compact_formats as zcash_compact;

const NOTE_VALUE: u64 = 125_000_000;
const FEE_SAT: u64 = 10_000;
const SERVERS: [&str; 2] = ["a", "b"];
const WALLET_BIRTHDAY: u32 = 10;
/// Height of the block that pays the wallet its notes.
const NOTES_HEIGHT: u32 = 11;

/// The pacing the tests give a coin: short enough that the bounded in-flight wait ends in a second.
fn test_pacing(in_flight_wait_secs: u64) -> SendPacing {
    SendPacing {
        in_flight_wait_secs,
        in_flight_retry_secs: 0.05,
        confirmation_wait_secs: 60,
        confirmation_check_secs: 1,
    }
}

fn sapling_params_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("ARRR_SAPLING_PARAMS")
        .map(PathBuf::from)
        .unwrap_or_else(crate::utxo::zcash_params_path);
    if dir.join(SAPLING_SPEND_NAME).exists() && dir.join(SAPLING_OUTPUT_NAME).exists() {
        return Some(dir);
    }
    eprintln!(
        "SKIPPED: Sapling parameters not found in {} (set ARRR_SAPLING_PARAMS); a spend proof can be \
         neither made nor checked without them",
        dir.display()
    );
    None
}

/// What each scripted endpoint answers: healthy on the Canopy branch, the chain at `tip`, and a
/// broadcast that is accepted and then mines the transaction at height 12.
fn endpoint_script(tip: u64) -> WireScript {
    let mut server_info = info(u32::from(BranchId::Canopy), u64::from(NOTES_HEIGHT));
    // The test parameters' Sapling activation height, which admission holds every endpoint to.
    server_info.sapling_activation_height = 2;
    WireScript {
        info: Some(Ok(server_info)),
        tip_floor: Some(tip),
        tip_after_send: Some(tip.max(12)),
        send: Some(Ok(())),
        // The height only has to be a mined one the tip has reached once the transaction is broadcast.
        transaction: Some(Ok(RawTx {
            data: Vec::new(),
            height: Some(12),
        })),
        ..WireScript::default()
    }
}

/// A Light-mode shielded coin over a real wallet database and a scripted backend.
struct LightCoinFixture {
    coin: ZCoin,
    transport: Arc<FakeTransport>,
    db_dir: PathBuf,
    params_dir: PathBuf,
    params: ZcoinConsensusParams,
    extsk: ExtendedSpendingKey,
    to_addr: PaymentAddress,
    in_flight_wait_secs: u64,
    /// The chain tip the scripted endpoints report.
    tip: u64,
}

impl LightCoinFixture {
    /// A wallet holding one note of 1.25 ARRR, scanned at height 11.
    fn new() -> Option<LightCoinFixture> { Self::with_notes(1, 2) }

    /// A wallet holding `notes` notes of 1.25 ARRR each, scanned at height 11, whose sends give up
    /// waiting for an in-flight spend after `in_flight_wait_secs`.
    fn with_notes(notes: usize, in_flight_wait_secs: u64) -> Option<LightCoinFixture> {
        let params_dir = sapling_params_dir()?;
        let params = test_params_with_zip212();
        let extsk = ExtendedSpendingKey::master(&[15; 32]);
        #[allow(deprecated)]
        let extfvk = extsk.to_extended_full_viewing_key();
        let db_dir = test_db_dir("note-lock");

        let history = Self::open_history(&db_dir, &params, &extfvk);
        let received: Vec<(u64, u8, u8)> = (0..notes).map(|i| (NOTE_VALUE, 68 + i as u8, 42 + i as u8)).collect();
        history
            .insert_compact_blocks_for_test(&[compact_block_with_received_notes(
                u64::from(NOTES_HEIGHT),
                NOTES_HEIGHT as u8,
                9,
                &extfvk,
                &received,
            )])
            .unwrap();
        assert_eq!(
            history
                .scan_cached_blocks_to_height(params.clone(), u64::from(NOTES_HEIGHT), 1_000, 0, |_, _| {})
                .unwrap(),
            u64::from(NOTES_HEIGHT)
        );
        assert_eq!(history.balance(params.clone()).unwrap(), NOTE_VALUE * notes as u64);

        Some(Self::assemble(
            db_dir,
            params_dir,
            params,
            extsk,
            history,
            in_flight_wait_secs,
            u64::from(NOTES_HEIGHT),
        ))
    }

    fn open_history(
        db_dir: &Path,
        params: &ZcoinConsensusParams,
        extfvk: &ExtendedFullViewingKey,
    ) -> ZCoinShieldedHistory {
        let checkpoint = CheckPointBlockInfo {
            height: WALLET_BIRTHDAY,
            hash: rpc::v1::types::H256([9u8; 32]),
            time: 10,
            sapling_tree: empty_sapling_tree_bytes().into(),
        };
        ZCoinShieldedHistory::open_or_create("ARRR", db_dir.to_path_buf(), params.clone(), extfvk, Some(&checkpoint))
            .unwrap()
    }

    /// Builds a coin the way the activation builder does, except that no background loop runs and the
    /// backend is the scripted transport.
    fn assemble(
        db_dir: PathBuf,
        params_dir: PathBuf,
        params: ZcoinConsensusParams,
        extsk: ExtendedSpendingKey,
        history: ZCoinShieldedHistory,
        in_flight_wait_secs: u64,
        tip: u64,
    ) -> LightCoinFixture {
        let history = Arc::new(history);
        let scanned_through = history.scanned_height().unwrap().unwrap_or(0);

        let transport = Arc::new(FakeTransport::default());
        for server in SERVERS {
            transport.serve(server, endpoint_script(tip));
        }
        let client = LightwalletdClient::new(
            transport.clone(),
            SERVERS.iter().map(|s| (*s).to_owned()).collect(),
            &params,
            test_addressing(),
        );
        client
            .backend()
            .bind_wallet(Arc::new(FileBranchStore::new((*history).clone())));

        let mut utxo = utxo_coin_fields_for_test(UtxoRpcClientEnum::LightWalletD(client), None, false);
        utxo.conf.ticker = "ARRR".to_owned();
        utxo.decimals = 8;
        // 0.0001 per kilobyte, as the other wallet-database fixtures pay.
        utxo.tx_fee = TxFee::FixedPerKb(FEE_SAT);

        let (_, my_z_addr) = extsk.default_address();
        let my_z_addr_encoded = encode_payment_address(params.hrp_sapling_payment_address(), &my_z_addr);
        let sapling_cache: Arc<dyn SaplingStateCacheOps + Send + Sync> = Arc::new(
            open_or_create_native_sapling_cache(native_sapling_cache_path("ARRR", db_dir.clone()))
                .unwrap_or_else(|e| panic!("sapling cache: {e}")),
        );
        let z_tx_prover = LocalTxProver::new(
            &params_dir.join(SAPLING_SPEND_NAME),
            &params_dir.join(SAPLING_OUTPUT_NAME),
        );
        let to_addr = ExtendedSpendingKey::master(&[99; 32]).default_address().1;

        let z_fields = ZCoinFields {
            mode: ZcoinMode::Light,
            dex_fee_addr: ExtendedSpendingKey::master(&[98; 32]).default_address().1,
            my_z_addr,
            my_z_addr_encoded,
            z_spending_key: extsk.clone(),
            z_tx_prover,
            z_unspent_mutex: AsyncMutex::new(()),
            in_flight_spends: Mutex::new(HashMap::new()),
            send_pacing: test_pacing(in_flight_wait_secs),
            electrum_fallback: z_electrum_fallback::ElectrumFallback::new(
                "ARRR",
                z_electrum_fallback::tests::ScriptedElectrum::none(),
            ),
            shielded_sync_error: Mutex::new(None),
            sapling_state_synced: AtomicBool::new(true),
            sapling_loop_error: Mutex::new(None),
            sapling_loop_stopped: Arc::new(AtomicBool::new(false)),
            sapling_cache,
            shielded_history: history,
            wallet_db_scan_complete: AtomicBool::new(true),
            wallet_db_scanned_through: AtomicU64::new(scanned_through),
            pending_receipts: Mutex::new(HashMap::new()),
            consensus_params: params.clone(),
            check_point_block: None,
            blocks_per_iteration: 1_000,
            inter_iteration_interval_ms: 0,
        };
        LightCoinFixture {
            coin: ZCoin {
                utxo_arc: UtxoArc::new(utxo),
                z_fields: Arc::new(z_fields),
            },
            transport,
            db_dir,
            params_dir,
            params,
            extsk,
            to_addr,
            in_flight_wait_secs,
            tip,
        }
    }

    /// A daemon restart: the coin is dropped and a new one is built over the same wallet database
    /// directory, with an empty in-flight map and a new connection to the (still scripted) backend.
    fn reopen(self) -> LightCoinFixture {
        let LightCoinFixture {
            coin,
            db_dir,
            params_dir,
            params,
            extsk,
            in_flight_wait_secs,
            tip,
            ..
        } = self;
        drop(coin);
        #[allow(deprecated)]
        let extfvk = extsk.to_extended_full_viewing_key();
        let history = Self::open_history(&db_dir, &params, &extfvk);
        Self::assemble(db_dir, params_dir, params, extsk, history, in_flight_wait_secs, tip)
    }

    fn history(&self) -> &ZCoinShieldedHistory { &self.coin.z_fields.shielded_history }

    /// Gives the coin's Electrum fallback the scripted servers `electrum` (CRD ch.39 R39.11.25).
    fn with_electrum(mut self, electrum: Arc<z_electrum_fallback::tests::ScriptedElectrum>) -> Self {
        Arc::get_mut(&mut self.coin.z_fields)
            .expect("the fixture's coin is not shared")
            .electrum_fallback = z_electrum_fallback::ElectrumFallback::new("ARRR", electrum);
        self
    }

    /// Every scripted lightwalletd endpoint answers every transparent-address read with the node's
    /// no-address-index rejection (R39.11.4i).
    fn without_address_index(&self) {
        for script in self.transport.scripts.lock().values_mut() {
            script.address_error = Some(EndpointError::NoAddressIndex(format!(
                "Internal error: -5: {}",
                z_lightwalletd::NO_ADDRESS_INDEX_MESSAGE
            )));
        }
    }

    /// Every scripted lightwalletd endpoint reports `hash` for the block at `height`.
    fn report_block_hash(&self, height: u64, hash: [u8; 32]) {
        for script in self.transport.scripts.lock().values_mut() {
            script.block_hashes.insert(height, hash);
        }
    }

    fn output(&self, amount_sat: u64) -> ZOutput {
        ZOutput {
            to_addr: self.to_addr.clone(),
            amount: Amount::from_u64(amount_sat).unwrap(),
            viewing_key: None,
            memo: None,
        }
    }

    async fn send(&self, amount_sat: u64) -> Result<ZTransaction, MmError<SendOutputsErr>> {
        self.coin.send_outputs(vec![], vec![self.output(amount_sat)]).await
    }

    /// A withdrawal-side build: selected and signed, nothing broadcast.
    async fn build(&self, amount_sat: u64) -> Result<(ZTransaction, AdditionalTxData), MmError<GenTxError>> {
        self.coin.gen_tx(vec![], vec![self.output(amount_sat)]).await
    }

    fn wallet_balance(&self) -> u64 { self.history().balance(self.params.clone()).unwrap() }

    /// The balance as `my_balance` answers it, in zatoshi: (spendable, unspendable).
    async fn reported_balance(&self) -> (BigDecimal, BigDecimal) {
        let balance = self.coin.my_balance().compat().await.unwrap();
        (balance.spendable, balance.unspendable)
    }

    /// Reports a mempool receipt for output `output_index` of the transaction `txid`, as the poll does.
    fn insert_pending_receipt(&self, txid: [u8; 32], output_index: u32, value: u64) {
        self.coin
            .z_fields
            .pending_receipts
            .lock()
            .unwrap()
            .insert((txid, output_index), value);
    }

    /// The first history page as `z_coin_tx_history` serves it, at the tip the backend reports.
    fn history_page(&self, include_unmined_sends: bool) -> ZCoinTxHistoryPage {
        self.history()
            .load_page(
                "ARRR",
                "zs-wallet",
                8,
                self.tip,
                &common::PagingOptionsEnum::PageNumber(std::num::NonZeroUsize::new(1).unwrap()),
                10,
                include_unmined_sends,
            )
            .unwrap()
    }

    /// The wallet database's received notes of `tx` as (output index, value, has nullifier, has tree position).
    fn received_notes_of(&self, tx: &ZTransaction) -> Vec<(u32, i64, bool, bool)> {
        let conn = db_common::sqlite::rusqlite::Connection::open(self.history().wallet_db_path()).unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT rn.output_index, rn.value, rn.nf IS NOT NULL, rn.commitment_tree_position IS NOT NULL
                 FROM sapling_received_notes rn
                 JOIN transactions t ON t.id_tx = rn.transaction_id
                 WHERE t.txid = ?1 ORDER BY rn.output_index",
            )
            .unwrap();
        let rows = stmt
            .query_map(db_common::sqlite::rusqlite::params![&tx.txid().as_ref()[..]], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows
    }

    /// The total the wallet database's own summary reports, which counts the notes of an unmined
    /// transaction as pending value.
    fn raw_summary_total(&self) -> u64 {
        let wallet_db = zcash_client_sqlite::WalletDb::for_path(
            self.history().wallet_db_path(),
            self.params.clone(),
            zcash_client_sqlite::util::SystemClock,
            rand::rngs::OsRng,
        )
        .unwrap();
        wallet_db
            .get_wallet_summary(ConfirmationsPolicy::MIN)
            .unwrap()
            .map(|summary| {
                summary
                    .account_balances()
                    .values()
                    .map(|balance| balance.sapling_balance().total().into_u64())
                    .sum()
            })
            .unwrap_or(0)
    }

    /// The value of this wallet's notes that the wallet database records `tx` as spending.
    fn recorded_spend(&self, tx: &ZTransaction) -> u64 {
        self.history().spent_value_of_transaction(tx.txid().as_ref()).unwrap()
    }

    fn in_flight(&self) -> BTreeSet<[u8; 32]> {
        self.coin
            .z_fields
            .in_flight_spends
            .lock()
            .unwrap()
            .keys()
            .copied()
            .collect()
    }

    /// How many broadcasts reached the backend, accepted or not.
    fn broadcasts(&self) -> usize { self.transport.sent.lock().len() }

    /// The transaction behind the `n`th broadcast the backend was handed.
    fn broadcast_tx(&self, n: usize) -> ZTransaction {
        let bytes = self.transport.sent.lock()[n].clone();
        ZTransaction::read(&bytes[..], BranchId::Canopy).unwrap()
    }

    fn script_send(&self, answer: Result<(), EndpointError>) {
        for script in self.transport.scripts.lock().values_mut() {
            script.send = Some(answer.clone());
        }
    }

    fn script_send_delay(&self, delay: Duration) {
        for script in self.transport.scripts.lock().values_mut() {
            script.send_delay = Some(delay);
        }
    }

    /// Moves the chain tip the endpoints report, which is also the height a broadcast is mined at.
    fn set_tip(&mut self, tip: u64) {
        self.tip = tip;
        for script in self.transport.scripts.lock().values_mut() {
            script.tip_floor = Some(tip);
            script.tip_after_send = Some(tip.max(12));
        }
    }

    /// Scans empty blocks up to `tip`, as the sync loop does, and moves the chain to it.
    fn advance_empty_blocks_to(&mut self, tip: u32) {
        let from = self.history().scanned_height().unwrap().unwrap();
        // The tree size is derived while scanning; the helper's zero would contradict the notes.
        let blocks = (from + 1..=u64::from(tip))
            .map(|h| {
                let mut block = empty_compact_block(h, h as u8, (h - 1) as u8);
                block.chain_metadata = None;
                block
            })
            .collect::<Vec<_>>();
        self.scan_blocks(&blocks, u64::from(tip));
    }

    /// Mines `tx` into block `height` and scans it: the block is made of the transaction's own
    /// spends and outputs, as a lightwalletd serves it.
    fn scan_block_containing(&mut self, tx: &ZTransaction, height: u32) {
        let scanned = self.history().scanned_height().unwrap().unwrap();
        assert_eq!(
            scanned,
            u64::from(height) - 1,
            "the containing block must follow the scanned tip directly"
        );
        let bundle = tx.sapling_bundle().expect("a shielded transaction");
        let compact_tx = zcash_compact::CompactTx {
            index: 1,
            txid: tx.txid().as_ref().to_vec(),
            spends: bundle
                .shielded_spends()
                .iter()
                .map(|spend| zcash_compact::CompactSaplingSpend {
                    nf: spend.nullifier().0.to_vec(),
                })
                .collect(),
            outputs: bundle
                .shielded_outputs()
                .iter()
                .map(|output| zcash_compact::CompactSaplingOutput {
                    cmu: output.cmu().to_bytes().to_vec(),
                    ephemeral_key: output.ephemeral_key().0.to_vec(),
                    ciphertext: output.enc_ciphertext()[..52].to_vec(),
                })
                .collect(),
            ..Default::default()
        };
        let mut block = empty_compact_block(u64::from(height), height as u8, (height - 1) as u8);
        block.vtx = vec![compact_tx];
        block.chain_metadata = None;
        self.scan_blocks(&[block], u64::from(height));
    }

    fn scan_blocks(&mut self, blocks: &[zcash_compact::CompactBlock], through: u64) {
        self.history().insert_compact_blocks_for_test(blocks).unwrap();
        let scanned = self
            .history()
            .scan_cached_blocks_to_height(self.params.clone(), through, 1_000, 0, |_, _| {})
            .unwrap();
        assert_eq!(scanned, through);
        self.coin
            .z_fields
            .wallet_db_scanned_through
            .store(scanned, AtomicOrdering::Relaxed);
        self.set_tip(self.tip.max(through));
    }
}

fn nullifiers(tx: &ZTransaction) -> BTreeSet<[u8; 32]> {
    tx.sapling_bundle()
        .map(|bundle| {
            bundle
                .shielded_spends()
                .iter()
                .map(|spend| spend.nullifier().0)
                .collect()
        })
        .unwrap_or_default()
}

/// The nullifier of the `index`th note (tree position `index`) a fixture wallet was paid, given the
/// rseed byte the fixture used for it.
fn received_note_nullifier(extsk: &ExtendedSpendingKey, rseed_byte: u8, index: u64) -> [u8; 32] {
    #[allow(deprecated)]
    let extfvk = extsk.to_extended_full_viewing_key();
    let note = Note::from_parts(
        extfvk.default_address().1,
        NoteValue::from_raw(NOTE_VALUE),
        Rseed::AfterZip212([rseed_byte; 32]),
    );
    note.nf(&extfvk.fvk.vk.nk, index).0
}

fn insufficient(err: &GenTxError) -> Option<&BigDecimal> {
    match err {
        GenTxError::InsufficientBalance { available, .. } => Some(available),
        _ => None,
    }
}

fn sat(value: u64) -> BigDecimal { big_decimal_from_sat_unsigned(value, 8) }

/// Guards the pacing production runs with: the in-flight budget is 600 s with 15 s retries, aligned
/// with the reference's budget; the confirmation values are unchanged. The builder installs
/// `SendPacing::default()`, so a change here is a change to production waits.
#[test]
fn production_send_pacing_is_the_reference_budget() {
    let pacing = SendPacing::default();
    assert_eq!(pacing.in_flight_wait_secs, 600);
    assert_eq!(pacing.in_flight_retry_secs, 15.);
    assert_eq!(pacing.confirmation_wait_secs, 4000);
    assert_eq!(pacing.confirmation_check_secs, 10);
}

/// T39.8.0h, send path baseline (R39.8.0ap, R39.8.0at): a successful send reaches the backend
/// exactly once, the wallet database record is written only after the backend accepted it, and the
/// record then holds the note it spent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_send_is_recorded_only_after_the_backend_accepted_it() {
    let Some(fixture) = LightCoinFixture::new() else { return };

    // What the wallet database knows of the transaction at the moment the backend receives it.
    let seen_at_broadcast = Arc::new(parking_lot::Mutex::new(Vec::new()));
    {
        let history = fixture.history().clone();
        let seen = seen_at_broadcast.clone();
        *fixture.transport.on_send.lock() = Some(Arc::new(move |bytes: &[u8]| {
            let tx = ZTransaction::read(bytes, BranchId::Canopy).unwrap();
            seen.lock()
                .push(history.spent_value_of_transaction(tx.txid().as_ref()).unwrap());
        }));
    }

    let tx = fixture.send(100_000_000).await.expect("the send succeeds");

    assert_eq!(fixture.broadcasts(), 1);
    assert_eq!(
        *seen_at_broadcast.lock(),
        vec![0],
        "nothing is recorded before the broadcast"
    );
    assert_eq!(
        fixture.broadcast_tx(0).txid(),
        tx.txid(),
        "the backend was handed the transaction the send returned"
    );
    assert_eq!(fixture.recorded_spend(&tx), NOTE_VALUE);
    assert_eq!(fixture.in_flight(), BTreeSet::from([*tx.txid().as_ref()]));
}

/// T39.8.0h: "a broadcast which the backend rejects leaves every selected note immediately reusable
/// and the reported balance unchanged, so an immediate retry succeeds" (R39.8.0ap).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rejected_broadcast_leaves_the_note_reusable_and_the_balance_unchanged() {
    let Some(fixture) = LightCoinFixture::new() else { return };
    let balance_before = fixture.wallet_balance();
    assert_eq!(balance_before, NOTE_VALUE);

    fixture.script_send(Err(EndpointError::Rejected {
        code: -26,
        message: "bad-txns-sapling-duplicate-nullifier".to_owned(),
    }));
    let err = fixture.send(100_000_000).await.expect_err("the backend refused it");
    assert!(
        matches!(err.get_inner(), SendOutputsErr::Rpc(_)),
        "the refusal is reported as the backend's: {err:?}"
    );

    assert_eq!(fixture.broadcasts(), 1);
    let rejected = fixture.broadcast_tx(0);
    assert_eq!(nullifiers(&rejected).len(), 1);
    assert_eq!(
        fixture.recorded_spend(&rejected),
        0,
        "a refused broadcast records nothing"
    );
    assert_eq!(fixture.wallet_balance(), balance_before);
    assert!(fixture.in_flight().is_empty());

    fixture.script_send(Ok(()));
    let retry = fixture.send(100_000_000).await.expect("the immediate retry succeeds");
    assert_eq!(
        nullifiers(&retry),
        nullifiers(&rejected),
        "the retry spends the very note the refused attempt had selected"
    );
    assert_eq!(fixture.recorded_spend(&retry), NOTE_VALUE);
}

/// T39.8.0h first clause, send and withdraw paths: after a broadcast, the notes it spends are not
/// offered to a second send, which waits (the shortfall is covered by the in-flight spend) and ends in
/// the typed bounded-wait failure rather than a transaction reusing them; the withdrawal-side build
/// names the cause. Nothing but the first transaction reaches the backend.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_send_declines_the_committed_note_and_ends_in_the_bounded_wait() {
    let Some(fixture) = LightCoinFixture::new() else { return };
    let first = fixture.send(100_000_000).await.unwrap();

    let started = Instant::now();
    let err = fixture
        .send(50_000_000)
        .await
        .expect_err("the only note is committed to the first transaction");
    assert!(
        matches!(err.get_inner(), SendOutputsErr::InFlightSpendWaitTimeout(budget) if *budget == fixture.in_flight_wait_secs),
        "{err:?}"
    );
    // The budget is counted in whole seconds, so a two second budget is more than one real second.
    assert!(
        started.elapsed() >= Duration::from_secs(1),
        "the send waited instead of failing at once"
    );
    assert_eq!(fixture.broadcasts(), 1, "the second send reached the backend");
    assert_eq!(fixture.in_flight(), BTreeSet::from([*first.txid().as_ref()]));

    let err = fixture.build(50_000_000).await.expect_err("the build is refused too");
    match err.get_inner() {
        GenTxError::BalanceLockedByUnconfirmedTransaction { available, locked, .. } => {
            assert_eq!(*available, sat(0));
            assert_eq!(*locked, sat(NOTE_VALUE));
        },
        other => panic!("expected the named shortfall, got {other}"),
    }
    assert_eq!(fixture.broadcasts(), 1);
}

/// T39.8.0h / R39.8.0as: "restarting the daemon between the broadcast and the containing block's
/// scan does not re-offer the excluded notes". The exclusion is the wallet database record, so it
/// survives. What does not survive is the process's own list of in-flight spends (D39.8.0e): the
/// shortfall is then the bare error, and a send fails at once instead of waiting for a spend it
/// cannot know about. This test pins that limit so a change to it is noticed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_between_broadcast_and_scan_does_not_re_offer_the_note() {
    let Some(fixture) = LightCoinFixture::with_notes(1, 30) else {
        return;
    };
    let first = fixture.send(100_000_000).await.unwrap();
    assert_eq!(fixture.in_flight().len(), 1);

    let restarted = fixture.reopen();
    assert!(restarted.in_flight().is_empty(), "the in-memory record is lost");
    assert_eq!(
        restarted.recorded_spend(&first),
        NOTE_VALUE,
        "the wallet database record survived"
    );

    let err = restarted
        .build(50_000_000)
        .await
        .expect_err("the note is still excluded");
    assert!(
        matches!(err.get_inner(), GenTxError::InsufficientBalance { available, .. } if *available == sat(0)),
        "after a restart the shortfall is the bare error (D39.8.0e): {err:?}"
    );

    let started = Instant::now();
    let err = restarted.send(50_000_000).await.expect_err("nothing is spendable");
    assert!(
        matches!(err.get_inner(), SendOutputsErr::GenTxError(e) if insufficient(e) == Some(&sat(0))),
        "{err:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the send failed at once and did not consume its {} s budget",
        restarted.in_flight_wait_secs
    );
    assert_eq!(restarted.broadcasts(), 0, "the restarted coin broadcast nothing");
    assert!(restarted.in_flight().is_empty());
    assert_eq!(restarted.recorded_spend(&first), NOTE_VALUE);
}

/// T39.8.0h (jointly with T39.8.0g): "scanning the containing block makes the change spendable and
/// the spent notes permanently absent, with no double counting across that transition".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scanning_the_containing_block_frees_the_change_and_retires_the_spent_note() {
    let Some(mut fixture) = LightCoinFixture::new() else {
        return;
    };
    let first = fixture.send(100_000_000).await.unwrap();
    assert_eq!(fixture.in_flight().len(), 1);

    fixture.scan_block_containing(&first, 12);
    assert!(fixture.history().transaction_is_scanned(first.txid().as_ref()).unwrap());
    let change = NOTE_VALUE - 100_000_000 - FEE_SAT;
    assert_eq!(
        fixture.wallet_balance(),
        change,
        "exactly the change: the spent note is gone and nothing is counted twice"
    );

    // A build prunes entries the scan has released.
    let (second_build, _) = fixture.build(10_000_000).await.expect("the change is spendable");
    assert!(
        fixture.in_flight().is_empty(),
        "the scanned transaction is no longer in flight"
    );
    assert!(
        nullifiers(&second_build).is_disjoint(&nullifiers(&first)),
        "the change note is spent, not the one the first transaction spent"
    );

    // More than the change: short for good, so it fails now. Were the spent note still offered the
    // request would be funded, were it merely excluded the failure would be a wait.
    let started = Instant::now();
    let err = fixture.send(30_000_000).await.expect_err("only the change remains");
    assert!(
        matches!(err.get_inner(), SendOutputsErr::GenTxError(e) if insufficient(e) == Some(&sat(change))),
        "{err:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(fixture.broadcasts(), 1);

    let second = fixture.send(10_000_000).await.expect("the change is spent");
    assert!(nullifiers(&second).is_disjoint(&nullifiers(&first)));
    assert_eq!(nullifiers(&second).len(), 1);
    assert_eq!(fixture.broadcasts(), 2);

    // The change note is now the committed one; the first note is still absent.
    let err = fixture.send(30_000_000).await.expect_err("nothing remains");
    assert!(
        matches!(err.get_inner(), SendOutputsErr::GenTxError(e) if insufficient(e) == Some(&sat(0))),
        "{err:?}"
    );
    assert_eq!(fixture.in_flight(), BTreeSet::from([*second.txid().as_ref()]));
}

/// T39.8.0h: "a transaction which is broadcast and never mined stops excluding its inputs once the
/// wallet's scanned tip passes its expiry height, with the recovered value spendable afterwards"
/// (R39.8.0ar). It still excludes them while it can yet be mined in the block at its expiry height.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_broadcast_that_is_never_mined_stops_excluding_its_note_after_its_expiry() {
    let Some(mut fixture) = LightCoinFixture::new() else {
        return;
    };
    let never_mined = fixture.send(100_000_000).await.unwrap();
    let expiry: u32 = never_mined.expiry_height().into();
    assert_eq!(expiry, 12 + 40, "the builder's default expiry delta bounds the hold");

    fixture.advance_empty_blocks_to(expiry - 1);
    let err = fixture
        .send(50_000_000)
        .await
        .expect_err("the transaction can still be mined, so its note is held");
    assert!(
        matches!(err.get_inner(), SendOutputsErr::InFlightSpendWaitTimeout(_)),
        "{err:?}"
    );
    assert_eq!(fixture.in_flight(), BTreeSet::from([*never_mined.txid().as_ref()]));
    assert_eq!(fixture.broadcasts(), 1);

    fixture.advance_empty_blocks_to(expiry);
    let reused = fixture
        .send(50_000_000)
        .await
        .expect("the note is free once the transaction has expired");
    assert_eq!(
        nullifiers(&reused),
        nullifiers(&never_mined),
        "the recovered note is spent again"
    );
    assert_eq!(
        fixture.in_flight(),
        BTreeSet::from([*reused.txid().as_ref()]),
        "the expired entry was pruned"
    );
    assert_eq!(fixture.broadcasts(), 2);
}

/// T39.8.0h concurrency fixture (R39.8.0at): two sends whose funding overlaps start together, each
/// able to be funded by either of two notes. Exactly one selects each note, and neither is
/// rejected for reusing a nullifier. The broadcast is slowed so the second send is waiting for the
/// section while the first is between selecting its note and recording the broadcast.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_overlapping_sends_select_different_notes_and_neither_reuses_a_nullifier() {
    let Some(fixture) = LightCoinFixture::with_notes(2, 30) else {
        return;
    };
    fixture.script_send_delay(Duration::from_millis(300));
    let expected: BTreeSet<[u8; 32]> = BTreeSet::from([
        received_note_nullifier(&fixture.extsk, 42, 0),
        received_note_nullifier(&fixture.extsk, 43, 1),
    ]);

    let (first_coin, second_coin) = (fixture.coin.clone(), fixture.coin.clone());
    let (first_out, second_out) = (fixture.output(100_000_000), fixture.output(100_000_000));
    let first_send = tokio::spawn(async move { first_coin.send_outputs(vec![], vec![first_out]).await });
    let second_send = tokio::spawn(async move { second_coin.send_outputs(vec![], vec![second_out]).await });
    let first = first_send.await.unwrap().expect("the first send succeeds");
    let second = second_send.await.unwrap().expect("the second send succeeds");

    assert_eq!(nullifiers(&first).len(), 1);
    assert_eq!(nullifiers(&second).len(), 1);
    assert!(
        nullifiers(&first).is_disjoint(&nullifiers(&second)),
        "both sends selected the same note"
    );
    assert_eq!(
        nullifiers(&first)
            .union(&nullifiers(&second))
            .copied()
            .collect::<BTreeSet<_>>(),
        expected,
        "together they spend exactly the wallet's two notes"
    );
    assert_eq!(fixture.broadcasts(), 2);
    assert_eq!(fixture.recorded_spend(&first), NOTE_VALUE);
    assert_eq!(fixture.recorded_spend(&second), NOTE_VALUE);
    assert_eq!(
        fixture.in_flight(),
        BTreeSet::from([*first.txid().as_ref(), *second.txid().as_ref()])
    );
}

/// R39.8.0at: "wait outside the section and re-select inside it on each attempt". A send that is short
/// only because an earlier spend is unscanned waits without holding the section, so the scan can go on,
/// and its next attempt selects what the scan freed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_waiting_send_re_selects_when_the_scan_frees_the_change() {
    let Some(mut fixture) = LightCoinFixture::with_notes(1, 60) else {
        return;
    };
    let first = fixture.send(100_000_000).await.unwrap();

    let waiting_coin = fixture.coin.clone();
    let output = fixture.output(10_000_000);
    let waiting = tokio::spawn(async move { waiting_coin.send_outputs(vec![], vec![output]).await });

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!waiting.is_finished(), "the send is short until the change is scanned");
    assert_eq!(fixture.broadcasts(), 1);

    fixture.scan_block_containing(&first, 12);
    let second = tokio::time::timeout(Duration::from_secs(300), waiting)
        .await
        .expect("the waiting send gave up")
        .unwrap()
        .expect("the waiting send succeeds once the change is scanned");

    assert!(nullifiers(&second).is_disjoint(&nullifiers(&first)));
    assert_eq!(nullifiers(&second).len(), 1, "it spent the change note");
    assert_eq!(fixture.recorded_spend(&second), NOTE_VALUE - 100_000_000 - FEE_SAT);
    assert_eq!(fixture.broadcasts(), 2);
}

/// The row an own send leaves in `z_coin_tx_history`, from broadcast on (CRD ch.39 R39.8.9i, T39.8.9a).
const SEND_AMOUNT: u64 = 100_000_000;

/// T39.8.9a, default half, over a real send and scan: the unmined row is served first with block
/// fields 0 and the final amounts; the block that mines it moves the same row (same internal id, same
/// amounts) to its position, and the scan leaves exactly one received note for the change.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn load_page_unmined_row_keeps_id_and_amounts_after_mining() {
    let Some(mut fixture) = LightCoinFixture::new() else {
        return;
    };
    let tx = fixture.send(SEND_AMOUNT).await.unwrap();
    let change = NOTE_VALUE - SEND_AMOUNT - FEE_SAT;

    let unmined_page = fixture.history_page(true);
    assert_eq!(unmined_page.total, 2, "the receipt of the note and the send");
    let unmined = unmined_page.transactions[0].clone();
    assert_eq!(
        (unmined.block_height, unmined.timestamp, unmined.confirmations),
        (0, 0, 0)
    );
    assert_eq!(unmined.tx_hash, tx.txid().to_string());
    assert_eq!(fixture.received_notes_of(&tx).len(), 1, "the change is recorded once");
    // the first row of the page is the receipt of the funding note, now second
    assert_eq!(unmined_page.transactions[1].block_height, u64::from(NOTES_HEIGHT));

    fixture.scan_block_containing(&tx, 12);
    assert!(fixture.history().transaction_is_scanned(tx.txid().as_ref()).unwrap());
    let mined_page = fixture.history_page(true);
    assert_eq!(mined_page.total, 2, "no second row appears for the mined transaction");
    let mined = &mined_page.transactions[0];
    assert_eq!(mined.internal_id, unmined.internal_id);
    assert_eq!(mined.tx_hash, unmined.tx_hash);
    assert_eq!(mined.spent_by_me, unmined.spent_by_me);
    assert_eq!(mined.received_by_me, unmined.received_by_me);
    assert_eq!(mined.my_balance_change, unmined.my_balance_change);
    assert_eq!(mined.block_height, 12);
    assert_ne!(mined.timestamp, 0);
    assert!(mined.confirmations >= 1);
    let notes = fixture.received_notes_of(&tx);
    assert_eq!(
        notes.len(),
        1,
        "the scan wrote the change over the recorded note, not next to it"
    );
    assert_eq!(notes[0].1 as u64, change);
    assert!(
        notes[0].2 && notes[0].3,
        "the scanned note has its nullifier and tree position"
    );
}

/// T39.8.9d: for a send of `A` with fee `F` from one note of value `V > A + F` the only row ever served
/// reports `spent = V`, `received = V - A - F` and a change of `-(A + F)`, never `-V`, before and after
/// mining.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn own_send_amounts_are_net_of_change() {
    let Some(mut fixture) = LightCoinFixture::new() else {
        return;
    };
    let tx = fixture.send(SEND_AMOUNT).await.unwrap();

    let check = |page: &ZCoinTxHistoryPage| {
        let row = &page.transactions[0];
        assert_eq!(row.tx_hash, tx.txid().to_string());
        assert_eq!(row.spent_by_me, sat(NOTE_VALUE));
        assert_eq!(row.received_by_me, sat(NOTE_VALUE - SEND_AMOUNT - FEE_SAT));
        assert_eq!(row.my_balance_change, -sat(SEND_AMOUNT + FEE_SAT));
        for served in &page.transactions {
            assert_ne!(
                served.my_balance_change,
                -sat(NOTE_VALUE),
                "the whole note is never the loss"
            );
        }
    };
    check(&fixture.history_page(true));
    // With the switch off the row appears only once the block is scanned, with the same amounts.
    assert_eq!(
        fixture.history_page(false).total,
        1,
        "only the receipt of the funding note"
    );
    fixture.scan_block_containing(&tx, 12);
    check(&fixture.history_page(true));
    let parity = fixture.history_page(false);
    assert_eq!(parity.total, 2);
    check(&parity);
}

/// The change a send records at broadcast is not a spendable note until its block is scanned
/// (R39.8.0q, R39.8.9i (d)): note selection cannot offer it, and it has neither a nullifier nor a
/// tree position.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recorded_change_is_not_spendable_before_scan() {
    let Some(mut fixture) = LightCoinFixture::new() else {
        return;
    };
    let tx = fixture.send(SEND_AMOUNT).await.unwrap();
    let change = NOTE_VALUE - SEND_AMOUNT - FEE_SAT;
    // The builder shuffles the order of the outputs, so the change is at either index of the two.
    let outputs = tx
        .sapling_bundle()
        .expect("a shielded transaction")
        .shielded_outputs()
        .len();
    let recorded = fixture.received_notes_of(&tx);
    assert_eq!(recorded.len(), 1, "only the change is recorded");
    let (change_index, change_value, has_nullifier, has_position) = recorded[0];
    assert!(
        (change_index as usize) < outputs,
        "the change index {change_index} is an output of the transaction"
    );
    assert_eq!(
        (change_value, has_nullifier, has_position),
        (change as i64, false, false),
        "the change is recorded, without a nullifier or a position in the tree"
    );

    // Neither the spent note nor the recorded change is offered, however small the request.
    let err = fixture
        .build(10_000)
        .await
        .expect_err("the only unspent value is the unmined change");
    assert!(
        matches!(err.get_inner(), GenTxError::BalanceLockedByUnconfirmedTransaction { available, .. } if *available == sat(0)),
        "nothing is available, so the change was not selectable: {err:?}"
    );

    // Once the block is scanned the change is a real note and is spent by the next build.
    fixture.scan_block_containing(&tx, 12);
    let (next, _) = fixture
        .build(10_000_000)
        .await
        .expect("the scanned change is spendable");
    assert!(nullifiers(&next).is_disjoint(&nullifiers(&tx)));
}

/// The change is one amount, never two: the wallet's scanned-notes figure leaves the recorded change
/// out, and the balance reports it once as non-spendable, whether or not a pending receipt for the
/// same output is also present (R39.8.0ag1, R39.8.0ah, R39.8.9i (d)).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recorded_change_is_not_double_counted_in_balance() {
    let Some(mut fixture) = LightCoinFixture::new() else {
        return;
    };
    assert_eq!(fixture.wallet_balance(), NOTE_VALUE);
    let tx = fixture.send(SEND_AMOUNT).await.unwrap();
    let change = NOTE_VALUE - SEND_AMOUNT - FEE_SAT;

    // The wallet database's own summary does count the recorded change; the scanned-notes figure
    // must not.
    assert_eq!(fixture.raw_summary_total(), change);
    assert_eq!(fixture.wallet_balance(), 0);

    // The change reaches the caller once, as non-spendable balance read from the wallet database;
    // a pending receipt for the same output does not add to it.
    // Changed from the former zero: the change is no longer left to a pending receipt (R39.8.0ag1).
    assert_eq!(fixture.reported_balance().await, (BigDecimal::from(0), sat(change)));
    let output_index = fixture.received_notes_of(&tx)[0].0;
    fixture.insert_pending_receipt(*tx.txid().as_ref(), output_index, change);
    assert_eq!(fixture.reported_balance().await, (BigDecimal::from(0), sat(change)));

    // After the scan it is a scanned note and no longer unspendable.
    fixture.scan_block_containing(&tx, 12);
    fixture.coin.z_fields.pending_receipts.lock().unwrap().clear();
    assert_eq!(fixture.wallet_balance(), change);
    assert_eq!(fixture.reported_balance().await, (sat(change), BigDecimal::from(0)));
}

/// R39.8.0ag1: right after the broadcast, with the pending set empty (the mempool poll can never
/// decrypt a change that goes to the internal address), the change is reported as non-spendable
/// balance. The spent note and the change are both out of the spendable figure.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unmined_send_change_is_unspendable_right_after_broadcast() {
    let Some(fixture) = LightCoinFixture::new() else {
        return;
    };
    assert_eq!(fixture.reported_balance().await, (sat(NOTE_VALUE), BigDecimal::from(0)));
    let _tx = fixture.send(SEND_AMOUNT).await.unwrap();
    let change = NOTE_VALUE - SEND_AMOUNT - FEE_SAT;

    assert!(fixture.coin.z_fields.pending_receipts.lock().unwrap().is_empty());
    assert_eq!(fixture.reported_balance().await, (BigDecimal::from(0), sat(change)));
}

/// R39.8.0ag1: a pending receipt for the output that the wallet database already records does not
/// add to it, while a pending receipt for a different output does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unmined_send_change_is_not_counted_twice_with_a_pending_receipt() {
    let Some(fixture) = LightCoinFixture::new() else {
        return;
    };
    let tx = fixture.send(SEND_AMOUNT).await.unwrap();
    let change = NOTE_VALUE - SEND_AMOUNT - FEE_SAT;
    let txid = *tx.txid().as_ref();
    let output_index = fixture.received_notes_of(&tx)[0].0;

    fixture.insert_pending_receipt(txid, output_index, change);
    assert_eq!(
        fixture.reported_balance().await,
        (BigDecimal::from(0), sat(change)),
        "the same transaction output is one amount"
    );

    // A different output of the same transaction (the builder shuffles the two outputs, so the
    // other one is whichever index the change does not have) and an output of another transaction
    // are other amounts.
    let outputs = tx
        .sapling_bundle()
        .expect("a shielded transaction")
        .shielded_outputs()
        .len();
    assert_eq!(outputs, 2, "the payment and the change");
    let other_index = (0..outputs as u32).find(|index| *index != output_index).unwrap();
    fixture.insert_pending_receipt(txid, other_index, 7_000);
    fixture.insert_pending_receipt([0x5a; 32], output_index, 3_000);
    assert_eq!(
        fixture.reported_balance().await,
        (BigDecimal::from(0), sat(change + 7_000 + 3_000))
    );
}

/// R39.8.0ag1: once the block that mines the send is scanned, the change is spendable and no longer
/// non-spendable: no gap and no double counting across the transition.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unmined_send_change_moves_to_spendable_after_scan() {
    let Some(mut fixture) = LightCoinFixture::new() else {
        return;
    };
    let tx = fixture.send(SEND_AMOUNT).await.unwrap();
    let change = NOTE_VALUE - SEND_AMOUNT - FEE_SAT;
    assert_eq!(fixture.reported_balance().await, (BigDecimal::from(0), sat(change)));

    fixture.scan_block_containing(&tx, 12);
    assert_eq!(fixture.reported_balance().await, (sat(change), BigDecimal::from(0)));
}

/// R39.8.0ag1 with R39.8.0ar: a send that expires unmined leaves the balance; the note it spent is
/// spendable again and the change is in neither figure.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unmined_send_change_leaves_the_balance_on_expiry() {
    let Some(mut fixture) = LightCoinFixture::new() else {
        return;
    };
    let tx = fixture.send(SEND_AMOUNT).await.unwrap();
    let change = NOTE_VALUE - SEND_AMOUNT - FEE_SAT;
    let expiry: u32 = tx.expiry_height().into();

    fixture.advance_empty_blocks_to(expiry - 1);
    assert_eq!(
        fixture.reported_balance().await,
        (BigDecimal::from(0), sat(change)),
        "still unspendable while it can be mined"
    );

    fixture.advance_empty_blocks_to(expiry);
    assert_eq!(
        fixture.reported_balance().await,
        (sat(NOTE_VALUE), BigDecimal::from(0)),
        "the spent note is back; the change is in neither figure"
    );
}

/// R39.8.0ai: the pending view is advisory. A poisoned pending-receipts lock makes its part zero and
/// does not fail the balance; the recorded change, which comes from the wallet database, is still
/// reported (R39.8.0ag1).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unspendable_total_degrades_pending_part_on_poisoned_lock() {
    let Some(fixture) = LightCoinFixture::new() else {
        return;
    };
    let _tx = fixture.send(SEND_AMOUNT).await.unwrap();
    let change = NOTE_VALUE - SEND_AMOUNT - FEE_SAT;
    fixture.insert_pending_receipt([0x5a; 32], 0, 3_000);

    let coin = fixture.coin.clone();
    let poisoner = std::thread::spawn(move || {
        let _guard = coin.z_fields.pending_receipts.lock().unwrap();
        panic!("poison the pending receipts lock");
    });
    assert!(poisoner.join().is_err());
    assert!(fixture.coin.z_fields.pending_receipts.is_poisoned());

    assert_eq!(fixture.reported_balance().await, (BigDecimal::from(0), sat(change)));
}

/// R39.8.0ag1: the value comes from the wallet database, so a restart between the broadcast and the
/// scan does not lose it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unmined_send_change_survives_restart() {
    let Some(fixture) = LightCoinFixture::new() else {
        return;
    };
    let _tx = fixture.send(SEND_AMOUNT).await.unwrap();
    let change = NOTE_VALUE - SEND_AMOUNT - FEE_SAT;

    let restarted = fixture.reopen();
    assert!(restarted.coin.z_fields.pending_receipts.lock().unwrap().is_empty());
    assert_eq!(restarted.reported_balance().await, (BigDecimal::from(0), sat(change)));
}

/// An unmined send that expires leaves nothing behind: its row is no longer served and its change is
/// not in the balance, while the note it spent is spendable again (R39.8.9i (e), R39.8.0ar).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_expired_unmined_send_leaves_no_row_and_no_change_in_the_balance() {
    let Some(mut fixture) = LightCoinFixture::new() else {
        return;
    };
    let tx = fixture.send(SEND_AMOUNT).await.unwrap();
    let expiry: u32 = tx.expiry_height().into();

    fixture.advance_empty_blocks_to(expiry - 1);
    assert_eq!(
        fixture.history_page(true).total,
        2,
        "still served while it can be mined"
    );
    assert_eq!(fixture.wallet_balance(), 0);
    // Added: the change is non-spendable balance while the send can still be mined (R39.8.0ag1).
    assert_eq!(
        fixture.reported_balance().await,
        (BigDecimal::from(0), sat(NOTE_VALUE - SEND_AMOUNT - FEE_SAT))
    );

    fixture.advance_empty_blocks_to(expiry);
    let page = fixture.history_page(true);
    assert_eq!(page.total, 1, "only the receipt of the funding note remains");
    assert_ne!(page.transactions[0].tx_hash, tx.txid().to_string());
    assert_eq!(
        fixture.wallet_balance(),
        NOTE_VALUE,
        "the spent note is back, the change is gone"
    );
    // Added: the expired change is in neither figure (R39.8.0ag1, R39.8.0ar).
    assert_eq!(fixture.reported_balance().await, (sat(NOTE_VALUE), BigDecimal::from(0)));
}

/// T39.5.4 (R39.5.4), a shielded coin's spend search with the swap's own HTLC key. The payment and its
/// refund are the live ARRR pair of `z_tx_read`'s tests (public chain data): the payment pays an HTLC
/// whose refund key is the taker's per-swap key, which is not the coin's persistent key. The backend is
/// the scripted lightwalletd transport; it serves the payment and, in its transparent history, the refund.
mod per_swap_htlc_key_recovery {
    use super::*;
    use crate::{HtlcPaymentCheck, SwapOps};

    const PAYMENT_HEX: &str = include_str!("test-fixtures/arrr-htlc-payment-75566e4d.hex");
    const REFUND_HEX: &str = include_str!("test-fixtures/arrr-htlc-refund-0496f6d3.hex");
    const PAYMENT_TXID_DISPLAY: &str = "75566e4d322874d79db2661d334fa404e449df15723b482957815fef203cf1e2";
    const TIME_LOCK: u32 = 1_790_664_056;
    const TAKER_KEY: &str = "03cd3144d880ce80ce9f2f4c24feb12d011bd6f067cdb44dcd2c41438923d64fe2";
    const MAKER_KEY: &str = "02ccd091588f2acb3b5320dc53950f7b00fff8dfae39286d745e96bed48f49c580";
    const SECRET_HASH: &str = "cb03f4f75479eb6731672f14069d05e1a08d8349";
    /// Heights inside the fixture's chain (tip 11, Sapling activation 2).
    const PAYMENT_HEIGHT: u64 = 5;
    const REFUND_HEIGHT: u64 = 8;
    const SEARCH_FROM: u64 = 3;

    fn bytes(hex_str: &str) -> Vec<u8> { hex::decode(hex_str.trim()).unwrap() }

    /// A Light-mode coin whose backend serves the live payment and, in the transparent history of the
    /// HTLC addresses, its refund. `None` where the Sapling parameters are absent.
    fn coin_with_refunded_payment() -> Option<LightCoinFixture> {
        let fixture = LightCoinFixture::new()?;
        for script in fixture.transport.scripts.lock().values_mut() {
            script.transaction = Some(Ok(RawTx {
                data: bytes(PAYMENT_HEX),
                height: Some(PAYMENT_HEIGHT),
            }));
            script.history = vec![RawTx {
                data: bytes(REFUND_HEX),
                height: Some(REFUND_HEIGHT),
            }];
        }
        Some(fixture)
    }

    fn assert_is_the_live_refund(found: Option<FoundSwapTxSpend>) {
        match found {
            Some(FoundSwapTxSpend::Refunded(tx)) => assert_eq!(tx.tx_hex(), bytes(REFUND_HEX)),
            other => panic!("expected the live refund, got {other:?}"),
        }
    }

    /// With the taker's per-swap key as "my" key the search over this node's own payment ends in the
    /// refund; from the counterparty's side (the maker's key as "my" key) the same payment is found
    /// refunded too.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_payment_built_with_a_per_swap_key_is_found_refunded() {
        let Some(fixture) = coin_with_refunded_payment() else {
            return;
        };
        let (taker, maker, secret_hash) = (bytes(TAKER_KEY), bytes(MAKER_KEY), bytes(SECRET_HASH));
        assert_ne!(
            taker.as_slice(),
            &**fixture.coin.my_public_key().unwrap(),
            "the swap key is not the persistent key"
        );

        let mine = fixture
            .coin
            .search_for_swap_tx_spend_my(
                TIME_LOCK,
                &taker,
                &maker,
                &secret_hash,
                &bytes(PAYMENT_HEX),
                SEARCH_FROM,
                &None,
            )
            .await
            .expect("the search succeeds with the swap's own key");
        assert_is_the_live_refund(mine);

        let other = fixture
            .coin
            .search_for_swap_tx_spend_other(
                TIME_LOCK,
                &maker,
                &taker,
                &secret_hash,
                &bytes(PAYMENT_HEX),
                SEARCH_FROM,
                &None,
            )
            .await
            .expect("the search succeeds from the counterparty's side");
        assert_is_the_live_refund(other);
    }

    /// R39.5.5: a spend search that a deadline drops while an endpoint is slow leaves the endpoint and
    /// address form it was reading, for the coin to report; a search of another HTLC has nothing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_spend_search_dropped_by_a_deadline_reports_the_endpoint_and_form_it_read_last() {
        let Some(fixture) = coin_with_refunded_payment() else {
            return;
        };
        for script in fixture.transport.scripts.lock().values_mut() {
            script.history_delay = Some(std::time::Duration::from_secs(600));
        }
        let (taker, maker, secret_hash) = (bytes(TAKER_KEY), bytes(MAKER_KEY), bytes(SECRET_HASH));
        assert_eq!(
            fixture
                .coin
                .htlc_detection_progress(TIME_LOCK, &taker, &maker, &secret_hash),
            None,
            "nothing is in flight before the search"
        );
        let payment = bytes(PAYMENT_HEX);
        let search = fixture.coin.search_for_swap_tx_spend_my(
            TIME_LOCK,
            &taker,
            &maker,
            &secret_hash,
            &payment,
            SEARCH_FROM,
            &None,
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(3), search)
                .await
                .is_err(),
            "the search is stalled on the slow history"
        );
        let progress = fixture
            .coin
            .htlc_detection_progress(TIME_LOCK, &taker, &maker, &secret_hash)
            .expect("the last read is reported");
        assert!(progress.contains("PubkeyHash form, history of"), "{progress}");
        // Another HTLC (here: the keys the other way round) has no detection in flight.
        assert_eq!(
            fixture
                .coin
                .htlc_detection_progress(TIME_LOCK, &maker, &taker, &secret_hash),
            None
        );
    }

    /// The trait method recovery asks to pick the HTLC key (R39.5.4) answers over the live payment.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_coin_tells_which_key_rebuilds_the_script_a_payment_pays() {
        let Some(fixture) = coin_with_refunded_payment() else {
            return;
        };
        let (taker, maker, secret_hash) = (bytes(TAKER_KEY), bytes(MAKER_KEY), bytes(SECRET_HASH));
        let persistent = fixture.coin.my_public_key().unwrap().to_vec();

        let with_swap_key = fixture
            .coin
            .check_payment_pays_htlc(TIME_LOCK, &taker, &maker, &secret_hash, &bytes(PAYMENT_HEX))
            .await;
        assert_eq!(with_swap_key, Ok(HtlcPaymentCheck::Pays));
        let with_persistent_key = fixture
            .coin
            .check_payment_pays_htlc(TIME_LOCK, &persistent, &maker, &secret_hash, &bytes(PAYMENT_HEX))
            .await;
        assert_eq!(
            with_persistent_key,
            Ok(HtlcPaymentCheck::PaysAnotherScript {
                payment_id: PAYMENT_TXID_DISPLAY.to_owned()
            })
        );
    }

    /// The coin's persistent key does not rebuild the swap's script: the hard stop, naming the payment by
    /// its display-order transaction id, in both directions.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_persistent_key_is_still_the_hard_stop_and_names_the_display_txid() {
        let Some(fixture) = coin_with_refunded_payment() else {
            return;
        };
        let persistent = fixture.coin.my_public_key().unwrap().to_vec();
        let (taker, maker, secret_hash) = (bytes(TAKER_KEY), bytes(MAKER_KEY), bytes(SECRET_HASH));

        let mine = fixture
            .coin
            .search_for_swap_tx_spend_my(
                TIME_LOCK,
                &persistent,
                &maker,
                &secret_hash,
                &bytes(PAYMENT_HEX),
                SEARCH_FROM,
                &None,
            )
            .await
            .unwrap_err();
        assert!(
            mine.contains("does not pay the HTLC script this swap negotiated"),
            "{mine}"
        );
        assert!(mine.contains(PAYMENT_TXID_DISPLAY), "{mine}");

        let other = fixture
            .coin
            .search_for_swap_tx_spend_other(
                TIME_LOCK,
                &persistent,
                &taker,
                &secret_hash,
                &bytes(PAYMENT_HEX),
                SEARCH_FROM,
                &None,
            )
            .await
            .unwrap_err();
        assert!(other.contains(PAYMENT_TXID_DISPLAY), "{other}");
    }
}

/// T39.11.q (R39.11.26): the wallet's own send record answers the already-sent question before any
/// server is asked, and never answers "not sent". The records are made by the real send path
/// (`z_send_htlc` -> `send_outputs` -> `record_sent_in_wallet_db`), the coin is then restarted over the
/// same wallet database, and the question is asked through the swap trait over the scripted backend.
mod own_send_record {
    use super::*;
    use crate::SwapOps;
    use keys::Public;

    const TIME_LOCK: u32 = 1_790_000_000;
    /// The compressed secp256k1 generator and its double: two valid public keys.
    const MY_KEY: &str = "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";
    const OTHER_KEY: &str = "02c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5";
    const SECRET_HASH: [u8; 20] = [5; 20];

    fn key(hex_str: &str) -> Vec<u8> { hex::decode(hex_str).unwrap() }

    /// Funds the HTLC of `(MY_KEY, OTHER_KEY, SECRET_HASH)` from the wallet and returns what the send
    /// returned.
    async fn fund_htlc(fixture: &LightCoinFixture) -> ZTransaction {
        super::super::z_htlc::z_send_htlc(
            &fixture.coin,
            TIME_LOCK,
            &Public::from_slice(&key(MY_KEY)).unwrap(),
            &Public::from_slice(&key(OTHER_KEY)).unwrap(),
            &SECRET_HASH,
            BigDecimal::from(1),
        )
        .await
        .expect("the HTLC is funded")
    }

    async fn already_sent(fixture: &LightCoinFixture, my_key: &str, other_key: &str) -> Option<crate::TransactionEnum> {
        fixture
            .coin
            .check_if_my_payment_sent(TIME_LOCK, &key(my_key), &key(other_key), &SECRET_HASH, 0, &None)
            .compat()
            .await
            .expect("the question is answered")
    }

    /// The requests the fixture's backend has seen, by method.
    fn methods(fixture: &LightCoinFixture) -> Vec<&'static str> {
        fixture.transport.calls.lock().iter().map(|(_, m)| *m).collect()
    }

    /// Requests that put the address question to a server (or fetch a transaction for it).
    fn address_requests(fixture: &LightCoinFixture) -> usize {
        methods(fixture)
            .into_iter()
            .filter(|m| ["history", "utxos", "tx", "balance", "send"].contains(m))
            .count()
    }

    fn assert_is(found: Option<crate::TransactionEnum>, funding: &ZTransaction) {
        let found = found.expect("the payment is reported as sent");
        let mut written = Vec::new();
        funding.write(&mut written).unwrap();
        assert_eq!(found.tx_hex(), written, "the recorded transaction itself is returned");
    }

    /// An unmined record paying exactly the HTLC script, within its expiry, answers "sent" after a
    /// restart. The only request the coin makes is the tip read that expiry is judged against.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unmined_record_within_expiry_answers_sent_without_an_address_request() {
        let Some(fixture) = LightCoinFixture::new() else { return };
        let funding = fund_htlc(&fixture).await;
        let restarted = fixture.reopen();
        assert!(methods(&restarted).is_empty(), "a restart makes no request of its own");

        let found = already_sent(&restarted, MY_KEY, OTHER_KEY).await;

        assert_is(found, &funding);
        assert_eq!(
            address_requests(&restarted),
            0,
            "no server was asked the address question: {:?}",
            methods(&restarted)
        );
        assert!(
            methods(&restarted).iter().all(|m| ["info", "cache", "tip"].contains(m)),
            "only the tip read for the expiry decision: {:?}",
            methods(&restarted)
        );
    }

    /// A mined record counts, and needs no request at all.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_mined_record_answers_sent_with_no_request() {
        let Some(mut fixture) = LightCoinFixture::new() else {
            return;
        };
        let funding = fund_htlc(&fixture).await;
        fixture.scan_block_containing(&funding, 12);
        let restarted = fixture.reopen();
        let script_pubkey = funding
            .transparent_bundle()
            .unwrap()
            .vout
            .iter()
            .find(|out| out.script_pubkey().0 .0.len() == 23)
            .unwrap()
            .script_pubkey()
            .0
             .0
            .clone();
        let records = restarted
            .history()
            .recorded_transactions_containing(&script_pubkey)
            .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].mined_height, Some(12), "the scan marked the record mined");

        let found = already_sent(&restarted, MY_KEY, OTHER_KEY).await;

        assert_is(found, &funding);
        assert!(
            methods(&restarted).is_empty(),
            "a mined record is answered with no request: {:?}",
            methods(&restarted)
        );
    }

    /// An unmined record whose expiry height the fresh tip has passed is disregarded and the servers
    /// are asked; their answer is what comes back.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unmined_record_past_its_expiry_is_disregarded_and_the_servers_are_asked() {
        let Some(fixture) = LightCoinFixture::new() else { return };
        let funding = fund_htlc(&fixture).await;
        let expiry: u64 = u32::from(funding.expiry_height()).into();
        let mut restarted = fixture.reopen();
        restarted.set_tip(expiry + 1);

        let found = already_sent(&restarted, MY_KEY, OTHER_KEY).await;

        assert!(found.is_none(), "the expired record did not answer");
        assert!(
            address_requests(&restarted) > 0,
            "the servers were asked: {:?}",
            methods(&restarted)
        );
    }

    /// The expiry boundary is the one `wait_for_confirmations` uses: a transaction is expired when the
    /// tip has passed its expiry height, not when the tip reaches it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_record_still_counts_at_the_expiry_height_itself() {
        let Some(fixture) = LightCoinFixture::new() else { return };
        let funding = fund_htlc(&fixture).await;
        let expiry: u64 = u32::from(funding.expiry_height()).into();
        let mut restarted = fixture.reopen();
        restarted.set_tip(expiry);

        let found = already_sent(&restarted, MY_KEY, OTHER_KEY).await;

        assert_is(found, &funding);
        assert_eq!(address_requests(&restarted), 0, "{:?}", methods(&restarted));
    }

    /// A recorded send that does not pay the asked HTLC's script is disregarded: the question for a
    /// different HTLC goes to the servers.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_recorded_send_without_the_htlc_output_is_disregarded() {
        let Some(fixture) = LightCoinFixture::new() else { return };
        // A plain shielded send, and an HTLC funding for another key pair: neither pays the asked script.
        fixture.send(SEND_AMOUNT / 2).await.unwrap();
        let restarted = fixture.reopen();

        let found = already_sent(&restarted, OTHER_KEY, MY_KEY).await;

        assert!(found.is_none());
        assert!(
            address_requests(&restarted) > 0,
            "the servers were asked: {:?}",
            methods(&restarted)
        );
    }

    /// The same record does not answer for a script it only resembles: the asked HTLC differs in its
    /// lock time, so the output is not exactly the asked script.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_record_paying_a_different_htlc_script_is_disregarded() {
        let Some(fixture) = LightCoinFixture::new() else { return };
        fund_htlc(&fixture).await;
        let restarted = fixture.reopen();

        let found = restarted
            .coin
            .check_if_my_payment_sent(TIME_LOCK + 1, &key(MY_KEY), &key(OTHER_KEY), &SECRET_HASH, 0, &None)
            .compat()
            .await
            .expect("the question is answered");

        assert!(found.is_none());
        assert!(address_requests(&restarted) > 0, "{:?}", methods(&restarted));
    }

    /// With nothing recorded the answer "not sent" comes from the servers' scripted healthy empty
    /// history; the record contributes nothing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn with_an_empty_record_not_sent_comes_from_the_servers() {
        let Some(fixture) = LightCoinFixture::new() else { return };

        let found = already_sent(&fixture, MY_KEY, OTHER_KEY).await;

        assert!(found.is_none());
        assert!(
            address_requests(&fixture) > 0,
            "the servers were asked and answered: {:?}",
            methods(&fixture)
        );
    }

    /// A wallet database that cannot be read is no answer either way: the question goes on to the
    /// server path, which here fails on the same unreadable database (the backend's branch binding
    /// lives in it) and so returns the undetermined error. The record answered neither "sent" nor
    /// "not sent".
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unreadable_record_falls_through_to_the_server_path() {
        let Some(fixture) = LightCoinFixture::new() else { return };
        fund_htlc(&fixture).await;
        let restarted = fixture.reopen();
        std::fs::write(restarted.history().wallet_db_path(), b"this is not a database").unwrap();

        let answer = restarted
            .coin
            .check_if_my_payment_sent(TIME_LOCK, &key(MY_KEY), &key(OTHER_KEY), &SECRET_HASH, 0, &None)
            .compat()
            .await;

        let error = answer.expect_err("an unreadable record never answers, either way");
        assert!(error.contains("the HTLC could not be searched for"), "{error}");
    }

    /// The own-send record still answers before any Electrum server is asked, even where every
    /// lightwalletd endpoint keeps no address index and a server that would answer "not sent" is listed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_record_answers_before_the_electrum_fallback_is_asked() {
        use crate::z_coin::z_electrum_fallback::tests::{full_header, ScriptedElectrum};
        let Some(fixture) = LightCoinFixture::new() else { return };
        let funding = fund_htlc(&fixture).await;
        let electrum = ScriptedElectrum::healthy(&["e1"], fixture.tip, &full_header());
        electrum.edit("e1", |s| s.history = Some(Ok(vec![])));
        let restarted = fixture.reopen().with_electrum(electrum.clone());
        restarted.without_address_index();

        let found = already_sent(&restarted, MY_KEY, OTHER_KEY).await;

        assert_is(found, &funding);
        assert!(
            electrum.calls().is_empty(),
            "Electrum was never asked: {:?}",
            electrum.calls()
        );
        assert_eq!(address_requests(&restarted), 0, "{:?}", methods(&restarted));
    }
}

/// T39.11.p (R39.11.25): the three swap-path questions of a Light-mode coin over the scripted
/// lightwalletd backend and scripted Electrum servers, asked through the swap trait. The payment and
/// the refund are the live ARRR pair of `per_swap_htlc_key_recovery`.
mod electrum_fallback {
    use super::*;
    use crate::z_coin::z_electrum_fallback::tests::{full_header, hash_of, item, ScriptedElectrum};
    use crate::z_coin::z_electrum_fallback::{UnspentItem, IDENTITY_DEPTH};
    use crate::SwapOps;

    const PAYMENT_HEX: &str = include_str!("test-fixtures/arrr-htlc-payment-75566e4d.hex");
    const REFUND_HEX: &str = include_str!("test-fixtures/arrr-htlc-refund-0496f6d3.hex");
    const TIME_LOCK: u32 = 1_790_664_056;
    const TAKER_KEY: &str = "03cd3144d880ce80ce9f2f4c24feb12d011bd6f067cdb44dcd2c41438923d64fe2";
    const MAKER_KEY: &str = "02ccd091588f2acb3b5320dc53950f7b00fff8dfae39286d745e96bed48f49c580";
    const SECRET_HASH: &str = "cb03f4f75479eb6731672f14069d05e1a08d8349";
    const SEARCH_FROM: u64 = 3;
    const SERVER: &str = "electrum1.example:50001";

    fn bytes(hex_str: &str) -> Vec<u8> { hex::decode(hex_str.trim()).unwrap() }

    /// A coin whose lightwalletd endpoints keep no address index and whose Electrum servers are healthy
    /// and on the same chain.
    fn coin_with_marked_endpoints() -> Option<(LightCoinFixture, Arc<ScriptedElectrum>)> {
        let fixture = LightCoinFixture::new()?;
        let electrum = ScriptedElectrum::healthy(&[SERVER], fixture.tip, &full_header());
        fixture.without_address_index();
        fixture.report_block_hash(fixture.tip - IDENTITY_DEPTH, hash_of(&full_header()));
        Some((fixture.with_electrum(electrum.clone()), electrum))
    }

    async fn already_sent(
        fixture: &LightCoinFixture,
        my_key: &str,
        other_key: &str,
    ) -> Result<Option<crate::TransactionEnum>, String> {
        fixture
            .coin
            .check_if_my_payment_sent(
                TIME_LOCK,
                &bytes(my_key),
                &bytes(other_key),
                &bytes(SECRET_HASH),
                SEARCH_FROM,
                &None,
            )
            .compat()
            .await
    }

    async fn spend_of_payment(fixture: &LightCoinFixture) -> Result<Option<FoundSwapTxSpend>, String> {
        fixture
            .coin
            .search_for_swap_tx_spend_my(
                TIME_LOCK,
                &bytes(TAKER_KEY),
                &bytes(MAKER_KEY),
                &bytes(SECRET_HASH),
                &bytes(PAYMENT_HEX),
                SEARCH_FROM,
                &None,
            )
            .await
    }

    /// An endpoint that answers is final: Electrum is not asked, for any of the questions, and the client
    /// is not even built.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_lightwalletd_answer_is_final_and_electrum_is_never_asked() {
        let Some(fixture) = LightCoinFixture::new() else { return };
        let electrum = ScriptedElectrum::healthy(&[SERVER], fixture.tip, &full_header());
        // What Electrum would say, if asked, differs from what lightwalletd says.
        electrum.edit(SERVER, |s| s.history = Some(Ok(vec![item(&bytes(PAYMENT_HEX), 5)])));
        for script in fixture.transport.scripts.lock().values_mut() {
            script.transaction = Some(Ok(RawTx {
                data: bytes(PAYMENT_HEX),
                height: Some(5),
            }));
            script.history = vec![RawTx {
                data: bytes(REFUND_HEX),
                height: Some(8),
            }];
        }
        let fixture = fixture.with_electrum(electrum.clone());

        // Spend search: lightwalletd finds the refund.
        match spend_of_payment(&fixture).await.expect("lightwalletd answers") {
            Some(FoundSwapTxSpend::Refunded(tx)) => assert_eq!(tx.tx_hex(), bytes(REFUND_HEX)),
            other => panic!("expected the live refund, got {other:?}"),
        }
        // Already-sent: lightwalletd's healthy empty history for another key pair is "not sent".
        let sent = already_sent(&fixture, MAKER_KEY, TAKER_KEY)
            .await
            .expect("lightwalletd answers");
        assert!(sent.is_none(), "a negative lightwalletd answer is final too");
        // Spend watch: one poll, answered by lightwalletd.
        let spend = fixture
            .coin
            .wait_for_tx_spend(&bytes(PAYMENT_HEX), common::now_ms() / 1000 + 30, SEARCH_FROM, &None)
            .compat()
            .await
            .expect("lightwalletd answers the poll");
        assert_eq!(spend.tx_hex(), bytes(REFUND_HEX));

        assert!(
            electrum.calls().is_empty(),
            "Electrum was asked: {:?}",
            electrum.calls()
        );
    }

    /// With every endpoint marked "no address index", already-sent is answered from Electrum, "sent" and
    /// "not sent" alike.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn already_sent_is_answered_from_electrum_when_every_endpoint_is_marked() {
        let Some((fixture, electrum)) = coin_with_marked_endpoints() else {
            return;
        };
        let payment = bytes(PAYMENT_HEX);
        electrum.edit(SERVER, |s| s.history = Some(Ok(vec![item(&payment, 5)])));
        electrum.serve_transaction(SERVER, &payment);

        let sent = already_sent(&fixture, TAKER_KEY, MAKER_KEY)
            .await
            .expect("Electrum answers");
        assert_eq!(sent.expect("sent").tx_hex(), payment);

        // The endpoints are now marked; a second question skips them and Electrum answers again, "not
        // sent" with a complete, empty history of another script.
        electrum.edit(SERVER, |s| s.history = Some(Ok(vec![])));
        let not_sent = already_sent(&fixture, MAKER_KEY, TAKER_KEY)
            .await
            .expect("Electrum answers");
        assert!(not_sent.is_none());
        assert!(electrum.asked(SERVER, "history") >= 2);
    }

    /// The one-shot spend search is answered from Electrum, and the spend it finds is classified by the
    /// shared rule.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_spend_search_is_answered_from_electrum_when_every_endpoint_is_marked() {
        let Some((fixture, electrum)) = coin_with_marked_endpoints() else {
            return;
        };
        let (payment, refund) = (bytes(PAYMENT_HEX), bytes(REFUND_HEX));
        electrum.edit(SERVER, |s| {
            s.history = Some(Ok(vec![item(&payment, 5), item(&refund, 8)]));
        });
        electrum.serve_transaction(SERVER, &refund);

        match spend_of_payment(&fixture).await.expect("Electrum answers") {
            Some(FoundSwapTxSpend::Refunded(tx)) => assert_eq!(tx.tx_hex(), refund),
            other => panic!("expected the live refund, got {other:?}"),
        }

        // No spender in the history: "not spent" needs the unspent listing.
        electrum.edit(SERVER, |s| s.history = Some(Ok(vec![item(&payment, 5)])));
        let error = spend_of_payment(&fixture)
            .await
            .expect_err("undetermined without the listing");
        assert!(error.contains("Electrum fallback"), "{error}");
        let funding = item(&payment, 5).txid;
        electrum.edit(SERVER, |s| {
            s.unspent = Some(Ok(vec![UnspentItem { txid: funding, vout: 0 }]))
        });
        assert!(spend_of_payment(&fixture).await.expect("Electrum answers").is_none());
    }

    /// A poll of the spend watch is answered from Electrum, and the wait ends with the spender.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_spend_watch_poll_is_answered_from_electrum_when_every_endpoint_is_marked() {
        let Some((fixture, electrum)) = coin_with_marked_endpoints() else {
            return;
        };
        let (payment, refund) = (bytes(PAYMENT_HEX), bytes(REFUND_HEX));
        electrum.edit(SERVER, |s| {
            s.history = Some(Ok(vec![item(&payment, 5), item(&refund, 8)]))
        });
        electrum.serve_transaction(SERVER, &refund);

        let spend = fixture
            .coin
            .wait_for_tx_spend(&payment, common::now_ms() / 1000 + 30, SEARCH_FROM, &None)
            .compat()
            .await
            .expect("the first poll finds the spender through Electrum");

        assert_eq!(spend.tx_hex(), refund);
    }

    /// When neither source answers the error carries the conditions of both, and they are on the
    /// sync-error channel; without a listed Electrum server it is lightwalletd's text alone.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn when_neither_source_answers_the_error_names_both() {
        let Some((fixture, electrum)) = coin_with_marked_endpoints() else {
            return;
        };
        electrum.edit(SERVER, |s| s.history = Some(Err("history too large".to_owned())));

        let error = already_sent(&fixture, TAKER_KEY, MAKER_KEY)
            .await
            .expect_err("undetermined");

        assert!(
            error.contains("keeps no address index"),
            "lightwalletd's condition: {error}"
        );
        assert!(error.contains("Electrum fallback"), "{error}");
        assert!(error.contains(&format!("{SERVER}: history too large")), "{error}");
        assert_eq!(fixture.coin.shielded_sync_error().as_deref(), Some(error.as_str()));

        let Some(plain) = LightCoinFixture::new() else { return };
        plain.without_address_index();
        let error = already_sent(&plain, TAKER_KEY, MAKER_KEY)
            .await
            .expect_err("undetermined");
        assert!(error.contains("keeps no address index"), "{error}");
        assert!(!error.contains("Electrum"), "no fallback is configured: {error}");
    }

    /// A server on another chain is not used at the coin level either, and the question is undetermined.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_server_on_another_chain_never_answers_for_the_coin() {
        let Some((fixture, electrum)) = coin_with_marked_endpoints() else {
            return;
        };
        let mut other = full_header();
        other[10] ^= 1;
        electrum.edit(SERVER, |s| {
            s.header = Some(Ok(other));
            s.history = Some(Ok(vec![])); // would be "not sent"
        });

        let error = already_sent(&fixture, TAKER_KEY, MAKER_KEY)
            .await
            .expect_err("not used, so undetermined");

        assert!(error.contains("not on this coin's chain"), "{error}");
        assert_eq!(electrum.asked(SERVER, "history"), 0);
    }
}

/// The refund gate of a shielded leg (CRD ch.39 R39.5.13, T39.5.f), over the scripted backend whose
/// block at height `h` carries time `1000 + h`: with the tip at 100 the median time past of the last
/// 11 blocks is 1095.
mod refund_gate {
    use super::*;
    use crate::{CanRefundHtlc, SwapOps};

    const TIP: u64 = 100;
    const MTP_AT_TIP: u64 = 1_095;

    fn script_tip(fixture: &LightCoinFixture, tip: Option<u64>) {
        for script in fixture.transport.scripts.lock().values_mut() {
            script.tips.clear();
            script.tip_floor = tip;
        }
    }

    fn reads(fixture: &LightCoinFixture, method: &'static str) -> usize {
        SERVERS.iter().map(|s| fixture.transport.called(s, method)).sum()
    }

    async fn gate(fixture: &LightCoinFixture, locktime: u64) -> Result<CanRefundHtlc, String> {
        fixture.coin.can_refund_htlc(locktime).compat().await
    }

    fn waits(answer: Result<CanRefundHtlc, String>) -> u64 {
        match answer.expect("the gate answers") {
            CanRefundHtlc::HaveToWait(secs) => secs,
            CanRefundHtlc::CanRefundNow => panic!("the refund was permitted"),
        }
    }

    /// While the wall clock has not passed the lock time the gate waits as it always did and reads
    /// nothing from the chain.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_wall_clock_wait_reads_no_chain_data() {
        let Some(fixture) = LightCoinFixture::new() else { return };
        script_tip(&fixture, Some(TIP));
        let in_a_while = common::now_ms() / 1000 + 1_000;

        let secs = waits(gate(&fixture, in_a_while).await);

        assert!((1_000..=1_002).contains(&secs), "{secs}");
        assert_eq!(reads(&fixture, "tip"), 0);
        assert_eq!(reads(&fixture, "time"), 0);
        assert_eq!(
            fixture
                .coin
                .refund_gate_closed_note(in_a_while)
                .compat()
                .await
                .map(|n| n.is_some()),
            Ok(true)
        );
        assert_eq!(reads(&fixture, "tip"), 0, "the recovery note reads nothing either");
    }

    /// The wall clock has passed the lock time but the median time past has not: the refund waits, by
    /// the distance to the median and never more than an hour.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_median_time_not_past_the_lock_time_waits_even_though_the_wall_clock_has_passed() {
        let Some(fixture) = LightCoinFixture::new() else { return };
        script_tip(&fixture, Some(TIP));

        assert_eq!(
            waits(gate(&fixture, MTP_AT_TIP).await),
            1,
            "the median equals the lock time"
        );
        assert_eq!(waits(gate(&fixture, MTP_AT_TIP + 100).await), 101);
        assert_eq!(
            waits(gate(&fixture, 100_000).await),
            3_600,
            "a wait of {} s is capped",
            100_000 - MTP_AT_TIP + 1
        );
        // The window is the 11 blocks ending at the tip, read from the one admitted tip.
        assert_eq!(reads(&fixture, "time"), 3 * 11);
        assert_eq!(reads(&fixture, "tip"), 3);
    }

    /// Once the median time past exceeds the lock time the refund is permitted; at the lock time itself it
    /// is not, as a node judges a lock time final only when it is strictly below the median.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_median_time_past_the_lock_time_permits_the_refund() {
        let Some(fixture) = LightCoinFixture::new() else { return };
        script_tip(&fixture, Some(TIP));

        assert!(matches!(
            gate(&fixture, MTP_AT_TIP - 1).await,
            Ok(CanRefundHtlc::CanRefundNow)
        ));
        assert!(matches!(
            gate(&fixture, MTP_AT_TIP).await,
            Ok(CanRefundHtlc::HaveToWait(1))
        ));
        assert_eq!(
            fixture.coin.refund_gate_closed_note(MTP_AT_TIP - 1).compat().await,
            Ok(None),
            "an open gate has nothing to report"
        );
    }

    /// A chain that cannot be read is an error the stage retries, never an open gate.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_chain_read_failure_is_an_error() {
        let Some(fixture) = LightCoinFixture::new() else { return };
        script_tip(&fixture, None);

        let err = match gate(&fixture, 1_000).await {
            Err(e) => e,
            Ok(_) => panic!("an unreadable chain opened the gate"),
        };
        assert!(err.contains("median time past"), "{err}");
        let recovery_err = fixture.coin.refund_gate_closed_note(1_000).compat().await.unwrap_err();
        assert!(recovery_err.contains("median time past"), "{recovery_err}");
    }

    /// `recover_funds_of_swap` before the gate opens is told the lock time and the median it is waiting for.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_recovery_note_names_the_lock_time_and_the_current_median() {
        let Some(fixture) = LightCoinFixture::new() else { return };
        script_tip(&fixture, Some(TIP));

        let note = fixture
            .coin
            .refund_gate_closed_note(MTP_AT_TIP + 100)
            .compat()
            .await
            .unwrap()
            .expect("the gate is closed");

        assert!(note.contains(&(MTP_AT_TIP + 100).to_string()), "{note}");
        assert!(note.contains(&MTP_AT_TIP.to_string()), "{note}");
        assert!(note.contains("median time past"), "{note}");
    }
}
