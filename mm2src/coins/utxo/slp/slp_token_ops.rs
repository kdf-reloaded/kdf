use super::*;

impl SlpToken {
    pub fn new(
        decimals: u8,
        ticker: String,
        token_id: H256,
        platform_coin: BchCoin,
        required_confirmations: u64,
    ) -> SlpToken {
        let conf = Arc::new(SlpTokenConf {
            decimals,
            ticker,
            token_id,
            required_confirmations: AtomicU64::new(required_confirmations),
        });
        SlpToken { conf, platform_coin }
    }

    /// Returns the OP_RETURN output for SLP Send transaction
    pub(crate) fn send_op_return_output(&self, amounts: &[u64]) -> TransactionOutput {
        slp_send_output(&self.conf.token_id, amounts)
    }

    pub(crate) fn rpc(&self) -> &UtxoRpcClientEnum { &self.platform_coin.as_ref().rpc_client }

    /// Returns unspents of the SLP token plus plain BCH UTXOs plus RecentlySpentOutPoints mutex guard
    pub(crate) async fn slp_unspents_for_spend(
        &self,
    ) -> UtxoRpcResult<(Vec<SlpUnspent>, Vec<UnspentInfo>, RecentlySpentOutPointsGuard<'_>)> {
        self.platform_coin.get_token_utxos_for_spend(&self.conf.token_id).await
    }

    pub(crate) async fn slp_unspents_for_display(&self) -> UtxoRpcResult<(Vec<SlpUnspent>, Vec<UnspentInfo>)> {
        self.platform_coin
            .get_token_utxos_for_display(&self.conf.token_id)
            .await
    }

    /// Generates the tx preimage that spends the SLP from my address to the desired destinations (script pubkeys)
    pub(crate) async fn generate_slp_tx_preimage(
        &self,
        slp_outputs: Vec<SlpOutput>,
    ) -> Result<(SlpTxPreimage, RecentlySpentOutPointsGuard<'_>), MmError<GenSlpSpendErr>> {
        // the limit is 19, but we may require the change to be added
        if slp_outputs.len() > 18 {
            return MmError::err(GenSlpSpendErr::TooManyOutputs);
        }
        let (slp_unspents, bch_unspents, recently_spent) = self.slp_unspents_for_spend().await.mm_err(Into::into)?;
        let total_slp_output = slp_outputs.iter().fold(0, |cur, slp_out| cur + slp_out.amount);
        let mut total_slp_input = 0;

        let mut inputs = vec![];
        for slp_utxo in slp_unspents {
            if total_slp_input >= total_slp_output {
                break;
            }

            total_slp_input += slp_utxo.slp_amount;
            inputs.push(slp_utxo);
        }

        if total_slp_input < total_slp_output {
            return MmError::err(GenSlpSpendErr::InsufficientSlpBalance {
                coin: self.ticker().into(),
                required: big_decimal_from_sat_unsigned(total_slp_output, self.decimals()),
                available: big_decimal_from_sat_unsigned(total_slp_input, self.decimals()),
            });
        }
        let change = total_slp_input - total_slp_output;

        let mut amounts_for_op_return: Vec<_> = slp_outputs.iter().map(|spend_to| spend_to.amount).collect();
        if change > 0 {
            amounts_for_op_return.push(change);
        }

        let op_return_out_mm = self.send_op_return_output(&amounts_for_op_return);
        let mut outputs = vec![op_return_out_mm];

        outputs.extend(slp_outputs.into_iter().map(|spend_to| TransactionOutput {
            value: self.platform_dust(),
            script_pubkey: spend_to.script_pubkey,
        }));

        if change > 0 {
            let my_public_key = self.platform_coin.my_public_key().mm_err(Into::into)?;
            let slp_change_out = TransactionOutput {
                value: self.platform_dust(),
                script_pubkey: ScriptBuilder::build_p2pkh(&my_public_key.address_hash().into()).to_bytes(),
            };
            outputs.push(slp_change_out);
        }

        validate_slp_utxos(self.platform_coin.bchd_urls(), &inputs, self.token_id())
            .await
            .mm_err(Into::into)?;
        let preimage = SlpTxPreimage {
            slp_inputs: inputs,
            available_bch_inputs: bch_unspents,
            outputs,
        };
        Ok((preimage, recently_spent))
    }

    pub async fn send_slp_outputs(&self, slp_outputs: Vec<SlpOutput>) -> Result<UtxoTx, TransactionErr> {
        let (preimage, recently_spent) = try_tx_s!(self.generate_slp_tx_preimage(slp_outputs).await);
        generate_and_send_tx(
            self,
            preimage.available_bch_inputs,
            Some(preimage.slp_inputs.into_iter().map(|slp| slp.bch_unspent).collect()),
            FeePolicy::SendExact,
            recently_spent,
            preimage.outputs,
        )
        .await
    }

