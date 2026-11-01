// solana_mm_coin — MmCoin trait implementation and withdraw logic.

use super::*;

async fn withdraw_base_coin_impl(coin: SolanaCoin, req: WithdrawRequest) -> WithdrawResult {
    let (hash, fees) = coin.estimate_withdraw_fees().await.mm_err(Into::into)?;
    let res = coin
        .check_balance_and_prepare_transfer(req.max, req.amount.clone(), fees)
        .await
        .mm_err(Into::into)?;
    let to = Pubkey::from_str(&req.to).map_err(|e| WithdrawError::InvalidAddress(format!("{:?}", e)))?;
    let tx = solana_system_transaction::transfer(&coin.key_pair, &to, res.lamports_to_send, hash);
    let serialized_tx = serialize(&tx).map_to_mm(|e| WithdrawError::InternalError(e.to_string()))?;
    let total_amount = lamports_to_sol(res.lamports_to_send);
    let received_by_me = if req.to == coin.my_address {
        total_amount.clone()
    } else {
        0.into()
    };
    let spent_by_me = &total_amount + &res.sol_required;
    Ok(TransactionDetails {
        tx_json: None,
        tx_hex: serialized_tx.into(),
        tx_hash: tx.signatures[0].to_string(),
        from: vec![coin.my_address.clone()],
        to: vec![req.to],
        total_amount: spent_by_me.clone(),
        my_balance_change: &received_by_me - &spent_by_me,
        spent_by_me,
        received_by_me,
        block_height: 0,
        timestamp: now_ms() / 1000,
        fee_details: Some(
            SolanaFeeDetails {
                amount: res.sol_required,
            }
            .into(),
        ),
        coin: coin.ticker.clone(),
        internal_id: vec![].into(),
        kmd_rewards: None,
        transaction_type: TransactionType::StandardTransfer,
    })
}

async fn withdraw_impl(coin: SolanaCoin, req: WithdrawRequest) -> WithdrawResult {
    let validate_address_result = coin.validate_address(&*req.to);
    if !validate_address_result.is_valid {
        return MmError::err(WithdrawError::InvalidAddress(
            validate_address_result.reason.unwrap_or_else(|| "Unknown".to_string()),
        ));
    }
    withdraw_base_coin_impl(coin, req).await
}

#[allow(forgetting_references, forgetting_copy_types, invalid_reference_casting)]
#[async_trait]
impl MmCoin for SolanaCoin {
    fn is_asset_chain(&self) -> bool { false }

    fn withdraw(&self, req: WithdrawRequest) -> WithdrawFut {
        Box::new(Box::pin(withdraw_impl(self.clone(), req)).compat())
    }

    fn get_raw_transaction(&self, _req: RawTransactionRequest) -> RawTransactionFut { unimplemented!() }

    fn decimals(&self) -> u8 { self.decimals }

    fn convert_to_address(&self, _from: &str, _to_address_format: Json) -> Result<String, String> { unimplemented!() }

    fn validate_address(&self, address: &str) -> ValidateAddressResult {
        if address.len() != 44 {
            return ValidateAddressResult {
                is_valid: false,
                reason: Some("Invalid address length".to_string()),
            };
        }
        let result = Pubkey::from_str(address);
        match result {
            Ok(pubkey) => {
                if pubkey.is_on_curve() {
                    ValidateAddressResult {
                        is_valid: true,
                        reason: None,
                    }
                } else {
                    ValidateAddressResult {
                        is_valid: false,
                        reason: Some("not_on_curve".to_string()),
                    }
                }
            },
            Err(err) => ValidateAddressResult {
                is_valid: false,
                reason: Some(format!("{:?}", err)),
            },
        }
    }

    fn process_history_loop(&self, _ctx: MmArc) -> Box<dyn Future<Item = (), Error = ()> + Send> { unimplemented!() }

    fn history_sync_status(&self) -> HistorySyncState { unimplemented!() }

    /// Get fee to be paid per 1 swap transaction
    fn get_trade_fee(&self) -> Box<dyn Future<Item = TradeFee, Error = String> + Send> { unimplemented!() }

    async fn get_sender_trade_fee(
        &self,
        value: TradePreimageValue,
        stage: FeeApproxStage,
    ) -> TradePreimageResult<TradeFee> {
        unimplemented!()
    }

    fn get_receiver_trade_fee(&self, _stage: FeeApproxStage) -> TradePreimageFut<TradeFee> { unimplemented!() }

    async fn get_fee_to_send_taker_fee(
        &self,
        dex_fee_amount: BigDecimal,
        stage: FeeApproxStage,
    ) -> TradePreimageResult<TradeFee> {
        unimplemented!()
    }

    fn required_confirmations(&self) -> u64 { 1 }

    fn requires_notarization(&self) -> bool { false }

    fn set_required_confirmations(&self, _confirmations: u64) { unimplemented!() }

    fn set_requires_notarization(&self, _requires_nota: bool) { unimplemented!() }

    fn swap_contract_address(&self) -> Option<BytesJson> { unimplemented!() }

    fn mature_confirmations(&self) -> Option<u32> { None }

    fn coin_protocol_info(&self) -> Vec<u8> { Vec::new() }

    fn is_coin_protocol_supported(&self, _info: &Option<Vec<u8>>) -> bool { true }
}
