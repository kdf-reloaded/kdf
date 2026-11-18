/// This module is named bch_and_slp_tx_history temporary. We will most likely use the same approach for every
/// supported UTXO coin.
use super::RequestTxHistoryResult;
use crate::history_absence::{removed_rows_line, AbsenceMarks, ChainChangeWatch};
use crate::my_tx_history_v2::TxHistoryStorage;
use crate::utxo::bch::BchCoin;
use crate::utxo::utxo_common::{self, listed_height_change, HeightChange};
use crate::utxo::UtxoStandardOps;
use crate::{BlockHeightAndTime, HistorySyncState, MarketCoinOps};
use async_trait::async_trait;
use common::executor::Timer;
use common::log::{error, info, warn};
use common::mm_number::BigDecimal;
use common::state_machine::prelude::*;
use futures::compat::Future01CompatExt;
use mm2_core::mm_ctx::MmArc;
use mm2_metrics::MetricsArc;
use rpc::v1::types::H256 as H256Json;
use std::collections::{HashMap, HashSet};
use std::str::FromStr;

struct BchAndSlpHistoryCtx<Storage: TxHistoryStorage> {
    coin: BchCoin,
    mm_ctx: MmArc,
    storage: Storage,
    metrics: MetricsArc,
    current_balance: BigDecimal,
    /// The absence rule (CRD ch.38 §38.5C): memory only, so a restart starts counting again.
    absence: AbsenceMarks,
    /// The chain-changed events of the coin's client: one ends the wait for the next pass at once (CRD ch.38 §38.5C
    /// amendment of 2026-11-18, item 3).
    chain_changes: ChainChangeWatch,
    /// A chain-changed event came and no listing has succeeded since: it survives a failed listing and is cleared only
    /// by an Ok one.
    chain_changed: bool,
}

// States have to be generic over storage type because BchAndSlpHistoryCtx is generic over it
struct Init<T> {
    phantom: std::marker::PhantomData<T>,
}

impl<T> Init<T> {
    fn new() -> Self {
        Init {
            phantom: Default::default(),
        }
    }
}

impl<T, E> TransitionFrom<Init<T>> for Stopped<T, E> {}

#[async_trait]
impl<T: TxHistoryStorage> State for Init<T> {
    type Ctx = BchAndSlpHistoryCtx<T>;
    type Result = ();

    async fn on_changed(self: Box<Self>, ctx: &mut BchAndSlpHistoryCtx<T>) -> StateResult<BchAndSlpHistoryCtx<T>, ()> {
        *ctx.coin.as_ref().history_sync_state.lock().unwrap() = HistorySyncState::NotStarted;

        if let Err(e) = ctx.storage.init(ctx.coin.ticker()).await {
            return Self::change_state(Stopped::storage_error(e));
        }
        // The address index of the shared store (CRD ch.36 R36.14.26 item 5): orphan clean-up and backfill. A failure
        // is logged and the fetcher goes on: this family has no stop path for it.
        if let Err(e) = ctx.storage.backfill_address_index(ctx.coin.ticker()).await {
            warn!(
                "{}: the address index of the history store could not be prepared: {:?}",
                ctx.coin.ticker(),
                e.into_inner()
            );
        }

        Self::change_state(FetchingTxHashes::new())
    }
}

// States have to be generic over storage type because BchAndSlpHistoryCtx is generic over it
struct FetchingTxHashes<T> {
    phantom: std::marker::PhantomData<T>,
}

impl<T> FetchingTxHashes<T> {
    fn new() -> Self {
        FetchingTxHashes {
            phantom: Default::default(),
        }
    }
}

impl<T> TransitionFrom<Init<T>> for FetchingTxHashes<T> {}
impl<T> TransitionFrom<OnIoErrorCooldown<T>> for FetchingTxHashes<T> {}
impl<T> TransitionFrom<WaitForHistoryUpdateTrigger<T>> for FetchingTxHashes<T> {}

#[async_trait]
impl<T: TxHistoryStorage> State for FetchingTxHashes<T> {
    type Ctx = BchAndSlpHistoryCtx<T>;
    type Result = ();

    async fn on_changed(self: Box<Self>, ctx: &mut BchAndSlpHistoryCtx<T>) -> StateResult<BchAndSlpHistoryCtx<T>, ()> {
        if let Err(e) = ctx.storage.init(ctx.coin.ticker()).await {
            return Self::change_state(Stopped::storage_error(e));
        }

        let maybe_tx_ids = ctx.coin.request_tx_history(ctx.metrics.clone()).await;
        match maybe_tx_ids {
            RequestTxHistoryResult::Ok(all_tx_ids_with_height, qualifying_tip) => {
                // The pass the event asked for has its listing.
                ctx.chain_changed = false;
                let in_storage = match ctx.storage.unique_tx_hashes_num_in_history(ctx.coin.ticker()).await {
                    Ok(num) => num,
                    Err(e) => return Self::change_state(Stopped::storage_error(e)),
                };
                if all_tx_ids_with_height.len() > in_storage {
                    let txes_left = all_tx_ids_with_height.len() - in_storage;
                    *ctx.coin.as_ref().history_sync_state.lock().unwrap() =
                        HistorySyncState::InProgress(json!({ "transactions_left": txes_left }));
                }

                Self::change_state(UpdatingUnconfirmedTxes::new(all_tx_ids_with_height, qualifying_tip))
            },
            RequestTxHistoryResult::HistoryTooLarge => Self::change_state(Stopped::<T, T::Error>::history_too_large()),
            RequestTxHistoryResult::Retry { error } => {
                error!("Error {} on requesting tx history for {}", error, ctx.coin.ticker());
                Self::change_state(OnIoErrorCooldown::new())
            },
            RequestTxHistoryResult::CriticalError(e) => {
                error!(
                    "Critical error {} on requesting tx history for {}",
                    e,
                    ctx.coin.ticker()
                );
                Self::change_state(Stopped::<T, T::Error>::unknown(e))
            },
        }
    }
}

// States have to be generic over storage type because BchAndSlpHistoryCtx is generic over it
struct OnIoErrorCooldown<T> {
    phantom: std::marker::PhantomData<T>,
}

impl<T> OnIoErrorCooldown<T> {
    fn new() -> Self {
        OnIoErrorCooldown {
            phantom: Default::default(),
        }
    }
}

