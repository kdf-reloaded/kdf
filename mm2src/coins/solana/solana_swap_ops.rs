// solana_swap_ops — SwapOps and WatcherOps trait implementations for SolanaCoin.

use super::*;

// Swaps are not supported for Solana coins; every operation answers an error. Orders and trade previews are also
// refused earlier (`untradable_coin_ticker`, CRD ch.60 R60.6.5). A swap that was imported from outside reaches these
// methods, so none of them may abort (CRD ch.40 R40.6B.14).
#[allow(forgetting_references, forgetting_copy_types, invalid_reference_casting)]
#[async_trait]
impl SwapOps for SolanaCoin {
    fn send_taker_fee(&self, _dex_fee: &DexFee, _fee_addr: &[u8], _uuid: &[u8]) -> TransactionFut {
        Box::new(futures01::future::err(TransactionErr::Plain(swaps_unsupported(
            &self.ticker,
        ))))
    }

    fn send_maker_payment(
        &self,
        time_lock: u32,
        maker_pub: &[u8],
        taker_pub: &[u8],
        secret_hash: &[u8],
        amount: BigDecimal,
        swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        Box::new(futures01::future::err(TransactionErr::Plain(swaps_unsupported(
            &self.ticker,
        ))))
    }

    fn send_taker_payment(
        &self,
        time_lock: u32,
        taker_pub: &[u8],
        maker_pub: &[u8],
        secret_hash: &[u8],
        amount: BigDecimal,
        swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        Box::new(futures01::future::err(TransactionErr::Plain(swaps_unsupported(
            &self.ticker,
        ))))
    }

    fn send_maker_spends_taker_payment(
        &self,
        taker_payment_tx: &[u8],
        time_lock: u32,
        taker_pub: &[u8],
        secret: &[u8],
        htlc_privkey: &[u8],
        swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        Box::new(futures01::future::err(TransactionErr::Plain(swaps_unsupported(
            &self.ticker,
        ))))
    }

    fn send_taker_spends_maker_payment(
        &self,
        maker_payment_tx: &[u8],
        time_lock: u32,
        maker_pub: &[u8],
        secret: &[u8],
        htlc_privkey: &[u8],
        swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        Box::new(futures01::future::err(TransactionErr::Plain(swaps_unsupported(
            &self.ticker,
        ))))
    }

    fn send_taker_refunds_payment(
        &self,
        taker_payment_tx: &[u8],
        time_lock: u32,
        maker_pub: &[u8],
        secret_hash: &[u8],
        htlc_privkey: &[u8],
        swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        Box::new(futures01::future::err(TransactionErr::Plain(swaps_unsupported(
            &self.ticker,
        ))))
    }

    fn send_maker_refunds_payment(
        &self,
        maker_payment_tx: &[u8],
        time_lock: u32,
        taker_pub: &[u8],
        secret_hash: &[u8],
        htlc_privkey: &[u8],
        swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        Box::new(futures01::future::err(TransactionErr::Plain(swaps_unsupported(
            &self.ticker,
        ))))
    }

    fn validate_fee(&self, _args: ValidateFeeArgs<'_>) -> Box<dyn Future<Item = (), Error = String> + Send> {
        Box::new(futures01::future::err(swaps_unsupported(&self.ticker)))
    }

    fn validate_maker_payment(&self, input: ValidatePaymentInput) -> Box<dyn Future<Item = (), Error = String> + Send> {
        Box::new(futures01::future::err(swaps_unsupported(&self.ticker)))
    }

    fn validate_taker_payment(&self, input: ValidatePaymentInput) -> Box<dyn Future<Item = (), Error = String> + Send> {
        Box::new(futures01::future::err(swaps_unsupported(&self.ticker)))
    }

    fn check_if_my_payment_sent(
        &self,
        time_lock: u32,
        my_pub: &[u8],
        other_pub: &[u8],
        secret_hash: &[u8],
        search_from_block: u64,
        swap_contract_address: &Option<BytesJson>,
    ) -> Box<dyn Future<Item = Option<TransactionEnum>, Error = String> + Send> {
        Box::new(futures01::future::err(swaps_unsupported(&self.ticker)))
    }

    async fn search_for_swap_tx_spend_my(
        &self,
        _time_lock: u32,
        _my_pub: &[u8],
        _other_pub: &[u8],
        _secret_hash: &[u8],
        _tx: &[u8],
        _search_from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
    ) -> Result<Option<FoundSwapTxSpend>, String> {
        Err(swaps_unsupported(&self.ticker))
    }

    async fn search_for_swap_tx_spend_other(
        &self,
        _time_lock: u32,
        _my_pub: &[u8],
        _other_pub: &[u8],
        _secret_hash: &[u8],
        _tx: &[u8],
        _search_from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
    ) -> Result<Option<FoundSwapTxSpend>, String> {
        Err(swaps_unsupported(&self.ticker))
    }

    fn extract_secret(&self, _secret_hash: &[u8], _spend_tx: &[u8]) -> Result<Vec<u8>, String> {
        Err(format!("Secret extraction is not supported for {}", self.ticker))
    }

    fn negotiate_swap_contract_addr(
        &self,
        _other_side_address: Option<&[u8]>,
    ) -> Result<Option<BytesJson>, MmError<NegotiateSwapContractAddrErr>> {
        MmError::err(NegotiateSwapContractAddrErr::NoSwapContract)
    }

    /// `None` is safe only because every send, spend, refund and validate method of this impl answers an error, so no
    /// funds can move with a fallback key.
    fn get_htlc_key_pair(&self) -> Option<KeyPair> { None }
}

#[async_trait]
impl WatcherOps for SolanaCoin {}
