// qrc20_swap_ops — SwapOps and WatcherOps trait implementations.

use super::*;

#[async_trait]
impl SwapOps for Qrc20Coin {
    fn send_taker_fee(&self, dex_fee: &DexFee, fee_addr: &[u8], _uuid: &[u8]) -> TransactionFut {
        let to_address = try_tx_fus!(self.contract_address_from_raw_pubkey(fee_addr));
        let amount = try_tx_fus!(wei_from_big_decimal(
            &dex_fee.total_spend_amount().to_decimal(),
            self.utxo.decimals
        ));
        let transfer_output =
            try_tx_fus!(self.transfer_output(to_address, amount, QRC20_GAS_LIMIT_DEFAULT, QRC20_GAS_PRICE_DEFAULT));
        let outputs = vec![transfer_output];

        let selfi = self.clone();
        let fut = async move { selfi.send_contract_calls(outputs).await };

        Box::new(fut.boxed().compat())
    }

    fn send_maker_payment(
        &self,
        time_lock: u32,
        _maker_pub: &[u8],
        taker_pub: &[u8],
        secret_hash: &[u8],
        amount: BigDecimal,
        swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        let taker_addr = try_tx_fus!(self.contract_address_from_raw_pubkey(taker_pub));
        let id = qrc20_swap_id(time_lock, secret_hash);
        let value = try_tx_fus!(wei_from_big_decimal(&amount, self.utxo.decimals));
        let secret_hash = Vec::from(secret_hash);
        let swap_contract_address = try_tx_fus!(swap_contract_address.try_to_address());

        let selfi = self.clone();
        let fut = async move {
            selfi
                .send_hash_time_locked_payment(id, value, time_lock, secret_hash, taker_addr, swap_contract_address)
                .await
        };
        Box::new(fut.boxed().compat())
    }

    fn send_taker_payment(
        &self,
        time_lock: u32,
        _taker_pub: &[u8],
        maker_pub: &[u8],
        secret_hash: &[u8],
        amount: BigDecimal,
        swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        let maker_addr = try_tx_fus!(self.contract_address_from_raw_pubkey(maker_pub));
        let id = qrc20_swap_id(time_lock, secret_hash);
        let value = try_tx_fus!(wei_from_big_decimal(&amount, self.utxo.decimals));
        let secret_hash = Vec::from(secret_hash);
        let swap_contract_address = try_tx_fus!(swap_contract_address.try_to_address());

        let selfi = self.clone();
        let fut = async move {
            selfi
                .send_hash_time_locked_payment(id, value, time_lock, secret_hash, maker_addr, swap_contract_address)
                .await
        };
        Box::new(fut.boxed().compat())
    }

    fn send_maker_spends_taker_payment(
        &self,
        taker_payment_tx: &[u8],
        _time_lock: u32,
        _taker_pub: &[u8],
        secret: &[u8],
        _htlc_privkey: &[u8],
        swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        let payment_tx: UtxoTx = try_tx_fus!(deserialize(taker_payment_tx).map_err(|e| ERRL!("{:?}", e)));
        let swap_contract_address = try_tx_fus!(swap_contract_address.try_to_address());
        let secret = secret.to_vec();

        let selfi = self.clone();
        let fut = async move {
            selfi
                .spend_hash_time_locked_payment(payment_tx, swap_contract_address, secret)
                .await
        };
        Box::new(fut.boxed().compat())
    }

    fn send_taker_spends_maker_payment(
        &self,
        maker_payment_tx: &[u8],
        _time_lock: u32,
        _maker_pub: &[u8],
        secret: &[u8],
        _htlc_privkey: &[u8],
        swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        let payment_tx: UtxoTx = try_tx_fus!(deserialize(maker_payment_tx).map_err(|e| ERRL!("{:?}", e)));
        let secret = secret.to_vec();
        let swap_contract_address = try_tx_fus!(swap_contract_address.try_to_address());

        let selfi = self.clone();
        let fut = async move {
            selfi
                .spend_hash_time_locked_payment(payment_tx, swap_contract_address, secret)
                .await
        };
        Box::new(fut.boxed().compat())
    }

    fn send_taker_refunds_payment(
        &self,
        taker_payment_tx: &[u8],
        _time_lock: u32,
        _maker_pub: &[u8],
        _secret_hash: &[u8],
        _htlc_privkey: &[u8],
        swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        let payment_tx: UtxoTx = try_tx_fus!(deserialize(taker_payment_tx).map_err(|e| ERRL!("{:?}", e)));
        let swap_contract_address = try_tx_fus!(swap_contract_address.try_to_address());

        let selfi = self.clone();
        let fut = async move {
            selfi
                .refund_hash_time_locked_payment(swap_contract_address, payment_tx)
                .await
        };
        Box::new(fut.boxed().compat())
    }

    fn send_maker_refunds_payment(
        &self,
        maker_payment_tx: &[u8],
        _time_lock: u32,
        _taker_pub: &[u8],
        _secret_hash: &[u8],
        _htlc_privkey: &[u8],
        swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        let payment_tx: UtxoTx = try_tx_fus!(deserialize(maker_payment_tx).map_err(|e| ERRL!("{:?}", e)));
        let swap_contract_address = try_tx_fus!(swap_contract_address.try_to_address());

        let selfi = self.clone();
        let fut = async move {
            selfi
                .refund_hash_time_locked_payment(swap_contract_address, payment_tx)
                .await
        };
        Box::new(fut.boxed().compat())
    }

