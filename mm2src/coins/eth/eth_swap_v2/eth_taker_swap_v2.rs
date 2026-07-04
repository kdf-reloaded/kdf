//! # Purpose
//! Taker-side EVM (Ethereum + ERC-20) payment paths for the V2
//! (Trading Protocol Upgrade) atomic-swap state machine.
//!
//! # Public exports
//! Method-only surface on [`crate::eth::EthCoin`]:
//! - [`EthCoin::send_taker_funding_impl`]
//! - [`EthCoin::validate_taker_funding_impl`]
//! - [`EthCoin::taker_payment_approve`]
//! - [`EthCoin::refund_taker_payment_with_timelock_impl`]
//! - [`EthCoin::refund_taker_funding_secret_impl`]
//! - [`EthCoin::search_for_taker_funding_spend_impl`]
//! - [`EthCoin::sign_and_broadcast_taker_payment_spend_impl`]
//! - [`EthCoin::find_taker_payment_spend_tx_impl`]
//! - [`EthCoin::extract_secret_v2_impl`]
//!
//! # Invariants
//! - Solidity entrypoint names (`ethTakerPayment`, `erc20TakerPayment`,
//!   `takerPaymentApprove`, `refundTakerPaymentTimelock`,
//!   `refundTakerPaymentSecret`, `spendTakerPayment`) are part of the
//!   on-chain ABI.
//! - The argument ordering inside each ABI call is fixed by the
//!   deployed `EtomicSwapTakerV2` contract.
//! - Event name `TakerPaymentSpent` is emitted by the V2 contract and
//!   keyed on by [`EthCoin::find_taker_payment_spend_tx_impl`].

use super::{check_decoded_length, extract_id_from_tx_data, validate_amount, validate_from_to_addresses,
            EthPaymentType, PaymentMethod, PrepareTxDataError, SpendTxSearchParams, ZERO_VALUE};
use crate::eth::abi::{Contract, Function, Token};
use crate::eth::legacy_tx::Action;
use crate::eth::{decode_contract_call, get_function_input_data, u256_from_big_decimal, EthCoin, EthCoinType,
                 ParseCoinAssocTypes, RefundFundingSecretArgs, RefundTakerPaymentArgs, SendTakerFundingArgs,
                 SignedEthTx, SwapTxTypeWithSecretHash, TakerPaymentStateV2, TransactionErr, ValidateSwapV2TxError,
                 ValidateSwapV2TxResult, ValidateTakerFundingArgs, TAKER_SWAP_V2};
use crate::{FindPaymentSpendError, FundingTxSpend, GenTakerFundingSpendArgs, GenTakerPaymentSpendArgs,
            SearchForFundingSpendErr};
use derive_more::Display;
use ethereum_types::{Address, Public, U256};
use futures::compat::Future01CompatExt;
use mm2_err_handle::prelude::{MapToMmResult, MmError, MmResult, MmResultExt};
use mm2_eth::keys::public_to_address;
use std::convert::TryInto;

// ABI entrypoint names + state-tuple offsets -------------------------------

const FN_ETH_TAKER_PAYMENT: &str = "ethTakerPayment";
const FN_ERC20_TAKER_PAYMENT: &str = "erc20TakerPayment";
const FN_TAKER_PAYMENT_APPROVE: &str = "takerPaymentApprove";
const FN_REFUND_TAKER_PAYMENT_TIMELOCK: &str = "refundTakerPaymentTimelock";
const FN_REFUND_TAKER_PAYMENT_SECRET: &str = "refundTakerPaymentSecret";
const FN_SPEND_TAKER_PAYMENT: &str = "spendTakerPayment";
const EVT_TAKER_PAYMENT_SPENT: &str = "TakerPaymentSpent";

/// Index of the `state` field inside the `TakerPayment` struct returned
/// by `EtomicSwapTakerV2.takerPayments(swapId)`.
const TAKER_PAYMENT_STATE_INDEX: usize = 3;

// Internal arg bags --------------------------------------------------------

/// Inputs for `ethTakerPayment` / `erc20TakerPayment` calldata builders.
struct TakerFundingInputs<'a> {
    dex_fee: U256,
    payment_amount: U256,
    maker_address: Address,
    taker_secret_hash: &'a [u8; 32],
    maker_secret_hash: &'a [u8; 32],
    funding_time_lock: u64,
    payment_time_lock: u64,
}

/// Inputs for `refundTakerPaymentTimelock` calldata builder.
struct TakerTimelockRefundInputs<'a> {
    dex_fee: U256,
    payment_amount: U256,
    maker_address: Address,
    taker_secret_hash: &'a [u8; 32],
    maker_secret_hash: &'a [u8; 32],
    payment_time_lock: u64,
    token_address: Address,
}

