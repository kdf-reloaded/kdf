// utxo_common_tx — transaction building, signing, fee estimation, UTXO management

use super::*;
use utxo_signer::with_key_pair::sign_tx;

pub const DEFAULT_FEE_VOUT: usize = 0;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UtxoMergeParams {
    merge_at: usize,
    #[serde(default = "ten_f64")]
    check_every: f64,
    #[serde(default = "one_hundred")]
    max_merge_at_once: usize,
}

pub async fn get_tx_fee(coin: &UtxoCoinFields) -> UtxoRpcResult<ActualTxFee> {
    let conf = &coin.conf;
    match &coin.tx_fee {
        TxFee::Dynamic(method) => {
            let fee = coin
                .rpc_client
                .estimate_fee_sat(coin.decimals, method, &conf.estimate_fee_mode, conf.estimate_fee_blocks)
                .compat()
                .await?;
            Ok(ActualTxFee::Dynamic(fee))
        },
        TxFee::FixedPerKb(satoshis) => Ok(ActualTxFee::FixedPerKb(*satoshis)),
    }
}

pub fn send_outputs_from_my_address<T>(coin: T, outputs: Vec<TransactionOutput>) -> TransactionFut
where
    T: UtxoCommonOps + GetUtxoListOps,
{
    let fut = send_outputs_from_my_address_impl(coin, outputs);
    Box::new(fut.boxed().compat().map(|tx| tx.into()))
}

/// Sends outputs while allowing one protocol-defined output to bypass the
/// generic spendable-output dust check.
///
/// This is intentionally crate-private and is used only by the legacy KMD
/// direct-burn taker-fee path. Change handling still uses the coin's dust.
pub(crate) fn send_outputs_from_my_address_with_underdust_output<T>(
    coin: T,
    outputs: Vec<TransactionOutput>,
    output_index: usize,
) -> TransactionFut
where
    T: UtxoCommonOps + GetUtxoListOps,
{
    let fut = send_outputs_from_my_address_impl_with_underdust_output(coin, outputs, Some(output_index));
    Box::new(fut.boxed().compat().map(|tx| tx.into()))
}

pub fn tx_size_in_v_bytes(from_addr_format: &UtxoAddressFormat, tx: &UtxoTx) -> usize {
    let transaction_bytes = serialize(tx);
    // 2 bytes are used to indicate the length of signature and pubkey
    // total is 107
    let additional_len = 2 + MAX_DER_SIGNATURE_LEN + COMPRESSED_PUBKEY_LEN;
    // Virtual size of the transaction
    // https://bitcoin.stackexchange.com/questions/87275/how-to-calculate-segwit-transaction-fee-in-bytes/87276#87276
    match from_addr_format {
        UtxoAddressFormat::Segwit => {
            let base_size = transaction_bytes.len();
            // 4 additional bytes (2 for the marker and 2 for the flag) and 1 additional byte for every input in the witness for the SIGHASH flag
            let total_size = transaction_bytes.len() + 4 + tx.inputs().len() * (additional_len + 1);
            ((0.75 * base_size as f64) + (0.25 * total_size as f64)) as usize
        },
        _ => transaction_bytes.len() + tx.inputs().len() * additional_len,
    }
}

pub(crate) async fn trade_preimage_sender_address(coin: &UtxoCoinFields) -> TradePreimageResult<Address> {
    match &coin.derivation_method {
        DerivationMethod::Iguana(my_address) => Ok(my_address.clone()),
        DerivationMethod::HDWallet(hd_wallet) => crate::utxo::utxo_standard_swap_v2::enabled_hd_address_info(
            coin,
            hd_wallet,
            "UTXO trade preimage sender address selection",
        )
        .await
        .map(|info| info.address)
        .map_to_mm(TradePreimageError::InternalError),
    }
}

pub struct UtxoTxBuilder<'a, T: AsRef<UtxoCoinFields> + UtxoTxGenerationOps> {
    coin: &'a T,
    from: Option<Address>,
    /// The available inputs that *can* be included in the resulting tx
    available_inputs: Vec<UnspentInfo>,
    fee_policy: FeePolicy,
    fee: Option<ActualTxFee>,
    gas_fee: Option<u64>,
    tx: TransactionInputSigner,
    change: u64,
    sum_inputs: u64,
    sum_outputs_value: u64,
    tx_fee: u64,
    min_relay_fee: Option<u64>,
    dust: Option<u64>,
    allowed_underdust_output: Option<usize>,
}

impl<'a, T: AsRef<UtxoCoinFields> + UtxoTxGenerationOps> UtxoTxBuilder<'a, T> {
    pub fn new(coin: &'a T) -> Self {
        UtxoTxBuilder {
            tx: coin.as_ref().transaction_preimage(),
            coin,
            from: coin.as_ref().derivation_method.iguana().cloned(),
            available_inputs: vec![],
            fee_policy: FeePolicy::SendExact,
            fee: None,
            gas_fee: None,
            change: 0,
            sum_inputs: 0,
            sum_outputs_value: 0,
            tx_fee: 0,
            min_relay_fee: None,
            dust: None,
            allowed_underdust_output: None,
        }
    }

    pub fn with_from_address(mut self, from: Address) -> Self {
        self.from = Some(from);
        self
    }

    pub fn with_dust(mut self, dust_amount: u64) -> Self {
        self.dust = Some(dust_amount);
        self
    }

    /// Permits one protocol-defined output to be positive but below the
    /// generic spendable-output dust threshold.
    ///
    /// This does not change the dust threshold used for any other output or
    /// for change construction.
    pub(crate) fn allow_underdust_output(mut self, output_index: usize) -> Self {
        self.allowed_underdust_output = Some(output_index);
        self
    }

    pub fn add_required_inputs(mut self, inputs: impl IntoIterator<Item = UnspentInfo>) -> Self {
        self.tx
            .inputs
            .extend(inputs.into_iter().map(|input| UnsignedTransactionInput {
                previous_output: input.outpoint,
                sequence: SEQUENCE_FINAL,
                amount: input.value,
                witness: Vec::new(),
            }));
        self
    }

    /// This function expects that utxos are sorted by amounts in ascending order
    /// Consider sorting before calling this function
    pub fn add_available_inputs(mut self, inputs: impl IntoIterator<Item = UnspentInfo>) -> Self {
        self.available_inputs.extend(inputs);
        self
    }

    pub fn add_outputs(mut self, outputs: impl IntoIterator<Item = TransactionOutput>) -> Self {
        self.tx.outputs.extend(outputs);
        self
    }

    pub fn with_fee_policy(mut self, new_policy: FeePolicy) -> Self {
        self.fee_policy = new_policy;
        self
    }

    pub fn with_fee(mut self, fee: ActualTxFee) -> Self {
        self.fee = Some(fee);
        self
    }

    /// Note `gas_fee` should be enough to execute all of the contract calls within UTXO outputs.
    /// QRC20 specific: `gas_fee` should be calculated by: gas_limit * gas_price * (count of contract calls),
    /// or should be sum of gas fee of all contract calls.
    pub fn with_gas_fee(mut self, gas_fee: u64) -> Self {
        self.gas_fee = Some(gas_fee);
        self
    }