impl<T> TransitionFrom<FetchingTxHashes<T>> for OnIoErrorCooldown<T> {}
impl<T> TransitionFrom<FetchingTransactionsData<T>> for OnIoErrorCooldown<T> {}
impl<T> TransitionFrom<UpdatingUnconfirmedTxes<T>> for OnIoErrorCooldown<T> {}

#[async_trait]
impl<T: TxHistoryStorage> State for OnIoErrorCooldown<T> {
    type Ctx = BchAndSlpHistoryCtx<T>;
    type Result = ();

    async fn on_changed(self: Box<Self>, _ctx: &mut BchAndSlpHistoryCtx<T>) -> StateResult<BchAndSlpHistoryCtx<T>, ()> {
        Timer::sleep(30.).await;
        Self::change_state(FetchingTxHashes::new())
    }
}

// States have to be generic over storage type because BchAndSlpHistoryCtx is generic over it
struct WaitForHistoryUpdateTrigger<T> {
    phantom: std::marker::PhantomData<T>,
}

impl<T> WaitForHistoryUpdateTrigger<T> {
    fn new() -> Self {
        WaitForHistoryUpdateTrigger {
            phantom: Default::default(),
        }
    }
}

impl<T> TransitionFrom<FetchingTransactionsData<T>> for WaitForHistoryUpdateTrigger<T> {}

#[async_trait]
impl<T: TxHistoryStorage> State for WaitForHistoryUpdateTrigger<T> {
    type Ctx = BchAndSlpHistoryCtx<T>;
    type Result = ();

    async fn on_changed(self: Box<Self>, ctx: &mut BchAndSlpHistoryCtx<T>) -> StateResult<Self::Ctx, Self::Result> {
        loop {
            // A chain-changed event runs the pass at once, whatever the tests below say. The event is kept in the flag
            // until a listing succeeds, and a burst of events counts once.
            {
                let idle = Timer::sleep(30.);
                futures::pin_mut!(idle);
                let event = ctx.chain_changes.wait();
                futures::pin_mut!(event);
                futures::future::select(idle, event).await;
            }
            ctx.chain_changed |= ctx.chain_changes.take();
            if ctx.chain_changed {
                return Self::change_state(FetchingTxHashes::new());
            }
            match ctx.storage.history_contains_unconfirmed_txes(ctx.coin.ticker()).await {
                Ok(contains) => {
                    if contains {
                        return Self::change_state(FetchingTxHashes::new());
                    }
                },
                Err(e) => return Self::change_state(Stopped::storage_error(e)),
            }
            // A row found absent once waits for a second listing: do not wait for a balance change.
            if ctx.absence.any() {
                return Self::change_state(FetchingTxHashes::new());
            }

            match ctx.coin.my_balance().compat().await {
                Ok(balance) => {
                    let total_balance = balance.into_total();
                    if ctx.current_balance != total_balance {
                        ctx.current_balance = total_balance;
                        return Self::change_state(FetchingTxHashes::new());
                    }
                },
                Err(e) => {
                    error!("Error {} on balance fetching for the coin {}", e, ctx.coin.ticker());
                },
            }
        }
    }
}

/// The height at which the backend listing reports the stored row `tx_hash`, or `None` when the
/// listing does not contain it. The row and the listing are matched by the transaction hash: the
/// stored text is the displayed (explorer) form, which `H256Json` parses and prints byte for byte,
/// the same order the listing uses (CRD ch.38 R38.5A.10). Errors if the stored text is not a hash.
#[cfg(test)]
fn listed_height(all_tx_ids_with_height: &[(H256Json, u64)], tx_hash: &str) -> Result<Option<u64>, String> {
    let tx_hash = H256Json::from_str(tx_hash).map_err(|e| e.to_string())?;
    Ok(all_tx_ids_with_height
        .iter()
        .find(|(hash, _)| *hash == tx_hash)
        .map(|(_, height)| *height))
}

/// Whether a stored row of height `stored` is to take the height `listed` that the backend listing reports (CRD ch.38
/// R38.5A.10 and the D38.5C.2 status note, item 1): an unconfirmed row listed at a positive height matures, a
/// confirmed row listed at another positive height takes it over, and a confirmed row listed as unconfirmed goes
/// back to height 0.
fn row_takes_listed_height(stored: u64, listed: u64) -> bool {
    (stored == 0 && listed > 0) || listed_height_change(stored, listed) != HeightChange::Same
}

// States have to be generic over storage type because BchAndSlpHistoryCtx is generic over it
struct UpdatingUnconfirmedTxes<T> {
    phantom: std::marker::PhantomData<T>,
    all_tx_ids_with_height: Vec<(H256Json, u64)>,
    /// The tip by which this listing may be judged for the absence rule; `None` when it does not qualify.
    qualifying_tip: Option<u64>,
}

impl<T> UpdatingUnconfirmedTxes<T> {
    fn new(all_tx_ids_with_height: Vec<(H256Json, u64)>, qualifying_tip: Option<u64>) -> Self {
        UpdatingUnconfirmedTxes {
            phantom: Default::default(),
            all_tx_ids_with_height,
            qualifying_tip,
        }
    }
}

impl<T> TransitionFrom<FetchingTxHashes<T>> for UpdatingUnconfirmedTxes<T> {}

#[async_trait]
impl<T: TxHistoryStorage> State for UpdatingUnconfirmedTxes<T> {
    type Ctx = BchAndSlpHistoryCtx<T>;
    type Result = ();

