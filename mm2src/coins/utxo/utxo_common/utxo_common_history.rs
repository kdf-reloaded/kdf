// utxo_common_history — transaction history processing, KMD rewards

use super::*;
use crate::history_absence::{removed_rows_line, AbsenceMarks, ChainChangeWatch};
use crate::UnreadableTransaction;
use std::collections::HashSet;

lazy_static! {
    pub static ref HISTORY_TOO_LARGE_ERROR: Json = json!({
        "code": 1,
        "message": "history too large"
    });
}

pub const HISTORY_TOO_LARGE_ERR_CODE: i64 = -1;

/// The order in which a legacy (file-backed) history is persisted and served: unconfirmed rows
/// (`block_height` 0) first, then confirmed rows by `block_height` descending, and rows of equal
/// height by `internal_id` ascending, compared bytewise (CRD ch.38 R38.5A.1). Over distinct
/// `internal_id` values this is a total order, so the result does not depend on the order in which
/// the rows were collected. Shared by the UTXO and the QRC20 history writers (R38.5A.8).
pub(crate) fn legacy_history_order(a: &TransactionDetails, b: &TransactionDetails) -> Ordering {
    let a_confirmed = a.block_height != 0;
    let b_confirmed = b.block_height != 0;
    a_confirmed
        .cmp(&b_confirmed)
        .then_with(|| b.block_height.cmp(&a.block_height))
        .then_with(|| a.internal_id.0.cmp(&b.internal_id.0))
}

/// Keys the loaded history rows by their transaction hash (R38.5A.4: one row per hash, and for a
/// UTXO coin `internal_id` is that hash). A stored history that holds several rows for one hash
/// collapses to one row here; a row whose `tx_hash` is not a valid hash is dropped and re-fetched
/// from the backend as a new transaction.
fn history_map_by_hash(history: Vec<TransactionDetails>) -> HashMap<H256Json, TransactionDetails> {
    history
        .into_iter()
        .filter_map(|tx| match H256Json::from_str(&tx.tx_hash) {
            Ok(hash) => Some((hash, tx)),
            Err(e) => {
                error!(
                    "Dropping a stored history row with invalid tx_hash '{}': {}",
                    tx.tx_hash, e
                );
                None
            },
        })
        .collect()
}

/// The rows of the working set in the persisted order (R38.5A.3: every write is sorted first).
fn sorted_rows_to_write(history_map: &HashMap<H256Json, TransactionDetails>) -> Vec<TransactionDetails> {
    let mut to_write: Vec<TransactionDetails> = history_map.values().cloned().collect();
    to_write.sort_unstable_by(legacy_history_order);
    to_write
}

/// Applies one qualifying listing, taken at `tip`, to the absence marks and removes from `history_map` the rows whose
/// transaction the listing lacks for the second time (CRD ch.38 §38.5C, D38.5C.1 status note). Returns the number of
/// removed rows.
fn remove_unlisted_rows(
    absence: &mut AbsenceMarks,
    history_map: &mut HashMap<H256Json, TransactionDetails>,
    tip: u64,
    tx_ids: &[(H256Json, u64)],
) -> usize {
    let listed: HashSet<H256Json> = tx_ids.iter().map(|(hash, _)| *hash).collect();
    let to_remove = absence.judge(tip, &listed, history_map.keys().copied());
    for hash in &to_remove {
        history_map.remove(hash);
    }
    to_remove.len()
}

/// Whether the legacy collector's need-to-run test is true: a stored row needs an update, a row carries an absence
/// mark so that the second listing does not wait for a balance change (the balance part is the loop's), or a
/// chain-changed event of the coin's client came and no pass has run since (`chain_changed`, CRD ch.38 §38.5C
/// amendment of 2026-11-18, item 3).
fn history_needs_pass(
    history_map: &HashMap<H256Json, TransactionDetails>,
    absence: &AbsenceMarks,
    chain_changed: bool,
) -> bool {
    chain_changed || history_map.iter().any(|(_, tx)| tx.should_update()) || absence.any()
}

/// What a listing says about the height of a stored row (CRD ch.38 D38.5C.2 status note, item 1).
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum HeightChange {
    /// Nothing to change: the heights agree, or the stored row is itself unconfirmed (R38.5A.5 fills those in).
    Same,
    /// The stored height is positive and the listing reports another positive height.
    Moved,
    /// The stored height is positive and the listing reports the transaction as unconfirmed.
    Unconfirmed,
}

/// Compares a stored `block_height` with the `listed` one (0 for an unconfirmed transaction). The all-ones value that
/// old histories hold for an unconfirmed row counts as unconfirmed.
pub(crate) fn listed_height_change(stored: u64, listed: u64) -> HeightChange {
    if stored == 0 || stored == u64::MAX {
        HeightChange::Same
    } else if listed == 0 {
        HeightChange::Unconfirmed
    } else if listed != stored {
        HeightChange::Moved
    } else {
        HeightChange::Same
    }
}

/// Keys a freshly loaded history (see [`history_map_by_hash`]) and, when keying collapsed or dropped
/// rows, returns the sorted rows that replace the stored file. The legacy `my_tx_history` serves the
/// stored file, and the loop writes it only when a transaction is new or updated, so a history that
/// an older key left doubled on disk would otherwise stay doubled until the next new transaction
/// (R38.5A.4). Nothing to collapse: no rows to write.
fn load_history_working_set(
    history: Vec<TransactionDetails>,
) -> (HashMap<H256Json, TransactionDetails>, Option<Vec<TransactionDetails>>) {
    let loaded_len = history.len();
    let history_map = history_map_by_hash(history);
    let healed = (history_map.len() < loaded_len).then(|| sorted_rows_to_write(&history_map));
    (history_map, healed)
}

/// At most one summary line per this long while the same failure repeats.
const FAILURE_SUMMARY_INTERVAL_MS: u64 = 10 * 60 * 1000;
/// The retry delay after the first failure, and the floor of the loop's poll rate.
const FAILURE_FIRST_DELAY_SECS: u64 = 10;
/// The retry delay after the second failure; it doubles on each further failure.
const FAILURE_SECOND_DELAY_SECS: u64 = 60;
/// The retry delay never exceeds this.
const FAILURE_MAX_DELAY_SECS: u64 = 300;

/// What a failed attempt asks the caller to log.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum FailureReport {
    /// The first failure, or a failure whose text differs from the previous one: log it once.
    Warn,
    /// The same text again and a summary is due: `repeats` failures since the last line was logged,
    /// this one included.
    Summary { repeats: u32 },
    /// The same text again and no summary is due yet: log nothing.
    Silent,
}

/// Removes the leading `file:line] ` location markers that `ERRL!` prepends, so that two failures
/// of the same cause compare equal wherever they were raised.
fn strip_location_prefix(mut text: &str) -> &str {
    while let Some((head, rest)) = text.split_once("] ") {
        let is_location = head.rsplit_once(':').is_some_and(|(file, line)| {
            !file.is_empty()
                && !file.contains(char::is_whitespace)
                && !line.is_empty()
                && line.bytes().all(|b| b.is_ascii_digit())
        });
        if !is_location {
            break;
        }
        text = rest;
    }
    text
}

/// The key two failures are compared by: the text without its location prefix, with every run of
/// ASCII digits replaced by one `#`. An error text can carry values that change on each attempt
/// (a verification time, "N seconds ago"), and such a repeat is the same failure.
///
/// A different server address that differs only in digits compares equal. That is acceptable: the
/// source and the ticker are fixed per throttle, and the text still differs in its words when the
/// cause does.
fn failure_key(text: &str) -> String {
    let mut key = String::with_capacity(text.len());
    let mut in_digits = false;
    for c in strip_location_prefix(text).chars() {
        if c.is_ascii_digit() {
            if !in_digits {
                key.push('#');
            }
            in_digits = true;
        } else {
            key.push(c);
            in_digits = false;
        }
    }
    key
}

/// Throttles the log lines and the retry delay of one failing source of the history loop (the
/// balance read, or the history request). The loop holds one per source, so they never share state.
///
/// While a request keeps failing: the first failure and each change of the error text log one
/// line, a repeat of the same text logs nothing except one summary line per
/// [`FAILURE_SUMMARY_INTERVAL_MS`], and the retry delay grows 10, 60, 120, 240, 300, 300, ... seconds.
/// The first success logs one line and resets everything. The clock is an argument, so the
/// throttle is testable without sleeping.
#[derive(Debug, Default)]
pub(crate) struct FailureThrottle {
    /// The [`failure_key`] of the previous failure.
    last_key: Option<String>,
    /// Failures in a row since the last success.
    consecutive: u32,
    /// Failures of the same text since the last line was logged.
    unreported: u32,
    /// When the last first-failure or summary line was logged, in ms.
    last_logged_ms: u64,
}

impl FailureThrottle {
    /// Records a failure at `now_ms` and says what to log. Failures are compared by [`failure_key`].
    pub(crate) fn on_failure(&mut self, error: &str, now_ms: u64) -> FailureReport {
        let key = failure_key(error);
        self.consecutive = self.consecutive.saturating_add(1);
        if self.last_key.as_deref() != Some(key.as_str()) {
            self.last_key = Some(key);
            self.unreported = 0;
            self.last_logged_ms = now_ms;
            return FailureReport::Warn;
        }
        self.unreported = self.unreported.saturating_add(1);
        if now_ms.saturating_sub(self.last_logged_ms) >= FAILURE_SUMMARY_INTERVAL_MS {
            let repeats = self.unreported;
            self.unreported = 0;
            self.last_logged_ms = now_ms;
            FailureReport::Summary { repeats }
        } else {
            FailureReport::Silent
        }
    }

    /// Records a success. Returns the number of failed attempts that preceded it when there were
    /// any, and resets the state.
    pub(crate) fn on_success(&mut self) -> Option<u32> {
        let failed_attempts = self.consecutive;
        *self = Self::default();
        (failed_attempts > 0).then_some(failed_attempts)
    }

    /// Resets the state quietly: [`Self::on_success`] without a line to log for the caller.
    pub(crate) fn reset(&mut self) { *self = Self::default(); }

    /// The seconds to wait before the next attempt: 10 after the first failure, 60 after the
    /// second, then doubling up to 300; 10 when nothing has failed.
    pub(crate) fn delay_secs(&self) -> u64 {
        match self.consecutive {
            0 | 1 => FAILURE_FIRST_DELAY_SECS,
            n => FAILURE_SECOND_DELAY_SECS
                .saturating_mul(1u64 << (n - 2).min(8))
                .min(FAILURE_MAX_DELAY_SECS),
        }
    }

    /// Records a failure of `what` for the coin `ticker` and logs what the throttle asks for, to the
    /// ctx log tail (the channel the loop has always used).
    pub(crate) fn record_failure(&mut self, ctx: &MmArc, ticker: &str, what: &str, error: &str) {
        let error = strip_location_prefix(error);
        match self.on_failure(error, now_ms()) {
            FailureReport::Warn => log_tag!(
                ctx,
                "",
                "tx_history",
                "coin" => ticker;
                fmt = "tx_history {}: {} failed: {}", ticker, what, error
            ),
            FailureReport::Summary { repeats } => log_tag!(
                ctx,
                "",
                "tx_history",
                "coin" => ticker;
                fmt = "tx_history {}: {} failed {} more times since the last line: {}", ticker, what, repeats, error
            ),
            FailureReport::Silent => (),
        }
    }

    /// Records a success of `what` for the coin `ticker` and logs the recovery to the ctx log tail
    /// when it follows failures.
    pub(crate) fn record_success(&mut self, ctx: &MmArc, ticker: &str, what: &str) {
        if let Some(attempts) = self.on_success() {
            log_tag!(
                ctx,
                "",
                "tx_history",
                "coin" => ticker;
                fmt = "tx_history {}: {} recovered after {} failed attempts", ticker, what, attempts
            );
        }
    }
}

