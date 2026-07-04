//! SwapOps and WatcherOps trait implementations for EthCoin.

use super::*;

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
            _ => panic!(),
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
                // Activation gating prevents this branch. P10.2.5.
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

            let mut from_block = from_block;

            loop {
                let to_block = current_block.min(from_block + selfi.logs_block_range);

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
                        let event_hash = alloy::primitives::B256::from_slice(&event.transaction_hash.unwrap().0);
                        let transaction =
                            try_s!(assert_send_future(provider.get_transaction_by_hash(event_hash)).await);
                        match transaction {
                            Some(t) => break Ok(Some(try_s!(signed_tx_from_alloy_tx(t)).into())),
                            None => break Ok(None),
                        }
                    },
                    None => {
                        if to_block >= current_block {
                            break Ok(None);
                        }
                        from_block = to_block;
                    },
                }
            }
        };
        Box::new(fut.boxed().compat())
    }

    async fn search_for_swap_tx_spend_my(
        &self,
        _time_lock: u32,
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

    fn extract_secret(&self, _secret_hash: &[u8], spend_tx: &[u8]) -> Result<Vec<u8>, String> {
        let unverified: UnverifiedTransaction = try_s!(rlp::decode(spend_tx));
        let function = try_s!(SWAP_CONTRACT.function("receiverSpend"));
        let tokens = try_s!(function.decode_input(&unverified.data[4..]));
        if tokens.len() < 3 {
            return ERR!("Invalid arguments in 'receiverSpend' call: {:?}", tokens);
        }
        match &tokens[2] {
            Token::FixedBytes(secret) => Ok(secret.to_vec()),
            _ => ERR!(
                "Expected secret to be fixed bytes, decoded function data is {:?}",
                tokens
            ),
        }
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
}

#[async_trait]
impl WatcherOps for EthCoin {}
