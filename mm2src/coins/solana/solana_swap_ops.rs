// solana_swap_ops — SwapOps and WatcherOps trait implementations for SolanaCoin.

use super::swap_sol;
use super::*;
use crate::CanRefundHtlc;

// The SOL legs of the legacy swap (CRD ch.40 §40.6D). The coin trades only when the activation's program check
// passed (`swap_program`); until then every operation answers an error that names the reason. Orders and trade
// previews are refused earlier (`untradable_coin_ticker`, CRD ch.60 R60.6.5), and a swap imported from outside reaches
// these methods, so none of them may abort (CRD ch.40 R40.6B.14).
#[allow(forgetting_references, forgetting_copy_types, invalid_reference_casting)]
#[async_trait]
impl SwapOps for SolanaCoin {
    fn send_taker_fee(&self, dex_fee: &DexFee, fee_addr: &[u8], uuid: &[u8]) -> TransactionFut {
        let coin = self.clone();
        let (dex_fee, fee_addr, uuid) = (dex_fee.clone(), fee_addr.to_vec(), uuid.to_vec());
        let fut = async move {
            coin.send_sol_taker_fee(&dex_fee, &fee_addr, &uuid)
                .await
                .map(TransactionEnum::from)
        };
        Box::new(fut.boxed().compat())
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
        let coin = self.clone();
        let (maker_pub, taker_pub, secret_hash) = (maker_pub.to_vec(), taker_pub.to_vec(), secret_hash.to_vec());
        let fut = async move {
            let terms = coin
                .my_payment_terms(&maker_pub, &taker_pub, &secret_hash, time_lock, &amount)
                .map_err(TransactionErr::Plain)?;
            coin.send_sol_payment(terms).await.map(TransactionEnum::from)
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
        let coin = self.clone();
        let (taker_pub, maker_pub, secret_hash) = (taker_pub.to_vec(), maker_pub.to_vec(), secret_hash.to_vec());
        let fut = async move {
            let terms = coin
                .my_payment_terms(&taker_pub, &maker_pub, &secret_hash, time_lock, &amount)
                .map_err(TransactionErr::Plain)?;
            coin.send_sol_payment(terms).await.map(TransactionEnum::from)
        };
        Box::new(fut.boxed().compat())
    }

    fn send_maker_spends_taker_payment(
        &self,
        taker_payment_tx: &[u8],
        time_lock: u32,
        taker_pub: &[u8],
        secret: &[u8],
        _htlc_privkey: &[u8],
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        let coin = self.clone();
        let (payment, taker_pub, secret) = (taker_payment_tx.to_vec(), taker_pub.to_vec(), secret.to_vec());
        let fut = async move {
            coin.claim_as_maker(&payment, time_lock, &taker_pub, &secret)
                .await
                .map(TransactionEnum::from)
        };
        Box::new(fut.boxed().compat())
    }

    fn send_taker_spends_maker_payment(
        &self,
        maker_payment_tx: &[u8],
        time_lock: u32,
        maker_pub: &[u8],
        secret: &[u8],
        _htlc_privkey: &[u8],
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        let coin = self.clone();
        let (payment, maker_pub, secret) = (maker_payment_tx.to_vec(), maker_pub.to_vec(), secret.to_vec());
        let fut = async move {
            coin.claim_as_taker(&payment, time_lock, &maker_pub, &secret)
                .await
                .map(TransactionEnum::from)
        };
        Box::new(fut.boxed().compat())
    }

    fn send_taker_refunds_payment(
        &self,
        taker_payment_tx: &[u8],
        time_lock: u32,
        maker_pub: &[u8],
        secret_hash: &[u8],
        _htlc_privkey: &[u8],
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        let coin = self.clone();
        let (payment, maker_pub, secret_hash) = (taker_payment_tx.to_vec(), maker_pub.to_vec(), secret_hash.to_vec());
        let fut = async move {
            coin.refund_payment(&payment, time_lock, &maker_pub, &secret_hash)
                .await
                .map(TransactionEnum::from)
        };
        Box::new(fut.boxed().compat())
    }

    fn send_maker_refunds_payment(
        &self,
        maker_payment_tx: &[u8],
        time_lock: u32,
        taker_pub: &[u8],
        secret_hash: &[u8],
        _htlc_privkey: &[u8],
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        let coin = self.clone();
        let (payment, taker_pub, secret_hash) = (maker_payment_tx.to_vec(), taker_pub.to_vec(), secret_hash.to_vec());
        let fut = async move {
            coin.refund_payment(&payment, time_lock, &taker_pub, &secret_hash)
                .await
                .map(TransactionEnum::from)
        };
        Box::new(fut.boxed().compat())
    }

    fn validate_fee(&self, args: ValidateFeeArgs<'_>) -> Box<dyn Future<Item = (), Error = String> + Send> {
        let coin = self.clone();
        let fee_tx = args.fee_tx.clone();
        let expected_sender = args.expected_sender.to_vec();
        let fee_addr = args.fee_addr.to_vec();
        let dex_fee = args.dex_fee.clone();
        let uuid = args.uuid.to_vec();
        let min_block_number = args.min_block_number;
        let fut = async move {
            coin.validate_sol_fee(ValidateFeeArgs {
                fee_tx: &fee_tx,
                expected_sender: &expected_sender,
                fee_addr: &fee_addr,
                dex_fee: &dex_fee,
                min_block_number,
                uuid: &uuid,
            })
            .await
        };
        Box::new(fut.boxed().compat())
    }

    /// The maker paid: the sender is the maker's key, the receiver the taker's (this node's) key.
    fn validate_maker_payment(&self, input: ValidatePaymentInput) -> Box<dyn Future<Item = (), Error = String> + Send> {
        let coin = self.clone();
        let fut = async move {
            coin.validate_sol_payment(
                &input.payment_tx,
                input.time_lock,
                &input.maker_pub,
                &input.taker_pub,
                true,
                &input.secret_hash,
                &input.amount,
            )
            .await
        };
        Box::new(fut.boxed().compat())
    }

    /// The taker paid: the sender is the taker's key, the receiver the maker's (this node's) key.
    fn validate_taker_payment(&self, input: ValidatePaymentInput) -> Box<dyn Future<Item = (), Error = String> + Send> {
        let coin = self.clone();
        let fut = async move {
            coin.validate_sol_payment(
                &input.payment_tx,
                input.time_lock,
                &input.taker_pub,
                &input.maker_pub,
                true,
                &input.secret_hash,
                &input.amount,
            )
            .await
        };
        Box::new(fut.boxed().compat())
    }

    /// The payment address derives from the amount (R40.6D.6), so the lookup needs it: the swap machines call
    /// [`SwapOps::check_if_my_payment_sent_for_amount`], and this form refuses.
    fn check_if_my_payment_sent(
        &self,
        _time_lock: u32,
        _my_pub: &[u8],
        _other_pub: &[u8],
        _secret_hash: &[u8],
        _search_from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
    ) -> Box<dyn Future<Item = Option<TransactionEnum>, Error = String> + Send> {
        Box::new(futures01::future::err(
            "A Solana payment is found by its amount: the lookup needs it".to_owned(),
        ))
    }

    fn check_if_my_payment_sent_for_amount(
        &self,
        time_lock: u32,
        my_pub: &[u8],
        other_pub: &[u8],
        secret_hash: &[u8],
        _search_from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
        amount: &BigDecimal,
    ) -> Box<dyn Future<Item = Option<TransactionEnum>, Error = String> + Send> {
        let coin = self.clone();
        let (my_pub, other_pub, secret_hash, amount) = (
            my_pub.to_vec(),
            other_pub.to_vec(),
            secret_hash.to_vec(),
            amount.clone(),
        );
        let fut = async move {
            let program = coin.swap_program().ok_or_else(|| swaps_unsupported(&coin.ticker))?;
            let terms = coin.my_payment_terms(&my_pub, &other_pub, &secret_hash, time_lock, &amount)?;
            match coin.find_payment_wire(&program, &terms).await? {
                Some(wire) => Ok(Some(TransactionEnum::from(SolanaSwapTx::from_wire(&wire)?))),
                None => Ok(None),
            }
        };
        Box::new(fut.boxed().compat())
    }

    async fn search_for_swap_tx_spend_my(
        &self,
        time_lock: u32,
        _my_pub: &[u8],
        _other_pub: &[u8],
        secret_hash: &[u8],
        tx: &[u8],
        _search_from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
    ) -> Result<Option<FoundSwapTxSpend>, String> {
        self.search_spend_of_payment(tx, time_lock, secret_hash).await
    }

    async fn search_for_swap_tx_spend_other(
        &self,
        time_lock: u32,
        _my_pub: &[u8],
        _other_pub: &[u8],
        secret_hash: &[u8],
        tx: &[u8],
        _search_from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
    ) -> Result<Option<FoundSwapTxSpend>, String> {
        self.search_spend_of_payment(tx, time_lock, secret_hash).await
    }

    /// The secret of a top-level claim instruction in the bytes alone. A claim that is an inner instruction needs the
    /// node: [`SwapOps::extract_secret_from_spend`] reads it.
    fn extract_secret(&self, secret_hash: &[u8], spend_tx: &[u8]) -> Result<Vec<u8>, String> {
        self.secret_from_wire(secret_hash, spend_tx)
    }

    async fn extract_secret_from_spend(&self, secret_hash: &[u8], spend_tx: &[u8]) -> Result<Vec<u8>, String> {
        self.secret_from_spend(secret_hash, spend_tx).await
    }

    /// The refund gate by the cluster's time, two nodes agreeing (R40.6D.11, R40.6D.16).
    fn can_refund_htlc(&self, locktime: u64) -> Box<dyn Future<Item = CanRefundHtlc, Error = String> + Send + '_> {
        Box::new(async move { Ok(self.refund_gate(locktime).await) }.boxed().compat())
    }