/// Inputs for `refundTakerPaymentSecret` calldata builder.
struct TakerSecretRefundInputs<'a> {
    dex_fee: U256,
    payment_amount: U256,
    maker_address: Address,
    taker_secret: &'a [u8; 32],
    maker_secret_hash: &'a [u8; 32],
    payment_time_lock: u64,
    token_address: Address,
}

/// Inputs for verifying a taker-funding tx against the swap state.
struct TakerVerifyInputs<'a> {
    swap_id: Vec<u8>,
    amount: U256,
    dex_fee: U256,
    receiver: Address,
    taker_secret_hash: &'a [u8; 32],
    maker_secret_hash: &'a [u8; 32],
    funding_time_lock: u64,
    payment_time_lock: u64,
}

// Setup resolvers ----------------------------------------------------------

impl EthCoin {
    /// Resolves the deployed taker-V2 contract address for swap-pipeline
    /// callers (failure becomes a `TransactionErr::Plain`).
    fn resolve_taker_v2_contract(&self) -> Result<Address, TransactionErr> {
        self.swap_v2_contracts
            .map(|c| c.taker_swap_v2_contract)
            .ok_or_else(|| TransactionErr::Plain(ERRL!("Expected swap_v2_contracts to be Some, but found None")))
    }

    /// Looks up the gas limit configured for a particular taker-side
    /// `(coin_type, method)` pair, mapping the lookup error into a
    /// `TransactionErr` for the swap pipeline.
    fn taker_gas_limit(&self, method: PaymentMethod) -> Result<u64, TransactionErr> {
        self.gas_limit_v2
            .gas_limit(&self.coin_type, EthPaymentType::TakerPayments, method)
            .map_err(|e| TransactionErr::Plain(ERRL!("{}", e)))
    }

    /// Returns the underlying ERC-20 token address (or zero for native ETH).
    fn token_address_for_taker(&self) -> Result<Address, TransactionErr> {
        self.get_token_address()
            .map_err(|e| TransactionErr::Plain(ERRL!("{}", e)))
    }
}

// Send taker funding -------------------------------------------------------

impl EthCoin {
    /// Calls `ethTakerPayment` or `erc20TakerPayment` on the taker-V2
    /// contract, locking the taker's funds for the duration of the swap.
    pub(crate) async fn send_taker_funding_impl(
        &self,
        args: SendTakerFundingArgs<'_>,
    ) -> Result<SignedEthTx, TransactionErr> {
        let taker_swap_v2_contract = self.resolve_taker_v2_contract()?;
        let dex_fee = try_tx_s!(u256_from_big_decimal(&args.dex_fee.fee_amount().into(), self.decimals));
        let payment_amount = try_tx_s!(u256_from_big_decimal(
            &(args.trading_amount.clone() + args.premium_amount.clone()),
            self.decimals
        ));

        let funding_args = TakerFundingInputs {
            dex_fee,
            payment_amount,
            maker_address: public_to_address(&Public::from_slice(args.maker_pub)),
            taker_secret_hash: try_tx_s!(args.taker_secret_hash.try_into()),
            maker_secret_hash: try_tx_s!(args.maker_secret_hash.try_into()),
            funding_time_lock: args.funding_time_lock,
            payment_time_lock: args.payment_time_lock,
        };

        match &self.coin_type {
            EthCoinType::Eth => {
                let data = try_tx_s!(self.prepare_taker_eth_funding_data(&funding_args).await);
                // ETH funding tx carries `payment_amount + dex_fee` as msg.value;
                // calldata only carries the dex_fee component.
                let eth_total_payment = payment_amount.checked_add(dex_fee).ok_or_else(|| {
                    TransactionErr::Plain(ERRL!("Overflow occurred while calculating eth_total_payment"))
                })?;
                self.sign_and_send_transaction(
                    eth_total_payment,
                    Action::Call(taker_swap_v2_contract),
                    data,
                    U256::from(self.gas_limit_v2.taker.eth_payment),
                )
                .compat()
                .await
            },
            EthCoinType::Erc20 {
                platform: _,
                token_addr,
            } => {
                let data = try_tx_s!(self.prepare_taker_erc20_funding_data(&funding_args, *token_addr).await);
                self.handle_allowance(taker_swap_v2_contract, payment_amount, args.funding_time_lock)
                    .await?;
                self.sign_and_send_transaction(
                    U256::from(ZERO_VALUE),
                    Action::Call(taker_swap_v2_contract),
                    data,
                    U256::from(self.gas_limit_v2.taker.erc20_payment),
                )
                .compat()
                .await
            },
            // TRON HTLC payments use the dedicated TRON pipeline.
            // Activation gating prevents this branch. P10.2.5.
            EthCoinType::Tron | EthCoinType::Trc20 { .. } => Err(TransactionErr::Plain(ERRL!(
                "TRON taker funding v2 not yet wired (pending P10.2.5)"
            ))),
        }
    }
}

