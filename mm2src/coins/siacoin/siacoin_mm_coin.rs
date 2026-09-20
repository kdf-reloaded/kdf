// siacoin_mm_coin — MmCoin and WatcherOps trait implementations.

use super::*;
use crate::{WatcherValidatePaymentInput, WatcherValidateTakerFeeInput};

// ── WatcherOps (CRD ch.20 §20.10 D7) ──────────────────────────────────
//
// Sia's HTLC is a native spend policy (§20.6), not a UTXO/EVM script or
// contract. Feasibility was judged per method against that model:
//
// - `is_supported_by_watchers` -> `true`. This mirrors the existing
//   precedent of `utxo_standard`/`bch`/`qtum` in this crate: the flag gates
//   p2p topic subscription and the per-swap watcher-data broadcast/receive
//   path (`taker_swap.rs`, `swap_watcher.rs`), not full six-method coverage
//   -- those coins ship `true` today with *none* of the other methods
//   overridden, relying entirely on the trait's own graceful-error
//   defaults for the rest. Sia's impl below covers strictly more of the
//   six than that existing precedent does.
// - `watcher_validate_taker_fee`, `watcher_validate_taker_payment`,
//   `watcher_search_for_swap_tx_spend`, `create_taker_payment_refund_preimage`
//   are genuinely implementable: none of them need any information a
//   third-party watcher (no private key, no swap-party state) doesn't
//   already have, and Sia's V2 signature hash provably excludes each
//   input's `satisfied_policy` bytes (`V2TransactionBuilder::input_sig_hash`,
//   sia-rust `utils/tx_builder.rs`), so a refund can be fully signed ahead
//   of time and made immediately broadcastable once the time lock elapses.
// - `create_maker_payment_spend_preimage` is judged infeasible for Sia and
//   is deliberately **not** overridden (left on the trait's own graceful
//   default) -- see the doc comment immediately above where it would go,
//   below.

/// Read the funding transaction's own recorded HTLC output (fixed at
/// `HTLC_VOUT_INDEX`, R-H1) directly off its bytes. A watcher-side caller is
/// only ever given the funding tx itself -- reading the address straight off
/// it, rather than reconstructing the spend-policy address from public keys
/// neither `watcher_search_for_swap_tx_spend`'s signature supplies both of,
/// is both simpler and strictly more available.
fn htlc_output_from_tx(tx: &SiaTransaction) -> Result<&SiacoinOutput, String> {
    tx.0.siacoin_outputs
        .get(HTLC_VOUT_INDEX as usize)
        .ok_or_else(|| format!("htlc output at index {} missing from tx {}", HTLC_VOUT_INDEX, tx.txid()))
}

/// Pure core of `watcher_validate_taker_payment`: does `payment_tx`'s
/// recorded HTLC output (R-H1) match the address a taker payment with these
/// exact parameters (maker holds the success key, taker the refund key,
/// §20.6) would fund, and the expected amount? Factored out of the trait
/// method (which only adds byte/hex parsing around this) so it is
/// unit-testable without any futures01/async machinery, mirroring
/// `classify_htlc_spend`'s (`siacoin_swap_ops.rs`) testing style.
fn check_taker_payment_output(
    payment_tx: &SiaTransaction,
    time_lock: u32,
    taker_pub: &PublicKey,
    maker_pub: &PublicKey,
    secret_hash: &Hash256,
    amount: Currency,
) -> Result<(), String> {
    let htlc_address = SpendPolicy::atomic_swap(maker_pub, taker_pub, time_lock as u64, secret_hash).address();
    let expected_output = SiacoinOutput {
        value: amount,
        address: htlc_address,
    };

    let actual_output = htlc_output_from_tx(payment_tx)?;
    if *actual_output != expected_output {
        return Err(format!(
            "taker payment {} htlc output {:?} does not match expected {:?}",
            payment_tx.txid(),
            actual_output,
            expected_output
        ));
    }
    Ok(())
}

