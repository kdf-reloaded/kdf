use super::*;

impl ZCoin {
    #[inline(always)]
    #[cfg(not(target_arch = "wasm32"))]
    pub fn z_rpc(&self) -> &(dyn ZRpcOps + Send + Sync) { self.utxo_arc.rpc_client.as_ref() }

    #[inline(always)]
    pub fn rpc_client(&self) -> &UtxoRpcClientEnum { &self.utxo_arc.rpc_client }

    #[inline(always)]
    pub fn is_sapling_state_synced(&self) -> bool { self.z_fields.sapling_state_synced.load(AtomicOrdering::Relaxed) }

    /// The Sapling network-upgrade activation height for this coin, sourced from
    /// the coin config's `protocol_data.consensus_params` (R39.6.4). This is the
    /// hard floor below which no shielded output can exist and thus the lower
    /// bound for any shielded sync start point (R39.8.0g).
    #[inline(always)]
    pub fn sapling_activation_height(&self) -> u64 {
        u64::from(self.z_fields.consensus_params.sapling_activation_height)
    }

    #[inline(always)]
    pub fn my_z_address_encoded(&self) -> String { self.z_fields.my_z_addr_encoded.clone() }

    #[inline(always)]
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn shielded_history(&self) -> &ZCoinShieldedHistory { &self.z_fields.shielded_history }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn shielded_wallet_db_scan_complete(&self) -> bool {
        self.z_fields.wallet_db_scan_complete.load(AtomicOrdering::Relaxed)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn fetch_lightwalletd_compact_blocks_to_height(
        &self,
        servers: &[String],
        target_height: u64,
        requested_start_height: Option<u64>,
        skip_sync_params: bool,
        progress: &(dyn Fn(u64, u64) + Send + Sync),
    ) -> Result<u64, String> {
        self.z_fields
            .shielded_history
            .fetch_compact_blocks_from_lightwalletd(
                &self.z_fields.consensus_params,
                servers,
                target_height,
                requested_start_height,
                skip_sync_params,
                progress,
            )
            .await
    }

    /// The height the shielded scan is anchored at (R39.8.0h), used to report
    /// `first_sync_block` unconditionally even when the caller supplied no
    /// explicit sync start. `None` when the wallet DB has no stored blocks.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn shielded_wallet_sync_start_height(&self) -> Option<u64> {
        self.z_fields.shielded_history.wallet_sync_start_height().ok().flatten()
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn scan_shielded_wallet_db_to_height<F: FnMut(u64, u64)>(
        &self,
        target_height: u64,
        progress: F,
    ) -> Result<u64, String> {
        let history = self.z_fields.shielded_history.clone();
        let consensus_params = self.z_fields.consensus_params.clone();
        let blocks_per_iteration = self.z_fields.blocks_per_iteration.max(1);
        let inter_iteration_interval_ms = self.z_fields.inter_iteration_interval_ms;
        let scan_result = tokio::task::block_in_place(move || {
            history.scan_cached_blocks_to_height(
                consensus_params,
                target_height,
                blocks_per_iteration,
                inter_iteration_interval_ms,
                progress,
            )
        });

        match scan_result {
            Ok(scanned_height) => {
                self.z_fields
                    .wallet_db_scanned_through
                    .store(scanned_height, AtomicOrdering::Relaxed);
                self.z_fields
                    .wallet_db_scan_complete
                    .store(true, AtomicOrdering::Relaxed);
                Ok(scanned_height)
            },
            Err(err) => {
                if let Ok(Some(scanned_height)) = self.z_fields.shielded_history.scanned_height() {
                    self.z_fields
                        .wallet_db_scanned_through
                        .store(scanned_height, AtomicOrdering::Relaxed);
                }
                self.z_fields
                    .wallet_db_scan_complete
                    .store(false, AtomicOrdering::Relaxed);
                log::error!(
                    "ZCoin shielded wallet DB scan failed for {} at target height {}: {}",
                    self.ticker(),
                    target_height,
                    err
                );
                Err(err)
            },
        }
    }

    /// Returns all unspents included currently unspendable (not confirmed)
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) async fn my_z_unspents_ordered(&self) -> UtxoRpcResult<Vec<ZUnspent>> {
        let min_conf = 0;
        let max_conf = i32::MAX as u32;
        let watch_only = true;

        let mut unspents = self
            .z_rpc()
            .z_list_unspent(min_conf, max_conf, watch_only, &[&self.z_fields.my_z_addr_encoded])
            .compat()
            .await?;

        unspents.sort_unstable_by(|a, b| a.amount.cmp(&b.amount));
        Ok(unspents)
    }

