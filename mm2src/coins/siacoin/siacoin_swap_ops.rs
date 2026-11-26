// siacoin_swap_ops — SwapOps trait implementation and internal swap helper methods.

use super::*;
use crate::{HtlcPubkeyError, SWAP_HTLC_PUBKEY_LEN};
use common::log::warn;
use keys::Public;

// ── Properly-typed swap methods (called by trait impls) ──────────────

impl SiaCoin {
    /// `utxo_from_txid` looks the payment's own event up by txid and then
    /// its address's unspent outputs (`ApiClientHelpers::utxo_from_txid`,
    /// sia-rust). A single unretried lookup here turned a very short
    /// walletd indexing lag into a hard swap failure: both
    /// `send_maker_spends_taker_payment` and `send_taker_spends_maker_payment`
    /// call this immediately after `wait_for_confirmations` has *just*
    /// polled the same event as sufficiently confirmed through the same
    /// endpoint (`GetEventRequest`) -- but `wait_for_confirmations` itself
    /// retries on any error, so a walletd blip there is invisible, while
    /// this spend-side lookup had no such tolerance and failed the whole
    /// swap outright (observed on a real testnet swap: a 404 "event not
    /// found" roughly 200ms after the wait step's last successful poll of
    /// the same txid). Retrying here brings this lookup's resilience in
    /// line with the wait step's.
    async fn utxo_from_txid_with_retry(
        &self,
        txid: &TransactionId,
        vout_index: u32,
    ) -> Result<sia_rust::types::UtxoWithBasis, Box<client_error::UtxoFromTxidError>> {
        const ATTEMPTS: u8 = 5;
        const RETRY_DELAY_SECONDS: f64 = 3.;
        let mut last_err = None;
        for attempt in 1..=ATTEMPTS {
            match self.client.utxo_from_txid(txid, vout_index).await {
                Ok(utxo) => return Ok(utxo),
                Err(e) => {
                    warn!(
                        "utxo_from_txid({}, {}) attempt {}/{} failed, retrying: {}",
                        txid,
                        vout_index,
                        attempt,
                        ATTEMPTS,
                        sia_text(&e)
                    );
                    last_err = Some(e);
                    if attempt < ATTEMPTS {
                        Timer::sleep(RETRY_DELAY_SECONDS).await;
                    }
                },
            }
        }
        Err(Box::new(
            last_err.expect("loop runs at least once, so this is always Some"),
        ))
    }

    async fn new_send_taker_fee(
        &self,
        dex_fee: &DexFee,
        uuid: &[u8],
        _fee_addr: &[u8],
    ) -> Result<TransactionEnum, SendTakerFeeError> {
        let uuid_type_check = Uuid::from_slice(uuid)?;

        match uuid_type_check.get_version_num() {
            4 => (),
            version => return Err(SendTakerFeeError::UuidVersion(version)),
        }

        let trade_fee_amount = match dex_fee {
            DexFee::Standard(mm_num) => siacoin_to_hastings(BigDecimal::from(mm_num.clone()))?,
            other => return Err(SendTakerFeeError::DexFeeVariant(other.to_string())),
        };

        let my_keypair = self.my_keypair()?;

        let tx = V2TransactionBuilder::new()
            .miner_fee(Currency::DEFAULT_FEE)
            .add_siacoin_output((self.fee_address.clone(), trade_fee_amount).into())
            .fund_tx_single_source(&self.client, &my_keypair.public())
            .await?
            .arbitrary_data(uuid.to_vec().into())
            .add_change_output(&my_keypair.public().address())
            .sign_simple(vec![my_keypair])
            .build();

        self.client.broadcast_transaction(&tx).await?;

        Ok(TransactionEnum::SiaTransaction(tx.into()))
    }

    async fn new_send_maker_payment(
        &self,
        time_lock: u32,
        _maker_pub: &[u8],
        taker_pub: &[u8],
        secret_hash: &[u8],
        amount: BigDecimal,
    ) -> Result<TransactionEnum, SendMakerPaymentError> {
        let my_keypair = self.my_keypair()?;
        let maker_public_key = my_keypair.public();

        if taker_pub.len() != 33 {
            return Err(SendMakerPaymentError::InvalidTakerPublicKeyLength(taker_pub.to_vec()));
        }
        let taker_public_key = PublicKey::from_bytes(&taker_pub[..32])?;

        let secret_hash = Hash256::try_from(secret_hash)?;

        let htlc_spend_policy =
            SpendPolicy::atomic_swap(&taker_public_key, &maker_public_key, time_lock as u64, &secret_hash);

        let trade_amount = siacoin_to_hastings(amount)?;

        let tx = V2TransactionBuilder::new()
            .miner_fee(Currency::DEFAULT_FEE)
            .add_siacoin_output((htlc_spend_policy.address(), trade_amount).into())
            .fund_tx_single_source(&self.client, &my_keypair.public())
            .await?
            .add_change_output(&my_keypair.public().address())
            .sign_simple(vec![my_keypair])
            .build();

        self.client.broadcast_transaction(&tx).await?;

        Ok(TransactionEnum::SiaTransaction(tx.into()))
    }

    async fn new_send_taker_payment(
        &self,
        time_lock: u32,
        _taker_pub: &[u8],
        maker_pub: &[u8],
        secret_hash: &[u8],
        amount: BigDecimal,
    ) -> Result<TransactionEnum, SendTakerPaymentError> {
        let my_keypair = self.my_keypair()?;
        let taker_public_key = my_keypair.public();

        if maker_pub.len() != 33 {
            return Err(SendTakerPaymentError::InvalidMakerPublicKeyLength(maker_pub.to_vec()));
        }
        let maker_public_key = PublicKey::from_bytes(&maker_pub[..32])?;

        let secret_hash = Hash256::try_from(secret_hash)?;

        let htlc_spend_policy =
            SpendPolicy::atomic_swap(&maker_public_key, &taker_public_key, time_lock as u64, &secret_hash);

        let trade_amount = siacoin_to_hastings(amount)?;

        let tx = V2TransactionBuilder::new()
            .miner_fee(Currency::DEFAULT_FEE)
            .add_siacoin_output((htlc_spend_policy.address(), trade_amount).into())
            .fund_tx_single_source(&self.client, &my_keypair.public())
            .await?
            .add_change_output(&my_keypair.public().address())
            .sign_simple(vec![my_keypair])
            .build();

        self.client.broadcast_transaction(&tx).await?;

        Ok(TransactionEnum::SiaTransaction(tx.into()))
    }

    async fn new_send_maker_spends_taker_payment(
        &self,
        taker_payment_tx: &[u8],
        time_lock: u32,
        taker_pub: &[u8],
        secret: &[u8],
        secret_hash: &[u8],
    ) -> Result<TransactionEnum, MakerSpendsTakerPaymentError> {
        let my_keypair = self.my_keypair()?;
        let maker_public_key = my_keypair.public();

        if taker_pub.len() != 33 {
            return Err(MakerSpendsTakerPaymentError::InvalidTakerPublicKeyLength(
                taker_pub.to_vec(),
            ));
        }
        let taker_public_key = PublicKey::from_bytes(&taker_pub[..32])?;

        let _taker_payment_tx = SiaTransaction::try_from(taker_payment_tx.to_vec())?;
        let taker_payment_txid = _taker_payment_tx.txid();

        let secret = Preimage::try_from(secret)?;
        let secret_hash = Hash256::try_from(secret_hash)?;

        let input_spend_policy =
            SpendPolicy::atomic_swap_success(&maker_public_key, &taker_public_key, time_lock as u64, &secret_hash);

        let htlc_utxo = self.utxo_from_txid_with_retry(&taker_payment_txid, 0).await?;

        let miner_fee = Currency::DEFAULT_FEE;
        let htlc_utxo_amount = htlc_utxo.output.siacoin_output.value;

        let output_amount = htlc_value_after_miner_fee(htlc_utxo_amount, miner_fee).ok_or(
            MakerSpendsTakerPaymentError::HtlcBelowMinerFee {
                htlc_value: htlc_utxo_amount,
                miner_fee,
            },
        )?;

        let tx = V2TransactionBuilder::new()
            .miner_fee(miner_fee)
            .add_siacoin_output((maker_public_key.address(), output_amount).into())
            .add_siacoin_input(htlc_utxo.output, input_spend_policy)
            .satisfy_atomic_swap_success(my_keypair, secret, 0u32)?
            .build();

        self.client.broadcast_transaction(&tx).await?;

        Ok(TransactionEnum::SiaTransaction(tx.into()))
    }

    async fn new_send_taker_spends_maker_payment(
        &self,
        maker_payment_tx: &[u8],
        time_lock: u32,
        maker_pub: &[u8],
        secret: &[u8],
        secret_hash: &[u8],
    ) -> Result<TransactionEnum, TakerSpendsMakerPaymentError> {
        let my_keypair = self.my_keypair()?;
        let taker_public_key = my_keypair.public();

        if maker_pub.len() != 33 {
            return Err(TakerSpendsMakerPaymentError::InvalidMakerPublicKeyLength(
                maker_pub.to_vec(),
            ));
        }
        let maker_public_key = PublicKey::from_bytes(&maker_pub[..32])?;

        let _maker_payment_tx = SiaTransaction::try_from(maker_payment_tx.to_vec())?;
        let maker_payment_txid = _maker_payment_tx.txid();

        let secret = Preimage::try_from(secret)?;
        let secret_hash = Hash256::try_from(secret_hash)?;

        let input_spend_policy =
            SpendPolicy::atomic_swap_success(&taker_public_key, &maker_public_key, time_lock as u64, &secret_hash);

        let htlc_utxo = self.utxo_from_txid_with_retry(&maker_payment_txid, 0).await?;

        let miner_fee = Currency::DEFAULT_FEE;
        let htlc_utxo_amount = htlc_utxo.output.siacoin_output.value;

        let output_amount = htlc_value_after_miner_fee(htlc_utxo_amount, miner_fee).ok_or(
            TakerSpendsMakerPaymentError::HtlcBelowMinerFee {
                htlc_value: htlc_utxo_amount,
                miner_fee,
            },
        )?;

        let tx = V2TransactionBuilder::new()
            .miner_fee(miner_fee)
            .add_siacoin_output((taker_public_key.address(), output_amount).into())
            .add_siacoin_input_with_basis(htlc_utxo, input_spend_policy)
            .satisfy_atomic_swap_success(my_keypair, secret, 0u32)?
            .build();

        self.client.broadcast_transaction(&tx).await?;

        Ok(TransactionEnum::SiaTransaction(tx.into()))
    }

