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

/// Guards the pacing production runs with: the values `send_outputs` used before they became a field.
/// The builder installs `SendPacing::default()`, so a change here is a change to production waits.
#[test]
fn production_send_pacing_is_the_historical_one() {
    let pacing = SendPacing::default();
    assert_eq!(pacing.in_flight_wait_secs, 300);
    assert_eq!(pacing.in_flight_retry_secs, 10.);
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