impl SiaCoin {
    /// Build (but do not broadcast) a signed refund transaction for
    /// `payment_tx`'s HTLC output (R-H3), paying back to this node's own
    /// address. Mirrors `send_refund_htlc`'s construction
    /// (`siacoin_swap_ops.rs`) minus the final broadcast -- broadcasting is
    /// the watcher's job, once it decides the refund path is due (R-H4).
    ///
    /// Unlike `send_refund_htlc`, this does not retry `utxo_from_txid` on a
    /// walletd indexing lag (that retry helper is private to
    /// `siacoin_swap_ops.rs`); a watcher calling this is expected to already
    /// be well past the payment's confirmation, so the lag `send_refund_htlc`
    /// guards against is far less likely here.
    async fn build_refund_preimage(
        &self,
        payment_tx: &[u8],
        time_lock: u32,
        success_pub: &[u8],
        secret_hash: &[u8],
    ) -> Result<TransactionEnum, String> {
        let my_keypair = self.my_keypair().map_err(|e| e.to_string())?;
        let refund_public_key = my_keypair.public();

        if success_pub.len() != 33 {
            return Err(format!(
                "success pubkey must be the 33-byte swap HTLC pubkey field, found {} bytes",
                success_pub.len()
            ));
        }
        let success_public_key = PublicKey::from_bytes(&success_pub[..32]).map_err(|e| e.to_string())?;
        let secret_hash = Hash256::try_from(secret_hash).map_err(|e| e.to_string())?;
        let payment_tx = SiaTransaction::try_from(payment_tx).map_err(|e| e.to_string())?;

        let input_spend_policy =
            SpendPolicy::atomic_swap_refund(&success_public_key, &refund_public_key, time_lock as u64, &secret_hash);

        let htlc_utxo = self
            .client
            .utxo_from_txid(&payment_tx.txid(), HTLC_VOUT_INDEX)
            .await
            .map_err(|e| e.to_string())?;

        let miner_fee = Currency::DEFAULT_FEE;
        let htlc_utxo_amount = htlc_utxo.output.siacoin_output.value;

        let tx = V2TransactionBuilder::new()
            .miner_fee(miner_fee)
            .add_siacoin_output((refund_public_key.address(), htlc_utxo_amount - miner_fee).into())
            .add_siacoin_input_with_basis(htlc_utxo, input_spend_policy)
            .satisfy_atomic_swap_refund(my_keypair, 0u32)
            .map_err(|e| e.to_string())?
            .build();

        Ok(TransactionEnum::SiaTransaction(tx.into()))
    }
}

#[async_trait]
impl WatcherOps for SiaCoin {
    fn is_supported_by_watchers(&self) -> bool { true }

    /// Real, but partial: `WatcherValidateTakerFeeInput` carries neither a
    /// dex-fee amount nor the swap uuid (unlike `ValidateFeeArgs`, which
    /// `SwapOps::validate_fee` uses for the full R-S1/R-S5 check), so this
    /// cannot correlate the fee tx to *this* swap or check its amount --
    /// only that a transaction with this hash exists on chain (or mempool)
    /// at or after `min_block_number` and pays the coin's own cached
    /// DEX-fee address (§20.4.2, checked on-chain rather than against
    /// `input.fee_addr`, matching `swap_watcher.rs`'s own comment that the
    /// coin impl re-derives it). `sender_pubkey` is also not checked: it is
    /// the swap's p2p-identity (secp256k1) key, not the Sia ed25519 HTLC key
    /// the fee tx's inputs are actually signed with (R-S9), so the two
    /// cannot be meaningfully compared.
    fn watcher_validate_taker_fee(
        &self,
        input: WatcherValidateTakerFeeInput,
    ) -> Box<dyn Future<Item = (), Error = String> + Send> {
        let coin = self.clone();
        let fut = async move {
            let fee_txid = Hash256::try_from(input.taker_fee_hash.as_slice()).map_err(|e| e.to_string())?;

            let fee_tx = match coin.client.get_event(&fee_txid).await {
                Ok(event) => {
                    let tx = match event.data {
                        EventDataWrapper::V2Transaction(tx) => tx,
                        other => return Err(format!("fee tx event is not a V2Transaction: {:?}", other)),
                    };
                    if event.index.height < input.min_block_number {
                        return Err(format!(
                            "fee tx {} confirmed at height {} before min_block_number {}",
                            fee_txid, event.index.height, input.min_block_number
                        ));
                    }
                    tx
                },
                Err(_) => match coin
                    .client
                    .get_unconfirmed_transaction(&fee_txid)
                    .await
                    .map_err(|e| e.to_string())?
                {
                    Some(tx) => tx,
                    None => return Err(format!("fee tx {} not found on chain or in mempool", fee_txid)),
                },
            };

            match fee_tx.siacoin_outputs.first() {
                Some(output) if output.address == coin.fee_address => Ok(()),
                Some(output) => Err(format!(
                    "fee tx {} pays {} instead of the expected fee address {}",
                    fee_txid, output.address, coin.fee_address
                )),
                None => Err(format!("fee tx {} has no outputs", fee_txid)),
            }
        };
        Box::new(fut.boxed().compat())
    }

