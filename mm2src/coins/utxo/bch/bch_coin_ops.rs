use super::*;
use crate::utxo::header_layout_fallback::HeaderReadPolicy;

impl BchCoin {
    pub fn slp_prefix(&self) -> &CashAddrPrefix { &self.slp_addr_prefix }

    pub fn slp_address(&self, address: &Address) -> Result<CashAddress, String> {
        let conf = &self.as_ref().conf;
        address.to_cashaddress(
            &self.slp_prefix().to_string(),
            conf.pub_addr_prefix,
            conf.p2sh_addr_prefix,
        )
    }

    pub fn bchd_urls(&self) -> &[String] { &self.bchd_urls }

    pub(crate) async fn utxos_into_bch_unspents(&self, utxos: Vec<UnspentInfo>) -> UtxoRpcResult<BchUnspents> {
        let mut result = BchUnspents::default();
        let mut temporary_undetermined = Vec::new();

        let to_verbose: HashSet<H256Json> = utxos
            .into_iter()
            .filter_map(|unspent| {
                if unspent.outpoint.index == 0 {
                    // Zero output is reserved for OP_RETURN of specific protocols
                    // so if we get it we can safely consider this as standard BCH UTXO.
                    // There is no need to request verbose transaction for such UTXO.
                    result.add_standard(unspent);
                    None
                } else {
                    let hash = unspent.outpoint.hash.reversed().into();
                    temporary_undetermined.push(unspent);
                    Some(hash)
                }
            })
            .collect();

        let verbose_txs = self
            .get_verbose_transactions_from_cache_or_rpc(to_verbose)
            .compat()
            .await?;

        for unspent in temporary_undetermined {
            let prev_tx_hash = unspent.outpoint.hash.reversed().into();
            let prev_tx_bytes = verbose_txs
                .get(&prev_tx_hash)
                .or_mm_err(|| {
                    UtxoRpcError::Internal(format!(
                        "'get_verbose_transactions_from_cache_or_rpc' should have returned '{:?}'",
                        prev_tx_hash
                    ))
                })?
                .to_inner();
            let prev_tx: UtxoTx = match deserialize(prev_tx_bytes.hex.as_slice()) {
                Ok(b) => b,
                Err(e) => {
                    warn!(
                        "Failed to deserialize prev_tx {:?} with error {:?}, considering {:?} as undetermined",
                        prev_tx_bytes, e, unspent
                    );
                    result.add_undetermined(unspent);
                    continue;
                },
            };

            if prev_tx.outputs.is_empty() {
                warn!(
                    "Prev_tx {:?} outputs are empty, considering {:?} as undetermined",
                    prev_tx_bytes, unspent
                );
                result.add_undetermined(unspent);
                continue;
            }

            let zero_out_script: Script = prev_tx.outputs[0].script_pubkey.clone().into();
            if zero_out_script.is_pay_to_public_key()
                || zero_out_script.is_pay_to_public_key_hash()
                || zero_out_script.is_pay_to_script_hash()
            {
                result.add_standard(unspent);
            } else {
                match parse_slp_script(&prev_tx.outputs[0].script_pubkey) {
                    Ok(slp_data) => match slp_data.transaction {
                        SlpTransaction::Send { token_id, amounts } => {
                            match amounts.get(unspent.outpoint.index as usize - 1) {
                                Some(slp_amount) => result.add_slp(token_id, unspent, *slp_amount),
                                None => result.add_standard(unspent),
                            }
                        },
                        SlpTransaction::Genesis(genesis) => {
                            if unspent.outpoint.index == 1 {
                                let token_id = prev_tx.hash().reversed();
                                result.add_slp(token_id, unspent, genesis.initial_token_mint_quantity);
                            } else if Some(unspent.outpoint.index) == genesis.mint_baton_vout.map(|u| u as u32) {
                                result.add_slp_baton(unspent);
                            } else {
                                result.add_standard(unspent);
                            }
                        },
                        SlpTransaction::Mint {
                            token_id,
                            additional_token_quantity,
                            mint_baton_vout,
                        } => {
                            if unspent.outpoint.index == 1 {
                                result.add_slp(token_id, unspent, additional_token_quantity);
                            } else if Some(unspent.outpoint.index) == mint_baton_vout.map(|u| u as u32) {
                                result.add_slp_baton(unspent);
                            } else {
                                result.add_standard(unspent);
                            }
                        },
                    },
                    Err(e) => {
                        warn!(
                            "Error {} parsing script {:?} as SLP, considering {:?} as undetermined",
                            e, prev_tx.outputs[0].script_pubkey, unspent
                        );
                        result.undetermined.push(unspent);
                    },
                };
            }
        }
        Ok(result)
    }