    /// shielded outputs are not spendable until confirmed
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) async fn my_spendable_z_unspents_ordered(&self) -> UtxoRpcResult<Vec<ZUnspent>> {
        let min_conf = 1;
        let max_conf = i32::MAX as u32;
        let watch_only = true;

        let mut unspents = self
            .z_rpc()
            .z_list_unspent(min_conf, max_conf, watch_only, &[&self.z_fields.my_z_addr_encoded])
            .compat()
            .await?;

        unspents.sort_unstable_by(|a, b| a.amount.cmp(&b.amount));
        Ok(unspents)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) async fn get_one_kbyte_tx_fee(&self) -> UtxoRpcResult<BigDecimal> {
        let fee = self.get_tx_fee().await?;
        match fee {
            ActualTxFee::Dynamic(fee) | ActualTxFee::FixedPerKb(fee) => {
                Ok(big_decimal_from_sat_unsigned(fee, self.decimals()))
            },
        }
    }

    /// Generates a tx sending outputs from our address
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) async fn gen_tx(
        &self,
        t_outputs: Vec<TxOut>,
        z_outputs: Vec<ZOutput>,
    ) -> Result<(ZTransaction, AdditionalTxData), MmError<GenTxError>> {
        let _lock = self.z_fields.z_unspent_mutex.lock().await;
        while !self.is_sapling_state_synced() {
            Timer::sleep(0.5).await
        }
        let tx_fee = self.get_one_kbyte_tx_fee().await.mm_err(Into::into)?;
        if matches!(self.rpc_client(), UtxoRpcClientEnum::Electrum(_)) {
            return self.gen_tx_from_shielded_wallet_db(t_outputs, z_outputs, tx_fee);
        }

        let t_output_sat: u64 = t_outputs.iter().fold(0, |cur, out| cur + u64::from(out.value()));
        let z_output_sat: u64 = z_outputs.iter().fold(0, |cur, out| cur + u64::from(out.amount));
        let total_output_sat = t_output_sat + z_output_sat;
        let total_output = big_decimal_from_sat_unsigned(total_output_sat, self.utxo_arc.decimals);
        let total_required = &total_output + &tx_fee;

        let z_unspents = self.my_spendable_z_unspents_ordered().await.mm_err(Into::into)?;
        let mut selected_unspents = Vec::new();
        let mut total_input_amount = BigDecimal::from(0u8);
        let mut change = BigDecimal::from(0u8);

        let mut received_by_me = 0u64;

        for unspent in z_unspents {
            total_input_amount += unspent.amount.to_decimal();
            selected_unspents.push(unspent);

            if total_input_amount >= total_required {
                change = &total_input_amount - &total_required;
                break;
            }
        }

        if total_input_amount < total_required {
            return MmError::err(GenTxError::InsufficientBalance {
                coin: self.ticker().into(),
                available: total_input_amount,
                required: total_required,
            });
        }

        let current_block = self
            .utxo_arc
            .rpc_client
            .get_block_count()
            .compat()
            .await
            .mm_err(Into::into)? as u32;
        let mut ext = HashMap::new();
        #[allow(deprecated)]
        let extfvk = self.z_fields.z_spending_key.to_extended_full_viewing_key();
        let ufvk = UnifiedFullViewingKey::from_sapling_extended_full_viewing_key(extfvk)
            .map_to_mm(|e| GenTxError::ShieldedWalletDb(format!("failed to construct unified viewing key: {}", e)))?;
        ext.insert(0u32, ufvk);
        let mut selected_notes_with_witness: Vec<(_, IncrementalWitness)> = Vec::with_capacity(selected_unspents.len());

        for unspent in selected_unspents {
            let prev_tx = self
                .rpc_client()
                .get_verbose_transaction(&unspent.txid)
                .compat()
                .await
                .mm_err(Into::into)?;

            let height = prev_tx.height.or_mm_err(|| GenTxError::PrevTxNotConfirmed)?;

            let mined_height = BlockHeight::from_u32(height as u32);
            let z_cash_tx = ZTransaction::read(
                prev_tx.hex.as_slice(),
                BranchId::for_height(&self.z_fields.consensus_params, mined_height),
            )
            .map_to_mm(|err| GenTxError::TxReadError { err, hex: prev_tx.hex })?;
            let decrypted = decrypt_transaction(
                &self.z_fields.consensus_params,
                Some(mined_height),
                Some(BlockHeight::from_u32(current_block)),
                &z_cash_tx,
                &ext,
            );

            let decrypted_output = decrypted
                .sapling_outputs()
                .iter()
                .find(|out| out.index() as u32 == unspent.out_index)
                .or_mm_err(|| GenTxError::DecryptedOutputNotFound)?;
            let witness = self
                .get_unspent_witness(decrypted_output.note(), height as u32)
                .await
                .mm_err(Into::into)?;
            selected_notes_with_witness.push((decrypted_output.note().clone(), witness));
        }

        let sapling_anchor = selected_notes_with_witness
            .first()
            .map(|(_, witness)| witness.root().into())
            .unwrap_or_else(sapling::Anchor::empty_tree);
        let mut tx_builder = ZTxBuilder::new(
            self.z_fields.consensus_params.clone(),
            current_block.into(),
            BuildConfig::Standard {
                sapling_anchor: Some(sapling_anchor),
                orchard_anchor: None,
            },
        );
        let fvk = FullViewingKey::from_expanded_spending_key(&self.z_fields.z_spending_key.expsk);
        for (note, witness) in selected_notes_with_witness {
            tx_builder.add_sapling_spend(
                fvk.clone(),
                note,
                witness.path().or_mm_err(|| GenTxError::FailedToGetMerklePath)?,
            )?;
        }

        for z_out in z_outputs {
            if z_out.to_addr == self.z_fields.my_z_addr {
                received_by_me += u64::from(z_out.amount);
            }

            tx_builder.add_sapling_output(
                z_out.viewing_key,
                z_out.to_addr,
                z_out.amount,
                z_out.memo.unwrap_or_else(MemoBytes::empty),
            )?;
        }

        if change > BigDecimal::from(0u8) {
            let change_sat = sat_from_big_decimal(&change, self.utxo_arc.decimals).mm_err(Into::into)?;
            received_by_me += change_sat;

            tx_builder.add_sapling_output(
                None,
                self.z_fields.my_z_addr.clone(),
                Amount::from_u64(change_sat).map_to_mm(|_| {
                    GenTxError::NumConversion(NumConversError(format!(
                        "Failed to get ZCash amount from {}",
                        change_sat
                    )))
                })?,
                MemoBytes::empty(),
            )?;
        }

        for output in t_outputs {
            tx_builder.add_transparent_output_raw(output);
        }

        let fee_amount = Amount::from_u64(sat_from_big_decimal(&tx_fee, self.decimals()).mm_err(Into::into)?)
            .map_to_mm(|_| GenTxError::NumConversion(NumConversError("Invalid ZCash fee amount".to_owned())))?;
        let fee_rule = FixedFeeRule::non_standard(fee_amount);
        let tx = tokio::task::block_in_place(|| {
            tx_builder.build(
                &TransparentSigningSet::new(),
                std::slice::from_ref(&self.z_fields.z_spending_key),
                &[],
                rand::rngs::OsRng,
                &self.z_fields.z_tx_prover,
                &self.z_fields.z_tx_prover,
                &fee_rule,
            )
        })?
        .into_transaction();

        let additional_data = AdditionalTxData {
            received_by_me,
            spent_by_me: sat_from_big_decimal(&total_input_amount, self.decimals()).mm_err(Into::into)?,
            fee_amount: sat_from_big_decimal(&tx_fee, self.decimals()).mm_err(Into::into)?,
            unused_change: None,
            kmd_rewards: None,
        };
        Ok((tx, additional_data))
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn gen_tx_from_shielded_wallet_db(
        &self,
        t_outputs: Vec<TxOut>,
        z_outputs: Vec<ZOutput>,
        tx_fee: BigDecimal,
    ) -> Result<(ZTransaction, AdditionalTxData), MmError<GenTxError>> {
        if !t_outputs.is_empty() {
            return MmError::err(GenTxError::UnsupportedLightWalletOutput);
        }
        if !self.shielded_wallet_db_scan_complete() {
            return MmError::err(GenTxError::ShieldedWalletDb(
                "shielded wallet DB scan is not complete".to_owned(),
            ));
        }

        let z_output_sat: u64 = z_outputs.iter().fold(0, |cur, out| cur + u64::from(out.amount));
        let tx_fee_sat = sat_from_big_decimal(&tx_fee, self.decimals()).mm_err(Into::into)?;
        let total_required_sat = z_output_sat
            .checked_add(tx_fee_sat)
            .or_mm_err(|| GenTxError::NumConversion(NumConversError("ZCoin total output overflow".to_owned())))?;
        let target_value = Amount::from_u64(total_required_sat).map_to_mm(|_| {
            GenTxError::NumConversion(NumConversError(format!(
                "Failed to get ZCash target amount from {}",
                total_required_sat
            )))
        })?;

        let mut wallet_db = WalletDb::for_path(
            self.z_fields.shielded_history.wallet_db_path(),
            self.z_fields.consensus_params.clone(),
            zcash_client_sqlite::util::SystemClock,
            rand::rngs::OsRng,
        )
        .map_to_mm(|e| GenTxError::ShieldedWalletDb(e.to_string()))?;
        let account_id = wallet_db
            .get_account_ids()
            .map_to_mm(|e| GenTxError::ShieldedWalletDb(e.to_string()))?
            .into_iter()
            .next()
            .or_mm_err(|| GenTxError::ShieldedWalletDb("shielded wallet DB has no account".to_owned()))?;
        let (height, anchor_height) = wallet_db
            .get_target_and_anchor_heights(NonZeroU32::MIN)
            .map_to_mm(|e| GenTxError::ShieldedWalletDb(e.to_string()))?
            .or_mm_err(|| GenTxError::ShieldedWalletDb("shielded wallet DB scan is required".to_owned()))?;
        let selected_notes = wallet_db
            .select_spendable_notes(
                account_id,
                TargetValue::AtLeast(target_value),
                &[ShieldedProtocol::Sapling],
                height,
                ConfirmationsPolicy::MIN,
                &[],
            )
            .map_to_mm(|e| GenTxError::ShieldedWalletDb(e.to_string()))?
            .take_sapling();
        let selected_value_sat = selected_notes
            .iter()
            .try_fold(0u64, |sum, note| {
                note.note_value()
                    .ok()
                    .and_then(|value| sum.checked_add(u64::from(value)))
            })
            .or_mm_err(|| GenTxError::NumConversion(NumConversError("ZCoin selected note overflow".to_owned())))?;
        if selected_value_sat < total_required_sat {
            return MmError::err(GenTxError::InsufficientBalance {
                coin: self.ticker().into(),
                available: big_decimal_from_sat_unsigned(selected_value_sat, self.decimals()),
                required: big_decimal_from_sat_unsigned(total_required_sat, self.decimals()),
            });
        }

        let sapling_anchor = wallet_db
            .with_sapling_tree_mut(|tree| tree.root_at_checkpoint_id(&anchor_height))
            .map_to_mm(|e| GenTxError::ShieldedWalletDb(e.to_string()))?
            .or_mm_err(|| GenTxError::ShieldedWalletDb("shielded wallet anchor is unavailable".to_owned()))?;
        let mut selected_notes_with_paths = Vec::with_capacity(selected_notes.len());
        for selected in selected_notes {
            let merkle_path = wallet_db
                .with_sapling_tree_mut(|tree| {
                    tree.witness_at_checkpoint_id_caching(selected.note_commitment_tree_position(), &anchor_height)
                })
                .map_to_mm(|e| GenTxError::ShieldedWalletDb(e.to_string()))?
                .or_mm_err(|| GenTxError::FailedToGetMerklePath)?;
            selected_notes_with_paths.push((selected.note().clone(), merkle_path));
        }

        let mut tx_builder = ZTxBuilder::new(
            self.z_fields.consensus_params.clone(),
            height.into(),
            BuildConfig::Standard {
                sapling_anchor: Some(sapling_anchor.into()),
                orchard_anchor: None,
            },
        );
        let fvk = FullViewingKey::from_expanded_spending_key(&self.z_fields.z_spending_key.expsk);
        for (note, merkle_path) in selected_notes_with_paths {
            tx_builder.add_sapling_spend(fvk.clone(), note, merkle_path)?;
        }

        let mut received_by_me = 0u64;
        for z_out in z_outputs {
            if z_out.to_addr == self.z_fields.my_z_addr {
                received_by_me += u64::from(z_out.amount);
            }
            tx_builder.add_sapling_output(
                z_out.viewing_key,
                z_out.to_addr,
                z_out.amount,
                z_out.memo.unwrap_or_else(MemoBytes::empty),
            )?;
        }

        let change_sat = selected_value_sat - total_required_sat;
        if change_sat > 0 {
            received_by_me += change_sat;
            tx_builder.add_sapling_output(
                None,
                self.z_fields.my_z_addr.clone(),
                Amount::from_u64(change_sat).map_to_mm(|_| {
                    GenTxError::NumConversion(NumConversError(format!(
                        "Failed to get ZCash change amount from {}",
                        change_sat
                    )))
                })?,
                MemoBytes::empty(),
            )?;
        }

        let fee_rule = FixedFeeRule::non_standard(
            Amount::from_u64(tx_fee_sat)
                .map_to_mm(|_| GenTxError::NumConversion(NumConversError("Invalid ZCash fee amount".to_owned())))?,
        );
        let tx = tokio::task::block_in_place(|| {
            tx_builder.build(
                &TransparentSigningSet::new(),
                std::slice::from_ref(&self.z_fields.z_spending_key),
                &[],
                rand::rngs::OsRng,
                &self.z_fields.z_tx_prover,
                &self.z_fields.z_tx_prover,
                &fee_rule,
            )
        })?
        .into_transaction();
        let additional_data = AdditionalTxData {
            received_by_me,
            spent_by_me: selected_value_sat,
            fee_amount: tx_fee_sat,
            unused_change: None,
            kmd_rewards: None,
        };
        Ok((tx, additional_data))
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn send_outputs(
        &self,
        t_outputs: Vec<TxOut>,
        z_outputs: Vec<ZOutput>,
    ) -> Result<ZTransaction, MmError<SendOutputsErr>> {
        let (tx, _) = self.gen_tx(t_outputs, z_outputs).await.mm_err(Into::into)?;
        let mut tx_bytes = Vec::with_capacity(1024);
        tx.write(&mut tx_bytes).expect("Write should not fail");

        self.rpc_client()
            .send_raw_transaction(tx_bytes.into())
            .compat()
            .await
            .mm_err(Into::into)?;

        self.rpc_client()
            .wait_for_confirmations(
                H256Json::from(*tx.txid().as_ref()).reversed(),
                tx.expiry_height().into(),
                1,
                false,
                now_ms() + 4000,
                10,
            )
            .compat()
            .await
            .map_to_mm(SendOutputsErr::TxNotMined)?;
        Ok(tx)
    }

    pub async fn get_unspent_witness(
        &self,
        note: &Note,
        tx_height: u32,
    ) -> Result<IncrementalWitness, MmError<GetUnspentWitnessErr>> {
        let mut attempts = 0;
        let states = loop {
            let states = self
                .z_fields
                .sapling_cache
                .query_states_after_height(tx_height)
                .await
                .map_err(|e| MmError::new(GetUnspentWitnessErr::StorageError(e.to_string())))?;
            if states.is_empty() {
                if attempts > 2 {
                    return MmError::err(GetUnspentWitnessErr::EmptyDbResult);
                }
                attempts += 1;
                Timer::sleep(10.).await;
            } else {
                break states;
            }
        };

        let mut tree = states[0].prev_tree_state.clone();
        let mut witness = None::<IncrementalWitness>;

        use keys::hash::H256;
        let note_cmu = H256::from(note.cmu().to_bytes());
        for state in states {
            for cmu in state.cmus {
                let build_witness = cmu == note_cmu;
                let node = Option::from(Node::from_bytes(cmu.take()))
                    .or_mm_err(|| GetUnspentWitnessErr::TreeOrWitnessAppendFailed)?;
                match witness {
                    Some(ref mut w) => w
                        .append(node)
                        .map_to_mm(|_| GetUnspentWitnessErr::TreeOrWitnessAppendFailed)?,
                    None => tree
                        .append(node)
                        .map_to_mm(|_| GetUnspentWitnessErr::TreeOrWitnessAppendFailed)?,
                };

                if build_witness {
                    witness = Some(
                        IncrementalWitness::from_tree(tree.clone())
                            .or_mm_err(|| GetUnspentWitnessErr::TreeOrWitnessAppendFailed)?,
                    );
                }
            }
        }

        witness.or_mm_err(|| GetUnspentWitnessErr::OutputCmuNotFoundInCache)
    }

    #[inline(always)]
    pub(crate) fn into_weak_parts(self) -> (UtxoWeak, Weak<ZCoinFields>) {
        (self.utxo_arc.downgrade(), Arc::downgrade(&self.z_fields))
    }

    pub(crate) fn from_weak_parts(utxo: &UtxoWeak, z_fields: &Weak<ZCoinFields>) -> Option<Self> {
        let utxo_arc = utxo.upgrade()?;
        let z_fields = z_fields.upgrade()?;

        Some(ZCoin { utxo_arc, z_fields })
    }
}