#[allow(clippy::cognitive_complexity)]
pub async fn process_history_loop<T>(coin: T, ctx: MmArc)
where
    T: UtxoStandardOps + UtxoCommonOps + MmCoin + MarketCoinOps,
{
    let mut my_balance: Option<CoinBalance> = None;
    let history = match coin.load_history_from_file(&ctx).compat().await {
        Ok(history) => history,
        Err(e) => {
            log_tag!(
                ctx,
                "",
                "tx_history",
                "coin" => coin.as_ref().conf.ticker;
                fmt = "Error {} on 'load_history_from_file', stop the history loop", e
            );
            return;
        },
    };
    let (mut history_map, healed_rows) = load_history_working_set(history);
    if let Some(to_write) = healed_rows {
        if let Err(e) = coin.save_history_to_file(&ctx, to_write).compat().await {
            log_tag!(
                ctx,
                "",
                "tx_history",
                "coin" => coin.as_ref().conf.ticker;
                fmt = "Error {} on 'save_history_to_file', stop the history loop", e
            );
            return;
        };
    }

    // Watch this coin's address so a confirmed transaction shortens the wait
    // below instead of costing a full idle interval. Electrum-backed coins only;
    // a native-RPC coin has nothing to subscribe to and keeps pure polling.
    let (wake_tx, mut wake_rx) = futures::channel::mpsc::unbounded::<String>();
    let watched_script_hash = match (&coin.as_ref().rpc_client, coin.as_ref().derivation_method.iguana()) {
        (UtxoRpcClientEnum::Electrum(electrum), Some(address)) => {
            let script_hash = hex::encode(electrum_script_hash(&output_script(&address, ScriptType::P2PKH)));
            match electrum.subscribe_scripthash(script_hash.clone()).await {
                Ok(()) => {
                    crate::utxo::rpc_clients::watch_scripthash(script_hash.clone(), wake_tx);
                    Some(script_hash)
                },
                // Losing the subscription only costs latency: the idle wait
                // below still expires on its own.
                Err(e) => {
                    common::log::debug!(
                        "tx_history for {}: script-hash subscription unavailable ({}); polling only",
                        coin.as_ref().conf.ticker,
                        e
                    );
                    None
                },
            }
        },
        _ => None,
    };

    let mut success_iteration = 0i32;
    // The absence rule (CRD ch.38 §38.5C): memory only, so a restart starts counting again.
    let mut absence = AbsenceMarks::default();
    // The chain-changed events of the coin's client: one makes the next pass run at once (amendment of 2026-11-18).
    let mut chain_changes = ChainChangeWatch::of(&coin.as_ref().rpc_client);
    let mut chain_changed = false;
    // A repeating failure of one source is logged once and then summarized, and a failing history
    // request backs the loop off (CRD ch.38 R38.6.5 decision 8). The two sources are independent.
    let mut balance_failures = FailureThrottle::default();
    let mut history_failures = FailureThrottle::default();
    loop {
        if ctx.is_stopping() {
            break;
        };
        {
            let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();
            let coins = coins_ctx.coins.lock().await;
            if !coins.contains_key(&coin.as_ref().conf.ticker) {
                log_tag!(ctx, "", "tx_history", "coin" => coin.as_ref().conf.ticker; fmt = "Loop stopped");
                if let Some(ref script_hash) = watched_script_hash {
                    wake_rx.close();
                    crate::utxo::rpc_clients::unwatch_scripthash(script_hash);
                }
                break;
            };
        }

        let actual_balance = match coin.my_balance().compat().await {
            Ok(actual_balance) => {
                balance_failures.record_success(&ctx, &coin.as_ref().conf.ticker, "balance read");
                Some(actual_balance)
            },
            Err(err) => {
                // The inner error's own text: the `{:?}` of the traced wrapper carries code locations.
                balance_failures.record_failure(
                    &ctx,
                    &coin.as_ref().conf.ticker,
                    "balance read",
                    &err.get_inner().to_string(),
                );
                None
            },
        };

        chain_changed |= chain_changes.take();
        let need_update = history_needs_pass(&history_map, &absence, chain_changed);
        match (&my_balance, &actual_balance) {
            (Some(prev_balance), Some(actual_balance)) if prev_balance == actual_balance && !need_update => {
                // Balance unchanged, so there is nothing to reload yet. Wait for
                // the idle interval or for a server to report that the watched
                // address moved, whichever comes first; the balance is re-read
                // at the top of the loop either way, so the notification only
                // decides *when* to look, never *what* is true.
                //
                // The balance read worked and nothing is pending, so the server answers again: end
                // a history backoff quietly (the balance source logs its own recovery). While the
                // balance differs or an update is pending this branch is not taken, so a server that
                // answers the balance and fails the history keeps its backoff.
                history_failures.reset();
                let idle = Timer::sleep(30.);
                futures::pin_mut!(idle);
                let woken = futures::StreamExt::next(&mut wake_rx);
                futures::pin_mut!(woken);
                let chain = chain_changes.wait();
                futures::pin_mut!(chain);
                let either = futures::future::select(woken, chain);
                if let futures::future::Either::Right(_) = futures::future::select(idle, either).await {
                    // Collapse a burst into one pass.
                    while wake_rx.try_recv().is_ok() {}
                }
                continue;
            },
            _ => (),
        }

        let metrics = ctx.metrics.clone();
        let tx_ids = match coin.request_tx_history(metrics).await {
            RequestTxHistoryResult::Ok(tx_ids, qualifying_tip) => {
                // Only a successful history request resets the backoff. A successful balance read
                // does not: a server can answer the balance and still fail the history.
                history_failures.record_success(&ctx, &coin.as_ref().conf.ticker, "history request");
                // The pass the event asked for has its listing.
                chain_changed = false;
                // The removal comes first, before any detail request of the pass (R38.5C.1 item 2).
                if let Some(tip) = qualifying_tip {
                    let removed = remove_unlisted_rows(&mut absence, &mut history_map, tip, &tx_ids);
                    if removed > 0 {
                        info!("{}", removed_rows_line(&coin.as_ref().conf.ticker, removed));
                        if let Err(e) = coin
                            .save_history_to_file(&ctx, sorted_rows_to_write(&history_map))
                            .compat()
                            .await
                        {
                            log_tag!(
                                ctx,
                                "",
                                "tx_history",
                                "coin" => coin.as_ref().conf.ticker;
                                fmt = "Error {} on 'save_history_to_file', stop the history loop", e
                            );
                            return;
                        };
                    }
                }
                tx_ids
            },
            RequestTxHistoryResult::Retry { error } => {
                history_failures.record_failure(&ctx, &coin.as_ref().conf.ticker, "history request", &error);
                // 10 s after the first failure, then 60, 120, 240, 300 s. This sleep does not wake on a
                // script-hash notification: the retry re-reads everything at its end anyway.
                Timer::sleep(history_failures.delay_secs() as f64).await;
                continue;
            },
            RequestTxHistoryResult::HistoryTooLarge => {
                log_tag!(
                    ctx,
                    "",
                    "tx_history",
                    "coin" => coin.as_ref().conf.ticker;
                    fmt = "Got `history too large`, stopping further attempts to retrieve it"
                );
                *coin.as_ref().history_sync_state.lock().unwrap() = HistorySyncState::Error(json!({
                    "code": HISTORY_TOO_LARGE_ERR_CODE,
                    "message": "Got `history too large` error from Electrum server. History is not available",
                }));
                break;
            },
            RequestTxHistoryResult::CriticalError(e) => {
                log_tag!(
                    ctx,
                    "",
                    "tx_history",
                    "coin" => coin.as_ref().conf.ticker;
                    fmt = "{}, stopping futher attempts to retreive it", e
                );
                break;
            },
        };
        let mut transactions_left = if tx_ids.len() > history_map.len() {
            *coin.as_ref().history_sync_state.lock().unwrap() = HistorySyncState::InProgress(json!({
                "transactions_left": tx_ids.len() - history_map.len()
            }));
            tx_ids.len() - history_map.len()
        } else {
            *coin.as_ref().history_sync_state.lock().unwrap() = HistorySyncState::InProgress(json!({
                "transactions_left": 0
            }));
            0
        };

        // This is the cache of the already requested transactions.
        let mut input_transactions = HistoryUtxoTxMap::default();
        // The transactions this pass skipped because they cannot be read. Rebuilt on every pass and kept in memory
        // only, so a transaction that reads on a later pass drops out of the report (CRD ch.39 D39.11.4V.1).
        let mut unreadable: Vec<UnreadableTransaction> = Vec::new();
        for (txid, height) in tx_ids {
            let mut updated = false;
            match history_map.entry(txid) {
                Entry::Vacant(e) => {
                    mm_counter!(ctx.metrics, "tx.history.request.count", 1, "coin" => coin.as_ref().conf.ticker.clone(), "method" => "tx_detail_by_hash");

                    match coin.tx_details_by_hash(&txid.0, &mut input_transactions).await {
                        Ok(mut tx_details) => {
                            mm_counter!(ctx.metrics, "tx.history.response.count", 1, "coin" => coin.as_ref().conf.ticker.clone(), "method" => "tx_detail_by_hash");

                            if tx_details.block_height == 0 && height > 0 {
                                tx_details.block_height = height;
                            }

                            let tx_history_record = tx_details.clone();
                            e.insert(tx_details);
                            crate::tx_history_streaming::publish_tx_history_records(
                                &ctx,
                                &coin.as_ref().conf.ticker,
                                vec![tx_history_record],
                            );
                            if transactions_left > 0 {
                                transactions_left -= 1;
                                *coin.as_ref().history_sync_state.lock().unwrap() =
                                    HistorySyncState::utxo_in_progress(transactions_left, &unreadable);
                            }
                            updated = true;
                        },
                        Err(e) => {
                            debug!(
                                "Full error on getting the details of {:?} for {}: {:?}",
                                txid,
                                coin.as_ref().conf.ticker,
                                e
                            );
                            log_tag!(
                                ctx,
                                "",
                                "tx_history",
                                "coin" => coin.as_ref().conf.ticker;
                                fmt = "Error {} on getting the details of {:?}, skipping the tx", e, txid
                            );
                            // A transaction that cannot be read is reported in the sync status. Any other failure
                            // (a transport, timeout or backend error) is skipped and tried again on the next pass,
                            // and is not reported.
                            if let TxDetailsByHashError::Unreadable(reason) = &e {
                                let tx_hash = format!("{:02x}", txid);
                                if !unreadable.iter().any(|skipped| skipped.tx_hash == tx_hash) {
                                    unreadable.push(UnreadableTransaction {
                                        tx_hash,
                                        error: reason.clone(),
                                    });
                                    *coin.as_ref().history_sync_state.lock().unwrap() =
                                        HistorySyncState::utxo_in_progress(transactions_left, &unreadable);
                                }
                            }
                        },
                    }
                },
                Entry::Occupied(mut e) => {
                    // update block height for previously unconfirmed transaction
                    if e.get().should_update_block_height() && height > 0 {
                        e.get_mut().block_height = height;
                        updated = true;
                    }
                    match listed_height_change(e.get().block_height, height) {
                        // The listing reports a confirmed row at another height: take the height over, with that
                        // block's time (CRD ch.38 D38.5C.2 status note, item 1). The time comes with the details.
                        HeightChange::Moved => {
                            mm_counter!(ctx.metrics, "tx.history.request.count", 1, "coin" => coin.as_ref().conf.ticker.clone(), "method" => "tx_detail_by_hash");
                            if let Ok(mut tx_details) = coin.tx_details_by_hash(&txid.0, &mut input_transactions).await
                            {
                                mm_counter!(ctx.metrics, "tx.history.response.count", 1, "coin" => coin.as_ref().conf.ticker.clone(), "method" => "tx_detail_by_hash");
                                tx_details.block_height = height;
                                e.insert(tx_details);
                                updated = true;
                            }
                        },
                        // The listing reports a confirmed row as unconfirmed: back to height 0.
                        HeightChange::Unconfirmed => {
                            e.get_mut().block_height = 0;
                            updated = true;
                        },
                        HeightChange::Same => (),
                    }
                    if e.get().should_update_timestamp() || e.get().firo_negative_fee() {
                        mm_counter!(ctx.metrics, "tx.history.request.count", 1, "coin" => coin.as_ref().conf.ticker.clone(), "method" => "tx_detail_by_hash");

                        if let Ok(tx_details) = coin.tx_details_by_hash(&txid.0, &mut input_transactions).await {
                            mm_counter!(ctx.metrics, "tx.history.response.count", 1, "coin" => coin.as_ref().conf.ticker.clone(), "method" => "tx_detail_by_hash");
                            // replace with new tx details in case we need to update any data
                            e.insert(tx_details);
                            updated = true;
                        }
                    }
                },
            }
            if updated {
                let to_write = sorted_rows_to_write(&history_map);
                if let Err(e) = coin.save_history_to_file(&ctx, to_write).compat().await {
                    log_tag!(
                        ctx,
                        "",
                        "tx_history",
                        "coin" => coin.as_ref().conf.ticker;
                        fmt = "Error {} on 'save_history_to_file', stop the history loop", e
                    );
                    return;
                };
            }
        }
        *coin.as_ref().history_sync_state.lock().unwrap() = HistorySyncState::utxo_finished(&unreadable);

        if success_iteration == 0 {
            log_tag!(
                ctx,
                "😅",
                "tx_history",
                "coin" => coin.as_ref().conf.ticker;
                fmt = "history has been loaded successfully"
            );
        }

        my_balance = actual_balance;
        success_iteration += 1;
        Timer::sleep(30.).await;
    }
}

