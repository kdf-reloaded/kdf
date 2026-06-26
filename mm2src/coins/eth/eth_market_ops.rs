//! MarketCoinOps trait implementation for EthCoin.

use super::*;

#[cfg_attr(test, mockable)]
impl MarketCoinOps for EthCoin {
    fn ticker(&self) -> &str { &self.ticker[..] }

    fn my_address(&self) -> Result<String, String> { Ok(checksum_address(&format!("{:#02x}", self.my_address))) }

    fn get_public_key(&self) -> Result<String, MmError<UnexpectedDerivationMethod>> { unimplemented!() }

    /// Hash message for signature using Ethereum's message signing format.
    /// keccak256(PREFIX_LENGTH + PREFIX + MESSAGE_LENGTH + MESSAGE)
    fn sign_message_hash(&self, message: &str) -> Option<[u8; 32]> {
        let message_prefix = self.sign_message_prefix.as_ref()?;
        let mut stream = Stream::new();
        let prefix_len = CompactInteger::from(message_prefix.len());
        prefix_len.serialize(&mut stream);
        stream.append_slice(message_prefix.as_bytes());
        stream.append_slice(message.len().to_string().as_bytes());
        stream.append_slice(message.as_bytes());
        Some(keccak256(&stream.out()).take())
    }

    fn sign_message(&self, message: &str) -> SignatureResult<String> {
        let message_hash = self.sign_message_hash(message).ok_or(SignatureError::PrefixNotFound)?;
        let privkey = self.signer.local_secret().ok_or_else(|| {
            SignatureError::InvalidRequest(
                "message signing is not available for a key-less signing policy (e.g. MetaMask)".to_string(),
            )
        })?;
        let signature = sign(privkey, &H256::from(message_hash))?;
        Ok(format!("0x{}", signature))
    }

    fn verify_message(&self, signature: &str, message: &str, address: &str) -> VerificationResult<bool> {
        let message_hash = self
            .sign_message_hash(message)
            .ok_or(VerificationError::PrefixNotFound)?;
        let address = self
            .address_from_str(address)
            .map_err(VerificationError::AddressDecodingError)?;
        let signature = Signature::from_str(signature.strip_prefix("0x").unwrap_or(signature))?;
        let is_verified = verify_address(&address, &signature, &H256::from(message_hash))?;
        Ok(is_verified)
    }

    fn my_balance(&self) -> BalanceFut<CoinBalance> {
        let decimals = self.decimals;
        let fut = self
            .my_balance()
            .and_then(move |result| Ok(u256_to_big_decimal(result, decimals).mm_err(Into::into)?))
            .map(|spendable| CoinBalance {
                spendable,
                unspendable: BigDecimal::from(0),
                ..Default::default()
            });
        Box::new(fut)
    }

    fn base_coin_balance(&self) -> BalanceFut<BigDecimal> {
        Box::new(
            self.eth_balance()
                .and_then(move |result| Ok(u256_to_big_decimal(result, 18).mm_err(Into::into)?)),
        )
    }

    fn platform_ticker(&self) -> &str {
        match &self.coin_type {
            EthCoinType::Eth => self.ticker(),
            EthCoinType::Erc20 { platform, .. } => platform,
            // TRX is its own platform; TRC20 references its parent TRX coin.
            EthCoinType::Tron => self.ticker(),
            EthCoinType::Trc20 { platform, .. } => platform,
        }
    }

    fn send_raw_tx(&self, mut tx: &str) -> Box<dyn Future<Item = String, Error = String> + Send> {
        // LP-17: alloy `Provider::send_raw_transaction` replaces
        // `web3.eth().send_raw_transaction(...)`. Both ultimately drive
        // `eth_sendRawTransaction`; the wire-level RPC method is
        // unchanged. Output format `format!("{:02x}", h)` (no `0x`
        // prefix) is preserved bit-for-bit so existing callers /
        // persisted swap state are not affected.
        use crate::eth::alloy_compat::assert_send_future;
        use alloy::providers::Provider;

        if tx.starts_with("0x") {
            tx = &tx[2..];
        }
        let bytes = try_fus!(hex::decode(tx));
        let selfi = self.clone();
        let fut = async move {
            let provider = selfi.alloy_provider();
            let pending = try_s!(assert_send_future(provider.send_raw_transaction(&bytes)).await);
            let hash = *pending.tx_hash();
            Ok(format!("{:02x}", hash))
        };
        Box::new(fut.boxed().compat())
    }

