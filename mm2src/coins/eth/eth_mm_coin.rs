//! MmCoin, ParseCoinAssocTypes, and V2 swap trait implementations for EthCoin.

use super::*;
use crate::rpc_command::init_withdraw::{InitWithdrawCoin, WithdrawInProgressStatus, WithdrawTaskHandle};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct EthTxFeeDetails {
    pub(crate) coin: String,
    pub(crate) gas: u64,
    /// WEI units per 1 gas
    pub(crate) gas_price: BigDecimal,
    pub(crate) total_fee: BigDecimal,
}

impl EthTxFeeDetails {
    pub fn new(gas: U256, gas_price: U256, coin: &str) -> NumConversResult<EthTxFeeDetails> {
        let total_fee = gas * gas_price;
        // Fees are always paid in ETH, can use 18 decimals by default
        let total_fee = u256_to_big_decimal(total_fee, 18)?;
        let gas_price = u256_to_big_decimal(gas_price, 18)?;

        Ok(EthTxFeeDetails {
            coin: coin.to_owned(),
            gas: gas.as_u64(),
            gas_price,
            total_fee,
        })
    }
}

#[async_trait]
impl InitWithdrawCoin for EthCoin {
    async fn init_withdraw(
        &self,
        ctx: MmArc,
        req: WithdrawRequest,
        task_handle: &WithdrawTaskHandle,
    ) -> Result<TransactionDetails, MmError<WithdrawError>> {
        // CRD §50 / R49.6: an EVM coin under the Trezor signing policy is signed
        // by the device through the task path; route to the dedicated device flow
        // (which performs its own validation, incl. the R50.20 TRON rejection).
        #[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
        if matches!(self.signer, EthSigner::Trezor(_)) {
            return crate::eth::eth_trezor_withdraw::withdraw_trezor_impl(ctx, self.clone(), req, task_handle).await;
        }
        validate_evm_withdraw_request(self, &req)?;
        task_handle
            .update_in_progress_status(WithdrawInProgressStatus::GeneratingTransaction)
            .mm_err(WithdrawError::from)?;
        withdraw_impl(ctx, self.clone(), req).await
    }
}

#[async_trait]
impl MmCoin for EthCoin {
    fn is_asset_chain(&self) -> bool { false }

    /// CRD R47.5.12 / R47.5.13a -- an EVM coin activated under the MetaMask
    /// signing policy is a non-swap (balance / address / `withdraw`) account: it
    /// holds no local secret, so it cannot derive a per-swap HTLC key-pair or
    /// produce the detached, framework-scheduled signatures atomic swaps require.
    /// Reporting it as `wallet_only` rejects it from a swap at the earliest
    /// practical lifecycle point -- order placement (`buy` / `sell` / `setprice`
    /// all gate on `wallet_only`) -- with a clean structured error, instead of
    /// letting it reach and abort inside a later HTLC key-derivation / signing
    /// path. WASM-only: the MetaMask policy exists only on the browser target.
    fn wallet_only(&self, ctx: &MmArc) -> bool {
        #[cfg(target_arch = "wasm32")]
        if matches!(self.signer, EthSigner::Metamask(_)) {
            return true;
        }
        let coin_conf = crate::coin_conf(ctx, self.ticker());
        coin_conf["wallet_only"].as_bool().unwrap_or(false)
    }

    fn get_raw_transaction(&self, req: RawTransactionRequest) -> RawTransactionFut {
        Box::new(get_raw_transaction_impl(self.clone(), req).boxed().compat())
    }