    /// Recalculates fee and checks whether transaction is complete (inputs collected cover the outputs)
    fn update_fee_and_check_completeness(
        &mut self,
        from_addr_format: &UtxoAddressFormat,
        actual_tx_fee: &ActualTxFee,
    ) -> bool {
        self.tx_fee = match &actual_tx_fee {
            ActualTxFee::Dynamic(f) => {
                let transaction = UtxoTx::from(self.tx.clone());
                let v_size = tx_size_in_v_bytes(from_addr_format, &transaction);
                (f * v_size as u64) / KILO_BYTE
            },
            ActualTxFee::FixedPerKb(f) => {
                let transaction = UtxoTx::from(self.tx.clone());
                let v_size = tx_size_in_v_bytes(from_addr_format, &transaction) as u64;
                let v_size_kb = if v_size % KILO_BYTE == 0 {
                    v_size / KILO_BYTE
                } else {
                    v_size / KILO_BYTE + 1
                };
                f * v_size_kb
            },
        };

        match self.fee_policy {
            FeePolicy::SendExact => {
                let mut outputs_plus_fee = self.sum_outputs_value + self.tx_fee;
                if self.sum_inputs >= outputs_plus_fee {
                    self.change = self.sum_inputs - outputs_plus_fee;
                    if self.change > self.dust() {
                        // there will be change output
                        if let ActualTxFee::Dynamic(ref f) = actual_tx_fee {
                            self.tx_fee += (f * P2PKH_OUTPUT_LEN) / KILO_BYTE;
                            outputs_plus_fee += (f * P2PKH_OUTPUT_LEN) / KILO_BYTE;
                        }
                    }
                    if let Some(min_relay) = self.min_relay_fee {
                        if self.tx_fee < min_relay {
                            outputs_plus_fee -= self.tx_fee;
                            outputs_plus_fee += min_relay;
                            self.tx_fee = min_relay;
                        }
                    }
                    self.sum_inputs >= outputs_plus_fee
                } else {
                    false
                }
            },
            FeePolicy::DeductFromOutput(_) => {
                if self.sum_inputs >= self.sum_outputs_value {
                    self.change = self.sum_inputs - self.sum_outputs_value;
                    if self.change > self.dust() {
                        if let ActualTxFee::Dynamic(ref f) = actual_tx_fee {
                            self.tx_fee += (f * P2PKH_OUTPUT_LEN) / KILO_BYTE;
                        }
                    }
                    if let Some(min_relay) = self.min_relay_fee {
                        if self.tx_fee < min_relay {
                            self.tx_fee = min_relay;
                        }
                    }
                    true
                } else {
                    false
                }
            },
        }
    }

    fn dust(&self) -> u64 {
        match self.dust {
            Some(dust) => dust,
            None => self.coin.as_ref().dust_amount,
        }
    }

    /// Generates unsigned transaction (TransactionInputSigner) from specified utxos and outputs.
    /// Sends the change (inputs amount - outputs amount) to the [`UtxoTxBuilder::from`] address.
    /// Also returns additional transaction data
    pub async fn build(mut self) -> GenerateTxResult {
        let coin = self.coin;
        let dust: u64 = self.dust();
        let from = self
            .from
            .clone()
            .or_mm_err(|| GenerateTxError::Internal("'from' address is not specified".to_owned()))?;
        let change_script_pubkey = output_script(&from, ScriptType::P2PKH).to_bytes();

        let actual_tx_fee = match self.fee {
            Some(fee) => fee,
            None => coin.get_tx_fee().await.mm_err(Into::into)?,
        };

        true_or!(!self.tx.outputs.is_empty(), GenerateTxError::EmptyOutputs);

        let mut received_by_me = 0;
        for (output_index, output) in self.tx.outputs.iter().enumerate() {
            let script: Script = output.script_pubkey.clone().into();
            let is_allowed_underdust_output = self.allowed_underdust_output == Some(output_index);
            if script.opcodes().next() != Some(Ok(Opcode::OP_RETURN)) {
                true_or!(
                    output.value >= dust || (is_allowed_underdust_output && output.value > 0),
                    GenerateTxError::OutputValueLessThanDust {
                        value: output.value,
                        dust
                    }
                );
            }
            self.sum_outputs_value += output.value;
            if output.script_pubkey == change_script_pubkey {
                received_by_me += output.value;
            }
        }

        if let Some(gas_fee) = self.gas_fee {
            self.sum_outputs_value += gas_fee;
        }

        true_or!(
            !self.available_inputs.is_empty() || !self.tx.inputs.is_empty(),
            GenerateTxError::EmptyUtxoSet {
                required: self.sum_outputs_value
            }
        );

        self.min_relay_fee = if coin.as_ref().conf.force_min_relay_fee {
            let fee_dec = coin.as_ref().rpc_client.get_relay_fee().compat().await?;
            let min_relay_fee = sat_from_big_decimal(&fee_dec, coin.as_ref().decimals).mm_err(Into::into)?;
            Some(min_relay_fee)
        } else {
            None
        };

        for utxo in self.available_inputs.clone() {
            self.tx.inputs.push(UnsignedTransactionInput {
                previous_output: utxo.outpoint,
                sequence: SEQUENCE_FINAL,
                amount: utxo.value,
                witness: vec![],
            });
            self.sum_inputs += utxo.value;

            if self.update_fee_and_check_completeness(&from.addr_format, &actual_tx_fee) {
                break;
            }
        }

        match self.fee_policy {
            FeePolicy::SendExact => self.sum_outputs_value += self.tx_fee,
            FeePolicy::DeductFromOutput(i) => {
                let min_output = self.tx_fee + dust;
                let val = self.tx.outputs[i].value;
                true_or!(val >= min_output, GenerateTxError::DeductFeeFromOutputFailed {
                    output_idx: i,
                    output_value: val,
                    required: min_output,
                });
                self.tx.outputs[i].value -= self.tx_fee;
                if self.tx.outputs[i].script_pubkey == change_script_pubkey {
                    received_by_me -= self.tx_fee;
                }
            },
        };
        true_or!(
            self.sum_inputs >= self.sum_outputs_value,
            GenerateTxError::NotEnoughUtxos {
                sum_utxos: self.sum_inputs,
                required: self.sum_outputs_value
            }
        );

        let change = self.sum_inputs - self.sum_outputs_value;
        let unused_change = if change > dust {
            self.tx.outputs.push({
                TransactionOutput {
                    value: change,
                    script_pubkey: change_script_pubkey.clone(),
                }
            });
            received_by_me += change;
            None
        } else if change > 0 {
            Some(change)
        } else {
            None
        };

        let data = AdditionalTxData {
            fee_amount: self.tx_fee,
            received_by_me,
            spent_by_me: self.sum_inputs,
            unused_change,
            // will be changed if the ticker is KMD
            kmd_rewards: None,
        };

        Ok(coin
            .calc_interest_if_required(self.tx, data, change_script_pubkey)
            .await
            .mm_err(Into::into)?)
    }
}

/// Calculates interest if the coin is KMD
/// Adds the value to existing output to my_script_pub or creates additional interest output
/// returns transaction and data as is if the coin is not KMD
pub async fn calc_interest_if_required<T: UtxoCommonOps>(
    coin: &T,
    mut unsigned: TransactionInputSigner,
    mut data: AdditionalTxData,
    my_script_pub: Bytes,
) -> UtxoRpcResult<(TransactionInputSigner, AdditionalTxData)> {
    if coin.as_ref().conf.ticker != "KMD" {
        return Ok((unsigned, data));
    }
    unsigned.lock_time = coin.get_current_mtp().await?;
    let mut interest = 0;
    for input in unsigned.inputs.iter() {
        let prev_hash = input.previous_output.hash.reversed().into();
        let tx = coin
            .as_ref()
            .rpc_client
            .get_verbose_transaction(&prev_hash)
            .compat()
            .await?;
        if let Ok(output_interest) =
            kmd_interest(tx.height, input.amount, tx.locktime as u64, unsigned.lock_time as u64)
        {
            interest += output_interest;
        };
    }
    if interest > 0 {
        data.received_by_me += interest;
        let mut output_to_me = unsigned
            .outputs
            .iter_mut()
            .find(|out| out.script_pubkey == my_script_pub);
        // add calculated interest to existing output to my address
        // or create the new one if it's not found
        match output_to_me {
            Some(ref mut output) => output.value += interest,
            None => {
                let interest_output = TransactionOutput {
                    script_pubkey: my_script_pub,
                    value: interest,
                };
                unsigned.outputs.push(interest_output);
            },
        };
    } else {
        // if interest is zero attempt to set the lowest possible lock_time to claim it later
        unsigned.lock_time = (now_ms() / 1000) as u32 - 3600 + 777 * 2;
    }
    let rewards_amount = big_decimal_from_sat_unsigned(interest, coin.as_ref().decimals);
    data.kmd_rewards = Some(KmdRewardsDetails::claimed_by_me(rewards_amount));
    Ok((unsigned, data))
}

