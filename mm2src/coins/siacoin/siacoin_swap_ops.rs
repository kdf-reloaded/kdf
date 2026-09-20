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
                        txid, vout_index, attempt, ATTEMPTS, e
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

        let tx = V2TransactionBuilder::new()
            .miner_fee(miner_fee)
            .add_siacoin_output((maker_public_key.address(), htlc_utxo_amount - miner_fee).into())
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

        let tx = V2TransactionBuilder::new()
            .miner_fee(miner_fee)
            .add_siacoin_output((taker_public_key.address(), htlc_utxo_amount - miner_fee).into())
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
                    e
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

        let tx = V2TransactionBuilder::new()
            .miner_fee(miner_fee)
            .add_siacoin_output((my_keypair.public().address(), htlc_utxo_amount - miner_fee).into())
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

        let events_result = self.client.get_address_events(htlc_address).await;
        let events = match events_result {
            Ok(events) => events,
            Err(_) => return Ok(None),
        };

        let event = match events.len() {
            0 => return Ok(None),
            _ => events[0].clone(),
        };

        let tx = match event.data {
            EventDataWrapper::V2Transaction(tx) => tx,
            wrong_variant => return Err(SiaCheckIfMyPaymentSentError::EventVariant(wrong_variant)),
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

        let events = self.fetch_all_events(&htlc_address).await?;

        Ok(classify_htlc_spend(&events, &htlc_output_id, &secret_hash))
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
        let median_timestamp = self.client.get_median_timestamp().await?;

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