// Validate taker funding ---------------------------------------------------

impl EthCoin {
    pub(crate) async fn validate_taker_funding_impl(
        &self,
        args: ValidateTakerFundingArgs<'_, Self>,
    ) -> ValidateSwapV2TxResult {
        let taker_swap_v2_contract = self
            .swap_v2_contracts
            .map(|c| c.taker_swap_v2_contract)
            .ok_or_else(|| {
                ValidateSwapV2TxError::InternalError(
                    "Expected swap_v2_contracts to be Some, but found None".to_string(),
                )
            })?;

        let taker_secret_hash = args.taker_secret_hash.try_into()?;
        let maker_secret_hash = args.maker_secret_hash.try_into()?;
        validate_amount(&args.trading_amount).map_err(ValidateSwapV2TxError::InternalError)?;

        let tx = args.funding_tx;
        let taker_address = public_to_address(args.taker_pub);
        validate_from_to_addresses(tx, taker_address, taker_swap_v2_contract).map_mm_err()?;

        let inputs = TakerVerifyInputs {
            swap_id: self.etomic_swap_id_v2(args.payment_time_lock, args.maker_secret_hash),
            amount: u256_from_big_decimal(&(args.trading_amount + args.premium_amount), self.decimals).map_mm_err()?,
            dex_fee: u256_from_big_decimal(&args.dex_fee.fee_amount().into(), self.decimals).map_mm_err()?,
            receiver: self.my_address,
            taker_secret_hash,
            maker_secret_hash,
            funding_time_lock: args.funding_time_lock,
            payment_time_lock: args.payment_time_lock,
        };

        match self.coin_type {
            EthCoinType::Eth => {
                let function = TAKER_SWAP_V2.function(FN_ETH_TAKER_PAYMENT)?;
                let decoded = decode_contract_call(function, &tx.data)?;
                verify_eth_taker_calldata(&decoded, &inputs, function, tx.value)
            },
            EthCoinType::Erc20 { token_addr, .. } => {
                let function = TAKER_SWAP_V2.function(FN_ERC20_TAKER_PAYMENT)?;
                let decoded = decode_contract_call(function, &tx.data)?;
                verify_erc20_taker_calldata(&decoded, &inputs, function, token_addr)
            },
            // TRON validation uses the dedicated TRON pipeline.
            // Activation gating prevents this branch. P10.2.5.
            EthCoinType::Tron | EthCoinType::Trc20 { .. } => MmError::err(ValidateSwapV2TxError::InternalError(
                "TRON taker funding validation not yet wired (pending P10.2.5)".to_owned(),
            )),
        }
    }
}

// Taker funding approve ---------------------------------------------------

impl EthCoin {
    /// Calls `takerPaymentApprove` to advance the on-chain state from
    /// `Funded` to `TakerApproved` once the maker's payment has been
    /// observed.
    pub(crate) async fn taker_payment_approve(
        &self,
        args: &GenTakerFundingSpendArgs<'_, Self>,
    ) -> Result<SignedEthTx, TransactionErr> {
        let gas_limit = match self.coin_type {
            EthCoinType::Eth | EthCoinType::Erc20 { .. } => U256::from(self.gas_limit_v2.taker.approve_payment),
            // TRON approve flow lives in the TRON pipeline. Gated. P10.2.5.
            EthCoinType::Tron | EthCoinType::Trc20 { .. } => {
                return Err(TransactionErr::Plain(ERRL!(
                    "TRON taker payment approve not yet wired (pending P10.2.5)"
                )))
            },
        };

        let (taker_swap_v2_contract, send_func, token_address) = self
            .taker_swap_v2_details(FN_ETH_TAKER_PAYMENT, FN_ERC20_TAKER_PAYMENT)
            .await?;
        let decoded = try_tx_s!(decode_contract_call(send_func, &args.funding_tx.data));
        let data = try_tx_s!(
            self.prepare_taker_payment_approve_data(args, decoded, token_address)
                .await
        );

        self.sign_and_send_transaction(
            U256::from(ZERO_VALUE),
            Action::Call(taker_swap_v2_contract),
            data,
            gas_limit,
        )
        .compat()
        .await
    }
}

// Refund taker payment (timelock + secret) ---------------------------------

