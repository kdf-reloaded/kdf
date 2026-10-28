use super::*;

#[async_trait]
impl SwapOps for SlpToken {
    fn send_taker_fee(&self, dex_fee: &DexFee, fee_addr: &[u8], _uuid: &[u8]) -> TransactionFut {
        let coin = self.clone();
        let fee_pubkey = try_tx_fus!(Public::from_slice(fee_addr));
        let script_pubkey = ScriptBuilder::build_p2pkh(&fee_pubkey.address_hash().into()).into();
        let amount = try_tx_fus!(sat_from_big_decimal(
            &dex_fee.total_spend_amount().to_decimal(),
            self.decimals()
        ));

        let fut = async move {
            let slp_out = SlpOutput { amount, script_pubkey };
            let (preimage, recently_spent) = try_tx_s!(coin.generate_slp_tx_preimage(vec![slp_out]).await);
            generate_and_send_tx(
                &coin,
                preimage.available_bch_inputs,
                Some(preimage.slp_inputs.into_iter().map(|slp| slp.bch_unspent).collect()),
                FeePolicy::SendExact,
                recently_spent,
                preimage.outputs,
            )
            .await
        };
        Box::new(fut.boxed().compat().map(|tx| tx.into()))
    }

    fn send_maker_payment(
        &self,
        time_lock: u32,
        maker_pub: &[u8],
        taker_pub: &[u8],
        secret_hash: &[u8],
        amount: BigDecimal,
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        let maker_pub = try_tx_fus!(Public::from_slice(maker_pub));
        let taker_pub = try_tx_fus!(Public::from_slice(taker_pub));
        let amount = try_tx_fus!(sat_from_big_decimal(&amount, self.decimals()));
        let secret_hash = secret_hash.to_owned();

        let coin = self.clone();
        let fut = async move {
            let tx = try_tx_s!(
                coin.send_htlc(&maker_pub, &taker_pub, time_lock, &secret_hash, amount)
                    .await
            );
            Ok(tx.into())
        };
        Box::new(fut.boxed().compat())
    }

    fn send_taker_payment(
        &self,
        time_lock: u32,
        taker_pub: &[u8],
        maker_pub: &[u8],
        secret_hash: &[u8],
        amount: BigDecimal,
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        let taker_pub = try_tx_fus!(Public::from_slice(taker_pub));
        let maker_pub = try_tx_fus!(Public::from_slice(maker_pub));
        let amount = try_tx_fus!(sat_from_big_decimal(&amount, self.decimals()));
        let secret_hash = secret_hash.to_owned();

        let coin = self.clone();
        let fut = async move {
            let tx = try_tx_s!(
                coin.send_htlc(&taker_pub, &maker_pub, time_lock, &secret_hash, amount)
                    .await
            );
            Ok(tx.into())
        };
        Box::new(fut.boxed().compat())
    }

    fn send_maker_spends_taker_payment(
        &self,
        taker_payment_tx: &[u8],
        time_lock: u32,
        taker_pub: &[u8],
        secret: &[u8],
        htlc_privkey: &[u8],
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        let tx = taker_payment_tx.to_owned();
        let taker_pub = try_tx_fus!(Public::from_slice(taker_pub));
        let secret = secret.to_owned();
        let htlc_keypair = try_tx_fus!(key_pair_from_secret(htlc_privkey));
        let coin = self.clone();

        let fut = async move {
            let tx = try_tx_s!(
                coin.spend_htlc(&tx, &taker_pub, time_lock, &secret, &htlc_keypair)
                    .await
            );
            Ok(tx.into())
        };
        Box::new(fut.boxed().compat())
    }

    fn send_taker_spends_maker_payment(
        &self,
        maker_payment_tx: &[u8],
        time_lock: u32,
        maker_pub: &[u8],
        secret: &[u8],
        htlc_privkey: &[u8],
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        let tx = maker_payment_tx.to_owned();
        let maker_pub = try_tx_fus!(Public::from_slice(maker_pub));
        let secret = secret.to_owned();
        let htlc_keypair = try_tx_fus!(key_pair_from_secret(htlc_privkey));
        let coin = self.clone();

        let fut = async move {
            let tx = try_tx_s!(
                coin.spend_htlc(&tx, &maker_pub, time_lock, &secret, &htlc_keypair)
                    .await
            );
            Ok(tx.into())
        };
        Box::new(fut.boxed().compat())
    }

