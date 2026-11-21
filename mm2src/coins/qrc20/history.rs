use super::*;
use crate::history_absence::{removed_rows_line, AbsenceMarks, ChainChangeWatch};
use crate::utxo::rpc_clients::electrum_pool::every_server_answered_an_error;
use crate::utxo::rpc_clients::UtxoJsonRpcClientInfo;
use crate::utxo::{RequestTxHistoryResult, UtxoFeeDetails};
use crate::{CoinsContext, MmCoinEnum, TxFeeDetails, TxHistoryResult};
use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use common::jsonrpc_client::JsonRpcErrorType;
use common::log::info;
use itertools::Itertools;
use mm2_metrics::MetricsArc;
use script_pubkey::{extract_contract_call_from_script, extract_gas_from_script, ExtractGasEnum};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use utxo_common::{HISTORY_TOO_LARGE_ERROR, HISTORY_TOO_LARGE_ERR_CODE};

/// The code of the history state error of a collector that ended because no listed server answers the contract-event
/// history (CRD ch.38 amendment of 2026-11-17 after R38.6.4Q.6). `history too large` has [`HISTORY_TOO_LARGE_ERR_CODE`].
const HISTORY_NO_SERVER_ANSWERS_ERR_CODE: i64 = -2;
/// How many passes in a row have to fail their history request on every server tried to end the collector.
const ALL_ERROR_PASSES_LIMIT: u32 = 3;

/// How the history request of a pass ended, as far as the end state of the collector counts it (the amendment after
/// R38.6.4Q.6, "Counting").
#[derive(Clone, Copy, Debug, PartialEq)]
enum HistoryPassKind {
    /// Every server tried answered with an error answer: the pass counts.
    EveryServerAnsweredAnError,
    /// A server failed by transport (or no server could be tried): the pass neither counts nor resets.
    TransportFailure,
    /// Anything else: an answer, an unreadable answer, `history too large`, a failure that is not the servers'.
    Other,
}

/// The passes in a row whose history request got an error answer from every server tried (the amendment after
/// R38.6.4Q.6). A pass that gets any other outcome of the request that is not a transport failure resets the count; a
/// pass in which a server failed by transport, a failed balance read and an idle pass change nothing.
#[derive(Debug, Default)]
struct AllErrorPasses(u32);

impl AllErrorPasses {
    /// Takes the kind of one pass; `true` when that pass ends the collector.
    fn pass(&mut self, kind: HistoryPassKind) -> bool {
        match kind {
            HistoryPassKind::EveryServerAnsweredAnError => {
                self.0 += 1;
                self.0 >= ALL_ERROR_PASSES_LIMIT
            },
            HistoryPassKind::TransportFailure => false,
            HistoryPassKind::Other => {
                self.0 = 0;
                false
            },
        }
    }
}

type TxTransferMap = HashMap<TxInternalId, TransactionDetails>;
type HistoryMapByHash = HashMap<H256Json, TxTransferMap>;
type TxIds = Vec<(H256Json, u64)>;

const HISTORY_RETRY_INITIAL_DELAY_SECS: u64 = 10;
const HISTORY_RETRY_MAX_DELAY_SECS: u64 = 300;

