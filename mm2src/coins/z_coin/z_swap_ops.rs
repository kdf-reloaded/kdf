use super::*;
use crate::z_coin::{z_refund_gate, z_tx_read};
use crate::CanRefundHtlc;
use crate::HtlcPaymentCheck;

#[async_trait]
impl SwapOps for ZCoin {
    fn send_taker_fee(&self, dex_fee: &DexFee, _fee_addr: &[u8], uuid: &[u8]) -> TransactionFut {
        let selfi = self.clone();
        let uuid = uuid.to_owned();
        let amount = dex_fee.total_spend_amount().to_decimal();
        let fut = async move {
            let tx = try_tx_s!(z_send_dex_fee(&selfi, amount, &uuid).await);
            Ok(tx.into())
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
        let selfi = self.clone();
        let maker_pub = try_tx_fus!(Public::from_slice(maker_pub));
        let taker_pub = try_tx_fus!(Public::from_slice(taker_pub));
        let secret_hash = secret_hash.to_vec();
        let fut = async move {
            let utxo_tx = try_tx_s!(z_send_htlc(&selfi, time_lock, &maker_pub, &taker_pub, &secret_hash, amount).await);
            Ok(utxo_tx.into())
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
        let selfi = self.clone();
        let taker_pub = try_tx_fus!(Public::from_slice(taker_pub));
        let maker_pub = try_tx_fus!(Public::from_slice(maker_pub));
        let secret_hash = secret_hash.to_vec();
        let fut = async move {
            let utxo_tx = try_tx_s!(z_send_htlc(&selfi, time_lock, &taker_pub, &maker_pub, &secret_hash, amount).await);
            Ok(utxo_tx.into())
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
        let tx = try_tx_fus!(ZTransaction::read(taker_payment_tx, BranchId::Sapling));
        let key_pair = try_tx_fus!(key_pair_from_secret(htlc_privkey));
        let redeem_script = payment_script(
            time_lock,
            &*dhash160(secret),
            &try_tx_fus!(Public::from_slice(taker_pub)),
            key_pair.public(),
        );
        let script_data = ScriptBuilder::default()
            .push_data(secret)
            .push_opcode(Opcode::OP_0)
            .into_script();
        let selfi = self.clone();
        let fut = async move {
            let tx_fut = z_p2sh_spend(
                &selfi,
                tx,
                time_lock,
                SEQUENCE_FINAL,
                redeem_script,
                script_data,
                key_pair.private().secret.as_slice(),
            );
            let tx = try_ztx_s!(tx_fut.await);
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
        let tx = try_tx_fus!(ZTransaction::read(maker_payment_tx, BranchId::Sapling));
        let key_pair = try_tx_fus!(key_pair_from_secret(htlc_privkey));
        let redeem_script = payment_script(
            time_lock,
            &*dhash160(secret),
            &try_tx_fus!(Public::from_slice(maker_pub)),
            key_pair.public(),
        );
        let script_data = ScriptBuilder::default()
            .push_data(secret)
            .push_opcode(Opcode::OP_0)
            .into_script();
        let selfi = self.clone();
        let fut = async move {
            let tx_fut = z_p2sh_spend(
                &selfi,
                tx,
                time_lock,
                SEQUENCE_FINAL,
                redeem_script,
                script_data,
                key_pair.private().secret.as_slice(),
            );
            let tx = try_ztx_s!(tx_fut.await);
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
        let tx = try_tx_fus!(ZTransaction::read(taker_payment_tx, BranchId::Sapling));
        let key_pair = try_tx_fus!(key_pair_from_secret(htlc_privkey));
        let redeem_script = payment_script(
            time_lock,
            secret_hash,
            key_pair.public(),
            &try_tx_fus!(Public::from_slice(maker_pub)),
        );
        let script_data = ScriptBuilder::default().push_opcode(Opcode::OP_1).into_script();
        let selfi = self.clone();
        let fut = async move {
            let tx_fut = z_p2sh_spend(
                &selfi,
                tx,
                time_lock,
                SEQUENCE_FINAL - 1,
                redeem_script,
                script_data,
                key_pair.private().secret.as_slice(),
            );
            let tx = try_ztx_s!(tx_fut.await);
            Ok(tx.into())
        };
        Box::new(fut.boxed().compat())
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
        let tx = try_tx_fus!(ZTransaction::read(maker_payment_tx, BranchId::Sapling));
        let key_pair = try_tx_fus!(key_pair_from_secret(htlc_privkey));
        let redeem_script = payment_script(
            time_lock,
            secret_hash,
            key_pair.public(),
            &try_tx_fus!(Public::from_slice(taker_pub)),
        );
        let script_data = ScriptBuilder::default().push_opcode(Opcode::OP_1).into_script();
        let selfi = self.clone();
        let fut = async move {
            let tx_fut = z_p2sh_spend(
                &selfi,
                tx,
                time_lock,
                SEQUENCE_FINAL - 1,
                redeem_script,
                script_data,
                key_pair.private().secret.as_slice(),
            );
            let tx = try_ztx_s!(tx_fut.await);
            Ok(tx.into())
        };
        Box::new(fut.boxed().compat())
    }

    fn validate_fee(&self, args: ValidateFeeArgs<'_>) -> Box<dyn Future<Item = (), Error = String> + Send> {
        let z_tx = match args.fee_tx {
            TransactionEnum::ZTransaction(t) => t.clone(),
            _ => panic!("Unexpected tx {:?}", args.fee_tx),
        };
        let amount = args.dex_fee.total_spend_amount().to_decimal();
        let amount_sat = try_fus!(sat_from_big_decimal(&amount, self.utxo_arc.decimals));
        let expected_memo = MemoBytes::from_bytes(args.uuid).expect("Uuid length < 512");
        let min_block_number = args.min_block_number;

        let coin = self.clone();
        let fut = async move {
            let tx_hash = H256::from(*z_tx.txid().as_ref()).reversed();
            // Bytes and mined height only: a Light coin's backend decodes nothing (R39.11.4e).
            let (rpc_bytes, rpc_height) = try_s!(coin.raw_transaction_with_height(&tx_hash.into()).await);
            let mut encoded = Vec::with_capacity(1024);
            z_tx.write(&mut encoded).expect("Writing should not fail");
            if encoded != rpc_bytes {
                return ERR!(
                    "Encoded transaction {:?} does not match the tx {:?} from RPC (height {:?})",
                    encoded,
                    rpc_bytes,
                    rpc_height
                );
            }

            let block_height = match rpc_height {
                Some(h) => {
                    if h < min_block_number {
                        return ERR!("Dex fee tx {:?} confirmed before min block {}", z_tx, min_block_number);
                    } else {
                        BlockHeight::from_u32(h as u32)
                    }
                },
                None => H0,
            };

            let Some(sapling_bundle) = z_tx.sapling_bundle() else {
                return ERR!("The dex fee tx {:?} has no Sapling bundle", z_tx);
            };
            for shielded_out in sapling_bundle.shielded_outputs() {
                if let Some((note, address, memo)) = try_sapling_output_recovery(
                    &DEX_FEE_OVK,
                    shielded_out,
                    // Decryption-only parameters: the dex-fee output is built by
                    // the counterparty, which may be a modern Pirate wallet using
                    // the post-ZIP-212 note plaintext (R39.8.0am).
                    zcash_primitives::transaction::components::sapling::zip212_enforcement(
                        &crate::z_coin::ZcoinDecryptionParams::new(coin.z_fields.consensus_params.clone()),
                        block_height,
                    ),
                ) {
                    if address != coin.z_fields.dex_fee_addr {
                        let hrp = coin.z_fields.consensus_params.hrp_sapling_payment_address();
                        let encoded = encode_payment_address(hrp, &address);
                        let expected = encode_payment_address(hrp, &coin.z_fields.dex_fee_addr);
                        return ERR!(
                            "Dex fee was sent to the invalid address {}, expected {}",
                            encoded,
                            expected
                        );
                    }

                    if note.value().inner() != amount_sat {
                        return ERR!(
                            "Dex fee has invalid amount {}, expected {}",
                            note.value().inner(),
                            amount_sat
                        );
                    }

                    if memo.as_slice() != expected_memo.as_array() {
                        return ERR!("Dex fee has invalid memo {:?}, expected {:?}", memo, expected_memo);
                    }

                    return Ok(());
                }
            }

            ERR!(
                "The dex fee tx {:?} has no shielded outputs or outputs decryption failed",
                z_tx
            )
        };

        Box::new(fut.boxed().compat())
    }

    fn validate_maker_payment(&self, input: ValidatePaymentInput) -> Box<dyn Future<Item = (), Error = String> + Send> {
        let (first_pub, second_pub) = (input.maker_pub.clone(), input.taker_pub.clone());
        z_tx_read::validate_payment(self, input, first_pub, second_pub)
    }

    fn validate_taker_payment(&self, input: ValidatePaymentInput) -> Box<dyn Future<Item = (), Error = String> + Send> {
        let (first_pub, second_pub) = (input.taker_pub.clone(), input.maker_pub.clone());
        z_tx_read::validate_payment(self, input, first_pub, second_pub)
    }

    fn check_if_my_payment_sent(
        &self,
        time_lock: u32,
        my_pub: &[u8],
        other_pub: &[u8],
        secret_hash: &[u8],
        search_from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
    ) -> Box<dyn Future<Item = Option<TransactionEnum>, Error = String> + Send> {
        z_tx_read::check_if_my_payment_sent(
            self.clone(),
            time_lock,
            my_pub,
            other_pub,
            secret_hash,
            search_from_block,
        )
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
        z_tx_read::search_for_swap_tx_spend(
            self,
            time_lock,
            &try_s!(Public::from_slice(my_pub)),
            &try_s!(Public::from_slice(other_pub)),
            secret_hash,
            tx,
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
        z_tx_read::search_for_swap_tx_spend(
            self,
            time_lock,
            &try_s!(Public::from_slice(other_pub)),
            &try_s!(Public::from_slice(my_pub)),
            secret_hash,
            tx,
            search_from_block,
        )
        .await
    }

    fn htlc_detection_progress(
        &self,
        time_lock: u32,
        first_pub: &[u8],
        second_pub: &[u8],
        secret_hash: &[u8],
    ) -> Option<String> {
        z_tx_read::htlc_detection_progress(self, time_lock, first_pub, second_pub, secret_hash)
    }

    async fn check_payment_pays_htlc(
        &self,
        time_lock: u32,
        first_pub: &[u8],
        second_pub: &[u8],
        secret_hash: &[u8],
        payment_tx: &[u8],
    ) -> Result<HtlcPaymentCheck, String> {
        z_tx_read::check_payment_pays_htlc(
            time_lock,
            &try_s!(Public::from_slice(first_pub)),
            &try_s!(Public::from_slice(second_pub)),
            secret_hash,
            payment_tx,
        )
    }

    fn htlc_key_is_random_per_swap(&self) -> bool { true }

    async fn ensure_own_payment_watchable(&self) -> Result<(), String> {
        z_tx_read::ensure_own_payment_watchable(self).await
    }

    fn extract_secret(&self, secret_hash: &[u8], spend_tx: &[u8]) -> Result<Vec<u8>, String> {
        z_tx_read::extract_secret(secret_hash, spend_tx)
    }

    fn negotiate_swap_contract_addr(
        &self,
        _other_side_address: Option<&[u8]>,
    ) -> Result<Option<BytesJson>, MmError<NegotiateSwapContractAddrErr>> {
        Ok(None)
    }

    fn get_htlc_key_pair(&self) -> Option<KeyPair> { Some(KeyPair::random_compressed()) }

    /// R39.5.13: the refund waits until the wall clock and the chain's median time past have both passed
    /// the lock time, and a read failure is an error the refund stage retries after 30 seconds.
    fn can_refund_htlc(&self, locktime: u64) -> Box<dyn Future<Item = CanRefundHtlc, Error = String> + Send + '_> {
        let fut = async move { Ok(z_refund_gate::refund_gate(self, locktime).await?.into_can_refund()) };
        Box::new(fut.boxed().compat())
    }

    fn retriable_refund_rejection(&self, rejection: &str) -> Option<String> {
        z_refund_gate::non_final_rejection(rejection)
    }

    fn refund_gate_closed_note(
        &self,
        locktime: u64,
    ) -> Box<dyn Future<Item = Option<String>, Error = String> + Send + '_> {
        let fut = async move { Ok(z_refund_gate::refund_gate(self, locktime).await?.closed_note(locktime)) };
        Box::new(fut.boxed().compat())
    }
}

#[async_trait]
impl WatcherOps for ZCoin {}
