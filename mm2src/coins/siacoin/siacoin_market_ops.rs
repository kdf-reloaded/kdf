// siacoin_market_ops — MarketCoinOps trait implementation.

use super::*;

impl MarketCoinOps for SiaCoin {
    fn ticker(&self) -> &str { &self.conf.ticker }

    fn my_address(&self) -> Result<String, String> {
        let key_pair = match &*self.priv_key_policy {
            PrivKeyPolicy::KeyPair(key_pair) => key_pair,
            _ => return Err("SiaCoin::my_address: Unexpected Key Derivation Method.".to_string()),
        };
        let address = key_pair.public().address();
        Ok(address.to_string())
    }

    fn get_public_key(&self) -> Result<String, MmError<super::UnexpectedDerivationMethod>> {
        let public_key = match &*self.priv_key_policy {
            PrivKeyPolicy::KeyPair(key_pair) => key_pair.public(),
            _ => return MmError::err(super::UnexpectedDerivationMethod::IguanaPrivKeyUnavailable),
        };
        Ok(public_key.to_string())
    }

    /// Sia's ed25519 signing (`sia_rust::types::Keypair::sign`) signs the raw message
    /// bytes directly and has no pre-hash step (unlike ECDSA's fixed-width digest
    /// requirement), so there is no 32-byte hash to expose here. Every other
    /// ed25519-keyed coin in this crate (e.g. Solana) answers the same way.
    fn sign_message_hash(&self, _message: &str) -> Option<[u8; 32]> { None }

    /// Signs `message`'s raw UTF-8 bytes with the coin's ed25519 keypair, matching
    /// `sia_rust::types::Keypair::sign`'s only signing primitive — it has no message
    /// prefix/hash convention of its own, so the raw bytes are signed as-is. The
    /// hex-encoded signature is `Signature`'s own `Display` format.
    fn sign_message(&self, message: &str) -> SignatureResult<String> {
        let key_pair = self
            .my_keypair()
            .map_err(|e| SignatureError::InternalError(e.to_string()))?;
        let signature = key_pair.sign(message.as_bytes());
        Ok(signature.to_string())
    }

    /// Verifies `signature` over `message`'s raw UTF-8 bytes against `pubkey`.
    ///
    /// Unlike this crate's ECDSA-recoverable coins, a plain ed25519 signature does
    /// not let the public key be recovered from `(message, signature)` alone, and a
    /// Sia wallet address (`Address`, see `siacoin_types.rs`) is a one-way blake2b
    /// hash of a spend policy, not an invertible encoding of the public key — so an
    /// address alone cannot serve as ed25519 verification key material. `pubkey` is
    /// therefore the sender's Sia public key (`sia_rust::types::PublicKey`'s own
    /// `ed25519:<hex>` string form), not a Sia wallet address; this mirrors how this
    /// crate's other ed25519-keyed coin (Solana) already resolves the identical
    /// constraint by taking a public key rather than an address in this parameter.
    fn verify_message(&self, signature: &str, message: &str, pubkey: &str) -> VerificationResult<bool> {
        let public_key =
            PublicKey::from_str(pubkey).map_err(|e| VerificationError::AddressDecodingError(e.to_string()))?;
        let signature = sia_rust::types::Signature::from_str(signature)
            .map_err(|e| VerificationError::SignatureDecodingError(e.to_string()))?;
        Ok(public_key.verify(message.as_bytes(), &signature).is_ok())
    }

    fn my_balance(&self) -> BalanceFut<CoinBalance> {
        let coin = self.clone();
        let fut = async move {
            let my_address = match &*coin.priv_key_policy {
                PrivKeyPolicy::KeyPair(key_pair) => key_pair.public().address(),
                _ => {
                    return MmError::err(BalanceError::UnexpectedDerivationMethod(
                        super::UnexpectedDerivationMethod::IguanaPrivKeyUnavailable,
                    ))
                },
            };
            let balance = coin
                .client
                .address_balance(my_address)
                .await
                .map_to_mm(|e| BalanceError::Transport(e.to_string()))?;
            Ok(CoinBalance {
                spendable: hastings_to_siacoin(balance.siacoins),
                unspendable: hastings_to_siacoin(balance.immature_siacoins),
            })
        };
        Box::new(fut.boxed().compat())
    }