    async fn on_changed(self: Box<Self>, ctx: &mut BchAndSlpHistoryCtx<T>) -> StateResult<BchAndSlpHistoryCtx<T>, ()> {
        let ticker = ctx.coin.ticker().to_owned();
        let stored = match ctx.storage.tx_hashes_and_heights(&ticker).await {
            Ok(stored) => stored,
            Err(e) => return Self::change_state(Stopped::storage_error(e)),
        };
        // The stored heights per hash. A row whose stored text is not a hash is skipped.
        let mut stored_heights: HashMap<H256Json, Vec<u64>> = HashMap::new();
        for (tx_hash, height) in stored {
            match H256Json::from_str(&tx_hash) {
                Ok(hash) => stored_heights.entry(hash).or_default().push(height),
                Err(e) => error!(
                    "Skipping the stored {} row with invalid tx_hash '{}': {}",
                    ticker, tx_hash, e
                ),
            }
        }

        // The removal comes first, before any detail request of the pass (R38.5C.1 item 2). All rows of a hash, the
        // BCH row and the SLP token rows, go together.
        if let Some(tip) = self.qualifying_tip {
            let listed: HashSet<H256Json> = self.all_tx_ids_with_height.iter().map(|(hash, _)| *hash).collect();
            let to_remove = ctx.absence.judge(tip, &listed, stored_heights.keys().copied());
            if !to_remove.is_empty() {
                let texts: Vec<String> = to_remove.iter().map(|hash| format!("{:02x}", hash)).collect();
                match ctx.storage.remove_txes_by_hash(&ticker, texts).await {
                    Ok(removed) => info!("{}", removed_rows_line(&ticker, removed)),
                    Err(e) => return Self::change_state(Stopped::storage_error(e)),
                }
                for hash in &to_remove {
                    stored_heights.remove(hash);
                }
            }
        }

        // A listed hash that the store holds takes the listed height (R38.5A.10, D38.5C.2 status note item 1).
        for (hash, height) in &self.all_tx_ids_with_height {
            let needs_change = stored_heights
                .get(hash)
                .is_some_and(|heights| heights.iter().any(|stored| row_takes_listed_height(*stored, *height)));
            if !needs_change {
                continue;
            }
            let timestamp = if *height > 0 {
                match ctx.coin.get_block_timestamp(*height).await {
                    Ok(time) => Some(time),
                    Err(_) => return Self::change_state(OnIoErrorCooldown::new()),
                }
            } else {
                None
            };
            let rows = match ctx.storage.get_txes_by_hash(&ticker, &format!("{:02x}", hash)).await {
                Ok(rows) => rows,
                Err(e) => return Self::change_state(Stopped::storage_error(e)),
            };
            for mut tx in rows {
                tx.block_height = *height;
                if let Some(time) = timestamp {
                    tx.timestamp = time;
                }
                if let Err(e) = ctx.storage.update_tx_in_history(&ticker, &tx).await {
                    return Self::change_state(Stopped::storage_error(e));
                }
            }
        }
        Self::change_state(FetchingTransactionsData::new(self.all_tx_ids_with_height))
    }
}

// States have to be generic over storage type because BchAndSlpHistoryCtx is generic over it
struct FetchingTransactionsData<T> {
    phantom: std::marker::PhantomData<T>,
    all_tx_ids_with_height: Vec<(H256Json, u64)>,
}

impl<T> TransitionFrom<UpdatingUnconfirmedTxes<T>> for FetchingTransactionsData<T> {}

impl<T> FetchingTransactionsData<T> {
    fn new(all_tx_ids_with_height: Vec<(H256Json, u64)>) -> Self {
        FetchingTransactionsData {
            phantom: Default::default(),
            all_tx_ids_with_height,
        }
    }
}

#[async_trait]
impl<T: TxHistoryStorage> State for FetchingTransactionsData<T> {
    type Ctx = BchAndSlpHistoryCtx<T>;
    type Result = ();

    async fn on_changed(self: Box<Self>, ctx: &mut BchAndSlpHistoryCtx<T>) -> StateResult<BchAndSlpHistoryCtx<T>, ()> {
        for (tx_hash, height) in self.all_tx_ids_with_height {
            let tx_hash_string = format!("{:02x}", tx_hash);
            match ctx
                .storage
                .history_has_tx_hash(ctx.coin.ticker(), &tx_hash_string)
                .await
            {
                Ok(true) => continue,
                Ok(false) => (),
                Err(e) => return Self::change_state(Stopped::storage_error(e)),
            }

            let block_height_and_time = if height > 0 {
                let timestamp = match ctx.coin.get_block_timestamp(height).await {
                    Ok(time) => time,
                    Err(_) => return Self::change_state(OnIoErrorCooldown::new()),
                };
                Some(BlockHeightAndTime { height, timestamp })
            } else {
                None
            };
            let tx_details = match ctx
                .coin
                .transaction_details_with_token_transfers(&tx_hash, block_height_and_time, &ctx.storage)
                .await
            {
                Ok(tx) => tx,
                Err(e) => {
                    error!(
                        "Error {:?} on getting {} tx details for hash {:02x}",
                        e,
                        ctx.coin.ticker(),
                        tx_hash
                    );
                    if e.get_inner().is_malformed_transaction() {
                        // The transaction itself cannot be interpreted: skip it and store the rest of the
                        // pass. It is not written, so a later pass tries it again (CRD ch.38 R38.5A.9).
                        continue;
                    }
                    return Self::change_state(OnIoErrorCooldown::new());
                },
            };

            let tx_history_records = tx_details.clone();
            if let Err(e) = ctx
                .storage
                .add_transactions_to_history(ctx.coin.ticker(), tx_details)
                .await
            {
                return Self::change_state(Stopped::storage_error(e));
            }
            crate::tx_history_streaming::publish_tx_history_records(&ctx.mm_ctx, ctx.coin.ticker(), tx_history_records);

            // wait for for one second to reduce the number of requests to electrum servers
            Timer::sleep(1.).await;
        }
        info!("Tx history fetching finished for {}", ctx.coin.ticker());
        *ctx.coin.as_ref().history_sync_state.lock().unwrap() = HistorySyncState::Finished;
        Self::change_state(WaitForHistoryUpdateTrigger::new())
    }
}

#[derive(Debug)]
enum StopReason<E> {
    HistoryTooLarge,
    StorageError(E),
    UnknownError(String),
}

struct Stopped<T, E> {
    phantom: std::marker::PhantomData<T>,
    stop_reason: StopReason<E>,
}

impl<T, E> Stopped<T, E> {
    fn history_too_large() -> Self {
        Stopped {
            phantom: Default::default(),
            stop_reason: StopReason::HistoryTooLarge,
        }
    }

    fn storage_error(e: E) -> Self {
        Stopped {
            phantom: Default::default(),
            stop_reason: StopReason::StorageError(e),
        }
    }

    fn unknown(e: String) -> Self {
        Stopped {
            phantom: Default::default(),
            stop_reason: StopReason::UnknownError(e),
        }
    }
}