    fn withdraw(&self, req: WithdrawRequest) -> WithdrawFut {
        let ctx = try_f!(MmArc::from_weak(&self.ctx).or_mm_err(|| WithdrawError::InternalError("!ctx".to_owned())));
        // CRD R50.24 / R49.6: the direct legacy `withdraw` method does not support
        // Trezor user-action signing; clients must use the `task::withdraw` API.
        #[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
        if matches!(self.signer, EthSigner::Trezor(_)) {
            return Box::new(futures01::future::err(MmError::new(
                WithdrawError::UnsupportedUnderTrezor(
                    "Trezor EVM withdrawals require the 'task::withdraw' API for device user-action signing".to_owned(),
                ),
            )));
        }
        Box::new(Box::pin(withdraw_impl(ctx, self.clone(), req)).compat())
    }

    fn decimals(&self) -> u8 { self.decimals }

    fn convert_to_address(&self, from: &str, to_address_format: Json) -> Result<String, String> {
        let to_address_format: EthAddressFormat =
            json::from_value(to_address_format).map_err(|e| ERRL!("Error on parse ETH address format {:?}", e))?;
        match to_address_format {
            EthAddressFormat::SingleCase => ERR!("conversion is available only to mixed-case"),
            EthAddressFormat::MixedCase => {
                let _addr = try_s!(addr_from_str(from));
                Ok(checksum_address(from))
            },
        }
    }

    fn validate_address(&self, address: &str) -> ValidateAddressResult {
        let result = self.address_from_str(address);
        ValidateAddressResult {
            is_valid: result.is_ok(),
            reason: result.err(),
        }
    }

    fn process_history_loop(&self, ctx: MmArc) -> Box<dyn Future<Item = (), Error = ()> + Send> {
        cfg_wasm32! {
            ctx.log.log(
                "🤔",
                &[&"tx_history", &self.ticker],
                &ERRL!("Transaction history is not supported for ETH/ERC20 coins"),
            );
            return Box::new(futures01::future::ok(()));
        }
        cfg_native! {
            let coin = self.clone();
            let fut = async move {
                match coin.coin_type {
                    EthCoinType::Eth => coin.process_eth_history(&ctx).await,
                    EthCoinType::Erc20 { ref token_addr, .. } => coin.process_erc20_history(*token_addr, &ctx).await,
                    // TRON tx history is fetched from a different API entirely
                    // (TronGrid /v1/accounts/{address}/transactions). Wired in P10.2.5.
                    EthCoinType::Tron | EthCoinType::Trc20 { .. } => {
                        log!("TRON tx history not yet wired (pending P10.2.5)");
                    },
                }
                Ok(())
            };
            Box::new(fut.boxed().compat())
        }
    }

    fn history_sync_status(&self) -> HistorySyncState { self.history_sync_state.lock().unwrap().clone() }

    fn get_trade_fee(&self) -> Box<dyn Future<Item = TradeFee, Error = String> + Send> {
        let coin = self.clone();
        Box::new(
            self.get_gas_price()
                .map_err(|e| e.to_string())
                .and_then(move |gas_price| {
                    let fee = gas_price * U256::from(150_000);
                    let fee_coin = match &coin.coin_type {
                        EthCoinType::Eth => &coin.ticker,
                        EthCoinType::Erc20 { platform, .. } => platform,
                        // For TRON the fee is paid in TRX. TRC20 references its parent.
                        EthCoinType::Tron => &coin.ticker,
                        EthCoinType::Trc20 { platform, .. } => platform,
                    };
                    Ok(TradeFee {
                        coin: fee_coin.into(),
                        amount: try_s!(u256_to_big_decimal(fee, 18)).into(),
                        paid_from_trading_vol: false,
                    })
                }),
        )
    }