    fn validate_fee(&self, args: ValidateFeeArgs<'_>) -> Box<dyn Future<Item = (), Error = String> + Send> {
        let fee_tx = match args.fee_tx {
            TransactionEnum::UtxoTx(tx) => tx,
            _ => panic!("Unexpected TransactionEnum"),
        };
        let fee_tx_hash = fee_tx.hash().reversed().into();
        if !try_fus!(check_all_inputs_signed_by_pub(&fee_tx, args.expected_sender)) {
            return Box::new(futures01::future::err(ERRL!("The dex fee was sent from wrong address")));
        }
        let fee_addr = try_fus!(self.contract_address_from_raw_pubkey(args.fee_addr));
        let expected_value = try_fus!(wei_from_big_decimal(
            &args.dex_fee.total_spend_amount().to_decimal(),
            self.utxo.decimals
        ));

        let selfi = self.clone();
        let min_block_number = args.min_block_number;
        let fut = async move {
            selfi
                .validate_fee_impl(fee_tx_hash, fee_addr, expected_value, min_block_number)
                .await
        };
        Box::new(fut.boxed().compat())
    }

    fn validate_maker_payment(&self, input: ValidatePaymentInput) -> Box<dyn Future<Item = (), Error = String> + Send> {
        let payment_tx: UtxoTx = try_fus!(deserialize(input.payment_tx.as_slice()).map_err(|e| ERRL!("{:?}", e)));
        let sender = try_fus!(self.contract_address_from_raw_pubkey(&input.maker_pub));
        let swap_contract_address = try_fus!(input.swap_contract_address.try_to_address());

        let selfi = self.clone();
        let fut = async move {
            selfi
                .validate_payment(
                    payment_tx,
                    input.time_lock,
                    sender,
                    input.secret_hash,
                    input.amount,
                    swap_contract_address,
                )
                .await
        };
        Box::new(fut.boxed().compat())
    }

    fn validate_taker_payment(&self, input: ValidatePaymentInput) -> Box<dyn Future<Item = (), Error = String> + Send> {
        let swap_contract_address = try_fus!(input.swap_contract_address.try_to_address());
        let payment_tx: UtxoTx = try_fus!(deserialize(input.payment_tx.as_slice()).map_err(|e| ERRL!("{:?}", e)));
        let sender = try_fus!(self.contract_address_from_raw_pubkey(&input.taker_pub));

        let selfi = self.clone();
        let fut = async move {
            selfi
                .validate_payment(
                    payment_tx,
                    input.time_lock,
                    sender,
                    input.secret_hash,
                    input.amount,
                    swap_contract_address,
                )
                .await
        };
        Box::new(fut.boxed().compat())
    }

    fn check_if_my_payment_sent(
        &self,
        time_lock: u32,
        _my_pub: &[u8],
        _other_pub: &[u8],
        secret_hash: &[u8],
        search_from_block: u64,
        swap_contract_address: &Option<BytesJson>,
    ) -> Box<dyn Future<Item = Option<TransactionEnum>, Error = String> + Send> {
        let swap_id = qrc20_swap_id(time_lock, secret_hash);
        let swap_contract_address = try_fus!(swap_contract_address.try_to_address());

        let selfi = self.clone();
        let fut = async move {
            selfi
                .check_if_my_payment_sent_impl(swap_contract_address, swap_id, search_from_block)
                .await
        };
        Box::new(fut.boxed().compat())
    }

    async fn search_for_swap_tx_spend_my(
        &self,
        time_lock: u32,
        _other_pub: &[u8],
        secret_hash: &[u8],
        tx: &[u8],
        search_from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
    ) -> Result<Option<FoundSwapTxSpend>, String> {
        let tx: UtxoTx = try_s!(deserialize(tx).map_err(|e| ERRL!("{:?}", e)));

        self.search_for_swap_tx_spend(time_lock, secret_hash.to_vec(), tx, search_from_block)
            .await
    }

    async fn search_for_swap_tx_spend_other(
        &self,
        time_lock: u32,
        _other_pub: &[u8],
        secret_hash: &[u8],
        tx: &[u8],
        search_from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
    ) -> Result<Option<FoundSwapTxSpend>, String> {
        let tx: UtxoTx = try_s!(deserialize(tx).map_err(|e| ERRL!("{:?}", e)));

        self.search_for_swap_tx_spend(time_lock, secret_hash.to_vec(), tx, search_from_block)
            .await
    }

    fn extract_secret(&self, secret_hash: &[u8], spend_tx: &[u8]) -> Result<Vec<u8>, String> {
        self.extract_secret_impl(secret_hash, spend_tx)
    }

    fn negotiate_swap_contract_addr(
        &self,
        other_side_address: Option<&[u8]>,
    ) -> Result<Option<BytesJson>, MmError<NegotiateSwapContractAddrErr>> {
        match other_side_address {
            Some(bytes) => {
                if bytes.len() != 20 {
                    return MmError::err(NegotiateSwapContractAddrErr::InvalidOtherAddrLen(bytes.into()));
                }
                let other_addr = H160::from_slice(bytes);
                if other_addr == self.swap_contract_address {
                    return Ok(Some(self.swap_contract_address.as_bytes().to_vec().into()));
                }

                if Some(other_addr) == self.fallback_swap_contract {
                    return Ok(self.fallback_swap_contract.map(|addr| addr.as_bytes().to_vec().into()));
                }
                MmError::err(NegotiateSwapContractAddrErr::UnexpectedOtherAddr(bytes.into()))
            },
            None => self
                .fallback_swap_contract
                .map(|addr| Some(addr.as_bytes().to_vec().into()))
                .ok_or_else(|| MmError::new(NegotiateSwapContractAddrErr::NoOtherAddrAndNoFallback)),
        }
    }

    fn get_htlc_key_pair(&self) -> Option<KeyPair> { utxo_common::get_htlc_key_pair(self) }
}

#[async_trait]
impl WatcherOps for Qrc20Coin {}