    fn base_coin_balance(&self) -> BalanceFut<BigDecimal> { Box::new(self.my_balance().map(|res| res.spendable)) }

    fn platform_ticker(&self) -> &str { self.ticker() }

    /// `tx` is hex per the `MarketCoinOps::send_raw_tx` contract (see its doc
    /// comment in `lp_coins_traits.rs`) — the same convention `BytesJson`
    /// produces for the `tx_hex` field a withdraw preview returns, and what
    /// `send_raw_transaction`/`lp_coins_ops.rs` and every other coin's
    /// `send_raw_tx` already assume (e.g. `utxo_common_tx::send_raw_tx`
    /// hex-decodes first). A Sia transaction's actual serialization is JSON,
    /// not raw binary, so this decodes the hex to get that JSON back and
    /// hands off to `send_raw_tx_bytes`, which does the real parse-and-
    /// broadcast work directly on bytes. This used to skip the hex step and
    /// JSON-parse the still-hex-encoded string directly, which failed on the
    /// wallet's very first real character with a "trailing characters at
    /// line 1 column 2" error — column 2 because a leading hex digit like
    /// `7` or `0` is itself a complete one-character JSON number literal, so
    /// the parser reported the next hex digit as unexpected trailing input
    /// instead of reporting the real problem.
    fn send_raw_tx(&self, tx: &str) -> Box<dyn Future<Item = String, Error = String> + Send> {
        let bytes = try_fus!(hex::decode(tx).map_err(|e| e.to_string()));
        self.send_raw_tx_bytes(&bytes)
    }

    /// `tx` is the raw serialized transaction, matching every other coin's
    /// `send_raw_tx_bytes` — for Sia that serialization is JSON text as
    /// bytes, which is exactly what callers already hand it (`swap_watcher`,
    /// `lp_network`'s P2P relay) and exactly what `V2Transaction`'s own
    /// `tx_hex()` produces (`siacoin_types.rs`). Does the actual parse and
    /// broadcast; `send_raw_tx` above only exists to unwrap the hex layer
    /// external callers add on top of this.
    fn send_raw_tx_bytes(&self, tx: &[u8]) -> Box<dyn Future<Item = String, Error = String> + Send> {
        let client = self.client.clone();
        let transaction: V2Transaction = try_fus!(serde_json::from_slice(tx).map_err(|e| e.to_string()));

        let fut = async move {
            let txid = transaction.txid().to_string();
            client
                .broadcast_transaction(&transaction)
                .await
                .map_err(|e| e.to_string())?;
            Ok(txid)
        };
        Box::new(fut.boxed().compat())
    }

    fn wait_for_confirmations(
        &self,
        tx: &[u8],
        confirmations: u64,
        _requires_nota: bool,
        wait_until: u64,
        check_every: u64,
    ) -> Box<dyn Future<Item = (), Error = String> + Send> {
        let tx: SiaTransaction = try_fus!(serde_json::from_slice(tx)
            .map_err(|e| format!("siacoin wait_for_confirmations payment_tx deser failed: {}", e)));
        let txid = tx.txid();
        let client = self.client.clone();
        let tx_request = GetEventRequest { txid: txid.clone() };

        let fut = async move {
            loop {
                if now_ms() / 1000 > wait_until {
                    return ERR!(
                        "Waited too long until {} for payment {} to be received",
                        wait_until,
                        tx.txid()
                    );
                }

                match client.dispatcher(tx_request.clone()).await {
                    Ok(event) => {
                        if event.confirmations >= confirmations {
                            return Ok(());
                        }
                    },
                    Err(e) => info!("Waiting for confirmation of Sia txid {}: {}", txid, e),
                }

                Timer::sleep(check_every as f64).await;
            }
        };

        Box::new(fut.boxed().compat())
    }

