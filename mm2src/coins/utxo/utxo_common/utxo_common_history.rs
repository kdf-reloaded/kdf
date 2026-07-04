// utxo_common_history — transaction history processing, KMD rewards

use super::*;

lazy_static! {
    pub static ref HISTORY_TOO_LARGE_ERROR: Json = json!({
        "code": 1,
        "message": "history too large"
    });
}

pub const HISTORY_TOO_LARGE_ERR_CODE: i64 = -1;

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
    let mut history_map: HashMap<H256Json, TransactionDetails> = history
        .into_iter()
        .map(|tx| (H256Json::from(tx.tx_hash.as_bytes()), tx))
        .collect();

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
                // my balance hasn't been changed, there is no need to reload tx_history
                Timer::sleep(30.).await;
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
                let mut to_write: Vec<TransactionDetails> =
                    history_map.iter().map(|(_, value)| value.clone()).collect();
                // the transactions with block_height == 0 are the most recent so we need to separately handle them while sorting
                to_write.sort_unstable_by(|a, b| {
                    if a.block_height == 0 {
                        Ordering::Less
                    } else if b.block_height == 0 {
                        Ordering::Greater
                    } else {
                        b.block_height.cmp(&a.block_height)
                    }
                });
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
                        Some((item.txid, item.blockindex))
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
    let my_address = try_s!(coin.as_ref().derivation_method.iguana_or_err());

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

        let prev_tx_value = prev_tx.outputs[input.previous_output.index as usize].value;
        input_amount += prev_tx_value;
        let from: Vec<Address> = try_s!(coin.addresses_from_script(
            &prev_tx.outputs[input.previous_output.index as usize]
                .script_pubkey
                .clone()
                .into()
        ));
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

        let prev_tx_value = prev_tx.tx.outputs[input.previous_output.index as usize].value;
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