    pub(crate) async fn send_htlc(
        &self,
        my_pub: &Public,
        other_pub: &Public,
        time_lock: u32,
        secret_hash: &[u8],
        amount: u64,
    ) -> Result<UtxoTx, TransactionErr> {
        let payment_script = payment_script(time_lock, secret_hash, my_pub, other_pub);
        let script_pubkey = ScriptBuilder::build_p2sh(&dhash160(&payment_script).into()).to_bytes();
        let slp_out = SlpOutput { amount, script_pubkey };
        let (preimage, recently_spent) = try_tx_s!(self.generate_slp_tx_preimage(vec![slp_out]).await);
        generate_and_send_tx(
            self,
            preimage.available_bch_inputs,
            Some(preimage.slp_inputs.into_iter().map(|slp| slp.bch_unspent).collect()),
            FeePolicy::SendExact,
            recently_spent,
            preimage.outputs,
        )
        .await
    }

    pub(crate) async fn validate_htlc(&self, input: ValidateHtlcInput) -> Result<(), MmError<ValidateHtlcError>> {
        let mut tx: UtxoTx = deserialize(input.tx.as_slice()).map_to_mm(ValidateHtlcError::TxParseError)?;
        tx.tx_hash_algo = self.platform_coin.as_ref().tx_hash_algo;
        if tx.outputs.len() < 2 {
            return MmError::err(ValidateHtlcError::TxLackOfOutputs);
        }

        let slp_satoshis = sat_from_big_decimal(&input.amount, self.decimals()).mm_err(Into::into)?;

        let slp_unspent = SlpUnspent {
            bch_unspent: UnspentInfo {
                outpoint: OutPoint {
                    hash: tx.hash(),
                    index: 1,
                },
                value: 0,
                height: None,
            },
            slp_amount: slp_satoshis,
        };
        validate_slp_utxos(self.platform_coin.bchd_urls(), &[slp_unspent], self.token_id())
            .await
            .mm_err(Into::into)?;

        let slp_tx: SlpTxDetails = parse_slp_script(tx.outputs[0].script_pubkey.as_slice()).mm_err(Into::into)?;

        match slp_tx.transaction {
            SlpTransaction::Send { token_id, amounts } => {
                if token_id != self.token_id() {
                    return MmError::err(ValidateHtlcError::InvalidSlpDetails);
                }

                if amounts.is_empty() {
                    return MmError::err(ValidateHtlcError::InvalidSlpDetails);
                }

                if amounts[0] != slp_satoshis {
                    return MmError::err(ValidateHtlcError::InvalidSlpDetails);
                }
            },
            _ => return MmError::err(ValidateHtlcError::InvalidSlpDetails),
        }

        let validate_fut = utxo_common::validate_payment(
            self.platform_coin.clone(),
            tx,
            SLP_SWAP_VOUT,
            &input.other_pub,
            &input.my_pub,
            &input.secret_hash,
            self.platform_dust_dec(),
            input.time_lock,
            now_ms() / 1000 + 60,
            input.confirmations,
        );

        validate_fut
            .compat()
            .await
            .map_to_mm(ValidateHtlcError::ValidatePaymentError)?;

        Ok(())
    }

    pub async fn refund_htlc(
        &self,
        htlc_tx: &[u8],
        other_pub: &Public,
        time_lock: u32,
        secret_hash: &[u8],
        htlc_keypair: &KeyPair,
    ) -> Result<UtxoTx, MmError<SpendHtlcError>> {
        let tx: UtxoTx = deserialize(htlc_tx)?;
        if tx.outputs.is_empty() {
            return MmError::err(SpendHtlcError::TxLackOfOutputs);
        }

        let slp_tx: SlpTxDetails = parse_slp_script(tx.outputs[0].script_pubkey.as_slice()).mm_err(Into::into)?;

        let other_pub = Public::from_slice(other_pub)?;
        let my_public_key = self.platform_coin.my_public_key().mm_err(Into::into)?;
        let redeem_script = payment_script(time_lock, secret_hash, my_public_key, &other_pub);

        let slp_amount = match slp_tx.transaction {
            SlpTransaction::Send { token_id, amounts } => {
                if token_id != self.token_id() {
                    return MmError::err(SpendHtlcError::InvalidSlpDetails);
                }
                *amounts.get(0).ok_or(SpendHtlcError::InvalidSlpDetails)?
            },
            _ => return MmError::err(SpendHtlcError::InvalidSlpDetails),
        };
        let slp_utxo = SlpUnspent {
            bch_unspent: UnspentInfo {
                outpoint: OutPoint {
                    hash: tx.hash(),
                    index: SLP_SWAP_VOUT as u32,
                },
                value: tx.outputs[1].value,
                height: None,
            },
            slp_amount,
        };

        let tx_locktime = self
            .platform_coin
            .p2sh_tx_locktime(time_lock)
            .await
            .mm_err(Into::into)?;
        let script_data = ScriptBuilder::default().push_opcode(Opcode::OP_1).into_script();
        let tx = self
            .spend_p2sh(
                slp_utxo,
                tx_locktime,
                SEQUENCE_FINAL - 1,
                script_data,
                redeem_script,
                htlc_keypair,
            )
            .await
            .mm_err(Into::into)?;
        Ok(tx)
    }