impl<T, E> TransitionFrom<FetchingTxHashes<T>> for Stopped<T, E> {}
impl<T, E> TransitionFrom<UpdatingUnconfirmedTxes<T>> for Stopped<T, E> {}
impl<T, E> TransitionFrom<WaitForHistoryUpdateTrigger<T>> for Stopped<T, E> {}
impl<T, E> TransitionFrom<FetchingTransactionsData<T>> for Stopped<T, E> {}

#[async_trait]
impl<T: TxHistoryStorage, E: std::fmt::Debug + Send + 'static> LastState for Stopped<T, E> {
    type Ctx = BchAndSlpHistoryCtx<T>;
    type Result = ();

    async fn on_changed(self: Box<Self>, ctx: &mut Self::Ctx) -> Self::Result {
        info!(
            "Stopping tx history fetching for {}. Reason: {:?}",
            ctx.coin.ticker(),
            self.stop_reason
        );
        let new_state_json = match self.stop_reason {
            StopReason::HistoryTooLarge => json!({
                "code": utxo_common::HISTORY_TOO_LARGE_ERR_CODE,
                "message": "Got `history too large` error from Electrum server. History is not available",
            }),
            reason => json!({
                "message": format!("{:?}", reason),
            }),
        };
        *ctx.coin.as_ref().history_sync_state.lock().unwrap() = HistorySyncState::Error(new_state_json);
    }
}