pub async fn request_tx_history<T>(coin: &T, metrics: MetricsArc) -> RequestTxHistoryResult
where
    T: UtxoCommonOps + MmCoin + MarketCoinOps,
{
    // Resolve the primary address, supporting both Iguana and HD derivation.
    // For HD wallets this uses the root public-key-derived address (the same one used by
    // trade_preimage_sender_address). This covers the most common single-address HD use case;
    // full multi-address HD history scanning would require the v2 tx history path.
    let coin_fields = coin.as_ref();
    let my_address_obj: Address = match &coin_fields.derivation_method {
        DerivationMethod::Iguana(addr) => addr.clone(),
        DerivationMethod::HDWallet(hd_wallet) => {
            let pk = match my_public_key(coin_fields) {
                Ok(pk) => pk,
                Err(e) => return RequestTxHistoryResult::CriticalError(e.to_string()),
            };
            address_from_pubkey(
                pk,
                coin_fields.conf.pub_addr_prefix,
                coin_fields.conf.pub_t_addr_prefix,
                coin_fields.conf.checksum_type,
                coin_fields.conf.bech32_hrp.clone(),
                hd_wallet.address_format.clone(),
            )
        },
    };
    let my_address = match my_address_obj.display_address() {
        Ok(addr) => addr,
        Err(e) => return RequestTxHistoryResult::CriticalError(e),
    };

    let (tx_ids, qualifying_tip) = match &coin.as_ref().rpc_client {
        #[cfg(not(target_arch = "wasm32"))]
        UtxoRpcClientEnum::LightWalletD(_) => {
            return RequestTxHistoryResult::CriticalError(
                "transparent transaction history is not supported over lightwalletd".to_owned(),
            )
        },
        UtxoRpcClientEnum::Native(client) => {
            // The tip must belong to the listing: the block count is read before and after the listing, and a
            // listing during which it moved qualifies for nothing (CRD ch.38 §38.5C).
            let tip_before = client.get_block_count().compat().await.ok();
            let mut from = 0;
            let mut all_transactions = vec![];
            loop {
                mm_counter!(metrics, "tx.history.request.count", 1,
                    "coin" => coin.as_ref().conf.ticker.clone(), "client" => "native", "method" => "listtransactions");

                let transactions = match client.list_transactions(100, from).compat().await {
                    Ok(value) => value,
                    Err(e) => {
                        return RequestTxHistoryResult::Retry {
                            error: ERRL!("Error {} on list transactions", e),
                        };
                    },
                };

                mm_counter!(metrics, "tx.history.response.count", 1,
                    "coin" => coin.as_ref().conf.ticker.clone(), "client" => "native", "method" => "listtransactions");

                if transactions.is_empty() {
                    break;
                }
                from += 100;
                all_transactions.extend(transactions);
            }

            mm_counter!(metrics, "tx.history.response.total_length", all_transactions.len() as u64,
                "coin" => coin.as_ref().conf.ticker.clone(), "client" => "native", "method" => "listtransactions");

            // The daemon is the single server: no up-to-date or disagreement test. A tip that cannot be read, or
            // that differs before and after the listing, leaves the complete listing unjudged for the absence rule.
            let tip_after = client.get_block_count().compat().await.ok();
            let tip = tip_before.filter(|before| Some(*before) == tip_after);

            let tx_ids: Vec<(H256Json, u64)> = all_transactions
                .into_iter()
                .filter_map(|item| {
                    if item.address == my_address {
                        Some((item.txid, item.block_height()))
                    } else {
                        None
                    }
                })
                .collect();
            (tx_ids, tip)
        },
        UtxoRpcClientEnum::Electrum(client) => {
            let my_address = &my_address_obj;
            let script = output_script(my_address, ScriptType::P2PKH);
            let script_hash = electrum_script_hash(&script);

            mm_counter!(metrics, "tx.history.request.count", 1,
                "coin" => coin.as_ref().conf.ticker.clone(), "client" => "electrum", "method" => "blockchain.scripthash.get_history");

            let (electrum_history, tip) = match client
                .scripthash_get_history_for_absence(&hex::encode(script_hash))
                .await
            {
                Ok(answer) => answer,
                Err(e) => match &e.error {
                    JsonRpcErrorType::InvalidRequest(e)
                    | JsonRpcErrorType::Transport(e)
                    | JsonRpcErrorType::Parse(_, e) => {
                        return RequestTxHistoryResult::Retry {
                            error: ERRL!("Error {} on scripthash_get_history", e),
                        };
                    },
                    JsonRpcErrorType::Response(_addr, err) => {
                        if HISTORY_TOO_LARGE_ERROR.eq(err) {
                            return RequestTxHistoryResult::HistoryTooLarge;
                        } else {
                            return RequestTxHistoryResult::Retry {
                                error: ERRL!("Error {:?} on scripthash_get_history", e),
                            };
                        }
                    },
                },
            };
            mm_counter!(metrics, "tx.history.response.count", 1,
                "coin" => coin.as_ref().conf.ticker.clone(), "client" => "electrum", "method" => "blockchain.scripthash.get_history");

            mm_counter!(metrics, "tx.history.response.total_length", electrum_history.len() as u64,
                "coin" => coin.as_ref().conf.ticker.clone(), "client" => "electrum", "method" => "blockchain.scripthash.get_history");

            // electrum returns the most recent transactions in the end but we need to
            // process them first so rev is required
            let tx_ids: Vec<(H256Json, u64)> = electrum_history
                .into_iter()
                .rev()
                .map(|item| {
                    let height = if item.height < 0 { 0 } else { item.height as u64 };
                    (item.tx_hash, height)
                })
                .collect();
            (tx_ids, tip)
        },
    };
    RequestTxHistoryResult::Ok(tx_ids, qualifying_tip)
}

/// The output of `prev_tx` that an input with the previous-output `index` spends. A malformed
/// transaction can name an index past the end of the output list; that is an error for the one
/// transaction, never a panic (CRD ch.38 R38.5A.9).
pub(crate) fn prev_output_at<'a>(
    prev_tx: &'a UtxoTx,
    prev_tx_hash: &H256Json,
    index: u32,
) -> Result<&'a TransactionOutput, String> {
    usize::try_from(index)
        .ok()
        .and_then(|i| prev_tx.outputs.get(i))
        .ok_or_else(|| {
            format!(
                "output {} of transaction {:02x} does not exist, it has {} outputs",
                index,
                prev_tx_hash,
                prev_tx.outputs.len()
            )
        })
}

/// Why [`tx_details_by_hash`] could not build the details of one transaction.
#[derive(Debug)]
pub enum TxDetailsByHashError {
    /// The transaction, or the transaction of one of its inputs, cannot be read: it does not parse, or an input
    /// spends an output that does not exist (CRD ch.38 R38.5A.9). The text is a short fixed reason with the parser's
    /// error kind, never a payload. The history collector skips the transaction, reports it in the sync status
    /// (CRD ch.39 D39.11.4V.1) and tries it again on a later pass.
    Unreadable(String),
    /// Any other failure: a transport, timeout or backend error, an address that cannot be derived, an amount that
    /// does not convert. The history collector skips the transaction and tries it again on the next pass; the sync
    /// status does not report it.
    Other(String),
}

impl std::fmt::Display for TxDetailsByHashError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TxDetailsByHashError::Unreadable(text) | TxDetailsByHashError::Other(text) => f.write_str(text),
        }
    }
}

/// The reason text for bytes that do not parse as a transaction: fixed wording plus the parser's error kind. The
/// text of a parser error of the `Custom` kind is taken over cut to 80 characters (it names a version number or a
/// fixed sentence); the bytes themselves are never part of it.
pub(crate) fn parse_failure_reason(what: &str, error: &serialization::Error) -> String {
    let kind: String = match error {
        serialization::Error::MalformedData => "malformed data".to_owned(),
        serialization::Error::UnexpectedEnd => "unexpected end of data".to_owned(),
        serialization::Error::UnreadData => "unread data after the transaction".to_owned(),
        serialization::Error::Custom(text) => text.chars().take(80).collect(),
    };
    format!("{} cannot be parsed: {}", what, kind)
}

/// Why a previous transaction could not be had from the map or from the backend.
enum PrevTxError {
    /// The backend request failed.
    Rpc(MmError<UtxoRpcError>),
    /// The backend answered bytes that do not parse as a transaction.
    Unparsed(serialization::Error),
}

