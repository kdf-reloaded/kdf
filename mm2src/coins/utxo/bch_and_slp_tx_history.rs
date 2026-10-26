/// This module is named bch_and_slp_tx_history temporary. We will most likely use the same approach for every
/// supported UTXO coin.
use super::RequestTxHistoryResult;
use crate::my_tx_history_v2::TxHistoryStorage;
use crate::utxo::bch::BchCoin;
use crate::utxo::utxo_common;
use crate::utxo::UtxoStandardOps;
use crate::{BlockHeightAndTime, HistorySyncState, MarketCoinOps};
use async_trait::async_trait;
use common::executor::Timer;
use common::log::{error, info};
use common::mm_number::BigDecimal;
use common::state_machine::prelude::*;
use futures::compat::Future01CompatExt;
use mm2_core::mm_ctx::MmArc;
use mm2_metrics::MetricsArc;
use rpc::v1::types::H256 as H256Json;
use std::str::FromStr;

struct BchAndSlpHistoryCtx<Storage: TxHistoryStorage> {
    coin: BchCoin,
    mm_ctx: MmArc,
    storage: Storage,
    metrics: MetricsArc,
    current_balance: BigDecimal,
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
            RequestTxHistoryResult::Ok(all_tx_ids_with_height) => {
                let in_storage = match ctx.storage.unique_tx_hashes_num_in_history(ctx.coin.ticker()).await {
                    Ok(num) => num,
                    Err(e) => return Self::change_state(Stopped::storage_error(e)),
                };
                if all_tx_ids_with_height.len() > in_storage {
                    let txes_left = all_tx_ids_with_height.len() - in_storage;
                    *ctx.coin.as_ref().history_sync_state.lock().unwrap() =
                        HistorySyncState::InProgress(json!({ "transactions_left": txes_left }));
                }

                Self::change_state(UpdatingUnconfirmedTxes::new(all_tx_ids_with_height))
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
            Timer::sleep(30.).await;
            match ctx.storage.history_contains_unconfirmed_txes(ctx.coin.ticker()).await {
                Ok(contains) => {
                    if contains {
                        return Self::change_state(FetchingTxHashes::new());
                    }
                },
                Err(e) => return Self::change_state(Stopped::storage_error(e)),
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
fn listed_height(all_tx_ids_with_height: &[(H256Json, u64)], tx_hash: &str) -> Result<Option<u64>, String> {
    let tx_hash = H256Json::from_str(tx_hash).map_err(|e| e.to_string())?;
    Ok(all_tx_ids_with_height
        .iter()
        .find(|(hash, _)| *hash == tx_hash)
        .map(|(_, height)| *height))
}

// States have to be generic over storage type because BchAndSlpHistoryCtx is generic over it
struct UpdatingUnconfirmedTxes<T> {
    phantom: std::marker::PhantomData<T>,
    all_tx_ids_with_height: Vec<(H256Json, u64)>,
}

impl<T> UpdatingUnconfirmedTxes<T> {
    fn new(all_tx_ids_with_height: Vec<(H256Json, u64)>) -> Self {
        UpdatingUnconfirmedTxes {
            phantom: Default::default(),
            all_tx_ids_with_height,
        }
    }
}

impl<T> TransitionFrom<FetchingTxHashes<T>> for UpdatingUnconfirmedTxes<T> {}

#[async_trait]
impl<T: TxHistoryStorage> State for UpdatingUnconfirmedTxes<T> {
    type Ctx = BchAndSlpHistoryCtx<T>;
    type Result = ();

    async fn on_changed(self: Box<Self>, ctx: &mut BchAndSlpHistoryCtx<T>) -> StateResult<BchAndSlpHistoryCtx<T>, ()> {
        match ctx.storage.get_unconfirmed_txes_from_history(ctx.coin.ticker()).await {
            Ok(unconfirmed) => {
                for mut tx in unconfirmed {
                    let found = match listed_height(&self.all_tx_ids_with_height, &tx.tx_hash) {
                        Ok(found) => found,
                        Err(e) => {
                            error!(
                                "Skipping the stored {} row with invalid tx_hash '{}': {}",
                                ctx.coin.ticker(),
                                tx.tx_hash,
                                e
                            );
                            continue;
                        },
                    };
                    match found {
                        Some(height) => {
                            if height > 0 {
                                match ctx.coin.get_block_timestamp(height).await {
                                    Ok(time) => tx.timestamp = time,
                                    Err(_) => return Self::change_state(OnIoErrorCooldown::new()),
                                };
                                tx.block_height = height;
                                if let Err(e) = ctx.storage.update_tx_in_history(ctx.coin.ticker(), &tx).await {
                                    return Self::change_state(Stopped::storage_error(e));
                                }
                            }
                        },
                        None => {
                            // This can potentially happen when unconfirmed tx is removed from mempool for some reason.
                            // We should remove it from storage too.
                            if let Err(e) = ctx
                                .storage
                                .remove_tx_from_history(ctx.coin.ticker(), &tx.internal_id)
                                .await
                            {
                                return Self::change_state(Stopped::storage_error(e));
                            }
                        },
                    }
                }
                Self::change_state(FetchingTransactionsData::new(self.all_tx_ids_with_height))
            },
            Err(e) => Self::change_state(Stopped::storage_error(e)),
        }
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
    let ctx = BchAndSlpHistoryCtx {
        coin,
        mm_ctx,
        storage,
        metrics,
        current_balance,
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
        };
        let state = FetchingTransactionsData::<SqliteTxHistoryStorage>::new(vec![(malformed_hash, 0), (valid_hash, 0)]);
        let _next = block_on(Box::new(state).on_changed(&mut ctx));

        let has =
            |hash: H256Json| block_on(ctx.storage.history_has_tx_hash(&ticker, &format!("{:02x}", hash))).unwrap();
        assert!(has(valid_hash), "the rest of the pass is stored");
        assert!(!has(malformed_hash), "the malformed transaction is not written");
    }
}