    async fn get_sender_trade_fee(
        &self,
        value: TradePreimageValue,
        stage: FeeApproxStage,
    ) -> TradePreimageResult<TradeFee> {
        let gas_price = self.get_gas_price().compat().await.mm_err(Into::into)?;
        let gas_price = increase_gas_price_by_stage(gas_price, &stage);
        let gas_limit = match self.coin_type {
            EthCoinType::Eth => {
                // this gas_limit includes gas for `ethPayment` and `senderRefund` contract calls
                U256::from(300_000)
            },
            EthCoinType::Erc20 { token_addr, .. } => {
                let value = match value {
                    TradePreimageValue::Exact(value) | TradePreimageValue::UpperBound(value) => {
                        wei_from_big_decimal(&value, self.decimals).mm_err(Into::into)?
                    },
                };
                let allowed = self
                    .allowance(self.swap_contract_address)
                    .compat()
                    .await
                    .mm_err(Into::into)?;
                if allowed < value {
                    // estimate gas for the `approve` contract call

                    // Pass a dummy spender. Let's use `my_address`.
                    let spender = self.my_address;
                    let approve_function = ERC20_CONTRACT.function("approve")?;
                    let approve_data = approve_function.encode_input(&[Token::Address(spender), Token::Uint(value)])?;
                    let approve_gas_limit = self
                        .estimate_gas_for_contract_call(token_addr, Bytes::from(approve_data))
                        .compat()
                        .await
                        .mm_err(Into::into)?;

                    // this gas_limit includes gas for `approve`, `erc20Payment` and `senderRefund` contract calls
                    U256::from(300_000) + approve_gas_limit
                } else {
                    // this gas_limit includes gas for `erc20Payment` and `senderRefund` contract calls
                    U256::from(300_000)
                }
            },
            // Trade-fee preimage for V1 ETH-style HTLC swaps; TRON uses a
            // bandwidth/energy fee model handled separately. Wired in P10.2.5.
            EthCoinType::Tron | EthCoinType::Trc20 { .. } => {
                unimplemented!("TRON V1 sender trade fee not wired (pending P10.2.5)")
            },
        };

        let total_fee = gas_limit * gas_price;
        let amount = u256_to_big_decimal(total_fee, 18).mm_err(Into::into)?;
        let fee_coin = match &self.coin_type {
            EthCoinType::Eth => &self.ticker,
            EthCoinType::Erc20 { platform, .. } => platform,
            EthCoinType::Tron => &self.ticker,
            EthCoinType::Trc20 { platform, .. } => platform,
        };
        Ok(TradeFee {
            coin: fee_coin.into(),
            amount: amount.into(),
            paid_from_trading_vol: false,
        })
    }

    fn get_receiver_trade_fee(&self, stage: FeeApproxStage) -> TradePreimageFut<TradeFee> {
        let coin = self.clone();
        let fut = async move {
            let gas_price = coin.get_gas_price().compat().await.mm_err(Into::into)?;
            let gas_price = increase_gas_price_by_stage(gas_price, &stage);
            let total_fee = gas_price * U256::from(150_000);
            let amount = u256_to_big_decimal(total_fee, 18).mm_err(Into::into)?;
            let fee_coin = match &coin.coin_type {
                EthCoinType::Eth => &coin.ticker,
                EthCoinType::Erc20 { platform, .. } => platform,
                EthCoinType::Tron => &coin.ticker,
                EthCoinType::Trc20 { platform, .. } => platform,
            };
            Ok(TradeFee {
                coin: fee_coin.into(),
                amount: amount.into(),
                paid_from_trading_vol: false,
            })
        };
        Box::new(fut.boxed().compat())
    }

