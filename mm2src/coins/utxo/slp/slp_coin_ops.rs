use super::*;

#[async_trait]
impl MmCoin for SlpToken {
    fn is_asset_chain(&self) -> bool { false }

    fn get_raw_transaction(&self, req: RawTransactionRequest) -> RawTransactionFut {
        Box::new(
            utxo_common::get_raw_transaction(self.platform_coin.as_ref(), req)
                .boxed()
                .compat(),
        )
    }

    fn withdraw(&self, req: WithdrawRequest) -> WithdrawFut {
        let coin = self.clone();
        let fut = async move {
            let my_address = coin
                .platform_coin
                .as_ref()
                .derivation_method
                .iguana_or_err()
                .mm_err(Into::into)?;
            let key_pair = coin
                .platform_coin
                .as_ref()
                .priv_key_policy
                .key_pair_or_err()
                .mm_err(Into::into)?;

            let address = CashAddress::decode(&req.to).map_to_mm(WithdrawError::InvalidAddress)?;
            if address.prefix != *coin.slp_prefix() {
                return MmError::err(WithdrawError::InvalidAddress(format!(
                    "Expected {} address prefix, not {}",
                    coin.slp_prefix(),
                    address.prefix
                )));
            };
            let amount = if req.max {
                coin.my_balance_sat().await.mm_err(Into::into)?
            } else {
                sat_from_big_decimal(&req.amount, coin.decimals()).mm_err(Into::into)?
            };

            if address.hash.len() != 20 {
                return MmError::err(WithdrawError::InvalidAddress(format!(
                    "Expected 20 address hash len, not {}",
                    address.hash.len()
                )));
            }

            // TODO clarify with community whether we should support withdrawal to SLP P2SH addresses
            let script_pubkey = match address.address_type {
                CashAddrType::P2PKH => {
                    ScriptBuilder::build_p2pkh(&AddressHashEnum::AddressHash(address.hash.as_slice().into())).to_bytes()
                },
                CashAddrType::P2SH => {
                    return MmError::err(WithdrawError::InvalidAddress(
                        "Withdrawal to P2SH is not supported".into(),
                    ))
                },
            };
            let slp_output = SlpOutput { amount, script_pubkey };
            let (slp_preimage, _) = coin
                .generate_slp_tx_preimage(vec![slp_output])
                .await
                .mm_err(Into::into)?;
            let mut tx_builder = UtxoTxBuilder::new(&coin.platform_coin)
                .add_required_inputs(slp_preimage.slp_inputs.into_iter().map(|slp| slp.bch_unspent))
                .add_available_inputs(slp_preimage.available_bch_inputs)
                .add_outputs(slp_preimage.outputs);

            let platform_decimals = coin.platform_decimals();
            match req.fee {
                Some(WithdrawFee::UtxoFixed { amount }) => {
                    let fixed = sat_from_big_decimal(&amount, platform_decimals).mm_err(Into::into)?;
                    tx_builder = tx_builder.with_fee(ActualTxFee::FixedPerKb(fixed))
                },
                Some(WithdrawFee::UtxoPerKbyte { amount }) => {
                    let dynamic = sat_from_big_decimal(&amount, platform_decimals).mm_err(Into::into)?;
                    tx_builder = tx_builder.with_fee(ActualTxFee::Dynamic(dynamic));
                },
                Some(fee_policy) => {
                    let error = format!(
                        "Expected 'UtxoFixed' or 'UtxoPerKbyte' fee types, found {:?}",
                        fee_policy
                    );
                    return MmError::err(WithdrawError::InvalidFeePolicy(error));
                },
                None => (),
            };

            let (unsigned, tx_data) = tx_builder.build().await.mm_err(|gen_tx_error| {
                WithdrawError::from_generate_tx_error(gen_tx_error, coin.platform_ticker().into(), platform_decimals)
            })?;

            let prev_script = ScriptBuilder::build_p2pkh(&my_address.hash);
            let signed = sign_tx(
                unsigned,
                key_pair,
                prev_script,
                coin.platform_conf().signature_version,
                coin.platform_conf().fork_id,
            )
            .mm_err(Into::into)?;
            let fee_details = SlpFeeDetails {
                amount: big_decimal_from_sat_unsigned(tx_data.fee_amount, coin.platform_decimals()),
                coin: coin.platform_coin.ticker().into(),
            };
            let my_address_string = coin.my_address().map_to_mm(WithdrawError::InternalError)?;
            let to_address = address.encode().map_to_mm(WithdrawError::InternalError)?;

            let total_amount = big_decimal_from_sat_unsigned(amount, coin.decimals());
            let spent_by_me = total_amount.clone();
            let (received_by_me, my_balance_change) = if my_address_string == to_address {
                (total_amount.clone(), 0.into())
            } else {
                (0.into(), &total_amount * &BigDecimal::from(-1))
            };

            let tx_hash: BytesJson = signed.hash().reversed().take().to_vec().into();
            let details = TransactionDetails {
                tx_json: None,
                tx_hex: serialize(&signed).into(),
                internal_id: tx_hash.clone(),
                tx_hash: tx_hash.to_tx_hash(),
                from: vec![my_address_string],
                to: vec![to_address],
                total_amount,
                spent_by_me,
                received_by_me,
                my_balance_change,
                block_height: 0,
                timestamp: now_ms() / 1000,
                fee_details: Some(fee_details.into()),
                coin: coin.ticker().into(),
                kmd_rewards: None,
                transaction_type: Default::default(),
            };
            Ok(details)
        };
        Box::new(fut.boxed().compat())
    }

