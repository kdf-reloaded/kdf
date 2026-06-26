//! SwapOps and WatcherOps trait implementations for EthCoin.

use super::*;

/// True for the TRON coin family (native TRX or a TRC20 token). Version-1 swap
/// operations route here to TRON-specific behaviour (R-S1).
fn is_tron_family(coin_type: &EthCoinType) -> bool {
    matches!(coin_type, EthCoinType::Tron | EthCoinType::Trc20 { .. })
}

/// Coerce a swap secret / secret-hash to the 32-byte form the TRON swap
/// contract uses (R-S2 / R-SA1).
fn tron_bytes32(label: &str, bytes: &[u8]) -> Result<[u8; 32], String> {
    if bytes.len() != 32 {
        return ERR!("TRON swap {} must be 32 bytes, got {}", label, bytes.len());
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(bytes);
    Ok(out)
}

impl EthCoin {
    /// Resolve the TRON swap-contract address for a swap operation (R-A3/R-D1):
    /// prefer the negotiated address, falling back to the coin's activated
    /// swap-contract address. A TRON coin activated without a swap-contract
    /// address (wallet-only) yields an error here, keeping it off every swap
    /// path (R-D1).
    pub(crate) fn tron_swap_contract(&self, negotiated: &Option<BytesJson>) -> Result<Address, String> {
        let from_arg = negotiated.try_to_address().ok().filter(|a| !a.is_zero());
        let addr = from_arg.unwrap_or(self.swap_contract_address);
        if addr.is_zero() {
            return ERR!("TRON coin has no swap-contract address configured (wallet-only)");
        }
        Ok(addr)
    }

    /// Validate a counterparty's TRON HTLC payment (R-L2). `funder_pub` is the
    /// payer's public key; the validator (self) is the receiver.
    fn validate_tron_payment(
        &self,
        input: ValidatePaymentInput,
        funder_pub: &[u8],
    ) -> Box<dyn Future<Item = (), Error = String> + Send> {
        let coin = self.clone();
        let swap_contract = try_fus!(self.tron_swap_contract(&input.swap_contract_address));
        let funder = try_fus!(addr_from_raw_pubkey(funder_pub));
        let receiver = self.my_address;
        let secret_hash = try_fus!(tron_bytes32("secret-hash", &input.secret_hash));
        let time_lock = input.time_lock as u64;
        let amount = input.amount.clone();
        let payment_tx = input.payment_tx.clone();
        let fut = async move {
            crate::eth::tron::swap_ops::validate_payment(
                coin,
                payment_tx,
                swap_contract,
                funder,
                receiver,
                secret_hash,
                time_lock,
                amount,
            )
            .await
        };
        Box::new(fut.boxed().compat())
    }

    /// Send a TRON HTLC payment (maker or taker side) (R-L1).
    fn send_tron_payment(
        &self,
        time_lock: u32,
        receiver: Address,
        secret_hash: &[u8],
        amount: BigDecimal,
        negotiated: &Option<BytesJson>,
    ) -> TransactionFut {
        let coin = self.clone();
        let swap_contract = try_tx_fus!(self.tron_swap_contract(negotiated));
        let secret_hash = try_tx_fus!(tron_bytes32("secret-hash", secret_hash));
        let fut = async move {
            crate::eth::tron::swap_ops::send_payment(coin, swap_contract, receiver, secret_hash, time_lock, amount)
                .await
                .map(TransactionEnum::from)
                .map_err(|e| TransactionErr::Plain(ERRL!("{}", e)))
        };
        Box::new(fut.boxed().compat())
    }

    /// Spend a TRON HTLC payment by revealing the secret (R-L3).
    fn spend_tron_payment(&self, payment_tx: &[u8], secret: &[u8], negotiated: &Option<BytesJson>) -> TransactionFut {
        let coin = self.clone();
        let swap_contract = try_tx_fus!(self.tron_swap_contract(negotiated));
        let secret = try_tx_fus!(tron_bytes32("secret", secret));
        let payment = payment_tx.to_vec();
        let fut = async move {
            crate::eth::tron::swap_ops::spend_payment(coin, swap_contract, payment, secret)
                .await
                .map(TransactionEnum::from)
                .map_err(|e| TransactionErr::Plain(ERRL!("{}", e)))
        };
        Box::new(fut.boxed().compat())
    }

    /// Refund a TRON HTLC payment after its lock-time has elapsed (R-L4).
    fn refund_tron_payment(&self, payment_tx: &[u8], time_lock: u32, negotiated: &Option<BytesJson>) -> TransactionFut {
        let coin = self.clone();
        let swap_contract = try_tx_fus!(self.tron_swap_contract(negotiated));
        let payment = payment_tx.to_vec();
        let fut = async move {
            crate::eth::tron::swap_ops::refund_payment(coin, swap_contract, payment, time_lock)
                .await
                .map(TransactionEnum::from)
                .map_err(|e| TransactionErr::Plain(ERRL!("{}", e)))
        };
        Box::new(fut.boxed().compat())
    }
}

#[async_trait]
impl SwapOps for EthCoin {
    fn send_taker_fee(&self, dex_fee: &DexFee, fee_addr: &[u8], _uuid: &[u8]) -> TransactionFut {
        let address = try_tx_fus!(addr_from_raw_pubkey(fee_addr));
        // For EVM coins, only the fee portion is sent on-chain; the burn
        // portion (if any) is implicit — not sent as a separate transfer.
        let amount = dex_fee.fee_amount();

        // R-DF1: a TRON coin sends the taker-fee through its dedicated transfer
        // pipeline (native TRX or TRC20), surfaced as a TronTx (R-T5).
        if is_tron_family(&self.coin_type) {
            let coin = self.clone();
            let fee_amount = amount.to_decimal();
            let fut = async move {
                crate::eth::tron::swap_ops::send_dex_fee(coin, address, fee_amount)
                    .await
                    .map(TransactionEnum::from)
                    .map_err(|e| TransactionErr::Plain(ERRL!("{}", e)))
            };
            return Box::new(fut.boxed().compat());
        }

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

        // R-L1/R-S1: route a TRON maker payment to the TRON swap flow.
        if is_tron_family(&self.coin_type) {
            return self.send_tron_payment(time_lock, taker_addr, secret_hash, amount, swap_contract_address);
        }

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

        // R-L1/R-S1: route a TRON taker payment to the TRON swap flow.
        if is_tron_family(&self.coin_type) {
            return self.send_tron_payment(time_lock, maker_addr, secret_hash, amount, swap_contract_address);
        }

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
        // R-L3/R-S1: route a TRON spend to the TRON swap flow.
        if is_tron_family(&self.coin_type) {
            return self.spend_tron_payment(taker_payment_tx, secret, swap_contract_address);
        }

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
        // R-L3/R-S1: route a TRON spend to the TRON swap flow.
        if is_tron_family(&self.coin_type) {
            return self.spend_tron_payment(maker_payment_tx, secret, swap_contract_address);
        }

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
        time_lock: u32,
        _maker_pub: &[u8],
        _secret_hash: &[u8],
        _htlc_privkey: &[u8],
        swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        // R-L4/R-S1: route a TRON refund to the TRON swap flow.
        if is_tron_family(&self.coin_type) {
            return self.refund_tron_payment(taker_payment_tx, time_lock, swap_contract_address);
        }

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
        time_lock: u32,
        _taker_pub: &[u8],
        _secret_hash: &[u8],
        _htlc_privkey: &[u8],
        swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        // R-L4/R-S1: route a TRON refund to the TRON swap flow.
        if is_tron_family(&self.coin_type) {
            return self.refund_tron_payment(maker_payment_tx, time_lock, swap_contract_address);
        }

        let tx: UnverifiedTransaction = try_tx_fus!(rlp::decode(maker_payment_tx));
        let signed = try_tx_fus!(SignedEthTx::new(tx));
        let swap_contract_address = try_tx_fus!(swap_contract_address.try_to_address());

        Box::new(
            self.refund_hash_time_locked_payment(swap_contract_address, signed)
                .map(TransactionEnum::from),
        )
    }

    fn validate_fee(&self, args: ValidateFeeArgs<'_>) -> Box<dyn Future<Item = (), Error = String> + Send> {
        // R-DF2/R-S1: a TRON coin validates the taker-fee through its dedicated
        // protobuf-decoding validator.
        if is_tron_family(&self.coin_type) {
            let recipient = try_fus!(addr_from_raw_pubkey(args.fee_addr));
            let amount = args.dex_fee.fee_amount().to_decimal();
            let fee_tx_bytes = match args.fee_tx {
                TransactionEnum::TronTx(t) => t.tx_hex(),
                other => return Box::new(futures01::future::err(ERRL!("expected a TRON fee tx, got {:?}", other))),
            };
            let res = crate::eth::tron::swap_ops::validate_dex_fee(self, &fee_tx_bytes, recipient, amount);
            return Box::new(futures01::future::result(res));
        }

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
                    let decoded_input = try_s!(function.decode_input(&tx_input));

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
                // V1 ETH/ERC20 fee validation only. A TRON coin is dispatched
                // to its protobuf-decoding validator at the top of this method,
                // so this arm is unreachable for TRON; it stays as a defensive
                // typed refusal rather than producing incorrect behaviour.
                EthCoinType::Tron | EthCoinType::Trc20 { .. } => {
                    return ERR!("TRON dex-fee validation is handled by the TRON fee validator, not the EVM path");
                },
            }

            Ok(())
        };
        Box::new(fut.boxed().compat())
    }

    fn validate_maker_payment(&self, input: ValidatePaymentInput) -> Box<dyn Future<Item = (), Error = String> + Send> {
        // R-L2/R-S1: the taker validates the maker's TRON payment (funder = maker).
        if is_tron_family(&self.coin_type) {
            let maker_pub = input.maker_pub.clone();
            return self.validate_tron_payment(input, &maker_pub);
        }
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
        // R-L2/R-S1: the maker validates the taker's TRON payment (funder = taker).
        if is_tron_family(&self.coin_type) {
            let taker_pub = input.taker_pub.clone();
            return self.validate_tron_payment(input, &taker_pub);
        }
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
        // R-L6/R-S1: discover a previously-sent TRON payment through the indexed
        // contract-event endpoint (PaymentSent). Fails with a typed error when no
        // event-indexer endpoint is configured rather than silently returning None.
        if is_tron_family(&self.coin_type) {
            let coin = self.clone();
            let swap_contract = try_fus!(self.tron_swap_contract(swap_contract_address));
            let id = crate::eth::tron::swap::swap_id(time_lock, secret_hash);
            let fut = async move { crate::eth::tron::swap_ops::check_if_payment_sent(&coin, swap_contract, id).await };
            return Box::new(fut.boxed().compat());
        }
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
        time_lock: u32,
        _other_pub: &[u8],
        secret_hash: &[u8],
        tx: &[u8],
        search_from_block: u64,
        swap_contract_address: &Option<BytesJson>,
    ) -> Result<Option<FoundSwapTxSpend>, String> {
        // R-L6/R-S1: discover the spend/refund of this payment through the indexed
        // contract-event endpoint, keyed by the swap id.
        if is_tron_family(&self.coin_type) {
            let swap_contract = self.tron_swap_contract(swap_contract_address)?;
            let id = crate::eth::tron::swap::swap_id(time_lock, secret_hash);
            return crate::eth::tron::swap_ops::search_for_swap_tx_spend(self, swap_contract, id).await;
        }
        let swap_contract_address = try_s!(swap_contract_address.try_to_address());
        self.search_for_swap_tx_spend(tx, swap_contract_address, search_from_block)
            .await
    }

    async fn search_for_swap_tx_spend_other(
        &self,
        time_lock: u32,
        _other_pub: &[u8],
        secret_hash: &[u8],
        tx: &[u8],
        search_from_block: u64,
        swap_contract_address: &Option<BytesJson>,
    ) -> Result<Option<FoundSwapTxSpend>, String> {
        // R-L6/R-S1: discover the spend/refund of this payment through the indexed
        // contract-event endpoint, keyed by the swap id.
        if is_tron_family(&self.coin_type) {
            let swap_contract = self.tron_swap_contract(swap_contract_address)?;
            let id = crate::eth::tron::swap::swap_id(time_lock, secret_hash);
            return crate::eth::tron::swap_ops::search_for_swap_tx_spend(self, swap_contract, id).await;
        }
        let swap_contract_address = try_s!(swap_contract_address.try_to_address());
        self.search_for_swap_tx_spend(tx, swap_contract_address, search_from_block)
            .await
    }

    fn extract_secret(&self, _secret_hash: &[u8], spend_tx: &[u8]) -> Result<Vec<u8>, String> {
        // R-L5/R-S1: extract the secret from a TRON receiverSpend protobuf tx.
        if is_tron_family(&self.coin_type) {
            return crate::eth::tron::swap_ops::extract_secret(spend_tx);
        }
        let unverified: UnverifiedTransaction = try_s!(rlp::decode(spend_tx));
        let function = try_s!(SWAP_CONTRACT.function("receiverSpend"));
        let tokens = try_s!(function.decode_input(&unverified.data));
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

    /// R-S2: a TRON coin's HTLC uses a 32-byte `SHA-256(secret)` payment
    /// secret-hash; Ethereum coins keep the 20-byte `RIPEMD-160(SHA-256)` form.
    fn swap_secret_hash(&self, secret: &[u8]) -> Vec<u8> {
        if is_tron_family(&self.coin_type) {
            crate::eth::tron::swap::sha256_secret_hash(secret).to_vec()
        } else {
            kdf_crypto::dhash160(secret).to_vec()
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
                let other_addr = Address::from(bytes);
                if other_addr == self.swap_contract_address {
                    return Ok(Some(self.swap_contract_address.to_vec().into()));
                }

                if Some(other_addr) == self.fallback_swap_contract {
                    return Ok(self.fallback_swap_contract.map(|addr| addr.to_vec().into()));
                }
                MmError::err(NegotiateSwapContractAddrErr::UnexpectedOtherAddr(bytes.into()))
            },
            None => self
                .fallback_swap_contract
                .map(|addr| Some(addr.to_vec().into()))
                .ok_or_else(|| MmError::new(NegotiateSwapContractAddrErr::NoOtherAddrAndNoFallback)),
        }
    }

    fn get_htlc_key_pair(&self) -> Option<keys::KeyPair> { None }
}

#[async_trait]
impl WatcherOps for EthCoin {}

#[cfg(test)]
mod swap_dispatch_tests {
    use super::*;

    /// R-S1: chain-family dispatch selects the TRON path for native TRX and
    /// TRC20 tokens, and leaves Ethereum coins on the Ethereum path.
    #[test]
    fn is_tron_family_dispatch() {
        assert!(!is_tron_family(&EthCoinType::Eth));
        assert!(!is_tron_family(&EthCoinType::Erc20 {
            platform: "ETH".to_owned(),
            token_addr: Address::default(),
        }));
        assert!(is_tron_family(&EthCoinType::Tron));
        assert!(is_tron_family(&EthCoinType::Trc20 {
            platform: "TRX".to_owned(),
            token_addr: Address::default(),
        }));
    }

    /// R-S2/R-SA1: TRON swap secret/secret-hash values must be exactly 32 bytes.
    #[test]
    fn tron_bytes32_enforces_length() {
        assert!(tron_bytes32("secret-hash", &[0u8; 20]).is_err());
        assert!(tron_bytes32("secret-hash", &[0u8; 33]).is_err());
        assert_eq!(tron_bytes32("secret", &[7u8; 32]).unwrap(), [7u8; 32]);
    }
}