    /// The program's address when the coin can trade, else the reason it cannot (R40.6D.3). A peer's field must be
    /// those same 32 bytes.
    fn negotiate_swap_contract_addr(
        &self,
        other_side_address: Option<&[u8]>,
    ) -> Result<Option<BytesJson>, MmError<NegotiateSwapContractAddrErr>> {
        let Some(program) = self.swap_program() else {
            return MmError::err(NegotiateSwapContractAddrErr::NoSwapContract);
        };
        match other_side_address {
            None | Some([]) => MmError::err(NegotiateSwapContractAddrErr::NoSwapContract),
            Some(other) if other.len() != 32 => {
                MmError::err(NegotiateSwapContractAddrErr::InvalidOtherAddrLen(other.to_vec().into()))
            },
            Some(other) if other != program.as_ref() => {
                MmError::err(NegotiateSwapContractAddrErr::UnexpectedOtherAddr(other.to_vec().into()))
            },
            Some(_) => Ok(Some(program.to_bytes().to_vec().into())),
        }
    }

    /// `None`: the coin signs with its own ed25519 key (CRD ch.40 R40.6D.5); the swap key it announces is
    /// [`SwapOps::derive_htlc_pubkey`], never a secp256k1 key.
    fn get_htlc_key_pair(&self) -> Option<KeyPair> { None }

    /// The coin's key in the 33-byte field of the negotiation (CRD ch.51 R63, R64; CRD ch.40 R40.6D.5).
    fn derive_htlc_pubkey(
        &self,
        _node_secp_pubkey: &keys::Public,
    ) -> MmResult<[u8; crate::SWAP_HTLC_PUBKEY_LEN], crate::HtlcPubkeyError> {
        Ok(swap_sol::swap_key_field(&self.key_pair.pubkey()))
    }

    fn validate_other_pubkey(&self, raw_pubkey: &[u8]) -> MmResult<(), crate::HtlcPubkeyError> {
        let key = swap_sol::parse_swap_key(raw_pubkey)?;
        if key == self.key_pair.pubkey() {
            return MmError::err(crate::HtlcPubkeyError::NotOnCurve(
                "ed25519",
                "the key is the coin's own key".to_owned(),
            ));
        }
        Ok(())
    }

    /// A volume that is not a whole number of lamports is refused (CRD ch.40 R40.6D.6).
    fn check_swap_volume_units(&self, volume: &BigDecimal) -> Result<(), String> {
        swap_sol::exact_lamports(volume).map(|_| ())
    }
}

#[async_trait]
impl WatcherOps for SolanaCoin {}