    /// Returns unspents to calculate balance, use for displaying purposes only!
    /// DO NOT USE to build transactions, it can lead to double spending attempt and also have other unpleasant consequences
    pub async fn bch_unspents_for_display(&self, address: &Address) -> UtxoRpcResult<BchUnspents> {
        // ordering is not required to display balance to we can simply call "normal" list_unspent
        let all_unspents = self
            .utxo_arc
            .rpc_client
            .list_unspent(address, self.utxo_arc.decimals)
            .compat()
            .await?;
        self.utxos_into_bch_unspents(all_unspents).await
    }

    /// Locks recently spent cache to safely return UTXOs for spending
    pub async fn bch_unspents_for_spend(
        &self,
        address: &Address,
    ) -> UtxoRpcResult<(BchUnspents, RecentlySpentOutPointsGuard<'_>)> {
        let (all_unspents, recently_spent) = utxo_common::get_unspent_ordered_list(self, address).await?;
        let result = self.utxos_into_bch_unspents(all_unspents).await?;

        Ok((result, recently_spent))
    }

    pub async fn get_token_utxos_for_spend(
        &self,
        token_id: &H256,
    ) -> UtxoRpcResult<(Vec<SlpUnspent>, Vec<UnspentInfo>, RecentlySpentOutPointsGuard<'_>)> {
        let my_address = self
            .as_ref()
            .derivation_method
            .iguana_or_err()
            .mm_err(|e| UtxoRpcError::Internal(e.to_string()))?;
        let (mut bch_unspents, recently_spent) = self.bch_unspents_for_spend(my_address).await?;
        let (mut slp_unspents, standard_utxos) = (
            bch_unspents.slp.remove(token_id).unwrap_or_default(),
            bch_unspents.standard,
        );

        slp_unspents.sort_by(|a, b| a.slp_amount.cmp(&b.slp_amount));
        Ok((slp_unspents, standard_utxos, recently_spent))
    }

    pub async fn get_token_utxos_for_display(
        &self,
        token_id: &H256,
    ) -> UtxoRpcResult<(Vec<SlpUnspent>, Vec<UnspentInfo>)> {
        let my_address = self
            .as_ref()
            .derivation_method
            .iguana_or_err()
            .mm_err(|e| UtxoRpcError::Internal(e.to_string()))?;
        let mut bch_unspents = self.bch_unspents_for_display(my_address).await?;
        let (mut slp_unspents, standard_utxos) = (
            bch_unspents.slp.remove(token_id).unwrap_or_default(),
            bch_unspents.standard,
        );

        slp_unspents.sort_by(|a, b| a.slp_amount.cmp(&b.slp_amount));
        Ok((slp_unspents, standard_utxos))
    }

    pub fn add_slp_token_info(&self, ticker: String, info: SlpTokenInfo) {
        self.slp_tokens_infos.lock().unwrap().insert(ticker, info);
    }