pub async fn tx_details_by_hash<T: UtxoCommonOps>(
    coin: &T,
    hash: &[u8],
    input_transactions: &mut HistoryUtxoTxMap,
) -> Result<TransactionDetails, TxDetailsByHashError> {
    // Like `try_s!`, with the failure classed as not unreadable.
    macro_rules! try_other {
        ($e:expr) => {
            match $e {
                Ok(ok) => ok,
                Err(err) => return Err(TxDetailsByHashError::Other(ERRL!("{}", err))),
            }
        };
    }

    let ticker = &coin.as_ref().conf.ticker;
    let hash = H256Json::from(hash);
    let verbose_tx = try_other!(coin.as_ref().rpc_client.get_verbose_transaction(&hash).compat().await);
    let mut tx: UtxoTx = match deserialize(verbose_tx.hex.as_slice()) {
        Ok(tx) => tx,
        Err(e) => {
            return Err(TxDetailsByHashError::Unreadable(parse_failure_reason(
                "the transaction",
                &e,
            )))
        },
    };
    tx.tx_hash_algo = coin.as_ref().tx_hash_algo;
    // `derivation_method.iguana_or_err()` only ever succeeds for a
    // single-address (Iguana) account -- it errors outright for an
    // HD-wallet-activated one, which made every transaction in an
    // HD-wallet coin's history fail to detail (even though
    // request_tx_history, above, already correctly finds their txids for
    // HD wallets too). Mirror request_tx_history's own resolution instead
    // (root/primary HD address; see its comment for the single-address
    // HD scope note).
    let coin_fields = coin.as_ref();
    let my_address_obj: Address = match &coin_fields.derivation_method {
        DerivationMethod::Iguana(addr) => addr.clone(),
        DerivationMethod::HDWallet(hd_wallet) => {
            let pk = try_other!(my_public_key(coin_fields));
            address_from_pubkey(
                pk,
                coin_fields.conf.pub_addr_prefix,
                coin_fields.conf.pub_t_addr_prefix,
                coin_fields.conf.checksum_type,
                coin_fields.conf.bech32_hrp.clone(),
                hd_wallet.address_format.clone(),
            )
        },
    };
    let my_address = &my_address_obj;

    input_transactions.insert(hash, HistoryUtxoTx {
        tx: tx.clone(),
        height: verbose_tx.height,
    });

    let mut input_amount = 0;
    let mut output_amount = 0;
    let mut from_addresses = Vec::new();
    let mut to_addresses = Vec::new();
    let mut spent_by_me = 0;
    let mut received_by_me = 0;

    for input in tx.inputs.iter() {
        // input transaction is zero if the tx is the coinbase transaction
        if input.previous_output.hash.is_zero() {
            continue;
        }

        let prev_tx_hash: H256Json = input.previous_output.hash.reversed().into();
        let prev_tx = match verbose_transaction_or_unparsed(coin, prev_tx_hash, input_transactions).await {
            Ok(prev_tx) => prev_tx,
            Err(PrevTxError::Rpc(e)) => return Err(TxDetailsByHashError::Other(ERRL!("{}", e))),
            Err(PrevTxError::Unparsed(e)) => {
                return Err(TxDetailsByHashError::Unreadable(parse_failure_reason(
                    "an input transaction",
                    &e,
                )))
            },
        };
        let prev_tx = &mut prev_tx.tx;
        prev_tx.tx_hash_algo = coin.as_ref().tx_hash_algo;

        let prev_output = match prev_output_at(prev_tx, &prev_tx_hash, input.previous_output.index) {
            Ok(prev_output) => prev_output,
            Err(e) => {
                return Err(TxDetailsByHashError::Unreadable(format!(
                    "an input spends an output that does not exist: {}",
                    e
                )))
            },
        };
        let prev_tx_value = prev_output.value;
        input_amount += prev_tx_value;
        let from: Vec<Address> = try_other!(coin.addresses_from_script(&prev_output.script_pubkey.clone().into()));
        if from.contains(my_address) {
            spent_by_me += prev_tx_value;
        }
        from_addresses.extend(from.into_iter());
    }

    for output in tx.outputs.iter() {
        output_amount += output.value;
        let to = try_other!(coin.addresses_from_script(&output.script_pubkey.clone().into()));
        if to.contains(my_address) {
            received_by_me += output.value;
        }
        to_addresses.extend(to.into_iter());
    }

    // TODO uncomment this when `calc_interest_of_tx` works fine
    // let (fee, kmd_rewards) = if ticker == "KMD" {
    //     let kmd_rewards = try_s!(coin.calc_interest_of_tx(&tx, input_transactions).await);
    //     // `input_amount = output_amount + fee`, where `output_amount = actual_output_amount + kmd_rewards`,
    //     // so to calculate an actual transaction fee, we have to subtract the `kmd_rewards` from the total `output_amount`:
    //     // `fee = input_amount - actual_output_amount` or simplified `fee = input_amount - output_amount + kmd_rewards`
    //     let fee = input_amount as i64 - output_amount as i64 + kmd_rewards as i64;
    //
    //     let my_address = &coin.as_ref().my_address;
    //     let claimed_by_me = from_addresses.iter().all(|from| from == my_address) && to_addresses.contains(my_address);
    //     let kmd_rewards_details = KmdRewardsDetails {
    //         amount: big_decimal_from_sat_unsigned(kmd_rewards, coin.as_ref().decimals),
    //         claimed_by_me,
    //     };
    //     (
    //         big_decimal_from_sat(fee, coin.as_ref().decimals),
    //         Some(kmd_rewards_details),
    //     )
    // } else if input_amount == 0 {
    //     let fee = verbose_tx.vin.iter().fold(0., |cur, input| {
    //         let fee = match input {
    //             TransactionInputEnum::Lelantus(lelantus) => lelantus.n_fees,
    //             _ => 0.,
    //         };
    //         cur + fee
    //     });
    //     (fee.into(), None)
    // } else {
    //     let fee = input_amount as i64 - output_amount as i64;
    //     (big_decimal_from_sat(fee, coin.as_ref().decimals), None)
    // };

    let (fee, kmd_rewards) = if input_amount == 0 {
        let fee = verbose_tx.vin.iter().fold(0., |cur, input| {
            let fee = match input {
                TransactionInputEnum::Lelantus(lelantus) => lelantus.n_fees,
                _ => 0.,
            };
            cur + fee
        });
        (try_other!(fee.try_into()), None)
    } else {
        let fee = input_amount as i64 - output_amount as i64;
        (big_decimal_from_sat(fee, coin.as_ref().decimals), None)
    };

    // remove address duplicates in case several inputs were spent from same address
    // or several outputs are sent to same address
    let mut from_addresses: Vec<String> =
        try_other!(from_addresses.into_iter().map(|addr| addr.display_address()).collect());
    from_addresses.sort();
    from_addresses.dedup();
    let mut to_addresses: Vec<String> =
        try_other!(to_addresses.into_iter().map(|addr| addr.display_address()).collect());
    to_addresses.sort();
    to_addresses.dedup();

    let fee_details = UtxoFeeDetails {
        coin: Some(coin.as_ref().conf.ticker.clone()),
        amount: fee,
    };

    Ok(TransactionDetails {
        unsigned: None,
        tx_json: None,
        from: from_addresses,
        to: to_addresses,
        received_by_me: big_decimal_from_sat_unsigned(received_by_me, coin.as_ref().decimals),
        spent_by_me: big_decimal_from_sat_unsigned(spent_by_me, coin.as_ref().decimals),
        my_balance_change: big_decimal_from_sat(received_by_me as i64 - spent_by_me as i64, coin.as_ref().decimals),
        total_amount: big_decimal_from_sat_unsigned(input_amount, coin.as_ref().decimals),
        tx_hash: tx.hash().reversed().to_vec().to_tx_hash(),
        tx_hex: verbose_tx.hex,
        fee_details: Some(fee_details.into()),
        block_height: verbose_tx.height.unwrap_or(0),
        coin: ticker.clone(),
        internal_id: tx.hash().reversed().to_vec().into(),
        timestamp: verbose_tx.time.into(),
        kmd_rewards,
        transaction_type: Default::default(),
        memo: None,
    })
}

/// The transaction `tx_hash` from `utxo_tx_map`, or from the backend when the map does not hold it. A failure of the
/// backend request and bytes that do not parse are told apart, so that the history collector can report the latter
/// (CRD ch.39 D39.11.4V.1).
async fn verbose_transaction_or_unparsed<'b, T>(
    coin: &T,
    tx_hash: H256Json,
    utxo_tx_map: &'b mut HistoryUtxoTxMap,
) -> Result<&'b mut HistoryUtxoTx, PrevTxError>
where
    T: AsRef<UtxoCoinFields>,
{
    let tx = match utxo_tx_map.entry(tx_hash) {
        Entry::Vacant(e) => {
            let verbose = coin
                .as_ref()
                .rpc_client
                .get_verbose_transaction(&tx_hash)
                .compat()
                .await
                .map_err(PrevTxError::Rpc)?;
            let tx = HistoryUtxoTx {
                tx: deserialize(verbose.hex.as_slice()).map_err(PrevTxError::Unparsed)?,
                height: verbose.height,
            };
            e.insert(tx)
        },
        Entry::Occupied(e) => e.into_mut(),
    };
    Ok(tx)
}

pub async fn get_mut_verbose_transaction_from_map_or_rpc<'a, 'b, T>(
    coin: &'a T,
    tx_hash: H256Json,
    utxo_tx_map: &'b mut HistoryUtxoTxMap,
) -> UtxoRpcResult<&'b mut HistoryUtxoTx>
where
    T: AsRef<UtxoCoinFields>,
{
    match verbose_transaction_or_unparsed(coin, tx_hash, utxo_tx_map).await {
        Ok(tx) => Ok(tx),
        Err(PrevTxError::Rpc(e)) => Err(e),
        Err(PrevTxError::Unparsed(e)) => {
            MmError::err(UtxoRpcError::InvalidResponse(format!("{:?}, tx: {:?}", e, tx_hash)))
        },
    }
}

/// This function is used when the transaction details were calculated without considering the KMD rewards.
/// We know that [`TransactionDetails::fee`] was calculated by `fee = input_amount - output_amount`,
/// where `output_amount = actual_output_amount + kmd_rewards` or `actual_output_amount = output_amount - kmd_rewards`.
/// To calculate an actual fee amount, we have to replace `output_amount` with `actual_output_amount`:
/// `actual_fee = input_amount - actual_output_amount` or `actual_fee = input_amount - output_amount + kmd_rewards`.
/// Substitute [`TransactionDetails::fee`] to the last equation:
/// `actual_fee = TransactionDetails::fee + kmd_rewards`
pub async fn update_kmd_rewards<T>(
    coin: &T,
    tx_details: &mut TransactionDetails,
    input_transactions: &mut HistoryUtxoTxMap,
) -> UtxoRpcResult<()>
where
    T: UtxoCommonOps + UtxoStandardOps + MarketCoinOps,
{
    if !tx_details.should_update_kmd_rewards() {
        let error = "There is no need to update KMD rewards".to_owned();
        return MmError::err(UtxoRpcError::Internal(error));
    }
    let tx: UtxoTx = deserialize(tx_details.tx_hex.as_slice()).map_to_mm(|e| {
        UtxoRpcError::Internal(format!(
            "Error deserializing the {:?} transaction hex: {:?}",
            tx_details.tx_hash, e
        ))
    })?;
    let kmd_rewards = coin.calc_interest_of_tx(&tx, input_transactions).await?;
    let kmd_rewards = big_decimal_from_sat_unsigned(kmd_rewards, coin.as_ref().decimals);

    if let Some(TxFeeDetails::Utxo(UtxoFeeDetails { ref amount, .. })) = tx_details.fee_details {
        let actual_fee_amount = amount + &kmd_rewards;
        tx_details.fee_details = Some(TxFeeDetails::Utxo(UtxoFeeDetails {
            coin: Some(coin.as_ref().conf.ticker.clone()),
            amount: actual_fee_amount,
        }));
    }

    let my_address = &coin.my_address().map_to_mm(UtxoRpcError::Internal)?;
    let claimed_by_me = tx_details.from.iter().all(|from| from == my_address) && tx_details.to.contains(my_address);

    tx_details.kmd_rewards = Some(KmdRewardsDetails {
        amount: kmd_rewards,
        claimed_by_me,
    });
    Ok(())
}

pub async fn calc_interest_of_tx<T: UtxoCommonOps>(
    coin: &T,
    tx: &UtxoTx,
    input_transactions: &mut HistoryUtxoTxMap,
) -> UtxoRpcResult<u64> {
    if coin.as_ref().conf.ticker != "KMD" {
        let error = format!("Expected KMD ticker, found {}", coin.as_ref().conf.ticker);
        return MmError::err(UtxoRpcError::Internal(error));
    }

    let mut kmd_rewards = 0;
    for input in tx.inputs.iter() {
        // input transaction is zero if the tx is the coinbase transaction
        if input.previous_output.hash.is_zero() {
            continue;
        }

        let prev_tx_hash: H256Json = input.previous_output.hash.reversed().into();
        let prev_tx = coin
            .get_mut_verbose_transaction_from_map_or_rpc(prev_tx_hash, input_transactions)
            .await?;

        let prev_tx_value = prev_output_at(&prev_tx.tx, &prev_tx_hash, input.previous_output.index)
            .map_err(|e| {
                UtxoRpcError::InvalidResponse(format!(
                    "Transaction {:?} spends a missing output: {}",
                    tx.hash().reversed(),
                    e
                ))
            })?
            .value;
        let prev_tx_locktime = prev_tx.tx.lock_time as u64;
        let this_tx_locktime = tx.lock_time as u64;
        if let Ok(interest) = kmd_interest(prev_tx.height, prev_tx_value, prev_tx_locktime, this_tx_locktime) {
            kmd_rewards += interest;
        }
    }
    Ok(kmd_rewards)
}

pub fn history_sync_status(coin: &UtxoCoinFields) -> HistorySyncState {
    coin.history_sync_state.lock().unwrap().clone()
}