impl EthCoin {
    pub(crate) async fn refund_taker_payment_with_timelock_impl(
        &self,
        args: RefundTakerPaymentArgs<'_>,
    ) -> Result<SignedEthTx, TransactionErr> {
        let token_address = self.token_address_for_taker()?;
        let taker_swap_v2_contract = self.resolve_taker_v2_contract()?;
        let gas_limit = self.taker_gas_limit(PaymentMethod::RefundTimelock)?;

        let (maker_secret_hash, taker_secret_hash) = match args.tx_type_with_secret_hash {
            SwapTxTypeWithSecretHash::TakerPaymentV2 {
                maker_secret_hash,
                taker_secret_hash,
            } => (maker_secret_hash, taker_secret_hash),
            _ => {
                return Err(TransactionErr::Plain(ERRL!(
                    "Unsupported swap tx type for timelock refund"
                )))
            },
        };

        let dex_fee = try_tx_s!(u256_from_big_decimal(
            &args.dex_fee.fee_amount().to_decimal(),
            self.decimals
        ));
        let payment_amount = try_tx_s!(u256_from_big_decimal(
            &(args.trading_amount + args.premium_amount),
            self.decimals
        ));

        let inputs = TakerTimelockRefundInputs {
            dex_fee,
            payment_amount,
            maker_address: public_to_address(&Public::from_slice(args.maker_pub)),
            taker_secret_hash: try_tx_s!(taker_secret_hash.try_into()),
            maker_secret_hash: try_tx_s!(maker_secret_hash.try_into()),
            payment_time_lock: args.time_lock,
            token_address,
        };
        let data = try_tx_s!(self.prepare_taker_refund_payment_timelock_data(inputs).await);

        self.sign_and_send_transaction(
            U256::from(ZERO_VALUE),
            Action::Call(taker_swap_v2_contract),
            data,
            U256::from(gas_limit),
        )
        .compat()
        .await
    }

    pub(crate) async fn refund_taker_funding_secret_impl(
        &self,
        args: RefundFundingSecretArgs<'_, Self>,
    ) -> Result<SignedEthTx, TransactionErr> {
        let token_address = self.token_address_for_taker()?;
        let taker_swap_v2_contract = self.resolve_taker_v2_contract()?;
        let gas_limit = self.taker_gas_limit(PaymentMethod::RefundSecret)?;

        let dex_fee = try_tx_s!(u256_from_big_decimal(
            &args.dex_fee.fee_amount().to_decimal(),
            self.decimals
        ));
        let payment_amount = try_tx_s!(u256_from_big_decimal(
            &(args.trading_amount + args.premium_amount),
            self.decimals
        ));

        let inputs = TakerSecretRefundInputs {
            dex_fee,
            payment_amount,
            maker_address: public_to_address(args.maker_pubkey),
            taker_secret: args.taker_secret,
            maker_secret_hash: try_tx_s!(args.maker_secret_hash.try_into()),
            payment_time_lock: args.payment_time_lock,
            token_address,
        };
        let data = try_tx_s!(self.prepare_taker_refund_payment_secret_data(&inputs).await);

        self.sign_and_send_transaction(
            U256::from(ZERO_VALUE),
            Action::Call(taker_swap_v2_contract),
            data,
            U256::from(gas_limit),
        )
        .compat()
        .await
    }
}

// Search & spend taker payment (maker side) --------------------------------

impl EthCoin {
    /// Inspects an on-chain funding tx and returns
    /// `Some(TransferredToTakerPayment(..))` once its state has reached
    /// `TakerApproved`. Called by the maker side of the swap.
    pub(crate) async fn search_for_taker_funding_spend_impl(
        &self,
        tx: &SignedEthTx,
    ) -> Result<Option<FundingTxSpend<Self>>, SearchForFundingSpendErr> {
        let (decoded, taker_swap_v2_contract) = self
            .get_funding_decoded_and_swap_contract(tx)
            .await
            .map_err(|e| SearchForFundingSpendErr::Internal(ERRL!("{}", e)))?;

        let taker_status = self
            .payment_status_v2(
                taker_swap_v2_contract,
                decoded[0].clone(),
                &TAKER_SWAP_V2,
                EthPaymentType::TakerPayments,
                TAKER_PAYMENT_STATE_INDEX,
            )
            .await
            .map_err(|e| SearchForFundingSpendErr::Internal(ERRL!("{}", e)))?;

        if taker_status == U256::from(TakerPaymentStateV2::TakerApproved as u8) {
            Ok(Some(FundingTxSpend::TransferredToTakerPayment(tx.clone())))
        } else {
            Ok(None)
        }
    }