    async fn get_fee_to_send_taker_fee(
        &self,
        dex_fee_amount: BigDecimal,
        stage: FeeApproxStage,
    ) -> TradePreimageResult<TradeFee> {
        let dex_fee_amount = wei_from_big_decimal(&dex_fee_amount, self.decimals).mm_err(Into::into)?;

        // pass the dummy params — preimage destination only, never broadcast.
        let netid = MmArc::from_weak(&self.ctx)
            .map(|c| c.netid())
            .unwrap_or(mm2_net_config::SUPPORTED_NETIDS[0]);
        let net_cfg = mm2_net_config::net_config_for(netid).ok_or_else(|| {
            TradePreimageError::InternalError(format!("no compiled network configuration for netid {netid}"))
        })?;
        let to_addr = addr_from_raw_pubkey(net_cfg.dex_fee_addr_raw_pubkey())
            .expect("addr_from_raw_pubkey should never fail with NetConfig dex_fee raw pubkey");
        let (eth_value, data, call_addr, fee_coin) = match &self.coin_type {
            EthCoinType::Eth => (dex_fee_amount, Vec::new(), &to_addr, &self.ticker),
            EthCoinType::Erc20 { platform, token_addr } => {
                let function = ERC20_CONTRACT.function("transfer")?;
                let data = function.encode_input(&[Token::Address(to_addr), Token::Uint(dex_fee_amount)])?;
                (0.into(), data, token_addr, platform)
            },
            // ETH-style fee preimage; TRON dex-fee preimage flows through the
            // dedicated TRON estimator. Activation gating prevents this branch. P10.2.5.
            EthCoinType::Tron | EthCoinType::Trc20 { .. } => {
                return MmError::err(TradePreimageError::InternalError(
                    "TRON dex-fee preimage not yet wired (pending P10.2.5)".to_owned(),
                ));
            },
        };

        let gas_price = self.get_gas_price().compat().await.mm_err(Into::into)?;
        let gas_price = increase_gas_price_by_stage(gas_price, &stage);
        let estimate_gas_req = CallRequest {
            value: Some(eth_value),
            data: Some(data.clone().into()),
            from: Some(self.my_address),
            to: *call_addr,
            gas: None,
            // gas price must be supplied because some smart contracts base their
            // logic on gas price, e.g. TUSD: https://github.com/KomodoPlatform/atomicDEX-API/issues/643
            gas_price: Some(gas_price),
        };

        // Please note if the wallet's balance is insufficient to withdraw, then `estimate_gas` may fail with the `Exception` error.
        // Ideally we should determine the case when we have the insufficient balance and return `TradePreimageError::NotSufficientBalance` error.
        let gas_limit = self
            .estimate_gas(estimate_gas_req)
            .compat()
            .await
            .mm_err(TradePreimageError::from)?;
        let total_fee = gas_limit * gas_price;
        let amount = u256_to_big_decimal(total_fee, 18).mm_err(Into::into)?;
        Ok(TradeFee {
            coin: fee_coin.into(),
            amount: amount.into(),
            paid_from_trading_vol: false,
        })
    }

    fn required_confirmations(&self) -> u64 { self.required_confirmations.load(AtomicOrdering::Relaxed) }

    fn requires_notarization(&self) -> bool { false }

    fn set_required_confirmations(&self, confirmations: u64) {
        self.required_confirmations
            .store(confirmations, AtomicOrdering::Relaxed);
    }

    fn set_requires_notarization(&self, _requires_nota: bool) {
        log!("Warning: set_requires_notarization doesn't take any effect on ETH/ERC20 coins");
    }

    fn swap_contract_address(&self) -> Option<BytesJson> {
        Some(BytesJson::from(self.swap_contract_address.0.as_ref()))
    }

    fn mature_confirmations(&self) -> Option<u32> { None }

    fn coin_protocol_info(&self) -> Vec<u8> { Vec::new() }

    fn is_coin_protocol_supported(&self, _info: &Option<Vec<u8>>) -> bool { true }
}

#[async_trait]
impl ParseCoinAssocTypes for EthCoin {
    type Address = Address;
    type AddressParseError = MmError<EthAssocTypesError>;
    type Pubkey = Public;
    type PubkeyParseError = MmError<EthAssocTypesError>;
    type Tx = SignedEthTx;
    type TxParseError = MmError<EthAssocTypesError>;
    type Preimage = Vec<u8>;
    type PreimageParseError = MmError<EthAssocTypesError>;
    type Sig = Vec<u8>;
    type SigParseError = MmError<EthAssocTypesError>;

    async fn my_addr(&self) -> Self::Address { self.my_address }

    fn parse_address(&self, address: &str) -> Result<Self::Address, Self::AddressParseError> {
        Address::from_str(address).map_to_mm(|e| EthAssocTypesError::InvalidHexString(e.to_string()))
    }