    async fn new_validate_fee_impl(&self, args: ValidateFeeArgs<'_>) -> Result<(), ValidateFeeError> {
        let args = SiaValidateFeeArgs::try_from(args)?;

        let peer_tx = args.fee_tx.0.clone();
        let fee_txid = peer_tx.txid();

        let found_in_block = self.client.get_event(&fee_txid).await;

        let fee_tx = match found_in_block {
            Ok(event) => {
                let tx = match event.data {
                    EventDataWrapper::V2Transaction(tx) => tx,
                    _ => return Err(ValidateFeeError::EventVariant(event)),
                };

                let confirmed_at_height = event.index.height;
                if confirmed_at_height < args.min_block_number {
                    return Err(ValidateFeeError::MininumConfirmedHeight {
                        txid: tx.txid(),
                        min_block_number: args.min_block_number,
                    });
                }
                tx
            },
            Err(e) => {
                debug!(
                    "SiaCoin::new_validate_fee: fee_tx not found on chain {}, checking mempool",
                    sia_text(&e)
                );
                match self.client.get_unconfirmed_transaction(&fee_txid).await? {
                    Some(tx) => {
                        let current_height = self.client.current_height().await?;
                        if current_height < args.min_block_number {
                            return Err(ValidateFeeError::MininumMempoolHeight {
                                txid: tx.txid(),
                                min_block_number: args.min_block_number,
                            });
                        }
                        tx
                    },
                    None => return Err(ValidateFeeError::TxNotFound(fee_txid.clone())),
                }
            },
        };

        if !fee_tx
            .siacoin_inputs
            .into_iter()
            .all(|input| input.satisfied_policy.policy.address() == args.taker_public_key.address())
        {
            return Err(ValidateFeeError::InputsOrigin(fee_txid.clone()));
        }

        match fee_tx.siacoin_outputs.len() {
            1 | 2 => (),
            outputs_length => {
                return Err(ValidateFeeError::VoutLength {
                    txid: fee_txid.clone(),
                    outputs_length,
                })
            },
        }

        if fee_tx.siacoin_outputs[0].address != self.fee_address {
            return Err(ValidateFeeError::InvalidFeeAddress {
                txid: fee_txid.clone(),
                address: fee_tx.siacoin_outputs[0].address.clone(),
            });
        }

        if fee_tx.siacoin_outputs[0].value != args.dex_fee_amount {
            return Err(ValidateFeeError::InvalidFeeAmount {
                txid: fee_txid.clone(),
                expected: args.dex_fee_amount,
                actual: fee_tx.siacoin_outputs[0].value,
            });
        }

        let fee_tx_uuid = Uuid::from_slice(&fee_tx.arbitrary_data.0)?;
        if fee_tx_uuid != args.uuid {
            return Err(ValidateFeeError::InvalidUuid {
                txid: fee_txid.clone(),
                expected: args.uuid,
                actual: fee_tx_uuid,
            });
        }

        Ok(())
    }

    async fn send_refund_htlc(
        &self,
        payment_tx: &[u8],
        time_lock: u32,
        other_pubkey: &[u8],
        secret_hash: &[u8],
    ) -> Result<TransactionEnum, SendRefundHltcError> {
        let my_keypair = self.my_keypair()?;
        let refund_public_key = my_keypair.public();

        let sia_args = SiaRefundPaymentArgs::try_from_positional(payment_tx, time_lock, other_pubkey, secret_hash)?;

        let input_spend_policy = SpendPolicy::atomic_swap_refund(
            &sia_args.success_public_key,
            &refund_public_key,
            sia_args.time_lock,
            &sia_args.secret_hash,
        );

        let htlc_utxo = self.utxo_from_txid_with_retry(&sia_args.payment_tx.txid(), 0).await?;

        let miner_fee = Currency::DEFAULT_FEE;
        let htlc_utxo_amount = htlc_utxo.output.siacoin_output.value;

        let output_amount =
            htlc_value_after_miner_fee(htlc_utxo_amount, miner_fee).ok_or(SendRefundHltcError::HtlcBelowMinerFee {
                htlc_value: htlc_utxo_amount,
                miner_fee,
            })?;

        let tx = V2TransactionBuilder::new()
            .miner_fee(miner_fee)
            .add_siacoin_output((my_keypair.public().address(), output_amount).into())
            .add_siacoin_input_with_basis(htlc_utxo, input_spend_policy)
            .satisfy_atomic_swap_refund(my_keypair, 0u32)?
            .build();

        self.client.broadcast_transaction(&tx).await?;

        Ok(TransactionEnum::SiaTransaction(tx.into()))
    }

    async fn new_check_if_my_payment_sent(
        &self,
        time_lock: u32,
        _my_pub: &[u8],
        other_pub: &[u8],
        secret_hash: &[u8],
        _search_from_block: u64,
        amount: BigDecimal,
    ) -> Result<Option<TransactionEnum>, SiaCheckIfMyPaymentSentError> {
        let sia_args = SiaCheckIfMyPaymentSentArgs::try_from_positional(time_lock, other_pub, secret_hash, amount)?;

        let my_keypair = self.my_keypair()?;
        let refund_public_key = my_keypair.public();

        let spend_policy = SpendPolicy::atomic_swap(
            &sia_args.success_public_key,
            &refund_public_key,
            sia_args.time_lock,
            &sia_args.secret_hash,
        );
        let htlc_address = spend_policy.address();

        // CRD ch.39 R39.11.5A.8, D39.11.5A.1 (a): a failed request is an undetermined answer, never "not sent".
        // CRD ch.51 R100 (P): the read goes through the coin's swap-evidence policy; a node that does not answer
        // leaves it undetermined as well.
        let (first_event, _) = self
            .swap_evidence_read(
                || async {
                    let events = self
                        .client
                        .get_address_events(htlc_address.clone())
                        .await
                        .map_err(|e| SiaCheckIfMyPaymentSentError::EventsRequest(sia_text(&e)).to_string())?;
                    Ok(events.into_iter().next().map(|event| event.data))
                },
                |first: &Option<EventDataWrapper>| {
                    first.as_ref().map(|data| match data {
                        EventDataWrapper::V2Transaction(tx) => tx.txid().to_string(),
                        other => format!("{other:?}"),
                    })
                },
                |_| false,
            )
            .await
            .map_err(SiaCheckIfMyPaymentSentError::Undetermined)?;

        let tx = match first_event {
            None => return Ok(None),
            Some(EventDataWrapper::V2Transaction(tx)) => tx,
            Some(wrong_variant) => return Err(SiaCheckIfMyPaymentSentError::EventVariant(wrong_variant)),
        };

        Ok(Some(SiaTransaction(tx).into()))
    }

    #[allow(clippy::result_large_err)]
    fn sia_extract_secret(
        &self,
        expected_hash_slice: &[u8],
        spend_tx: &[u8],
    ) -> Result<Vec<u8>, SiaCoinSiaExtractSecretError> {
        let tx = SiaTransaction::try_from(spend_tx)?;
        let expected_hash = Hash256::try_from(expected_hash_slice)?;

        extract_secret_from_tx(&tx.0, &expected_hash)
            .ok_or(SiaCoinSiaExtractSecretError::FailedToExtract { tx, expected_hash })
    }

    /// D3: locate the spend of `tx`'s HTLC output (fixed at `HTLC_VOUT_INDEX`,
    /// R-H1) by walking the HTLC address's event history, and classify it as
    /// spent-via-secret vs. refunded-via-timelock. `success_public_key`/
    /// `refund_public_key` are the same `SpendPolicy::atomic_swap` roles §20.6
    /// builds the HTLC from -- callers decide which side "my" key plays
    /// before calling this (mirrors `search_for_swap_output_spend`'s shape in
    /// the UTXO implementation).
    async fn sia_search_for_swap_tx_spend(
        &self,
        time_lock: u32,
        secret_hash: &[u8],
        tx: &[u8],
        success_public_key: PublicKey,
        refund_public_key: PublicKey,
    ) -> Result<Option<FoundSwapTxSpend>, SiaCoinSearchSwapTxSpendError> {
        let payment_tx = SiaTransaction::try_from(tx)?;
        let secret_hash = Hash256::try_from(secret_hash)?;

        let htlc_address =
            SpendPolicy::atomic_swap(&success_public_key, &refund_public_key, time_lock as u64, &secret_hash).address();
        let htlc_output_id = SiacoinOutputId::new(payment_tx.txid(), HTLC_VOUT_INDEX);

        // CRD ch.51 R100 (S): the walk goes through the coin's swap-evidence policy. A claim whose secret hashes to
        // the swap's secret hash proves itself; a refund or "unspent" needs the policy's answer.
        let (found, _) = self
            .swap_evidence_read(
                || async {
                    let events = self
                        .fetch_all_events(&htlc_address)
                        .await
                        .map_err(|e| SiaCoinSearchSwapTxSpendError::FetchEvents(e).to_string())?;
                    Ok(classify_htlc_spend(&events, &htlc_output_id, &secret_hash))
                },
                spend_decision,
                spend_proves_itself,
            )
            .await
            .map_err(SiaCoinSearchSwapTxSpendError::Undetermined)?;
        Ok(found)
    }

    /// `tx` is the payment *I* sent, so I hold the refund key and the
    /// counterparty holds the success key (§20.6, mirrors
    /// `send_maker_payment`/`send_taker_payment`'s role assignment).
    async fn sia_search_for_swap_tx_spend_my(
        &self,
        time_lock: u32,
        other_pub: &[u8],
        secret_hash: &[u8],
        tx: &[u8],
    ) -> Result<Option<FoundSwapTxSpend>, SiaCoinSearchSwapTxSpendError> {
        let my_keypair = self.my_keypair()?;

        if other_pub.len() != 33 {
            return Err(SiaCoinSearchSwapTxSpendError::InvalidOtherPublicKeyLength(
                other_pub.to_vec(),
            ));
        }
        let other_public_key = PublicKey::from_bytes(&other_pub[..32])?;

        self.sia_search_for_swap_tx_spend(time_lock, secret_hash, tx, other_public_key, my_keypair.public())
            .await
    }

    /// `tx` is the counterparty's payment, so they hold the refund key and I
    /// hold the success key -- which is why "spent" here means *I* already
    /// claimed it.
    async fn sia_search_for_swap_tx_spend_other(
        &self,
        time_lock: u32,
        other_pub: &[u8],
        secret_hash: &[u8],
        tx: &[u8],
    ) -> Result<Option<FoundSwapTxSpend>, SiaCoinSearchSwapTxSpendError> {
        let my_keypair = self.my_keypair()?;

        if other_pub.len() != 33 {
            return Err(SiaCoinSearchSwapTxSpendError::InvalidOtherPublicKeyLength(
                other_pub.to_vec(),
            ));
        }
        let other_public_key = PublicKey::from_bytes(&other_pub[..32])?;

        self.sia_search_for_swap_tx_spend(time_lock, secret_hash, tx, my_keypair.public(), other_public_key)
            .await
    }

    async fn sia_can_refund_htlc(&self, locktime: u64) -> Result<CanRefundHtlc, SiaCoinSiaCanRefundHtlcError> {
        // CRD ch.51 R100 (T): the gate is "past the lock or not" on the coin's swap-evidence read of the chain
        // time; an undetermined read is no decision either way.
        let (median_timestamp, _) = self
            .swap_evidence_read(
                || async {
                    self.client
                        .get_median_timestamp()
                        .await
                        .map_err(|e| SiaCoinSiaCanRefundHtlcError::FetchTimestamp(e).to_string())
                },
                |median_timestamp: &u64| locktime < *median_timestamp,
                |_| false,
            )
            .await
            .map_err(SiaCoinSiaCanRefundHtlcError::Undetermined)?;

        if locktime < median_timestamp {
            return Ok(CanRefundHtlc::CanRefundNow);
        }
        Ok(CanRefundHtlc::HaveToWait(locktime - median_timestamp))
    }

