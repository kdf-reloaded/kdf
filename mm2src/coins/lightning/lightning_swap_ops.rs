// lightning_swap_ops — SwapOps + WatcherOps trait impls for LightningCoin
// Swaps are not supported for Lightning coins; every operation answers an error.
use super::*;

#[async_trait]
// Todo: Implement this when implementing swaps for lightning as it's is used only for swaps
impl SwapOps for LightningCoin {
    fn send_taker_fee(&self, _dex_fee: &DexFee, _fee_addr: &[u8], _uuid: &[u8]) -> TransactionFut {
        Box::new(futures01::future::err(TransactionErr::Plain(swaps_unsupported(
            &self.conf.ticker,
        ))))
    }

    fn send_maker_payment(
        &self,
        _time_lock: u32,
        _maker_pub: &[u8],
        _taker_pub: &[u8],
        _secret_hash: &[u8],
        _amount: BigDecimal,
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        Box::new(futures01::future::err(TransactionErr::Plain(swaps_unsupported(
            &self.conf.ticker,
        ))))
    }

    fn send_taker_payment(
        &self,
        _time_lock: u32,
        _taker_pub: &[u8],
        _maker_pub: &[u8],
        _secret_hash: &[u8],
        _amount: BigDecimal,
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        Box::new(futures01::future::err(TransactionErr::Plain(swaps_unsupported(
            &self.conf.ticker,
        ))))
    }

    fn send_maker_spends_taker_payment(
        &self,
        _taker_payment_tx: &[u8],
        _time_lock: u32,
        _taker_pub: &[u8],
        _secret: &[u8],
        _htlc_privkey: &[u8],
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        Box::new(futures01::future::err(TransactionErr::Plain(swaps_unsupported(
            &self.conf.ticker,
        ))))
    }

    fn send_taker_spends_maker_payment(
        &self,
        _maker_payment_tx: &[u8],
        _time_lock: u32,
        _maker_pub: &[u8],
        _secret: &[u8],
        _htlc_privkey: &[u8],
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        Box::new(futures01::future::err(TransactionErr::Plain(swaps_unsupported(
            &self.conf.ticker,
        ))))
    }

    fn send_taker_refunds_payment(
        &self,
        _taker_payment_tx: &[u8],
        _time_lock: u32,
        _maker_pub: &[u8],
        _secret_hash: &[u8],
        _htlc_privkey: &[u8],
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        Box::new(futures01::future::err(TransactionErr::Plain(swaps_unsupported(
            &self.conf.ticker,
        ))))
    }

    fn send_maker_refunds_payment(
        &self,
        _maker_payment_tx: &[u8],
        _time_lock: u32,
        _taker_pub: &[u8],
        _secret_hash: &[u8],
        _htlc_privkey: &[u8],
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        Box::new(futures01::future::err(TransactionErr::Plain(swaps_unsupported(
            &self.conf.ticker,
        ))))
    }

    fn validate_fee(&self, _args: ValidateFeeArgs<'_>) -> Box<dyn Future<Item = (), Error = String> + Send> {
        Box::new(futures01::future::err(swaps_unsupported(&self.conf.ticker)))
    }

    fn validate_maker_payment(
        &self,
        _input: ValidatePaymentInput,
    ) -> Box<dyn Future<Item = (), Error = String> + Send> {
        Box::new(futures01::future::err(swaps_unsupported(&self.conf.ticker)))
    }

    fn validate_taker_payment(
        &self,
        _input: ValidatePaymentInput,
    ) -> Box<dyn Future<Item = (), Error = String> + Send> {
        Box::new(futures01::future::err(swaps_unsupported(&self.conf.ticker)))
    }

    fn check_if_my_payment_sent(
        &self,
        _time_lock: u32,
        _my_pub: &[u8],
        _other_pub: &[u8],
        _secret_hash: &[u8],
        _search_from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
    ) -> Box<dyn Future<Item = Option<TransactionEnum>, Error = String> + Send> {
        Box::new(futures01::future::err(swaps_unsupported(&self.conf.ticker)))
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
        Err(swaps_unsupported(&self.conf.ticker))
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
        Err(swaps_unsupported(&self.conf.ticker))
    }

    fn extract_secret(&self, _secret_hash: &[u8], _spend_tx: &[u8]) -> Result<Vec<u8>, String> {
        Err(format!("Secret extraction is not supported for {}", self.conf.ticker))
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
impl WatcherOps for LightningCoin {}
