// qrc20_mm_coin — MmCoin trait implementation and withdraw logic.

use super::*;

#[async_trait]
impl MmCoin for Qrc20Coin {
    fn is_asset_chain(&self) -> bool { utxo_common::is_asset_chain(&self.utxo) }

    fn withdraw(&self, req: WithdrawRequest) -> WithdrawFut {
        Box::new(qrc20_withdraw(self.clone(), req).boxed().compat())
    }

    fn get_raw_transaction(&self, req: RawTransactionRequest) -> RawTransactionFut {
        Box::new(utxo_common::get_raw_transaction(&self.utxo, req).boxed().compat())
    }

    fn decimals(&self) -> u8 { utxo_common::decimals(&self.utxo) }

    fn convert_to_address(&self, from: &str, to_address_format: Json) -> Result<String, String> {
        qtum::QtumBasedCoin::convert_to_address(self, from, to_address_format)
    }

    fn validate_address(&self, address: &str) -> ValidateAddressResult { utxo_common::validate_address(self, address) }

    fn process_history_loop(&self, ctx: MmArc) -> Box<dyn Future<Item = (), Error = ()> + Send> {
        Box::new(self.clone().history_loop(ctx).map(|_| Ok(())).boxed().compat())
    }

    fn history_sync_status(&self) -> HistorySyncState { utxo_common::history_sync_status(&self.utxo) }

    /// This method is called to check our QTUM balance.
    fn get_trade_fee(&self) -> Box<dyn Future<Item = TradeFee, Error = String> + Send> {
        // `erc20Payment` may require two `approve` contract calls in worst case,
        let gas_fee = (2 * QRC20_GAS_LIMIT_DEFAULT + QRC20_PAYMENT_GAS_LIMIT) * QRC20_GAS_PRICE_DEFAULT;

        let selfi = self.clone();
        let fut = async move {
            let fee = try_s!(selfi.get_qrc20_tx_fee(gas_fee).await);
            Ok(TradeFee {
                coin: selfi.platform.clone(),
                amount: big_decimal_from_sat(fee as i64, selfi.utxo.decimals).into(),
                paid_from_trading_vol: false,
            })
        };
        Box::new(fut.boxed().compat())
    }

    async fn get_sender_trade_fee(
        &self,
        value: TradePreimageValue,
        stage: FeeApproxStage,
    ) -> TradePreimageResult<TradeFee> {
        let decimals = self.utxo.decimals;
        // pass the dummy params
        let timelock = (now_ms() / 1000) as u32;
        let secret_hash = vec![0; 20];
        let swap_id = qrc20_swap_id(timelock, &secret_hash);
        let receiver_addr = H160::default();
        // we can avoid the requesting balance, because it doesn't affect the total fee
        let my_balance = U256::max_value();
        let value = match value {
            TradePreimageValue::Exact(value) | TradePreimageValue::UpperBound(value) => {
                wei_from_big_decimal(&value, decimals).mm_err(Into::into)?
            },
        };

        let erc20_payment_fee = {
            let erc20_payment_outputs = self
                .generate_swap_payment_outputs(
                    my_balance,
                    swap_id.clone(),
                    value,
                    timelock,
                    secret_hash.clone(),
                    receiver_addr,
                    self.swap_contract_address,
                )
                .await
                .mm_err(Into::into)?;
            self.preimage_trade_fee_required_to_send_outputs(erc20_payment_outputs, &stage)
                .await?
        };

        let sender_refund_fee = {
            let sender_refund_output = self
                .sender_refund_output(&self.swap_contract_address, swap_id, value, secret_hash, receiver_addr)
                .mm_err(Into::into)?;
            self.preimage_trade_fee_required_to_send_outputs(vec![sender_refund_output], &stage)
                .await?
        };

        let total_fee = erc20_payment_fee + sender_refund_fee;
        Ok(TradeFee {
            coin: self.platform.clone(),
            amount: total_fee.into(),
            paid_from_trading_vol: false,
        })
    }