    fn parse_pubkey(&self, pubkey: &[u8]) -> Result<Self::Pubkey, Self::PubkeyParseError> {
        if pubkey.len() != 64 {
            return MmError::err(EthAssocTypesError::InvalidHexString(format!(
                "Expected 64-byte public key, got {} bytes",
                pubkey.len()
            )));
        }
        Ok(Public::from_slice(pubkey))
    }

    fn parse_tx(&self, tx: &[u8]) -> Result<Self::Tx, Self::TxParseError> {
        signed_eth_tx_from_bytes(tx).map_to_mm(EthAssocTypesError::TxParseError)
    }

    fn parse_preimage(&self, preimage: &[u8]) -> Result<Self::Preimage, Self::PreimageParseError> {
        Ok(preimage.to_vec())
    }

    fn parse_signature(&self, sig: &[u8]) -> Result<Self::Sig, Self::SigParseError> { Ok(sig.to_vec()) }
}

// ─── CommonSwapOpsV2 for EthCoin ────────────────────────────────────────────

#[async_trait]
impl CommonSwapOpsV2 for EthCoin {
    fn derive_htlc_pubkey_v2(&self, _swap_unique_data: &[u8]) -> Public { self.signer.public() }

    fn derive_htlc_pubkey_v2_bytes(&self, swap_unique_data: &[u8]) -> Vec<u8> {
        self.derive_htlc_pubkey_v2(swap_unique_data).as_bytes().to_vec()
    }
}

// ─── MakerCoinSwapOpsV2 for EthCoin ────────────────────────────────────────

#[async_trait]
impl MakerCoinSwapOpsV2 for EthCoin {
    async fn send_maker_payment_v2(&self, args: SendMakerPaymentArgs<'_, Self>) -> Result<SignedEthTx, TransactionErr> {
        self.send_maker_payment_v2_impl(args).await
    }

    async fn validate_maker_payment_v2(&self, args: ValidateMakerPaymentArgs<'_, Self>) -> ValidateSwapV2TxResult {
        self.validate_maker_payment_v2_impl(args).await
    }

    async fn refund_maker_payment_v2_timelock(
        &self,
        args: RefundMakerPaymentTimelockArgs<'_>,
    ) -> Result<SignedEthTx, TransactionErr> {
        self.refund_maker_payment_v2_timelock_impl(args).await
    }

    async fn refund_maker_payment_v2_secret(
        &self,
        args: RefundMakerPaymentSecretArgs<'_, Self>,
    ) -> Result<SignedEthTx, TransactionErr> {
        self.refund_maker_payment_v2_secret_impl(args).await
    }

    async fn spend_maker_payment_v2(
        &self,
        args: SpendMakerPaymentArgs<'_, Self>,
    ) -> Result<SignedEthTx, TransactionErr> {
        self.spend_maker_payment_v2_impl(args).await
    }
}

// ─── TakerCoinSwapOpsV2 for EthCoin ────────────────────────────────────────

#[async_trait]
impl TakerCoinSwapOpsV2 for EthCoin {
    async fn send_taker_funding(&self, args: SendTakerFundingArgs<'_>) -> Result<SignedEthTx, TransactionErr> {
        self.send_taker_funding_impl(args).await
    }

    async fn validate_taker_funding(&self, args: ValidateTakerFundingArgs<'_, Self>) -> ValidateSwapV2TxResult {
        self.validate_taker_funding_impl(args).await
    }

    async fn refund_taker_funding_timelock(
        &self,
        args: RefundTakerPaymentArgs<'_>,
    ) -> Result<SignedEthTx, TransactionErr> {
        self.refund_taker_payment_with_timelock_impl(args).await
    }

    async fn refund_taker_funding_secret(
        &self,
        args: RefundFundingSecretArgs<'_, Self>,
    ) -> Result<SignedEthTx, TransactionErr> {
        self.refund_taker_funding_secret_impl(args).await
    }