    /// Calls `spendTakerPayment` on the maker side, releasing the taker
    /// funds to the maker once the maker has revealed their secret.
    pub(crate) async fn sign_and_broadcast_taker_payment_spend_impl(
        &self,
        gen_args: &GenTakerPaymentSpendArgs<'_, Self>,
        secret: &[u8],
    ) -> Result<SignedEthTx, TransactionErr> {
        let gas_limit = self.taker_gas_limit(PaymentMethod::Spend)?;

        let (taker_swap_v2_contract, taker_payment, token_address) = self
            .taker_swap_v2_details(FN_ETH_TAKER_PAYMENT, FN_ERC20_TAKER_PAYMENT)
            .await?;
        let decoded = try_tx_s!(decode_contract_call(taker_payment, &gen_args.taker_tx.data));
        let data = try_tx_s!(
            self.prepare_spend_taker_payment_data(gen_args, secret, decoded, token_address)
                .await
        );

        self.sign_and_send_transaction(
            U256::from(ZERO_VALUE),
            Action::Call(taker_swap_v2_contract),
            data,
            U256::from(gas_limit),
        )
        .compat()
        .await
    }

    /// Polls the chain until the `TakerPaymentSpent` event matching the
    /// approved swap_id is observed, then returns the spend tx.
    pub(crate) async fn find_taker_payment_spend_tx_impl(
        &self,
        taker_payment: &SignedEthTx,
        from_block: u64,
        wait_until: u64,
        check_every: f64,
    ) -> MmResult<SignedEthTx, FindPaymentSpendError> {
        let taker_swap_v2_contract = self
            .swap_v2_contracts
            .map(|c| c.taker_swap_v2_contract)
            .ok_or_else(|| {
                FindPaymentSpendError::Internal("Expected swap_v2_contracts to be Some, but found None".to_string())
            })?;

        let id_array: [u8; 32] = extract_id_from_tx_data(&taker_payment.data, &TAKER_SWAP_V2, FN_TAKER_PAYMENT_APPROVE)
            .await?
            .as_slice()
            .try_into()?;

        let params = SpendTxSearchParams {
            swap_contract_address: taker_swap_v2_contract,
            event_name: EVT_TAKER_PAYMENT_SPENT,
            abi_contract: &TAKER_SWAP_V2,
            swap_id: &id_array,
            from_block,
            wait_until,
            check_every,
        };
        let tx_hash = self.find_transaction_hash_by_event(params).await?;
        self.wait_for_transaction(tx_hash, wait_until, check_every).await
    }

    /// Pulls the maker's secret out of the calldata of a
    /// `spendTakerPayment` tx. Used by the maker-side recovery path.
    pub(crate) async fn extract_secret_v2_impl(&self, spend_tx: &SignedEthTx) -> Result<[u8; 32], String> {
        let function = try_s!(TAKER_SWAP_V2.function(FN_SPEND_TAKER_PAYMENT));
        let expected_signature = function.short_signature();
        if &spend_tx.data[0..4] != expected_signature {
            return ERR!(
                "Expected 'spendTakerPayment' contract call signature: {:?}, found {:?}",
                expected_signature,
                &spend_tx.data[0..4]
            );
        }

        let decoded = try_s!(decode_contract_call(function, &spend_tx.data));
        if decoded.len() < 7 {
            return ERR!("Invalid arguments in 'spendTakerPayment' call: {:?}", decoded);
        }

        match &decoded[5] {
            Token::FixedBytes(secret) => Ok(try_s!(secret.as_slice().try_into())),
            other => ERR!(
                "Expected secret to be fixed bytes, but decoded function data is {:?}",
                other
            ),
        }
    }
}

// Calldata builders --------------------------------------------------------

impl EthCoin {
    async fn prepare_taker_eth_funding_data(
        &self,
        args: &TakerFundingInputs<'_>,
    ) -> Result<Vec<u8>, PrepareTxDataError> {
        let function = TAKER_SWAP_V2.function(FN_ETH_TAKER_PAYMENT)?;
        let id = self.etomic_swap_id_v2(args.payment_time_lock, args.maker_secret_hash);
        Ok(function.encode_input(&[
            Token::FixedBytes(id),
            Token::Uint(args.dex_fee),
            Token::Address(args.maker_address),
            Token::FixedBytes(args.taker_secret_hash.to_vec()),
            Token::FixedBytes(args.maker_secret_hash.to_vec()),
            Token::Uint(args.funding_time_lock.into()),
            Token::Uint(args.payment_time_lock.into()),
        ])?)
    }

    async fn prepare_taker_erc20_funding_data(
        &self,
        args: &TakerFundingInputs<'_>,
        token_address: Address,
    ) -> Result<Vec<u8>, PrepareTxDataError> {
        let function = TAKER_SWAP_V2.function(FN_ERC20_TAKER_PAYMENT)?;
        let id = self.etomic_swap_id_v2(args.payment_time_lock, args.maker_secret_hash);
        Ok(function.encode_input(&[
            Token::FixedBytes(id),
            Token::Uint(args.payment_amount),
            Token::Uint(args.dex_fee),
            Token::Address(token_address),
            Token::Address(args.maker_address),
            Token::FixedBytes(args.taker_secret_hash.to_vec()),
            Token::FixedBytes(args.maker_secret_hash.to_vec()),
            Token::Uint(args.funding_time_lock.into()),
            Token::Uint(args.payment_time_lock.into()),
        ])?)
    }