    /// Real: reconstructs the taker-payment HTLC address from the caller-
    /// supplied `maker_pub`/`taker_pub` (maker holds the success key, taker
    /// the refund key -- the role assignment `send_taker_payment` itself
    /// builds, §20.6) and checks the payment tx's fixed-position output
    /// (R-H1) against it, the same check `validate_htlc_payment` makes for
    /// `SwapOps::validate_taker_payment` -- but without needing this node's
    /// own key, since a watcher has no stake of its own in the swap.
    fn watcher_validate_taker_payment(
        &self,
        input: WatcherValidatePaymentInput,
    ) -> Box<dyn Future<Item = (), Error = String> + Send> {
        let fut = async move {
            let payment_tx = SiaTransaction::try_from(input.payment_tx.as_slice()).map_err(|e| e.to_string())?;

            if input.taker_pub.len() != 33 || input.maker_pub.len() != 33 {
                return Err("taker_pub and maker_pub must each be the 33-byte swap HTLC pubkey field".to_string());
            }
            let taker_public_key = PublicKey::from_bytes(&input.taker_pub[..32]).map_err(|e| e.to_string())?;
            let maker_public_key = PublicKey::from_bytes(&input.maker_pub[..32]).map_err(|e| e.to_string())?;
            let secret_hash = Hash256::try_from(input.secret_hash.as_slice()).map_err(|e| e.to_string())?;
            let amount = siacoin_to_hastings(input.amount).map_err(|e| e.to_string())?;

            check_taker_payment_output(
                &payment_tx,
                input.time_lock,
                &taker_public_key,
                &maker_public_key,
                &secret_hash,
                amount,
            )
        };
        Box::new(fut.boxed().compat())
    }

    // `create_maker_payment_spend_preimage` is judged INFEASIBLE for Sia and
    // is intentionally not overridden here -- it falls through to the
    // trait's own default, which returns a graceful `TransactionErr::Plain`
    // error rather than panicking.
    //
    // Reasoning: the trait signature gives this method a `secret_hash`, never
    // the secret itself -- and it is called (`taker_swap.rs`'s
    // `maybe_broadcast_watcher_data`) *before* the secret exists to the
    // caller, right after the taker sends its payment. Sia's success path
    // (R-H2) requires the actual preimage bytes in `SatisfiedPolicy.preimages`
    // to satisfy the on-chain hash-lock check; a transaction built now can
    // carry a signature (§20.6's spend-policy sig hash provably excludes the
    // preimage bytes, so signing ahead of the secret being known is sound in
    // principle -- confirmed against `V2TransactionBuilder::input_sig_hash`,
    // sia-rust `utils/tx_builder.rs`) but never a *valid* preimage, since
    // this node does not have it yet. Making that a fully-valid,
    // immediately-broadcastable transaction requires a second phase -- some
    // caller inserting the now-revealed secret into the built transaction
    // right before broadcast. No such hook is reachable from here: this
    // crate's only consumer, `swap_watcher.rs`'s `SpendMakerPayment` state,
    // broadcasts the stored preimage bytes verbatim once it independently
    // extracts the secret (the extracted `secret` field there is explicitly
    // `#[allow(dead_code)]`, "retained for logging/auditing" only) -- it
    // never splices the secret back in, and that file is outside this pass's
    // scope to change. A stub `unimplemented!()` override here would instead
    // be a live regression: `is_supported_by_watchers() == true` means
    // `maybe_broadcast_watcher_data` *will* call this for real Sia swaps, and
    // panicking there is strictly worse than the trait's own default, which
    // already degrades this one path gracefully (a logged warning, no
    // watcher data broadcast for that swap) exactly the way it does today
    // for every other coin family in this crate.

    /// Real: the refund path (R-H3) needs only a signature and the elapsed
    /// time lock, never the secret -- unlike the success-path preimage
    /// above, a refund can be fully signed and made immediately
    /// broadcastable ahead of time using only this node's own key.
    /// `maker_pub` here is the counterparty's success-path pubkey needed to
    /// reconstruct the full policy (matching `send_taker_refunds_payment`'s/
    /// `send_refund_htlc`'s existing "other pubkey" convention). Builds but
    /// does not broadcast -- broadcasting is the watcher's job.
    fn create_taker_payment_refund_preimage(
        &self,
        taker_payment_tx: &[u8],
        time_lock: u32,
        maker_pub: &[u8],
        secret_hash: &[u8],
        _swap_unique_data: &[u8],
    ) -> TransactionFut {
        let coin = self.clone();
        let taker_payment_tx = taker_payment_tx.to_vec();
        let maker_pub = maker_pub.to_vec();
        let secret_hash = secret_hash.to_vec();
        let fut = async move {
            coin.build_refund_preimage(&taker_payment_tx, time_lock, &maker_pub, &secret_hash)
                .await
                .map_err(TransactionErr::Plain)
        };
        Box::new(fut.boxed().compat())
    }