pub async fn bch_and_slp_history_loop(
    coin: BchCoin,
    mm_ctx: MmArc,
    storage: impl TxHistoryStorage,
    metrics: MetricsArc,
    current_balance: BigDecimal,
) {
    let chain_changes = ChainChangeWatch::of(&coin.as_ref().rpc_client);
    let ctx = BchAndSlpHistoryCtx {
        coin,
        mm_ctx,
        storage,
        metrics,
        current_balance,
        absence: AbsenceMarks::default(),
        chain_changes,
        chain_changed: false,
    };
    let state_machine: StateMachine<_, ()> = StateMachine::from_ctx(ctx);
    state_machine.run(Init::new()).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utxo::UtxoTx;
    use chain::{OutPoint, TransactionInput, TransactionOutput};
    use rpc::v1::types::ToTxHash;

    // A BCH transaction id as an explorer shows it.
    const DISPLAYED_TXID: &str = "a8dcc3c6776e93e7bd21fb81551e853447c55e2d8ac141b418583bc8095ce390";

    #[test]
    fn bch_unconfirmed_row_matches_listed_hash() {
        // The listing carries the id in the displayed order, as the Electrum client parses it.
        let listed: H256Json = serde_json::from_value(json!(DISPLAYED_TXID)).unwrap();
        // This id is not its own reverse, so a byte-order mix-up cannot pass.
        assert_ne!(listed, listed.reversed());

        let other = listed.reversed();
        let listing = vec![(other, 7), (listed, 1_234)];

        // The stored row holds the displayed text of the id.
        assert_eq!(listed_height(&listing, DISPLAYED_TXID), Ok(Some(1_234)));
        // The reversed id is a different transaction: it is found at its own height, not at the
        // height of the first.
        assert_eq!(listed_height(&listing, &format!("{:02x}", other)), Ok(Some(7)));
        // A stored hash that the listing does not contain.
        let unlisted = "00".repeat(31) + "01";
        assert_eq!(listed_height(&listing, &unlisted), Ok(None));
        // An unconfirmed entry in the listing is reported with its height 0.
        assert_eq!(listed_height(&[(listed, 0)], DISPLAYED_TXID), Ok(Some(0)));
    }

    #[test]
    fn bch_unconfirmed_row_matches_hash_of_a_real_transaction() {
        let mut tx = UtxoTx::default();
        tx.inputs.push(TransactionInput {
            previous_output: OutPoint {
                hash: [3; 32].into(),
                index: 1,
            },
            script_sig: vec![].into(),
            sequence: 0,
            script_witness: vec![],
        });
        tx.outputs.push(TransactionOutput {
            value: 546,
            script_pubkey: vec![0x6a].into(),
        });
        // The details builder stores the displayed (reversed) hash, the listing keys the same hash
        // the same way (`prev_tx_hash` in the details builders).
        let stored = tx.hash().reversed().to_vec().to_tx_hash();
        let listed: H256Json = tx.hash().reversed().into();
        let raw_order: H256Json = tx.hash().into();
        assert_ne!(listed, raw_order);

        assert_eq!(listed_height(&[(listed, 42)], &stored), Ok(Some(42)));
        assert_eq!(listed_height(&[(raw_order, 42)], &stored), Ok(None));
    }

    #[test]
    fn bch_unconfirmed_row_with_an_invalid_hash_is_an_error() {
        let listing = vec![(H256Json::default(), 1)];
        assert!(listed_height(&listing, "not a hash").is_err());
        assert!(listed_height(&listing, "abcd").is_err());
        assert!(listed_height(&listing, "").is_err());
    }

    /// CRD ch.38 R38.5A.9, T38.5A.7: a transaction without outputs is skipped and the rest of the
    /// pass is stored; the skipped transaction is not written.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn malformed_transaction_does_not_stop_the_rest_of_the_pass() {
        use crate::sql_tx_history_storage::SqliteTxHistoryStorage;
        use crate::utxo::bch::bch_coin_without_network_for_test;
        use crate::utxo::{output_script, ScriptType};
        use common::block_on;
        use mm2_core::mm_ctx::MmCtxBuilder;
        use rpc::v1::types::Bytes as BytesJson;

        fn spending(prev_tx_hash: keys::hash::H256, index: u32, output: Option<(u64, BytesJson)>) -> UtxoTx {
            let mut tx = UtxoTx::default();
            tx.inputs.push(TransactionInput {
                previous_output: OutPoint {
                    hash: prev_tx_hash,
                    index,
                },
                script_sig: vec![].into(),
                sequence: 0,
                script_witness: vec![],
            });
            if let Some((value, script_pubkey)) = output {
                tx.outputs.push(TransactionOutput {
                    value,
                    script_pubkey: script_pubkey.0.into(),
                });
            }
            tx
        }

        let coin = bch_coin_without_network_for_test();
        let ticker = coin.ticker().to_owned();
        let storage = SqliteTxHistoryStorage::in_memory();
        block_on(storage.init(&ticker)).unwrap();

        let my_address = coin.as_ref().derivation_method.iguana_or_err().unwrap();
        let my_script = BytesJson::new(output_script(my_address, ScriptType::P2PKH).to_bytes().take());

        let cache = |tx: &UtxoTx| -> H256Json {
            let hash: H256Json = tx.hash().reversed().into();
            let key = BytesJson::new(hash.0.to_vec());
            let bytes = BytesJson::new(serialization::serialize(tx).take());
            block_on(storage.add_tx_to_cache(&ticker, &key, &bytes)).unwrap();
            hash
        };

        // The funding transaction pays my address; the valid transaction spends it.
        let funding = spending([3; 32].into(), 0, Some((1_000, my_script)));
        cache(&funding);
        let valid = spending(funding.hash(), 0, Some((500, BytesJson::new(vec![0x6a]))));
        let valid_hash = cache(&valid);
        // The malformed transaction has no outputs. It comes first in the pass.
        let malformed = spending([4; 32].into(), 0, None);
        let malformed_hash = cache(&malformed);

        let mm_ctx = MmCtxBuilder::default().into_mm_arc();
        let mut ctx = BchAndSlpHistoryCtx {
            coin,
            metrics: mm_ctx.metrics.clone(),
            mm_ctx,
            storage,
            current_balance: BigDecimal::from(0),
            absence: AbsenceMarks::default(),
            chain_changes: ChainChangeWatch::default(),
            chain_changed: false,
        };
        let state = FetchingTransactionsData::<SqliteTxHistoryStorage>::new(vec![(malformed_hash, 0), (valid_hash, 0)]);
        let _next = block_on(Box::new(state).on_changed(&mut ctx));

        let has =
            |hash: H256Json| block_on(ctx.storage.history_has_tx_hash(&ticker, &format!("{:02x}", hash))).unwrap();
        assert!(has(valid_hash), "the rest of the pass is stored");
        assert!(!has(malformed_hash), "the malformed transaction is not written");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn starting_the_fetcher_prepares_the_address_index_of_an_existing_store() {
        // CRD ch.36 R36.14.26 item 5: the preparation (orphan clean-up and backfill) runs when the fetcher starts.
        use crate::sql_tx_history_storage::SqliteTxHistoryStorage;
        use crate::utxo::bch::bch_coin_without_network_for_test;
        use common::block_on;
        use mm2_core::mm_ctx::MmCtxBuilder;

        let coin = bch_coin_without_network_for_test();
        let ticker = coin.ticker().to_owned();
        let table = format!("{}_tx_address", ticker.replace('-', "_"));
        let storage = SqliteTxHistoryStorage::in_memory();
        block_on(storage.init(&ticker)).unwrap();
        let details = |id: &str, from: &str, to: &str| -> crate::TransactionDetails {
            serde_json::from_value(json!({
                "tx_hex": "00", "tx_hash": id, "from": [from], "to": [to],
                "total_amount": "1", "spent_by_me": "0", "received_by_me": "0", "my_balance_change": "0",
                "block_height": 5, "timestamp": 1, "fee_details": {"type": "Utxo", "amount": "0.00001"},
                "coin": ticker, "internal_id": id
            }))
            .unwrap()
        };
        block_on(storage.add_transactions_to_history(&ticker, vec![
            details("aa", "other1", "other2"),
            details("bb", "other3", "other4"),
        ]))
        .unwrap();
        {
            // The state of a store written before the index: no entries of the rows, one entry of a missing row.
            let conn = storage.0.lock().unwrap();
            conn.execute(&format!("DELETE FROM {};", table), []).unwrap();
            conn.execute(
                &format!("INSERT INTO {} (internal_id, address) VALUES ('gone', 'x');", table),
                [],
            )
            .unwrap();
        }

        let mm_ctx = MmCtxBuilder::default().into_mm_arc();
        let mut ctx = BchAndSlpHistoryCtx {
            coin,
            metrics: mm_ctx.metrics.clone(),
            mm_ctx,
            storage,
            current_balance: BigDecimal::from(0),
            absence: AbsenceMarks::default(),
            chain_changes: ChainChangeWatch::default(),
            chain_changed: false,
        };
        let _next = block_on(Box::new(Init::<SqliteTxHistoryStorage>::new()).on_changed(&mut ctx));

        let conn = ctx.storage.0.lock().unwrap();
        let count = |condition: &str| -> i64 {
            conn.query_row(
                &format!("SELECT COUNT(*) FROM {} WHERE {};", table, condition),
                [],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert_eq!(count("internal_id = 'gone'"), 0);
        assert_eq!(count("internal_id = 'aa'"), 2);
        assert_eq!(count("internal_id = 'bb'"), 2);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_failing_preparation_of_the_address_index_does_not_stop_the_fetcher() {
        use crate::sql_tx_history_storage::SqliteTxHistoryStorage;
        use crate::utxo::bch::bch_coin_without_network_for_test;
        use common::block_on;
        use mm2_core::mm_ctx::MmCtxBuilder;

        let coin = bch_coin_without_network_for_test();
        let ticker = coin.ticker().to_owned();
        let storage = SqliteTxHistoryStorage::in_memory();
        block_on(storage.init(&ticker)).unwrap();
        {
            // A stored row whose JSON cannot be read makes the backfill fail.
            let conn = storage.0.lock().unwrap();
            conn.execute(
                &format!(
                    "INSERT INTO {}_tx_history (tx_hash, internal_id, block_height, confirmation_status, token_id, details_json) \
                     VALUES ('aa', 'aa', 5, 1, '', 'not json');",
                    ticker.replace('-', "_")
                ),
                [],
            )
            .unwrap();
        }
        assert!(block_on(storage.backfill_address_index(&ticker)).is_err());

        let mm_ctx = MmCtxBuilder::default().into_mm_arc();
        let mut ctx = BchAndSlpHistoryCtx {
            coin,
            metrics: mm_ctx.metrics.clone(),
            mm_ctx,
            storage,
            current_balance: BigDecimal::from(0),
            absence: AbsenceMarks::default(),
            chain_changes: ChainChangeWatch::default(),
            chain_changed: false,
        };
        // Indirect check only: the state machine gives no hook to name the next state, so this proves that the state
        // does not panic and hands over (a `ChangeState`, not a `Finish`) with the sync state as `Init` set it.
        let next = block_on(Box::new(Init::<SqliteTxHistoryStorage>::new()).on_changed(&mut ctx));
        assert!(matches!(next, StateResult::ChangeState(_)));
        assert!(matches!(
            *ctx.coin.as_ref().history_sync_state.lock().unwrap(),
            HistorySyncState::NotStarted
        ));
    }
}

/// CRD ch.38 §38.5C (D38.5C.1 and D38.5C.2 status notes): the removal rule and the changed height in the BCH and SLP
/// collector, over the SQLite store.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod absence_tests {
    use super::*;
    use crate::sql_tx_history_storage::SqliteTxHistoryStorage;
    use crate::utxo::bch::bch_coin_without_network_for_test;
    use crate::utxo::rpc_clients::{NativeClientImpl, VerboseBlock};
    use common::block_on;
    use mm2_core::mm_ctx::MmCtxBuilder;
    use mocktopus::mocking::*;

    const SLP_TOKEN: &str = "cc";

    fn text_of(byte: u8) -> String { hex::encode([byte; 32]) }

    fn hash_of(byte: u8) -> H256Json { H256Json::from([byte; 32]) }

    /// The BCH row of the transaction `byte`; its `internal_id` is the hash text.
    fn bch_row(ticker: &str, byte: u8, height: u64) -> crate::TransactionDetails {
        bch_row_with_text(ticker, &text_of(byte), height)
    }

    fn bch_row_with_text(ticker: &str, id: &str, height: u64) -> crate::TransactionDetails {
        serde_json::from_value(json!({
            "tx_hex": "00", "tx_hash": id, "from": ["me"], "to": ["other"],
            "total_amount": "1", "spent_by_me": "0", "received_by_me": "0", "my_balance_change": "0",
            "block_height": height, "timestamp": if height > 0 { 1 } else { 0 },
            "fee_details": {"type": "Utxo", "amount": "0.00001"},
            "coin": ticker, "internal_id": id
        }))
        .unwrap()
    }

    /// The SLP token row of the same transaction: the same `tx_hash`, its own `internal_id`.
    fn slp_row(ticker: &str, byte: u8, height: u64) -> crate::TransactionDetails {
        slp_row_with_text(ticker, &text_of(byte), height)
    }

    fn slp_row_with_text(ticker: &str, id: &str, height: u64) -> crate::TransactionDetails {
        let mut row = bch_row_with_text(ticker, id, height);
        row.internal_id = hex::decode(format!("{}01", id)).unwrap().into();
        row.transaction_type = crate::TransactionType::TokenTransfer(hex::decode(SLP_TOKEN).unwrap().into());
        row.from = vec!["me-token-form".to_owned()];
        row.to = vec!["other-token-form".to_owned()];
        row
    }

    struct Fixture {
        ctx: BchAndSlpHistoryCtx<SqliteTxHistoryStorage>,
        ticker: String,
    }

    impl Fixture {
        fn new(rows: impl FnOnce(&str) -> Vec<crate::TransactionDetails>) -> Fixture {
            let coin = bch_coin_without_network_for_test();
            let ticker = coin.ticker().to_owned();
            let storage = SqliteTxHistoryStorage::in_memory();
            block_on(storage.init(&ticker)).unwrap();
            block_on(storage.add_transactions_to_history(&ticker, rows(&ticker))).unwrap();
            let mm_ctx = MmCtxBuilder::default().into_mm_arc();
            Fixture {
                ctx: BchAndSlpHistoryCtx {
                    coin,
                    metrics: mm_ctx.metrics.clone(),
                    mm_ctx,
                    storage,
                    current_balance: BigDecimal::from(0),
                    absence: AbsenceMarks::default(),
                    chain_changes: ChainChangeWatch::default(),
                    chain_changed: false,
                },
                ticker,
            }
        }

        /// One pass over `listing`, taken at `tip` (`None`: the listing does not qualify).
        fn pass(&mut self, listing: &[(u8, u64)], tip: Option<u64>) {
            let listing = listing.iter().map(|(byte, height)| (hash_of(*byte), *height)).collect();
            self.pass_hashes(listing, tip);
        }

        fn pass_hashes(&mut self, listing: Vec<(H256Json, u64)>, tip: Option<u64>) {
            let state = UpdatingUnconfirmedTxes::<SqliteTxHistoryStorage>::new(listing, tip);
            let _next = block_on(Box::new(state).on_changed(&mut self.ctx));
        }

        fn rows_of(&self, byte: u8) -> Vec<crate::TransactionDetails> { self.rows_of_text(&text_of(byte)) }

        fn rows_of_text(&self, text: &str) -> Vec<crate::TransactionDetails> {
            block_on(self.ctx.storage.get_txes_by_hash(&self.ticker, text)).unwrap()
        }

        fn count(&self, table_suffix: &str) -> i64 {
            let conn = self.ctx.storage.0.lock().unwrap();
            conn.query_row(
                &format!(
                    "SELECT COUNT(*) FROM {}_{};",
                    self.ticker.replace('-', "_"),
                    table_suffix
                ),
                [],
                |row| row.get(0),
            )
            .unwrap()
        }
    }

    /// A confirmed transaction with its BCH row and its SLP token row, and a second, listed one.
    fn two_transactions(ticker: &str) -> Vec<crate::TransactionDetails> {
        vec![
            bch_row(ticker, 0xa1, 100),
            bch_row(ticker, 0xb2, 90),
            slp_row(ticker, 0xb2, 90),
        ]
    }

    #[test]
    fn a_confirmed_transaction_absent_from_two_listings_one_block_apart_is_removed_with_its_token_row() {
        let mut fx = Fixture::new(two_transactions);
        assert_eq!((fx.count("tx_history"), fx.count("tx_cache")), (3, 2));
        assert!(fx.count("tx_address") > 0);

        fx.pass(&[(0xa1, 100)], Some(800));
        assert_eq!(fx.rows_of(0xb2).len(), 2, "kept after one absence");
        assert!(fx.ctx.absence.any());

        fx.pass(&[(0xa1, 100)], Some(801));
        assert!(fx.rows_of(0xb2).is_empty(), "the BCH row and the SLP row went together");
        assert_eq!(fx.rows_of(0xa1).len(), 1);
        assert!(!fx.ctx.absence.any());
        // The address index entries of the removed rows went with them; the raw transactions stay in the cache.
        assert_eq!(fx.count("tx_history"), 1);
        let conn = fx.ctx.storage.0.lock().unwrap();
        let orphans: i64 = conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM {0}_tx_address WHERE internal_id NOT IN (SELECT internal_id FROM {0}_tx_history);",
                    fx.ticker.replace('-', "_")
                ),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(orphans, 0);
        drop(conn);
        assert_eq!(fx.count("tx_cache"), 2);
    }

    #[test]
    fn an_unconfirmed_transaction_is_no_longer_removed_at_its_first_absence() {
        let mut fx = Fixture::new(|ticker| vec![bch_row(ticker, 0xe5, 0), slp_row(ticker, 0xe5, 0)]);
        fx.pass(&[], Some(800));
        assert_eq!(fx.rows_of(0xe5).len(), 2, "the first absence keeps it");
        fx.pass(&[], Some(801));
        assert!(fx.rows_of(0xe5).is_empty());
    }

    /// CRD ch.38 §38.5C amendment of 2026-11-18, item 3: a chain-changed event ends the wait for the next pass at once
    /// (the wait is 30 s otherwise), whatever the balance and the marks say.
    #[test]
    fn a_chain_changed_event_ends_the_wait_for_the_next_pass_at_once() {
        let mut fx = Fixture::new(|_| vec![]);
        let (events, receiver) = futures::channel::mpsc::unbounded();
        fx.ctx.chain_changes = ChainChangeWatch::from_receiver(receiver);
        events.unbounded_send(()).unwrap();
        let started = std::time::Instant::now();
        let _next =
            block_on(Box::new(WaitForHistoryUpdateTrigger::<SqliteTxHistoryStorage>::new()).on_changed(&mut fx.ctx));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "the wait was not cut short"
        );
        assert!(!fx.ctx.absence.any());
    }

    /// Two events that are queued together make one pass: the wait takes the whole burst.
    #[test]
    fn two_queued_chain_changed_events_give_one_pass() {
        let mut fx = Fixture::new(|_| vec![]);
        let (events, receiver) = futures::channel::mpsc::unbounded();
        fx.ctx.chain_changes = ChainChangeWatch::from_receiver(receiver);
        events.unbounded_send(()).unwrap();
        events.unbounded_send(()).unwrap();
        let _next =
            block_on(Box::new(WaitForHistoryUpdateTrigger::<SqliteTxHistoryStorage>::new()).on_changed(&mut fx.ctx));
        assert!(fx.ctx.chain_changed, "the pass is asked for");
        assert!(
            !fx.ctx.chain_changes.take(),
            "no second event is left for a second pass"
        );
    }

    fn native_listing_error() -> common::jsonrpc_client::JsonRpcError {
        common::jsonrpc_client::JsonRpcError {
            client_info: "test".to_owned(),
            request: common::jsonrpc_client::JsonRpcRequestEnum::Single(common::jsonrpc_client::JsonRpcRequest {
                jsonrpc: "2.0".to_owned(),
                id: "0".to_owned(),
                method: "listtransactions".to_owned(),
                params: Vec::new(),
            }),
            error: common::jsonrpc_client::JsonRpcErrorType::Transport("no answer".to_owned()),
        }
    }

    /// A chain-changed event survives a failed listing and is cleared only by an Ok one.
    #[test]
    fn a_failed_listing_keeps_the_chain_changed_flag_and_an_ok_one_clears_it() {
        let mut fx = Fixture::new(|_| vec![]);
        fx.ctx.chain_changed = true;
        NativeClientImpl::get_block_count.mock_safe(|_| MockResult::Return(Box::new(futures01::future::ok(800u64))));
        NativeClientImpl::list_transactions
            .mock_safe(|_, _, _| MockResult::Return(Box::new(futures01::future::err(native_listing_error()))));
        let _next = block_on(Box::new(FetchingTxHashes::<SqliteTxHistoryStorage>::new()).on_changed(&mut fx.ctx));
        assert!(fx.ctx.chain_changed, "the failed listing keeps the event");

        NativeClientImpl::list_transactions
            .mock_safe(|_, _, _| MockResult::Return(Box::new(futures01::future::ok(vec![]))));
        let _next = block_on(Box::new(FetchingTxHashes::<SqliteTxHistoryStorage>::new()).on_changed(&mut fx.ctx));
        assert!(!fx.ctx.chain_changed, "the Ok listing clears it");
    }

    #[test]
    fn two_absences_at_the_same_tip_keep_the_transaction() {
        let mut fx = Fixture::new(two_transactions);
        fx.pass(&[(0xa1, 100)], Some(800));
        fx.pass(&[(0xa1, 100)], Some(800));
        assert_eq!(fx.rows_of(0xb2).len(), 2);
        assert!(fx.ctx.absence.any());
    }

    #[test]
    fn a_listing_that_contains_the_transaction_again_clears_the_mark() {
        let mut fx = Fixture::new(two_transactions);
        fx.pass(&[(0xa1, 100)], Some(800));
        assert!(fx.ctx.absence.any());
        fx.pass(&[(0xa1, 100), (0xb2, 90)], Some(801));
        assert!(!fx.ctx.absence.any());
        fx.pass(&[(0xa1, 100)], Some(802));
        assert_eq!(fx.rows_of(0xb2).len(), 2, "counting started again");
    }

    /// A listing that failed, was cut off, was `history too large`, or came from an out-of-date server or during the
    /// disagreement state reaches the pass without a tip (see the tests of `listing_tip_of`).
    #[test]
    fn a_listing_without_a_tip_removes_nothing_and_sets_no_mark() {
        let mut fx = Fixture::new(two_transactions);
        fx.pass(&[(0xa1, 100)], None);
        fx.pass(&[(0xa1, 100)], None);
        assert_eq!(fx.rows_of(0xb2).len(), 2);
        assert!(!fx.ctx.absence.any());
    }

    #[test]
    fn a_confirmed_row_listed_as_unconfirmed_goes_back_to_height_zero_with_its_token_row() {
        let mut fx = Fixture::new(two_transactions);
        fx.pass(&[(0xa1, 100), (0xb2, 0)], Some(800));
        let rows = fx.rows_of(0xb2);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row.block_height == 0), "{rows:?}");
        assert_eq!(fx.rows_of(0xa1)[0].block_height, 100);
    }

    /// The time of a block comes from the daemon: here the height added to a base.
    fn mock_block_times() {
        NativeClientImpl::get_block_hash.mock_safe(|_client, height| {
            let mut bytes = [0u8; 32];
            bytes[..8].copy_from_slice(&height.to_be_bytes());
            MockResult::Return(Box::new(futures01::future::ok(H256Json::from(bytes))))
        });
        NativeClientImpl::get_block.mock_safe(|_client, hash| {
            let height = u64::from_be_bytes(hash.0[..8].try_into().unwrap());
            let block: VerboseBlock = serde_json::from_value(json!({
                "hash": hash, "confirmations": 1, "size": 1, "version": 1,
                "merkleroot": "00".repeat(32), "tx": [], "time": 1_700_000_000u64 + height,
                "nonce": 0, "bits": "1d00ffff", "difficulty": 1.0, "chainwork": "00".repeat(32),
            }))
            .unwrap();
            MockResult::Return(Box::new(futures01::future::ok(block)))
        });
    }

    #[test]
    fn a_held_hash_listed_at_another_height_takes_the_new_height_and_time_in_all_its_rows() {
        mock_block_times();
        let mut fx = Fixture::new(two_transactions);
        fx.pass(&[(0xa1, 100), (0xb2, 95)], Some(800));
        let rows = fx.rows_of(0xb2);
        assert_eq!(rows.len(), 2);
        assert!(rows
            .iter()
            .all(|row| row.block_height == 95 && row.timestamp == 1_700_000_095));
        // The listed hash at its stored height is passed over: nothing changes.
        let a = fx.rows_of(0xa1);
        assert_eq!((a[0].block_height, a[0].timestamp), (100, 1));
    }

    #[test]
    fn an_unconfirmed_row_listed_at_a_positive_height_still_matures_for_both_rows() {
        mock_block_times();
        let mut fx = Fixture::new(|ticker| vec![bch_row(ticker, 0xe5, 0), slp_row(ticker, 0xe5, 0)]);
        fx.pass(&[(0xe5, 120)], Some(800));
        let rows = fx.rows_of(0xe5);
        assert_eq!(rows.len(), 2);
        assert!(rows
            .iter()
            .all(|row| row.block_height == 120 && row.timestamp == 1_700_000_120));
    }

    #[test]
    fn the_store_methods_list_heights_and_remove_all_rows_of_a_hash() {
        let fx = Fixture::new(two_transactions);
        let mut listed = block_on(fx.ctx.storage.tx_hashes_and_heights(&fx.ticker)).unwrap();
        listed.sort();
        assert_eq!(listed.len(), 3);
        assert!(listed.contains(&(text_of(0xa1), 100)));
        assert_eq!(listed.iter().filter(|(hash, _)| *hash == text_of(0xb2)).count(), 2);
        let removed = block_on(fx.ctx.storage.remove_txes_by_hash(&fx.ticker, vec![text_of(0xb2)])).unwrap();
        assert_eq!(removed, 2);
        assert!(fx.rows_of(0xb2).is_empty());
    }

    /// A hash whose bytes read differently in the two byte orders: `[b; 32]` cannot show a reversal.
    fn ordered_text() -> String { hex::encode((0u8..32).collect::<Vec<_>>()) }

    /// The stored text and the listed hash relate as in production: the listing parses the displayed text, and prints
    /// it back byte for byte; the reversed hash is another transaction.
    fn ordered_listing_hash() -> (H256Json, H256Json) {
        let text = ordered_text();
        let listed = H256Json::from_str(&text).unwrap();
        assert_eq!(format!("{:02x}", listed), text);
        assert_ne!(listed, listed.reversed());
        (listed, listed.reversed())
    }

    fn ordered_rows(ticker: &str) -> Vec<crate::TransactionDetails> {
        let text = ordered_text();
        vec![
            bch_row_with_text(ticker, &text, 100),
            slp_row_with_text(ticker, &text, 100),
            bch_row(ticker, 0xa1, 100),
        ]
    }

    #[test]
    fn removal_matches_the_hash_in_the_byte_order_of_the_listing() {
        let (listed, reversed) = ordered_listing_hash();
        let text = ordered_text();

        // The listing contains the stored transaction: kept, and no mark.
        let mut fx = Fixture::new(ordered_rows);
        fx.pass_hashes(vec![(listed, 100), (hash_of(0xa1), 100)], Some(800));
        fx.pass_hashes(vec![(listed, 100), (hash_of(0xa1), 100)], Some(801));
        assert_eq!(fx.rows_of_text(&text).len(), 2);
        assert!(!fx.ctx.absence.any());

        // The listing contains the reversed hash only, which is another transaction: the stored one is absent twice.
        let mut fx = Fixture::new(ordered_rows);
        fx.pass_hashes(vec![(reversed, 100), (hash_of(0xa1), 100)], Some(800));
        assert_eq!(fx.rows_of_text(&text).len(), 2, "kept after one absence");
        fx.pass_hashes(vec![(reversed, 100), (hash_of(0xa1), 100)], Some(801));
        assert!(
            fx.rows_of_text(&text).is_empty(),
            "the BCH row and the SLP row went together"
        );
        assert_eq!(fx.rows_of(0xa1).len(), 1);
    }

    #[test]
    fn a_changed_height_matches_the_hash_in_the_byte_order_of_the_listing() {
        mock_block_times();
        let (listed, reversed) = ordered_listing_hash();
        let text = ordered_text();

        // The reversed hash at another height is another transaction: the stored rows keep their height.
        let mut fx = Fixture::new(ordered_rows);
        fx.pass_hashes(vec![(reversed, 95)], None);
        assert!(fx.rows_of_text(&text).iter().all(|row| row.block_height == 100));

        // The listed hash at another height moves both rows, with that block's time.
        fx.pass_hashes(vec![(listed, 95)], None);
        let rows = fx.rows_of_text(&text);
        assert_eq!(rows.len(), 2);
        assert!(rows
            .iter()
            .all(|row| row.block_height == 95 && row.timestamp == 1_700_000_095));
    }
}
