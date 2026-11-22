//! SwapOps and WatcherOps trait implementations for EthCoin.

use super::*;
use crate::utxo::utxo_common::capped_refund_wait;
use crate::{wall_clock_refund_eligibility, CanRefundHtlc};

#[async_trait]
impl SwapOps for EthCoin {
    fn send_taker_fee(&self, dex_fee: &DexFee, fee_addr: &[u8], _uuid: &[u8]) -> TransactionFut {
        let address = try_tx_fus!(addr_from_raw_pubkey(fee_addr));
        // For EVM coins, only the fee portion is sent on-chain; the burn
        // portion (if any) is implicit — not sent as a separate transfer.
        let amount = dex_fee.fee_amount();

        Box::new(
            self.send_to_address(
                address,
                try_tx_fus!(wei_from_big_decimal(&amount.to_decimal(), self.decimals)),
            )
            .map(TransactionEnum::from),
        )
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
        let taker_addr = try_tx_fus!(addr_from_raw_pubkey(taker_pub));
        let swap_contract_address = try_tx_fus!(swap_contract_address.try_to_address());

        Box::new(
            self.send_hash_time_locked_payment(
                self.etomic_swap_id(time_lock, secret_hash),
                try_tx_fus!(wei_from_big_decimal(&amount, self.decimals)),
                time_lock,
                secret_hash,
                taker_addr,
                swap_contract_address,
            )
            .map(TransactionEnum::from),
        )
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
        let maker_addr = try_tx_fus!(addr_from_raw_pubkey(maker_pub));
        let swap_contract_address = try_tx_fus!(swap_contract_address.try_to_address());

        Box::new(
            self.send_hash_time_locked_payment(
                self.etomic_swap_id(time_lock, secret_hash),
                try_tx_fus!(wei_from_big_decimal(&amount, self.decimals)),
                time_lock,
                secret_hash,
                maker_addr,
                swap_contract_address,
            )
            .map(TransactionEnum::from),
        )
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
        let tx: UnverifiedTransaction = try_tx_fus!(rlp::decode(taker_payment_tx));
        let signed = try_tx_fus!(SignedEthTx::new(tx));
        let swap_contract_address = try_tx_fus!(swap_contract_address.try_to_address(), signed);

        Box::new(
            self.spend_hash_time_locked_payment(signed, swap_contract_address, secret)
                .map(TransactionEnum::from),
        )
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
        let tx: UnverifiedTransaction = try_tx_fus!(rlp::decode(maker_payment_tx));
        let signed = try_tx_fus!(SignedEthTx::new(tx));
        let swap_contract_address = try_tx_fus!(swap_contract_address.try_to_address());
        Box::new(
            self.spend_hash_time_locked_payment(signed, swap_contract_address, secret)
                .map(TransactionEnum::from),
        )
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
        let tx: UnverifiedTransaction = try_tx_fus!(rlp::decode(taker_payment_tx));
        let signed = try_tx_fus!(SignedEthTx::new(tx));
        let swap_contract_address = try_tx_fus!(swap_contract_address.try_to_address());

        Box::new(
            self.refund_hash_time_locked_payment(swap_contract_address, signed)
                .map(TransactionEnum::from),
        )
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
        let tx: UnverifiedTransaction = try_tx_fus!(rlp::decode(maker_payment_tx));
        let signed = try_tx_fus!(SignedEthTx::new(tx));
        let swap_contract_address = try_tx_fus!(swap_contract_address.try_to_address());

        Box::new(
            self.refund_hash_time_locked_payment(swap_contract_address, signed)
                .map(TransactionEnum::from),
        )
    }