/// A minimal history row for the ordering and keying tests: `tx_hash` is the hex of the first 32
/// bytes of `internal_id`.
#[cfg(test)]
pub(crate) fn history_row_for_test(block_height: u64, internal_id: Vec<u8>) -> TransactionDetails {
    json::from_value(json!({
        "tx_hex": "",
        "tx_hash": hex::encode(&internal_id[..32]),
        "from": [],
        "to": [],
        "total_amount": "0",
        "spent_by_me": "0",
        "received_by_me": "0",
        "my_balance_change": "0",
        "block_height": block_height,
        "timestamp": 1,
        "coin": "TEST",
        "internal_id": hex::encode(&internal_id),
    }))
    .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(block_height: u64, id_byte: u8) -> TransactionDetails {
        history_row_for_test(block_height, vec![id_byte; 32])
    }

    fn sorted_ids(mut rows: Vec<TransactionDetails>) -> Vec<(u64, u8)> {
        rows.sort_unstable_by(legacy_history_order);
        rows.iter().map(|r| (r.block_height, r.internal_id.0[0])).collect()
    }

    /// Calls `f` with every permutation of `items` (Heap's algorithm).
    fn for_each_permutation<T: Clone>(items: &mut Vec<T>, k: usize, f: &mut dyn FnMut(&[T])) {
        if k <= 1 {
            f(items);
            return;
        }
        for i in 0..k {
            for_each_permutation(items, k - 1, f);
            let swap_with = if k % 2 == 0 { i } else { 0 };
            items.swap(swap_with, k - 1);
        }
    }

    // CRD ch.38 T38.5A.1. `my_tx_history` itself needs a context to run, so the paging skip is not
    // exercised here: the persisted sequence, which the method serves unchanged, is what is asserted.
    #[test]
    fn legacy_history_order_is_total_and_deterministic() {
        // two unconfirmed rows, two rows at one confirmed height, one row at a lower height
        let mut rows = vec![
            row(0, 0x30),
            row(0, 0x10),
            row(200, 0x50),
            row(200, 0x20),
            row(100, 0x01),
        ];
        let expected = vec![(0, 0x10), (0, 0x30), (200, 0x20), (200, 0x50), (100, 0x01)];

        // the forward and the reversed insertion order
        assert_eq!(sorted_ids(rows.clone()), expected);
        rows.reverse();
        assert_eq!(sorted_ids(rows.clone()), expected);

        // and every other insertion order
        let n = rows.len();
        let mut checked = 0;
        for_each_permutation(&mut rows, n, &mut |permutation| {
            assert_eq!(sorted_ids(permutation.to_vec()), expected);
            checked += 1;
        });
        assert_eq!(checked, 120);
    }

    // CRD ch.38 T38.5A.3, reduced: the collapse by hash that the loop relies on is
    // `history_map_by_hash` (the loop itself needs a coin and a backend); the per-transaction net
    // amounts are not covered here.
    #[test]
    fn legacy_history_one_row_per_hash() {
        // a persisted history that holds one transaction twice collapses to one row ...
        let history = vec![row(10, 0xab), row(0, 0xab), row(11, 0xcd)];
        let map = history_map_by_hash(history);
        assert_eq!(map.len(), 2);

        // ... that is found under the transaction hash a backend listing reports (a hash parsed
        // from its hex form), with the `internal_id` equal to that hash
        let listed: H256Json = H256Json::from_str(&hex::encode([0xab; 32])).unwrap();
        let stored = map.get(&listed).expect("the row is keyed by the listed hash");
        assert_eq!(stored.internal_id.0, listed.0.to_vec());

        // a row whose `tx_hash` is not a hash is dropped, not kept under a wrong key
        let mut bad = row(5, 0x01);
        bad.tx_hash = "not a hash".to_owned();
        assert!(history_map_by_hash(vec![bad]).is_empty());
    }

    // CRD ch.38 R38.5A.4: a history doubled on disk by the old key is rewritten once, sorted, right
    // after loading. The test drives `load_history_working_set`, the helper the loop calls before its
    // first pass; the loop itself (coin, backend, file save) is not run.
    #[test]
    fn legacy_history_doubled_file_heals_on_first_load() {
        let unique = vec![row(0, 0x30), row(100, 0x01), row(200, 0x50), row(100, 0x02)];
        // every transaction stored twice, identical copies, in an arbitrary order
        let doubled: Vec<TransactionDetails> = unique.iter().chain(unique.iter().rev()).cloned().collect();
        let (map, healed) = load_history_working_set(doubled);
        assert_eq!(map.len(), 4);
        let written = healed.expect("a collapsed history is rewritten");
        assert_eq!(
            written
                .iter()
                .map(|r| (r.block_height, r.internal_id.0[0]))
                .collect::<Vec<_>>(),
            vec![(0, 0x30), (200, 0x50), (100, 0x01), (100, 0x02)]
        );
        let hashes: std::collections::HashSet<_> = written.iter().map(|r| r.tx_hash.clone()).collect();
        assert_eq!(hashes.len(), written.len());

        // a row whose hash cannot be parsed is dropped, which also rewrites the file
        let mut bad = row(5, 0x01);
        bad.tx_hash = "not a hash".to_owned();
        let (map, healed) = load_history_working_set(vec![row(7, 0x09), bad]);
        assert_eq!(map.len(), 1);
        assert_eq!(healed.expect("a dropped row is rewritten").len(), 1);

        // nothing to collapse: no write (an empty history included)
        let (map, healed) = load_history_working_set(unique);
        assert_eq!(map.len(), 4);
        assert!(healed.is_none());
        assert!(load_history_working_set(Vec::new()).1.is_none());
    }

    // CRD ch.38 T38.5A.4 (order part): the row keeps its `internal_id` and moves.
    #[test]
    fn legacy_history_maturation_moves_row() {
        let mut rows = vec![row(0, 0x10), row(0, 0x20), row(0, 0x30), row(100, 0x01)];
        assert_eq!(sorted_ids(rows.clone()), vec![
            (0, 0x10),
            (0, 0x20),
            (0, 0x30),
            (100, 0x01)
        ]);

        // the middle unconfirmed row is mined above the confirmed one
        let matured = rows.iter_mut().find(|r| r.internal_id.0[0] == 0x20).unwrap();
        assert!(matured.should_update_block_height());
        matured.block_height = 150;
        assert_eq!(sorted_ids(rows), vec![(0, 0x10), (0, 0x30), (150, 0x20), (100, 0x01)]);
    }

    // The native listing pairs a hash with a block height (never the position inside the block).
    #[test]
    fn native_listing_pairs_hash_with_block_height() {
        let confirmed: crate::utxo::rpc_clients::ListTransactionsItem = json::from_value(json!({
            "address": "a", "category": "receive", "amount": 1.0, "vout": 0, "timereceived": 1,
            "blockindex": 437, "blockheight": 700000, "confirmations": 5,
            "txid": hex::encode([0x11; 32]),
        }))
        .unwrap();
        let pair = (confirmed.txid, confirmed.block_height());
        assert_eq!(pair, (H256Json::from([0x11; 32]), 700000));

        // no height reported (an unconfirmed entry, or a daemon without the field): 0, not `blockindex`
        let unconfirmed: crate::utxo::rpc_clients::ListTransactionsItem = json::from_value(json!({
            "address": "a", "category": "receive", "amount": 1.0, "vout": 0, "timereceived": 1,
            "blockindex": 437, "confirmations": 0,
            "txid": hex::encode([0x22; 32]),
        }))
        .unwrap();
        assert_eq!(
            (unconfirmed.txid, unconfirmed.block_height()),
            (H256Json::from([0x22; 32]), 0)
        );
    }

    const MINUTE_MS: u64 = 60 * 1000;
    const TLS_ERROR: &str = "transport: the certificate of electrum1.example:50002 has expired";

    // The failure times of the throttle test: 50 failures of one text across 30 minutes, three of
    // them exactly 10, 20 and 30 minutes after the first, the rest between those, never a full
    // summary interval after the previous summary.
    fn failure_times_over_30_minutes() -> Vec<u64> {
        let mut times = vec![0, 1000];
        for segment in 0..3u64 {
            for step in 1..=15u64 {
                times.push(segment * 10 * MINUTE_MS + step * 37_000);
            }
            times.push((segment + 1) * 10 * MINUTE_MS);
        }
        times.sort_unstable();
        times
    }

    #[test]
    fn failure_throttle_logs_a_repeating_error_once_and_summarizes() {
        let times = failure_times_over_30_minutes();
        assert_eq!(times.len(), 50);

        let mut throttle = FailureThrottle::default();
        let reports: Vec<FailureReport> = times
            .iter()
            .map(|t| throttle.on_failure(&format!("utxo_common_history.rs:423] {TLS_ERROR}"), *t))
            .collect();

        let warns = reports.iter().filter(|r| **r == FailureReport::Warn).count();
        let silent = reports.iter().filter(|r| **r == FailureReport::Silent).count();
        let summaries: Vec<u32> = reports
            .iter()
            .filter_map(|r| match r {
                FailureReport::Summary { repeats } => Some(*repeats),
                _ => None,
            })
            .collect();
        assert_eq!(warns, 1);
        assert_eq!(reports[0], FailureReport::Warn);
        assert_eq!(summaries, vec![17, 16, 16]);
        assert_eq!(silent, 46);
    }

    #[test]
    fn failure_throttle_ignores_the_location_prefix() {
        assert_eq!(
            strip_location_prefix("utxo_common_history.rs:423] a:1] Error x on y"),
            "Error x on y"
        );
        // not a location marker: left as it is
        assert_eq!(strip_location_prefix("Error x] on y"), "Error x] on y");
        assert_eq!(strip_location_prefix("plain"), "plain");

        let mut throttle = FailureThrottle::default();
        assert_eq!(throttle.on_failure("a.rs:1] same", 0), FailureReport::Warn);
        // the same cause raised from another line is the same failure
        assert_eq!(throttle.on_failure("b.rs:99] same", 1000), FailureReport::Silent);
        assert_eq!(throttle.on_failure("same", 2000), FailureReport::Silent);
    }

    #[test]
    fn failure_throttle_delay_backs_off_and_is_bounded() {
        let mut throttle = FailureThrottle::default();
        assert_eq!(throttle.delay_secs(), 10);
        let mut delays = vec![];
        for i in 0..8 {
            throttle.on_failure(TLS_ERROR, i * 1000);
            delays.push(throttle.delay_secs());
        }
        assert_eq!(delays, vec![10, 60, 120, 240, 300, 300, 300, 300]);

        // a very long failure run does not overflow the shift
        for i in 8..200 {
            throttle.on_failure(TLS_ERROR, i * 1000);
        }
        assert_eq!(throttle.delay_secs(), 300);
    }

    #[test]
    fn failure_throttle_logs_a_changed_error_text_again() {
        let mut throttle = FailureThrottle::default();
        assert_eq!(throttle.on_failure(TLS_ERROR, 0), FailureReport::Warn);
        assert_eq!(throttle.on_failure(TLS_ERROR, 1000), FailureReport::Silent);
        assert_eq!(
            throttle.on_failure("transport: connection refused by electrum1.example:50002", 2000),
            FailureReport::Warn
        );
        // the new text is the one that repeats now, and the delay keeps growing
        assert_eq!(
            throttle.on_failure("transport: connection refused by electrum1.example:50002", 3000),
            FailureReport::Silent
        );
        assert_eq!(throttle.delay_secs(), 240);
        // going back to the first text is a change too
        assert_eq!(throttle.on_failure(TLS_ERROR, 4000), FailureReport::Warn);
    }

    #[test]
    fn failure_throttle_recovers_once_and_resets() {
        let mut throttle = FailureThrottle::default();
        // a success with no failure before it says nothing
        assert_eq!(throttle.on_success(), None);

        for i in 0..5 {
            throttle.on_failure(TLS_ERROR, i * 1000);
        }
        assert_eq!(throttle.delay_secs(), 300);
        assert_eq!(throttle.on_success(), Some(5));
        assert_eq!(throttle.delay_secs(), 10);
        // one recovery line only
        assert_eq!(throttle.on_success(), None);

        // the same text after the recovery is a first failure again
        assert_eq!(throttle.on_failure(TLS_ERROR, 10_000), FailureReport::Warn);
        assert_eq!(throttle.delay_secs(), 10);
    }

    #[test]
    fn failure_throttles_of_the_two_sources_are_independent() {
        let mut balance = FailureThrottle::default();
        let mut history = FailureThrottle::default();

        assert_eq!(balance.on_failure(TLS_ERROR, 0), FailureReport::Warn);
        // the same text on the other source is its own first failure
        assert_eq!(history.on_failure(TLS_ERROR, 500), FailureReport::Warn);
        assert_eq!(balance.on_failure(TLS_ERROR, 1000), FailureReport::Silent);
        assert_eq!(history.on_failure(TLS_ERROR, 1500), FailureReport::Silent);

        // a summary of one source does not reset the other's clock
        assert_eq!(balance.on_failure(TLS_ERROR, 10 * MINUTE_MS), FailureReport::Summary {
            repeats: 2
        });
        assert_eq!(
            history.on_failure(TLS_ERROR, 10 * MINUTE_MS + 500),
            FailureReport::Summary { repeats: 2 }
        );

        // a history success resets only the history source: the balance still fails and backs off
        assert_eq!(history.on_success(), Some(3));
        assert_eq!(balance.delay_secs(), 120);
        assert_eq!(history.delay_secs(), 10);
        assert_eq!(balance.on_success(), Some(3));
    }

    #[test]
    fn failure_throttle_treats_changing_numbers_as_the_same_error() {
        let field = |verified: u64, ago: u64, line: u32| {
            format!(
                "utxo_common_history.rs:{line}] Error MmError {{ etype: Transport(\"invalid peer certificate: \
                 certificate expired: verification time {verified} (UNIX), but certificate is not valid after \
                 1791710492 ({ago} seconds ago)\") }} on getting balance"
            )
        };
        assert_eq!(
            failure_key(&field(1794225784, 2515292, 279)),
            failure_key(&field(1794225794, 2515302, 311))
        );

        let mut throttle = FailureThrottle::default();
        assert_eq!(
            throttle.on_failure(&field(1794225784, 2515292, 279), 0),
            FailureReport::Warn
        );
        // the verification time and the seconds count advanced, the cause did not
        assert_eq!(
            throttle.on_failure(&field(1794225794, 2515302, 311), 10_000),
            FailureReport::Silent
        );

        // a text that differs in words is a different failure
        let other = field(1794225804, 2515312, 279).replace("certificate expired", "unknown issuer");
        assert_eq!(throttle.on_failure(&other, 20_000), FailureReport::Warn);
    }

    #[test]
    fn failure_throttle_reset_returns_to_the_start() {
        let mut throttle = FailureThrottle::default();
        for i in 0..4 {
            throttle.on_failure(TLS_ERROR, i * 1000);
        }
        assert_eq!(throttle.delay_secs(), 240);

        throttle.reset();
        assert_eq!(throttle.delay_secs(), 10);
        // nothing is left to report as a recovery
        assert_eq!(throttle.on_success(), None);
        // the same text after the reset is a first failure again, with the first delay
        assert_eq!(throttle.on_failure(TLS_ERROR, 5000), FailureReport::Warn);
        assert_eq!(throttle.delay_secs(), 10);
    }
}