    async fn validate_htlc_payment(
        &self,
        input: ValidatePaymentInput,
        payment_owner: ValidatingPaymentOwner,
    ) -> Result<(), SiaValidateHtlcPaymentError> {
        let sia_args = SiaValidatePaymentInputArgs::try_from_validate_payment_input(input, payment_owner)?;

        let my_keypair = self.my_keypair()?;

        // Validating a payment *this* node can claim: our key takes the
        // success branch, the counterparty's the refund branch (§20.6).
        check_htlc_payment_output(
            &sia_args.payment_tx,
            &my_keypair.public(),
            &sia_args.other_pub,
            sia_args.time_lock,
            &sia_args.secret_hash,
            sia_args.amount,
        )
    }
}

/// Pure core of `validate_htlc_payment`: does `payment_tx`'s HTLC output
/// (R-H1, index `HTLC_VOUT_INDEX`) fund exactly the address an atomic-swap
/// spend policy with these parameters produces, for exactly `amount`?
///
/// Factored out of the trait method -- which only adds keypair lookup and
/// argument parsing around this -- so it is unit-testable without a walletd
/// client, in the same style as `classify_htlc_spend` below and
/// `check_taker_payment_output` (`siacoin_mm_coin.rs`).
///
/// The argument order is load-bearing and not interchangeable:
/// `success_pub` is the key that claims the payment by revealing the
/// preimage, `refund_pub` the key that reclaims it after `time_lock`.
/// Swapping them yields a different address, so a payment only the
/// counterparty could claim does not validate.
fn check_htlc_payment_output(
    payment_tx: &SiaTransaction,
    success_pub: &PublicKey,
    refund_pub: &PublicKey,
    time_lock: u64,
    secret_hash: &Hash256,
    amount: Currency,
) -> Result<(), SiaValidateHtlcPaymentError> {
    let htlc_address = SpendPolicy::atomic_swap(success_pub, refund_pub, time_lock, secret_hash).address();

    let expected_htlc_output = SiacoinOutput {
        value: amount,
        address: htlc_address,
    };

    let htlc_output = match payment_tx.0.siacoin_outputs.get(HTLC_VOUT_INDEX as usize) {
        Some(output) => output,
        None => {
            return Err(SiaValidateHtlcPaymentError::InvalidOutputLength {
                expected: HTLC_VOUT_INDEX + 1,
                actual: payment_tx.0.siacoin_outputs.len() as u32,
                txid: payment_tx.0.txid(),
            })
        },
    };

    if *htlc_output != expected_htlc_output {
        return Err(SiaValidateHtlcPaymentError::InvalidOutput {
            expected: expected_htlc_output,
            actual: htlc_output.clone(),
            txid: payment_tx.0.txid(),
        });
    }

    Ok(())
}

// ── Swap-spend event-walk classification (CRD ch.20 §20.10 D3) ───────

/// Find the revealed secret preimage in a spend transaction's satisfied
/// policies matching `expected_hash` (R-S6). Shared by `extract_secret` and
/// the swap-spend classification below so the hash comparison lives in
/// exactly one place.
fn extract_secret_from_tx(tx: &V2Transaction, expected_hash: &Hash256) -> Option<Vec<u8>> {
    tx.siacoin_inputs
        .iter()
        .flat_map(|input| input.satisfied_policy.preimages.iter())
        .find(|extracted_secret| Hash256(sha256(&extracted_secret.0).take()) == *expected_hash)
        .map(|secret| secret.0.to_vec())
}

/// What a spend search decided, as two reads are compared on it (CRD ch.51 R100 (S)): unspent, or the id of the
/// transaction that claimed or refunded the HTLC output.
#[derive(Debug, PartialEq, Eq)]
enum SpendDecision {
    Unspent,
    Claimed(String),
    Refunded(String),
}

fn spend_decision(found: &Option<FoundSwapTxSpend>) -> SpendDecision {
    let id = |tx: &TransactionEnum| match tx {
        TransactionEnum::SiaTransaction(tx) => tx.txid().to_string(),
        other => format!("{other:?}"),
    };
    match found {
        None => SpendDecision::Unspent,
        Some(FoundSwapTxSpend::Spent(tx)) => SpendDecision::Claimed(id(tx)),
        Some(FoundSwapTxSpend::Refunded(tx)) => SpendDecision::Refunded(id(tx)),
    }
}

/// A claim proves itself (CRD ch.51 R100): `classify_htlc_spend` calls a spend `Spent` only when it reveals a
/// secret that hashes to the swap's secret hash, which no node can fake, so one node's answer is enough. A refund
/// or "unspent" is a claim about the chain that needs the coin's policy.
fn spend_proves_itself(found: &Option<FoundSwapTxSpend>) -> bool { matches!(found, Some(FoundSwapTxSpend::Spent(_))) }

/// The secret hashes a spend policy commits to: every `Hash` leaf of it.
fn policy_hash_leaves<'a>(policy: &'a SpendPolicy, leaves: &mut Vec<&'a Hash256>) {
    match policy {
        SpendPolicy::Hash(hash) => leaves.push(hash),
        SpendPolicy::Threshold { of, .. } => of.iter().for_each(|inner| policy_hash_leaves(inner, leaves)),
        _ => (),
    }
}

/// Whether `spend` is a claim of the HTLC output `htlc_output_id` that proves itself (CRD ch.51 R100): it spends
/// that output, the spend policy it satisfies is the one `htlc_address` commits to, and it reveals a preimage whose
/// SHA-256 equals a hash in that policy. No node can fake such a spend, so one node's answer is enough; it needs no
/// secret hash from the caller, because the hash is the one the output's own address commits to.
pub(super) fn claim_proves_itself(
    spend: &V2Transaction,
    htlc_output_id: &SiacoinOutputId,
    htlc_address: &Address,
) -> bool {
    spend
        .siacoin_inputs
        .iter()
        .filter(|input| &input.parent.id == htlc_output_id)
        .any(|input| {
            let satisfied = &input.satisfied_policy;
            if &satisfied.policy.address() != htlc_address {
                return false;
            }
            let mut hashes = Vec::new();
            policy_hash_leaves(&satisfied.policy, &mut hashes);
            satisfied
                .preimages
                .iter()
                .any(|preimage| hashes.contains(&&Hash256(sha256(&preimage.0).take())))
        })
}

/// Locate, within an already-fetched event set, the event whose consumed
/// inputs spend `htlc_output_id`, and classify it as spent-via-secret
/// (R-H2) vs. refunded-via-timelock (R-H3) by whether the spending
/// transaction reveals the expected secret preimage -- the two paths share
/// the same public/private-key policy leaves (§20.6), so the preimage is
/// what actually distinguishes them, exactly as R-S6's secret extraction
/// already relies on. `Ok(None)` means genuinely unspent (the event set
/// carries no consumer of this output).
///
/// Pure: takes the event set as data, so it needs no walletd access and is
/// directly unit-testable against constructed events.
fn classify_htlc_spend(
    events: &[Event],
    htlc_output_id: &SiacoinOutputId,
    secret_hash: &Hash256,
) -> Option<FoundSwapTxSpend> {
    for event in events {
        let spend_tx = match &event.data {
            EventDataWrapper::V2Transaction(tx) => tx,
            _ => continue,
        };

        let spends_htlc_output = spend_tx
            .siacoin_inputs
            .iter()
            .any(|input| &input.parent.id == htlc_output_id);
        if !spends_htlc_output {
            continue;
        }

        let found_tx: TransactionEnum = SiaTransaction(spend_tx.clone()).into();
        return Some(match extract_secret_from_tx(spend_tx, secret_hash) {
            Some(_) => FoundSwapTxSpend::Spent(found_tx),
            None => FoundSwapTxSpend::Refunded(found_tx),
        });
    }
    None
}

// ── Ed25519 keys in the fixed-width swap field (CRD ch.51 R64) ───────

/// Width of the native ed25519 public key Sia signs with.
const ED25519_PUBKEY_LEN: usize = 32;

/// Place an ed25519 public key in the fixed-width swap field.
///
/// R64 dictates both halves of the convention: the 32 native bytes take the
/// field's *leading* positions and the final byte is zero. A leading pad or a
/// non-zero pad is not interoperable, so neither may be varied.
fn ed25519_pubkey_to_swap_field(pubkey: &PublicKey) -> [u8; SWAP_HTLC_PUBKEY_LEN] {
    let mut field = [0u8; SWAP_HTLC_PUBKEY_LEN];
    field[..ED25519_PUBKEY_LEN].copy_from_slice(&pubkey.to_bytes());
    field
}

/// Read an ed25519 public key back out of the fixed-width swap field.
///
/// R64's receive half: require exactly the bound width, then take the *leading*
/// 32 bytes as the native key. The final byte is ignored rather than checked —
/// the rule constrains what this node sends, not what it will accept.
fn ed25519_pubkey_from_swap_field(field: &[u8]) -> MmResult<PublicKey, HtlcPubkeyError> {
    if field.len() != SWAP_HTLC_PUBKEY_LEN {
        return MmError::err(HtlcPubkeyError::UnexpectedLength(field.len()));
    }
    PublicKey::from_bytes(&field[..ED25519_PUBKEY_LEN])
        .map_to_mm(|e| HtlcPubkeyError::NotOnCurve("ed25519", e.to_string()))
}

// ── SwapOps trait impl ───────────────────────────────────────────────