    fn validate_fee(&self, args: ValidateFeeArgs<'_>) -> Box<dyn Future<Item = (), Error = String> + Send> {
        // LP-17: replaces `selfi.web3.eth().transaction(TransactionId::Hash(tx.hash))` with
        // alloy's native `Provider::get_transaction_by_hash`. Wire-level
        // RPC method (`eth_getTransactionByHash`) is unchanged. The
        // alloy `Transaction` carries the typed-envelope inner; field
        // accesses are routed through the `alloy::consensus::Transaction`
        // trait (gas_price/value/input/to/etc.) plus the wrapper's own
        // `block_number` / `inner.signer()`.
        use crate::eth::alloy_compat::assert_send_future;
        use alloy::consensus::Transaction as _;
        use alloy::providers::Provider;

        let selfi = self.clone();
        let tx = match args.fee_tx {
            TransactionEnum::SignedEthTx(t) => t.clone(),
            _ => {
                return Box::new(futures01::future::err(ERRL!(
                    "The fee transaction is not an EVM transaction"
                )))
            },
        };
        let sender_addr = try_fus!(addr_from_raw_pubkey(args.expected_sender));
        let fee_addr = try_fus!(addr_from_raw_pubkey(args.fee_addr));
        // For EVM, only fee_amount is sent on-chain (burn portion is implicit)
        let amount = args.dex_fee.fee_amount().to_decimal();
        let min_block_number = args.min_block_number;

        let fut = async move {
            let expected_value = try_s!(wei_from_big_decimal(&amount, selfi.decimals));
            let provider = selfi.alloy_provider();
            let tx_hash_alloy = alloy::primitives::B256::from_slice(&tx.hash.0);
            let tx_from_rpc = try_s!(assert_send_future(provider.get_transaction_by_hash(tx_hash_alloy)).await);
            let tx_from_rpc = match tx_from_rpc {
                Some(t) => t,
                None => return ERR!("Didn't find provided tx {:?} on ETH node", tx),
            };

            // Re-derive the legacy `ethereum_types::Address` shape that the
            // rest of this function still uses. alloy's `Address` is
            // bit-for-bit identical (20-byte H160).
            let from_addr = Address::from_slice(tx_from_rpc.inner.signer().as_slice());
            if from_addr != sender_addr {
                return ERR!(
                    "Fee tx {:?} was sent from wrong address, expected {:?}",
                    tx_from_rpc,
                    sender_addr
                );
            }

            if let Some(block_number) = tx_from_rpc.block_number {
                if block_number <= min_block_number {
                    return ERR!(
                        "Fee tx {:?} confirmed before min_block {}",
                        tx_from_rpc,
                        min_block_number,
                    );
                }
            }

            let envelope = tx_from_rpc.inner.inner();
            let to_addr = envelope.to().map(|a| Address::from_slice(a.as_slice()));
            let tx_value = {
                let bytes: [u8; 32] = envelope.value().to_be_bytes();
                U256::from_big_endian(&bytes)
            };
            let tx_input: Vec<u8> = envelope.input().to_vec();

            match &selfi.coin_type {
                EthCoinType::Eth => {
                    if to_addr != Some(fee_addr) {
                        return ERR!(
                            "Fee tx {:?} was sent to wrong address, expected {:?}",
                            tx_from_rpc,
                            fee_addr
                        );
                    }

                    if tx_value < expected_value {
                        return ERR!(
                            "Fee tx {:?} value is less than expected {:?}",
                            tx_from_rpc,
                            expected_value
                        );
                    }
                },
                EthCoinType::Erc20 {
                    platform: _,
                    token_addr,
                } => {
                    if to_addr != Some(*token_addr) {
                        return ERR!(
                            "ERC20 Fee tx {:?} called wrong smart contract, expected {:?}",
                            tx_from_rpc,
                            token_addr
                        );
                    }

                    let function = try_s!(ERC20_CONTRACT.function("transfer"));
                    let decoded_input = try_s!(function.decode_input(&tx_input[4..]));

                    if decoded_input[0] != Token::Address(fee_addr) {
                        return ERR!(
                            "ERC20 Fee tx was sent to wrong address {:?}, expected {:?}",
                            decoded_input[0],
                            fee_addr
                        );
                    }

                    match decoded_input[1] {
                        Token::Uint(value) => {
                            if value < expected_value {
                                return ERR!("ERC20 Fee tx value {} is less than expected {}", value, expected_value);
                            }
                        },
                        _ => return ERR!("Should have got uint token but got {:?}", decoded_input[1]),
                    }
                },
                // V1 ETH/ERC20 fee validation; TRON uses a separate fee validator.
                // TRON coins are wallet-only while
                // `TRON_SWAPS_AVAILABLE` is `false` (CRD ch.21 R-D1, R21.B3), so a swap never reaches this branch.
                EthCoinType::Tron | EthCoinType::Trc20 { .. } => {
                    return ERR!("TRON dex-fee validation not yet wired (pending P10.2.5)");
                },
            }

            Ok(())
        };
        Box::new(fut.boxed().compat())
    }