#[allow(clippy::too_many_arguments)]
pub async fn p2sh_spending_tx<T: UtxoCommonOps>(
    coin: &T,
    prev_transaction: UtxoTx,
    redeem_script: Bytes,
    outputs: Vec<TransactionOutput>,
    script_data: Script,
    sequence: u32,
    lock_time: u32,
    keypair: &KeyPair,
) -> Result<UtxoTx, String> {
    let lock_time = try_s!(coin.p2sh_tx_locktime(lock_time).await);
    let n_time = if coin.as_ref().conf.is_pos {
        Some((now_ms() / 1000) as u32)
    } else {
        None
    };
    let str_d_zeel = if coin.as_ref().conf.ticker == "NAV" {
        Some("".into())
    } else {
        None
    };
    let hash_algo = coin.as_ref().tx_hash_algo.into();
    let unsigned = TransactionInputSigner {
        lock_time,
        version: coin.as_ref().conf.tx_version,
        n_time,
        overwintered: coin.as_ref().conf.overwintered,
        inputs: vec![UnsignedTransactionInput {
            sequence,
            previous_output: OutPoint {
                hash: prev_transaction.hash(),
                index: DEFAULT_SWAP_VOUT as u32,
            },
            amount: prev_transaction.outputs[0].value,
            witness: Vec::new(),
        }],
        outputs: outputs.clone(),
        expiry_height: 0,
        join_splits: vec![],
        shielded_spends: vec![],
        shielded_outputs: vec![],
        value_balance: 0,
        version_group_id: coin.as_ref().conf.version_group_id,
        consensus_branch_id: coin.as_ref().conf.consensus_branch_id,
        zcash: coin.as_ref().conf.zcash,
        str_d_zeel,
        hash_algo,
    };
    let signed_input = try_s!(p2sh_spend(
        &unsigned,
        DEFAULT_SWAP_VOUT,
        keypair,
        script_data,
        redeem_script.into(),
        coin.as_ref().conf.signature_version,
        coin.as_ref().conf.fork_id
    ));
    Ok(UtxoTx {
        version: unsigned.version,
        n_time: unsigned.n_time,
        overwintered: unsigned.overwintered,
        lock_time: unsigned.lock_time,
        inputs: vec![signed_input],
        outputs,
        expiry_height: unsigned.expiry_height,
        join_splits: vec![],
        shielded_spends: vec![],
        shielded_outputs: vec![],
        value_balance: 0,
        version_group_id: coin.as_ref().conf.version_group_id,
        binding_sig: H512::default(),
        join_split_sig: H512::default(),
        join_split_pubkey: H256::default(),
        zcash: coin.as_ref().conf.zcash,
        str_d_zeel: unsigned.str_d_zeel,
        tx_hash_algo: unsigned.hash_algo.into(),
    })
}

/// Takes raw transaction as input and returns tx hash in hexadecimal format
pub fn send_raw_tx(coin: &UtxoCoinFields, tx: &str) -> Box<dyn Future<Item = String, Error = String> + Send> {
    let bytes = try_fus!(hex::decode(tx));
    Box::new(
        coin.rpc_client
            .send_raw_transaction(bytes.into())
            .map_err(|e| ERRL!("{}", e))
            .map(|hash| format!("{:?}", hash)),
    )
}

/// Takes raw transaction bytes as input and returns tx hash in hexadecimal format
pub fn send_raw_tx_bytes(
    coin: &UtxoCoinFields,
    tx_bytes: &[u8],
) -> Box<dyn Future<Item = String, Error = String> + Send> {
    Box::new(
        coin.rpc_client
            .send_raw_transaction(tx_bytes.into())
            .map_err(|e| ERRL!("{}", e))
            .map(|hash| format!("{:?}", hash)),
    )
}

pub fn tx_enum_from_bytes(coin: &UtxoCoinFields, bytes: &[u8]) -> Result<TransactionEnum, String> {
    let mut transaction: UtxoTx = try_s!(deserialize(bytes).map_err(|err| format!("{:?}", err)));
    transaction.tx_hash_algo = coin.tx_hash_algo;
    Ok(transaction.into())
}

pub fn get_trade_fee<T: UtxoCommonOps>(coin: T) -> Box<dyn Future<Item = TradeFee, Error = String> + Send> {
    let ticker = coin.as_ref().conf.ticker.clone();
    let decimals = coin.as_ref().decimals;
    let fut = async move {
        let fee = try_s!(coin.get_tx_fee().await);
        let amount = match fee {
            ActualTxFee::Dynamic(f) => f,
            ActualTxFee::FixedPerKb(f) => f,
        };
        Ok(TradeFee {
            coin: ticker,
            amount: big_decimal_from_sat(amount as i64, decimals).into(),
            paid_from_trading_vol: false,
        })
    };
    Box::new(fut.boxed().compat())
}

/// To ensure the `get_sender_trade_fee(x) <= get_sender_trade_fee(y)` condition is satisfied for any `x < y`,
/// we should include a `change` output into the result fee. Imagine this case:
/// Let `sum_inputs = 11000` and `total_tx_fee: { 200, if there is no the change output; 230, if there is the change output }`.
///
/// If `value = TradePreimageValue::Exact(10000)`, therefore `sum_outputs = 10000`.
/// then `change = sum_inputs - sum_outputs - total_tx_fee = 800`, so `change < dust` and `total_tx_fee = 200` (including the change output).
///
/// But if `value = TradePreimageValue::Exact(9000)`, therefore `sum_outputs = 9000`. Let `sum_inputs = 11000`, `total_tx_fee = 230`
/// where `change = sum_inputs - sum_outputs - total_tx_fee = 1770`, so `change > dust` and `total_tx_fee = 230` (including the change output).
///
/// To sum up, `get_sender_trade_fee(TradePreimageValue::Exact(9000)) > get_sender_trade_fee(TradePreimageValue::Exact(10000))`.
/// So we should always return a fee as if a transaction includes the change output.
pub async fn preimage_trade_fee_required_to_send_outputs<T>(
    coin: &T,
    outputs: Vec<TransactionOutput>,
    fee_policy: FeePolicy,
    gas_fee: Option<u64>,
    stage: &FeeApproxStage,
) -> TradePreimageResult<BigDecimal>
where
    T: UtxoCommonOps + GetUtxoListOps,
{
    let ticker = coin.as_ref().conf.ticker.clone();
    let decimals = coin.as_ref().decimals;
    let tx_fee = coin.get_tx_fee().await.mm_err(Into::into)?;
    // [`FeePolicy::DeductFromOutput`] is used if the value is [`TradePreimageValue::UpperBound`] only
    let is_amount_upper_bound = matches!(fee_policy, FeePolicy::DeductFromOutput(_));
    let my_address = trade_preimage_sender_address(coin.as_ref()).await?;

    match tx_fee {
        // if it's a dynamic fee, we should generate a swap transaction to get an actual trade fee
        ActualTxFee::Dynamic(fee) => {
            // take into account that the dynamic tx fee may increase during the swap
            let dynamic_fee = coin.increase_dynamic_fee_by_stage(fee, stage);

            let outputs_count = outputs.len();
            let (unspents, _recently_sent_txs) = coin.get_unspent_ordered_list(&my_address).await.mm_err(Into::into)?;

            let actual_tx_fee = ActualTxFee::Dynamic(dynamic_fee);

            let mut tx_builder = UtxoTxBuilder::new(coin)
                .with_from_address(my_address.clone())
                .add_available_inputs(unspents)
                .add_outputs(outputs)
                .with_fee_policy(fee_policy)
                .with_fee(actual_tx_fee);
            if let Some(gas) = gas_fee {
                tx_builder = tx_builder.with_gas_fee(gas);
            }
            let (tx, _data) = tx_builder
                .build()
                .await
                .mm_err(|e| TradePreimageError::from_generate_tx_error(e, ticker, decimals, is_amount_upper_bound))?;

            // The estimate must be identical whether the amount is expressed as a
            // `TradePreimageValue::UpperBound` or the equivalent `Exact` value (the
            // max-taker-volume fixed point). The builder folds a change-output allowance
            // into its fee inconsistently across fee policies and rounding boundaries: in
            // the `SendExact` path the allowance can consume the change below dust so that
            // no change output is materialised, yet the allowance stays in the fee — and
            // the old `tx.outputs.len() == outputs_count` test then added a *second*
            // allowance, over-counting by one P2PKH output for some fee rates.
            //
            // Recompute here from the built transaction size, always including exactly one
            // P2PKH change output (a real swap tx carries one) with a single rounding step.
            // This is an estimate used for display and max-volume math only; it does not
            // build the broadcast transaction.
            let tx = UtxoTx::from(tx);
            let mut v_size = tx_size_in_v_bytes(&my_address.addr_format, &tx) as u64;
            if tx.outputs.len() != outputs_count {
                // a change output was materialised by the builder; drop it so the base
                // size is change-free before adding the single allowance below
                v_size = v_size.saturating_sub(P2PKH_OUTPUT_LEN);
            }
            let total_fee = (dynamic_fee * (v_size + P2PKH_OUTPUT_LEN)) / KILO_BYTE;

            Ok(big_decimal_from_sat(total_fee as i64, decimals))
        },
        ActualTxFee::FixedPerKb(fee) => {
            let outputs_count = outputs.len();
            let (unspents, _recently_sent_txs) = coin.get_unspent_ordered_list(&my_address).await.mm_err(Into::into)?;

            let mut tx_builder = UtxoTxBuilder::new(coin)
                .with_from_address(my_address.clone())
                .add_available_inputs(unspents)
                .add_outputs(outputs)
                .with_fee_policy(fee_policy)
                .with_fee(tx_fee);
            if let Some(gas) = gas_fee {
                tx_builder = tx_builder.with_gas_fee(gas);
            }
            let (tx, data) = tx_builder
                .build()
                .await
                .mm_err(|e| TradePreimageError::from_generate_tx_error(e, ticker, decimals, is_amount_upper_bound))?;

            let total_fee = if tx.outputs.len() == outputs_count {
                // take into account the change output if tx_size_kb(tx with change) > tx_size_kb(tx without change)
                let tx = UtxoTx::from(tx);
                let tx_bytes = serialize(&tx);
                if tx_bytes.len() as u64 % KILO_BYTE + P2PKH_OUTPUT_LEN > KILO_BYTE {
                    data.fee_amount + fee
                } else {
                    data.fee_amount
                }
            } else {
                // the change output is included already
                data.fee_amount
            };

            Ok(big_decimal_from_sat(total_fee as i64, decimals))
        },
    }
}