    fn decimals(&self) -> u8 { self.decimals() }

    fn convert_to_address(&self, from: &str, to_address_format: Json) -> Result<String, String> {
        utxo_common::convert_to_address(&self.platform_coin, from, to_address_format)
    }

    fn validate_address(&self, address: &str) -> ValidateAddressResult {
        let cash_address = match CashAddress::decode(address) {
            Ok(a) => a,
            Err(e) => {
                return ValidateAddressResult {
                    is_valid: false,
                    reason: Some(format!("Error {} on parsing the {} as cash address", e, address)),
                }
            },
        };

        if cash_address.prefix == *self.slp_prefix() {
            ValidateAddressResult {
                is_valid: true,
                reason: None,
            }
        } else {
            ValidateAddressResult {
                is_valid: false,
                reason: Some(format!(
                    "Address {} has invalid prefix {}, expected {}",
                    address,
                    cash_address.prefix,
                    self.slp_prefix()
                )),
            }
        }
    }

    fn process_history_loop(&self, _ctx: MmArc) -> Box<dyn Future<Item = (), Error = ()> + Send> {
        warn!("process_history_loop is not implemented for SLP yet!");
        Box::new(futures01::future::err(()))
    }

    fn history_sync_status(&self) -> HistorySyncState { self.platform_coin.history_sync_status() }

    /// Get fee to be paid per 1 swap transaction
    fn get_trade_fee(&self) -> Box<dyn Future<Item = TradeFee, Error = String> + Send> {
        utxo_common::get_trade_fee(self.platform_coin.clone())
    }

    async fn get_sender_trade_fee(
        &self,
        value: TradePreimageValue,
        stage: FeeApproxStage,
    ) -> TradePreimageResult<TradeFee> {
        let slp_amount = match value {
            TradePreimageValue::Exact(decimal) | TradePreimageValue::UpperBound(decimal) => {
                sat_from_big_decimal(&decimal, self.decimals()).mm_err(Into::into)?
            },
        };
        // can use dummy P2SH script_pubkey here
        let script_pubkey = ScriptBuilder::build_p2sh(&H160::default().into()).into();
        let slp_out = SlpOutput {
            amount: slp_amount,
            script_pubkey,
        };
        let (preimage, _) = self.generate_slp_tx_preimage(vec![slp_out]).await.mm_err(Into::into)?;
        let fee = utxo_common::preimage_trade_fee_required_to_send_outputs(
            &self.platform_coin,
            preimage.outputs,
            FeePolicy::SendExact,
            None,
            &stage,
        )
        .await?;
        Ok(TradeFee {
            coin: self.platform_coin.ticker().into(),
            amount: fee.into(),
            paid_from_trading_vol: false,
        })
    }

    fn get_receiver_trade_fee(&self, _stage: FeeApproxStage) -> TradePreimageFut<TradeFee> {
        let coin = self.clone();

        let fut = async move {
            let htlc_fee = coin
                .platform_coin
                .get_htlc_spend_fee(SLP_HTLC_SPEND_SIZE)
                .await
                .mm_err(Into::into)?;
            let amount =
                (big_decimal_from_sat_unsigned(htlc_fee, coin.platform_decimals()) + coin.platform_dust_dec()).into();
            Ok(TradeFee {
                coin: coin.platform_coin.ticker().into(),
                amount,
                paid_from_trading_vol: false,
            })
        };

        Box::new(fut.boxed().compat())
    }

    async fn get_fee_to_send_taker_fee(
        &self,
        dex_fee_amount: BigDecimal,
        stage: FeeApproxStage,
    ) -> TradePreimageResult<TradeFee> {
        let slp_amount = sat_from_big_decimal(&dex_fee_amount, self.decimals()).mm_err(Into::into)?;
        // can use dummy P2PKH script_pubkey here
        let script_pubkey = ScriptBuilder::build_p2pkh(&H160::default().into()).into();
        let slp_out = SlpOutput {
            amount: slp_amount,
            script_pubkey,
        };
        let (preimage, _) = self.generate_slp_tx_preimage(vec![slp_out]).await.mm_err(Into::into)?;
        let fee = utxo_common::preimage_trade_fee_required_to_send_outputs(
            &self.platform_coin,
            preimage.outputs,
            FeePolicy::SendExact,
            None,
            &stage,
        )
        .await?;
        Ok(TradeFee {
            coin: self.platform_coin.ticker().into(),
            amount: fee.into(),
            paid_from_trading_vol: false,
        })
    }

    fn required_confirmations(&self) -> u64 { self.conf.required_confirmations.load(AtomicOrdering::Relaxed) }

    fn requires_notarization(&self) -> bool { false }

    fn set_required_confirmations(&self, confirmations: u64) {
        self.conf
            .required_confirmations
            .store(confirmations, AtomicOrdering::Relaxed);
    }

    fn set_requires_notarization(&self, _requires_nota: bool) {
        warn!("set_requires_notarization has no effect on SLPTOKEN!")
    }

    fn swap_contract_address(&self) -> Option<BytesJson> { None }

    fn mature_confirmations(&self) -> Option<u32> { self.platform_coin.mature_confirmations() }

    fn coin_protocol_info(&self) -> Vec<u8> { Vec::new() }

    fn is_coin_protocol_supported(&self, _info: &Option<Vec<u8>>) -> bool { true }
}
