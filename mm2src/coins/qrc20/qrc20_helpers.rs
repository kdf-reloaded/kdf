// qrc20_helpers — Constructor, builder, internal methods, and UTXO trait implementations.

use super::*;

pub(crate) struct Qrc20CoinBuilder<'a> {
    ctx: &'a MmArc,
    ticker: &'a str,
    conf: &'a Json,
    activation_params: &'a Qrc20ActivationParams,
    priv_key: &'a [u8],
    platform: String,
    token_contract_address: H160,
}

impl<'a> Qrc20CoinBuilder<'a> {
    pub fn new(
        ctx: &'a MmArc,
        ticker: &'a str,
        conf: &'a Json,
        activation_params: &'a Qrc20ActivationParams,
        priv_key: &'a [u8],
        platform: String,
        token_contract_address: H160,
    ) -> Qrc20CoinBuilder<'a> {
        Qrc20CoinBuilder {
            ctx,
            ticker,
            conf,
            activation_params,
            priv_key,
            platform,
            token_contract_address,
        }
    }
}

#[async_trait]
impl<'a> UtxoCoinBuilderCommonOps for Qrc20CoinBuilder<'a> {
    fn ctx(&self) -> &MmArc { self.ctx }

    fn conf(&self) -> &Json { self.conf }

    fn activation_params(&self) -> &UtxoActivationParams { &self.activation_params.utxo_params }

    fn ticker(&self) -> &str { self.ticker }

    async fn decimals(&self, rpc_client: &UtxoRpcClientEnum) -> UtxoCoinBuildResult<u8> {
        if let Some(d) = self.conf()["decimals"].as_u64() {
            return Ok(d as u8);
        }

        rpc_client
            .token_decimals(&self.token_contract_address)
            .compat()
            .await
            .map_to_mm(UtxoCoinBuildError::ErrorDetectingDecimals)
    }

    fn dust_amount(&self) -> u64 { QRC20_DUST }

    #[cfg(not(target_arch = "wasm32"))]
    fn confpath(&self) -> UtxoCoinBuildResult<PathBuf> {
        use crate::utxo::coin_daemon_data_dir;

        // Documented at https://github.com/jl777/coins#bitcoin-protocol-specific-json
        // "USERHOME/" prefix should be replaced with the user's home folder.
        let declared_confpath = match self.conf()["confpath"].as_str() {
            Some(path) if !path.is_empty() => path.trim(),
            _ => {
                let is_asset_chain = false;
                let platform = self.platform.to_lowercase();
                let data_dir = coin_daemon_data_dir(&platform, is_asset_chain);

                let confname = format!("{}.conf", platform);
                return Ok(data_dir.join(&confname[..]));
            },
        };

        let (confpath, rel_to_home) = match declared_confpath.strip_prefix("~/") {
            Some(stripped) => (stripped, true),
            None => match declared_confpath.strip_prefix("USERHOME/") {
                Some(stripped) => (stripped, true),
                None => (declared_confpath, false),
            },
        };

        if rel_to_home {
            let home = dirs::home_dir().or_mm_err(|| UtxoCoinBuildError::CantDetectUserHome)?;
            Ok(home.join(confpath))
        } else {
            Ok(confpath.into())
        }
    }

    fn check_utxo_maturity(&self) -> bool {
        if let Some(false) = self.activation_params.utxo_params.check_utxo_maturity {
            warn!("'check_utxo_maturity' is ignored because QRC20 gas refund is returned as a coinbase transaction");
        }
        true
    }

    /// Override [`UtxoCoinBuilderCommonOps::tx_cache`] to initialize TX cache with the platform ticker.
    /// Please note the method is overridden for Native mode only.
    #[inline]
    #[cfg(not(target_arch = "wasm32"))]
    fn tx_cache(&self) -> UtxoVerboseCacheShared {
        crate::utxo::tx_cache::fs_tx_cache::FsVerboseCache::new(self.platform.clone(), self.tx_cache_path())
            .into_shared()
    }
}

#[async_trait]
impl<'a> UtxoFieldsWithIguanaPrivKeyBuilder for Qrc20CoinBuilder<'a> {}