    async fn search_for_taker_funding_spend(
        &self,
        tx: &SignedEthTx,
        _from_block: u64,
        _secret_hash: &[u8],
    ) -> Result<Option<FundingTxSpend<Self>>, SearchForFundingSpendErr> {
        self.search_for_taker_funding_spend_impl(tx).await
    }

    async fn gen_taker_funding_spend_preimage(
        &self,
        args: &GenTakerFundingSpendArgs<'_, Self>,
        _swap_unique_data: &[u8],
    ) -> GenPreimageResult<Self> {
        // EVM coins don't need a real preimage — the approve flow replaces it.
        // Return the funding tx bytes as "preimage" and a dummy signature.
        Ok(TxPreimageWithSig {
            preimage: rlp::encode(args.funding_tx).to_vec(),
            signature: vec![],
        })
    }

    async fn validate_taker_funding_spend_preimage(
        &self,
        _gen_args: &GenTakerFundingSpendArgs<'_, Self>,
        _preimage: &TxPreimageWithSig<Self>,
    ) -> ValidateTakerFundingSpendPreimageResult {
        // EVM: always valid (approve-based flow, no preimage exchange).
        Ok(())
    }

    async fn sign_and_send_taker_funding_spend(
        &self,
        _preimage: &TxPreimageWithSig<Self>,
        args: &GenTakerFundingSpendArgs<'_, Self>,
        _swap_unique_data: &[u8],
    ) -> Result<SignedEthTx, TransactionErr> {
        // For EVM, this sends takerPaymentApprove (not a traditional funding spend).
        self.taker_payment_approve(args).await
    }

    async fn refund_combined_taker_payment(
        &self,
        args: RefundTakerPaymentArgs<'_>,
    ) -> Result<SignedEthTx, TransactionErr> {
        // In EVM, combined taker payment refund uses the same timelock path.
        self.refund_taker_payment_with_timelock_impl(args).await
    }

    fn skip_taker_payment_spend_preimage(&self) -> bool { true }

    async fn gen_taker_payment_spend_preimage(
        &self,
        _args: &GenTakerPaymentSpendArgs<'_, Self>,
        _swap_unique_data: &[u8],
    ) -> GenPreimageResult<Self> {
        Err(MmError::new(crate::TxGenError::Other(
            "EVM coins skip taker payment spend preimage".into(),
        )))
    }

    async fn validate_taker_payment_spend_preimage(
        &self,
        _gen_args: &GenTakerPaymentSpendArgs<'_, Self>,
        _preimage: &TxPreimageWithSig<Self>,
    ) -> ValidateTakerPaymentSpendPreimageResult {
        Err(MmError::new(
            crate::ValidateTakerPaymentSpendPreimageError::InternalError(
                "EVM coins skip taker payment spend preimage".into(),
            ),
        ))
    }

    async fn sign_and_broadcast_taker_payment_spend(
        &self,
        _preimage: Option<&TxPreimageWithSig<Self>>,
        gen_args: &GenTakerPaymentSpendArgs<'_, Self>,
        secret: &[u8],
        _swap_unique_data: &[u8],
    ) -> Result<SignedEthTx, TransactionErr> {
        self.sign_and_broadcast_taker_payment_spend_impl(gen_args, secret).await
    }

    async fn find_taker_payment_spend_tx(
        &self,
        taker_payment: &SignedEthTx,
        from_block: u64,
        wait_until: u64,
    ) -> MmResult<SignedEthTx, FindPaymentSpendError> {
        self.find_taker_payment_spend_tx_impl(taker_payment, from_block, wait_until, 10.0)
            .await
    }

    async fn extract_secret_v2(&self, _secret_hash: &[u8], spend_tx: &SignedEthTx) -> Result<[u8; 32], String> {
        self.extract_secret_v2_impl(spend_tx).await
    }
}