    fn wait_for_tx_spend(
        &self,
        transaction: &[u8],
        wait_until: u64,
        _from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
    ) -> super::TransactionFut {
        let tx_bytes = transaction.to_vec();
        let client = self.client.clone();

        let fut = async move {
            let tx = SiaTransaction::try_from(tx_bytes).map_err(|e| TransactionErr::Plain(e.to_string()))?;
            let htlc_lock_txid = tx.txid();
            let output_id = SiacoinOutputId::new(htlc_lock_txid.clone(), HTLC_VOUT_INDEX);
            let check_every = 10f64;

            loop {
                // A transport failure here means "we do not know yet", not "not
                // spent", so the loop keeps polling either way -- aborting the
                // wait on one blip would fail a swap that is perfectly fine. But
                // it is logged rather than discarded: a node that is persistently
                // unreachable otherwise presents exactly like an unspent HTLC, and
                // the swap runs silently to its timeout with nothing to explain
                // why. The chain-side lookup below already reports its errors this
                // way; this one used to swallow them.
                let mempool_transactions = match client.dispatcher(TxpoolTransactionsRequest).await {
                    Ok(response) => response.v2transactions,
                    Err(e) => {
                        debug!("SiaCoin::wait_for_tx_spend: mempool query failed: {}", e);
                        Vec::new()
                    },
                };

                let found_in_mempool = mempool_transactions
                    .into_iter()
                    .find(|tx| tx.siacoin_inputs.iter().any(|input| input.parent.id == output_id));

                if let Some(tx) = found_in_mempool {
                    return Ok(TransactionEnum::SiaTransaction(SiaTransaction(tx)));
                }

                let found_in_block = client.find_where_utxo_spent(&output_id).await;

                match found_in_block {
                    Ok(Some(tx)) => return Ok(TransactionEnum::SiaTransaction(SiaTransaction(tx))),
                    Err(e) => debug!("SiaCoin::wait_for_tx_spend: find_where_utxo_spent failed: {}", e),
                    _ => (),
                }

                if now_ms() / 1000 >= wait_until {
                    return Err(TransactionErr::Plain(format!(
                        "Timed out waiting for spend of txid:{} vout 0",
                        htlc_lock_txid
                    )));
                }

                Timer::sleep(check_every).await;
            }
        };

        Box::new(fut.boxed().compat())
    }

    fn tx_enum_from_bytes(&self, bytes: &[u8]) -> Result<TransactionEnum, String> {
        let tx: V2Transaction = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        Ok(TransactionEnum::SiaTransaction(SiaTransaction(tx)))
    }

    fn current_block(&self) -> Box<dyn Future<Item = u64, Error = String> + Send> {
        let client = self.client.clone();
        let height_fut = async move { client.current_height().await.map_err(|e| e.to_string()) }
            .boxed()
            .compat();
        Box::new(height_fut)
    }

    fn display_priv_key(&self) -> Result<String, String> { Err("SiaCoin::display_priv_key: Unsupported".to_string()) }

    fn min_tx_amount(&self) -> BigDecimal { hastings_to_siacoin(1u64.into()) }

    fn min_trading_vol(&self) -> MmNumber { hastings_to_siacoin(1u64.into()).into() }
}

#[cfg(test)]
mod send_raw_tx_tests {
    use super::*;

    /// `send_raw_tx` receives its input as hex — the `MarketCoinOps` contract
    /// (`lp_coins_traits.rs`) and what a withdraw preview's `BytesJson`-
    /// encoded `tx_hex` field actually produces — but a Sia transaction's own
    /// serialization is JSON, not raw binary. Confirms the hex layer
    /// round-trips: hex-encoding a transaction's JSON bytes and hex-decoding
    /// them back recovers bytes that parse into an equivalent transaction —
    /// the exact two steps `send_raw_tx` now performs before handing off to
    /// `send_raw_tx_bytes`.
    #[test]
    fn hex_encoded_json_tx_round_trips_to_the_same_transaction() {
        let tx = V2TransactionBuilder::new().build();
        let json_bytes = serde_json::to_vec(&tx).expect("V2Transaction always serializes");
        let hex_tx = hex::encode(&json_bytes);

        let decoded = hex::decode(&hex_tx).expect("send_raw_tx's own hex-decode step");
        let recovered: V2Transaction = serde_json::from_slice(&decoded).expect("send_raw_tx_bytes's own parse step");

        assert_eq!(recovered.txid().to_string(), tx.txid().to_string());
    }