#[async_trait]
impl<'a> UtxoCoinWithIguanaPrivKeyBuilder for Qrc20CoinBuilder<'a> {
    type ResultCoin = Qrc20Coin;
    type Error = UtxoCoinBuildError;

    fn priv_key(&self) -> &[u8] { self.priv_key }

    async fn build(self) -> MmResult<Self::ResultCoin, Self::Error> {
        let utxo = self.build_utxo_fields_with_iguana_priv_key(self.priv_key()).await?;
        let inner = Qrc20CoinFields {
            utxo,
            platform: self.platform,
            contract_address: self.token_contract_address,
            swap_contract_address: self.activation_params.swap_contract_address,
            fallback_swap_contract: self.activation_params.fallback_swap_contract,
        };
        Ok(Qrc20Coin(Arc::new(inner)))
    }
}

pub async fn qrc20_coin_from_conf_and_params(
    ctx: &MmArc,
    ticker: &str,
    platform: &str,
    conf: &Json,
    params: &Qrc20ActivationParams,
    priv_key: &[u8],
    contract_address: H160,
) -> Result<Qrc20Coin, String> {
    let builder = Qrc20CoinBuilder::new(
        ctx,
        ticker,
        conf,
        params,
        priv_key,
        platform.to_owned(),
        contract_address,
    );
    Ok(try_s!(builder.build().await))
}

impl Qrc20Coin {
    /// `gas_fee` should be calculated by: gas_limit * gas_price * (count of contract calls),
    /// or should be sum of gas fee of all contract calls.
    pub async fn get_qrc20_tx_fee(&self, gas_fee: u64) -> Result<u64, String> {
        match try_s!(self.get_tx_fee().await) {
            ActualTxFee::Dynamic(amount) | ActualTxFee::FixedPerKb(amount) => Ok(amount + gas_fee),
        }
    }

    /// Generate and send a transaction with the specified UTXO outputs.
    /// Note this function locks the `UTXO_LOCK`.
    pub async fn send_contract_calls(
        &self,
        outputs: Vec<ContractCallOutput>,
    ) -> Result<TransactionEnum, TransactionErr> {
        // TODO: we need to somehow refactor it using RecentlySpentOutpoints cache
        // Move over all QRC20 tokens should share the same cache with each other and base QTUM coin
        let _utxo_lock = UTXO_LOCK.lock().await;

        let GenerateQrc20TxResult { signed, .. } = self
            .generate_qrc20_transaction(outputs)
            .await
            .map_err(|e| TransactionErr::Plain(ERRL!("{}", e)))?;
        try_tx_s!(self.utxo.rpc_client.send_transaction(&signed).compat().await, signed);
        Ok(signed.into())
    }

    /// Generate Qtum UTXO transaction with contract calls.
    /// Note: lock the UTXO_LOCK mutex before this function will be called.
    pub(crate) async fn generate_qrc20_transaction(
        &self,
        contract_outputs: Vec<ContractCallOutput>,
    ) -> Result<GenerateQrc20TxResult, MmError<Qrc20GenTxError>> {
        let my_address = self.utxo.derivation_method.iguana_or_err().mm_err(Into::into)?;
        let (unspents, _) = self.get_unspent_ordered_list(my_address).await.mm_err(Into::into)?;

        let mut gas_fee = 0;
        let mut outputs = Vec::with_capacity(contract_outputs.len());
        for output in contract_outputs {
            gas_fee += output.gas_limit * output.gas_price;
            outputs.push(TransactionOutput::from(output));
        }

        let (unsigned, data) = UtxoTxBuilder::new(self)
            .add_available_inputs(unspents)
            .add_outputs(outputs)
            .with_gas_fee(gas_fee)
            .build()
            .await
            .mm_err(Into::into)?;

        let my_address = self.utxo.derivation_method.iguana_or_err().mm_err(Into::into)?;
        let key_pair = self.utxo.priv_key_policy.key_pair_or_err().mm_err(Into::into)?;

        let prev_script = ScriptBuilder::build_p2pkh(&my_address.hash);
        let signed = sign_tx(
            unsigned,
            key_pair,
            prev_script,
            self.utxo.conf.signature_version,
            self.utxo.conf.fork_id,
        )
        .mm_err(Into::into)?;

        let miner_fee = data.fee_amount + data.unused_change.unwrap_or_default();
        Ok(GenerateQrc20TxResult {
            signed,
            miner_fee,
            gas_fee,
        })
    }