/// CRD ch.38 §38.5C: the absence rule and the changed height in the legacy collector.
#[cfg(test)]
mod absence_tests {
    use super::*;

    fn row(block_height: u64, id_byte: u8) -> TransactionDetails {
        history_row_for_test(block_height, vec![id_byte; 32])
    }

    fn h(byte: u8) -> H256Json { H256Json::from([byte; 32]) }

    fn map_of(rows: Vec<TransactionDetails>) -> HashMap<H256Json, TransactionDetails> { history_map_by_hash(rows) }

    #[test]
    fn a_confirmed_row_absent_from_two_listings_one_block_apart_is_removed() {
        let mut map = map_of(vec![row(100, 0xa1), row(90, 0xb2)]);
        let mut marks = AbsenceMarks::default();
        let listing = [(h(0xa1), 100)];
        assert_eq!(remove_unlisted_rows(&mut marks, &mut map, 800, &listing), 0);
        assert_eq!(map.len(), 2);
        assert_eq!(remove_unlisted_rows(&mut marks, &mut map, 801, &listing), 1);
        assert!(map.contains_key(&h(0xa1)) && !map.contains_key(&h(0xb2)));
        assert!(!marks.any());
    }

    #[test]
    fn an_unconfirmed_row_follows_the_same_rule() {
        let mut map = map_of(vec![row(0, 0xe5)]);
        let mut marks = AbsenceMarks::default();
        assert_eq!(remove_unlisted_rows(&mut marks, &mut map, 800, &[]), 0);
        assert_eq!(remove_unlisted_rows(&mut marks, &mut map, 801, &[]), 1);
        assert!(map.is_empty());
    }

    #[test]
    fn absent_from_one_listing_is_kept_and_the_mark_keeps_the_need_to_run_test_true() {
        let mut map = map_of(vec![row(90, 0xb2)]);
        let mut marks = AbsenceMarks::default();
        // With no mark and a settled row, the test is false.
        assert!(!history_needs_pass(&map, &marks, false));
        assert_eq!(remove_unlisted_rows(&mut marks, &mut map, 800, &[]), 0);
        assert_eq!(map.len(), 1);
        assert!(history_needs_pass(&map, &marks, false));
    }

    /// CRD ch.38 §38.5C amendment of 2026-11-18, item 3: a chain-changed event makes the pass run whatever the rest of
    /// the test says.
    #[test]
    fn a_chain_changed_event_forces_the_need_to_run_test() {
        let map = map_of(vec![row(90, 0xb2)]);
        let marks = AbsenceMarks::default();
        assert!(!history_needs_pass(&map, &marks, false));
        assert!(history_needs_pass(&map, &marks, true));
        assert!(history_needs_pass(&HashMap::new(), &marks, true));
    }

    #[test]
    fn two_listings_at_the_same_tip_keep_the_row() {
        let mut map = map_of(vec![row(90, 0xb2)]);
        let mut marks = AbsenceMarks::default();
        assert_eq!(remove_unlisted_rows(&mut marks, &mut map, 800, &[]), 0);
        assert_eq!(remove_unlisted_rows(&mut marks, &mut map, 800, &[]), 0);
        assert_eq!(map.len(), 1);
        assert!(marks.any());
    }

    #[test]
    fn listed_again_after_the_first_absence_keeps_the_row_and_clears_the_mark() {
        let mut map = map_of(vec![row(90, 0xb2)]);
        let mut marks = AbsenceMarks::default();
        assert_eq!(remove_unlisted_rows(&mut marks, &mut map, 800, &[]), 0);
        assert_eq!(remove_unlisted_rows(&mut marks, &mut map, 801, &[(h(0xb2), 90)]), 0);
        assert!(!marks.any());
        assert!(!history_needs_pass(&map, &marks, false));
        // Counting starts again: one absence is not enough.
        assert_eq!(remove_unlisted_rows(&mut marks, &mut map, 802, &[]), 0);
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn the_height_of_a_listed_row_is_compared() {
        assert_eq!(listed_height_change(100, 100), HeightChange::Same);
        assert_eq!(listed_height_change(100, 120), HeightChange::Moved);
        assert_eq!(listed_height_change(100, 0), HeightChange::Unconfirmed);
        // An unconfirmed stored row is R38.5A.5's: filled in, not "moved".
        assert_eq!(listed_height_change(0, 120), HeightChange::Same);
        assert_eq!(listed_height_change(u64::MAX, 120), HeightChange::Same);
        assert_eq!(listed_height_change(0, 0), HeightChange::Same);
    }
}

/// CRD ch.38 §38.5C, legacy collector, one real pass of `process_history_loop` over a native-backed test coin. The
/// loop waits 30 s between passes, so a test drives exactly one pass; the rule over several passes is tested on
/// [`remove_unlisted_rows`] in `absence_tests`.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod absence_loop_tests {
    use super::*;
    use crate::utxo::rpc_clients::{ListTransactionsItem, NativeClient, NativeClientImpl};
    use crate::utxo::utxo_tests::software_coin_for_test;
    use crate::{CoinsContext, MmCoinEnum};
    use common::block_on;
    use futures::future::Either;
    use mm2_core::mm_ctx::MmCtxBuilder;
    use mocktopus::mocking::*;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::Arc;

    const A: u8 = 0xa1;
    const B: u8 = 0xb2;
    const U: u8 = 0xe5;

    fn row(block_height: u64, id_byte: u8) -> TransactionDetails {
        history_row_for_test(block_height, vec![id_byte; 32])
    }

    fn hash(b: u8) -> String { hex::encode([b; 32]) }

    // Two null inputs, one P2PKH output, so `tx_details_by_hash` needs only `get_verbose_transaction`.
    fn verbose_tx(time: u64) -> RpcTransaction {
        const P2PKH: &str = "76a91405aab5342166f8594baf17a7d9bef5d56744332788ac";
        let null_input = format!("{}ffffffff01c9ffffffff", "00".repeat(32));
        let tx_hex = format!("0100000002{null_input}{null_input}01a08601000000000019{P2PKH}00000000");
        let lelantus_input = |n_fees: f64, serial: &str| {
            json!({
                "scriptSig": { "asm": "OP_LELANTUSJOINSPLIT", "hex": "c9" },
                "nFees": n_fees,
                "serials": [serial],
                "sequence": 4294967295u32,
            })
        };
        json::from_value(json!({
            "hex": tx_hex,
            "txid": "4242424242424242424242424242424242424242424242424242424242424242",
            "version": 1,
            "locktime": 0,
            "vin": [lelantus_input(0.00003793, "0a0b0c0d"), lelantus_input(0.00045778, "0e0f1011")],
            "vout": [{
                "value": 0.001,
                "n": 0,
                "scriptPubKey": {
                    "asm": "OP_DUP OP_HASH160 05aab5342166f8594baf17a7d9bef5d567443327 OP_EQUALVERIFY OP_CHECKSIG",
                    "hex": P2PKH,
                    "reqSigs": 1,
                    "type": "pubkeyhash",
                    "addresses": ["R9o9xTocqr6CeEDGDH6mEYpwLoMz6jNjMW"],
                },
            }],
            "blockhash": "4343434343434343434343434343434343434343434343434343434343434343",
            "confirmations": 10,
            "time": time,
            "blocktime": time,
            "height": 1000,
        }))
        .unwrap()
    }

    /// What one pass is given: the stored rows, the listing (`None`: the listing request fails) and the tip of the
    /// daemon.
    struct Pass {
        seeded: Vec<TransactionDetails>,
        listing: Option<Vec<(u8, u64)>>,
        tip: u64,
    }

