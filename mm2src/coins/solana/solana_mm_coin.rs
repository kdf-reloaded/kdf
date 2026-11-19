// solana_mm_coin — MmCoin trait implementation and withdraw logic.

use super::*;
use crate::solana::solana_common::{amount_to_units, truncate_amount, units_to_amount};

/// `withdraw` of SOL (CRD ch.40 R40.6B.9). `fee`, `memo`, `from` and `broadcast` change nothing.
async fn withdraw_impl(coin: SolanaCoin, req: WithdrawRequest) -> WithdrawResult {
    let to = solana_common::parse_recipient(&req.to)?;
    // The blockhash, the fee quote and the balance are read from one node; a transport failure reruns the reads on the
    // next one. Nothing is signed or sent before they are done (CRD ch.40 R40.1.5).
    let (hash, fee, balance) = coin.withdraw_reads().await?;
    let decimals = coin.decimals;
    let fee_amount = units_to_amount(fee, decimals);
    let balance_amount = units_to_amount(balance, decimals);
    let threshold = coin.min_tx_amount();

    let lamports = if req.max {
        // The balance less the fee; when that is nothing the amount is too low.
        match balance.checked_sub(fee) {
            Some(lamports) if lamports > 0 => lamports,
            other => {
                return MmError::err(WithdrawError::AmountTooLow {
                    amount: units_to_amount(other.unwrap_or(0), decimals),
                    threshold,
                })
            },
        }
    } else {
        let amount = truncate_amount(&req.amount, decimals);
        if amount <= BigDecimal::from(0) {
            return MmError::err(WithdrawError::AmountTooLow { amount, threshold });
        }
        let required = &amount + &fee_amount;
        if required > balance_amount {
            return MmError::err(WithdrawError::NotSufficientBalance {
                coin: coin.ticker.clone(),
                available: balance_amount,
                required,
            });
        }
        amount_to_units(&amount, decimals).mm_err(Into::into)?
    };

    let tx = solana_system_transaction::transfer(&coin.key_pair, &to, lamports, hash);
    let serialized_tx = serialize(&tx).map_to_mm(|e| WithdrawError::InternalError(e.to_string()))?;
    let tx_hash = tx
        .signatures
        .first()
        .map(ToString::to_string)
        .or_mm_err(|| WithdrawError::InternalError("The transaction carries no signature".to_string()))?;
    let total_amount = units_to_amount(lamports, decimals);
    let received_by_me = if to == coin.key_pair.pubkey() {
        total_amount.clone()
    } else {
        0.into()
    };
    let spent_by_me = &total_amount + &fee_amount;
    Ok(TransactionDetails {
        unsigned: None,
        tx_json: None,
        tx_hex: serialized_tx.into(),
        internal_id: tx_hash.as_bytes().to_vec().into(),
        tx_hash,
        from: vec![coin.my_address.clone()],
        to: vec![req.to],
        total_amount,
        my_balance_change: &received_by_me - &spent_by_me,
        spent_by_me,
        received_by_me,
        block_height: 0,
        timestamp: now_ms() / 1000,
        fee_details: Some(SolanaFeeDetails::new(fee_amount, 0.into()).into()),
        coin: coin.ticker.clone(),
        kmd_rewards: None,
        transaction_type: TransactionType::StandardTransfer,
        memo: None,
    })
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