    pub async fn spend_htlc(
        &self,
        htlc_tx: &[u8],
        other_pub: &Public,
        time_lock: u32,
        secret: &[u8],
        keypair: &KeyPair,
    ) -> Result<UtxoTx, MmError<SpendHtlcError>> {
        let tx: UtxoTx = deserialize(htlc_tx)?;
        let slp_tx: SlpTxDetails = deserialize(tx.outputs[0].script_pubkey.as_slice())?;

        let other_pub = Public::from_slice(other_pub)?;
        let redeem = payment_script(time_lock, &*dhash160(secret), &other_pub, keypair.public());

        let slp_amount = match slp_tx.transaction {
            SlpTransaction::Send { token_id, amounts } => {
                if token_id != self.token_id() {
                    return MmError::err(SpendHtlcError::InvalidSlpDetails);
                }
                *amounts.get(0).ok_or(SpendHtlcError::InvalidSlpDetails)?
            },
            _ => return MmError::err(SpendHtlcError::InvalidSlpDetails),
        };
        let slp_utxo = SlpUnspent {
            bch_unspent: UnspentInfo {
                outpoint: OutPoint {
                    hash: tx.hash(),
                    index: SLP_SWAP_VOUT as u32,
                },
                value: tx.outputs[1].value,
                height: None,
            },
            slp_amount,
        };

        let tx_locktime = self
            .platform_coin
            .p2sh_tx_locktime(time_lock)
            .await
            .mm_err(Into::into)?;
        let script_data = ScriptBuilder::default()
            .push_data(secret)
            .push_opcode(Opcode::OP_0)
            .into_script();
        let tx = self
            .spend_p2sh(slp_utxo, tx_locktime, SEQUENCE_FINAL, script_data, redeem, keypair)
            .await
            .mm_err(Into::into)?;
        Ok(tx)
    }

    pub async fn spend_p2sh(
        &self,
        p2sh_utxo: SlpUnspent,
        tx_locktime: u32,
        input_sequence: u32,
        script_data: Script,
        redeem_script: Script,
        htlc_keypair: &KeyPair,
    ) -> Result<UtxoTx, MmError<SpendP2SHError>> {
        let op_return_out_mm = self.send_op_return_output(&[p2sh_utxo.slp_amount]);
        let mut outputs = Vec::with_capacity(3);
        outputs.push(op_return_out_mm);

        let my_public_key = self.platform_coin.my_public_key().mm_err(Into::into)?;
        let my_script_pubkey = ScriptBuilder::build_p2pkh(&my_public_key.address_hash().into());
        let slp_output = TransactionOutput {
            value: self.platform_dust(),
            script_pubkey: my_script_pubkey.to_bytes(),
        };
        outputs.push(slp_output);

        let (_, bch_inputs, _recently_spent) = self.slp_unspents_for_spend().await.mm_err(Into::into)?;
        let (mut unsigned, _) = UtxoTxBuilder::new(&self.platform_coin)
            .add_required_inputs(std::iter::once(p2sh_utxo.bch_unspent))
            .add_available_inputs(bch_inputs)
            .add_outputs(outputs)
            .build()
            .await
            .mm_err(Into::into)?;

        unsigned.lock_time = tx_locktime;
        unsigned.inputs[0].sequence = input_sequence;

        let my_key_pair = self
            .platform_coin
            .as_ref()
            .priv_key_policy
            .key_pair_or_err()
            .mm_err(Into::into)?;
        let signed_p2sh_input = p2sh_spend(
            &unsigned,
            0,
            htlc_keypair,
            script_data,
            redeem_script,
            self.platform_coin.as_ref().conf.signature_version,
            self.platform_coin.as_ref().conf.fork_id,
        )
        .mm_err(Into::into)?;

        let signed_inputs: Result<Vec<_>, _> = unsigned
            .inputs
            .iter()
            .enumerate()
            .skip(1)
            .map(|(i, _)| {
                p2pkh_spend(
                    &unsigned,
                    i,
                    my_key_pair,
                    my_script_pubkey.clone(),
                    self.platform_coin.as_ref().conf.signature_version,
                    self.platform_coin.as_ref().conf.fork_id,
                )
            })
            .collect();

        let mut signed_inputs = signed_inputs.mm_err(Into::into)?;

        signed_inputs.insert(0, signed_p2sh_input);

        let signed = UtxoTx {
            version: unsigned.version,
            n_time: unsigned.n_time,
            overwintered: unsigned.overwintered,
            version_group_id: unsigned.version_group_id,
            inputs: signed_inputs,
            outputs: unsigned.outputs,
            lock_time: unsigned.lock_time,
            expiry_height: unsigned.expiry_height,
            shielded_spends: unsigned.shielded_spends,
            shielded_outputs: unsigned.shielded_outputs,
            join_splits: unsigned.join_splits,
            value_balance: unsigned.value_balance,
            join_split_pubkey: Default::default(),
            join_split_sig: Default::default(),
            binding_sig: Default::default(),
            zcash: unsigned.zcash,
            str_d_zeel: unsigned.str_d_zeel,
            tx_hash_algo: self.platform_coin.as_ref().tx_hash_algo,
        };

        let _broadcast = self
            .rpc()
            .send_raw_transaction(serialize(&signed).into())
            .compat()
            .await
            .mm_err(Into::into)?;
        Ok(signed)
    }