    /// Real: reads the funding tx's own recorded HTLC output id and address
    /// directly off `tx` (R-H1) rather than reconstructing the policy from
    /// public keys (this method's signature carries only one of the two
    /// pubkeys involved), then classifies the spend the same way R-S6/D3's
    /// event walk does -- reusing the public `SwapOps::extract_secret`
    /// method rather than duplicating its hash-comparison logic, which lives
    /// in `siacoin_swap_ops.rs` (out of this pass's scope to touch). Uses a
    /// single-page `get_address_events` lookup (mirroring
    /// `check_if_my_payment_sent`'s pattern in this same coin) rather than
    /// D3's paginated `fetch_all_events`, which is private to that file.
    async fn watcher_search_for_swap_tx_spend(
        &self,
        _time_lock: u32,
        _other_pub: &[u8],
        secret_hash: &[u8],
        tx: &[u8],
        _search_from_block: u64,
    ) -> Result<Option<FoundSwapTxSpend>, String> {
        let payment_tx = SiaTransaction::try_from(tx).map_err(|e| e.to_string())?;
        let htlc_output_id = SiacoinOutputId::new(payment_tx.txid(), HTLC_VOUT_INDEX);
        let htlc_address = htlc_output_from_tx(&payment_tx)?.address.clone();

        let events = self
            .client
            .get_address_events(htlc_address)
            .await
            .map_err(|e| e.to_string())?;

        for event in events {
            let spend_tx = match event.data {
                EventDataWrapper::V2Transaction(tx) => tx,
                _ => continue,
            };
            let spends_htlc_output = spend_tx
                .siacoin_inputs
                .iter()
                .any(|input| input.parent.id == htlc_output_id);
            if !spends_htlc_output {
                continue;
            }

            let found_tx: TransactionEnum = SiaTransaction(spend_tx).into();
            return Ok(Some(match self.extract_secret(secret_hash, &found_tx.tx_hex()) {
                Ok(_) => FoundSwapTxSpend::Spent(found_tx),
                Err(_) => FoundSwapTxSpend::Refunded(found_tx),
            }));
        }
        Ok(None)
    }
}

#[async_trait]
impl MmCoin for SiaCoin {
    fn is_asset_chain(&self) -> bool { false }

    fn withdraw(&self, req: WithdrawRequest) -> WithdrawFut {
        let coin = self.clone();
        let fut = async move {
            let builder = SiaWithdrawBuilder::new(&coin, req)?;
            builder.build().await
        };
        Box::new(fut.boxed().compat())
    }

    /// Real (CRD ch.20 §20.10 D8): fetches a transaction by txid, confirmed
    /// or in the mempool. `sia_rust`'s `ApiClientHelpers::get_transaction`
    /// is the confirmed-tx-by-id lookup -- verified against D7's
    /// `watcher_validate_taker_fee` precedent (this same file) rather than
    /// assumed: `get_transaction` is itself defined (sia-rust
    /// `transport/client/helpers.rs`) as a plain `get_event(txid)` filtered
    /// to the `EventDataWrapper::V2Transaction` variant, i.e. exactly the
    /// txid-scoped lookup D7 inlines, not an address/wallet-scoped query
    /// that happens to work for D7's narrower need -- so it transfers
    /// cleanly to this general raw-tx fetch. Falls back to
    /// `get_unconfirmed_transaction` (same mempool-only lookup this coin
    /// already uses, e.g. `wait_for_tx_spend`'s `found_in_mempool`/D7's own
    /// fallback) for a transaction that exists only in the mempool.
    fn get_raw_transaction(&self, req: RawTransactionRequest) -> RawTransactionFut {
        let coin = self.clone();
        let fut = async move {
            let txid =
                Hash256::from_str(&req.tx_hash).map_to_mm(|e| RawTransactionError::InvalidHashError(e.to_string()))?;

            let tx = match coin.client.get_transaction(&txid).await {
                Ok(tx) => tx,
                Err(_) => coin
                    .client
                    .get_unconfirmed_transaction(&txid)
                    .await
                    .map_to_mm(|e| RawTransactionError::Transport(e.to_string()))?
                    .or_mm_err(|| RawTransactionError::HashNotExist(req.tx_hash.clone()))?,
            };

            // Both carriers, on the same terms the withdraw path uses (ch.20 R-W12,
            // R-W6/R-W7): `tx_hex` is the hex of Sia's native transaction JSON and
            // `tx_json` that same JSON unencoded, derived from the very bytes
            // `tx_hex` encodes so the two can never describe different
            // transactions. Serialising here rather than through
            // `Transaction::tx_hex()` is deliberate: that trait method has no error
            // channel and substitutes an empty vector on failure, which would hand
            // the caller a successful response carrying no transaction at all.
            let tx_bytes = serde_json::ser::to_vec(&SiaTransaction(tx)).map_to_mm(|e| {
                RawTransactionError::InternalError(format!("Failed to serialize the transaction: {e}"))
            })?;
            let tx_json = serde_json::from_slice(&tx_bytes).map_to_mm(|e| {
                RawTransactionError::InternalError(format!("Failed to reparse the serialized transaction: {e}"))
            })?;

            Ok(RawTransactionRes {
                tx_hex: BytesJson(tx_bytes),
                tx_json: Some(tx_json),
            })
        };
        Box::new(fut.boxed().compat())
    }

