use super::*;

/// How long a send waits for an in-flight shielded spend to be scanned before it
/// gives up (R39.8.0at requires the wait to be bounded and to end in a typed
/// failure). One pass costs at most a block plus a shielded-sync poll period, so
/// this allows several attempts without outliving the swap timeouts above it.
#[cfg(not(target_arch = "wasm32"))]
const IN_FLIGHT_SPEND_WAIT_SECS: u64 = 300;

/// Gap between re-selection attempts while waiting. Matches the polling cadence
/// used elsewhere on this path.
#[cfg(not(target_arch = "wasm32"))]
const IN_FLIGHT_SPEND_RETRY_SECS: f64 = 10.;

impl ZCoin {
    #[inline(always)]
    #[cfg(not(target_arch = "wasm32"))]
    pub fn z_rpc(&self) -> &(dyn ZRpcOps + Send + Sync) { self.utxo_arc.rpc_client.as_ref() }

    #[inline(always)]
    pub fn rpc_client(&self) -> &UtxoRpcClientEnum { &self.utxo_arc.rpc_client }

    /// How this coin reaches the chain. Decided at construction and carried on the coin; every
    /// mode-dependent branch reads it (R39.11.4g).
    #[inline(always)]
    pub fn mode(&self) -> ZcoinMode { self.z_fields.mode }

    #[inline(always)]
    pub fn is_light(&self) -> bool { self.z_fields.mode.is_light() }