    pub(crate) async fn validate_dex_fee(
        &self,
        tx: UtxoTx,
        expected_sender: &[u8],
        fee_addr: &[u8],
        amount: BigDecimal,
        min_block_number: u64,
    ) -> Result<(), MmError<ValidateDexFeeError>> {
        if tx.outputs.len() < 2 {
            return MmError::err(ValidateDexFeeError::TxLackOfOutputs);
        }

        let slp_tx: SlpTxDetails = parse_slp_script(tx.outputs[0].script_pubkey.as_slice()).mm_err(Into::into)?;

        match slp_tx.transaction {
            SlpTransaction::Send { token_id, amounts } => {
                if token_id != self.token_id() {
                    return MmError::err(ValidateDexFeeError::InvalidSlpDetails);
                }

                if amounts.is_empty() {
                    return MmError::err(ValidateDexFeeError::InvalidSlpDetails);
                }

                let expected = sat_from_big_decimal(&amount, self.decimals()).mm_err(Into::into)?;

                if amounts[0] != expected {
                    return MmError::err(ValidateDexFeeError::InvalidSlpDetails);
                }
            },
            _ => return MmError::err(ValidateDexFeeError::InvalidSlpDetails),
        }

        let platform_dust_fee = DexFee::Standard(self.platform_dust_dec().into());
        let validate_fut = utxo_common::validate_fee(
            self.platform_coin.clone(),
            tx,
            SLP_FEE_VOUT,
            expected_sender,
            &platform_dust_fee,
            min_block_number,
            fee_addr,
        );

        validate_fut
            .compat()
            .await
            .map_to_mm(ValidateDexFeeError::ValidatePaymentError)?;

        Ok(())
    }

    pub fn platform_dust(&self) -> u64 { self.platform_coin.as_ref().dust_amount }

    pub fn platform_decimals(&self) -> u8 { self.platform_coin.as_ref().decimals }

    pub fn platform_dust_dec(&self) -> BigDecimal {
        big_decimal_from_sat_unsigned(self.platform_dust(), self.platform_decimals())
    }

    pub fn decimals(&self) -> u8 { self.conf.decimals }

    pub fn token_id(&self) -> &H256 { &self.conf.token_id }

    pub(crate) fn platform_conf(&self) -> &UtxoCoinConf { &self.platform_coin.as_ref().conf }

    pub(crate) async fn my_balance_sat(&self) -> UtxoRpcResult<u64> {
        let (slp_unspents, _) = self.slp_unspents_for_display().await?;
        let satoshi = slp_unspents.iter().fold(0, |cur, unspent| cur + unspent.slp_amount);
        Ok(satoshi)
    }

    pub async fn my_coin_balance(&self) -> UtxoRpcResult<CoinBalance> {
        let balance_sat = self.my_balance_sat().await?;
        let spendable = big_decimal_from_sat_unsigned(balance_sat, self.decimals());
        Ok(CoinBalance {
            spendable,
            unspendable: 0.into(),
            ..Default::default()
        })
    }

    pub(crate) fn slp_prefix(&self) -> &CashAddrPrefix { self.platform_coin.slp_prefix() }

    pub fn get_info(&self) -> SlpTokenInfo {
        SlpTokenInfo {
            token_id: self.conf.token_id,
            decimals: self.conf.decimals,
        }
    }
}