#[async_trait]
impl SwapOps for SiaCoin {
    fn send_taker_fee(&self, dex_fee: &DexFee, fee_addr: &[u8], uuid: &[u8]) -> super::TransactionFut {
        let coin = self.clone();
        let dex_fee = dex_fee.clone();
        let fee_addr = fee_addr.to_vec();
        let uuid = uuid.to_vec();
        let fut = async move {
            coin.new_send_taker_fee(&dex_fee, &uuid, &fee_addr)
                .await
                .map_err(|e| TransactionErr::Plain(e.to_string()))
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
    ) -> super::TransactionFut {
        let coin = self.clone();
        let maker_pub = maker_pub.to_vec();
        let taker_pub = taker_pub.to_vec();
        let secret_hash = secret_hash.to_vec();
        let fut = async move {
            coin.new_send_maker_payment(time_lock, &maker_pub, &taker_pub, &secret_hash, amount)
                .await
                .map_err(|e| TransactionErr::Plain(e.to_string()))
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
    ) -> super::TransactionFut {
        let coin = self.clone();
        let taker_pub = taker_pub.to_vec();
        let maker_pub = maker_pub.to_vec();
        let secret_hash = secret_hash.to_vec();
        let fut = async move {
            coin.new_send_taker_payment(time_lock, &taker_pub, &maker_pub, &secret_hash, amount)
                .await
                .map_err(|e| TransactionErr::Plain(e.to_string()))
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
    ) -> super::TransactionFut {
        let coin = self.clone();
        let taker_payment_tx = taker_payment_tx.to_vec();
        let taker_pub = taker_pub.to_vec();
        let secret = secret.to_vec();
        // Use secret to derive secret_hash for the HTLC
        let secret_hash_bytes: Vec<u8> = sha256(&secret).take().to_vec();
        let fut = async move {
            coin.new_send_maker_spends_taker_payment(
                &taker_payment_tx,
                time_lock,
                &taker_pub,
                &secret,
                &secret_hash_bytes,
            )
            .await
            .map_err(|e| TransactionErr::Plain(e.to_string()))
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
    ) -> super::TransactionFut {
        let coin = self.clone();
        let maker_payment_tx = maker_payment_tx.to_vec();
        let maker_pub = maker_pub.to_vec();
        let secret = secret.to_vec();
        let secret_hash_bytes: Vec<u8> = sha256(&secret).take().to_vec();
        let fut = async move {
            coin.new_send_taker_spends_maker_payment(
                &maker_payment_tx,
                time_lock,
                &maker_pub,
                &secret,
                &secret_hash_bytes,
            )
            .await
            .map_err(|e| TransactionErr::Plain(e.to_string()))
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
    ) -> super::TransactionFut {
        let coin = self.clone();
        let taker_payment_tx = taker_payment_tx.to_vec();
        let maker_pub = maker_pub.to_vec();
        let secret_hash = secret_hash.to_vec();
        let fut = async move {
            coin.send_refund_htlc(&taker_payment_tx, time_lock, &maker_pub, &secret_hash)
                .await
                .map_err(|e| TransactionErr::Plain(format!("taker refund: {}", e)))
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
    ) -> super::TransactionFut {
        let coin = self.clone();
        let maker_payment_tx = maker_payment_tx.to_vec();
        let taker_pub = taker_pub.to_vec();
        let secret_hash = secret_hash.to_vec();
        let fut = async move {
            coin.send_refund_htlc(&maker_payment_tx, time_lock, &taker_pub, &secret_hash)
                .await
                .map_err(|e| TransactionErr::Plain(format!("maker refund: {}", e)))
        };
        Box::new(fut.boxed().compat())
    }

    fn validate_fee(&self, args: ValidateFeeArgs<'_>) -> Box<dyn Future<Item = (), Error = String> + Send> {
        let coin = self.clone();
        // Extract everything we need from the borrowed args before moving into the future
        let fee_tx_bytes = args.fee_tx.tx_hex();
        let expected_sender = args.expected_sender.to_vec();
        let fee_addr = args.fee_addr.to_vec();
        let dex_fee = args.dex_fee.clone();
        let min_block_number = args.min_block_number;
        let uuid = args.uuid.to_vec();

        let fut = async move {
            // Re-parse the tx from bytes to reconstruct ValidateFeeArgs with proper lifetimes
            let tx_enum = coin
                .tx_enum_from_bytes(&fee_tx_bytes)
                .map_err(|e| format!("Failed to parse fee tx: {}", e))?;
            let validate_args = ValidateFeeArgs {
                fee_tx: &tx_enum,
                expected_sender: &expected_sender,
                fee_addr: &fee_addr,
                dex_fee: &dex_fee,
                min_block_number,
                uuid: &uuid,
            };
            coin.new_validate_fee_impl(validate_args)
                .await
                .map_err(|e| e.to_string())
        };
        Box::new(fut.boxed().compat())
    }

    fn validate_maker_payment(&self, input: ValidatePaymentInput) -> Box<dyn Future<Item = (), Error = String> + Send> {
        let coin = self.clone();
        // We are the taker here (validating the payment the maker sent us),
        // so the counterparty is the maker.
        let fut = async move {
            coin.validate_htlc_payment(input, ValidatingPaymentOwner::Maker)
                .await
                .map_err(|e| e.to_string())
        };
        Box::new(fut.boxed().compat())
    }

    fn validate_taker_payment(&self, input: ValidatePaymentInput) -> Box<dyn Future<Item = (), Error = String> + Send> {
        let coin = self.clone();
        // We are the maker here (validating the payment the taker sent us),
        // so the counterparty is the taker.
        let fut = async move {
            coin.validate_htlc_payment(input, ValidatingPaymentOwner::Taker)
                .await
                .map_err(|e| e.to_string())
        };
        Box::new(fut.boxed().compat())
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
        let coin = self.clone();
        let my_pub = my_pub.to_vec();
        let other_pub = other_pub.to_vec();
        let secret_hash = secret_hash.to_vec();
        let amount = BigDecimal::from(0); // amount not used in payment sent check
        let fut = async move {
            coin.new_check_if_my_payment_sent(time_lock, &my_pub, &other_pub, &secret_hash, search_from_block, amount)
                .await
                .map_err(|e| e.to_string())
        };
        Box::new(fut.boxed().compat())
    }

    async fn search_for_swap_tx_spend_my(
        &self,
        time_lock: u32,
        _my_pub: &[u8],
        other_pub: &[u8],
        secret_hash: &[u8],
        tx: &[u8],
        _search_from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
    ) -> Result<Option<FoundSwapTxSpend>, String> {
        // Every non-test caller of this trait method is `TakerSwap`/`MakerSwap`
        // `recover_funds` (grep confirms it -- neither is reached from the
        // normal swap FSM's own spend-detection/wait loop, which uses
        // `wait_for_confirmations`/`tx_details_from_event` instead). D3
        // (CRD ch.20 §20.10) closes the walk this depends on.
        self.sia_search_for_swap_tx_spend_my(time_lock, other_pub, secret_hash, tx)
            .await
            .map_err(|e| e.to_string())
    }

    async fn search_for_swap_tx_spend_other(
        &self,
        time_lock: u32,
        _my_pub: &[u8],
        other_pub: &[u8],
        secret_hash: &[u8],
        tx: &[u8],
        _search_from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
    ) -> Result<Option<FoundSwapTxSpend>, String> {
        // See `search_for_swap_tx_spend_my` above -- same reasoning, mirrored
        // for the counterparty-payment side of `recover_funds`.
        self.sia_search_for_swap_tx_spend_other(time_lock, other_pub, secret_hash, tx)
            .await
            .map_err(|e| e.to_string())
    }

    fn extract_secret(&self, secret_hash: &[u8], spend_tx: &[u8]) -> Result<Vec<u8>, String> {
        self.sia_extract_secret(secret_hash, spend_tx)
            .map_err(|e| e.to_string())
    }

    fn can_refund_htlc(&self, locktime: u64) -> Box<dyn Future<Item = CanRefundHtlc, Error = String> + Send + '_> {
        let fut = async move { self.sia_can_refund_htlc(locktime).await.map_err(|e| e.to_string()) };
        Box::new(fut.boxed().compat())
    }

    fn negotiate_swap_contract_addr(
        &self,
        _other_side_address: Option<&[u8]>,
    ) -> Result<Option<BytesJson>, MmError<NegotiateSwapContractAddrErr>> {
        Ok(None)
    }

    fn get_htlc_key_pair(&self) -> Option<KeyPair> {
        // Sia uses ed25519 keys, not secp256k1 KeyPair. Return None.
        None
    }

    /// Sia signs with ed25519, so the key it puts on the negotiation wire is
    /// its own 32-byte key padded into the 33-byte field by the convention of
    /// ch.51 R64 — never the node's secp256k1 key, which Sia cannot sign with,
    /// and which is why `node_secp_pubkey` is ignored here.
    fn derive_htlc_pubkey(&self, _node_secp_pubkey: &Public) -> MmResult<[u8; SWAP_HTLC_PUBKEY_LEN], HtlcPubkeyError> {
        let keypair = self
            .my_keypair()
            .map_to_mm(|e| HtlcPubkeyError::NotAvailable(e.to_string()))?;
        Ok(ed25519_pubkey_to_swap_field(&keypair.public()))
    }

    /// The counterparty's key for a Sia HTLC is an ed25519 key in the same
    /// 33-byte field (R63, R64): exactly the bound width, with the leading 32
    /// bytes a well-formed curve point. This is the check the Sia swap
    /// transactions would otherwise fail much later, mid-swap.
    fn validate_other_pubkey(&self, raw_pubkey: &[u8]) -> MmResult<(), HtlcPubkeyError> {
        ed25519_pubkey_from_swap_field(raw_pubkey).map(|_| ())
    }
}

#[cfg(test)]
mod swap_field_tests {
    use super::*;

    fn test_pubkey() -> PublicKey {
        SiaKeypair::from_private_bytes(&[1u8; 32])
            .expect("32 bytes is a valid ed25519 secret key")
            .public()
    }

    /// ch.51 R64 send half: native bytes in the leading positions, final byte
    /// zero. Both are dictated by the deployed format, so both are asserted.
    #[test]
    fn ed25519_key_occupies_the_field_by_a_trailing_zero_pad() {
        let pubkey = test_pubkey();
        let field = ed25519_pubkey_to_swap_field(&pubkey);

        assert_eq!(field.len(), SWAP_HTLC_PUBKEY_LEN);
        assert_eq!(&field[..ED25519_PUBKEY_LEN], pubkey.as_bytes());
        assert_eq!(field[SWAP_HTLC_PUBKEY_LEN - 1], 0);
    }

    /// R64 receive half: the same field validates, and the key recovered from
    /// it is the one that was sent.
    #[test]
    fn ed25519_swap_field_round_trips() {
        let pubkey = test_pubkey();
        let field = ed25519_pubkey_to_swap_field(&pubkey);

        assert_eq!(ed25519_pubkey_from_swap_field(&field).unwrap(), pubkey);
    }

    /// R63: exactly 33 bytes. A bare 32-byte ed25519 key is the honest short
    /// case and is still refused — the field width does not follow the curve.
    #[test]
    fn only_the_bound_width_is_accepted() {
        let pubkey = test_pubkey();

        for field in [pubkey.as_bytes(), &[0u8; SWAP_HTLC_PUBKEY_LEN + 1][..], &[][..]] {
            assert_eq!(
                ed25519_pubkey_from_swap_field(field).unwrap_err().into_inner(),
                HtlcPubkeyError::UnexpectedLength(field.len())
            );
        }
    }

    /// A field of the right width whose leading bytes are not a curve point is
    /// rejected, rather than carried into a swap transaction that cannot be
    /// satisfied.
    #[test]
    fn a_bound_width_field_that_is_not_a_curve_point_is_rejected() {
        // y = 2 has no matching x on the Edwards curve, so this encodes no point.
        let mut field = [0u8; SWAP_HTLC_PUBKEY_LEN];
        field[0] = 2;

        assert!(matches!(
            ed25519_pubkey_from_swap_field(&field).unwrap_err().into_inner(),
            HtlcPubkeyError::NotOnCurve("ed25519", _)
        ));
    }

    /// A swap negotiated before Sia pairs required the 32-byte SHA256 secret
    /// hash (select_secret_hash_algo, ch.51 R71/R72) has the legacy 20-byte
    /// RIPEMD160(SHA256(_)) one instead. That must be reported as the
    /// structural fact it is -- "this swap can't be checked against Sia" --
    /// not `Hash256Error`'s generic "invalid slice length", which said
    /// nothing about *why* a 20-byte hash showed up (observed verbatim in a
    /// real recover_funds_of_swap failure).
    #[test]
    fn check_if_my_payment_sent_args_reports_a_legacy_20_byte_secret_hash_by_name() {
        let other_pub = ed25519_pubkey_to_swap_field(&test_pubkey());
        let legacy_secret_hash = [0u8; 20];

        let err = SiaCheckIfMyPaymentSentArgs::try_from_positional(
            1787000000,
            &other_pub,
            &legacy_secret_hash,
            BigDecimal::from(1),
        )
        .err()
        .unwrap();

        assert!(matches!(err, SiaCheckIfMyPaymentSentArgsError::WrongSecretHashLength {
            actual: 20
        }));
    }
}

/// CRD ch.39 R39.11.5A.8, D39.11.5A.1 (a) (T39.11.5A.5): what the already-sent query answers for a failed and for
/// an empty answer of the walletd's address-events request.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod already_sent_query_tests {
    use super::*;
    use crate::answer_stub::{http_response, serve};
    use std::sync::atomic::AtomicUsize;

    const TIP: &str = r#"{"height":10,"id":"0000000000000000000000000000000000000000000000000000000000000000"}"#;

    /// A coin of a walletd that answers the connection probe (the first request) with a consensus tip and every
    /// later request with `reply`.
    fn coin_after_ping(reply: Vec<u8>) -> SiaCoin {
        let requests = Arc::new(AtomicUsize::new(0));
        let port = serve(move |_| {
            if requests.fetch_add(1, AtomicOrdering::SeqCst) == 0 {
                http_response("200 OK", "application/json", TIP.as_bytes())
            } else {
                reply.clone()
            }
        });
        let client_conf: SiaClientConf = serde_json::from_value(serde_json::json!({
            "server_url": format!("http://127.0.0.1:{port}/"),
            "password": "pw",
            "timeout": 5,
        }))
        .unwrap();
        let client = common::block_on(SiaClient::new(client_conf)).unwrap();
        let key_pair = SiaKeypair::from_private_bytes(&[2u8; 32]).unwrap();
        let address = key_pair.public().address();
        SiaCoin {
            conf: SiaCoinConf {
                ticker: "SC".to_owned(),
                required_confirmations: 1,
            },
            priv_key_policy: PrivKeyPolicy::KeyPair(key_pair).into(),
            client: Arc::new(client),
            history_sync_state: Mutex::new(HistorySyncState::NotEnabled).into(),
            required_confirmations: AtomicU64::new(1).into(),
            fee_address: address.clone(),
            derivation_method: Arc::new(DerivationMethod::Iguana(address)),
            swap_evidence: crate::SwapEvidencePolicy::NotAdopted,
            swap_evidence_node_name: "127.0.0.1".to_owned(),
        }
    }

    /// The answer of the query as the swap gets it, through [`SwapOps::check_if_my_payment_sent`].
    fn query(coin: &SiaCoin) -> Result<Option<TransactionEnum>, String> {
        let other_pub = ed25519_pubkey_to_swap_field(&SiaKeypair::from_private_bytes(&[1u8; 32]).unwrap().public());
        let my_pub = [0u8; SWAP_HTLC_PUBKEY_LEN];
        let secret_hash = [7u8; 32];
        common::block_on(
            coin.check_if_my_payment_sent(1_787_000_000, &my_pub, &other_pub, &secret_hash, 0, &None)
                .compat(),
        )
    }

    #[test]
    fn t39_11_5a_5_sia_failed_events_request_is_undetermined() {
        let coin = coin_after_ping(http_response(
            "500 Internal Server Error",
            "text/plain",
            b"database is locked\n",
        ));
        let err = query(&coin).unwrap_err();
        assert!(err.contains("Undetermined whether the payment was sent"), "{err}");
        assert!(
            err.contains("the request for the events of the HTLC address failed"),
            "{err}"
        );
        // The request failure is carried.
        assert!(err.contains("500"), "{err}");
        assert!(err.contains("database is locked"), "{err}");
    }

    /// R39.11.5A.8: an empty event list still answers "not sent".
    #[test]
    fn r39_11_5a_8_sia_empty_event_list_is_not_sent() {
        let coin = coin_after_ping(http_response("200 OK", "application/json", b"[]"));
        assert_eq!(query(&coin).unwrap(), None);
    }
}

#[cfg(test)]
mod swap_spend_search_tests {
    //! Regression surface for `classify_htlc_spend` (CRD ch.20 §20.10 D3).
    //!
    //! Follows `siacoin_history.rs`'s test style: events are constructed from
    //! the JSON shape walletd actually returns, and the walk itself is pure
    //! (it takes an already-fetched event set), so none of these need a
    //! walletd instance.

    use super::*;

    /// Output id the constructed HTLC payment funded -- 64 hex chars, a
    /// well-formed but otherwise arbitrary `Hash256`.
    const HTLC_OUTPUT_ID: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    /// A different output id, used to prove an event that spends *something
    /// else* is not mistaken for the HTLC spend.
    const OTHER_OUTPUT_ID: &str = "2222222222222222222222222222222222222222222222222222222222222222";
    const ADDR: &str = "c34caa97740668de2bbdb7174572ed64c861342bf27e80313cbfa02e9251f52e30aad3892533";
    /// Placeholder `pk` policy/signature -- their content is never inspected
    /// by `classify_htlc_spend` (it distinguishes success vs. refund purely
    /// by the revealed preimage, per R-S6), only their shape needs to parse.
    const PLACEHOLDER_PUBKEY: &str = "a729be53dae7b0ed812f2a123ce93556014bbad8516ba6b1b496a112b46bbd97";
    const PLACEHOLDER_SIG: &str = "160e79ac52e0eaab5e92bd1675604a94b56ec58fdd0be3f3a842a4ece07d794f7ee1e8cc8f29b596bf71b2dc594df53347b9a4bcbec46fe09244ce6d3f6a6708";
    const EVENT_ID: &str = "0f088eddda5320f8453a55349063abe43ba5b282631d5d2b9e684548f083055a";
    const BLOCK_ID: &str = "b37a5387883748f73c1475ca85c8f3200eef09126c44824d0f44574109dabedc";

    fn htlc_output_id() -> SiacoinOutputId { SiacoinOutputId(Hash256::from_str(HTLC_OUTPUT_ID).expect("valid hash")) }

    /// A v2Transaction event whose single input consumes `consumed_output_id`,
    /// optionally revealing `preimage` (present for a success-path spend,
    /// absent for a refund-path spend).
    fn spend_event(consumed_output_id: &str, preimage: Option<[u8; 32]>) -> Event {
        let mut satisfied_policy = json!({
            "policy": { "type": "pk", "policy": format!("ed25519:{}", PLACEHOLDER_PUBKEY) },
            "signatures": [PLACEHOLDER_SIG],
        });
        if let Some(secret) = preimage {
            satisfied_policy["preimages"] = json!([hex::encode(secret)]);
        }

        let json = json!({
            "id": EVENT_ID,
            "index": { "height": 42, "id": BLOCK_ID },
            "confirmations": 7,
            "timestamp": "2024-05-01T12:00:00Z",
            "maturityHeight": 0,
            "type": "v2Transaction",
            "data": {
                "siacoinInputs": [{
                    "parent": {
                        "id": consumed_output_id,
                        "stateElement": { "leafIndex": 3, "merkleProof": [] },
                        "siacoinOutput": { "value": "1000000000000000000000000", "address": ADDR },
                        "maturityHeight": 0,
                    },
                    "satisfiedPolicy": satisfied_policy,
                }],
                "siacoinOutputs": [{ "value": "999990000000000000000000", "address": ADDR }],
                "minerFee": "10000000000000000000",
            },
        });
        serde_json::from_value(json).expect("valid walletd event")
    }

    /// R-H2: a spend revealing the secret preimage is the success path.
    #[test]
    fn a_spend_revealing_the_preimage_is_classified_as_spent() {
        let secret = [7u8; 32];
        let secret_hash = Hash256(sha256(&secret).take());
        let event = spend_event(HTLC_OUTPUT_ID, Some(secret));

        let found = classify_htlc_spend(&[event], &htlc_output_id(), &secret_hash);

        assert!(matches!(found, Some(FoundSwapTxSpend::Spent(_))));
    }

    /// R-H3: a spend of the same output with no revealed preimage is the
    /// refund path.
    #[test]
    fn a_spend_without_the_preimage_is_classified_as_refunded() {
        let secret_hash = Hash256(sha256(&[7u8; 32]).take());
        let event = spend_event(HTLC_OUTPUT_ID, None);

        let found = classify_htlc_spend(&[event], &htlc_output_id(), &secret_hash);

        assert!(matches!(found, Some(FoundSwapTxSpend::Refunded(_))));
    }

    /// An event that spends a *different* output is not a match, regardless
    /// of whether it reveals a preimage.
    #[test]
    fn an_event_consuming_a_different_output_is_not_a_match() {
        let secret_hash = Hash256(sha256(&[7u8; 32]).take());
        let event = spend_event(OTHER_OUTPUT_ID, Some([7u8; 32]));

        assert_eq!(classify_htlc_spend(&[event], &htlc_output_id(), &secret_hash), None);
    }

    /// No event in the set spends the HTLC output at all: genuinely unspent,
    /// the one case "not found" was always the correct answer for (§20.10 D3).
    #[test]
    fn no_matching_event_is_not_found() {
        let secret_hash = Hash256(sha256(&[7u8; 32]).take());
        assert_eq!(classify_htlc_spend(&[], &htlc_output_id(), &secret_hash), None);
    }
}

/// CRD ch.51 R100 / T42 (Sia): the swap reads of the coin's one walletd node, judged by the coin's policy.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod swap_evidence_read_tests {
    use super::*;
    use crate::answer_stub::{http_response, serve};
    use crate::siacoin::siacoin_helpers::sia_swap_evidence_policy;
    use crate::swap_evidence::node_name;
    use crate::swap_evidence::{Agreement, SwapEvidencePolicy, UndeterminedReason};
    use std::sync::atomic::AtomicUsize;

    const TIP: &str = r#"{"height":10,"id":"0000000000000000000000000000000000000000000000000000000000000000"}"#;
    const ADDR: &str = "c34caa97740668de2bbdb7174572ed64c861342bf27e80313cbfa02e9251f52e30aad3892533";
    const PLACEHOLDER_PUBKEY: &str = "a729be53dae7b0ed812f2a123ce93556014bbad8516ba6b1b496a112b46bbd97";
    const PLACEHOLDER_SIG: &str = "160e79ac52e0eaab5e92bd1675604a94b56ec58fdd0be3f3a842a4ece07d794f7ee1e8cc8f29b596bf71b2dc594df53347b9a4bcbec46fe09244ce6d3f6a6708";
    /// The median of the chain times `tipstate` answers (2026-01-01T00:00:05Z).
    const MEDIAN: u64 = 1_767_225_605;
    const LOCK: u64 = MEDIAN - 100;

    /// The chain-time answer of a walletd whose last 11 block times have the median [`MEDIAN`].
    fn tipstate() -> Vec<u8> {
        let times: Vec<String> = (0..11).map(|i| format!("2026-01-01T00:00:{i:02}Z")).collect();
        let body = json!({"index": {"height": 10, "id": "00".repeat(32)}, "prevTimestamps": times});
        http_response("200 OK", "application/json", body.to_string().as_bytes())
    }

    /// The events a walletd answers for the HTLC address: one v2 transaction that consumes `output_id`, revealing
    /// `preimage` when the spend is a claim.
    fn spend_events(output_id: &str, preimage: Option<[u8; 32]>) -> Vec<u8> {
        http_response(
            "200 OK",
            "application/json",
            spend_events_json(output_id, preimage).to_string().as_bytes(),
        )
    }

    fn spend_events_json(output_id: &str, preimage: Option<[u8; 32]>) -> serde_json::Value {
        let mut satisfied_policy = json!({
            "policy": { "type": "pk", "policy": format!("ed25519:{PLACEHOLDER_PUBKEY}") },
            "signatures": [PLACEHOLDER_SIG],
        });
        if let Some(secret) = preimage {
            satisfied_policy["preimages"] = json!([hex::encode(secret)]);
        }
        json!([{
            "id": "0f088eddda5320f8453a55349063abe43ba5b282631d5d2b9e684548f083055a",
            "index": { "height": 42, "id": "b37a5387883748f73c1475ca85c8f3200eef09126c44824d0f44574109dabedc" },
            "confirmations": 7,
            "timestamp": "2024-05-01T12:00:00Z",
            "maturityHeight": 0,
            "type": "v2Transaction",
            "data": {
                "siacoinInputs": [{
                    "parent": {
                        "id": output_id,
                        "stateElement": { "leafIndex": 3, "merkleProof": [] },
                        "siacoinOutput": { "value": "1000000000000000000000000", "address": ADDR },
                        "maturityHeight": 0,
                    },
                    "satisfiedPolicy": satisfied_policy,
                }],
                "siacoinOutputs": [{ "value": "999990000000000000000000", "address": ADDR }],
                "minerFee": "10000000000000000000",
            },
        }])
    }

    /// A coin of a walletd that answers the connection probe (the first request) with a consensus tip and every
    /// later request with `reply(request)`; returns the coin and the number of requests the walletd saw.
    fn coin_with(
        policy: SwapEvidencePolicy,
        reply: impl Fn(&str) -> Vec<u8> + Send + 'static,
    ) -> (SiaCoin, Arc<AtomicUsize>) {
        let requests = Arc::new(AtomicUsize::new(0));
        let seen = requests.clone();
        let port = serve(move |request| {
            if requests.fetch_add(1, AtomicOrdering::SeqCst) == 0 {
                http_response("200 OK", "application/json", TIP.as_bytes())
            } else {
                reply(request)
            }
        });
        let server_url = format!("http://127.0.0.1:{port}/");
        let client_conf: SiaClientConf = serde_json::from_value(serde_json::json!({
            "server_url": server_url,
            "password": "pw",
            "timeout": 5,
        }))
        .unwrap();
        let client = common::block_on(SiaClient::new(client_conf)).unwrap();
        let key_pair = SiaKeypair::from_private_bytes(&[2u8; 32]).unwrap();
        let address = key_pair.public().address();
        let coin = SiaCoin {
            conf: SiaCoinConf {
                ticker: "SC".to_owned(),
                required_confirmations: 1,
            },
            priv_key_policy: PrivKeyPolicy::KeyPair(key_pair).into(),
            client: Arc::new(client),
            history_sync_state: Mutex::new(HistorySyncState::NotEnabled).into(),
            required_confirmations: AtomicU64::new(1).into(),
            fee_address: address.clone(),
            derivation_method: Arc::new(DerivationMethod::Iguana(address)),
            swap_evidence: policy,
            swap_evidence_node_name: node_name(&server_url),
        };
        (coin, seen)
    }

    /// The `Trusted` policy of a coin the field named: what activation computes for the coin's one `server_url`.
    fn trusted(coin_url_port: &str) -> SwapEvidencePolicy {
        sia_swap_evidence_policy("SC", Some(coin_url_port), &format!("http://{coin_url_port}/")).unwrap()
    }

    /// A coin whose walletd answers every request after the probe with `reply`, trusted by the field.
    fn trusted_coin(reply: impl Fn(&str) -> Vec<u8> + Send + 'static) -> (SiaCoin, Arc<AtomicUsize>) {
        // The port is not known before the stub is served: the policy is set once the coin is built.
        let (mut coin, seen) = coin_with(SwapEvidencePolicy::NotAdopted, reply);
        coin.swap_evidence = trusted(&coin.swap_evidence_node_name);
        (coin, seen)
    }

    /// The readiness check before a swap starts (maintainer decision of 2026-11-25/26 under CRD ch.51 R100): the one
    /// walletd node the field trusts must answer; without the field the coin is never ready.
    #[test]
    fn sia_readiness_follows_its_walletd_node() {
        let (mut up, seen) = trusted_coin(|_| http_response("200 OK", "application/json", TIP.as_bytes()));
        up.conf.ticker = "SCRDYUP".to_owned();
        assert_eq!(common::block_on(MmCoin::swap_evidence_ready(&up)), Ok(()));
        let asked = seen.load(AtomicOrdering::SeqCst);
        // The probe's answer counts for the window: nothing extra is sent.
        assert_eq!(common::block_on(MmCoin::swap_evidence_ready(&up)), Ok(()));
        assert_eq!(seen.load(AtomicOrdering::SeqCst), asked);

        let (mut down, _) = trusted_coin(|_| http_response("500 Internal Server Error", "text/plain", b"down"));
        down.conf.ticker = "SCRDYDOWN".to_owned();
        assert_eq!(
            common::block_on(MmCoin::swap_evidence_ready(&down)),
            Err("SCRDYDOWN: swap evidence not available (the trusted node is not answering)".to_owned())
        );

        let (mut without, _) = coin_with(SwapEvidencePolicy::Insufficient { hosts: 1 }, |_| tipstate());
        without.conf.ticker = "SCRDYNONE".to_owned();
        assert_eq!(
            common::block_on(MmCoin::swap_evidence_ready(&without)),
            Err("SCRDYNONE: swap evidence not available (fewer than two nodes listed)".to_owned())
        );
    }

    /// `Ok(None)`: the gate is open; `Ok(Some(seconds))`: closed for that long.
    fn can_refund(coin: &SiaCoin, lock: u64) -> Result<Option<u64>, String> {
        common::block_on(coin.can_refund_htlc(lock).compat()).map(|gate| match gate {
            CanRefundHtlc::CanRefundNow => None,
            CanRefundHtlc::HaveToWait(seconds) => Some(seconds),
        })
    }

    fn payment_sent(coin: &SiaCoin) -> Result<Option<TransactionEnum>, String> {
        let other_pub = ed25519_pubkey_to_swap_field(&SiaKeypair::from_private_bytes(&[1u8; 32]).unwrap().public());
        let my_pub = [0u8; SWAP_HTLC_PUBKEY_LEN];
        common::block_on(
            coin.check_if_my_payment_sent(LOCK as u32, &my_pub, &other_pub, &[7u8; 32], 0, &None)
                .compat(),
        )
    }

    /// The HTLC payment the spend search looks at, its spend's secret and the success and refund keys.
    fn search(
        coin: &SiaCoin,
        secret_hash: [u8; 32],
        tx: &SiaTransaction,
    ) -> Result<Option<FoundSwapTxSpend>, SiaCoinSearchSwapTxSpendError> {
        let theirs = SiaKeypair::from_private_bytes(&[1u8; 32]).unwrap().public();
        common::block_on(coin.sia_search_for_swap_tx_spend(
            LOCK as u32,
            &secret_hash,
            &serde_json::to_vec(tx).unwrap(),
            theirs,
            coin.my_keypair().unwrap().public(),
        ))
    }

    fn payment() -> (SiaTransaction, String) {
        let tx = SiaTransaction(V2Transaction::default());
        let output_id = SiacoinOutputId::new(tx.txid(), HTLC_VOUT_INDEX);
        (tx, output_id.to_string())
    }

    /// The field equal to the `server_url` makes the coin `Trusted`; the node's chain time decides the refund gate.
    #[test]
    fn t42_sia_trusted_node_chain_time_decides_the_refund_gate() {
        let (coin, _) = trusted_coin(|_| tipstate());
        assert!(matches!(
            MmCoin::swap_evidence_policy(&coin),
            SwapEvidencePolicy::Trusted { .. }
        ));
        // the chain time is past the lock: the gate is open; before the lock: closed
        assert_eq!(can_refund(&coin, MEDIAN - 100), Ok(None));
        assert_eq!(can_refund(&coin, MEDIAN + 100), Ok(Some(100)));
    }

    /// The node failing leaves P, S and T undetermined: no refund decision, no "not sent", no "not spent".
    #[test]
    fn t42_sia_trusted_node_failing_leaves_every_read_undetermined() {
        let failing = |_: &str| http_response("500 Internal Server Error", "text/plain", b"database is locked\n");
        let (coin, _) = trusted_coin(failing);
        let err = can_refund(&coin, LOCK).unwrap_err();
        assert!(err.contains("the trusted node did not answer"), "{err}");
        assert!(err.contains("127.0.0.1:"), "the node is named: {err}");
        assert!(!err.contains("pw"), "{err}");
        assert!(crate::swap_evidence::is_undetermined(&err), "{err}");
        let err = payment_sent(&coin).unwrap_err();
        assert!(err.contains("Undetermined whether the payment was sent"), "{err}");
        assert!(crate::swap_evidence::is_undetermined(&err), "{err}");
        assert!(err.contains("the trusted node did not answer"), "{err}");
        let (tx, _) = payment();
        let err = search(&coin, [7u8; 32], &tx).unwrap_err();
        assert!(err.to_string().contains("the trusted node did not answer"), "{err}");
        assert!(crate::swap_evidence::is_undetermined(&err.to_string()), "{err}");
    }

    /// A node that answers "no events" is "not sent" and "unspent"; those are answers, not silence.
    #[test]
    fn t42_sia_trusted_node_answering_empty_is_not_sent_and_unspent() {
        let (coin, _) = trusted_coin(|_| http_response("200 OK", "application/json", b"[]"));
        assert_eq!(payment_sent(&coin).unwrap(), None);
        let (tx, _) = payment();
        assert!(search(&coin, [7u8; 32], &tx).unwrap().is_none());
    }

    /// A claim whose secret hashes to the swap's secret hash is taken on the one answer; a spend that does not
    /// reveal it is a refund.
    #[test]
    fn t42_sia_claim_with_the_matching_secret_is_a_claim_and_other_spends_are_refunds() {
        let secret = [7u8; 32];
        let secret_hash = sha256(&secret).take();
        let (tx, output_id) = payment();
        let claim = spend_events(&output_id, Some(secret));
        let (coin, _) = trusted_coin(move |_| claim.clone());
        assert!(matches!(
            search(&coin, secret_hash, &tx).unwrap(),
            Some(FoundSwapTxSpend::Spent(_))
        ));
        // a secret of another hash: the same spend is not a claim of this swap
        let (coin, _) = trusted_coin({
            let claim = spend_events(&output_id, Some([8u8; 32]));
            move |_| claim.clone()
        });
        assert!(matches!(
            search(&coin, secret_hash, &tx).unwrap(),
            Some(FoundSwapTxSpend::Refunded(_))
        ));
    }

    /// The claim proves itself and a refund does not: with two nodes of which one is silent, the claim stands on
    /// the one answer and the refund stays undetermined. (Sia has one node; this is the helper's behavior the
    /// read is built on, with the coin's own classification.)
    #[test]
    fn t42_sia_only_a_matching_claim_stands_on_one_answer() {
        let secret = [7u8; 32];
        let secret_hash = Hash256(sha256(&secret).take());
        let (_, output_id) = payment();
        let output_id = SiacoinOutputId(Hash256::from_str(&output_id).unwrap());
        let policy = SwapEvidencePolicy::TwoNodes {
            hosts: vec!["a.test".to_owned(), "b.test".to_owned()],
        };
        let nodes = ["a.test:9980".to_owned(), "b.test:9980".to_owned()];
        let one_answer = |preimage: Option<[u8; 32]>| {
            let events: Vec<Event> =
                serde_json::from_value(spend_events_json(&output_id.to_string(), preimage)).unwrap();
            common::block_on(crate::swap_evidence::agree(
                &policy,
                &nodes,
                |node| node.clone(),
                |node| {
                    let answer = if node.starts_with("a.") {
                        Ok(classify_htlc_spend(&events, &output_id, &secret_hash))
                    } else {
                        Err("no answer".to_owned())
                    };
                    async move { answer }
                },
                spend_decision,
                spend_proves_itself,
            ))
        };
        assert!(matches!(one_answer(Some(secret)), Agreement::Agreed {
            answer: Some(FoundSwapTxSpend::Spent(_)),
            ..
        }));
        assert!(matches!(
            one_answer(Some([8u8; 32])),
            Agreement::Undetermined(u) if u.reason == UndeterminedReason::TooFewAnswers
        ));
    }

    /// No field with adoption on: the coin is `Insufficient`, refuses with R0's text, asks its node nothing, and
    /// every read is undetermined with the fix named.
    #[test]
    fn t42_sia_without_the_field_the_coin_is_insufficient_and_waits() {
        assert!(crate::swap_evidence::family_adopted(
            crate::swap_evidence::SwapFamily::Sia
        ));
        let (coin, _) = coin_with(SwapEvidencePolicy::Insufficient { hosts: 1 }, |_| tipstate());
        assert_eq!(
            MmCoin::swap_evidence_refusal(&coin).as_deref(),
            Some("SC has fewer than two nodes for swap evidence; add a second node or set swap_evidence_node")
        );
        let err = can_refund(&coin, LOCK).unwrap_err();
        assert!(err.contains("swap_evidence_node"), "{err}");
        assert!(err.contains("127.0.0.1:"), "{err}");
        assert!(payment_sent(&coin).unwrap_err().contains("swap_evidence_node"));
        // The node answered the chain time past the lock, and the refund gate is still undetermined above: under
        // `Insufficient` the node is asked only for an answer that proves itself (R100), and a chain time does not.
    }

    /// The HTLC payment of a swap, and the answer of a walletd whose mempool holds a spend of it: a claim revealing
    /// `preimage` (success branch) or a refund (no preimage).
    fn htlc_payment_and_mempool_spend(secret: [u8; 32], preimage: Option<[u8; 32]>) -> (SiaTransaction, Vec<u8>) {
        let success = SiaKeypair::from_private_bytes(&[1u8; 32]).unwrap().public();
        let refund = SiaKeypair::from_private_bytes(&[2u8; 32]).unwrap().public();
        let secret_hash = Hash256(sha256(&secret).take());
        let htlc_address = SpendPolicy::atomic_swap(&success, &refund, LOCK, &secret_hash).address();
        let payment = SiaTransaction(V2Transaction {
            siacoin_outputs: vec![SiacoinOutput {
                value: Currency(1_000),
                address: htlc_address.clone(),
            }],
            ..Default::default()
        });
        let output_id = SiacoinOutputId::new(payment.txid(), HTLC_VOUT_INDEX);
        let policy = match preimage {
            Some(_) => SpendPolicy::atomic_swap_success(&success, &refund, LOCK, &secret_hash),
            None => SpendPolicy::atomic_swap_refund(&success, &refund, LOCK, &secret_hash),
        };
        let mut satisfied_policy = json!({ "policy": policy, "signatures": [PLACEHOLDER_SIG] });
        if let Some(preimage) = preimage {
            satisfied_policy["preimages"] = json!([hex::encode(preimage)]);
        }
        let spend: V2Transaction = serde_json::from_value(json!({
            "siacoinInputs": [{
                "parent": {
                    "id": output_id.to_string(),
                    "stateElement": { "leafIndex": 3, "merkleProof": [] },
                    "siacoinOutput": { "value": "1000", "address": htlc_address.to_string() },
                    "maturityHeight": 0,
                },
                "satisfiedPolicy": satisfied_policy,
            }],
            "siacoinOutputs": [{ "value": "990", "address": ADDR }],
            "minerFee": "10",
        }))
        .unwrap();
        let body = json!({ "transactions": [], "v2transactions": [spend] });
        (
            payment,
            http_response("200 OK", "application/json", body.to_string().as_bytes()),
        )
    }

    fn wait_for_spend(coin: &SiaCoin, payment: &SiaTransaction) -> Result<TransactionEnum, String> {
        // `wait_until` is now: the wait reads once and then times out, so a spend that is not returned is shown.
        let wait_until = now_ms() / 1000;
        common::block_on(
            coin.wait_for_tx_spend(&serde_json::to_vec(payment).unwrap(), wait_until, 0, &None)
                .compat(),
        )
        .map_err(|e| format!("{e:?}"))
    }

    /// R100 / T42: under `Insufficient` (no field) the wait still returns a claim that reveals a preimage of the
    /// hash its HTLC address commits to: the taker learns the secret. A refund is not returned: it stays under the
    /// policy and the wait times out.
    #[test]
    fn t42_sia_insufficient_wait_returns_a_proven_claim_and_not_a_refund() {
        let secret = [7u8; 32];
        let (payment, reply) = htlc_payment_and_mempool_spend(secret, Some(secret));
        let (coin, _) = coin_with(SwapEvidencePolicy::Insufficient { hosts: 1 }, move |_| reply.clone());
        let spend = wait_for_spend(&coin, &payment).expect("a proven claim is returned");
        let TransactionEnum::SiaTransaction(spend) = spend else {
            panic!("not a Sia transaction")
        };
        assert!(extract_secret_from_tx(&spend.0, &Hash256(sha256(&secret).take())).is_some());

        let (payment, reply) = htlc_payment_and_mempool_spend(secret, None);
        let (coin, _) = coin_with(SwapEvidencePolicy::Insufficient { hosts: 1 }, move |_| reply.clone());
        let err = wait_for_spend(&coin, &payment).unwrap_err();
        assert!(
            err.contains("Timed out waiting for spend"),
            "a refund was returned: {err}"
        );
    }

    /// A spend that reveals a preimage of another hash does not prove itself, whatever policy it claims to satisfy.
    #[test]
    fn t42_sia_a_preimage_of_another_hash_does_not_prove_a_claim() {
        let secret = [7u8; 32];
        let (payment, reply) = htlc_payment_and_mempool_spend(secret, Some([8u8; 32]));
        let (coin, _) = coin_with(SwapEvidencePolicy::Insufficient { hosts: 1 }, move |_| reply.clone());
        let err = wait_for_spend(&coin, &payment).unwrap_err();
        assert!(err.contains("Timed out waiting for spend"), "{err}");
    }

    /// The library behavior `claim_proves_itself` relies on: `address()` of an HTLC policy with either branch
    /// replaced by its opaque form (as a spend reveals it) equals `address()` of the full policy.
    #[test]
    fn t42_sia_htlc_address_is_unchanged_with_either_branch_opaque() {
        let success = SiaKeypair::from_private_bytes(&[1u8; 32]).unwrap().public();
        let refund = SiaKeypair::from_private_bytes(&[2u8; 32]).unwrap().public();
        let secret_hash = Hash256(sha256(&[7u8; 32]).take());
        let full = SpendPolicy::atomic_swap(&success, &refund, LOCK, &secret_hash);
        let SpendPolicy::Threshold { n, of } = full.clone() else {
            panic!("an HTLC policy is a threshold")
        };
        // (a) the refund branch opaque
        let refund_opaque = SpendPolicy::Threshold {
            n,
            of: vec![of[0].clone(), SpendPolicy::opaque(&of[1])],
        };
        assert_eq!(refund_opaque.address(), full.address());
        // (b) the claim branch opaque
        let claim_opaque = SpendPolicy::Threshold {
            n,
            of: vec![SpendPolicy::opaque(&of[0]), of[1].clone()],
        };
        assert_eq!(claim_opaque.address(), full.address());
    }

    /// The `swap_evidence_node` field against the one `server_url` under the port rule (a port is compared only when
    /// both sides have one; an `http(s)` URL without a port has its scheme's default): explicit port, no port, and
    /// the field written with or without the default port.
    #[test]
    fn t42_sia_field_match_follows_the_port_rule() {
        let trusted = |field: &str, url: &str| {
            matches!(
                sia_swap_evidence_policy("SC", Some(field), url),
                Ok(SwapEvidencePolicy::Trusted { .. })
            )
        };
        // explicit port: a field without a port or with the same port matches, another port does not
        let url = "http://walletd.example:9980/";
        assert!(trusted("walletd.example", url));
        assert!(trusted("WALLETD.example:9980", url));
        assert!(!trusted("walletd.example:9981", url));
        assert!(!trusted("walletd.example:443", url));
        // https without a port: the default port is 443
        let url = "https://walletd.example/api";
        assert!(trusted("walletd.example", url));
        assert!(trusted("walletd.example:443", url));
        assert!(trusted("https://walletd.example", url));
        assert!(!trusted("walletd.example:8443", url));
        // http without a port: the default port is 80
        let url = "http://walletd.example";
        assert!(trusted("walletd.example:80", url));
        assert!(!trusted("walletd.example:443", url));
        // another host never matches
        assert!(!trusted("other.example", url));
    }

    /// The label the reads ask the node by (the shared `node_name`) matches the `Trusted` node it came from, with or without a port.
    #[test]
    fn t42_sia_node_label_matches_the_trusted_node() {
        for url in [
            "https://walletd.example/api",
            "http://walletd.example:9980/",
            "https://user:pw@walletd.example:8443",
        ] {
            let label = node_name(url);
            assert!(!label.contains("pw") && !label.contains('/'), "{label}");
            let policy = sia_swap_evidence_policy("SC", Some("walletd.example"), url).unwrap();
            let SwapEvidencePolicy::Trusted { node } = policy else {
                panic!("not trusted")
            };
            assert!(crate::swap_evidence::node_matches(&node, &label), "{url}: {label}");
        }
        assert_eq!(node_name("https://walletd.example/"), "walletd.example:443");
    }
}

#[cfg(test)]
mod validate_htlc_payment_tests {
    //! Regression surface for the counterparty-payment validation a swap
    //! accepts or rejects on (`validate_maker_payment` /
    //! `validate_taker_payment`, R-H1). Exercises the extracted pure helper
    //! directly, no walletd client needed -- the same style as
    //! `swap_spend_search_tests` below and `watcher_ops_tests`
    //! (`siacoin_mm_coin.rs`).
    //!
    //! Every case here is a *rejection* property except the first: the
    //! security-relevant behaviour is refusing a payment this node could not
    //! actually claim on the terms it negotiated. A validation that accepted
    //! any of these would let a swap proceed against a contract that pays
    //! someone else, pays the wrong amount, or unlocks at the wrong time.

    use super::*;

    const TIME_LOCK: u64 = 1_800_000_000;
    const AMOUNT: Currency = Currency(500);

    fn keypair(seed: u8) -> SiaKeypair {
        SiaKeypair::from_private_bytes(&[seed; 32]).expect("32 bytes is a valid ed25519 secret key")
    }

    fn secret_hash_of(secret: &[u8; 32]) -> Hash256 { Hash256(sha256(secret).take()) }

    /// A payment funding the atomic-swap address these parameters produce,
    /// with `amount` in the HTLC output.
    fn payment_paying(
        success_pub: &PublicKey,
        refund_pub: &PublicKey,
        time_lock: u64,
        secret_hash: &Hash256,
        amount: Currency,
    ) -> SiaTransaction {
        let htlc_address = SpendPolicy::atomic_swap(success_pub, refund_pub, time_lock, secret_hash).address();
        SiaTransaction(V2Transaction {
            siacoin_outputs: vec![SiacoinOutput {
                value: amount,
                address: htlc_address,
            }],
            ..Default::default()
        })
    }

    /// The parameters both sides agreed on: this node claims with `mine`,
    /// the counterparty refunds with `theirs`.
    fn agreed() -> (SiaKeypair, SiaKeypair, Hash256) { (keypair(1), keypair(2), secret_hash_of(&[7u8; 32])) }

    /// Pins the protocol property every role-order case below depends on:
    /// the success and refund keys are not interchangeable, so the two
    /// orders fund different addresses.
    ///
    /// Worth asserting separately because the fixtures here build their
    /// expected address with the same `SpendPolicy::atomic_swap` call the
    /// production helper makes. That is fine as long as the two use the
    /// same argument order deliberately -- but it means a mutation applied
    /// to *both* at once cancels itself out and the role-order cases still
    /// pass. This test does not compare against a fixture at all, so it
    /// cannot be fooled that way.
    #[test]
    fn the_success_and_refund_roles_are_not_interchangeable() {
        let (mine, theirs, secret_hash) = agreed();

        let ours_claims = SpendPolicy::atomic_swap(&mine.public(), &theirs.public(), TIME_LOCK, &secret_hash).address();
        let theirs_claims =
            SpendPolicy::atomic_swap(&theirs.public(), &mine.public(), TIME_LOCK, &secret_hash).address();

        assert_ne!(ours_claims, theirs_claims);
    }

    #[test]
    fn a_payment_on_the_agreed_terms_validates() {
        let (mine, theirs, secret_hash) = agreed();
        let tx = payment_paying(&mine.public(), &theirs.public(), TIME_LOCK, &secret_hash, AMOUNT);

        assert!(
            check_htlc_payment_output(&tx, &mine.public(), &theirs.public(), TIME_LOCK, &secret_hash, AMOUNT).is_ok()
        );
    }

    /// The case that matters most: a payment whose policy gives *them* the
    /// success branch and *us* the refund branch funds a different address.
    /// Accepting it would mean proceeding with a swap whose payment we can
    /// only ever reclaim after the timelock, never claim with the secret.
    #[test]
    fn a_payment_with_the_roles_swapped_is_rejected() {
        let (mine, theirs, secret_hash) = agreed();
        let tx = payment_paying(&theirs.public(), &mine.public(), TIME_LOCK, &secret_hash, AMOUNT);

        assert!(matches!(
            check_htlc_payment_output(&tx, &mine.public(), &theirs.public(), TIME_LOCK, &secret_hash, AMOUNT),
            Err(SiaValidateHtlcPaymentError::InvalidOutput { .. })
        ));
    }

    #[test]
    fn a_payment_for_the_wrong_amount_is_rejected() {
        let (mine, theirs, secret_hash) = agreed();
        let tx = payment_paying(&mine.public(), &theirs.public(), TIME_LOCK, &secret_hash, Currency(499));

        assert!(matches!(
            check_htlc_payment_output(&tx, &mine.public(), &theirs.public(), TIME_LOCK, &secret_hash, AMOUNT),
            Err(SiaValidateHtlcPaymentError::InvalidOutput { .. })
        ));
    }

    /// Bound to the negotiated secret hash: a contract locked to a different
    /// preimage cannot be claimed with the secret this swap will reveal.
    #[test]
    fn a_payment_locked_to_a_different_secret_is_rejected() {
        let (mine, theirs, secret_hash) = agreed();
        let other_hash = secret_hash_of(&[8u8; 32]);
        let tx = payment_paying(&mine.public(), &theirs.public(), TIME_LOCK, &other_hash, AMOUNT);

        assert!(matches!(
            check_htlc_payment_output(&tx, &mine.public(), &theirs.public(), TIME_LOCK, &secret_hash, AMOUNT),
            Err(SiaValidateHtlcPaymentError::InvalidOutput { .. })
        ));
    }

    /// Bound to the negotiated counterparty key, so a payment refundable by
    /// some third key is not accepted as theirs.
    #[test]
    fn a_payment_refundable_by_a_different_key_is_rejected() {
        let (mine, theirs, secret_hash) = agreed();
        let stranger = keypair(3);
        let tx = payment_paying(&mine.public(), &stranger.public(), TIME_LOCK, &secret_hash, AMOUNT);

        assert!(matches!(
            check_htlc_payment_output(&tx, &mine.public(), &theirs.public(), TIME_LOCK, &secret_hash, AMOUNT),
            Err(SiaValidateHtlcPaymentError::InvalidOutput { .. })
        ));
    }

    /// Bound to the negotiated timelock: a shorter one would let the
    /// counterparty reclaim the payment before this node's own leg is safe.
    #[test]
    fn a_payment_with_a_different_timelock_is_rejected() {
        let (mine, theirs, secret_hash) = agreed();
        let tx = payment_paying(&mine.public(), &theirs.public(), TIME_LOCK - 3600, &secret_hash, AMOUNT);

        assert!(matches!(
            check_htlc_payment_output(&tx, &mine.public(), &theirs.public(), TIME_LOCK, &secret_hash, AMOUNT),
            Err(SiaValidateHtlcPaymentError::InvalidOutput { .. })
        ));
    }

    /// A transaction with no outputs at all reports the length problem
    /// rather than panicking on the missing index.
    #[test]
    fn a_payment_with_no_outputs_is_rejected_as_too_short() {
        let (mine, theirs, secret_hash) = agreed();
        let tx = SiaTransaction(V2Transaction::default());

        assert!(matches!(
            check_htlc_payment_output(&tx, &mine.public(), &theirs.public(), TIME_LOCK, &secret_hash, AMOUNT),
            Err(SiaValidateHtlcPaymentError::InvalidOutputLength { actual: 0, .. })
        ));
    }

    /// The HTLC must be at `HTLC_VOUT_INDEX`. A transaction that funds the
    /// right address at some *other* index is rejected, so a payment cannot
    /// be smuggled past validation by burying the real output behind a
    /// decoy -- the spend path only ever consumes `HTLC_VOUT_INDEX`.
    #[test]
    fn a_payment_funding_the_htlc_at_the_wrong_index_is_rejected() {
        let (mine, theirs, secret_hash) = agreed();
        let correct = payment_paying(&mine.public(), &theirs.public(), TIME_LOCK, &secret_hash, AMOUNT);
        let htlc_output = correct.0.siacoin_outputs[HTLC_VOUT_INDEX as usize].clone();

        let decoy = SiacoinOutput {
            value: AMOUNT,
            address: SpendPolicy::PublicKey(keypair(9).public()).address(),
        };
        let mut outputs = vec![decoy];
        outputs.insert(HTLC_VOUT_INDEX as usize + 1, htlc_output);
        let tx = SiaTransaction(V2Transaction {
            siacoin_outputs: outputs,
            ..Default::default()
        });

        assert!(matches!(
            check_htlc_payment_output(&tx, &mine.public(), &theirs.public(), TIME_LOCK, &secret_hash, AMOUNT),
            Err(SiaValidateHtlcPaymentError::InvalidOutput { .. })
        ));
    }
}

#[cfg(test)]
mod htlc_miner_fee_refusal_tests {
    //! CRD ch.20 R-S11 and ch.60 R60.6.4 (T60.5), R60.6.7 item 2 (T60.6 (b)): the spend and refund builders
    //! subtract the fixed miner fee through `htlc_value_after_miner_fee`, which refuses a value not above
    //! the fee; the Sia safe amount is that fee plus one hasting.

    use super::*;

    #[test]
    fn an_htlc_equal_to_or_below_the_miner_fee_is_refused() {
        let fee = Currency::DEFAULT_FEE;
        assert_eq!(htlc_value_after_miner_fee(fee, fee), None);
        assert_eq!(htlc_value_after_miner_fee(Currency(fee.0 - 1), fee), None);
        assert_eq!(htlc_value_after_miner_fee(Currency(0), fee), None);
    }

    #[test]
    fn an_htlc_above_the_miner_fee_leaves_the_difference() {
        let fee = Currency::DEFAULT_FEE;
        assert_eq!(htlc_value_after_miner_fee(Currency(fee.0 + 1), fee), Some(Currency(1)));
        assert_eq!(
            htlc_value_after_miner_fee(Currency(fee.0 + 500), fee),
            Some(Currency(500))
        );
    }

    #[test]
    fn the_safe_amount_is_the_miner_fee_plus_one_hasting() {
        let expected = hastings_to_siacoin(Currency(Currency::DEFAULT_FEE.0 + 1));
        assert_eq!(htlc_safe_amount(), expected);
        // The safe amount is exactly the smallest HTLC value that survives the fee deduction.
        let safe = Currency(Currency::DEFAULT_FEE.0 + 1);
        assert!(htlc_value_after_miner_fee(safe, Currency::DEFAULT_FEE).is_some());
        assert!(htlc_value_after_miner_fee(Currency(safe.0 - 1), Currency::DEFAULT_FEE).is_none());
    }

    #[test]
    fn the_miner_fee_is_the_fixed_figure_of_2e19_hastings() {
        assert_eq!(Currency::DEFAULT_FEE, Currency(20_000_000_000_000_000_000));
        assert_eq!(
            hastings_to_siacoin(Currency::DEFAULT_FEE),
            "0.00002".parse::<BigDecimal>().unwrap()
        );
    }

    #[test]
    fn the_minimum_trading_volume_is_ten_times_the_fee_and_above_the_safe_amount() {
        assert_eq!(MIN_TRADING_VOL_FEE_MULTIPLIER, 10);
        assert_eq!(min_trading_vol_amount(), "0.0002".parse::<BigDecimal>().unwrap());
        assert!(min_trading_vol_amount() >= htlc_safe_amount());
        // The minimum transaction amount stays one hasting (no dust limit on Sia).
        let one_hasting: BigDecimal = "0.000000000000000000000001".parse().unwrap();
        assert_eq!(hastings_to_siacoin(1u64.into()), one_hasting);
        assert!(min_trading_vol_amount() > hastings_to_siacoin(1u64.into()));
    }
}