    fn validate_maker_payment(&self, input: ValidatePaymentInput) -> Box<dyn Future<Item = (), Error = String> + Send> {
        let swap_contract_address = try_fus!(input.swap_contract_address.try_to_address());
        self.validate_payment(
            &input.payment_tx,
            input.time_lock,
            &input.maker_pub,
            &input.secret_hash,
            input.amount,
            swap_contract_address,
        )
    }

    fn validate_taker_payment(&self, input: ValidatePaymentInput) -> Box<dyn Future<Item = (), Error = String> + Send> {
        let swap_contract_address = try_fus!(input.swap_contract_address.try_to_address());
        self.validate_payment(
            &input.payment_tx,
            input.time_lock,
            &input.taker_pub,
            &input.secret_hash,
            input.amount,
            swap_contract_address,
        )
    }

    fn check_if_my_payment_sent(
        &self,
        time_lock: u32,
        _my_pub: &[u8],
        _other_pub: &[u8],
        secret_hash: &[u8],
        from_block: u64,
        swap_contract_address: &Option<BytesJson>,
    ) -> Box<dyn Future<Item = Option<TransactionEnum>, Error = String> + Send> {
        let id = self.etomic_swap_id(time_lock, secret_hash);
        let swap_contract_address = try_fus!(swap_contract_address.try_to_address());
        let selfi = self.clone();
        let fut = async move {
            let status = try_s!(
                selfi
                    .payment_status(swap_contract_address, Token::FixedBytes(id.clone()))
                    .compat()
                    .await
            );

            if status == PAYMENT_STATE_UNINITIALIZED.into() {
                return Ok(None);
            };

            let mut current_block = try_s!(selfi.current_block().compat().await);
            if current_block < from_block {
                current_block = from_block;
            }

            let search_from_block = from_block;
            let mut from_block = from_block;

            loop {
                let to_block = selfi.log_window_end(from_block, current_block);

                let events = try_s!(
                    selfi
                        .payment_sent_events(swap_contract_address, from_block, to_block)
                        .compat()
                        .await
                );

                let found = events.iter().find(|event| &event.data.0[..32] == id.as_slice());

                match found {
                    Some(event) => {
                        // LP-17: alloy `Provider::get_transaction_by_hash` replaces
                        // `web3.eth().transaction(...)`. The fetched alloy
                        // `Transaction` is round-tripped through
                        // `signed_tx_from_alloy_tx` which reconstructs the
                        // legacy `UnverifiedTransaction` (artemii235 fork).
                        use crate::eth::alloy_compat::assert_send_future;
                        use alloy::providers::Provider;

                        let provider = selfi.alloy_provider();
                        let Some(hash) = event.transaction_hash else {
                            return ERR!("The node returned a payment event without a transaction hash");
                        };
                        let event_hash = alloy::primitives::B256::from_slice(&hash.0);
                        let transaction =
                            try_s!(assert_send_future(provider.get_transaction_by_hash(event_hash)).await);
                        match transaction {
                            Some(t) => break Ok(Some(try_s!(signed_tx_from_alloy_tx(t)).into())),
                            // CRD ch.39 R39.11.5A.6, D39.11.5A.1 (a): an event without its transaction does
                            // not show that no payment exists.
                            None => {
                                break ERR!(
                                    "Undetermined whether the payment was sent: a payment event was found in \
                                     block range {} to {}, but the node does not return its transaction {:#x}",
                                    from_block,
                                    to_block,
                                    event_hash
                                )
                            },
                        }
                    },
                    None => {
                        if to_block >= current_block {
                            // CRD ch.39 R39.11.5A.4, D39.11.5A.1 (a): this point is reached only with a
                            // contract state other than `Uninitialized` (checked above), so a search that
                            // ends without the event contradicts the state.
                            break ERR!(
                                "Undetermined whether the payment was sent: the swap contract reports a \
                                 payment for this swap, but no payment event was found in blocks {} to {}",
                                search_from_block,
                                current_block
                            );
                        }
                        from_block = to_block + 1;
                    },
                }
            }
        };
        Box::new(fut.boxed().compat())
    }