    /// Runs one pass and returns the file content after it, with the `tx_hash` that the mocked verbose transaction
    /// gives a row that is fetched (its hash, not the listed one) and the number of detail requests.
    fn drive_one_pass(pass: Pass) -> (Vec<TransactionDetails>, String, usize) {
        static DIRS: AtomicUsize = AtomicUsize::new(0);
        let db_root = std::env::temp_dir().join(format!(
            "kdf-legacy-absence-{}-{}",
            std::process::id(),
            DIRS.fetch_add(1, AtomicOrdering::SeqCst)
        ));
        let ctx = MmCtxBuilder::new()
            .with_conf(json!({ "dbdir": db_root.to_string_lossy() }))
            .into_mm_arc();
        std::fs::create_dir_all(ctx.dbdir().join("TRANSACTIONS")).unwrap();

        let coin = software_coin_for_test("ABSENCE");
        let my_address = match &coin.as_ref().derivation_method {
            DerivationMethod::Iguana(a) => a.display_address().unwrap(),
            _ => unreachable!(),
        };
        let history_path = coin.tx_history_path(&ctx);
        std::fs::write(&history_path, json::to_vec(&pass.seeded).unwrap()).unwrap();

        let detail_requests = Arc::new(AtomicUsize::new(0));
        let listing_requests = Arc::new(AtomicUsize::new(0));
        let verbose = verbose_tx(1_700_000_000);
        let counter = detail_requests.clone();
        NativeClient::get_verbose_transaction.mock_safe(move |_client, _txid| {
            counter.fetch_add(1, AtomicOrdering::SeqCst);
            MockResult::Return(Box::new(futures01::future::ok(verbose.clone())))
        });
        NativeClient::display_balance
            .mock_safe(|_client, _addr, _dec| MockResult::Return(Box::new(futures01::future::ok(BigDecimal::from(1)))));
        let tip = pass.tip;
        NativeClientImpl::get_block_count
            .mock_safe(move |_client| MockResult::Return(Box::new(futures01::future::ok(tip))));
        let listing = pass.listing.clone();
        let listing_counter = listing_requests.clone();
        NativeClientImpl::list_transactions.mock_safe(move |_client, _count, from| {
            if from == 0 {
                listing_counter.fetch_add(1, AtomicOrdering::SeqCst);
            }
            let result: common::jsonrpc_client::RpcRes<Vec<ListTransactionsItem>> = match &listing {
                None => Box::new(futures01::future::err(common::jsonrpc_client::JsonRpcError {
                    client_info: "test".into(),
                    request: common::jsonrpc_client::JsonRpcRequestEnum::Single(
                        common::jsonrpc_client::JsonRpcRequest {
                            jsonrpc: "2.0".into(),
                            id: "1".into(),
                            method: "listtransactions".into(),
                            params: vec![],
                        },
                    ),
                    error: common::jsonrpc_client::JsonRpcErrorType::Transport("stub failure".into()),
                })),
                Some(listing) => {
                    let items = if from == 0 {
                        listing
                            .iter()
                            .map(|(b, height)| ListTransactionsItem {
                                address: my_address.clone(),
                                category: "receive".into(),
                                txid: H256Json::from([*b; 32]),
                                blockheight: Some(*height),
                                ..Default::default()
                            })
                            .collect()
                    } else {
                        vec![]
                    };
                    Box::new(futures01::future::ok(items))
                },
            };
            MockResult::Return(result)
        });

        // The `tx_hash` the details of a fetched row carry (the hash of the mocked transaction).
        let fetched_hash = {
            let mut cache = HistoryUtxoTxMap::default();
            block_on(coin.tx_details_by_hash(&[0u8; 32], &mut cache))
                .unwrap()
                .tx_hash
        };
        detail_requests.store(0, AtomicOrdering::SeqCst);

        let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();
        block_on(coins_ctx.add_coin(MmCoinEnum::UtxoCoin(coin.clone()))).unwrap();

        // The pass is over when the listing was asked and the sync state says finished, or, for a failed listing,
        // shortly after it was asked (the loop then backs off for 10 s).
        let failed = pass.listing.is_none();
        let finished_coin = coin.clone();
        let asked = listing_requests.clone();
        let poll = async move {
            for _ in 0..400 {
                if asked.load(AtomicOrdering::SeqCst) > 0 {
                    if failed {
                        common::executor::Timer::sleep(0.5).await;
                        return;
                    }
                    if matches!(
                        *finished_coin.as_ref().history_sync_state.lock().unwrap(),
                        HistorySyncState::Finished
                    ) {
                        return;
                    }
                }
                common::executor::Timer::sleep(0.05).await;
            }
            panic!("the pass did not end within 20 s");
        };
        let lp = process_history_loop(coin.clone(), ctx.clone());
        futures::pin_mut!(poll);
        futures::pin_mut!(lp);
        match block_on(futures::future::select(poll, lp)) {
            Either::Left(_) => (),
            Either::Right(_) => panic!("the loop ended before the pass did"),
        }

        let after: Vec<TransactionDetails> = json::from_slice(&std::fs::read(&history_path).unwrap()).unwrap();
        let _ = std::fs::remove_dir_all(&db_root);
        (after, fetched_hash, detail_requests.load(AtomicOrdering::SeqCst))
    }

    fn present(rows: &[TransactionDetails], h: &str) -> bool { rows.iter().any(|r| r.tx_hash == h) }

    /// The native listing's tip with the block count answering `counts` in turn (the last one for ever): the count
    /// is read before and after the listing.
    fn native_listing_tip(counts: Vec<u64>) -> Option<u64> {
        let ctx = MmCtxBuilder::new().into_mm_arc();
        let coin = software_coin_for_test("ABSENCETIP");
        let my_address = match &coin.as_ref().derivation_method {
            DerivationMethod::Iguana(a) => a.display_address().unwrap(),
            _ => unreachable!(),
        };
        let reads = Arc::new(AtomicUsize::new(0));
        let counter = reads.clone();
        NativeClientImpl::get_block_count.mock_safe(move |_client| {
            let n = counter.fetch_add(1, AtomicOrdering::SeqCst);
            let count = counts[n.min(counts.len() - 1)];
            MockResult::Return(Box::new(futures01::future::ok(count)))
        });
        NativeClientImpl::list_transactions.mock_safe(move |_client, _count, from| {
            let items = if from == 0 {
                vec![ListTransactionsItem {
                    address: my_address.clone(),
                    category: "receive".into(),
                    txid: H256Json::from([A; 32]),
                    blockheight: Some(100),
                    ..Default::default()
                }]
            } else {
                vec![]
            };
            MockResult::Return(Box::new(futures01::future::ok(items)))
        });
        match block_on(request_tx_history(&coin, ctx.metrics.clone())) {
            RequestTxHistoryResult::Ok(ids, tip) => {
                assert_eq!(ids.len(), 1, "the listing itself is used whatever the tip says");
                assert_eq!(
                    reads.load(AtomicOrdering::SeqCst),
                    2,
                    "once before and once after the listing"
                );
                tip
            },
            _ => panic!("the listing is a success"),
        }
    }

    #[test]
    fn a_native_listing_with_the_same_block_count_before_and_after_carries_that_tip() {
        assert_eq!(native_listing_tip(vec![800, 800]), Some(800));
    }

    #[test]
    fn a_native_listing_during_which_a_block_arrived_carries_no_tip() {
        assert_eq!(native_listing_tip(vec![800, 801]), None);
    }

    #[test]
    fn a_first_absence_changes_nothing_in_the_file() {
        let seeded = vec![row(100, A), row(90, B), row(0, U)];
        let (after, _, details) = drive_one_pass(Pass {
            seeded: seeded.clone(),
            listing: Some(vec![(A, 100)]),
            tip: 800,
        });
        assert_eq!(details, 0);
        for b in [A, B, U] {
            assert!(present(&after, &hash(b)), "{b:x} is kept");
        }
        assert_eq!(after.len(), 3);
    }

    #[test]
    fn a_failed_listing_removes_nothing_and_sets_no_mark() {
        let (after, _, details) = drive_one_pass(Pass {
            seeded: vec![row(100, A), row(90, B)],
            listing: None,
            tip: 800,
        });
        assert_eq!(details, 0);
        assert_eq!(after.len(), 2);
    }

    #[test]
    fn a_listed_confirmed_row_at_another_height_takes_the_new_height_and_time() {
        let (after, fetched, details) = drive_one_pass(Pass {
            seeded: vec![row(100, A), row(90, B)],
            listing: Some(vec![(A, 100), (B, 95)]),
            tip: 800,
        });
        assert_eq!(details, 1, "the time of the block comes with one detail request");
        let moved = after
            .iter()
            .find(|r| r.tx_hash == fetched)
            .expect("the row of B was replaced");
        assert_eq!((moved.block_height, moved.timestamp), (95, 1_700_000_000));
        assert!(present(&after, &hash(A)));
        assert_eq!(after.len(), 2);
    }

    #[test]
    fn a_listed_confirmed_row_that_is_now_unconfirmed_goes_back_to_height_zero() {
        let (after, _, details) = drive_one_pass(Pass {
            seeded: vec![row(100, A), row(90, B)],
            listing: Some(vec![(A, 100), (B, 0)]),
            tip: 800,
        });
        assert_eq!(details, 0);
        let b = after.iter().find(|r| r.tx_hash == hash(B)).unwrap();
        assert_eq!(b.block_height, 0);
        assert_eq!(after.iter().find(|r| r.tx_hash == hash(A)).unwrap().block_height, 100);
    }
}