    async fn prepare_taker_refund_payment_timelock_data(
        &self,
        args: TakerTimelockRefundInputs<'_>,
    ) -> Result<Vec<u8>, PrepareTxDataError> {
        let function = TAKER_SWAP_V2.function(FN_REFUND_TAKER_PAYMENT_TIMELOCK)?;
        let id = self.etomic_swap_id_v2(args.payment_time_lock, args.maker_secret_hash);
        Ok(function.encode_input(&[
            Token::FixedBytes(id),
            Token::Uint(args.payment_amount),
            Token::Uint(args.dex_fee),
            Token::Address(args.maker_address),
            Token::FixedBytes(args.taker_secret_hash.to_vec()),
            Token::FixedBytes(args.maker_secret_hash.to_vec()),
            Token::Address(args.token_address),
        ])?)
    }

    async fn prepare_taker_refund_payment_secret_data(
        &self,
        args: &TakerSecretRefundInputs<'_>,
    ) -> Result<Vec<u8>, PrepareTxDataError> {
        let function = TAKER_SWAP_V2.function(FN_REFUND_TAKER_PAYMENT_SECRET)?;
        let id = self.etomic_swap_id_v2(args.payment_time_lock, args.maker_secret_hash);
        Ok(function.encode_input(&[
            Token::FixedBytes(id),
            Token::Uint(args.payment_amount),
            Token::Uint(args.dex_fee),
            Token::Address(args.maker_address),
            Token::FixedBytes(args.taker_secret.to_vec()),
            Token::FixedBytes(args.maker_secret_hash.to_vec()),
            Token::Address(args.token_address),
        ])?)
    }

    /// Builds the `takerPaymentApprove` calldata. The shape varies by
    /// coin type because the original funding tx's calldata is reshuffled
    /// into the approve call.
    async fn prepare_taker_payment_approve_data(
        &self,
        args: &GenTakerFundingSpendArgs<'_, Self>,
        decoded: Vec<Token>,
        token_address: Address,
    ) -> Result<Vec<u8>, PrepareTxDataError> {
        let function = TAKER_SWAP_V2.function(FN_TAKER_PAYMENT_APPROVE)?;
        match self.coin_type {
            EthCoinType::Eth => {
                // ETH funding tx encodes `dex_fee` in calldata; payment
                // amount = msg.value − dex_fee.
                let (dex_fee, amount) = split_eth_payment_value(&decoded, args.funding_tx.value)?;
                Ok(function.encode_input(&[
                    decoded[0].clone(),                                 // id
                    Token::Uint(amount),                                // amount = value - dex_fee
                    Token::Uint(dex_fee),                               // dex_fee
                    decoded[2].clone(),                                 // receiver
                    Token::FixedBytes(args.taker_secret_hash.to_vec()), // taker_secret_hash
                    Token::FixedBytes(args.maker_secret_hash.to_vec()), // maker_secret_hash
                    Token::Address(token_address),                      // token (zero for ETH)
                ])?)
            },
            EthCoinType::Erc20 { .. } => {
                check_decoded_length(&decoded, 9)?;
                Ok(function.encode_input(&[
                    decoded[0].clone(),                                 // id
                    decoded[1].clone(),                                 // amount
                    decoded[2].clone(),                                 // dex_fee
                    decoded[4].clone(),                                 // receiver
                    Token::FixedBytes(args.taker_secret_hash.to_vec()), // taker_secret_hash
                    Token::FixedBytes(args.maker_secret_hash.to_vec()), // maker_secret_hash
                    Token::Address(token_address),                      // token addr
                ])?)
            },
            EthCoinType::Tron | EthCoinType::Trc20 { .. } => Err(PrepareTxDataError::Internal(
                "TRON taker payment approve data not yet wired (pending P10.2.5)".to_owned(),
            )),
        }
    }

    /// Builds the `spendTakerPayment` calldata using the maker's revealed
    /// secret.
    async fn prepare_spend_taker_payment_data(
        &self,
        args: &GenTakerPaymentSpendArgs<'_, Self>,
        secret: &[u8],
        decoded: Vec<Token>,
        token_address: Address,
    ) -> Result<Vec<u8>, PrepareTxDataError> {
        let function = TAKER_SWAP_V2.function(FN_SPEND_TAKER_PAYMENT)?;
        let taker_address = public_to_address(args.taker_pub);
        match self.coin_type {
            EthCoinType::Eth => {
                let (dex_fee, amount) = split_eth_payment_value(&decoded, args.taker_tx.value)?;
                Ok(function.encode_input(&[
                    decoded[0].clone(),
                    Token::Uint(amount),
                    Token::Uint(dex_fee),
                    Token::Address(taker_address),
                    decoded[3].clone(),
                    Token::FixedBytes(secret.to_vec()),
                    Token::Address(token_address),
                ])?)
            },
            EthCoinType::Erc20 { .. } => {
                check_decoded_length(&decoded, 9)?;
                Ok(function.encode_input(&[
                    decoded[0].clone(),
                    decoded[1].clone(),
                    decoded[2].clone(),
                    Token::Address(taker_address),
                    decoded[5].clone(),
                    Token::FixedBytes(secret.to_vec()),
                    Token::Address(token_address),
                ])?)
            },
            EthCoinType::Tron | EthCoinType::Trc20 { .. } => Err(PrepareTxDataError::Internal(
                "TRON spend taker payment data not yet wired (pending P10.2.5)".to_owned(),
            )),
        }
    }
}