    async fn search_for_swap_tx_spend_my(
        &self,
        _time_lock: u32,
        _my_pub: &[u8],
        _other_pub: &[u8],
        _secret_hash: &[u8],
        tx: &[u8],
        search_from_block: u64,
        swap_contract_address: &Option<BytesJson>,
    ) -> Result<Option<FoundSwapTxSpend>, String> {
        let swap_contract_address = try_s!(swap_contract_address.try_to_address());
        self.search_for_swap_tx_spend(tx, swap_contract_address, search_from_block)
            .await
    }

    async fn search_for_swap_tx_spend_other(
        &self,
        _time_lock: u32,
        _my_pub: &[u8],
        _other_pub: &[u8],
        _secret_hash: &[u8],
        tx: &[u8],
        search_from_block: u64,
        swap_contract_address: &Option<BytesJson>,
    ) -> Result<Option<FoundSwapTxSpend>, String> {
        let swap_contract_address = try_s!(swap_contract_address.try_to_address());
        self.search_for_swap_tx_spend(tx, swap_contract_address, search_from_block)
            .await
    }

    fn extract_secret(&self, secret_hash: &[u8], spend_tx: &[u8]) -> Result<Vec<u8>, String> {
        let unverified: UnverifiedTransaction = try_s!(rlp::decode(spend_tx));
        match secret_of_claim_call(&unverified.data, secret_hash)? {
            Some(secret) => Ok(secret),
            None => ERR!("The call input of the spend is not a 'receiverSpend' call with the secret of the swap"),
        }
    }

    /// CRD ch.51 §51.6E R86: the call input first, then the `secret` word of the `ReceiverSpent` logs of the
    /// transaction's receipt. A value is the secret only when it passes the secret-hash rule.
    async fn extract_secret_from_spend(&self, secret_hash: &[u8], spend_tx: &[u8]) -> Result<Vec<u8>, String> {
        use crate::eth::alloy_compat::assert_send_future;
        use alloy::providers::Provider;

        let unverified: UnverifiedTransaction = try_s!(rlp::decode(spend_tx));
        if let Some(secret) = secret_of_claim_call(&unverified.data, secret_hash)? {
            return Ok(secret);
        }

        let spend_hash = unverified.hash();
        let provider = self.alloy_provider();
        let alloy_hash = alloy::primitives::B256::from_slice(&spend_hash.0);
        let receipt = match assert_send_future(provider.get_transaction_receipt(alloy_hash)).await {
            Ok(Some(receipt)) => receipt,
            Ok(None) => {
                return ERR!(
                    "The node returned no receipt for the spend transaction {:02x}",
                    spend_hash
                )
            },
            Err(e) => {
                return ERR!(
                    "Error {} on getting the receipt of the spend transaction {:02x}",
                    e,
                    spend_hash
                )
            },
        };
        let logs: Vec<(Option<Vec<u8>>, Vec<u8>)> = receipt
            .logs()
            .iter()
            .map(|log| {
                (
                    log.topics().first().map(|topic| topic.as_slice().to_vec()),
                    log.data().data.to_vec(),
                )
            })
            .collect();
        match secret_of_spent_logs(&logs, secret_hash)? {
            Some(secret) => Ok(secret),
            None => ERR!(
                "Neither the call input nor the logs of the spend transaction {:02x} carry the secret of the swap",
                spend_hash
            ),
        }
    }