    fn send_raw_tx_bytes(&self, tx: &[u8]) -> Box<dyn Future<Item = String, Error = String> + Send> {
        use crate::eth::alloy_compat::assert_send_future;
        use alloy::providers::Provider;

        let bytes = tx.to_vec();
        let selfi = self.clone();
        let fut = async move {
            let provider = selfi.alloy_provider();
            let pending = try_s!(assert_send_future(provider.send_raw_transaction(&bytes)).await);
            let hash = *pending.tx_hash();
            Ok(format!("{:02x}", hash))
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
        let ctx = try_fus!(MmArc::from_weak(&self.ctx).ok_or("No context"));
        let mut status = ctx.log.status_handle();
        status.status(&[&self.ticker], "Waiting for confirmations…");
        status.deadline(wait_until * 1000);

        let unsigned: UnverifiedTransaction = try_fus!(rlp::decode(tx));
        let tx = try_fus!(SignedEthTx::new(unsigned));

        let required_confirms = U256::from(confirmations);
        let selfi = self.clone();
        let fut = async move {
            // LP-17: alloy `Provider::get_transaction_receipt` /
            // `get_block_number` replace `web3.eth().transaction_receipt`
            // / `web3.eth().block_number`. Wire-level RPC methods
            // (`eth_getTransactionReceipt`, `eth_blockNumber`) are
            // unchanged. alloy's `ReceiptEnvelope::status()` returns
            // `bool` (true == 1) which directly replaces the legacy
            // `Some(1.into())` comparison.
            use crate::eth::alloy_compat::assert_send_future;
            use alloy::providers::Provider;

            let provider = selfi.alloy_provider();
            loop {
                if status.ms2deadline().unwrap() < 0 {
                    status.append(" Timed out.");
                    return ERR!(
                        "Waited too long until {} for transaction {:?} confirmation ",
                        wait_until,
                        tx
                    );
                }

                let web3_receipt = match assert_send_future(
                    provider.get_transaction_receipt(alloy::primitives::B256::from_slice(&tx.hash().0)),
                )
                .await
                {
                    Ok(r) => r,
                    Err(e) => {
                        log!("Error " [e] " getting the " (selfi.ticker()) " transaction " [tx.tx_hash()] ", retrying in 15 seconds");
                        Timer::sleep(check_every as f64).await;
                        continue;
                    },
                };
                if let Some(receipt) = web3_receipt {
                    if !receipt.inner.status() {
                        status.append(" Failed.");
                        return ERR!(
                            "Tx receipt {:?} status of {} tx {:?} is failed",
                            receipt,
                            selfi.ticker(),
                            tx.tx_hash()
                        );
                    }

                    if let Some(confirmed_at) = receipt.block_number {
                        let confirmed_at = U256::from(confirmed_at);
                        let current_block = match assert_send_future(provider.get_block_number()).await {
                            Ok(b) => U256::from(b),
                            Err(e) => {
                                log!("Error " [e] " getting the " (selfi.ticker()) " block number retrying in 15 seconds");
                                Timer::sleep(check_every as f64).await;
                                continue;
                            },
                        };
                        // checking if the current block is above the confirmed_at block prediction for pos chain to prevent overflow
                        if current_block >= confirmed_at && current_block - confirmed_at + 1 >= required_confirms {
                            status.append(" Confirmed.");
                            return Ok(());
                        }
                    }
                }
                Timer::sleep(check_every as f64).await;
            }
        };
        Box::new(fut.boxed().compat())
    }

    fn wait_for_tx_spend(
        &self,
        tx_bytes: &[u8],
        wait_until: u64,
        from_block: u64,
        swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        let unverified: UnverifiedTransaction = try_tx_fus!(rlp::decode(tx_bytes));
        let tx = try_tx_fus!(SignedEthTx::new(unverified));
        let swap_contract_address = try_tx_fus!(swap_contract_address.try_to_address());

        let func_name = match self.coin_type {
            EthCoinType::Eth => "ethPayment",
            EthCoinType::Erc20 { .. } => "erc20Payment",
            // V1 ETH-style HTLC swaps don't apply to TRON; activation gating
            // prevents this code path from being reached. Real TRON swap
            // wiring lands in P10.2.5.
            EthCoinType::Tron | EthCoinType::Trc20 { .. } => {
                unimplemented!("TRON V1 swap watchers not wired (pending P10.2.5)")
            },
        };

        let payment_func = try_tx_fus!(SWAP_CONTRACT.function(func_name));
        let decoded = try_tx_fus!(payment_func.decode_input(&tx.data));
        let id = match &decoded[0] {
            Token::FixedBytes(bytes) => bytes.clone(),
            _ => panic!(),
        };
        let selfi = self.clone();

        let fut = async move {
            loop {
                let current_block = match selfi.current_block().compat().await {
                    Ok(b) => b,
                    Err(e) => {
                        log!("Error " (e) " getting block number");
                        Timer::sleep(5.).await;
                        continue;
                    },
                };

                let events = match selfi
                    .spend_events(swap_contract_address, from_block, current_block)
                    .compat()
                    .await
                {
                    Ok(ev) => ev,
                    Err(e) => {
                        log!("Error " (e) " getting spend events");
                        Timer::sleep(5.).await;
                        continue;
                    },
                };

                let found = events.iter().find(|event| &event.data.0[..32] == id.as_slice());

                if let Some(event) = found {
                    if let Some(tx_hash) = event.transaction_hash {
                        // LP-17: alloy `Provider::get_transaction_by_hash`
                        // + `signed_tx_from_alloy_tx` round-trip replaces
                        // the web3 `transaction(...)` + `signed_tx_from_web3_tx`
                        // pair. Same wire RPC method (`eth_getTransactionByHash`).
                        use crate::eth::alloy_compat::assert_send_future;
                        use alloy::providers::Provider;

                        let provider = selfi.alloy_provider();
                        let alloy_hash = alloy::primitives::B256::from_slice(&tx_hash.0);
                        let transaction = match assert_send_future(provider.get_transaction_by_hash(alloy_hash)).await {
                            Ok(Some(t)) => t,
                            Ok(None) => {
                                log!("Tx " (tx_hash) " not found yet");
                                Timer::sleep(5.).await;
                                continue;
                            },
                            Err(e) => {
                                log!("Get tx " (tx_hash) " error " (e));
                                Timer::sleep(5.).await;
                                continue;
                            },
                        };

                        return Ok(TransactionEnum::from(try_tx_s!(signed_tx_from_alloy_tx(transaction))));
                    }
                }

                if now_ms() / 1000 > wait_until {
                    return TX_PLAIN_ERR!(
                        "Waited too long until {} for transaction {:?} to be spent ",
                        wait_until,
                        tx,
                    );
                }
                Timer::sleep(5.).await;
                continue;
            }
        };
        Box::new(fut.boxed().compat())
    }

    fn tx_enum_from_bytes(&self, bytes: &[u8]) -> Result<TransactionEnum, String> {
        Ok(try_s!(signed_eth_tx_from_bytes(bytes)).into())
    }

    fn current_block(&self) -> Box<dyn Future<Item = u64, Error = String> + Send> {
        // LP-17: alloy `Provider::get_block_number` returns `u64`
        // directly (no `U256 -> u64` conversion needed). Underlying
        // wire RPC method `eth_blockNumber` is unchanged.
        use crate::eth::alloy_compat::assert_send_future;
        use alloy::providers::Provider;

        let selfi = self.clone();
        let fut = async move {
            let provider = selfi.alloy_provider();
            assert_send_future(provider.get_block_number())
                .await
                .map_err(|e| ERRL!("{}", e))
        };
        Box::new(fut.boxed().compat())
    }

    fn display_priv_key(&self) -> Result<String, String> {
        match self.signer.local_secret() {
            Some(secret) => Ok(format!("{:#02x}", secret)),
            None => Err("Private key export is unsupported under the MetaMask signing policy".to_string()),
        }
    }

    fn min_tx_amount(&self) -> BigDecimal { BigDecimal::from(0) }

    fn min_trading_vol(&self) -> MmNumber {
        let pow = self.decimals / 3;
        MmNumber::from(1) / MmNumber::from(10u64.pow(pow as u32))
    }

    fn sign_raw_tx(&self, args: &SignRawTransactionRequest) -> RawTransactionFut {
        let coin = self.clone();
        let args = args.clone();
        Box::new(sign_raw_eth_tx_impl(coin, args).boxed().compat())
    }
}
