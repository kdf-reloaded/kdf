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
        unsigned: None,
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
        memo: None,
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

    fn get_raw_transaction(&self, _req: RawTransactionRequest) -> RawTransactionFut {
        let coin = self.ticker.to_string();
        Box::new(futures01::future::err(MmError::new(
            RawTransactionError::NotImplemented { coin },
        )))
    }

    fn decimals(&self) -> u8 { self.decimals }

    fn convert_to_address(&self, _from: &str, _to_address_format: Json) -> Result<String, String> {
        Err(format!("convertaddress is not supported for {}", self.ticker))
    }

    fn validate_address(&self, address: &str) -> ValidateAddressResult { solana_common::validate_address(address) }

    /// Solana has no transaction history yet (CRD ch.40 R40.6B.15): the loop has nothing to do and ends at once, so
    /// a background start can never abort the process.
    fn process_history_loop(&self, _ctx: MmArc) -> Box<dyn Future<Item = (), Error = ()> + Send> {
        Box::new(futures01::future::ok(()))
    }

    /// There is no history to sync, so the state is the neutral `NotEnabled`.
    fn history_sync_status(&self) -> HistorySyncState { HistorySyncState::NotEnabled }

    /// Get fee to be paid per 1 swap transaction
    fn get_trade_fee(&self) -> Box<dyn Future<Item = TradeFee, Error = String> + Send> {
        let error = format!("get_trade_fee is not supported for {}", self.ticker);
        Box::new(futures01::future::err(error))
    }

    async fn get_sender_trade_fee(
        &self,
        _value: TradePreimageValue,
        _stage: FeeApproxStage,
    ) -> TradePreimageResult<TradeFee> {
        Err(MmError::new(TradePreimageError::InternalError(format!(
            "Trade fees are not supported for {}",
            self.ticker
        ))))
    }

    fn get_receiver_trade_fee(&self, _stage: FeeApproxStage) -> TradePreimageFut<TradeFee> {
        let error = TradePreimageError::InternalError(format!("Trade fees are not supported for {}", self.ticker));
        Box::new(futures01::future::err(MmError::new(error)))
    }

    async fn get_fee_to_send_taker_fee(
        &self,
        _dex_fee_amount: BigDecimal,
        _stage: FeeApproxStage,
    ) -> TradePreimageResult<TradeFee> {
        Err(MmError::new(TradePreimageError::InternalError(format!(
            "Trade fees are not supported for {}",
            self.ticker
        ))))
    }

    fn required_confirmations(&self) -> u64 { self.settings.required_confirmations() }

    fn requires_notarization(&self) -> bool { self.settings.requires_notarization() }

    fn set_required_confirmations(&self, confirmations: u64) { self.settings.set_required_confirmations(confirmations) }

    fn set_requires_notarization(&self, requires_nota: bool) { self.settings.set_requires_notarization(requires_nota) }

    /// No swap contract exists: Solana swaps are not supported.
    fn swap_contract_address(&self) -> Option<BytesJson> { None }

    fn mature_confirmations(&self) -> Option<u32> { None }

    fn coin_protocol_info(&self) -> Vec<u8> { Vec::new() }

    fn is_coin_protocol_supported(&self, _info: &Option<Vec<u8>>) -> bool { true }
}