    fn send_taker_refunds_payment(
        &self,
        taker_payment_tx: &[u8],
        time_lock: u32,
        maker_pub: &[u8],
        secret_hash: &[u8],
        htlc_privkey: &[u8],
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        let tx = taker_payment_tx.to_owned();
        let maker_pub = try_tx_fus!(Public::from_slice(maker_pub));
        let secret_hash = secret_hash.to_owned();
        let keypair = try_tx_fus!(key_pair_from_secret(htlc_privkey));
        let coin = self.clone();

        let fut = async move {
            let tx = try_s!(
                coin.refund_htlc(&tx, &maker_pub, time_lock, &secret_hash, &keypair)
                    .await
            );
            Ok(tx.into())
        };
        Box::new(fut.boxed().compat().map_err(TransactionErr::Plain))
    }

    fn send_maker_refunds_payment(
        &self,
        maker_payment_tx: &[u8],
        time_lock: u32,
        taker_pub: &[u8],
        secret_hash: &[u8],
        htlc_privkey: &[u8],
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        let tx = maker_payment_tx.to_owned();
        let taker_pub = try_tx_fus!(Public::from_slice(taker_pub));
        let secret_hash = secret_hash.to_owned();
        let keypair = try_tx_fus!(key_pair_from_secret(htlc_privkey));
        let coin = self.clone();

        let fut = async move {
            let tx = try_tx_s!(
                coin.refund_htlc(&tx, &taker_pub, time_lock, &secret_hash, &keypair)
                    .await
            );
            Ok(tx.into())
        };
        Box::new(fut.boxed().compat())
    }

    fn validate_fee(&self, args: ValidateFeeArgs<'_>) -> Box<dyn Future<Item = (), Error = String> + Send> {
        let tx = match args.fee_tx {
            TransactionEnum::UtxoTx(tx) => tx.clone(),
            _ => panic!(),
        };
        let coin = self.clone();
        let expected_sender = args.expected_sender.to_owned();
        let fee_addr = args.fee_addr.to_owned();
        let amount = args.dex_fee.total_spend_amount().to_decimal();
        let min_block_number = args.min_block_number;

        let fut = async move {
            try_s!(
                coin.validate_dex_fee(tx, &expected_sender, &fee_addr, amount, min_block_number)
                    .await
            );
            Ok(())
        };
        Box::new(fut.boxed().compat())
    }

    fn validate_maker_payment(&self, input: ValidatePaymentInput) -> Box<dyn Future<Item = (), Error = String> + Send> {
        let maker_pub = try_fus!(Public::from_slice(&input.maker_pub));
        let taker_pub = try_fus!(Public::from_slice(&input.taker_pub));
        let tx = input.payment_tx.to_owned();
        let secret_hash = input.secret_hash.to_owned();
        let time_lock = input.time_lock;
        let amount = input.amount;
        let confirmations = input.confirmations;

        let coin = self.clone();
        let input = ValidateHtlcInput {
            tx,
            other_pub: maker_pub,
            my_pub: taker_pub,
            time_lock,
            secret_hash,
            amount,
            confirmations,
        };
        let fut = async move {
            try_s!(coin.validate_htlc(input).await);
            Ok(())
        };
        Box::new(fut.boxed().compat())
    }

    fn validate_taker_payment(&self, input: ValidatePaymentInput) -> Box<dyn Future<Item = (), Error = String> + Send> {
        let taker_pub = try_fus!(Public::from_slice(&input.taker_pub));
        let maker_pub = try_fus!(Public::from_slice(&input.maker_pub));
        let confirmations = input.confirmations;

        let coin = self.clone();
        let input = ValidateHtlcInput {
            tx: input.payment_tx,
            other_pub: taker_pub,
            my_pub: maker_pub,
            time_lock: input.time_lock,
            secret_hash: input.secret_hash,
            amount: input.amount,
            confirmations,
        };
        let fut = async move {
            try_s!(coin.validate_htlc(input).await);
            Ok(())
        };
        Box::new(fut.boxed().compat())
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
        utxo_common::check_if_my_payment_sent(self.platform_coin.clone(), time_lock, my_pub, other_pub, secret_hash)
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
            self.platform_coin.as_ref(),
            time_lock,
            my_pub,
            other_pub,
            secret_hash,
            tx,
            SLP_SWAP_VOUT,
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
            self.platform_coin.as_ref(),
            time_lock,
            my_pub,
            other_pub,
            secret_hash,
            tx,
            SLP_SWAP_VOUT,
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

    fn get_htlc_key_pair(&self) -> Option<KeyPair> { utxo_common::get_htlc_key_pair(&self.platform_coin) }
}

#[async_trait]
impl WatcherOps for SlpToken {}
