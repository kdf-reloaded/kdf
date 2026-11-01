// utxo_common_history — transaction history processing, KMD rewards

use super::*;

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
            Ok(actual_balance) => Some(actual_balance),
            Err(err) => {
                log_tag!(
                    ctx,
                    "",
                    "tx_history",
                    "coin" => coin.as_ref().conf.ticker;
                    fmt = "Error {:?} on getting balance", err
                );
                None
            },
        };

        let need_update = history_map.iter().any(|(_, tx)| tx.should_update());
        match (&my_balance, &actual_balance) {
            (Some(prev_balance), Some(actual_balance)) if prev_balance == actual_balance && !need_update => {
                // Balance unchanged, so there is nothing to reload yet. Wait for
                // the idle interval or for a server to report that the watched
                // address moved, whichever comes first; the balance is re-read
                // at the top of the loop either way, so the notification only
                // decides *when* to look, never *what* is true.
                let idle = Timer::sleep(30.);
                futures::pin_mut!(idle);
                let woken = futures::StreamExt::next(&mut wake_rx);
                futures::pin_mut!(woken);
                if let futures::future::Either::Right(_) = futures::future::select(idle, woken).await {
                    // Collapse a burst into one pass.
                    while wake_rx.try_recv().is_ok() {}
                }
                continue;
            },
            _ => (),
        }

        let metrics = ctx.metrics.clone();
        let tx_ids = match coin.request_tx_history(metrics).await {
            RequestTxHistoryResult::Ok(tx_ids) => tx_ids,
            RequestTxHistoryResult::Retry { error } => {
                log_tag!(
                    ctx,
                    "",
                    "tx_history",
                    "coin" => coin.as_ref().conf.ticker;
                    fmt = "{}, retrying", error
                );
                Timer::sleep(10.).await;
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
                                    HistorySyncState::InProgress(json!({ "transactions_left": transactions_left }));
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
                            )
                        },
                    }
                },
                Entry::Occupied(mut e) => {
                    // update block height for previously unconfirmed transaction
                    if e.get().should_update_block_height() && height > 0 {
                        e.get_mut().block_height = height;
                        updated = true;
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
        *coin.as_ref().history_sync_state.lock().unwrap() = HistorySyncState::Finished;

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

    let tx_ids = match &coin.as_ref().rpc_client {
        #[cfg(not(target_arch = "wasm32"))]
        UtxoRpcClientEnum::LightWalletD(_) => {
            return RequestTxHistoryResult::CriticalError(
                "transparent transaction history is not supported over lightwalletd".to_owned(),
            )
        },
        UtxoRpcClientEnum::Native(client) => {
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

            all_transactions
                .into_iter()
                .filter_map(|item| {
                    if item.address == my_address {
                        Some((item.txid, item.block_height()))
                    } else {
                        None
                    }
                })
                .collect()
        },
        UtxoRpcClientEnum::Electrum(client) => {
            let my_address = &my_address_obj;
            let script = output_script(my_address, ScriptType::P2PKH);
            let script_hash = electrum_script_hash(&script);

            mm_counter!(metrics, "tx.history.request.count", 1,
                "coin" => coin.as_ref().conf.ticker.clone(), "client" => "electrum", "method" => "blockchain.scripthash.get_history");

            let electrum_history = match client.scripthash_get_history(&hex::encode(script_hash)).compat().await {
                Ok(value) => value,
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
            electrum_history
                .into_iter()
                .rev()
                .map(|item| {
                    let height = if item.height < 0 { 0 } else { item.height as u64 };
                    (item.tx_hash, height)
                })
                .collect()
        },
    };
    RequestTxHistoryResult::Ok(tx_ids)
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

pub async fn tx_details_by_hash<T: UtxoCommonOps>(
    coin: &T,
    hash: &[u8],
    input_transactions: &mut HistoryUtxoTxMap,
) -> Result<TransactionDetails, String> {
    let ticker = &coin.as_ref().conf.ticker;
    let hash = H256Json::from(hash);
    let verbose_tx = try_s!(coin.as_ref().rpc_client.get_verbose_transaction(&hash).compat().await);
    let mut tx: UtxoTx = try_s!(deserialize(verbose_tx.hex.as_slice()).map_err(|e| ERRL!("{:?}", e)));
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
            let pk = try_s!(my_public_key(coin_fields));
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
        let prev_tx = try_s!(
            coin.get_mut_verbose_transaction_from_map_or_rpc(prev_tx_hash, input_transactions)
                .await
        );
        let prev_tx = &mut prev_tx.tx;
        prev_tx.tx_hash_algo = coin.as_ref().tx_hash_algo;

        let prev_output = try_s!(prev_output_at(prev_tx, &prev_tx_hash, input.previous_output.index)
            .map_err(|e| ERRL!("Transaction {:02x} spends a missing output: {}", hash, e)));
        let prev_tx_value = prev_output.value;
        input_amount += prev_tx_value;
        let from: Vec<Address> = try_s!(coin.addresses_from_script(&prev_output.script_pubkey.clone().into()));
        if from.contains(my_address) {
            spent_by_me += prev_tx_value;
        }
        from_addresses.extend(from.into_iter());
    }

    for output in tx.outputs.iter() {
        output_amount += output.value;
        let to = try_s!(coin.addresses_from_script(&output.script_pubkey.clone().into()));
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
        (try_s!(fee.try_into()), None)
    } else {
        let fee = input_amount as i64 - output_amount as i64;
        (big_decimal_from_sat(fee, coin.as_ref().decimals), None)
    };

    // remove address duplicates in case several inputs were spent from same address
    // or several outputs are sent to same address
    let mut from_addresses: Vec<String> =
        try_s!(from_addresses.into_iter().map(|addr| addr.display_address()).collect());
    from_addresses.sort();
    from_addresses.dedup();
    let mut to_addresses: Vec<String> = try_s!(to_addresses.into_iter().map(|addr| addr.display_address()).collect());
    to_addresses.sort();
    to_addresses.dedup();

    let fee_details = UtxoFeeDetails {
        coin: Some(coin.as_ref().conf.ticker.clone()),
        amount: fee,
    };

    Ok(TransactionDetails {
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
    })
}

pub async fn get_mut_verbose_transaction_from_map_or_rpc<'a, 'b, T>(
    coin: &'a T,
    tx_hash: H256Json,
    utxo_tx_map: &'b mut HistoryUtxoTxMap,
) -> UtxoRpcResult<&'b mut HistoryUtxoTx>
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
                .await?;
            let tx = HistoryUtxoTx {
                tx: deserialize(verbose.hex.as_slice())
                    .map_to_mm(|e| UtxoRpcError::InvalidResponse(format!("{:?}, tx: {:?}", e, tx_hash)))?,
                height: verbose.height,
            };
            e.insert(tx)
        },
        Entry::Occupied(e) => e.into_mut(),
    };
    Ok(tx)
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
}