    fn get_receiver_trade_fee(&self, stage: FeeApproxStage) -> TradePreimageFut<TradeFee> {
        let selfi = self.clone();
        let fut = async move {
            // pass the dummy params
            let timelock = (now_ms() / 1000) as u32;
            let secret = vec![0; 32];
            let swap_id = qrc20_swap_id(timelock, &secret[0..20]);
            let sender_addr = H160::default();
            // get the max available value that we can pass into the contract call params
            // see `generate_contract_call_script_pubkey`
            let value = u64::MAX.into();
            let output = selfi
                .receiver_spend_output(&selfi.swap_contract_address, swap_id, value, secret, sender_addr)
                .mm_err(Into::into)?;

            let total_fee = selfi
                .preimage_trade_fee_required_to_send_outputs(vec![output], &stage)
                .await?;
            Ok(TradeFee {
                coin: selfi.platform.clone(),
                amount: total_fee.into(),
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
        let amount = wei_from_big_decimal(&dex_fee_amount, self.utxo.decimals).mm_err(Into::into)?;

        // pass the dummy params
        let to_addr = H160::default();
        let transfer_output = self
            .transfer_output(to_addr, amount, QRC20_GAS_LIMIT_DEFAULT, QRC20_GAS_PRICE_DEFAULT)
            .mm_err(Into::into)?;

        let total_fee = self
            .preimage_trade_fee_required_to_send_outputs(vec![transfer_output], &stage)
            .await?;

        Ok(TradeFee {
            coin: self.platform.clone(),
            amount: total_fee.into(),
            paid_from_trading_vol: false,
        })
    }

    fn required_confirmations(&self) -> u64 { utxo_common::required_confirmations(&self.utxo) }

    fn requires_notarization(&self) -> bool { utxo_common::requires_notarization(&self.utxo) }

    fn set_required_confirmations(&self, confirmations: u64) {
        utxo_common::set_required_confirmations(&self.utxo, confirmations)
    }

    fn set_requires_notarization(&self, requires_nota: bool) {
        utxo_common::set_requires_notarization(&self.utxo, requires_nota)
    }

    fn swap_contract_address(&self) -> Option<BytesJson> { Some(BytesJson::from(&self.swap_contract_address.0[..])) }

    fn mature_confirmations(&self) -> Option<u32> { Some(self.utxo.conf.mature_confirmations) }

    fn coin_protocol_info(&self) -> Vec<u8> { utxo_common::coin_protocol_info(self) }

    fn is_coin_protocol_supported(&self, info: &Option<Vec<u8>>) -> bool {
        utxo_common::is_coin_protocol_supported(self, info)
    }
}

pub(crate) async fn qrc20_withdraw(coin: Qrc20Coin, req: WithdrawRequest) -> WithdrawResult {
    let to_addr = UtxoAddress::from_str(&req.to)
        .map_err(|e| e.to_string())
        .map_to_mm(WithdrawError::InvalidAddress)?;
    let conf = &coin.utxo.conf;
    let is_p2pkh = to_addr.prefix == conf.pub_addr_prefix && to_addr.t_addr_prefix == conf.pub_t_addr_prefix;
    let is_p2sh =
        to_addr.prefix == conf.p2sh_addr_prefix && to_addr.t_addr_prefix == conf.p2sh_t_addr_prefix && conf.segwit;
    if !is_p2pkh && !is_p2sh {
        let error = "Expected either P2PKH or P2SH".to_owned();
        return MmError::err(WithdrawError::InvalidAddress(error));
    }

    let _utxo_lock = UTXO_LOCK.lock().await;

    let qrc20_balance = coin.my_spendable_balance().compat().await.mm_err(Into::into)?;

    // the qrc20_amount_sat is used only within smart contract calls
    let (qrc20_amount_sat, qrc20_amount) = if req.max {
        let amount = wei_from_big_decimal(&qrc20_balance, coin.utxo.decimals).mm_err(Into::into)?;
        if amount.is_zero() {
            return MmError::err(WithdrawError::ZeroBalanceToWithdrawMax);
        }
        (amount, qrc20_balance.clone())
    } else {
        let amount_sat = wei_from_big_decimal(&req.amount, coin.utxo.decimals).mm_err(Into::into)?;
        if req.amount > qrc20_balance {
            return MmError::err(WithdrawError::NotSufficientBalance {
                coin: coin.ticker().to_owned(),
                available: qrc20_balance,
                required: req.amount,
            });
        }
        (amount_sat, req.amount)
    };

    let (gas_limit, gas_price) = match req.fee {
        Some(WithdrawFee::Qrc20Gas { gas_limit, gas_price }) => (gas_limit, gas_price),
        Some(fee_policy) => {
            let error = format!("Expected 'Qrc20Gas' fee type, found {:?}", fee_policy);
            return MmError::err(WithdrawError::InvalidFeePolicy(error));
        },
        None => (QRC20_GAS_LIMIT_DEFAULT, QRC20_GAS_PRICE_DEFAULT),
    };

    // [`Qrc20Coin::transfer_output`] shouldn't fail if the arguments are correct
    let contract_addr = qtum::contract_addr_from_utxo_addr(to_addr.clone()).mm_err(Into::into)?;
    let transfer_output = coin
        .transfer_output(contract_addr, qrc20_amount_sat, gas_limit, gas_price)
        .mm_err(Into::into)?;
    let outputs = vec![transfer_output];

    let GenerateQrc20TxResult {
        signed,
        miner_fee,
        gas_fee,
    } = coin
        .generate_qrc20_transaction(outputs)
        .await
        .mm_err(|gen_tx_error| gen_tx_error.into_withdraw_error(coin.platform.clone(), coin.utxo.decimals))?;

    let my_address = coin.utxo.derivation_method.iguana_or_err().mm_err(Into::into)?;
    let received_by_me = if to_addr == *my_address {
        qrc20_amount.clone()
    } else {
        0.into()
    };
    let my_balance_change = &received_by_me - &qrc20_amount;

    // [`MarketCoinOps::my_address`] and [`UtxoCommonOps::display_address`] shouldn't fail
    let my_address_string = coin.my_address().map_to_mm(WithdrawError::InternalError)?;
    let to_address = to_addr.display_address().map_to_mm(WithdrawError::InternalError)?;

    let fee_details = Qrc20FeeDetails {
        // QRC20 fees are paid in base platform currency (in particular Qtum)
        coin: coin.platform.clone(),
        miner_fee: utxo_common::big_decimal_from_sat(miner_fee as i64, coin.utxo.decimals),
        gas_limit,
        gas_price,
        total_gas_fee: utxo_common::big_decimal_from_sat(gas_fee as i64, coin.utxo.decimals),
    };
    Ok(TransactionDetails {
        from: vec![my_address_string],
        to: vec![to_address],
        total_amount: qrc20_amount.clone(),
        spent_by_me: qrc20_amount,
        received_by_me,
        my_balance_change,
        tx_hash: signed.hash().reversed().to_vec().to_tx_hash(),
        tx_hex: serialize(&signed).into(),
        fee_details: Some(fee_details.into()),
        block_height: 0,
        coin: conf.ticker.clone(),
        internal_id: vec![].into(),
        timestamp: now_ms() / 1000,
        kmd_rewards: None,
        transaction_type: TransactionType::StandardTransfer,
    })
}