/// Maker or Taker should pay fee only for sending his payment.
/// Even if refund will be required the fee will be deducted from P2SH input.
/// Please note the `get_sender_trade_fee` satisfies the following condition:
/// `get_sender_trade_fee(x) <= get_sender_trade_fee(y)` for any `x < y`.
pub async fn get_sender_trade_fee<T>(
    coin: &T,
    value: TradePreimageValue,
    stage: FeeApproxStage,
) -> TradePreimageResult<TradeFee>
where
    T: MarketCoinOps + UtxoCommonOps,
{
    let (amount, fee_policy) = match value {
        TradePreimageValue::UpperBound(upper_bound) => (upper_bound, FeePolicy::DeductFromOutput(0)),
        TradePreimageValue::Exact(amount) => (amount, FeePolicy::SendExact),
    };

    // pass the dummy params
    let time_lock = (now_ms() / 1000) as u32;
    let my_pub = &[0; 33]; // H264 is 33 bytes
    let other_pub = &[0; 33]; // H264 is 33 bytes
    let secret_hash = &[0; 20]; // H160 is 20 bytes

    // `generate_swap_payment_outputs` may fail due to either invalid `other_pub` or a number conversation error
    let SwapPaymentOutputsResult { outputs, .. } =
        generate_swap_payment_outputs(&coin, time_lock, my_pub, other_pub, secret_hash, amount)
            .map_to_mm(TradePreimageError::InternalError)?;
    let gas_fee = None;
    let fee_amount = coin
        .preimage_trade_fee_required_to_send_outputs(outputs, fee_policy, gas_fee, &stage)
        .await?;
    Ok(TradeFee {
        coin: coin.as_ref().conf.ticker.clone(),
        amount: fee_amount.into(),
        paid_from_trading_vol: false,
    })
}

/// The fee to spend (receive) other payment is deducted from the trading amount so we should display it
pub fn get_receiver_trade_fee<T: UtxoCommonOps>(coin: T) -> TradePreimageFut<TradeFee> {
    let fut = async move {
        let amount_sat = get_htlc_spend_fee(&coin, DEFAULT_SWAP_TX_SPEND_SIZE)
            .await
            .mm_err(Into::into)?;
        let amount = big_decimal_from_sat_unsigned(amount_sat, coin.as_ref().decimals).into();
        Ok(TradeFee {
            coin: coin.as_ref().conf.ticker.clone(),
            amount,
            paid_from_trading_vol: true,
        })
    };
    Box::new(fut.boxed().compat())
}

pub async fn get_fee_to_send_taker_fee<T>(
    coin: &T,
    dex_fee_amount: BigDecimal,
    stage: FeeApproxStage,
) -> TradePreimageResult<TradeFee>
where
    T: MarketCoinOps + UtxoCommonOps,
{
    let decimals = coin.as_ref().decimals;
    let value = sat_from_big_decimal(&dex_fee_amount, decimals).mm_err(Into::into)?;
    let output = TransactionOutput {
        value,
        script_pubkey: Builder::build_p2pkh(&AddressHashEnum::default_address_hash()).to_bytes(),
    };
    let gas_fee = None;
    let fee_amount = coin
        .preimage_trade_fee_required_to_send_outputs(vec![output], FeePolicy::SendExact, gas_fee, &stage)
        .await?;
    Ok(TradeFee {
        coin: coin.ticker().to_owned(),
        amount: fee_amount.into(),
        paid_from_trading_vol: false,
    })
}