    pub(crate) fn transfer_output(
        &self,
        to_addr: H160,
        amount: U256,
        gas_limit: u64,
        gas_price: u64,
    ) -> Qrc20AbiResult<ContractCallOutput> {
        let function = eth::ERC20_CONTRACT.function("transfer")?;
        let params = function.encode_input(&[Token::Address(to_addr), Token::Uint(amount)])?;

        let script_pubkey =
            generate_contract_call_script_pubkey(&params, gas_limit, gas_price, self.contract_address.as_bytes())?
                .to_bytes();

        Ok(ContractCallOutput {
            value: OUTPUT_QTUM_AMOUNT,
            script_pubkey,
            gas_limit,
            gas_price,
        })
    }

    pub(crate) async fn preimage_trade_fee_required_to_send_outputs(
        &self,
        contract_outputs: Vec<ContractCallOutput>,
        stage: &FeeApproxStage,
    ) -> TradePreimageResult<BigDecimal> {
        let decimals = self.as_ref().decimals;
        let mut gas_fee = 0;
        let mut outputs = Vec::with_capacity(contract_outputs.len());
        for output in contract_outputs {
            gas_fee += output.gas_limit * output.gas_price;
            outputs.push(TransactionOutput::from(output));
        }
        let fee_policy = FeePolicy::SendExact;
        let miner_fee =
            UtxoCommonOps::preimage_trade_fee_required_to_send_outputs(self, outputs, fee_policy, Some(gas_fee), stage)
                .await?;
        let gas_fee = big_decimal_from_sat(gas_fee as i64, decimals);
        Ok(miner_fee + gas_fee)
    }
}

// if mockable is placed before async_trait there is `munmap_chunk(): invalid pointer` error on async fn mocking attempt
#[async_trait]
#[cfg_attr(test, mockable)]
impl UtxoTxBroadcastOps for Qrc20Coin {
    async fn broadcast_tx(&self, tx: &UtxoTx) -> Result<H256Json, MmError<BroadcastTxErr>> {
        utxo_common::broadcast_tx(self, tx).await
    }
}

#[async_trait]
#[cfg_attr(test, mockable)]
impl UtxoTxGenerationOps for Qrc20Coin {
    /// Get only QTUM transaction fee.
    async fn get_tx_fee(&self) -> UtxoRpcResult<ActualTxFee> { utxo_common::get_tx_fee(&self.utxo).await }

    async fn calc_interest_if_required(
        &self,
        unsigned: TransactionInputSigner,
        data: AdditionalTxData,
        my_script_pub: ScriptBytes,
    ) -> UtxoRpcResult<(TransactionInputSigner, AdditionalTxData)> {
        utxo_common::calc_interest_if_required(self, unsigned, data, my_script_pub).await
    }
}

#[async_trait]
#[cfg_attr(test, mockable)]
impl GetUtxoListOps for Qrc20Coin {
    async fn get_unspent_ordered_list(
        &self,
        address: &Address,
    ) -> UtxoRpcResult<(Vec<UnspentInfo>, RecentlySpentOutPointsGuard<'_>)> {
        utxo_common::get_unspent_ordered_list(self, address).await
    }

    async fn get_all_unspent_ordered_list(
        &self,
        address: &Address,
    ) -> UtxoRpcResult<(Vec<UnspentInfo>, RecentlySpentOutPointsGuard<'_>)> {
        utxo_common::get_all_unspent_ordered_list(self, address).await
    }

    async fn get_mature_unspent_ordered_list(
        &self,
        address: &Address,
    ) -> UtxoRpcResult<(MatureUnspentList, RecentlySpentOutPointsGuard<'_>)> {
        utxo_common::get_mature_unspent_ordered_list(self, address).await
    }
}

#[async_trait]
#[cfg_attr(test, mockable)]
impl UtxoCommonOps for Qrc20Coin {
    async fn get_htlc_spend_fee(&self, tx_size: u64) -> UtxoRpcResult<u64> {
        utxo_common::get_htlc_spend_fee(self, tx_size).await
    }