/// CRD ch.39 D39.11.4V.1 and T39.11.4V.8: the legacy collector reports the transactions it skipped because they cannot
/// be read, in the sync status, one real pass of `process_history_loop` over a native-backed test coin. A pass is
/// driven from a stored file, so a later pass is a new run over the rows the earlier one wrote.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod unreadable_report_tests {
    use super::*;
    use crate::utxo::rpc_clients::{ListTransactionsItem, NativeClient, NativeClientImpl, UtxoRpcFut};
    use crate::utxo::utxo_tests::software_coin_for_test;
    use crate::{CoinsContext, MmCoinEnum};
    use common::block_on;
    use futures::future::Either;
    use mm2_core::mm_ctx::MmCtxBuilder;
    use mocktopus::mocking::*;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::{Arc, Mutex as StdMutex};

    const P2PKH: &str = "76a91405aab5342166f8594baf17a7d9bef5d56744332788ac";
    /// The height every mocked transaction is confirmed at, and at which the listing reports it.
    const HEIGHT: u64 = 1000;

    /// What the backend answers for one transaction id.
    #[derive(Clone)]
    enum Served {
        Tx(String),
        Transport,
    }

    type Backend = Arc<StdMutex<HashMap<[u8; 32], Served>>>;

    fn backend(entries: Vec<([u8; 32], Served)>) -> Backend { Arc::new(StdMutex::new(entries.into_iter().collect())) }

    /// An overwintered version-4 transaction with one input and one P2PKH output. With no `prevout` the input is a
    /// null one, so the transaction needs no input transaction.
    fn v4_tx(prevout: Option<([u8; 32], u32)>, value: u64) -> String {
        let (prev_hash, prev_index) = prevout.unwrap_or(([0u8; 32], u32::MAX));
        let prev_hash = hex::encode(prev_hash);
        let prev_index = hex::encode(prev_index.to_le_bytes());
        let value = hex::encode(value.to_le_bytes());
        // Header with the overwintered bit, version group id, one input, one output, lock time, expiry height,
        // value balance, no shielded spends, no shielded outputs, no joinsplits.
        format!(
            "04000080{}01{prev_hash}{prev_index}01c9ffffffff01{value}19{P2PKH}{}000000",
            "85202f89",
            "00".repeat(16)
        )
    }

    /// An overwintered version-6 transaction, which the shared parser refuses.
    fn v6_tx() -> String { format!("06000080{}{}", "85202f89", "00".repeat(16)) }

    /// The id under which the listing and the rows know the transaction `tx_hex`.
    fn id_of(tx_hex: &str) -> [u8; 32] {
        let coin = software_coin_for_test("IDS");
        let mut tx: UtxoTx = deserialize(hex::decode(tx_hex).unwrap().as_slice()).unwrap();
        tx.tx_hash_algo = coin.as_ref().tx_hash_algo;
        let id: H256Json = tx.hash().reversed().into();
        id.0
    }

    fn text(id: [u8; 32]) -> String { hex::encode(id) }

    fn verbose_of(tx_hex: &str) -> RpcTransaction {
        let lelantus_input = |n_fees: f64, serial: &str| {
            json!({
                "scriptSig": { "asm": "OP_LELANTUSJOINSPLIT", "hex": "c9" },
                "nFees": n_fees,
                "serials": [serial],
                "sequence": 4294967295u32,
            })
        };
        json::from_value(json!({
            "hex": tx_hex,
            "txid": "4242424242424242424242424242424242424242424242424242424242424242",
            "version": 4,
            "locktime": 0,
            "vin": [lelantus_input(0.00003793, "0a0b0c0d")],
            "vout": [{
                "value": 0.001,
                "n": 0,
                "scriptPubKey": {
                    "asm": "OP_DUP OP_HASH160 05aab5342166f8594baf17a7d9bef5d567443327 OP_EQUALVERIFY OP_CHECKSIG",
                    "hex": P2PKH,
                    "reqSigs": 1,
                    "type": "pubkeyhash",
                    "addresses": ["R9o9xTocqr6CeEDGDH6mEYpwLoMz6jNjMW"],
                },
            }],
            "blockhash": "4343434343434343434343434343434343434343434343434343434343434343",
            "confirmations": 10,
            "time": 1_700_000_000u64,
            "blocktime": 1_700_000_000u64,
            "height": HEIGHT,
        }))
        .unwrap()
    }

    fn transport_error() -> common::jsonrpc_client::JsonRpcError {
        common::jsonrpc_client::JsonRpcError {
            client_info: "test".into(),
            request: common::jsonrpc_client::JsonRpcRequestEnum::Single(common::jsonrpc_client::JsonRpcRequest {
                jsonrpc: "2.0".into(),
                id: "1".into(),
                method: "getrawtransaction".into(),
                params: vec![],
            }),
            error: common::jsonrpc_client::JsonRpcErrorType::Transport("stub failure".into()),
        }
    }

    /// Runs one pass over the stored rows `seeded` with the backend listing `listing`, and returns the rows of the
    /// file after it and the sync status as the RPC answer shows it.
    fn drive_one_pass(
        backend: &Backend,
        listing: &[[u8; 32]],
        seeded: Vec<TransactionDetails>,
    ) -> (Vec<TransactionDetails>, Json) {
        static DIRS: AtomicUsize = AtomicUsize::new(0);
        let db_root = std::env::temp_dir().join(format!(
            "kdf-legacy-unreadable-{}-{}",
            std::process::id(),
            DIRS.fetch_add(1, AtomicOrdering::SeqCst)
        ));
        let ctx = MmCtxBuilder::new()
            .with_conf(json!({ "dbdir": db_root.to_string_lossy() }))
            .into_mm_arc();
        std::fs::create_dir_all(ctx.dbdir().join("TRANSACTIONS")).unwrap();

        let coin = software_coin_for_test("UNREADABLE");
        let my_address = match &coin.as_ref().derivation_method {
            DerivationMethod::Iguana(a) => a.display_address().unwrap(),
            _ => unreachable!(),
        };
        let history_path = coin.tx_history_path(&ctx);
        std::fs::write(&history_path, json::to_vec(&seeded).unwrap()).unwrap();

        let listing_requests = Arc::new(AtomicUsize::new(0));
        let served_by = backend.clone();
        NativeClient::get_verbose_transaction.mock_safe(move |_client, txid| {
            let served = served_by.lock().unwrap().get(&txid.0).cloned();
            let answer: UtxoRpcFut<RpcTransaction> = match served {
                Some(Served::Tx(tx_hex)) => Box::new(futures01::future::ok(verbose_of(&tx_hex))),
                Some(Served::Transport) | None => Box::new(futures01::future::err(MmError::new(
                    UtxoRpcError::Transport(transport_error()),
                ))),
            };
            MockResult::Return(answer)
        });
        NativeClient::display_balance
            .mock_safe(|_client, _addr, _dec| MockResult::Return(Box::new(futures01::future::ok(BigDecimal::from(1)))));
        NativeClientImpl::get_block_count
            .mock_safe(move |_client| MockResult::Return(Box::new(futures01::future::ok(2000u64))));
        let listed: Vec<[u8; 32]> = listing.to_vec();
        let listing_counter = listing_requests.clone();
        NativeClientImpl::list_transactions.mock_safe(move |_client, _count, from| {
            if from == 0 {
                listing_counter.fetch_add(1, AtomicOrdering::SeqCst);
            }
            let items: Vec<ListTransactionsItem> = if from == 0 {
                listed
                    .iter()
                    .map(|id| ListTransactionsItem {
                        address: my_address.clone(),
                        category: "receive".into(),
                        txid: H256Json::from(*id),
                        blockheight: Some(HEIGHT),
                        ..Default::default()
                    })
                    .collect()
            } else {
                vec![]
            };
            MockResult::Return(Box::new(futures01::future::ok(items)))
        });

        let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();
        block_on(coins_ctx.add_coin(MmCoinEnum::UtxoCoin(coin.clone()))).unwrap();

        // The pass is over when the listing was asked and the sync state says finished.
        let finished_coin = coin.clone();
        let asked = listing_requests.clone();
        let poll = async move {
            for _ in 0..400 {
                if asked.load(AtomicOrdering::SeqCst) > 0
                    && finished_coin.as_ref().history_sync_state.lock().unwrap().is_finished()
                {
                    return;
                }
                common::executor::Timer::sleep(0.05).await;
            }
            panic!("the pass did not end within 20 s");
        };
        let lp = process_history_loop(coin.clone(), ctx.clone());
        futures::pin_mut!(poll);
        futures::pin_mut!(lp);
        match block_on(futures::future::select(poll, lp)) {
            Either::Left(_) => (),
            Either::Right(_) => panic!("the loop ended before the pass did"),
        }

        let after: Vec<TransactionDetails> = json::from_slice(&std::fs::read(&history_path).unwrap()).unwrap();
        let status = json::to_value(coin.history_sync_status()).unwrap();
        let _ = std::fs::remove_dir_all(&db_root);
        (after, status)
    }

    fn present(rows: &[TransactionDetails], id: [u8; 32]) -> bool { rows.iter().any(|row| row.tx_hash == text(id)) }

    /// The `unreadable_transactions` member of a finished status, as (tx_hash, error) pairs.
    fn reported(status: &Json) -> Vec<(String, String)> {
        assert_eq!(status["state"], "Finished");
        status["additional_info"]["unreadable_transactions"]
            .as_array()
            .expect("a finished status with skips carries the list")
            .iter()
            .map(|item| {
                (
                    item["tx_hash"].as_str().unwrap().to_owned(),
                    item["error"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    fn plain_finished(status: &Json) -> bool { json::to_string(status).unwrap() == r#"{"state":"Finished"}"# }

    #[test]
    fn an_unreadable_transaction_is_reported_and_drops_out_when_it_reads() {
        let (a, b, c) = (v4_tx(None, 100_000), v4_tx(None, 200_000), v4_tx(None, 300_000));
        let (a_id, b_id) = (id_of(&a), id_of(&b));
        let unreadable_id = [0x22u8; 32];
        let served = backend(vec![
            (a_id, Served::Tx(a)),
            (unreadable_id, Served::Tx(v6_tx())),
            (b_id, Served::Tx(b)),
        ]);

        // The first pass: both version-4 rows are stored, the version-6 transaction is reported.
        let listing = [a_id, unreadable_id, b_id];
        let (rows, status) = drive_one_pass(&served, &listing, vec![]);
        assert_eq!(rows.len(), 2);
        assert!(present(&rows, a_id) && present(&rows, b_id));
        let list = reported(&status);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].0, text(unreadable_id));
        assert!(list[0].1.contains("unsupported transaction version 6"), "{}", list[0].1);

        // The next pass reads it as a version-4 transaction: it is stored and the list is empty.
        let c_id = id_of(&c);
        served.lock().unwrap().insert(unreadable_id, Served::Tx(c));
        let (rows, status) = drive_one_pass(&served, &listing, rows);
        assert_eq!(rows.len(), 3);
        assert!(present(&rows, a_id) && present(&rows, b_id) && present(&rows, c_id));
        assert!(
            plain_finished(&status),
            "an empty list leaves the plain answer: {status}"
        );
    }

    #[test]
    fn a_transaction_listed_twice_is_reported_once() {
        let (a, b) = (v4_tx(None, 100_000), v4_tx(None, 200_000));
        let (a_id, b_id) = (id_of(&a), id_of(&b));
        let unreadable_id = [0x22u8; 32];
        let served = backend(vec![
            (a_id, Served::Tx(a)),
            (unreadable_id, Served::Tx(v6_tx())),
            (b_id, Served::Tx(b)),
        ]);
        let (rows, status) = drive_one_pass(&served, &[a_id, unreadable_id, unreadable_id, b_id], vec![]);
        assert_eq!(rows.len(), 2);
        assert_eq!(reported(&status).len(), 1);
    }

    #[test]
    fn a_transaction_with_an_unreadable_input_transaction_is_reported_with_the_input_reason() {
        let (a, b) = (v4_tx(None, 100_000), v4_tx(None, 200_000));
        let spending = v4_tx(Some(([0xc1u8; 32], 0)), 400_000);
        let (a_id, b_id, spending_id) = (id_of(&a), id_of(&b), id_of(&spending));
        let served = backend(vec![
            (a_id, Served::Tx(a)),
            (spending_id, Served::Tx(spending)),
            ([0xc1u8; 32], Served::Tx(v6_tx())),
            (b_id, Served::Tx(b)),
        ]);
        let (rows, status) = drive_one_pass(&served, &[a_id, spending_id, b_id], vec![]);
        assert_eq!(rows.len(), 2);
        assert!(present(&rows, a_id) && present(&rows, b_id));
        let list = reported(&status);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].0, text(spending_id));
        assert!(
            list[0].1.contains("an input transaction cannot be parsed")
                && list[0].1.contains("unsupported transaction version 6"),
            "{}",
            list[0].1
        );
    }

    #[test]
    fn a_transaction_that_spends_a_missing_output_is_reported() {
        let (a, b) = (v4_tx(None, 100_000), v4_tx(None, 200_000));
        let spending = v4_tx(Some(([0xc2u8; 32], 5)), 500_000);
        let (a_id, b_id, spending_id) = (id_of(&a), id_of(&b), id_of(&spending));
        let served = backend(vec![
            (a_id, Served::Tx(a.clone())),
            (spending_id, Served::Tx(spending)),
            // The input transaction reads, but has one output only.
            ([0xc2u8; 32], Served::Tx(a)),
            (b_id, Served::Tx(b)),
        ]);
        let (rows, status) = drive_one_pass(&served, &[a_id, spending_id, b_id], vec![]);
        assert_eq!(rows.len(), 2);
        let list = reported(&status);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].0, text(spending_id));
        assert!(
            list[0].1.contains("an input spends an output that does not exist")
                && list[0].1.contains("it has 1 outputs"),
            "{}",
            list[0].1
        );
    }

    #[test]
    fn a_transport_error_is_not_reported_and_the_transaction_is_tried_again() {
        let (a, b, c) = (v4_tx(None, 100_000), v4_tx(None, 200_000), v4_tx(None, 300_000));
        let (a_id, b_id, c_id) = (id_of(&a), id_of(&b), id_of(&c));
        let failing_id = [0x55u8; 32];
        let served = backend(vec![
            (a_id, Served::Tx(a)),
            (failing_id, Served::Transport),
            (b_id, Served::Tx(b)),
        ]);
        let listing = [a_id, failing_id, b_id];
        let (rows, status) = drive_one_pass(&served, &listing, vec![]);
        // Skipped, as today: no row, and no report.
        assert_eq!(rows.len(), 2);
        assert!(
            plain_finished(&status),
            "a transport error is not an unreadable transaction: {status}"
        );

        // The backend answers on the next pass: the transaction is stored then.
        served.lock().unwrap().insert(failing_id, Served::Tx(c));
        let (rows, status) = drive_one_pass(&served, &listing, rows);
        assert_eq!(rows.len(), 3);
        assert!(present(&rows, c_id));
        assert!(plain_finished(&status));
    }

    #[test]
    fn a_pass_without_a_skip_answers_exactly_as_before() {
        let (a, b) = (v4_tx(None, 100_000), v4_tx(None, 200_000));
        let (a_id, b_id) = (id_of(&a), id_of(&b));
        let served = backend(vec![(a_id, Served::Tx(a)), (b_id, Served::Tx(b))]);
        let (rows, status) = drive_one_pass(&served, &[a_id, b_id], vec![]);
        assert_eq!(rows.len(), 2);
        assert_eq!(json::to_string(&status).unwrap(), r#"{"state":"Finished"}"#);
    }
}