    fn decimals(&self) -> u8 { 24 }

    fn convert_to_address(&self, from: &str, _to_address_format: Json) -> Result<String, String> {
        Ok(from.to_string())
    }

    fn validate_address(&self, address: &str) -> ValidateAddressResult {
        match Address::from_str(address) {
            Ok(_) => ValidateAddressResult {
                is_valid: true,
                reason: None,
            },
            Err(e) => ValidateAddressResult {
                is_valid: false,
                reason: Some(e.to_string()),
            },
        }
    }

    fn process_history_loop(&self, ctx: MmArc) -> Box<dyn Future<Item = (), Error = ()> + Send> {
        // The tracking pass of CRD ch.53 §53.6: populates the coin-generic
        // runtime history store from the wallet address's walletd event log.
        let coin = self.clone();
        Box::new(
            async move {
                super::process_history_loop(coin, ctx).await;
                Ok(())
            }
            .boxed()
            .compat(),
        )
    }

    fn history_sync_status(&self) -> HistorySyncState { self.history_sync_state.lock().unwrap().clone() }

    fn get_trade_fee(&self) -> Box<dyn Future<Item = TradeFee, Error = String> + Send> {
        Box::new(futures01::future::ok(TradeFee {
            coin: self.ticker().to_string(),
            amount: MmNumber::from("0.00001"),
            paid_from_trading_vol: false,
        }))
    }

    async fn get_sender_trade_fee(
        &self,
        _value: TradePreimageValue,
        _stage: FeeApproxStage,
    ) -> TradePreimageResult<TradeFee> {
        Ok(TradeFee {
            coin: self.ticker().to_string(),
            amount: MmNumber::from("0.00001"),
            paid_from_trading_vol: false,
        })
    }

    fn get_receiver_trade_fee(&self, _stage: FeeApproxStage) -> TradePreimageFut<TradeFee> {
        Box::new(futures01::future::ok(TradeFee {
            coin: self.ticker().to_string(),
            amount: MmNumber::from("0.00001"),
            paid_from_trading_vol: false,
        }))
    }

    async fn get_fee_to_send_taker_fee(
        &self,
        _dex_fee_amount: BigDecimal,
        _stage: FeeApproxStage,
    ) -> TradePreimageResult<TradeFee> {
        Ok(TradeFee {
            coin: self.ticker().to_string(),
            amount: MmNumber::from("0.00001"),
            paid_from_trading_vol: false,
        })
    }

    fn required_confirmations(&self) -> u64 { self.required_confirmations.load(AtomicOrdering::Relaxed) }

    fn requires_notarization(&self) -> bool { false }

    fn set_required_confirmations(&self, confirmations: u64) {
        self.required_confirmations
            .store(confirmations, AtomicOrdering::Relaxed);
    }

    fn set_requires_notarization(&self, _requires_nota: bool) {}

    fn swap_contract_address(&self) -> Option<BytesJson> { None }

    fn mature_confirmations(&self) -> Option<u32> { None }

    fn coin_protocol_info(&self) -> Vec<u8> { Vec::new() }