    /// Regression for the reported bug: before the fix, `send_raw_tx` parsed
    /// its still-hex-encoded input as a generic `serde_json::Value` first
    /// (matching the old code's actual first step — not a direct typed
    /// parse, which fails with a different message). A hex string's leading
    /// digit is itself a complete one-character JSON number literal, so that
    /// generic parse always succeeded on one character and then failed on
    /// the next with exactly "trailing characters at line 1 column 2" — the
    /// error text from the bug report — without ever reaching the real
    /// transaction.
    #[test]
    fn json_parsing_the_still_hex_encoded_string_fails_the_way_the_bug_report_did() {
        let tx = V2TransactionBuilder::new().build();
        let hex_tx = hex::encode(serde_json::to_vec(&tx).unwrap());

        let err = serde_json::from_str::<serde_json::Value>(&hex_tx)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("trailing characters at line 1 column 2"),
            "expected the old bug's exact failure mode, got: {err}"
        );
    }
}

/// `sign_message`/`verify_message` exercise `SiaKeypair::sign` and `PublicKey::verify`
/// exactly as written above; no mock `SiaApiClient` fixture exists yet in this crate to
/// build a full `SiaCoin` (see `mod.rs`'s generic-over-backend `SiaCoinGeneric`), so
/// these tests exercise those same sia-rust primitives and the string encode/decode
/// steps directly, matching this file's existing pure-logic test style.
#[cfg(test)]
mod sign_verify_message_tests {
    use super::*;

    fn keypair(seed: u8) -> SiaKeypair {
        SiaKeypair::from_private_bytes(&[seed; 32]).expect("32 bytes is a valid ed25519 secret key")
    }

    #[test]
    fn signed_message_verifies_against_the_signer_pubkey() {
        let key_pair = keypair(1);
        let pubkey_str = key_pair.public().to_string();

        let signature = key_pair.sign(b"hello sia").to_string();

        let public_key = PublicKey::from_str(&pubkey_str).unwrap();
        let parsed_sig = sia_rust::types::Signature::from_str(&signature).unwrap();
        assert!(public_key.verify(b"hello sia", &parsed_sig).is_ok());
    }

    #[test]
    fn verification_fails_for_a_tampered_message() {
        let key_pair = keypair(1);
        let signature = key_pair.sign(b"hello sia").to_string();

        let public_key = key_pair.public();
        let parsed_sig = sia_rust::types::Signature::from_str(&signature).unwrap();
        assert!(public_key.verify(b"hello SIA", &parsed_sig).is_err());
    }

    #[test]
    fn verification_fails_against_a_different_signers_pubkey() {
        let signer = keypair(1);
        let other = keypair(2);
        let signature = signer.sign(b"hello sia").to_string();

        let parsed_sig = sia_rust::types::Signature::from_str(&signature).unwrap();
        assert!(other.public().verify(b"hello sia", &parsed_sig).is_err());
    }

    /// `verify_message`'s `pubkey` argument is a Sia public key
    /// (`ed25519:<64-hex-chars>`), not a Sia wallet address — an address is a
    /// one-way blake2b hash of a spend policy and cannot serve as ed25519
    /// verification key material. A genuine Sia address must therefore fail to
    /// parse as a `PublicKey`, the same decode step `verify_message` performs.
    #[test]
    fn a_sia_wallet_address_does_not_parse_as_a_pubkey() {
        let address = keypair(1).public().address().to_string();
        assert!(PublicKey::from_str(&address).is_err());
    }

    #[test]
    fn malformed_pubkey_string_is_rejected() {
        assert!(PublicKey::from_str("not-a-pubkey").is_err());
    }

    #[test]
    fn malformed_signature_string_is_rejected() {
        assert!(sia_rust::types::Signature::from_str("not-a-signature").is_err());
    }
}
