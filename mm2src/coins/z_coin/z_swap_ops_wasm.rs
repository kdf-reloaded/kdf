//! `SwapOps` for the shielded coin on the WASM target.
//!
//! The WASM build can activate, sync and report a shielded coin (CRD ch. 39 R39.6.1, R39.10.1)
//! but has no Sapling prover, so it cannot build the shielded transactions a swap needs. Every
//! operation that would build or broadcast such a transaction fails with a typed error; the
//! coin is also reported as `wallet_only`, so no swap can be started with it.
use super::*;

const WASM_SWAP_UNSUPPORTED: &str = "Shielded swap transactions cannot be built on the WASM target";

fn unsupported_tx_fut() -> TransactionFut {
    Box::new(futures01::future::err(crate::TransactionErr::Plain(ERRL!(
        "{}",
        WASM_SWAP_UNSUPPORTED
    ))))
}

#[async_trait]
impl SwapOps for ZCoin {
    fn send_taker_fee(&self, _dex_fee: &DexFee, _fee_addr: &[u8], _uuid: &[u8]) -> TransactionFut {
        unsupported_tx_fut()
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
        unsupported_tx_fut()
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
        unsupported_tx_fut()
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
        unsupported_tx_fut()
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
        unsupported_tx_fut()
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
        unsupported_tx_fut()
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
        unsupported_tx_fut()
    }

    fn validate_fee(&self, _args: ValidateFeeArgs<'_>) -> Box<dyn Future<Item = (), Error = String> + Send> {
        Box::new(futures01::future::err(WASM_SWAP_UNSUPPORTED.to_owned()))
    }

    fn validate_maker_payment(&self, input: ValidatePaymentInput) -> Box<dyn Future<Item = (), Error = String> + Send> {
        utxo_common::validate_maker_payment(self, input)
    }

    fn validate_taker_payment(&self, input: ValidatePaymentInput) -> Box<dyn Future<Item = (), Error = String> + Send> {
        utxo_common::validate_taker_payment(self, input)
    }

    fn check_if_my_payment_sent(
        &self,
        time_lock: u32,
        my_pub: &[u8],
        other_pub: &[u8],
        secret_hash: &[u8],
        _search_from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
    ) -> Box<dyn Future<Item = Option<TransactionEnum>, Error = String> + Send> {
        utxo_common::check_if_my_payment_sent(self.clone(), time_lock, my_pub, other_pub, secret_hash)
    }

    async fn search_for_swap_tx_spend_my(
        &self,
        time_lock: u32,
        my_pub: &[u8],
        other_pub: &[u8],
        secret_hash: &[u8],
        tx: &[u8],
        search_from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
    ) -> Result<Option<FoundSwapTxSpend>, String> {
        utxo_common::search_for_swap_tx_spend_my(
            self.as_ref(),
            time_lock,
            my_pub,
            other_pub,
            secret_hash,
            tx,
            utxo_common::DEFAULT_SWAP_VOUT,
            search_from_block,
        )
        .await
    }

    async fn search_for_swap_tx_spend_other(
        &self,
        time_lock: u32,
        my_pub: &[u8],
        other_pub: &[u8],
        secret_hash: &[u8],
        tx: &[u8],
        search_from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
    ) -> Result<Option<FoundSwapTxSpend>, String> {
        utxo_common::search_for_swap_tx_spend_other(
            self.as_ref(),
            time_lock,
            my_pub,
            other_pub,
            secret_hash,
            tx,
            utxo_common::DEFAULT_SWAP_VOUT,
            search_from_block,
        )
        .await
    }

    fn extract_secret(&self, secret_hash: &[u8], spend_tx: &[u8]) -> Result<Vec<u8>, String> {
        utxo_common::extract_secret(secret_hash, spend_tx)
    }

    fn negotiate_swap_contract_addr(
        &self,
        _other_side_address: Option<&[u8]>,
    ) -> Result<Option<BytesJson>, MmError<NegotiateSwapContractAddrErr>> {
        Ok(None)
    }

    // The shielded leg's HTLC key is derived per swap, never random (CRD ch.39 R39.5.6): see
    // `derive_swap_htlc_key_pair`. The swap machines ask for it there, so this key-less answer is not used.
    fn get_htlc_key_pair(&self) -> Option<KeyPair> { None }

    fn derives_swap_htlc_key(&self) -> bool { true }

    fn derive_swap_htlc_key_pair(&self, swap_unique_input: &[u8]) -> Result<KeyPair, String> {
        self.derived_htlc_key_pair(swap_unique_input)
    }

    // The derived key is unique per swap (derived from the fresh secret hash or the uuid), so the property the
    // fresh-swap skip needs still holds (CRD ch.39 R39.11.27).
    fn htlc_key_is_random_per_swap(&self) -> bool { true }
}

#[async_trait]
impl WatcherOps for ZCoin {}