    /// The lightwalletd client of a Light-mode coin.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn lightwalletd(&self) -> Option<&LightwalletdClient> {
        match self.rpc_client() {
            UtxoRpcClientEnum::LightWalletD(client) => Some(client),
            _ => None,
        }
    }

    /// The lightwalletd endpoints admission lets this coin use right now (R39.11.7a). Fails with a
    /// typed error that tells "every endpoint unreachable" from "every reachable endpoint excluded"
    /// (R39.11.6).
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn admitted_lightwalletd_servers(&self) -> Result<Vec<String>, LightwalletdError> {
        let client = self.lightwalletd().ok_or(LightwalletdError::Unsupported {
            op: "admission",
            reason: "the coin is not in Light mode",
        })?;
        client.backend().admitted().await
    }

    /// The endpoints excluded by the latest admission decision, each with its reason (R39.11.7a(f)).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn excluded_lightwalletd_endpoints(&self) -> Vec<(String, String)> {
        self.lightwalletd()
            .map(|client| client.backend().excluded_endpoints())
            .unwrap_or_default()
    }

    /// The raw bytes and mined height of a transaction by id (display order), from whichever backend
    /// serves the coin. A Light coin takes both from `GetTransaction`; nothing is decoded server-side.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) async fn raw_transaction_with_height(
        &self,
        txid_display: &H256Json,
    ) -> Result<(Vec<u8>, Option<u64>), String> {
        match self.rpc_client() {
            UtxoRpcClientEnum::LightWalletD(client) => client
                .raw_transaction(txid_display)
                .await
                .map(|raw| (raw.data, raw.height))
                .map_err(|e| e.to_string()),
            other => other
                .get_verbose_transaction(txid_display)
                .compat()
                .await
                .map(|tx| (tx.hex.0, tx.height))
                .map_err(|e| e.to_string()),
        }
    }

    /// The one height every shielded construction path hands its transaction builder
    /// (R39.11.9, R39.11.11).
    ///
    /// Light mode: the admitted lightwalletd endpoints' tip plus one, refused when it cannot be
    /// corroborated and never replaced by the wallet database's own target height, a last known tip
    /// or the scanned height. Native mode: the full node's block count, as before.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) async fn builder_target_height(&self) -> Result<u32, MmError<BuilderHeightError>> {
        match self.rpc_client() {
            UtxoRpcClientEnum::LightWalletD(client) => {
                let tip = client
                    .backend()
                    .fresh_tip()
                    .await
                    .map_err(|e| MmError::new(BuilderHeightError::from(e)))?;
                let scanned = self.z_fields.wallet_db_scanned_through.load(AtomicOrdering::Relaxed);
                let target = z_lightwalletd::builder_target_height(tip, scanned)
                    .map_err(|e| MmError::new(BuilderHeightError::from(e)))?;
                // The version the builder will pick follows from the height it is given, so the
                // activation height must be known before it is handed one (R39.6.4e).
                self.ensure_ironwood_activation(client.backend()).await?;
                Ok(target)
            },
            other => other
                .get_block_count()
                .compat()
                .await
                .map(|height| height as u32)
                .map_err(|e| MmError::new(BuilderHeightError::TipUnavailable(e.to_string()))),
        }
    }

    /// Makes sure the builder will see the coin's Ironwood activation height if it has one.
    ///
    /// Nothing is done when a height is already configured or discovered. Otherwise the server's
    /// branch and the activation timestamp are read ([`z_ironwood::discover`]); a height found is
    /// recorded in the coin's shared consensus parameters, and "not activated" records nothing, so
    /// the coin keeps building pre-Ironwood transactions and asks again next time. If the answer
    /// cannot be established the build is refused: the wrong transaction version is rejected by the
    /// network, which reports it as an opaque failure after the fact.
    #[cfg(not(target_arch = "wasm32"))]
    async fn ensure_ironwood_activation(
        &self,
        backend: &z_lightwalletd::LightwalletdBackend,
    ) -> Result<(), MmError<BuilderHeightError>> {
        use super::z_ironwood::{discover, IronwoodStatus};
        use zcash_protocol::consensus::{NetworkUpgrade, Parameters};

        let params = &self.z_fields.consensus_params;
        if params.activation_height(NetworkUpgrade::Nu6_3).is_some() {
            return Ok(());
        }
        let activation_time = super::configured_or_builtin_ironwood_time(params, self.ticker());
        match discover(backend, activation_time).await {
            Ok(IronwoodStatus::ActiveFrom(height)) => {
                common::log::info!("{}: Ironwood is active from height {}", self.ticker(), height);
                params.record_discovered_ironwood_height(height);
                Ok(())
            },
            Ok(IronwoodStatus::NotActivated) => Ok(()),
            Err(e) => MmError::err(BuilderHeightError::IronwoodActivation(e.to_string())),
        }
    }

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

    /// Records why the commitment-tree sync loop could not read from its backend.
    pub(crate) fn set_sapling_loop_error(&self, message: String) {
        if let Ok(mut slot) = self.z_fields.sapling_loop_error.lock() {
            *slot = Some(message);
        }
    }

    pub(crate) fn clear_sapling_loop_error(&self) {
        if let Ok(mut slot) = self.z_fields.sapling_loop_error.lock() {
            *slot = None;
        }
    }

    /// The current failure of the commitment-tree sync loop, if its backend is failing right now.
    pub fn sapling_loop_error(&self) -> Option<String> {
        self.z_fields
            .sapling_loop_error
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
    }

    /// Whether the commitment-tree sync loop has ended (finished, bailed out or panicked). When it
    /// has ended without `is_sapling_state_synced` ever becoming true, the sync died.
    pub fn sapling_loop_stopped(&self) -> bool { self.z_fields.sapling_loop_stopped.load(AtomicOrdering::SeqCst) }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn shielded_wallet_db_scan_complete(&self) -> bool {
        self.z_fields.wallet_db_scan_complete.load(AtomicOrdering::Relaxed)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn fetch_lightwalletd_compact_blocks_to_height(
        &self,
        target_height: u64,
        requested_start_height: Option<u64>,
        skip_sync_params: bool,
        progress: &(dyn Fn(u64, u64) + Send + Sync),
    ) -> Result<u64, ShieldedFetchError> {
        // Only admitted endpoints: an off-branch endpoint above the wallet's anchor would otherwise
        // make the wallet rewind and follow that endpoint's chain (R39.11.7a).
        let admitted = self
            .admitted_lightwalletd_servers()
            .await
            .map_err(|e| ShieldedFetchError::BeforeSync(e.to_string()))?;
        let servers = admitted.as_slice();
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

    /// WASM has no wallet database. Its commitment-tree sync starts right after the coin
    /// config's checkpoint block, or at Sapling activation when the config has none, so the
    /// anchor is the checkpoint height (CRD ch.39 R39.10.9, `first_sync_block`).
    #[cfg(target_arch = "wasm32")]
    pub fn shielded_wallet_sync_start_height(&self) -> Option<u64> {
        self.z_fields
            .check_point_block
            .as_ref()
            .map(|check_point| u64::from(check_point.height))
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
                self.clear_shielded_sync_error();
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
                self.set_shielded_sync_error(&err);
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

    /// Records why the shielded sync last failed, so the reported history-sync
    /// state can say "failed" rather than "still scanning" (R39.8.0au). A later
    /// successful scan clears it.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn set_shielded_sync_error(&self, message: &str) {
        if let Ok(mut slot) = self.z_fields.shielded_sync_error.lock() {
            *slot = Some(message.to_owned());
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn clear_shielded_sync_error(&self) {
        if let Ok(mut slot) = self.z_fields.shielded_sync_error.lock() {
            *slot = None;
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn shielded_sync_error(&self) -> Option<String> {
        self.z_fields
            .shielded_sync_error
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
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
        self.prepare_shielded_build().await?;
        let _lock = self.z_fields.z_unspent_mutex.lock().await;
        self.forget_released_in_flight_spends();
        // Only this entry point names the cause: `send_outputs` calls `gen_tx_locked` directly and
        // waits out a transient shortfall, so it must keep seeing the plain error (D39.8.0e).
        self.gen_tx_locked(t_outputs, z_outputs)
            .await
            .map_err(|e| e.map(|e| explain_shortfall(e, self.in_flight_spend_total(), self.decimals())))
    }

    /// The gates that must clear before the critical section is entered.
    ///
    /// Every ARRR transaction is built through here, including withdrawals, which
    /// never pass the swap gates. The Ironwood refusal comes first deliberately:
    /// refusing after the wait would make the caller block first and be refused
    /// second (R39.6.4c). The sapling-state wait stays outside the section because
    /// the section must not be held across a wait (R39.8.0at).
    #[cfg(not(target_arch = "wasm32"))]
    async fn prepare_shielded_build(&self) -> Result<(), MmError<GenTxError>> {
        if self.ironwood_build_refused() {
            return MmError::err(GenTxError::IronwoodUpgradeUnsupported {
                coin: self.ticker().to_owned(),
                // The effective time, not the configured one: the guard fires on a
                // built-in activation time too (R39.6.4d), and reporting the
                // configured value would name `0` in exactly that case.
                activation_time: self.effective_ironwood_activation_time().unwrap_or_default(),
            });
        }
        while !self.is_sapling_state_synced() {
            Timer::sleep(0.5).await
        }
        Ok(())
    }

    /// Selects notes and builds the transaction. The caller must already hold
    /// `z_unspent_mutex` and must keep holding it through broadcast and recording
    /// (R39.8.0at).
    #[cfg(not(target_arch = "wasm32"))]
    async fn gen_tx_locked(
        &self,
        t_outputs: Vec<TxOut>,
        z_outputs: Vec<ZOutput>,
    ) -> Result<(ZTransaction, AdditionalTxData), MmError<GenTxError>> {
        let tx_fee = self.get_one_kbyte_tx_fee().await.mm_err(Into::into)?;
        // One height authority for every construction path (R39.11.11); the read fails the build
        // rather than falling back to another source.
        let builder_height = self.builder_target_height().await.mm_err(GenTxError::from)?;
        if self.is_light() {
            return self.gen_tx_from_shielded_wallet_db(t_outputs, z_outputs, tx_fee, builder_height);
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

        let current_block = builder_height;
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
            // Decryption-only parameters, so a counterparty's post-ZIP-212 note is
            // not silently discarded (R39.8.0am).
            let decryption_params = ZcoinDecryptionParams::new(self.z_fields.consensus_params.clone());
            let decrypted = decrypt_transaction(
                &decryption_params,
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
                ironwood_anchor: None,
                orchard_padding: BundlePadding::DEFAULT,
                ironwood_padding: BundlePadding::DEFAULT,
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
        builder_height: u32,
    ) -> Result<(ZTransaction, AdditionalTxData), MmError<GenTxError>> {
        if !self.shielded_wallet_db_scan_complete() {
            return MmError::err(GenTxError::ShieldedWalletDb(
                "shielded wallet DB scan is not complete".to_owned(),
            ));
        }

        let inputs = WalletDbBuild {
            ticker: self.ticker(),
            decimals: self.decimals(),
            wallet_db_path: self.z_fields.shielded_history.wallet_db_path(),
            params: &self.z_fields.consensus_params,
            spending_key: &self.z_fields.z_spending_key,
            my_addr: &self.z_fields.my_z_addr,
            prover: &self.z_fields.z_tx_prover,
        };
        build_from_wallet_db(&inputs, t_outputs, z_outputs, tx_fee, builder_height)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn send_outputs(
        &self,
        t_outputs: Vec<TxOut>,
        z_outputs: Vec<ZOutput>,
    ) -> Result<ZTransaction, MmError<SendOutputsErr>> {
        self.prepare_shielded_build().await.mm_err(SendOutputsErr::from)?;

        let deadline = now_ms() / 1000 + IN_FLIGHT_SPEND_WAIT_SECS;
        let tx = loop {
            // Selection, construction, broadcast and recording are one critical
            // section. Selecting outside it would let a second send choose the same
            // notes before this one records its broadcast, and the loser is rejected
            // by the network as a duplicate nullifier (R39.8.0at).
            let broadcast = {
                let _lock = self.z_fields.z_unspent_mutex.lock().await;
                self.forget_released_in_flight_spends();

                match self.gen_tx_locked(t_outputs.clone(), z_outputs.clone()).await {
                    Ok((tx, additional_data)) => {
                        let mut tx_bytes = Vec::with_capacity(1024);
                        tx.write(&mut tx_bytes).expect("Write should not fail");

                        self.rpc_client()
                            .send_raw_transaction(tx_bytes.into())
                            .compat()
                            .await
                            .mm_err(SendOutputsErr::from)?;

                        // Only after the backend accepted it: a refused broadcast must
                        // leave every selected note immediately reusable (R39.8.0ap).
                        self.record_sent_shielded_tx(&tx, additional_data.fee_amount);
                        Some(tx)
                    },
                    // A shortfall that no in-flight spend can cover will not improve by
                    // waiting, so it fails now instead of consuming the budget.
                    Err(e) if !self.shortfall_can_clear(e.get_inner()) => return Err(e).mm_err(SendOutputsErr::from),
                    Err(_) => None,
                }
            };

            match broadcast {
                Some(tx) => break tx,
                None => {
                    if now_ms() / 1000 >= deadline {
                        return MmError::err(SendOutputsErr::InFlightSpendWaitTimeout(IN_FLIGHT_SPEND_WAIT_SECS));
                    }
                    // Deliberately outside the section: the shortfall clears only when
                    // the scanner advances, and it cannot advance while we hold the
                    // section (R39.8.0at).
                    Timer::sleep(IN_FLIGHT_SPEND_RETRY_SECS).await;
                },
            }
        };

        self.rpc_client()
            .wait_for_confirmations(
                H256Json::from(*tx.txid().as_ref()).reversed(),
                tx.expiry_height().into(),
                1,
                false,
                // Seconds, as `wait_for_confirmations` compares it against `now_ms() / 1000`.
                now_ms() / 1000 + 4000,
                10,
            )
            .compat()
            .await
            .map_to_mm(SendOutputsErr::TxNotMined)?;
        Ok(tx)
    }

    /// Records a just-broadcast transaction in the shielded wallet database so the notes it
    /// spends stop being selected before the block containing it is scanned.
    ///
    /// Light mode only. The native path selects through `z_list_unspent`, where the daemon
    /// already accounts for its own mempool spends.
    ///
    /// Best-effort by construction: the transaction is on the network by the time this runs,
    /// so a failure here must not turn a sent transaction into a caller-visible error. It
    /// degrades to the previous behaviour, which the scan corrects once the block lands.
    #[cfg(not(target_arch = "wasm32"))]
    fn record_sent_shielded_tx(&self, tx: &ZTransaction, fee_sat: u64) {
        if !self.is_light() {
            return;
        }

        match record_sent_in_wallet_db(
            self.z_fields.shielded_history.wallet_db_path(),
            &self.z_fields.consensus_params,
            tx,
            fee_sat,
        ) {
            Ok(()) => self.note_in_flight_spend(tx),
            Err(e) => log::warn!(
                "ZCoin {}: could not record sent transaction {:?} in the shielded wallet DB: {}. \
                 Its notes stay selectable until the containing block is scanned.",
                self.ticker(),
                tx.txid(),
                e
            ),
        }
    }

    /// Records a transaction the backend has just accepted through `send_raw_transaction`, which
    /// is how a withdrawal reaches the network: it is built and returned, and the caller submits
    /// it (R39.8.0ap, D39.8.0e). Only the raw bytes are available here, so everything is derived
    /// from them.
    ///
    /// A transaction with no Sapling spend, or one that spends none of this wallet's notes, is
    /// left alone: the wallet database matches spends to its own notes by nullifier and ignores
    /// the rest. A build that is never submitted never reaches this point and records nothing, so
    /// an abandoned withdrawal strands no notes.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn record_broadcast_shielded_tx(&self, tx_bytes: &[u8]) {
        if !self.is_light() {
            return;
        }
        match record_broadcast_in_wallet_db(
            self.z_fields.shielded_history.wallet_db_path(),
            &self.z_fields.consensus_params,
            tx_bytes,
        ) {
            Ok(Some(tx)) => self.note_in_flight_spend(&tx),
            Ok(None) => {},
            Err(e) => log::warn!(
                "ZCoin {}: could not record a broadcast transaction in the shielded wallet DB: {}. \
                 Its notes stay selectable until the containing block is scanned.",
                self.ticker(),
                e
            ),
        }
    }

    /// Remembers that `tx` commits the notes it spends, so a shortfall can be told apart from one
    /// no waiting will fix. The value is read back from the wallet database record just written.
    #[cfg(not(target_arch = "wasm32"))]
    fn note_in_flight_spend(&self, tx: &ZTransaction) {
        let txid: [u8; 32] = *tx.txid().as_ref();
        let value = match self.z_fields.shielded_history.spent_value_of_transaction(&txid) {
            Ok(value) => value,
            Err(e) => {
                log::warn!(
                    "ZCoin {}: could not read the value spent by {:?}: {}",
                    self.ticker(),
                    tx.txid(),
                    e
                );
                return;
            },
        };
        // None of this wallet's notes: nothing is committed, so there is nothing to track.
        if value == 0 {
            return;
        }
        if let Ok(mut in_flight) = self.z_fields.in_flight_spends.lock() {
            in_flight.insert(txid, InFlightSpend {
                value,
                expiry_height: tx.expiry_height().into(),
            });
        }
    }

    /// The height the wallet database judges transaction expiry against, when it has one.
    #[cfg(not(target_arch = "wasm32"))]
    fn wallet_db_target_height(&self) -> Option<u32> {
        let wallet_db = WalletDb::for_path(
            self.z_fields.shielded_history.wallet_db_path(),
            self.z_fields.consensus_params.clone(),
            zcash_client_sqlite::util::SystemClock,
            rand::rngs::OsRng,
        )
        .ok()?;
        let (target_height, _) = wallet_db.get_target_and_anchor_heights(NonZeroU32::MIN).ok()??;
        Some(target_height.into())
    }

    /// Drops in-flight entries that no longer hold anything: those whose transaction the wallet
    /// database has since scanned (from then on the database's own spend record is the exclusion,
    /// R39.8.0aq, so keeping the entry would only delay a later send), and those whose expiry
    /// height the chain has passed, which can never be mined (R39.8.0ar).
    ///
    /// The expiry test uses the same height the wallet database uses to decide that the
    /// transaction's notes are free again, so this map and the database release together. Without
    /// the second rule a broadcast transaction that is never mined, and so never scanned, would stay here for the
    /// life of the process.
    #[cfg(not(target_arch = "wasm32"))]
    fn forget_released_in_flight_spends(&self) {
        let target_height = self.wallet_db_target_height();
        let mut in_flight = match self.z_fields.in_flight_spends.lock() {
            Ok(in_flight) => in_flight,
            Err(_) => return,
        };
        in_flight.retain(|txid, spend| {
            let scanned = matches!(self.z_fields.shielded_history.transaction_is_scanned(txid), Ok(true));
            !scanned && !in_flight_spend_expired(spend, target_height)
        });
    }

    /// Total value currently committed to this process's in-flight spends.
    #[cfg(not(target_arch = "wasm32"))]
    fn in_flight_spend_total(&self) -> u64 {
        self.z_fields
            .in_flight_spends
            .lock()
            .map(|in_flight| {
                in_flight
                    .values()
                    .fold(0u64, |acc, spend| acc.saturating_add(spend.value))
            })
            .unwrap_or(0)
    }

    /// Whether a shortfall could still be covered once this process's in-flight
    /// spends have been scanned. A request that cannot be funded even with every
    /// note unexcluded fails immediately rather than waiting (R39.8.0at).
    #[cfg(not(target_arch = "wasm32"))]
    fn shortfall_can_clear(&self, err: &GenTxError) -> bool {
        let (available, required) = match err {
            GenTxError::InsufficientBalance {
                available, required, ..
            } => (available, required),
            _ => return false,
        };
        let in_flight = big_decimal_from_sat_unsigned(self.in_flight_spend_total(), self.decimals());
        in_flight > BigDecimal::from(0u8) && available + in_flight >= *required
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

/// The parts of a [`ZCoin`] that building from the shielded wallet database reads. Taken
/// as a value so the build can run against a wallet database alone.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct WalletDbBuild<'a> {
    pub(crate) ticker: &'a str,
    pub(crate) decimals: u8,
    pub(crate) wallet_db_path: &'a Path,
    pub(crate) params: &'a ZcoinConsensusParams,
    pub(crate) spending_key: &'a ExtendedSpendingKey,
    pub(crate) my_addr: &'a PaymentAddress,
    pub(crate) prover: &'a LocalTxProver,
}

/// Builds a transaction from the notes the shielded wallet database offers as spendable.
/// Records nothing: the caller decides whether and when the notes it spends are locked.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn build_from_wallet_db(
    inputs: &WalletDbBuild<'_>,
    t_outputs: Vec<TxOut>,
    z_outputs: Vec<ZOutput>,
    tx_fee: BigDecimal,
    builder_height: u32,
) -> Result<(ZTransaction, AdditionalTxData), MmError<GenTxError>> {
    let t_output_sat: u64 = t_outputs.iter().fold(0, |cur, out| cur + u64::from(out.value()));
    let z_output_sat: u64 = z_outputs.iter().fold(0, |cur, out| cur + u64::from(out.amount));
    let tx_fee_sat = sat_from_big_decimal(&tx_fee, inputs.decimals).mm_err(Into::into)?;
    let total_required_sat = t_output_sat
        .checked_add(z_output_sat)
        .and_then(|outputs_sat| outputs_sat.checked_add(tx_fee_sat))
        .or_mm_err(|| GenTxError::NumConversion(NumConversError("ZCoin total output overflow".to_owned())))?;
    let target_value = Amount::from_u64(total_required_sat).map_to_mm(|_| {
        GenTxError::NumConversion(NumConversError(format!(
            "Failed to get ZCash target amount from {}",
            total_required_sat
        )))
    })?;

    let mut wallet_db = WalletDb::for_path(
        inputs.wallet_db_path,
        inputs.params.clone(),
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
            &[ShieldedPool::Sapling],
            height,
            ConfirmationsPolicy::MIN,
            &[],
            // The wallet takes no shielded note locks, so lock state is ignored.
            LockFilter::Unfiltered,
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
            coin: inputs.ticker.into(),
            available: big_decimal_from_sat_unsigned(selected_value_sat, inputs.decimals),
            required: big_decimal_from_sat_unsigned(total_required_sat, inputs.decimals),
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

    // `height` above only selects notes and picks the anchor from the wallet database. The builder
    // takes the single height authority's value so this path and the HTLC spend/refund path can
    // never disagree about a transaction's expiry (R39.11.11).
    let mut tx_builder = ZTxBuilder::new(inputs.params.clone(), builder_height.into(), BuildConfig::Standard {
        sapling_anchor: Some(sapling_anchor.into()),
        orchard_anchor: None,
        ironwood_anchor: None,
        orchard_padding: BundlePadding::DEFAULT,
        ironwood_padding: BundlePadding::DEFAULT,
    });
    let fvk = FullViewingKey::from_expanded_spending_key(&inputs.spending_key.expsk);
    for (note, merkle_path) in selected_notes_with_paths {
        tx_builder.add_sapling_spend(fvk.clone(), note, merkle_path)?;
    }

    let mut received_by_me = 0u64;
    for z_out in z_outputs {
        if z_out.to_addr == *inputs.my_addr {
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
            inputs.my_addr.clone(),
            Amount::from_u64(change_sat).map_to_mm(|_| {
                GenTxError::NumConversion(NumConversError(format!(
                    "Failed to get ZCash change amount from {}",
                    change_sat
                )))
            })?,
            MemoBytes::empty(),
        )?;
    }

    // Swap HTLCs are transparent P2SH outputs paid for out of shielded notes, so the
    // light-wallet path has to emit them too. Added last to keep the vout ordering the
    // native path produces: `z_p2sh_spend` spends transparent output 0.
    for output in t_outputs {
        tx_builder.add_transparent_output_raw(output);
    }

    let fee_rule = FixedFeeRule::non_standard(
        Amount::from_u64(tx_fee_sat)
            .map_to_mm(|_| GenTxError::NumConversion(NumConversError("Invalid ZCash fee amount".to_owned())))?,
    );
    let tx = tokio::task::block_in_place(|| {
        tx_builder.build(
            &TransparentSigningSet::new(),
            std::slice::from_ref(inputs.spending_key),
            &[],
            rand::rngs::OsRng,
            inputs.prover,
            inputs.prover,
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

/// Writes a transaction this wallet built into the shielded wallet database as sent and
/// unmined, which marks the notes it spends so `select_spendable_notes` stops offering them.
/// The wallet database releases them by itself once the transaction is mined (the scan
/// re-marks them) or once the chain passes the transaction's expiry height (R39.8.0ar).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn record_sent_in_wallet_db(
    wallet_db_path: &Path,
    params: &ZcoinConsensusParams,
    tx: &ZTransaction,
    fee_sat: u64,
) -> Result<(), MmError<RecordSentTxErr>> {
    let mut wallet_db = WalletDb::for_path(
        wallet_db_path,
        params.clone(),
        zcash_client_sqlite::util::SystemClock,
        rand::rngs::OsRng,
    )
    .map_to_mm(|e| RecordSentTxErr::ShieldedWalletDb(e.to_string()))?;
    let account_id = wallet_db
        .get_account_ids()
        .map_to_mm(|e| RecordSentTxErr::ShieldedWalletDb(e.to_string()))?
        .into_iter()
        .next()
        .or_mm_err(|| RecordSentTxErr::NoAccount)?;
    let (target_height, _) = wallet_db
        .get_target_and_anchor_heights(NonZeroU32::MIN)
        .map_to_mm(|e| RecordSentTxErr::ShieldedWalletDb(e.to_string()))?
        .or_mm_err(|| RecordSentTxErr::ScanRequired)?;
    let fee_amount = Amount::from_u64(fee_sat).map_to_mm(|_| RecordSentTxErr::InvalidFeeAmount(fee_sat))?;

    // No outputs are declared. The only wallet-internal output we create is change, which
    // cannot be spent until mined regardless, is already surfaced as a pending mempool
    // receipt, and is recorded for real when the block is scanned. Declaring none keeps
    // this write to the part that is load-bearing: marking the spent nullifiers, which
    // `select_spendable_notes` then excludes on its own.
    wallet_db
        .store_transactions_to_be_sent(&[SentTransaction::new(
            tx,
            time::OffsetDateTime::now_utc(),
            target_height,
            account_id,
            &[],
            fee_amount,
            &[],
        )])
        .map_to_mm(|e| RecordSentTxErr::ShieldedWalletDb(e.to_string()))
}

/// Names the cause of a shortfall that notes committed to a not-yet-scanned transaction explain:
/// the request would have been funded had those notes been free. Any other error, and a shortfall
/// no in-flight spend accounts for, is returned unchanged.
///
/// `in_flight_sat` is only what this process knows; after a restart it is zero and the bare
/// `InsufficientBalance` stands (D39.8.0e).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn explain_shortfall(err: GenTxError, in_flight_sat: u64, decimals: u8) -> GenTxError {
    let locked = big_decimal_from_sat_unsigned(in_flight_sat, decimals);
    match err {
        GenTxError::InsufficientBalance {
            coin,
            available,
            required,
        } if locked > BigDecimal::from(0u8) && &available + &locked >= required => {
            GenTxError::BalanceLockedByUnconfirmedTransaction {
                coin,
                available,
                required,
                locked,
            }
        },
        other => other,
    }
}

/// Whether the chain has passed an in-flight spend's expiry height, so the transaction can never
/// be mined. Mirrors the wallet database's own "unexpired" test (`expiry_height >= target height`,
/// zero meaning it never expires) so that the two release together. With no target height known
/// the spend is kept: releasing early is the unsafe direction.
#[cfg(not(target_arch = "wasm32"))]
fn in_flight_spend_expired(spend: &InFlightSpend, target_height: Option<u32>) -> bool {
    match target_height {
        Some(target) => spend.expiry_height != 0 && spend.expiry_height < target,
        None => false,
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod in_flight_tests {
    use super::*;

    const SPEND: InFlightSpend = InFlightSpend {
        value: 1,
        expiry_height: 100,
    };

    /// The wallet database holds the notes while `expiry_height >= target height`; the map must
    /// release on exactly the same boundary or the error would name a spend the database has
    /// already let go of.
    #[test]
    fn an_in_flight_spend_expires_when_the_target_height_passes_its_expiry() {
        assert!(!in_flight_spend_expired(&SPEND, Some(99)));
        assert!(
            !in_flight_spend_expired(&SPEND, Some(100)),
            "can still be mined in block 100"
        );
        assert!(in_flight_spend_expired(&SPEND, Some(101)));
    }

    #[test]
    fn an_in_flight_spend_is_kept_when_no_height_is_known() {
        assert!(!in_flight_spend_expired(&SPEND, None));
    }

    #[test]
    fn a_spend_with_no_expiry_never_expires() {
        let never = InFlightSpend {
            value: 1,
            expiry_height: 0,
        };
        assert!(!in_flight_spend_expired(&never, Some(u32::MAX)));
    }
}

/// Records raw transaction bytes the backend has accepted, deriving everything from the bytes.
///
/// `Ok(None)` when the bytes are not a transaction that spends Sapling notes (nothing to record).
/// A transaction that spends none of this wallet's notes is recorded harmlessly: the wallet
/// database matches spends to its own notes by nullifier and ignores the rest.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn record_broadcast_in_wallet_db(
    wallet_db_path: &Path,
    params: &ZcoinConsensusParams,
    tx_bytes: &[u8],
) -> Result<Option<ZTransaction>, String> {
    // The backend accepted bytes we cannot read: not a shielded transaction of ours.
    let Ok(tx) = z_tx_read::read_tx(tx_bytes) else {
        return Ok(None);
    };
    if !tx
        .sapling_bundle()
        .is_some_and(|bundle| !bundle.shielded_spends().is_empty())
    {
        return Ok(None);
    }
    let fee_sat = broadcast_fee_sat(&tx).ok_or_else(|| {
        format!(
            "transaction {:?} spends shielded notes but its fee cannot be derived from its bytes",
            tx.txid()
        )
    })?;
    record_sent_in_wallet_db(wallet_db_path, params, &tx, fee_sat).map_err(|e| e.to_string())?;
    Ok(Some(tx))
}

/// The fee of a transaction whose only shielded pool is Sapling and which has no transparent
/// inputs, from its bytes alone: the value leaving the Sapling pool less the transparent outputs
/// it pays. `None` when the transaction is any other shape, where the fee would need data the
/// bytes do not carry (transparent input values).
#[cfg(not(target_arch = "wasm32"))]
fn broadcast_fee_sat(tx: &ZTransaction) -> Option<u64> {
    if tx.orchard_bundle().is_some() || tx.ironwood_bundle().is_some() {
        return None;
    }
    let leaving_sapling = i64::from(tx.sapling_bundle()?.value_balance());
    let transparent_out = match tx.transparent_bundle() {
        Some(bundle) if !bundle.vin.is_empty() => return None,
        Some(bundle) => bundle.vout.iter().try_fold(0i64, |sum, out| {
            i64::try_from(u64::from(out.value()))
                .ok()
                .and_then(|value| sum.checked_add(value))
        })?,
        None => 0,
    };
    u64::try_from(leaving_sapling.checked_sub(transparent_out)?).ok()
}