    fn addresses_from_script(&self, script: &Script) -> Result<Vec<UtxoAddress>, String> {
        utxo_common::addresses_from_script(self, script)
    }

    fn denominate_satoshis(&self, satoshi: i64) -> f64 { utxo_common::denominate_satoshis(&self.utxo, satoshi) }

    fn my_public_key(&self) -> Result<&Public, MmError<UnexpectedDerivationMethod>> {
        utxo_common::my_public_key(self.as_ref())
    }

    fn address_from_str(&self, address: &str) -> Result<UtxoAddress, String> {
        utxo_common::checked_address_from_str(self, address)
    }

    async fn get_current_mtp(&self) -> UtxoRpcResult<u32> {
        utxo_common::get_current_mtp(&self.utxo, CoinVariant::Qtum).await
    }

    fn is_unspent_mature(&self, output: &RpcTransaction) -> bool { self.is_qtum_unspent_mature(output) }

    async fn calc_interest_of_tx(
        &self,
        _tx: &UtxoTx,
        _input_transactions: &mut HistoryUtxoTxMap,
    ) -> UtxoRpcResult<u64> {
        MmError::err(UtxoRpcError::Internal(
            "QRC20 coin doesn't support transaction rewards".to_owned(),
        ))
    }

    async fn get_mut_verbose_transaction_from_map_or_rpc<'a, 'b>(
        &'a self,
        tx_hash: H256Json,
        utxo_tx_map: &'b mut HistoryUtxoTxMap,
    ) -> UtxoRpcResult<&'b mut HistoryUtxoTx> {
        utxo_common::get_mut_verbose_transaction_from_map_or_rpc(self, tx_hash, utxo_tx_map).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn p2sh_spending_tx(
        &self,
        prev_transaction: UtxoTx,
        redeem_script: ScriptBytes,
        outputs: Vec<TransactionOutput>,
        script_data: Script,
        sequence: u32,
        lock_time: u32,
        keypair: &KeyPair,
    ) -> Result<UtxoTx, String> {
        utxo_common::p2sh_spending_tx(
            self,
            prev_transaction,
            redeem_script,
            outputs,
            script_data,
            sequence,
            lock_time,
            keypair,
        )
        .await
    }

    fn get_verbose_transactions_from_cache_or_rpc(
        &self,
        tx_ids: HashSet<H256Json>,
    ) -> UtxoRpcFut<HashMap<H256Json, VerboseTransactionFrom>> {
        let selfi = self.clone();
        let fut = async move { utxo_common::get_verbose_transactions_from_cache_or_rpc(&selfi.utxo, tx_ids).await };
        Box::new(fut.boxed().compat())
    }

    async fn preimage_trade_fee_required_to_send_outputs(
        &self,
        outputs: Vec<TransactionOutput>,
        fee_policy: FeePolicy,
        gas_fee: Option<u64>,
        stage: &FeeApproxStage,
    ) -> TradePreimageResult<BigDecimal> {
        utxo_common::preimage_trade_fee_required_to_send_outputs(self, outputs, fee_policy, gas_fee, stage).await
    }

    fn increase_dynamic_fee_by_stage(&self, dynamic_fee: u64, stage: &FeeApproxStage) -> u64 {
        utxo_common::increase_dynamic_fee_by_stage(self, dynamic_fee, stage)
    }

    async fn p2sh_tx_locktime(&self, htlc_locktime: u32) -> Result<u32, MmError<UtxoRpcError>> {
        utxo_common::p2sh_tx_locktime(self, &self.utxo.conf.ticker, htlc_locktime).await
    }

    fn addr_format(&self) -> &UtxoAddressFormat { utxo_common::addr_format(self) }

    fn addr_format_for_standard_scripts(&self) -> UtxoAddressFormat {
        utxo_common::addr_format_for_standard_scripts(self)
    }

    fn address_from_pubkey(&self, pubkey: &Public) -> Address {
        let conf = &self.utxo.conf;
        utxo_common::address_from_pubkey(
            pubkey,
            conf.pub_addr_prefix,
            conf.pub_t_addr_prefix,
            conf.checksum_type,
            conf.bech32_hrp.clone(),
            self.addr_format().clone(),
        )
    }
}