/// [`GetUtxoListOps::get_mature_unspent_ordered_list`] implementation.
/// Returns available mature and immature unspents in ascending order
/// + `RecentlySpentOutPoints` MutexGuard for further interaction (e.g. to add new transaction to it).
pub async fn get_mature_unspent_ordered_list<'a, T>(
    coin: &'a T,
    address: &Address,
) -> UtxoRpcResult<(MatureUnspentList, RecentlySpentOutPointsGuard<'a>)>
where
    T: UtxoCommonOps + GetUtxoListOps,
{
    let (unspents, recently_spent) = coin.get_all_unspent_ordered_list(address).await?;
    let mature_unspents = identify_mature_unspents(coin, unspents).await?;
    Ok((mature_unspents, recently_spent))
}

/// [`GetUtxoMapOps::get_mature_unspent_ordered_map`] implementation.
/// Returns available mature and immature unspents in ascending order for every given `addresses`
/// + `RecentlySpentOutPoints` MutexGuard for further interaction (e.g. to add new transaction to it).
pub async fn get_mature_unspent_ordered_map<T>(
    coin: &T,
    addresses: Vec<Address>,
) -> UtxoRpcResult<(MatureUnspentMap, RecentlySpentOutPointsGuard<'_>)>
where
    T: UtxoCommonOps + GetUtxoMapOps,
{
    let (unspents_map, recently_spent) = coin.get_all_unspent_ordered_map(addresses).await?;
    // Get an iterator of futures: `Future<Output=UtxoRpcResult<(Address, MatureUnspentList)>>`
    let fut_it = unspents_map.into_iter().map(|(address, unspents)| {
        identify_mature_unspents(coin, unspents).map(|res| -> UtxoRpcResult<(Address, MatureUnspentList)> {
            let mature_unspents = res?;
            Ok((address, mature_unspents))
        })
    });
    // Poll the `fut_it` futures concurrently.
    let result_map = futures::future::try_join_all(fut_it).await?.into_iter().collect();
    Ok((result_map, recently_spent))
}

/// Splits the given `unspents` outputs into mature and immature.
pub async fn identify_mature_unspents<T>(coin: &T, unspents: Vec<UnspentInfo>) -> UtxoRpcResult<MatureUnspentList>
where
    T: UtxoCommonOps,
{
    /// Returns `true` if the given transaction has a known non-zero height.
    fn can_tx_be_cached(tx: &RpcTransaction) -> bool { tx.height > Some(0) }

    /// Calculates actual confirmations number of the given `tx` transaction loaded from cache.
    fn calc_actual_cached_tx_confirmations(tx: &RpcTransaction, block_count: u64) -> UtxoRpcResult<u32> {
        let tx_height = tx.height.or_mm_err(|| {
            UtxoRpcError::Internal(format!(r#"Warning, height of cached "{:?}" tx is unknown"#, tx.txid))
        })?;
        // There shouldn't be cached transactions with height == 0
        if tx_height == 0 {
            let error = format!(
                r#"Warning, height of cached "{:?}" tx is expected to be non-zero"#,
                tx.txid
            );
            return MmError::err(UtxoRpcError::Internal(error));
        }
        if block_count < tx_height {
            let error = format!(
                r#"Warning, actual block_count {} less than cached tx_height {} of {:?}"#,
                block_count, tx_height, tx.txid
            );
            return MmError::err(UtxoRpcError::Internal(error));
        }

        let confirmations = block_count - tx_height + 1;
        Ok(confirmations as u32)
    }

    let block_count = coin.as_ref().rpc_client.get_block_count().compat().await?;

    let to_verbose: HashSet<H256Json> = unspents
        .iter()
        .map(|unspent| unspent.outpoint.hash.reversed().into())
        .collect();
    let verbose_txs = coin
        .get_verbose_transactions_from_cache_or_rpc(to_verbose)
        .compat()
        .await?;
    // Transactions that should be cached.
    let mut txs_to_cache = HashMap::with_capacity(verbose_txs.len());

    let mut result = MatureUnspentList::with_capacity(unspents.len());
    for unspent in unspents {
        let tx_hash: H256Json = unspent.outpoint.hash.reversed().into();
        let tx_info = verbose_txs
            .get(&tx_hash)
            .or_mm_err(|| {
                UtxoRpcError::Internal(format!(
                    "'get_verbose_transactions_from_cache_or_rpc' should have returned '{:?}'",
                    tx_hash
                ))
            })?
            .clone();
        let tx_info = match tx_info {
            VerboseTransactionFrom::Cache(mut tx) => {
                if unspent.height.is_some() {
                    tx.height = unspent.height;
                }
                match calc_actual_cached_tx_confirmations(&tx, block_count) {
                    Ok(conf) => tx.confirmations = conf,
                    // do not skip the transaction with unknown confirmations,
                    // because the transaction can be matured
                    Err(e) => error!("{}", e),
                }
                tx
            },
            VerboseTransactionFrom::Rpc(mut tx) => {
                if tx.height.is_none() {
                    tx.height = unspent.height;
                }
                if can_tx_be_cached(&tx) {
                    txs_to_cache.insert(tx_hash, tx.clone());
                }
                tx
            },
        };

        if coin.is_unspent_mature(&tx_info) {
            result.mature.push(unspent);
        } else {
            result.immature.push(unspent);
        }
    }

    coin.as_ref()
        .tx_cache
        .cache_transactions_concurrently(&txs_to_cache)
        .await;
    Ok(result)
}

pub fn is_unspent_mature(mature_confirmations: u32, output: &RpcTransaction) -> bool {
    // don't skip outputs with confirmations == 0, because we can spend them
    !output.is_coinbase() || output.confirmations >= mature_confirmations
}

/// [`UtxoCommonOps::get_verbose_transactions_from_cache_or_rpc`] implementation.
/// Loads verbose transactions from cache or requests it using RPC client.
pub async fn get_verbose_transactions_from_cache_or_rpc(
    coin: &UtxoCoinFields,
    tx_ids: HashSet<H256Json>,
) -> UtxoRpcResult<HashMap<H256Json, VerboseTransactionFrom>> {
    /// Determines whether the transaction is needed to be requested through RPC or not.
    /// Puts the inner `RpcTransaction` transaction into `result_map` if it has been loaded successfully,
    /// otherwise puts `txid` into `to_request`.
    fn on_cached_transaction_result(
        result_map: &mut HashMap<H256Json, VerboseTransactionFrom>,
        to_request: &mut Vec<H256Json>,
        txid: H256Json,
        res: TxCacheResult<Option<RpcTransaction>>,
    ) {
        match res {
            Ok(Some(tx)) => {
                result_map.insert(txid, VerboseTransactionFrom::Cache(tx));
            },
            // txid not found
            Ok(None) => {
                to_request.push(txid);
            },
            Err(err) => {
                error!(
                    "Error loading the {:?} transaction: {:?}. Trying to request tx using RPC client",
                    err, txid
                );
                to_request.push(txid);
            },
        }
    }

    let mut result_map = HashMap::with_capacity(tx_ids.len());
    let mut to_request = Vec::with_capacity(tx_ids.len());

    coin.tx_cache
        .load_transactions_from_cache_concurrently(tx_ids)
        .await
        .into_iter()
        .for_each(|(txid, res)| on_cached_transaction_result(&mut result_map, &mut to_request, txid, res));

    result_map.extend(
        coin.rpc_client
            .get_verbose_transactions(&to_request)
            .compat()
            .await?
            .into_iter()
            .map(|tx| (tx.txid, VerboseTransactionFrom::Rpc(tx))),
    );
    Ok(result_map)
}

/// [`GetUtxoListOps::get_unspent_ordered_list`] implementation.
/// Returns available unspents in ascending order
/// + `RecentlySpentOutPoints` MutexGuard for further interaction (e.g. to add new transaction to it).
pub async fn get_unspent_ordered_list<'a, T>(
    coin: &'a T,
    address: &Address,
) -> UtxoRpcResult<(Vec<UnspentInfo>, RecentlySpentOutPointsGuard<'a>)>
where
    T: UtxoCommonOps + GetUtxoListOps,
{
    if coin.as_ref().check_utxo_maturity {
        coin.get_mature_unspent_ordered_list(address)
            .await
            // Convert `MatureUnspentList` into `Vec<UnspentInfo>` by discarding immature unspents.
            .map(|(mature_unspents, recently_spent)| (mature_unspents.only_mature(), recently_spent))
    } else {
        coin.get_all_unspent_ordered_list(address).await
    }
}

/// [`GetUtxoMapOps::get_unspent_ordered_map`] implementation.
/// Returns available unspents in ascending order + `RecentlySpentOutPoints` MutexGuard for further interaction
/// (e.g. to add new transaction to it).
pub async fn get_unspent_ordered_map<T>(
    coin: &T,
    addresses: Vec<Address>,
) -> UtxoRpcResult<(UnspentMap, RecentlySpentOutPointsGuard<'_>)>
where
    T: UtxoCommonOps + GetUtxoMapOps,
{
    if coin.as_ref().check_utxo_maturity {
        coin.get_mature_unspent_ordered_map(addresses)
            .await
            // Convert `MatureUnspentMap` into `UnspentMap` by discarding immature unspents.
            .map(|(mature_unspents_map, recently_spent)| {
                let unspents_map = mature_unspents_map
                    .into_iter()
                    .map(|(address, unspents)| (address, unspents.only_mature()))
                    .collect();
                (unspents_map, recently_spent)
            })
    } else {
        coin.get_all_unspent_ordered_map(addresses).await
    }
}

/// [`GetUtxoListOps::get_all_unspent_ordered_list`] implementation.
/// Returns available mature and immature unspents in ascending
/// + `RecentlySpentOutPoints` MutexGuard for further interaction (e.g. to add new transaction to it).
pub async fn get_all_unspent_ordered_list<'a, T: UtxoCommonOps>(
    coin: &'a T,
    address: &Address,
) -> UtxoRpcResult<(Vec<UnspentInfo>, RecentlySpentOutPointsGuard<'a>)> {
    let decimals = coin.as_ref().decimals;
    let mut unspents = coin
        .as_ref()
        .rpc_client
        .list_unspent(address, decimals)
        .compat()
        .await?;

    // For Electrum legacy addresses also query `<pubkey> OP_CHECKSIG` (P2PK)
    // script-hash unspents so they are available for selection and spending.
    let mut p2pk_unspents = crate::utxo::electrum_p2pk_unspents_for_address(coin.as_ref(), address).await?;
    unspents.append(&mut p2pk_unspents);

    let recently_spent = coin.as_ref().recently_spent_outpoints.lock().await;
    let unordered_unspents = recently_spent.replace_spent_outputs_with_cache(unspents.into_iter().collect());
    let ordered_unspents = sort_dedup_unspents(unordered_unspents);
    Ok((ordered_unspents, recently_spent))
}

/// [`GetUtxoMapOps::get_all_unspent_ordered_map`] implementation.
/// Returns available mature and immature unspents in ascending order for every given `addresses`
/// + `RecentlySpentOutPoints` MutexGuard for further interaction (e.g. to add new transaction to it).
pub async fn get_all_unspent_ordered_map<T: UtxoCommonOps>(
    coin: &T,
    addresses: Vec<Address>,
) -> UtxoRpcResult<(UnspentMap, RecentlySpentOutPointsGuard<'_>)> {
    let decimals = coin.as_ref().decimals;
    let mut unspents_map = coin
        .as_ref()
        .rpc_client
        .list_unspent_group(addresses, decimals)
        .compat()
        .await?;
    let recently_spent = coin.as_ref().recently_spent_outpoints.lock().await;
    for (_address, unspents) in unspents_map.iter_mut() {
        let unordered_unspents = recently_spent.replace_spent_outputs_with_cache(unspents.iter().cloned().collect());
        *unspents = sort_dedup_unspents(unordered_unspents);
    }
    Ok((unspents_map, recently_spent))
}

/// Increase the given `dynamic_fee` according to the fee approximation `stage` using the [`UtxoCoinFields::tx_fee_volatility_percent`].
pub fn increase_dynamic_fee_by_stage<T>(coin: &T, dynamic_fee: u64, stage: &FeeApproxStage) -> u64
where
    T: AsRef<UtxoCoinFields>,
{
    let base_percent = coin.as_ref().conf.tx_fee_volatility_percent;
    let percent = match stage {
        FeeApproxStage::WithoutApprox => return dynamic_fee,
        // Take into account that the dynamic fee may increase during the swap by [`UtxoCoinFields::tx_fee_volatility_percent`].
        FeeApproxStage::StartSwap => base_percent,
        // Take into account that the dynamic fee may increase at each of the following stages up to [`UtxoCoinFields::tx_fee_volatility_percent`]:
        // - until a swap is started;
        // - during the swap.
        FeeApproxStage::OrderIssue => base_percent * 2.,
        // Take into account that the dynamic fee may increase at each of the following stages up to [`UtxoCoinFields::tx_fee_volatility_percent`]:
        // - until an order is issued;
        // - until a swap is started;
        // - during the swap.
        FeeApproxStage::TradePreimage => base_percent * 2.5,
    };
    increase_by_percent(dynamic_fee, percent)
}

fn increase_by_percent(num: u64, percent: f64) -> u64 {
    let percent = num as f64 / 100. * percent;
    num + (percent.round() as u64)
}

#[derive(Deserialize)]
#[serde(default)]
pub struct MergeConditions {
    pub merge_at: usize,
    pub max_merge_at_once: usize,
}

impl Default for MergeConditions {
    fn default() -> Self {
        MergeConditions {
            merge_at: 50,
            max_merge_at_once: 50,
        }
    }
}

pub enum UtxoMergeError {
    BadMergeConditions(String),
    InternalError(String),
}

pub async fn merge_utxos<Coin>(
    coin: &Coin,
    from_address: &Address,
    to_script_pubkey: &Script,
    merge_conditions: &MergeConditions,
    broadcast: bool,
) -> MmResult<(UtxoTx, Vec<UnspentInfo>), UtxoMergeError>
where
    Coin: UtxoCommonOps + GetUtxoListOps + UtxoTxGenerationOps + UtxoTxBroadcastOps,
{
    let ticker = &coin.as_ref().conf.ticker;
    let (unspents, recently_spent) = coin.get_unspent_ordered_list(from_address).await.mm_err(|e| {
        UtxoMergeError::InternalError(format!("Error in get_unspent_ordered_list for coin={ticker}: {e}"))
    })?;

    if unspents.len() < merge_conditions.merge_at {
        return Err(UtxoMergeError::BadMergeConditions(format!(
            "Not enough unspent UTXOs to merge for coin={ticker}, found={}, required={}",
            unspents.len(),
            merge_conditions.merge_at
        ))
        .into());
    }
    let unspents: Vec<_> = unspents.into_iter().take(merge_conditions.max_merge_at_once).collect();
    if unspents.len() < 2 {
        return Err(UtxoMergeError::BadMergeConditions(format!(
            "No point of merging only a single UTXO (coin={ticker})"
        ))
        .into());
    }

    let value = unspents.iter().fold(0, |sum, unspent| sum + unspent.value);
    let output = TransactionOutput {
        value,
        script_pubkey: to_script_pubkey.to_bytes(),
    };

    if broadcast {
        let tx = generate_and_send_tx(
            coin,
            unspents.clone(),
            None,
            FeePolicy::DeductFromOutput(0),
            recently_spent,
            vec![output],
        )
        .await
        .map_to_mm(|e| {
            UtxoMergeError::InternalError(format!("Error in generate_and_send_tx for coin={ticker}: {e:?}"))
        })?;
        Ok((tx, unspents))
    } else {
        drop(recently_spent);

        let my_address = coin
            .as_ref()
            .derivation_method
            .iguana_or_err()
            .mm_err(|e| UtxoMergeError::InternalError(format!("No iguana address for coin={ticker}: {e}")))?;
        let key_pair = coin
            .as_ref()
            .priv_key_policy
            .key_pair_or_err()
            .mm_err(|e| UtxoMergeError::InternalError(format!("No key pair for coin={ticker}: {e}")))?;

        let builder = UtxoTxBuilder::new(coin)
            .add_available_inputs(unspents.clone())
            .add_outputs(vec![output])
            .with_fee_policy(FeePolicy::DeductFromOutput(0));
        let (unsigned, _) = builder
            .build()
            .await
            .mm_err(|e| UtxoMergeError::InternalError(format!("Error in tx build for coin={ticker}: {e}")))?;

        let signature_version = match &my_address.addr_format {
            UtxoAddressFormat::Segwit => SignatureVersion::WitnessV0,
            _ => coin.as_ref().conf.signature_version,
        };
        let prev_script = Builder::build_p2pkh(&my_address.hash);
        let tx = sign_tx(
            unsigned,
            key_pair,
            prev_script,
            signature_version,
            coin.as_ref().conf.fork_id,
        )
        .mm_err(|e| UtxoMergeError::InternalError(format!("Error signing tx for coin={ticker}: {e}")))?;

        Ok((tx, unspents))
    }
}

/// Fetches previous transaction outputs for the given inputs from the chain.
/// Returns a vector of (outpoint, amount_in_satoshis, script_pubkey) tuples.
async fn get_unspents_for_inputs(
    coin: &UtxoCoinFields,
    inputs: &[chain::TransactionInput],
) -> Result<Vec<(OutPoint, u64, Script)>, RawTransactionError> {
    let txids_reversed: HashSet<H256Json> = inputs
        .iter()
        .map(|input| input.previous_output.hash.reversed().into())
        .collect();

    if txids_reversed.is_empty() {
        return Ok(vec![]);
    }

    let prev_txns_loaded = get_verbose_transactions_from_cache_or_rpc(coin, txids_reversed)
        .await
        .map_err(|err| RawTransactionError::Transport(err.to_string()))?;

    let mut result = Vec::with_capacity(inputs.len());

    for input in inputs {
        let prev_tx = prev_txns_loaded
            .iter()
            .find(|prev_tx| (*prev_tx.0).reversed() == input.previous_output.hash.into())
            .ok_or_else(|| {
                RawTransactionError::NonExistentPrevOutputError(format!(
                    "{}/{}",
                    input.previous_output.hash, input.previous_output.index
                ))
            })?;
        let prev_tx = prev_tx.1.to_inner();
        if (input.previous_output.index as usize) >= prev_tx.vout.len() {
            return Err(RawTransactionError::NonExistentPrevOutputError(format!(
                "{}/{}",
                input.previous_output.hash, input.previous_output.index
            )));
        }
        let vout = &prev_tx.vout[input.previous_output.index as usize];
        let prev_script = Script::from(vout.script.hex.to_vec());
        let prev_amount_f64 = vout.value.ok_or_else(|| {
            RawTransactionError::NonExistentPrevOutputError("No amount in transaction vout".to_string())
        })?;
        let prev_amount: BigDecimal = BigDecimal::try_from(prev_amount_f64)
            .map_err(|e| RawTransactionError::DecodeError(format!("Failed converting vout value: {e}")))?;
        let amount_sat = sat_from_big_decimal(&prev_amount, coin.decimals)
            .map_err(|e| RawTransactionError::DecodeError(format!("Failed sat conversion: {e}")))?;

        result.push((input.previous_output, amount_sat, prev_script));
    }
    Ok(result)
}

/// Signs a raw UTXO transaction hex and returns the signed transaction.
async fn sign_raw_utxo_tx<T: AsRef<UtxoCoinFields> + UtxoTxGenerationOps>(
    coin: &T,
    args: &crate::SignUtxoTransactionParams,
) -> RawTransactionResult {
    let tx_bytes =
        hex::decode(args.tx_hex.as_bytes()).map_to_mm(|e| RawTransactionError::DecodeError(e.to_string()))?;
    let tx: UtxoTx = deserialize(tx_bytes.as_slice())
        .map_to_mm(|e| RawTransactionError::DecodeError(format!("Failed to deserialize transaction: {e}")))?;

    // Collect amounts for each input from prev_txns or from chain lookup.
    // We need amounts to set on the TransactionInputSigner's inputs.
    let mut input_amounts: HashMap<OutPoint, u64> = HashMap::new();

    // Parse user-provided prev_txns
    if let Some(prev_txns) = &args.prev_txns {
        for prev_utxo in prev_txns {
            let prev_hash_bytes = hex::decode(prev_utxo.tx_hash.as_bytes())
                .map_to_mm(|e| RawTransactionError::DecodeError(e.to_string()))?;
            let prev_hash = {
                let len = prev_hash_bytes.len();
                let arr: [u8; 32] = prev_hash_bytes.try_into().map_to_mm(|_| {
                    RawTransactionError::DecodeError(format!(
                        "Invalid prev_out_hash length: expected 32 bytes, got {len}"
                    ))
                })?;
                arr.into()
            };
            let amount_sat = sat_from_big_decimal(&prev_utxo.amount, coin.as_ref().decimals)
                .mm_err(|e| RawTransactionError::DecodeError(format!("Failed sat conversion: {e}")))?;
            input_amounts.insert(
                OutPoint {
                    hash: prev_hash,
                    index: prev_utxo.index,
                },
                amount_sat,
            );
        }
    }

    // Find inputs that still need amounts from chain
    let inputs_to_load: Vec<chain::TransactionInput> = tx
        .inputs()
        .iter()
        .filter(|input| !input_amounts.contains_key(&input.previous_output))
        .cloned()
        .collect();

    if !inputs_to_load.is_empty() {
        let loaded = get_unspents_for_inputs(coin.as_ref(), &inputs_to_load).await?;
        for (outpoint, amount, _script) in loaded {
            input_amounts.insert(outpoint, amount);
        }
    }

    let key_pair = coin
        .as_ref()
        .priv_key_policy
        .key_pair_or_err()
        .mm_err(|e| RawTransactionError::InternalError(e.to_string()))?;

    // Build TransactionInputSigner from the decoded tx
    let mut input_signer = TransactionInputSigner::from(tx);
    input_signer.consensus_branch_id = coin.as_ref().conf.consensus_branch_id;

    // Set amounts on each input
    for input in input_signer.inputs.iter_mut() {
        if let Some(&amount) = input_amounts.get(&input.previous_output) {
            input.amount = amount;
        }
    }

    let prev_script = Builder::build_p2pkh(&AddressHashEnum::AddressHash(key_pair.public().address_hash()));
    let signature_version = coin.as_ref().conf.signature_version;
    let fork_id = coin.as_ref().conf.fork_id;

    let tx_signed = sign_tx(input_signer, key_pair, prev_script, signature_version, fork_id)
        .mm_err(|e| RawTransactionError::SigningError(e.to_string()))?;

    let tx_signed_bytes = serialize_with_flags(&tx_signed, SERIALIZE_TRANSACTION_WITNESS);
    Ok(RawTransactionRes {
        tx_hex: tx_signed_bytes.into(),
    })
}

/// Public async entry for sign_raw_tx on UTXO coins.
/// Dispatches by SignRawTransactionEnum variant.
pub async fn sign_raw_tx<T: AsRef<UtxoCoinFields> + UtxoTxGenerationOps>(
    coin: T,
    args: crate::SignRawTransactionRequest,
) -> RawTransactionResult {
    use crate::SignRawTransactionEnum;
    match &args.tx {
        SignRawTransactionEnum::UTXO(utxo_args) => sign_raw_utxo_tx(&coin, utxo_args).await,
        _ => MmError::err(RawTransactionError::InvalidParam(format!(
            "UTXO type expected for coin {}",
            coin.as_ref().conf.ticker
        ))),
    }
}

pub async fn merge_utxo_loop<T>(
    weak: UtxoWeak,
    merge_at: usize,
    check_every: f64,
    max_merge_at_once: usize,
    constructor: impl Fn(UtxoArc) -> T,
) where
    T: UtxoCommonOps + GetUtxoListOps,
{
    loop {
        Timer::sleep(check_every).await;

        let coin = match weak.upgrade() {
            Some(arc) => constructor(arc),
            None => break,
        };

        let my_address = match coin.as_ref().derivation_method {
            DerivationMethod::Iguana(ref my_address) => my_address,
            DerivationMethod::HDWallet(_) => {
                warn!("'merge_utxo_loop' is currently not used for HD wallets");
                return;
            },
        };

        let ticker = &coin.as_ref().conf.ticker;
        let (unspents, recently_spent) = match coin.get_unspent_ordered_list(my_address).await {
            Ok((unspents, recently_spent)) => (unspents, recently_spent),
            Err(e) => {
                error!("Error {} on get_unspent_ordered_list of coin {}", e, ticker);
                continue;
            },
        };
        if unspents.len() >= merge_at {
            let unspents: Vec<_> = unspents.into_iter().take(max_merge_at_once).collect();
            info!("Trying to merge {} UTXOs of coin {}", unspents.len(), ticker);
            let value = unspents.iter().fold(0, |sum, unspent| sum + unspent.value);
            let script_pubkey = Builder::build_p2pkh(&my_address.hash).to_bytes();
            let output = TransactionOutput { value, script_pubkey };
            let merge_tx_fut = generate_and_send_tx(
                &coin,
                unspents,
                None,
                FeePolicy::DeductFromOutput(0),
                recently_spent,
                vec![output],
            );
            match merge_tx_fut.await {
                Ok(tx) => info!(
                    "UTXO merge successful for coin {}, tx_hash {:?}",
                    ticker,
                    tx.hash().reversed()
                ),
                Err(e) => error!("Error {:?} on UTXO merge attempt for coin {}", e, ticker),
            }
        }
    }
}

pub async fn broadcast_tx<T>(coin: &T, tx: &UtxoTx) -> Result<H256Json, MmError<BroadcastTxErr>>
where
    T: AsRef<UtxoCoinFields>,
{
    coin.as_ref()
        .rpc_client
        .send_transaction(tx)
        .compat()
        .await
        .mm_err(From::from)
}

/// Sorts and deduplicates the given `unspents` in ascending order.
fn sort_dedup_unspents<I>(unspents: I) -> Vec<UnspentInfo>
where
    I: IntoIterator<Item = UnspentInfo>,
{
    unspents
        .into_iter()
        // dedup just in case we add duplicates of same unspent out
        .unique_by(|unspent| unspent.outpoint)
        .sorted_unstable_by(|a, b| {
            if a.value < b.value {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        })
        .collect()
}

#[test]
fn test_increase_by_percent() {
    assert_eq!(increase_by_percent(4300, 1.), 4343);
    assert_eq!(increase_by_percent(30, 6.9), 32);
    assert_eq!(increase_by_percent(30, 6.), 32);
    assert_eq!(increase_by_percent(10, 6.), 11);
    assert_eq!(increase_by_percent(1000, 0.1), 1001);
    assert_eq!(increase_by_percent(0, 20.), 0);
    assert_eq!(increase_by_percent(20, 0.), 20);
    assert_eq!(increase_by_percent(23, 100.), 46);
    assert_eq!(increase_by_percent(100, 2.4), 102);
    assert_eq!(increase_by_percent(100, 2.5), 103);
}

#[test]
fn test_tx_v_size() {
    // Multiple legacy inputs with P2SH and P2PKH output
    // https://live.blockcypher.com/btc-testnet/tx/ac6218b33d02e069c4055af709bbb6ca92ce11e55450cde96bc17411e281e5e7/
    let mut tx: UtxoTx = "0100000002440f1a2929eb08c350cc8d2385c77c40411560c3b43b65efb5b06f997fc67672020000006b483045022100f82e88af256d2487afe0c30a166c9ecf6b7013e764e1407317c712d47f7731bd0220358a4d7987bfde2271599b5c4376d26f9ce9f1df2e04f5de8f89593352607110012103c6a78589e18b482aea046975e6d0acbdea7bf7dbf04d9d5bd67fda917815e3edfffffffffb9c2fd7a19b55a4ffbda2ce5065d988a4f4efcf1ae567b4ddb6d97529c8fb0c000000006b483045022100dd75291db32dc859657a5eead13b85c340b4d508e57d2450ebfad76484f254130220727fcd65dda046ea62b449ab217da264dbf7c7ca7e63b39c8835973a152752c1012103c6a78589e18b482aea046975e6d0acbdea7bf7dbf04d9d5bd67fda917815e3edffffffff03102700000000000017a9148d0ad41545dea44e914c419d33d422148c35a274870000000000000000166a149c0a919d4e9a23f0234df916a7dd21f9e2fdaa8f931d0000000000001976a9146d9d2b554d768232320587df75c4338ecc8bf37d88acbd8ff160".into();
    // Removing inputs script_sig as it's not included in UnsignedTransactionInput when fees are calculated
    tx.inputs[0].script_sig = Bytes::new();
    tx.inputs[1].script_sig = Bytes::new();
    let v_size = tx_size_in_v_bytes(&UtxoAddressFormat::Standard, &tx);
    assert_eq!(v_size, 403);
    // Segwit input with 2 P2WPKH outputs
    // https://live.blockcypher.com/btc-testnet/tx/8a32e794b2a8a0356bb3b2717279d118b4010bf8bb3229abb5a2b4fb86541bb2/
    // the transaction is deserialized without the witnesses which makes the calculation of v_size similar to how
    // it's calculated in generate_transaction
    let tx: UtxoTx = "0200000000010192a4497268107d7999e9551be733f5e0eab479be7d995a061a7bbdc43ef0e5ed0000000000feffffff02cd857a00000000001600145cb39bfcd68d520e29cadc990bceb5cd1562c507a0860100000000001600149a85cc05e9a722575feb770a217c73fd6145cf01024730440220030e0fb58889ab939c701f12d950f00b64836a1a33ec0d6697fd3053d469d244022053e33d72ef53b37b86eea8dfebbafffb0f919ef952dcb6ea6058b81576d8dc86012102225de6aed071dc29d0ca10b9f64a4b502e33e55b3c0759eedd8e333834c6a7d07a1f2000".into();
    let v_size = tx_size_in_v_bytes(&UtxoAddressFormat::Segwit, &tx);
    assert_eq!(v_size, 141);
    // Segwit input with 1 P2WSH output
    // https://live.blockcypher.com/btc-testnet/tx/f8c1fed6f307eb131040965bd11018787567413e6437c907b1fd15de6517ad16/
    let tx: UtxoTx = "010000000001017996e77b2b1f4e66da606cfc2f16e3f52e1eac4a294168985bd4dbd54442e61f0100000000ffffffff01ab36010000000000220020693090c0e291752d448826a9dc72c9045b34ed4f7bd77e6e8e62645c23d69ac502483045022100d0800719239d646e69171ede7f02af916ac778ffe384fa0a5928645b23826c9f022044072622de2b47cfc81ac5172b646160b0c48d69d881a0ce77be06dbd6f6e5ac0121031ac6d25833a5961e2a8822b2e8b0ac1fd55d90cbbbb18a780552cbd66fc02bb3735a9e61".into();
    let v_size = tx_size_in_v_bytes(&UtxoAddressFormat::Segwit, &tx);
    assert_eq!(v_size, 122);
    // Multipl segwit inputs with P2PKH output
    // https://live.blockcypher.com/btc-testnet/tx/649d514d76702a0925a917d830e407f4f1b52d78832520e486c140ce8d0b879f/
    let tx: UtxoTx = "0100000000010250c434acbad252481564d56b41990577c55d247aedf4bb853dca3567c4404c8f0000000000ffffffff55baf016f0628ecf0f0ec228e24d8029879b0491ab18bac61865afaa9d16e8bb0000000000ffffffff01e8030000000000001976a9146d9d2b554d768232320587df75c4338ecc8bf37d88ac0247304402202611c05dd0e748f7c9955ed94a172af7ed56a0cdf773e8c919bef6e70b13ec1c02202fd7407891c857d95cdad1038dcc333186815f50da2fc9a334f814dd8d0a2d63012103c6a78589e18b482aea046975e6d0acbdea7bf7dbf04d9d5bd67fda917815e3ed02483045022100bb9d483f6b2b46f8e70d62d65b33b6de056e1878c9c2a1beed69005daef2f89502201690cd44cf6b114fa0d494258f427e1ed11a21d897e407d8a1ff3b7e09b9a426012103c6a78589e18b482aea046975e6d0acbdea7bf7dbf04d9d5bd67fda917815e3ed9cf7bd60".into();
    let v_size = tx_size_in_v_bytes(&UtxoAddressFormat::Segwit, &tx);
    assert_eq!(v_size, 181);
    // Multiple segwit inputs
    // https://live.blockcypher.com/btc-testnet/tx/a7bb128703b57058955d555ed48b65c2c9bdefab6d3acbb4243c56e430533def/
    let tx: UtxoTx = "010000000001023b7308e5ca5d02000b743441f7653c1110e07275b7ab0e983f489e92bfdd2b360100000000ffffffffd6c4f22e9b1090b2584a82cf4cb6f85595dd13c16ad065711a7585cc373ae2e50000000000ffffffff02947b2a00000000001600148474e72f396d44504cd30b1e7b992b65344240c609050700000000001600141b891309c8fe1338786fa3476d5d1a9718d43a0202483045022100bfae465fcd8d2636b2513f68618eb4996334c94d47e285cb538e3416eaf4521b02201b953f46ff21c8715a0997888445ca814dfdb834ef373a29e304bee8b32454d901210226bde3bca3fe7c91e4afb22c4bc58951c60b9bd73514081b6bd35f5c09b8c9a602483045022100ba48839f7becbf8f91266140f9727edd08974fcc18017661477af1d19603ed31022042fd35af1b393eeb818b420e3a5922079776cc73f006d26dd67be932e1b4f9000121034b6a54040ad2175e4c198370ac36b70d0b0ab515b59becf100c4cd310afbfd0c00000000".into();
    let v_size = tx_size_in_v_bytes(&UtxoAddressFormat::Segwit, &tx);
    assert_eq!(v_size, 209)
}