// On-chain state-tuple helpers --------------------------------------------

impl EthCoin {
    /// Looks up the taker-V2 contract address, the funding-payment ABI
    /// function, and the underlying token address. Used by the approve
    /// and spend paths.
    async fn taker_swap_v2_details(
        &self,
        eth_func_name: &str,
        erc20_func_name: &str,
    ) -> Result<(Address, &Function, Address), TransactionErr> {
        let (func, token_address) = match self.coin_type {
            EthCoinType::Eth => (try_tx_s!(TAKER_SWAP_V2.function(eth_func_name)), Address::default()),
            EthCoinType::Erc20 { token_addr, .. } => (try_tx_s!(TAKER_SWAP_V2.function(erc20_func_name)), token_addr),
            EthCoinType::Tron | EthCoinType::Trc20 { .. } => {
                return Err(TransactionErr::Plain(ERRL!(
                    "TRON swap v2 contract details not yet wired (pending P10.2.5)"
                )))
            },
        };
        let taker_swap_v2_contract = self.resolve_taker_v2_contract()?;
        Ok((taker_swap_v2_contract, func, token_address))
    }

    /// Decodes a funding tx's calldata using whichever of the
    /// `eth*Payment` / `erc20*Payment` functions matches the coin type.
    async fn get_funding_decoded_and_swap_contract(
        &self,
        tx: &SignedEthTx,
    ) -> Result<(Vec<Token>, Address), PrepareTxDataError> {
        let func = match self.coin_type {
            EthCoinType::Eth => TAKER_SWAP_V2.function(FN_ETH_TAKER_PAYMENT)?,
            EthCoinType::Erc20 { .. } => TAKER_SWAP_V2.function(FN_ERC20_TAKER_PAYMENT)?,
            EthCoinType::Tron | EthCoinType::Trc20 { .. } => {
                return Err(PrepareTxDataError::Internal(
                    "TRON funding decoding not yet wired (pending P10.2.5)".to_owned(),
                ));
            },
        };
        let decoded = decode_contract_call(func, &tx.data)?;
        let taker_swap_v2_contract = self
            .swap_v2_contracts
            .map(|c| c.taker_swap_v2_contract)
            .ok_or_else(|| {
                PrepareTxDataError::Internal("Expected swap_v2_contracts to be Some, but found None".to_string())
            })?;
        Ok((decoded, taker_swap_v2_contract))
    }

    /// Calls a `payments(id)` getter on the V2 contract and extracts the
    /// `state` slot from the returned struct.
    async fn payment_status_v2(
        &self,
        swap_address: Address,
        swap_id: Token,
        contract_abi: &Contract,
        payment_type: EthPaymentType,
        state_index: usize,
    ) -> Result<U256, PaymentStatusErr> {
        let function = contract_abi.function(payment_type.as_str())?;
        let data = function.encode_input(&[swap_id])?;
        let bytes = self
            .call_request(swap_address, None, Some(data.into()))
            .compat()
            .await
            .map_err(|e| PaymentStatusErr::Transport(e.to_string()))?;
        let decoded_tokens = function.decode_output(&bytes.0)?;

        let state = decoded_tokens.get(state_index).ok_or_else(|| {
            PaymentStatusErr::Internal(format!(
                "Payment status must contain 'state' as the {state_index} token"
            ))
        })?;

        match state {
            Token::Uint(state) => Ok(*state),
            other => Err(PaymentStatusErr::InvalidData(format!(
                "Payment status must be Uint, got {other:?}"
            ))),
        }
    }
}

// Internal err for state-tuple reads --------------------------------------

#[derive(Debug, Display)]
enum PaymentStatusErr {
    #[display(fmt = "ABI error: {}", _0)]
    ABIError(String),
    #[display(fmt = "Transport error: {}", _0)]
    Transport(String),
    #[display(fmt = "Internal error: {}", _0)]
    Internal(String),
    #[display(fmt = "Invalid data error: {}", _0)]
    InvalidData(String),
}