fn next_history_retry_delay(current_delay_secs: u64) -> u64 {
    current_delay_secs.saturating_mul(2).min(HISTORY_RETRY_MAX_DELAY_SECS)
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TxInternalId {
    tx_hash: H256Json,
    output_index: u64,
    log_index: u64,
}

impl TxInternalId {
    pub fn new(tx_hash: H256Json, output_index: u64, log_index: u64) -> TxInternalId {
        TxInternalId {
            tx_hash,
            output_index,
            log_index,
        }
    }

    /// TODO use parity_bitcoin::serialization instead of this custom implementation
    pub fn from_bytes(bytes: &BytesJson) -> Result<TxInternalId, String> {
        // H256(32 bytes) + output_index(8 bytes) + log_index(8 bytes)
        const EXPECTED_LEN: usize = 32 + 8 + 8;

        if bytes.len() != EXPECTED_LEN {
            return ERR!("Incorrect bytes len {}, expected {}", bytes.len(), EXPECTED_LEN);
        }

        let tx_hash: H256Json = bytes[0..32].into();

        let buf = bytes[32..].to_vec();
        let mut cursor = Cursor::new(buf);
        let output_index = cursor.read_u64::<BigEndian>().unwrap();
        let log_index = cursor.read_u64::<BigEndian>().unwrap();

        Ok(TxInternalId {
            tx_hash,
            output_index,
            log_index,
        })
    }
}

impl From<TxInternalId> for BytesJson {
    fn from(id: TxInternalId) -> Self {
        let mut bytes = id.tx_hash.0.to_vec();
        bytes
            .write_u64::<BigEndian>(id.output_index)
            .expect("Error on write_u64");
        bytes.write_u64::<BigEndian>(id.log_index).expect("Error on write_u64");
        bytes.into()
    }
}

#[derive(Debug, PartialEq)]
enum ProcessCachedTransferMapResult {
    Updated,
    UpdateIsNotNeeded,
    ReloadIsRequired,
}

impl Qrc20Coin {
    /// Runs the history collector of this instance until it ends of itself or the token is disabled: the disable aborts
    /// it wherever it is (CRD ch.38 D38.6.4Q.5 status note, R38.6.4Q.10 item 1), and the instance's client closes with
    /// the last handle of it. One instance has one collector: a second start of the same instance returns at once.
    pub async fn history_loop(self, ctx: MmArc) {
        let Some(registration) = self.history_abort.take_registration() else {
            return;
        };
        let _ended_by_abort = futures::future::Abortable::new(self.history_loop_body(ctx), registration).await;
    }

    async fn history_loop_body(&self, ctx: MmArc) {
        let mut history_map = match self.try_load_history_from_file(&ctx).await {
            Ok(history) => history,
            Err(e) => {
                ctx.log.log(
                    "😟",
                    &[&"tx_history", &self.utxo.conf.ticker],
                    &ERRL!("Error {} on load history from file, stop the history loop", e),
                );
                return;
            },
        };

        let mut my_balance: Option<CoinBalance> = None;
        let mut success_iteration = 0i32;
        let mut history_retry_delay_secs = HISTORY_RETRY_INITIAL_DELAY_SECS;
        // A pass that left a listed transaction out, because a detail request failed, is incomplete: the next pass asks
        // for the history again after the retry delay, whatever the balance (code-quality finding after R38.6.4Q.6).
        let mut incomplete_pass = false;
        let mut incomplete_retry_delay_secs = HISTORY_RETRY_INITIAL_DELAY_SECS;
        let mut all_error_passes = AllErrorPasses::default();
        // A repeating failure of one source is logged once and then summarized (the retry delays
        // below are this loop's own and stay as they are). The two sources are independent.
        let mut balance_failures = utxo_common::FailureThrottle::default();
        let mut history_failures = utxo_common::FailureThrottle::default();
        // The absence rule (CRD ch.38 §38.5C): memory only, so a restart starts counting again.
        let mut absence = AbsenceMarks::default();
        // The chain-changed events of the coin's client: one makes the next pass run at once (CRD ch.38 §38.5C
        // amendment of 2026-11-18, item 3).
        let mut chain_changes = ChainChangeWatch::of(&self.utxo.rpc_client);
        let mut chain_changed = false;
        loop {
            if ctx.is_stopping() {
                break;
            };
            {
                let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();
                let coins = coins_ctx.coins.lock().await;
                // The ticker has to be registered, and registered as this instance: the token may have been disabled
                // and enabled again (CRD ch.38 D38.6.4Q.5 status note, item 2). A passive platform is still
                // registered, and its background work goes on (CRD ch.43 R43.1E.6 item 3).
                let registered = matches!(
                    coins.registered().get(&self.utxo.conf.ticker),
                    Some(MmCoinEnum::Qrc20Coin(registered)) if Arc::ptr_eq(&registered.0, &self.0)
                );
                if !registered {
                    ctx.log
                        .log("", &[&"tx_history", &self.utxo.conf.ticker], "Loop stopped");
                    break;
                };
            }

            let actual_balance = match self.my_balance().compat().await {
                Ok(b) => {
                    balance_failures.record_success(&ctx, &self.utxo.conf.ticker, "balance read");
                    b
                },
                Err(err) => {
                    balance_failures.record_failure(
                        &ctx,
                        &self.utxo.conf.ticker,
                        "balance read",
                        &err.get_inner().to_string(),
                    );
                    Timer::sleep(10.).await;
                    continue;
                },
            };

            chain_changed |= chain_changes.take();
            let need_update = self.check_if_history_update_is_needed(
                &history_map,
                &my_balance,
                &actual_balance,
                &absence,
                chain_changed || incomplete_pass,
            );
            if !need_update {
                // The balance read worked and nothing is pending, so the server answers again: end a
                // history backoff quietly (the balance source logs its own recovery). While the
                // balance differs or an update is pending this branch is not taken, so a server that
                // answers the balance and fails the history keeps its backoff.
                history_failures.reset();
                // An event of the chain-changed channel ends the wait; the next round takes it.
                let idle = Timer::sleep(30.);
                futures::pin_mut!(idle);
                let event = chain_changes.wait();
                futures::pin_mut!(event);
                futures::future::select(idle, event).await;
                continue;
            }

            let metrics = ctx.metrics.clone();
            let mut removed_rows = false;
            let (history_outcome, pass_kind) = self.request_tx_history_counted(metrics).await;
            if all_error_passes.pass(pass_kind) {
                ctx.log.log(
                    "😟",
                    &[&"tx_history", &self.utxo.conf.ticker],
                    &ERRL!(
                        "{} passes in a row got only error answers to the history request, stopping further attempts to retrieve it",
                        ALL_ERROR_PASSES_LIMIT
                    ),
                );
                *self.utxo.history_sync_state.lock().unwrap() = HistorySyncState::Error(json!({
                    "code": HISTORY_NO_SERVER_ANSWERS_ERR_CODE,
                    "message": "No listed Electrum server answers the contract-event history. History is not available",
                }));
                break;
            }
            let tx_ids = match history_outcome {
                RequestTxHistoryResult::Ok(tx_ids, qualifying_tip) => {
                    history_failures.record_success(&ctx, &self.utxo.conf.ticker, "history request");
                    // The pass the event asked for has its listing.
                    chain_changed = false;
                    // The removal comes first, before any detail request of the pass (R38.6.4Q.6, D38.5C.1).
                    if let Some(tip) = qualifying_tip {
                        removed_rows = self.remove_unlisted_rows(&mut absence, &mut history_map, tip, &tx_ids);
                    }
                    tx_ids
                },
                RequestTxHistoryResult::Retry { error } => {
                    history_failures.record_failure(&ctx, &self.utxo.conf.ticker, "history request", &error);
                    Timer::sleep(history_retry_delay_secs as f64).await;
                    history_retry_delay_secs = next_history_retry_delay(history_retry_delay_secs);
                    continue;
                },
                RequestTxHistoryResult::HistoryTooLarge => {
                    ctx.log.log(
                        "😟",
                        &[&"tx_history", &self.utxo.conf.ticker],
                        &ERRL!("Got `history too large`, stopping further attempts to retrieve it"),
                    );
                    *self.utxo.history_sync_state.lock().unwrap() = HistorySyncState::Error(json!({
                        "code": HISTORY_TOO_LARGE_ERR_CODE,
                        "message": "Got `history too large` error from Electrum server. History is not available",
                    }));
                    break;
                },
                RequestTxHistoryResult::CriticalError(e) => {
                    ctx.log.log(
                        "😟",
                        &[&"tx_history", &self.utxo.conf.ticker],
                        &ERRL!("{}, stopping futher attempts to retreive it", e),
                    );
                    break;
                },
            };
            history_retry_delay_secs = HISTORY_RETRY_INITIAL_DELAY_SECS;

            // Rows removed above are written even when nothing else changed.
            let listed: Vec<H256Json> = tx_ids.iter().map(|(hash, _)| *hash).collect();
            let updated = self.process_tx_ids(&ctx, &mut history_map, tx_ids).await || removed_rows;
            // A listed transaction that is not stored after the pass was left out because its details could not be read.
            let left_out = listed.iter().any(|hash| !history_map.contains_key(hash));
            if success_iteration == 0 {
                ctx.log.log(
                    "😅",
                    &[&"tx_history", &("coin", self.utxo.conf.ticker.clone().as_str())],
                    "history has been loaded successfully",
                );
            }

            my_balance = Some(actual_balance);
            success_iteration += 1;
            incomplete_pass = left_out;
            // The next pass of an incomplete one comes after the retry delay; consecutive incomplete passes back off
            // like failed history requests do, and a complete pass starts the delay again.
            let wait_after_pass_secs = incomplete_retry_delay_secs;
            incomplete_retry_delay_secs = if left_out {
                next_history_retry_delay(incomplete_retry_delay_secs)
            } else {
                HISTORY_RETRY_INITIAL_DELAY_SECS
            };

            if !updated {
                if left_out {
                    Timer::sleep(wait_after_pass_secs as f64).await;
                }
                continue;
            }

            // `history_map` has been updated.
            let mut to_write: Vec<TransactionDetails> = history_map
                .iter()
                .flat_map(|(_, value)| value)
                .map(|(_tx_id, tx)| tx.clone())
                .collect();
            // rows of one transaction share the leading `internal_id` bytes, so they stay adjacent
            // with the lower output/event index first
            to_write.sort_unstable_by(utxo_common::legacy_history_order);
            if let Err(e) = self.save_history_to_file(&ctx, to_write).compat().await {
                ctx.log.log(
                    "",
                    &[&"tx_history", &self.as_ref().conf.ticker],
                    &ERRL!("Error {} on 'save_history_to_file', stop the history loop", e),
                );
                return;
            }
            if left_out {
                Timer::sleep(wait_after_pass_secs as f64).await;
            }
        }
    }

    pub async fn transfer_details_by_hash(&self, tx_hash: H256Json) -> Result<TxTransferMap, String> {
        let receipts = try_s!(self.utxo.rpc_client.get_transaction_receipts(&tx_hash).compat().await);
        // request Qtum transaction details to get a tx_hex, timestamp, block_height and calculate a miner_fee
        let mut input_transactions = HistoryUtxoTxMap::new();
        let qtum_details = try_s!(utxo_common::tx_details_by_hash(self, &tx_hash.0, &mut input_transactions).await);
        // Deserialize the UtxoTx to get a script pubkey
        let qtum_tx: UtxoTx = try_s!(deserialize(qtum_details.tx_hex.as_slice()).map_err(|e| ERRL!("{:?}", e)));

        let miner_fee = {
            let total_qtum_fee = match qtum_details.fee_details {
                Some(TxFeeDetails::Utxo(UtxoFeeDetails { ref amount, .. })) => amount.clone(),
                Some(ref fee) => return ERR!("Unexpected fee details {:?}", fee),
                None => return ERR!("No Qtum fee details"),
            };
            let total_gas_used = receipts.iter().fold(0, |gas, receipt| gas + receipt.gas_used);
            let total_gas_used = big_decimal_from_sat(total_gas_used as i64, self.utxo.decimals);
            total_qtum_fee - total_gas_used
        };

        let mut details = TxTransferMap::new();
        for receipt in receipts {
            let log_details =
                try_s!(self.transfer_details_from_receipt(&qtum_tx, &qtum_details, receipt, miner_fee.clone()));
            details.extend(log_details.into_iter())
        }

        Ok(details)
    }

    fn transfer_details_from_receipt(
        &self,
        qtum_tx: &UtxoTx,
        qtum_details: &TransactionDetails,
        receipt: TxReceipt,
        miner_fee: BigDecimal,
    ) -> Result<TxTransferMap, String> {
        let my_address = try_s!(self.utxo.derivation_method.iguana_or_err());
        let tx_hash: H256Json = try_s!(H256Json::from_str(&qtum_details.tx_hash));
        if qtum_tx.outputs.len() <= (receipt.output_index as usize) {
            return ERR!(
                "Length of the transaction {:?} outputs less than output_index {}",
                tx_hash,
                receipt.output_index
            );
        }
        let script_pubkey: Script = qtum_tx.outputs[receipt.output_index as usize]
            .script_pubkey
            .clone()
            .into();
        let fee_details = {
            let gas_limit = try_s!(extract_gas_from_script(&script_pubkey, ExtractGasEnum::GasLimit));
            let gas_price = try_s!(extract_gas_from_script(&script_pubkey, ExtractGasEnum::GasPrice));

            let total_gas_fee = utxo_common::big_decimal_from_sat(receipt.gas_used as i64, self.utxo.decimals);
            Qrc20FeeDetails {
                // QRC20 fees are paid in base platform currency (particular in Qtum)
                coin: self.platform.clone(),
                miner_fee,
                gas_limit,
                gas_price,
                total_gas_fee,
            }
        };

        let mut details = TxTransferMap::new();
        for (log_index, log_entry) in receipt.log.into_iter().enumerate() {
            if log_entry.topics.len() != 3 {
                continue;
            }
            // the first topic should be ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef
            // https://github.com/qtumproject/qtum-electrum/blob/v4.0.2/electrum/wallet.py#L2101
            if log_entry.topics[0] != QRC20_TRANSFER_TOPIC {
                continue;
            }
            if try_s!(log_entry.parse_address()) != self.contract_address {
                continue;
            }

            let (total_amount, from, to) = {
                let event = try_s!(transfer_event_from_log(&log_entry));
                // https://github.com/qtumproject/qtum-electrum/blob/v4.0.2/electrum/wallet.py#L2093
                if event.contract_address != self.contract_address {
                    // contract address mismatch
                    continue;
                }
                let amount = try_s!(u256_to_big_decimal(event.amount, self.decimals()));
                let from = self.utxo_addr_from_contract_addr(event.sender);
                let to = self.utxo_addr_from_contract_addr(event.receiver);
                (amount, from, to)
            };

            // https://github.com/qtumproject/qtum-electrum/blob/v4.0.2/electrum/wallet.py#L2102
            if from != *my_address && to != *my_address {
                // address mismatch
                continue;
            }

            let spent_by_me = if from == *my_address {
                total_amount.clone()
            } else {
                0.into()
            };
            let received_by_me = if to == *my_address {
                total_amount.clone()
            } else {
                0.into()
            };

            // do not inherit the block_height from qtum_tx (usually it is None)
            let block_height = receipt.block_number;
            let my_balance_change = &received_by_me - &spent_by_me;
            let internal_id = TxInternalId::new(tx_hash, receipt.output_index, log_index as u64);

            let from = if is_transferred_from_contract(&script_pubkey) {
                try_s!(qtum::display_as_contract_address(from))
            } else {
                try_s!(from.display_address())
            };

            let to = if is_transferred_to_contract(&script_pubkey) {
                try_s!(qtum::display_as_contract_address(to))
            } else {
                try_s!(to.display_address())
            };

            let tx_details = TransactionDetails {
                unsigned: None,
                tx_json: None,
                from: vec![from],
                to: vec![to],
                total_amount,
                spent_by_me,
                received_by_me,
                my_balance_change,
                block_height,
                fee_details: Some(fee_details.clone().into()),
                internal_id: internal_id.clone().into(),
                ..qtum_details.clone()
            };

            details.insert(internal_id, tx_details);
        }

        Ok(details)
    }

    #[cfg(test)]
    async fn request_tx_history(&self, metrics: MetricsArc) -> RequestTxHistoryResult {
        self.request_tx_history_counted(metrics).await.0
    }

    /// The outcome of the history request and its [`HistoryPassKind`]. Only a call that failed with every server tried
    /// answering an error answer counts towards the end of the collector; the pool says so in the text of its failure
    /// (the amendment after R38.6.4Q.6, "Counting").
    async fn request_tx_history_counted(&self, metrics: MetricsArc) -> (RequestTxHistoryResult, HistoryPassKind) {
        mm_counter!(metrics, "tx.history.request.count", 1,
                    "coin" => self.utxo.conf.ticker.clone(), "client" => "electrum", "method" => "blockchain.contract.event.get_history");
        let history_res = TransferHistoryBuilder::new(self.clone()).build_tx_idents().await;
        let (history, qualifying_tip) = match history_res {
            Ok(h) => h,
            Err(e) => match e.into_inner() {
                UtxoRpcError::Transport(json_rpc_e) | UtxoRpcError::ResponseParseError(json_rpc_e) => {
                    match json_rpc_e.error {
                        JsonRpcErrorType::Response(_addr, err) => {
                            return if HISTORY_TOO_LARGE_ERROR.eq(&err) {
                                (RequestTxHistoryResult::HistoryTooLarge, HistoryPassKind::Other)
                            } else {
                                (
                                    RequestTxHistoryResult::Retry {
                                        error: ERRL!("Error {:?} on blockchain_contract_event_get_history", err),
                                    },
                                    HistoryPassKind::Other,
                                )
                            }
                        },
                        // The pool reports a call that failed on every server it tried as a transport failure.
                        JsonRpcErrorType::Transport(err) => {
                            // The failure names the client, which is the platform's on a shared client.
                            let kind = match self.utxo.rpc_client {
                                UtxoRpcClientEnum::Electrum(ref client)
                                    if every_server_answered_an_error(client.coin_name(), &err) =>
                                {
                                    HistoryPassKind::EveryServerAnsweredAnError
                                },
                                UtxoRpcClientEnum::Electrum(_) => HistoryPassKind::TransportFailure,
                                _ => HistoryPassKind::Other,
                            };
                            return (
                                RequestTxHistoryResult::Retry {
                                    error: ERRL!("Error {} on blockchain_contract_event_get_history", err),
                                },
                                kind,
                            );
                        },
                        JsonRpcErrorType::InvalidRequest(err) | JsonRpcErrorType::Parse(_, err) => {
                            return (
                                RequestTxHistoryResult::Retry {
                                    error: ERRL!("Error {} on blockchain_contract_event_get_history", err),
                                },
                                HistoryPassKind::Other,
                            );
                        },
                    }
                },
                UtxoRpcError::InvalidResponse(e) | UtxoRpcError::Internal(e) => {
                    return (
                        RequestTxHistoryResult::Retry {
                            error: ERRL!("Error {} on blockchain_contract_event_get_history", e),
                        },
                        HistoryPassKind::Other,
                    )
                },
            },
        };
        mm_counter!(metrics, "tx.history.response.count", 1,
                    "coin" => self.utxo.conf.ticker.clone(), "client" => "electrum", "method" => "blockchain.contract.event.get_history");

        mm_counter!(metrics, "tx.history.response.total_length", history.len() as u64,
                    "coin" => self.utxo.conf.ticker.clone(), "client" => "electrum", "method" => "blockchain.contract.event.get_history");

        (
            RequestTxHistoryResult::Ok(history, qualifying_tip),
            HistoryPassKind::Other,
        )
    }

    fn check_if_history_update_is_needed(
        &self,
        history: &HistoryMapByHash,
        last_balance: &Option<CoinBalance>,
        actual_balance: &CoinBalance,
        absence: &AbsenceMarks,
        must_ask: bool,
    ) -> bool {
        // `must_ask`: the chain changed since the last listing, or the last pass was incomplete.
        let need_update = must_ask
            || history
                .iter()
                .flat_map(|(_, txs)| txs)
                .any(|(_, tx)| tx.should_update_timestamp() || tx.should_update_block_height());
        match last_balance {
            // A mark of the absence rule keeps the test true: the second listing does not wait for a balance change.
            Some(last_balance) if last_balance == actual_balance && !need_update && !absence.any() => {
                // my balance hasn't been changed, there is no need to reload tx_history
                false
            },
            _ => true,
        }
    }

    /// Applies one qualifying listing taken at `tip` to the absence marks and removes the transactions whose second
    /// absence it is, with every transfer row of each (CRD ch.38 §38.5C, D38.5C.1 status note). Returns whether any row
    /// was removed; the removal is logged once.
    fn remove_unlisted_rows(
        &self,
        absence: &mut AbsenceMarks,
        history_map: &mut HistoryMapByHash,
        tip: u64,
        tx_ids: &TxIds,
    ) -> bool {
        let listed: HashSet<H256Json> = tx_ids.iter().map(|(hash, _)| *hash).collect();
        let to_remove = absence.judge(tip, &listed, history_map.keys().copied());
        let removed: usize = to_remove
            .iter()
            .filter_map(|hash| history_map.remove(hash))
            .map(|rows| rows.len())
            .sum();
        if removed > 0 {
            info!("{}", removed_rows_line(&self.utxo.conf.ticker, removed));
        }
        removed > 0
    }

    /// Returns true if the `history_map` has been updated.
    async fn process_cached_tx_transfer_map(
        &self,
        ctx: &MmArc,
        tx_hash: &H256Json,
        tx_height: u64,
        transfer_map: &mut TxTransferMap,
    ) -> ProcessCachedTransferMapResult {
        async fn get_verbose_transaction(coin: &Qrc20Coin, ctx: &MmArc, tx_hash: H256Json) -> Option<RpcTransaction> {
            mm_counter!(ctx.metrics, "tx.history.request.count", 1, "coin" => coin.utxo.conf.ticker.clone(), "method" => "get_verbose_transaction");
            match coin.utxo.rpc_client.get_verbose_transaction(&tx_hash).compat().await {
                Ok(d) => {
                    mm_counter!(ctx.metrics, "tx.history.response.count", 1, "coin" => coin.utxo.conf.ticker.clone(), "method" => "get_verbose_transaction");
                    Some(d)
                },
                Err(e) => {
                    ctx.log.log(
                        "😟",
                        &[&"tx_history", &coin.utxo.conf.ticker],
                        &ERRL!("Error {:?} on get_verbose_transaction for {:?} tx", e, tx_hash),
                    );
                    None
                },
            }
        }

        // `qtum_verbose` will be initialized once if it's required
        let mut qtum_verbose = None;

        let mut updated = false;
        for (id, tx) in transfer_map {
            if id.tx_hash != *tx_hash {
                ctx.log.log(
                    "😟",
                    &[&"tx_history", &self.utxo.conf.ticker],
                    &ERRL!(
                        "Warning: TxTransferMap contains entries with the different tx_hash {:?}, expected {:?}",
                        id.tx_hash,
                        tx_hash
                    ),
                );
                return ProcessCachedTransferMapResult::ReloadIsRequired;
            }

            // update block height for previously unconfirmed transaction
            if tx.should_update_block_height() && tx_height > 0 {
                tx.block_height = tx_height;
                updated = true;
            }
            // A confirmed row listed at another height takes it over, with that block's time; one listed as
            // unconfirmed goes back to height 0. All rows of the transaction change together (D38.5C.2 status note).
            match utxo_common::listed_height_change(tx.block_height, tx_height) {
                utxo_common::HeightChange::Moved => {
                    if qtum_verbose.is_none() {
                        qtum_verbose = get_verbose_transaction(self, ctx, *tx_hash).await;
                    }
                    // Without the block's time the row is left as it is and the next pass tries again.
                    if let Some(ref qtum_verbose) = qtum_verbose {
                        tx.block_height = tx_height;
                        tx.timestamp = qtum_verbose.time as u64;
                        updated = true;
                    }
                },
                utxo_common::HeightChange::Unconfirmed => {
                    tx.block_height = 0;
                    updated = true;
                },
                utxo_common::HeightChange::Same => (),
            }
            if tx.should_update_timestamp() {
                if qtum_verbose.is_none() {
                    qtum_verbose = get_verbose_transaction(self, ctx, *tx_hash).await;
                }
                if let Some(ref qtum_verbose) = qtum_verbose {
                    tx.timestamp = qtum_verbose.time as u64;
                    updated = true;
                } // else `UtxoRpcClientEnum::get_verbose_transaction` failed for some reason
            }
        }

        if updated {
            ProcessCachedTransferMapResult::Updated
        } else {
            ProcessCachedTransferMapResult::UpdateIsNotNeeded
        }
    }

    /// Returns true if the `history_map` has been updated.
    async fn process_tx_ids(&self, ctx: &MmArc, history_map: &mut HistoryMapByHash, tx_ids: TxIds) -> bool {
        let mut transactions_left = if history_map.len() < tx_ids.len() {
            tx_ids.len() - history_map.len()
        } else {
            0
        };
        *self.utxo.history_sync_state.lock().unwrap() =
            HistorySyncState::InProgress(json!({ "transactions_left": transactions_left }));

        let mut updated = false;
        for (tx_hash, height) in tx_ids {
            // first check if the `transfer` details are initialized for the `tx_hash`
            if let Some(tx_hash_history) = history_map.get_mut(&tx_hash) {
                // we should check if the cached `transfer` details are up-to-date (timestamp and blockheight are not zeros)
                match self
                    .process_cached_tx_transfer_map(ctx, &tx_hash, height, tx_hash_history)
                    .await
                {
                    ProcessCachedTransferMapResult::Updated => {
                        updated = true;
                        continue;
                    },
                    ProcessCachedTransferMapResult::UpdateIsNotNeeded => continue,
                    ProcessCachedTransferMapResult::ReloadIsRequired => (),
                }
            }

            // `transfer` details are not initialized for the `tx_hash`
            // or there is an error in cached `tx_hash_history`
            mm_counter!(ctx.metrics, "tx.history.request.count", 1, "coin" => self.utxo.conf.ticker.clone(), "method" => "transfer_details_by_hash");
            let tx_hash_history = match self.transfer_details_by_hash(tx_hash).await {
                Ok(d) => d,
                Err(e) => {
                    ctx.log.log(
                        "😟",
                        &[&"tx_history", &self.utxo.conf.ticker],
                        &ERRL!("Error {:?} on getting the details of {:?}, skipping the tx", e, tx_hash),
                    );
                    continue;
                },
            };

            let tx_history_records: Vec<_> = tx_hash_history.values().cloned().collect();
            if history_map.insert(tx_hash, tx_hash_history).is_some() {
                ctx.log.log(
                    "😟",
                    &[&"tx_history", &self.utxo.conf.ticker],
                    &format!("'transfer' details of {:?} were reloaded", tx_hash),
                );
            }
            crate::tx_history_streaming::publish_tx_history_records(ctx, &self.utxo.conf.ticker, tx_history_records);

            mm_counter!(ctx.metrics, "tx.history.response.count", 1, "coin" => self.utxo.conf.ticker.clone(), "method" => "transfer_details_by_hash");
            if transactions_left > 0 {
                transactions_left -= 1;
                *self.utxo.history_sync_state.lock().unwrap() =
                    HistorySyncState::InProgress(json!({ "transactions_left": transactions_left }));
            }

            updated = true;
        }

        *self.utxo.history_sync_state.lock().unwrap() = HistorySyncState::Finished;
        updated
    }

    async fn try_load_history_from_file(&self, ctx: &MmArc) -> TxHistoryResult<HistoryMapByHash> {
        let history = self.load_history_from_file(ctx).compat().await?;
        let mut history_map: HistoryMapByHash = HashMap::default();

        for tx in history {
            let id = match TxInternalId::from_bytes(&tx.internal_id) {
                Ok(i) => i,
                Err(e) => {
                    ctx.log.log(
                        "😟",
                        &[&"tx_history", &self.utxo.conf.ticker],
                        &ERRL!("Error {:?} on load history from file", e),
                    );
                    return Ok(HistoryMapByHash::default());
                },
            };
            let tx_hash_history = history_map.entry(id.tx_hash).or_insert_with(HashMap::default);
            if tx_hash_history.insert(id, tx).is_some() {
                ctx.log.log(
                    "😟",
                    &[&"tx_history", &self.utxo.conf.ticker],
                    &ERRL!("History file contains entries with the same 'internal_id'"),
                );
                return Ok(HistoryMapByHash::default());
            }
        }

        Ok(history_map)
    }
}

pub struct TransferHistoryBuilder {
    coin: Qrc20Coin,
    from_block: u64,
    address: Option<H160>,
    token_address: H160,
}

struct TransferHistoryParams {
    from_block: u64,
    address: H160,
    token_address: H160,
}

impl TransferHistoryBuilder {
    pub fn new(coin: Qrc20Coin) -> TransferHistoryBuilder {
        let token_address = coin.contract_address;
        TransferHistoryBuilder {
            coin,
            from_block: 0,
            address: None,
            token_address,
        }
    }

    #[allow(clippy::wrong_self_convention)]
    pub fn from_block(mut self, from_block: u64) -> TransferHistoryBuilder {
        self.from_block = from_block;
        self
    }

    pub fn address(mut self, address: H160) -> TransferHistoryBuilder {
        self.address = Some(address);
        self
    }

    #[allow(dead_code)]
    pub fn token_address(mut self, token_address: H160) -> TransferHistoryBuilder {
        self.token_address = token_address;
        self
    }

    pub async fn build(self) -> Result<Vec<TxReceipt>, MmError<UtxoRpcError>> {
        let params = self.build_params()?;
        self.coin.utxo.rpc_client.build(params).await
    }

    pub async fn build_tx_idents(self) -> Result<(Vec<(H256Json, u64)>, Option<u64>), MmError<UtxoRpcError>> {
        let params = self.build_params()?;
        self.coin.utxo.rpc_client.build_tx_idents(params).await
    }

    fn build_params(&self) -> Result<TransferHistoryParams, MmError<UtxoRpcError>> {
        let address = match self.address {
            Some(addr) => addr,
            None => {
                let my_address = self
                    .coin
                    .utxo
                    .derivation_method
                    .iguana_or_err()
                    .mm_err(|e| UtxoRpcError::Internal(e.to_string()))?;
                qtum::contract_addr_from_utxo_addr(my_address.clone())
                    .mm_err(|e| UtxoRpcError::Internal(e.to_string()))?
            },
        };

        Ok(TransferHistoryParams {
            from_block: self.from_block,
            address,
            token_address: self.token_address,
        })
    }
}

#[async_trait]
trait BuildTransferHistory {
    async fn build(&self, params: TransferHistoryParams) -> Result<Vec<TxReceipt>, MmError<UtxoRpcError>>;

    /// The transactions with their heights, and the tip by which the listing may be judged for the absence rule (CRD
    /// ch.38 §38.5C): `Some` only when the listing is complete and the server that answered it was up to date.
    async fn build_tx_idents(
        &self,
        params: TransferHistoryParams,
    ) -> Result<(Vec<(H256Json, u64)>, Option<u64>), MmError<UtxoRpcError>>;
}

#[async_trait]
impl BuildTransferHistory for UtxoRpcClientEnum {
    async fn build(&self, params: TransferHistoryParams) -> Result<Vec<TxReceipt>, MmError<UtxoRpcError>> {
        match self {
            UtxoRpcClientEnum::Native(native) => native.build(params).await,
            UtxoRpcClientEnum::Electrum(electrum) => electrum.build(params).await,
            #[cfg(not(target_arch = "wasm32"))]
            UtxoRpcClientEnum::LightWalletD(_) => MmError::err(UtxoRpcError::Internal(
                "QRC20 history is not supported over lightwalletd".to_owned(),
            )),
        }
    }

    async fn build_tx_idents(
        &self,
        params: TransferHistoryParams,
    ) -> Result<(Vec<(H256Json, u64)>, Option<u64>), MmError<UtxoRpcError>> {
        match self {
            UtxoRpcClientEnum::Native(native) => native.build_tx_idents(params).await,
            UtxoRpcClientEnum::Electrum(electrum) => electrum.build_tx_idents(params).await,
            #[cfg(not(target_arch = "wasm32"))]
            UtxoRpcClientEnum::LightWalletD(_) => MmError::err(UtxoRpcError::Internal(
                "QRC20 history is not supported over lightwalletd".to_owned(),
            )),
        }
    }
}

#[async_trait]
impl BuildTransferHistory for ElectrumClient {
    async fn build(&self, params: TransferHistoryParams) -> Result<Vec<TxReceipt>, MmError<UtxoRpcError>> {
        let (tx_idents, _tip) = self.build_tx_idents(params).await?;

        let mut receipts = Vec::new();
        for (tx_hash, _height) in tx_idents {
            let mut tx_receipts = self.blockchain_transaction_get_receipt(&tx_hash).compat().await?;
            // remove receipts of contract calls didn't emit at least one `Transfer` event
            tx_receipts.retain(|receipt| receipt.log.iter().any(is_transfer_event_log));
            receipts.extend(tx_receipts.into_iter());
        }

        Ok(receipts)
    }

    async fn build_tx_idents(
        &self,
        params: TransferHistoryParams,
    ) -> Result<(Vec<(H256Json, u64)>, Option<u64>), MmError<UtxoRpcError>> {
        let address = contract_addr_into_rpc_format(&params.address);
        let token_address = contract_addr_into_rpc_format(&params.token_address);
        let (history, tip) = self
            .blockchain_contract_event_get_history_for_absence(&address, &token_address, QRC20_TRANSFER_TOPIC)
            .await?;

        Ok((
            history
                .into_iter()
                .filter(|item| params.from_block <= item.height)
                .map(|tx| (tx.tx_hash, tx.height))
                .unique()
                .collect(),
            tip,
        ))
    }
}

impl NativeClient {
    /// Searches the logs from `params.from_block` up to the block count read at the start and returns the receipts
    /// with that block count: the listing covers exactly up to it, so it is the tip the listing belongs to (CRD ch.38
    /// §38.5C), whatever blocks arrive while the search runs.
    async fn search_logs_up_to_tip(
        &self,
        params: TransferHistoryParams,
    ) -> Result<(Vec<TxReceipt>, u64), MmError<UtxoRpcError>> {
        const SEARCH_LOGS_STEP: u64 = 100;

        let token_address = contract_addr_into_rpc_format(&params.token_address);
        let address_topic = address_to_log_topic(&params.address);

        // «Skip the log if none of the topics are matched»
        // https://github.com/qtumproject/qtum-enterprise/blob/qtumx_beta_0.16.0/src/rpc/blockchain.cpp#L1590
        //
        // It means disjunction of topics (binary `OR`).
        // So we cannot specify `Transfer` event signature in the first topic,
        // but we can specify either `sender` or `receiver` in `Transfer` event.
        let topics = vec![
            TopicFilter::Skip,                         // event signature
            TopicFilter::Match(address_topic.clone()), // `sender` address in `Transfer` event
            TopicFilter::Match(address_topic.clone()), // `receiver` address in `Transfer` event
        ];

        let block_count = self.get_block_count().compat().await?;

        let mut result = Vec::new();
        let mut from_block = params.from_block;
        while from_block <= block_count {
            // The last window ends at the block count, so the listing covers exactly up to the tip.
            let to_block = (from_block + SEARCH_LOGS_STEP - 1).min(block_count);
            let mut receipts = self
                .search_logs(from_block, Some(to_block), vec![token_address], topics.clone())
                .compat()
                .await?;

            // remove receipts of transaction that didn't emit at least one `Transfer` event
            receipts.retain(|receipt| receipt.log.iter().any(is_transfer_event_log));

            result.extend(receipts.into_iter());
            from_block += SEARCH_LOGS_STEP;
        }
        Ok((result, block_count))
    }
}

#[async_trait]
impl BuildTransferHistory for NativeClient {
    async fn build(&self, params: TransferHistoryParams) -> Result<Vec<TxReceipt>, MmError<UtxoRpcError>> {
        Ok(self.search_logs_up_to_tip(params).await?.0)
    }

    async fn build_tx_idents(
        &self,
        params: TransferHistoryParams,
    ) -> Result<(Vec<(H256Json, u64)>, Option<u64>), MmError<UtxoRpcError>> {
        // The daemon is the single server: no up-to-date or disagreement test. The tip is the block count that bounded
        // the search, so the listing belongs to it and a block that arrives meanwhile cannot move it.
        let (receipts, block_count) = self.search_logs_up_to_tip(params).await?;
        Ok((
            receipts
                .into_iter()
                .map(|receipt| (receipt.transaction_hash, receipt.block_number))
                .unique()
                .collect(),
            Some(block_count),
        ))
    }
}

fn is_transferred_from_contract(script_pubkey: &Script) -> bool {
    let contract_call_bytes = match extract_contract_call_from_script(script_pubkey) {
        Ok(bytes) => bytes,
        Err(e) => {
            error!("{}", e);
            return false;
        },
    };
    let call_type = match MutContractCallType::from_script_pubkey(&contract_call_bytes) {
        Ok(Some(t)) => t,
        Ok(None) => return false,
        Err(e) => {
            error!("{}", e);
            return false;
        },
    };
    match call_type {
        MutContractCallType::Transfer => false,
        MutContractCallType::Erc20Payment => false,
        MutContractCallType::ReceiverSpend => true,
        MutContractCallType::SenderRefund => true,
    }
}

fn is_transferred_to_contract(script_pubkey: &Script) -> bool {
    let contract_call_bytes = match extract_contract_call_from_script(script_pubkey) {
        Ok(bytes) => bytes,
        Err(e) => {
            error!("{}", e);
            return false;
        },
    };
    let call_type = match MutContractCallType::from_script_pubkey(&contract_call_bytes) {
        Ok(Some(t)) => t,
        Ok(None) => return false,
        Err(e) => {
            error!("{}", e);
            return false;
        },
    };
    match call_type {
        MutContractCallType::Transfer => false,
        MutContractCallType::Erc20Payment => true,
        MutContractCallType::ReceiverSpend => false,
        MutContractCallType::SenderRefund => false,
    }
}

fn is_transfer_event_log(log: &LogEntry) -> bool {
    match log.topics.first() {
        Some(first_topic) => first_topic == QRC20_TRANSFER_TOPIC,
        None => false,
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use common::block_on;
    use mm2_metrics::{MetricType, MetricsJson, MetricsOps};
    use mm2_test_helpers::for_tests::find_metrics_in_json;
    use qrc20_tests::qrc20_coin_for_test;

    /// CRD ch.38 §38.5C: the tip of a native listing is the block count that bounded the log search, even when
    /// blocks arrive while the search runs.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_listing_tip_is_the_block_count_that_bounded_the_search() {
        use crate::utxo::rpc_clients::{NativeClient, NativeClientImpl, UtxoRpcClientEnum};
        use crate::utxo::utxo_tests::utxo_coin_fields_for_test;
        use mocktopus::mocking::*;
        use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
        use std::sync::Arc;

        let client = UtxoRpcClientEnum::Native(NativeClient(Arc::new(NativeClientImpl::default())));
        let utxo = utxo_coin_fields_for_test(client, None, false);
        let coin = Qrc20Coin(Arc::new(Qrc20CoinFields {
            utxo,
            platform: "QTUM".into(),
            contract_address: H160::from([7u8; 20]),
            swap_contract_address: H160::from([8u8; 20]),
            fallback_swap_contract: None,
            history_abort: HistoryAbort::new(),
        }));

        // The block count grows with every read; only the first one bounds the search.
        let reads = Arc::new(AtomicUsize::new(0));
        let counter = reads.clone();
        NativeClientImpl::get_block_count.mock_safe(move |_client| {
            let n = counter.fetch_add(1, AtomicOrdering::SeqCst) as u64;
            MockResult::Return(Box::new(futures01::future::ok(250 + n)))
        });
        let searches = Arc::new(AtomicUsize::new(0));
        let search_counter = searches.clone();
        let last_to_block = Arc::new(AtomicUsize::new(0));
        let to_block_seen = last_to_block.clone();
        NativeClient::search_logs.mock_safe(move |_client, _from, to, _addresses, _topics| {
            search_counter.fetch_add(1, AtomicOrdering::SeqCst);
            to_block_seen.store(to.expect("a bounded search") as usize, AtomicOrdering::SeqCst);
            MockResult::Return(Box::new(futures01::future::ok(vec![])))
        });

        let (idents, tip) = block_on(TransferHistoryBuilder::new(coin).build_tx_idents()).unwrap();
        assert!(idents.is_empty());
        // Blocks 0..=250 in steps of 100: three searches, and the tip is the count the search was bounded by.
        assert_eq!(searches.load(AtomicOrdering::SeqCst), 3);
        assert_eq!(tip, Some(250));
        // The last window ends at the tip, not at the end of its 100 blocks (299).
        assert_eq!(last_to_block.load(AtomicOrdering::SeqCst), 250);
        assert_eq!(reads.load(AtomicOrdering::SeqCst), 1, "no further block-count read");
    }

    // CRD ch.38 T38.5A.6 (order part): the transfer rows of one transaction stay adjacent, the
    // lower output/event index first, between rows of other transactions at the same height.
    #[test]
    fn qrc20_history_order_keeps_transfer_rows_adjacent() {
        let internal_id = |hash_byte: u8, output_index: u64, log_index: u64| -> Vec<u8> {
            BytesJson::from(TxInternalId::new(
                H256Json::from([hash_byte; 32]),
                output_index,
                log_index,
            ))
            .0
        };
        let mut rows = vec![
            utxo_common::history_row_for_test(500, internal_id(0x40, 1, 0)),
            utxo_common::history_row_for_test(500, internal_id(0x50, 0, 0)),
            utxo_common::history_row_for_test(500, internal_id(0x40, 0, 1)),
            utxo_common::history_row_for_test(0, internal_id(0x70, 0, 0)),
            utxo_common::history_row_for_test(500, internal_id(0x40, 0, 0)),
        ];
        let expected = vec![
            internal_id(0x70, 0, 0),
            internal_id(0x40, 0, 0),
            internal_id(0x40, 0, 1),
            internal_id(0x40, 1, 0),
            internal_id(0x50, 0, 0),
        ];
        for _ in 0..2 {
            rows.sort_unstable_by(utxo_common::legacy_history_order);
            let ids: Vec<Vec<u8>> = rows.iter().map(|r| r.internal_id.0.clone()).collect();
            assert_eq!(ids, expected);
            rows.reverse();
        }
    }

    #[test]
    fn history_retry_delay_backs_off_and_is_bounded() {
        let mut delay = HISTORY_RETRY_INITIAL_DELAY_SECS;
        let mut observed = vec![delay];
        for _ in 0..6 {
            delay = next_history_retry_delay(delay);
            observed.push(delay);
        }

        assert_eq!(observed, vec![10, 20, 40, 80, 160, 300, 300]);
    }

    #[test]
    fn test_tx_internal_id() {
        let tx_hash = hex::decode("39104d29d77ba83c5c6c63ab7a0f096301c443b4538dc6b30140453a40caa80a").unwrap();
        let expected_id = TxInternalId::new(tx_hash.as_slice().into(), 13, 257);
        let actual_bytes: BytesJson = expected_id.clone().into();

        let mut expected_bytes = tx_hash.clone();
        expected_bytes.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 13]);
        expected_bytes.extend_from_slice(&[0, 0, 0, 0, 0, 0, 1, 1]);
        assert_eq!(actual_bytes, expected_bytes.into());

        let actual_id = TxInternalId::from_bytes(&actual_bytes).unwrap();
        assert_eq!(actual_id, expected_id);
    }

    #[test]
    #[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
    fn test_process_cached_tx_transfer_map_update_is_not_needed() {
        // priv_key of qXxsj5RtciAby9T7m98AgAATL4zTi4UwDG
        let priv_key = [
            3, 98, 177, 3, 108, 39, 234, 144, 131, 178, 103, 103, 127, 80, 230, 166, 53, 68, 147, 215, 42, 216, 144,
            72, 172, 110, 180, 13, 123, 179, 10, 49,
        ];
        let (ctx, coin) = qrc20_coin_for_test(&priv_key, None);
        ctx.metrics.init().unwrap();

        let tx_hash: H256Json = hex::decode("85ede12ccc12fb1709c4d9e403e96c0c394b0916f2f6098d41d8dfa00013fcdb")
            .unwrap()
            .as_slice()
            .into();
        let tx_height = 699545;
        let transfer_map_expected = block_on(coin.transfer_details_by_hash(tx_hash.clone())).unwrap();

        let mut transfer_map = transfer_map_expected.clone();
        assert_eq!(
            block_on(coin.process_cached_tx_transfer_map(&ctx, &tx_hash, tx_height, &mut transfer_map)),
            ProcessCachedTransferMapResult::UpdateIsNotNeeded
        );
        assert_eq!(transfer_map, transfer_map_expected);

        let value: MetricsJson = json::from_value(ctx.metrics.collect_json().unwrap()).unwrap();
        let found = find_metrics_in_json(value, "tx.history.request.count", &[(
            "method",
            "transfer_details_by_hash",
        )]);
        assert_eq!(found, None);
    }

    #[test]
    #[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
    fn test_process_cached_tx_transfer_map_updated() {
        // priv_key of qXxsj5RtciAby9T7m98AgAATL4zTi4UwDG
        let priv_key = [
            3, 98, 177, 3, 108, 39, 234, 144, 131, 178, 103, 103, 127, 80, 230, 166, 53, 68, 147, 215, 42, 216, 144,
            72, 172, 110, 180, 13, 123, 179, 10, 49,
        ];
        let (ctx, coin) = qrc20_coin_for_test(&priv_key, None);
        ctx.metrics.init().unwrap();

        let tx_hash: H256Json = hex::decode("85ede12ccc12fb1709c4d9e403e96c0c394b0916f2f6098d41d8dfa00013fcdb")
            .unwrap()
            .as_slice()
            .into();
        let tx_height = 699545;
        let transfer_map_expected = block_on(coin.transfer_details_by_hash(tx_hash.clone())).unwrap();

        let mut transfer_map_zero_timestamp = transfer_map_expected
            .clone()
            .into_iter()
            .map(|(id, mut tx)| {
                tx.timestamp = 0;
                (id, tx)
            })
            .collect();
        assert_eq!(
            block_on(coin.process_cached_tx_transfer_map(&ctx, &tx_hash, tx_height, &mut transfer_map_zero_timestamp)),
            ProcessCachedTransferMapResult::Updated
        );
        assert_eq!(transfer_map_zero_timestamp, transfer_map_expected);

        let value: MetricsJson = json::from_value(ctx.metrics.collect_json().unwrap()).unwrap();
        let found = find_metrics_in_json(value, "tx.history.request.count", &[(
            "method",
            "get_verbose_transaction",
        )]);
        match found {
            Some(MetricType::Counter { key, value, .. }) if key == "tx.history.request.count" && value == 1 => (),
            found => panic!("Found metric type: {:?}", found),
        }
    }

    #[test]
    #[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
    fn test_process_cached_tx_transfer_map_reload_is_required() {
        // priv_key of qXxsj5RtciAby9T7m98AgAATL4zTi4UwDG
        let priv_key = [
            3, 98, 177, 3, 108, 39, 234, 144, 131, 178, 103, 103, 127, 80, 230, 166, 53, 68, 147, 215, 42, 216, 144,
            72, 172, 110, 180, 13, 123, 179, 10, 49,
        ];
        let (ctx, coin) = qrc20_coin_for_test(&priv_key, None);
        ctx.metrics.init().unwrap();

        let tx_hash: H256Json = hex::decode("85ede12ccc12fb1709c4d9e403e96c0c394b0916f2f6098d41d8dfa00013fcdb")
            .unwrap()
            .as_slice()
            .into();
        let tx_height = 699545;
        let transfer_map_expected = block_on(coin.transfer_details_by_hash(tx_hash.clone())).unwrap();

        let mut transfer_map_unexpected_tx_id = transfer_map_expected
            .clone()
            .into_iter()
            .map(|(mut id, tx)| {
                // just another tx_hash
                id.tx_hash = hex::decode("8a7270110ab7b56142b3bac89999276beb70320a7fe7666f460a05aa615eb0a0")
                    .unwrap()
                    .as_slice()
                    .into();
                (id, tx)
            })
            .collect();
        let actual_res = block_on(coin.process_cached_tx_transfer_map(
            &ctx,
            &tx_hash,
            tx_height,
            &mut transfer_map_unexpected_tx_id,
        ));
        assert_eq!(actual_res, ProcessCachedTransferMapResult::ReloadIsRequired);

        let value: MetricsJson = json::from_value(ctx.metrics.collect_json().unwrap()).unwrap();
        let found = find_metrics_in_json(value, "tx.history.request.count", &[("method", "tx_detail_by_hash")]);
        assert_eq!(found, None);
    }

    #[test]
    #[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
    fn test_process_tx_ids_updated() {
        // priv_key of qXxsj5RtciAby9T7m98AgAATL4zTi4UwDG
        let priv_key = [
            3, 98, 177, 3, 108, 39, 234, 144, 131, 178, 103, 103, 127, 80, 230, 166, 53, 68, 147, 215, 42, 216, 144,
            72, 172, 110, 180, 13, 123, 179, 10, 49,
        ];
        let (ctx, coin) = qrc20_coin_for_test(&priv_key, None);

        let tx_hash: H256Json = hex::decode("35e03bc529528a853ee75dde28f27eec8ed7b152b6af7ab6dfa5d55ea46f25ac")
            .unwrap()
            .as_slice()
            .into();
        let tx_height = 681443;
        let transfer_map_expected = block_on(coin.transfer_details_by_hash(tx_hash.clone())).unwrap();
        let mut history_map_expected = HistoryMapByHash::new();
        history_map_expected.insert(tx_hash.clone(), transfer_map_expected);

        let tx_ids = vec![(tx_hash, tx_height)];
        let mut history_map = HistoryMapByHash::new();
        let updated = block_on(coin.process_tx_ids(&ctx, &mut history_map, tx_ids));
        assert!(updated);
        assert_eq!(history_map, history_map_expected);
    }

    #[test]
    #[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
    fn test_process_tx_ids_not_updated() {
        // priv_key of qXxsj5RtciAby9T7m98AgAATL4zTi4UwDG
        let priv_key = [
            3, 98, 177, 3, 108, 39, 234, 144, 131, 178, 103, 103, 127, 80, 230, 166, 53, 68, 147, 215, 42, 216, 144,
            72, 172, 110, 180, 13, 123, 179, 10, 49,
        ];
        let (ctx, coin) = qrc20_coin_for_test(&priv_key, None);

        let tx_hash: H256Json = hex::decode("85ede12ccc12fb1709c4d9e403e96c0c394b0916f2f6098d41d8dfa00013fcdb")
            .unwrap()
            .as_slice()
            .into();
        let tx_height = 699545;
        let transfer_map_expected = block_on(coin.transfer_details_by_hash(tx_hash.clone())).unwrap();
        let mut history_map_expected = HistoryMapByHash::new();
        history_map_expected.insert(tx_hash.clone(), transfer_map_expected);

        let tx_ids = vec![(tx_hash, tx_height)];
        let mut history_map = history_map_expected.clone();
        let updated = block_on(coin.process_tx_ids(&ctx, &mut history_map, tx_ids));
        assert!(!updated);
        assert_eq!(history_map, history_map_expected);
    }

    #[test]
    #[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
    fn test_process_tx_ids_error_on_details() {
        // priv_key of qXxsj5RtciAby9T7m98AgAATL4zTi4UwDG
        let priv_key = [
            3, 98, 177, 3, 108, 39, 234, 144, 131, 178, 103, 103, 127, 80, 230, 166, 53, 68, 147, 215, 42, 216, 144,
            72, 172, 110, 180, 13, 123, 179, 10, 49,
        ];
        let (ctx, coin) = qrc20_coin_for_test(&priv_key, None);

        let metrics = MetricsArc::new();
        metrics.init().unwrap();

        let tx_hash_invalid: H256Json = hex::decode("0000000000000000000000000000000000000000000000000000000000000000")
            .unwrap()
            .as_slice()
            .into();
        let tx_hash: H256Json = hex::decode("85ede12ccc12fb1709c4d9e403e96c0c394b0916f2f6098d41d8dfa00013fcdb")
            .unwrap()
            .as_slice()
            .into();
        let tx_height = 699545;
        let transfer_map_expected = block_on(coin.transfer_details_by_hash(tx_hash.clone())).unwrap();
        let mut history_map_expected = HistoryMapByHash::new();
        // should contain only valid tx
        history_map_expected.insert(tx_hash.clone(), transfer_map_expected);

        let tx_ids = vec![(tx_hash, tx_height), (tx_hash_invalid, tx_height)];
        let mut history_map = HistoryMapByHash::default();
        let updated = block_on(coin.process_tx_ids(&ctx, &mut history_map, tx_ids));
        assert!(updated);
        assert_eq!(history_map, history_map_expected);
    }
}

// Offline tests of the collector and the token's Electrum methods over scripted servers (CRD ch.38 §38.6.4Q).
// A child of this module, so that they can call its private steps.
#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "qrc20_electrum_stub_tests.rs"]
mod electrum_stub_tests;