    pub fn get_slp_tokens_infos(&self) -> MutexGuard<'_, HashMap<String, SlpTokenInfo>> {
        self.slp_tokens_infos.lock().unwrap()
    }

    pub fn get_my_slp_address(&self) -> Result<CashAddress, String> {
        let my_address = try_s!(self.as_ref().derivation_method.iguana_or_err());
        let slp_address = my_address.to_cashaddress(
            &self.slp_prefix().to_string(),
            self.as_ref().conf.pub_addr_prefix,
            self.as_ref().conf.p2sh_addr_prefix,
        )?;
        Ok(slp_address)
    }

    pub(crate) async fn tx_from_storage_or_rpc<T: TxHistoryStorage>(
        &self,
        tx_hash: &H256Json,
        storage: &T,
    ) -> Result<UtxoTx, MmError<GetTxDetailsError<T::Error>>> {
        let tx_hash_as_bytes = BytesJson::new(tx_hash.0.to_vec());
        let tx_bytes = match storage
            .tx_bytes_from_cache(self.ticker(), &tx_hash_as_bytes)
            .await
            .mm_err(Into::into)?
        {
            Some(tx_bytes) => tx_bytes,
            None => {
                let tx_bytes = self
                    .as_ref()
                    .rpc_client
                    .get_transaction_bytes(tx_hash)
                    .compat()
                    .await
                    .mm_err(Into::into)?;
                storage
                    .add_tx_to_cache(self.ticker(), &tx_hash_as_bytes, &tx_bytes)
                    .await
                    .mm_err(Into::into)?;
                tx_bytes
            },
        };
        let tx = deserialize(tx_bytes.0.as_slice())?;
        Ok(tx)
    }

    /// Returns multiple details by tx hash if token transfers also occurred in the transaction
    pub async fn transaction_details_with_token_transfers<T: TxHistoryStorage>(
        &self,
        tx_hash: &H256Json,
        block_height_and_time: Option<BlockHeightAndTime>,
        storage: &T,
    ) -> Result<Vec<TransactionDetails>, MmError<GetTxDetailsError<T::Error>>> {
        let tx = self.tx_from_storage_or_rpc(tx_hash, storage).await?;

        let bch_tx_details = self
            .bch_tx_details(tx_hash, &tx, block_height_and_time, storage)
            .await?;
        let maybe_op_return: Script = tx.outputs[0].script_pubkey.clone().into();
        if !(maybe_op_return.is_pay_to_public_key_hash()
            || maybe_op_return.is_pay_to_public_key()
            || maybe_op_return.is_pay_to_script_hash())
        {
            if let Ok(slp_details) = parse_slp_script(&maybe_op_return) {
                let slp_tx_details = self
                    .slp_tx_details(
                        &tx,
                        slp_details.transaction,
                        block_height_and_time,
                        bch_tx_details.fee_details.clone(),
                        storage,
                    )
                    .await?;
                return Ok(vec![bch_tx_details, slp_tx_details]);
            }
        }

        Ok(vec![bch_tx_details])
    }

    pub(crate) async fn bch_tx_details<T: TxHistoryStorage>(
        &self,
        tx_hash: &H256Json,
        tx: &UtxoTx,
        height_and_time: Option<BlockHeightAndTime>,
        storage: &T,
    ) -> Result<TransactionDetails, MmError<GetTxDetailsError<T::Error>>> {
        let my_address = self.as_ref().derivation_method.iguana_or_err().mm_err(Into::into)?;
        let my_addresses = [my_address.clone()];
        let mut tx_builder = TxDetailsBuilder::new(self.ticker().to_owned(), tx, height_and_time, my_addresses);
        for output in &tx.outputs {
            let addresses = match self.addresses_from_script(&output.script_pubkey.clone().into()) {
                Ok(a) => a,
                Err(_) => continue,
            };

            if addresses.is_empty() {
                continue;
            }

            if addresses.len() != 1 {
                let msg = format!(
                    "{} tx {:02x} output script resulted into unexpected number of addresses",
                    self.ticker(),
                    tx_hash,
                );
                return MmError::err(GetTxDetailsError::AddressesFromScriptError(msg));
            }

            let amount = big_decimal_from_sat_unsigned(output.value, self.decimals());
            for address in addresses {
                tx_builder.transferred_to(address, &amount);
            }
        }

        let mut total_input = 0;
        for input in &tx.inputs {
            let index = input.previous_output.index;
            let prev_tx = self
                .tx_from_storage_or_rpc(&input.previous_output.hash.reversed().into(), storage)
                .await?;
            let prev_script = prev_tx.outputs[index as usize].script_pubkey.clone().into();
            let addresses = self
                .addresses_from_script(&prev_script)
                .map_to_mm(GetTxDetailsError::AddressesFromScriptError)?;
            if addresses.len() != 1 {
                let msg = format!(
                    "{} tx {:02x} output script resulted into unexpected number of addresses",
                    self.ticker(),
                    tx_hash,
                );
                return MmError::err(GetTxDetailsError::AddressesFromScriptError(msg));
            }

            let prev_value = prev_tx.outputs[index as usize].value;
            total_input += prev_value;
            let amount = big_decimal_from_sat_unsigned(prev_value, self.decimals());
            for address in addresses {
                tx_builder.transferred_from(address, &amount);
            }
        }

        let total_output = tx.outputs.iter().fold(0, |total, output| total + output.value);
        let fee = Some(TxFeeDetails::Utxo(UtxoFeeDetails {
            coin: Some(self.ticker().into()),
            amount: big_decimal_from_sat_unsigned(total_input - total_output, self.decimals()),
        }));
        tx_builder.set_tx_fee(fee);
        Ok(tx_builder.build())
    }

    pub(crate) async fn get_slp_genesis_params<T: TxHistoryStorage>(
        &self,
        token_id: H256,
        storage: &T,
    ) -> Result<SlpGenesisParams, MmError<GetTxDetailsError<T::Error>>> {
        let token_genesis_tx = self.tx_from_storage_or_rpc(&token_id.into(), storage).await?;
        let maybe_genesis_script: Script = token_genesis_tx.outputs[0].script_pubkey.clone().into();
        let slp_details = parse_slp_script(&maybe_genesis_script).mm_err(Into::into)?;
        match slp_details.transaction {
            SlpTransaction::Genesis(params) => Ok(params),
            _ => MmError::err(GetTxDetailsError::SlpTokenIdIsNotGenesisTx(token_id)),
        }
    }

    pub(crate) async fn slp_transferred_amounts<T: TxHistoryStorage>(
        &self,
        utxo_tx: &UtxoTx,
        slp_tx: SlpTransaction,
        storage: &T,
    ) -> Result<HashMap<usize, (CashAddress, BigDecimal)>, MmError<GetTxDetailsError<T::Error>>> {
        let slp_amounts = match slp_tx {
            SlpTransaction::Send { token_id, amounts } => {
                let genesis_params = self.get_slp_genesis_params(token_id, storage).await?;
                EitherIter::Left(
                    amounts
                        .into_iter()
                        .map(move |amount| big_decimal_from_sat_unsigned(amount, genesis_params.decimals[0])),
                )
            },
            SlpTransaction::Mint {
                token_id,
                additional_token_quantity,
                ..
            } => {
                let slp_genesis_params = self.get_slp_genesis_params(token_id, storage).await?;
                EitherIter::Right(std::iter::once(big_decimal_from_sat_unsigned(
                    additional_token_quantity,
                    slp_genesis_params.decimals[0],
                )))
            },
            SlpTransaction::Genesis(genesis_params) => EitherIter::Right(std::iter::once(
                big_decimal_from_sat_unsigned(genesis_params.initial_token_mint_quantity, genesis_params.decimals[0]),
            )),
        };

        let mut result = HashMap::new();
        for (i, amount) in slp_amounts.into_iter().enumerate() {
            let output_index = i + 1;
            match utxo_tx.outputs.get(output_index) {
                Some(output) => {
                    let addresses = self
                        .addresses_from_script(&output.script_pubkey.clone().into())
                        .map_to_mm(GetTxDetailsError::AddressesFromScriptError)?;
                    if addresses.len() != 1 {
                        let msg = format!(
                            "{} tx {:?} output script resulted into unexpected number of addresses",
                            self.ticker(),
                            utxo_tx.hash().reversed(),
                        );
                        return MmError::err(GetTxDetailsError::AddressesFromScriptError(msg));
                    }

                    let slp_address = self
                        .slp_address(&addresses[0])
                        .map_to_mm(GetTxDetailsError::ToSlpAddressError)?;
                    result.insert(output_index, (slp_address, amount));
                },
                None => return MmError::err(GetTxDetailsError::InvalidSlpTransaction(utxo_tx.hash().reversed())),
            }
        }
        Ok(result)
    }

    pub(crate) async fn slp_tx_details<Storage: TxHistoryStorage>(
        &self,
        tx: &UtxoTx,
        slp_tx: SlpTransaction,
        height_and_time: Option<BlockHeightAndTime>,
        tx_fee: Option<TxFeeDetails>,
        storage: &Storage,
    ) -> Result<TransactionDetails, MmError<GetTxDetailsError<Storage::Error>>> {
        let token_id = match slp_tx.token_id() {
            Some(id) => id,
            None => tx.hash().reversed(),
        };

        let my_address = self.as_ref().derivation_method.iguana_or_err().mm_err(Into::into)?;
        let slp_address = self
            .slp_address(my_address)
            .map_to_mm(GetTxDetailsError::ToSlpAddressError)?;
        let addresses = [slp_address];

        let mut slp_tx_details_builder =
            TxDetailsBuilder::new(self.ticker().to_owned(), tx, height_and_time, addresses);
        let slp_transferred_amounts = self.slp_transferred_amounts(tx, slp_tx, storage).await?;
        for (_, (address, amount)) in slp_transferred_amounts {
            slp_tx_details_builder.transferred_to(address, &amount);
        }

        for input in &tx.inputs {
            let prev_tx = self
                .tx_from_storage_or_rpc(&input.previous_output.hash.reversed().into(), storage)
                .await?;
            if let Ok(slp_tx_details) = parse_slp_script(&prev_tx.outputs[0].script_pubkey) {
                let mut prev_slp_transferred = self
                    .slp_transferred_amounts(&prev_tx, slp_tx_details.transaction, storage)
                    .await?;
                let i = input.previous_output.index as usize;
                if let Some((address, amount)) = prev_slp_transferred.remove(&i) {
                    slp_tx_details_builder.transferred_from(address, &amount);
                }
            }
        }

        slp_tx_details_builder.set_transaction_type(TransactionType::TokenTransfer(token_id.take().to_vec().into()));
        slp_tx_details_builder.set_tx_fee(tx_fee);

        Ok(slp_tx_details_builder.build())
    }

    pub async fn get_block_timestamp(&self, height: u64) -> Result<u64, MmError<UtxoRpcError>> {
        let policy = HeaderReadPolicy::for_conf(&self.as_ref().conf);
        self.as_ref()
            .rpc_client
            .get_block_timestamp_with_policy(height, &policy)
            .await
    }
}