impl From<crate::eth::abi::AbiError> for PaymentStatusErr {
    fn from(err: crate::eth::abi::AbiError) -> Self { PaymentStatusErr::ABIError(err.to_string()) }
}

// Calldata verifiers -------------------------------------------------------

/// One row of the `(decoded_index, expected_token, field_name)`
/// verification table consumed by [`verify_taker_token_table`].
type VerifyRow<'a> = (usize, Token, &'a str);

/// Walks a verification table over a decoded calldata vector and
/// raises `WrongPaymentTx` on the first mismatch.
fn verify_taker_token_table(
    decoded: &[Token],
    func: &Function,
    rows: &[VerifyRow<'_>],
    contract_label: &str,
) -> Result<(), MmError<ValidateSwapV2TxError>> {
    for (index, expected_token, field_name) in rows {
        let actual = get_function_input_data(decoded, func, *index).map_to_mm(ValidateSwapV2TxError::InternalError)?;
        if actual != *expected_token {
            return MmError::err(ValidateSwapV2TxError::WrongPaymentTx(format!(
                "{} `{}` {:?} is invalid, expected {:?}",
                contract_label,
                field_name,
                decoded.get(*index),
                expected_token
            )));
        }
    }
    Ok(())
}

fn verify_eth_taker_calldata(
    decoded: &[Token],
    args: &TakerVerifyInputs<'_>,
    func: &Function,
    tx_value: U256,
) -> Result<(), MmError<ValidateSwapV2TxError>> {
    let rows: [VerifyRow; 7] = [
        (0, Token::FixedBytes(args.swap_id.clone()), "id"),
        (1, Token::Uint(args.dex_fee), "dexFee"),
        (2, Token::Address(args.receiver), "receiver"),
        (3, Token::FixedBytes(args.taker_secret_hash.to_vec()), "takerSecretHash"),
        (4, Token::FixedBytes(args.maker_secret_hash.to_vec()), "makerSecretHash"),
        (5, Token::Uint(U256::from(args.funding_time_lock)), "preApproveLockTime"),
        (6, Token::Uint(U256::from(args.payment_time_lock)), "paymentLockTime"),
    ];
    verify_taker_token_table(decoded, func, &rows, "ETH Taker Payment")?;

    // For ETH, msg.value carries amount + dex_fee.
    let total = args.amount.checked_add(args.dex_fee).ok_or_else(|| {
        ValidateSwapV2TxError::Overflow("Overflow occurred while calculating total payment".to_string())
    })?;
    if total != tx_value {
        return MmError::err(ValidateSwapV2TxError::WrongPaymentTx(format!(
            "ETH Taker Payment amount is invalid, expected {total:?}, got {tx_value:?}"
        )));
    }
    Ok(())
}

fn verify_erc20_taker_calldata(
    decoded: &[Token],
    args: &TakerVerifyInputs<'_>,
    func: &Function,
    token_addr: Address,
) -> Result<(), MmError<ValidateSwapV2TxError>> {
    let rows: [VerifyRow; 9] = [
        (0, Token::FixedBytes(args.swap_id.clone()), "id"),
        (1, Token::Uint(args.amount), "amount"),
        (2, Token::Uint(args.dex_fee), "dexFee"),
        (3, Token::Address(token_addr), "tokenAddress"),
        (4, Token::Address(args.receiver), "receiver"),
        (5, Token::FixedBytes(args.taker_secret_hash.to_vec()), "takerSecretHash"),
        (6, Token::FixedBytes(args.maker_secret_hash.to_vec()), "makerSecretHash"),
        (7, Token::Uint(U256::from(args.funding_time_lock)), "preApproveLockTime"),
        (8, Token::Uint(U256::from(args.payment_time_lock)), "paymentLockTime"),
    ];
    verify_taker_token_table(decoded, func, &rows, "ERC20 Taker Payment")
}

// ETH-only payment value splitting ----------------------------------------

/// Splits an ETH funding tx's `msg.value` into the `(dex_fee, amount)`
/// pair using the `dex_fee` slot encoded in the funding calldata.
fn split_eth_payment_value(decoded: &[Token], tx_value: U256) -> Result<(U256, U256), PrepareTxDataError> {
    check_decoded_length(decoded, 7)?;
    let dex_fee = match decoded.get(1) {
        Some(Token::Uint(dex_fee)) => *dex_fee,
        other => {
            return Err(PrepareTxDataError::Internal(format!(
                "Invalid token type for dex fee, got decoded function data: {other:?}"
            )))
        },
    };
    let amount = tx_value
        .checked_sub(dex_fee)
        .ok_or_else(|| PrepareTxDataError::Internal("Underflow occurred while calculating amount".into()))?;
    Ok((dex_fee, amount))
}