    /// The Tron family keeps the wall-clock rule, with no chain read (CRD ch.51 §51.6E R92 item 3). Every other
    /// EVM coin reads the latest block's timestamp once the local clock is past the lock time
    /// (CRD ch.51 §51.6E R88). A failed read, or a block without a timestamp, is an error.
    fn can_refund_htlc(&self, locktime: u64) -> Box<dyn Future<Item = CanRefundHtlc, Error = String> + Send + '_> {
        let fut = async move {
            let now = now_ms() / 1000;
            if self.is_tron_family() {
                return Ok(wall_clock_refund_eligibility(now, locktime));
            }
            if now < locktime {
                return Ok(evm_refund_eligibility(now, locktime, None));
            }
            let tip_ts = self.latest_block_timestamp().await?;
            Ok(evm_refund_eligibility(now, locktime, Some(tip_ts)))
        };
        Box::new(fut.boxed().compat())
    }

    fn negotiate_swap_contract_addr(
        &self,
        other_side_address: Option<&[u8]>,
    ) -> Result<Option<BytesJson>, MmError<NegotiateSwapContractAddrErr>> {
        // A wallet-only coin may be activated without a swap contract (the zero address); it can never trade.
        if self.swap_contract_address == Address::default() {
            return MmError::err(NegotiateSwapContractAddrErr::NoSwapContract);
        }
        match other_side_address {
            Some(bytes) => {
                if bytes.len() != 20 {
                    return MmError::err(NegotiateSwapContractAddrErr::InvalidOtherAddrLen(bytes.into()));
                }
                let other_addr = Address::from_slice(bytes);
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

    fn get_htlc_key_pair(&self) -> Option<keys::KeyPair> { None }

    /// An EVM coin with a software signer announces the key its signer signs with, in compressed form (CRD
    /// ch.05 R41 item 3). The swap start announces and saves it only when it is not the node's persistent key.
    /// A hardware signer (Trezor) and a key-less signer (MetaMask) have no secret to offer and keep today's
    /// behavior: the node's persistent key.
    fn own_signing_key_pair(&self) -> Option<keys::KeyPair> {
        if self.uses_hardware_signer() {
            return None;
        }
        let secret = self.signer.local_secret()?;
        crypto::privkey::key_pair_from_secret(secret.as_bytes()).ok()
    }

    /// An EVM coin pays from its signer (CRD ch.05 R36 item 2, R44). With a software signer the swap key is the
    /// signer's own key and the two agree by construction; a signer that supplies no key of its own (MetaMask)
    /// announces the node's persistent key and agrees only when it is the signer. A hardware signer is out of
    /// the rule's scope.
    fn swap_key_disagreement(&self, node_pubkey: &keys::Public) -> Option<crate::SwapKeyDisagreement> {
        if self.uses_hardware_signer() {
            return None;
        }
        let node_bytes: &[u8] = node_pubkey;
        let swap_key = match self.own_signing_key_pair() {
            Some(pair) => pair.public().to_vec(),
            None => node_bytes.to_vec(),
        };
        (compressed_secp256k1_public(&self.signer.public())[..] != swap_key[..])
            .then_some(crate::SwapKeyDisagreement::EvmSigner)
    }
}

#[async_trait]
impl WatcherOps for EthCoin {}

impl EthCoin {
    /// The timestamp of the latest block, read through the coin's node failover.
    async fn latest_block_timestamp(&self) -> Result<u64, String> {
        use crate::eth::alloy_compat::assert_send_future;
        use alloy::eips::BlockNumberOrTag;
        use alloy::providers::Provider;
        use std::future::IntoFuture;

        let provider = self.alloy_provider();
        match assert_send_future(provider.get_block_by_number(BlockNumberOrTag::Latest).into_future()).await {
            Ok(Some(block)) => Ok(block.header.timestamp),
            Ok(None) => ERR!("The node returned no latest block"),
            Err(e) => ERR!("Error {} on getting the latest block", e),
        }
    }
}

/// The longest wait, in seconds, after the latest block's timestamp has not passed the lock time while the
/// local clock has: the node is behind, so the stage asks again soon.
pub(crate) const EVM_TIP_RECHECK_MAX_SECS: u64 = 60;

/// The refund gate of an EVM coin (CRD ch.51 §51.6E R88): while the local clock `now` has not passed the
/// lock time the answer is a wait of at most one hour and no chain read is needed. Afterwards the latest
/// block's timestamp `tip_ts` must be past the lock time; otherwise (no timestamp read, or the tip at or
/// before the lock time) the answer is a wait of at most [`EVM_TIP_RECHECK_MAX_SECS`].
pub(crate) fn evm_refund_eligibility(now: u64, locktime: u64, tip_ts: Option<u64>) -> CanRefundHtlc {
    if now < locktime {
        return capped_refund_wait(locktime - now + 1);
    }
    match tip_ts {
        Some(tip_ts) if tip_ts > locktime => CanRefundHtlc::CanRefundNow,
        Some(tip_ts) => CanRefundHtlc::HaveToWait((locktime - tip_ts + 1).min(EVM_TIP_RECHECK_MAX_SECS)),
        None => CanRefundHtlc::HaveToWait(EVM_TIP_RECHECK_MAX_SECS),
    }
}

#[cfg(test)]
mod refund_gate_tests {
    use super::*;

    #[test]
    fn the_tron_family_rule_is_the_wall_clock_rule_around_the_lock_time() {
        // `can_refund_htlc` answers a Tron-family coin with `wall_clock_refund_eligibility` and no chain read;
        // the trait default is the same function.
        let lock = 5000;
        assert!(matches!(
            wall_clock_refund_eligibility(lock - 1, lock),
            CanRefundHtlc::HaveToWait(2)
        ));
        assert!(matches!(
            wall_clock_refund_eligibility(lock, lock),
            CanRefundHtlc::HaveToWait(1)
        ));
        assert!(matches!(
            wall_clock_refund_eligibility(lock + 1, lock),
            CanRefundHtlc::CanRefundNow
        ));
    }

    #[test]
    fn evm_refund_gate_waits_before_the_lock_time_on_the_local_clock() {
        assert!(matches!(
            evm_refund_eligibility(1000, 1100, None),
            CanRefundHtlc::HaveToWait(101)
        ));
        assert!(matches!(
            evm_refund_eligibility(1000, 100_000, None),
            CanRefundHtlc::HaveToWait(3600)
        ));
    }

    #[test]
    fn evm_refund_gate_waits_while_the_tip_is_not_past_the_lock_time() {
        assert!(matches!(
            evm_refund_eligibility(2000, 1000, Some(1000)),
            CanRefundHtlc::HaveToWait(1)
        ));
        assert!(matches!(
            evm_refund_eligibility(2000, 1000, Some(990)),
            CanRefundHtlc::HaveToWait(11)
        ));
    }

    #[test]
    fn evm_refund_gate_opens_when_the_tip_is_past_the_lock_time() {
        assert!(matches!(
            evm_refund_eligibility(2000, 1000, Some(1001)),
            CanRefundHtlc::CanRefundNow
        ));
    }

    #[test]
    fn evm_refund_gate_caps_the_wait_when_the_tip_is_far_behind() {
        assert!(matches!(
            evm_refund_eligibility(1_000_000, 100_000, Some(1000)),
            CanRefundHtlc::HaveToWait(60)
        ));
    }

    #[test]
    fn evm_refund_gate_waits_the_gap_when_the_tip_is_10_s_behind() {
        assert!(matches!(
            evm_refund_eligibility(2000, 1000, Some(990)),
            CanRefundHtlc::HaveToWait(11)
        ));
    }

    #[test]
    fn evm_refund_gate_waits_without_a_tip_timestamp() {
        assert!(matches!(
            evm_refund_eligibility(2000, 1000, None),
            CanRefundHtlc::HaveToWait(60)
        ));
    }
}

/// The secret of `secret_hash` in the call input of a spend: the third argument of a `receiverSpend` call, when
/// it passes the secret-hash rule (CRD ch.51 §51.6E R86). `Ok(None)` when the input is not such a call.
///
/// # Errors
///
/// `secret_hash` has an unsupported width.
fn secret_of_claim_call(input: &[u8], secret_hash: &[u8]) -> Result<Option<Vec<u8>>, String> {
    let function = try_s!(SWAP_CONTRACT.function("receiverSpend"));
    let Some((selector, args)) = input.split_at_checked(4) else {
        return Ok(None);
    };
    if selector != function.short_signature() {
        return Ok(None);
    }
    let Ok(tokens) = function.decode_input(args) else {
        return Ok(None);
    };
    match tokens.get(2) {
        Some(Token::FixedBytes(secret)) => {
            crypto::secret_hash_algo::secret_matches_hash(secret, secret_hash).map(|ok| ok.then(|| secret.clone()))
        },
        _ => Ok(None),
    }
}

/// The first `secret` word of a `ReceiverSpent(bytes32 id, bytes32 secret)` log that passes the secret-hash rule
/// (CRD ch.51 §51.6E R86). Each log is its first topic and its data; only a log whose first topic is the
/// event's signature and whose data is the two words id and secret counts.
///
/// # Errors
///
/// `secret_hash` has an unsupported width.
fn secret_of_spent_logs(logs: &[(Option<Vec<u8>>, Vec<u8>)], secret_hash: &[u8]) -> Result<Option<Vec<u8>>, String> {
    let event_topic = try_s!(SWAP_CONTRACT.event("ReceiverSpent")).signature();
    for (topic, data) in logs {
        if topic.as_deref() != Some(event_topic.as_bytes()) || data.len() != 64 {
            continue;
        }
        let secret = &data[32..];
        if crypto::secret_hash_algo::secret_matches_hash(secret, secret_hash)? {
            return Ok(Some(secret.to_vec()));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod extract_secret_tests {
    use super::*;
    use crypto::secret_hash_algo::SecretHashAlgo;

    const SECRET: [u8; 32] = [0x44; 32];

    fn claim_input(function: &str, secret: &[u8]) -> Vec<u8> {
        SWAP_CONTRACT
            .function(function)
            .unwrap()
            .encode_input(&[
                Token::FixedBytes(vec![0x11; 32]),
                Token::Uint(1000.into()),
                Token::FixedBytes(secret.to_vec()),
                Token::Address(Address::from_low_u64_be(1)),
                Token::Address(Address::from_low_u64_be(2)),
            ])
            .unwrap()
    }

    fn secret_hash() -> Vec<u8> { SecretHashAlgo::DHASH160.hash_secret(&SECRET) }

    fn spent_topic() -> Vec<u8> {
        SWAP_CONTRACT
            .event("ReceiverSpent")
            .unwrap()
            .signature()
            .as_bytes()
            .to_vec()
    }

    fn spent_data(secret: &[u8]) -> Vec<u8> { [&[0x11u8; 32][..], secret].concat() }

    #[test]
    fn claim_call_with_the_matching_secret_gives_the_secret() {
        let input = claim_input("receiverSpend", &SECRET);
        assert_eq!(secret_of_claim_call(&input, &secret_hash()), Ok(Some(SECRET.to_vec())));
    }

    #[test]
    fn claim_call_with_another_secret_is_no_candidate() {
        let input = claim_input("receiverSpend", &[0x45; 32]);
        assert_eq!(secret_of_claim_call(&input, &secret_hash()), Ok(None));
    }

    #[test]
    fn call_of_another_selector_with_the_same_layout_is_no_candidate() {
        // Only the `receiverSpend` selector marks a claim, whatever the argument words hold.
        let mut input = claim_input("receiverSpend", &SECRET);
        input[..4].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(secret_of_claim_call(&input, &secret_hash()), Ok(None));
    }

    #[test]
    fn call_input_shorter_than_a_selector_is_no_candidate() {
        for len in 0..4 {
            assert_eq!(secret_of_claim_call(&vec![0xed; len], &secret_hash()), Ok(None));
        }
    }

    #[test]
    fn claim_call_with_a_wrong_hash_width_is_an_error() {
        let input = claim_input("receiverSpend", &SECRET);
        assert!(secret_of_claim_call(&input, &[0u8; 19]).is_err());
    }

    #[test]
    fn spent_log_with_the_matching_secret_gives_the_secret() {
        let logs = vec![(Some(spent_topic()), spent_data(&SECRET))];
        assert_eq!(secret_of_spent_logs(&logs, &secret_hash()), Ok(Some(SECRET.to_vec())));
    }

    #[test]
    fn spent_log_with_another_secret_is_no_candidate() {
        let logs = vec![(Some(spent_topic()), spent_data(&[0x45; 32]))];
        assert_eq!(secret_of_spent_logs(&logs, &secret_hash()), Ok(None));
    }

    #[test]
    fn log_of_another_topic_is_ignored() {
        let logs = vec![(Some(vec![0x99; 32]), spent_data(&SECRET)), (None, spent_data(&SECRET))];
        assert_eq!(secret_of_spent_logs(&logs, &secret_hash()), Ok(None));
    }

    #[test]
    fn spent_log_with_data_of_another_size_is_ignored() {
        let logs = vec![(Some(spent_topic()), SECRET.to_vec())];
        assert_eq!(secret_of_spent_logs(&logs, &secret_hash()), Ok(None));
    }

    #[test]
    fn the_first_matching_log_wins() {
        let logs = vec![
            (Some(spent_topic()), spent_data(&[0x45; 32])),
            (Some(spent_topic()), spent_data(&SECRET)),
        ];
        assert_eq!(secret_of_spent_logs(&logs, &secret_hash()), Ok(Some(SECRET.to_vec())));
    }
}