    fn is_coin_protocol_supported(&self, _info: &Option<Vec<u8>>) -> bool { true }
}

#[cfg(test)]
mod watcher_ops_tests {
    //! Pure-logic regression surface for D7 (CRD ch.20 §20.10). Follows
    //! `siacoin_swap_ops.rs`'s `swap_spend_search_tests` style: exercise the
    //! extracted pure helpers directly, no walletd client needed.

    use super::*;

    fn keypair(seed: u8) -> SiaKeypair {
        SiaKeypair::from_private_bytes(&[seed; 32]).expect("32 bytes is a valid ed25519 secret key")
    }

    fn secret_hash_of(secret: &[u8; 32]) -> Hash256 { Hash256(sha256(secret).take()) }

    /// R-H1: a payment tx whose HTLC output (index `HTLC_VOUT_INDEX`)
    /// matches exactly what a taker payment with these parameters funds
    /// (maker = success key, taker = refund key, §20.6) passes.
    #[test]
    fn matching_htlc_output_validates() {
        let taker_pub = keypair(1).public();
        let maker_pub = keypair(2).public();
        let secret_hash = secret_hash_of(&[7u8; 32]);
        let time_lock = 1_800_000_000u32;
        let amount = Currency(500);

        let htlc_address = SpendPolicy::atomic_swap(&maker_pub, &taker_pub, time_lock as u64, &secret_hash).address();
        let payment_tx = SiaTransaction(V2Transaction {
            siacoin_outputs: vec![SiacoinOutput {
                value: amount,
                address: htlc_address,
            }],
            ..Default::default()
        });

        assert!(
            check_taker_payment_output(&payment_tx, time_lock, &taker_pub, &maker_pub, &secret_hash, amount).is_ok()
        );
    }

    /// A payment whose output amount doesn't match is rejected, even though
    /// the address is right.
    #[test]
    fn wrong_amount_is_rejected() {
        let taker_pub = keypair(1).public();
        let maker_pub = keypair(2).public();
        let secret_hash = secret_hash_of(&[7u8; 32]);
        let time_lock = 1_800_000_000u32;

        let htlc_address = SpendPolicy::atomic_swap(&maker_pub, &taker_pub, time_lock as u64, &secret_hash).address();
        let payment_tx = SiaTransaction(V2Transaction {
            siacoin_outputs: vec![SiacoinOutput {
                value: Currency(500),
                address: htlc_address,
            }],
            ..Default::default()
        });

        assert!(check_taker_payment_output(
            &payment_tx,
            time_lock,
            &taker_pub,
            &maker_pub,
            &secret_hash,
            Currency(999)
        )
        .is_err());
    }

    /// Swapping which pubkey plays the success/refund role produces a
    /// different HTLC address, so validating against the correct
    /// maker/taker assignment is what actually matters here (ch.51 R63's
    /// "wrong field silently validates against the wrong policy" failure
    /// mode, applied to the watcher path).
    #[test]
    fn swapped_success_refund_roles_do_not_validate() {
        let taker_pub = keypair(1).public();
        let maker_pub = keypair(2).public();
        let secret_hash = secret_hash_of(&[7u8; 32]);
        let time_lock = 1_800_000_000u32;
        let amount = Currency(500);

        // Funded as if taker were the success key and maker the refund key --
        // the reverse of what a real taker payment does.
        let wrong_address = SpendPolicy::atomic_swap(&taker_pub, &maker_pub, time_lock as u64, &secret_hash).address();
        let payment_tx = SiaTransaction(V2Transaction {
            siacoin_outputs: vec![SiacoinOutput {
                value: amount,
                address: wrong_address,
            }],
            ..Default::default()
        });

        assert!(
            check_taker_payment_output(&payment_tx, time_lock, &taker_pub, &maker_pub, &secret_hash, amount).is_err()
        );
    }

    /// R-H1's "fixed, deterministic position": a tx with no output at
    /// `HTLC_VOUT_INDEX` at all is a parse-level failure, not a mismatch.
    #[test]
    fn missing_htlc_output_is_rejected() {
        let taker_pub = keypair(1).public();
        let maker_pub = keypair(2).public();
        let secret_hash = secret_hash_of(&[7u8; 32]);

        let empty_tx = SiaTransaction(V2Transaction::default());
        assert!(htlc_output_from_tx(&empty_tx).is_err());
        assert!(check_taker_payment_output(
            &empty_tx,
            1_800_000_000,
            &taker_pub,
            &maker_pub,
            &secret_hash,
            Currency(500)
        )
        .is_err());
    }
}
