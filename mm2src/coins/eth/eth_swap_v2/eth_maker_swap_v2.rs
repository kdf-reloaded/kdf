//! # Purpose
//! Maker-side EVM (Ethereum + ERC-20) payment paths for the V2
//! (Trading Protocol Upgrade) atomic-swap state machine.
//!
//! # Public exports
//! Method-only surface on [`crate::eth::EthCoin`]:
//! - [`EthCoin::send_maker_payment_v2_impl`]
//! - [`EthCoin::validate_maker_payment_v2_impl`]
//! - [`EthCoin::refund_maker_payment_v2_timelock_impl`]
//! - [`EthCoin::refund_maker_payment_v2_secret_impl`]
//! - [`EthCoin::spend_maker_payment_v2_impl`]
//!
//! # Invariants
//! - Solidity entrypoint names (`ethMakerPayment`, `erc20MakerPayment`,
//!   `refundMakerPaymentTimelock`, `refundMakerPaymentSecret`,
//!   `spendMakerPayment`) are part of the on-chain ABI; do not rename.
//! - Argument ordering inside each ABI call is fixed by the deployed
//!   `EtomicSwapMakerV2` contract; reorder a `Token::*` and you break
//!   real on-chain swaps.

use super::{validate_amount, validate_from_to_addresses, EthPaymentType, PaymentMethod, PrepareTxDataError, ZERO_VALUE};
use crate::eth::legacy_tx::Action;
use crate::eth::{decode_contract_call, get_function_input_data, u256_from_big_decimal, EthCoin, EthCoinType,
                 SignedEthTx, ValidatePaymentError, ValidatePaymentResult, MAKER_SWAP_V2};
use crate::{ParseCoinAssocTypes, RefundMakerPaymentSecretArgs, RefundMakerPaymentTimelockArgs, SendMakerPaymentArgs,
            SpendMakerPaymentArgs, SwapTxTypeWithSecretHash, TransactionErr, ValidateMakerPaymentArgs};
use ethabi::{Function, Token};
use ethereum_types::{Address, Public, U256};
use futures::compat::Future01CompatExt;
use mm2_err_handle::prelude::{MapToMmResult, MmError, MmResultExt};
use mm2_eth::keys::public_to_address;
use std::convert::TryInto;

// ABI entrypoint names -----------------------------------------------------

const FN_ETH_MAKER_PAYMENT: &str = "ethMakerPayment";
const FN_ERC20_MAKER_PAYMENT: &str = "erc20MakerPayment";
const FN_REFUND_MAKER_PAYMENT_TIMELOCK: &str = "refundMakerPaymentTimelock";
const FN_REFUND_MAKER_PAYMENT_SECRET: &str = "refundMakerPaymentSecret";
const FN_SPEND_MAKER_PAYMENT: &str = "spendMakerPayment";

// Internal arg bags --------------------------------------------------------

/// Inputs for `ethMakerPayment` / `erc20MakerPayment` calldata builders.
struct MakerSendInputs<'a> {
    taker_address: Address,
    taker_secret_hash: &'a [u8; 32],
    maker_secret_hash: &'a [u8; 32],
    payment_time_lock: u64,
}

/// Inputs for verifying a maker-payment tx against the swap state.
struct MakerVerifyInputs<'a> {
    swap_id: Vec<u8>,
    amount: U256,
    taker: Address,
    taker_secret_hash: &'a [u8; 32],
    maker_secret_hash: &'a [u8; 32],
    payment_time_lock: u64,
}

/// Inputs for `refundMakerPaymentTimelock` calldata builder.
struct MakerTimelockRefundInputs<'a> {
    payment_amount: U256,
    taker_address: Address,
    taker_secret_hash: &'a [u8; 32],
    maker_secret_hash: &'a [u8; 32],
    payment_time_lock: u64,
    token_address: Address,
}

/// Inputs for `refundMakerPaymentSecret` calldata builder.
struct MakerSecretRefundInputs<'a> {
    payment_amount: U256,
    taker_address: Address,
    taker_secret: &'a [u8; 32],
    maker_secret_hash: &'a [u8; 32],
    payment_time_lock: u64,
    token_address: Address,
}

// Setup resolver -----------------------------------------------------------

impl EthCoin {
    /// Resolves the deployed maker-V2 contract address. Returned as a
    /// `TransactionErr::Plain` so callers can bubble it through the
    /// `try_tx_s!` machinery.
    fn resolve_maker_v2_contract(&self) -> Result<Address, TransactionErr> {
        self.swap_v2_contracts
            .map(|c| c.maker_swap_v2_contract)
            .ok_or_else(|| TransactionErr::Plain(ERRL!("Expected swap_v2_contracts to be Some, but found None")))
    }

    /// Resolves the deployed maker-V2 contract address for validation
    /// paths, mapping the absence into a [`ValidatePaymentError`].
    fn resolve_maker_v2_contract_for_validation(&self) -> Result<Address, MmError<ValidatePaymentError>> {
        self.swap_v2_contracts.map(|c| c.maker_swap_v2_contract).ok_or_else(|| {
            MmError::new(ValidatePaymentError::InternalError(
                "Expected swap_v2_contracts to be Some, but found None".to_string(),
            ))
        })
    }

    /// Looks up the gas limit configured for a particular maker-side
    /// `(coin_type, method)` pair, mapping the lookup error into a
    /// `TransactionErr` for the swap pipeline.
    fn maker_gas_limit(&self, method: PaymentMethod) -> Result<u64, TransactionErr> {
        self.gas_limit_v2
            .gas_limit(&self.coin_type, EthPaymentType::MakerPayments, method)
            .map_err(|e| TransactionErr::Plain(ERRL!("{}", e)))
    }

    /// Returns the underlying ERC-20 token address (or zero for native ETH).
    fn token_address_for_maker(&self) -> Result<Address, TransactionErr> {
        self.get_token_address()
            .map_err(|e| TransactionErr::Plain(ERRL!("{}", e)))
    }
}

// Send maker payment -------------------------------------------------------

impl EthCoin {
    pub(crate) async fn send_maker_payment_v2_impl(
        &self,
        args: SendMakerPaymentArgs<'_, Self>,
    ) -> Result<SignedEthTx, TransactionErr> {
        let maker_swap_v2_contract = self.resolve_maker_v2_contract()?;
        let payment_amount = try_tx_s!(u256_from_big_decimal(&args.amount, self.decimals));

        let payment_args = MakerSendInputs {
            taker_address: public_to_address(args.taker_pub),
            taker_secret_hash: try_tx_s!(args.taker_secret_hash.try_into()),
            maker_secret_hash: try_tx_s!(args.maker_secret_hash.try_into()),
            payment_time_lock: args.time_lock,
        };

        match &self.coin_type {
            EthCoinType::Eth => {
                let data = try_tx_s!(self.prepare_maker_eth_payment_data(&payment_args).await);
                self.sign_and_send_transaction(
                    payment_amount,
                    Action::Call(maker_swap_v2_contract),
                    data,
                    U256::from(self.gas_limit_v2.maker.eth_payment),
                )
                .compat()
                .await
            },
            EthCoinType::Erc20 {
                platform: _,
                token_addr,
            } => {
                let data = try_tx_s!(
                    self.prepare_maker_erc20_payment_data(&payment_args, payment_amount, *token_addr)
                        .await
                );
                self.handle_allowance(maker_swap_v2_contract, payment_amount, args.time_lock)
                    .await?;
                self.sign_and_send_transaction(
                    U256::from(ZERO_VALUE),
                    Action::Call(maker_swap_v2_contract),
                    data,
                    U256::from(self.gas_limit_v2.maker.erc20_payment),
                )
                .compat()
                .await
            },
            // R-D1: TRON is supported only on the version-1 swap path; the
            // version-2 HTLC protocol is permanently out of scope for TRON.
            EthCoinType::Tron | EthCoinType::Trc20 { .. } => Err(TransactionErr::Plain(ERRL!(
                "TRON is not supported on the version-2 swap path"
            ))),
        }
    }
}

// Validate maker payment ---------------------------------------------------

impl EthCoin {
    pub(crate) async fn validate_maker_payment_v2_impl(
        &self,
        args: ValidateMakerPaymentArgs<'_, Self>,
    ) -> ValidatePaymentResult<()> {
        let maker_swap_v2_contract = self.resolve_maker_v2_contract_for_validation()?;

        let taker_secret_hash = args.taker_secret_hash.try_into()?;
        let maker_secret_hash = args.maker_secret_hash.try_into()?;
        validate_amount(&args.amount).map_to_mm(ValidatePaymentError::InternalError)?;

        let tx = args.maker_payment_tx;
        let maker_address = public_to_address(args.maker_pub);
        validate_from_to_addresses(tx, maker_address, maker_swap_v2_contract).map_mm_err()?;

        let inputs = MakerVerifyInputs {
            swap_id: self.etomic_swap_id_v2(args.time_lock, args.maker_secret_hash),
            amount: u256_from_big_decimal(&args.amount, self.decimals).map_mm_err()?,
            taker: self.my_address,
            taker_secret_hash,
            maker_secret_hash,
            payment_time_lock: args.time_lock,
        };

        match self.coin_type {
            EthCoinType::Eth => {
                let function = MAKER_SWAP_V2.function(FN_ETH_MAKER_PAYMENT)?;
                let decoded = decode_contract_call(function, &tx.data)?;
                verify_eth_maker_calldata(&decoded, &inputs, function, tx.value)
            },
            EthCoinType::Erc20 { token_addr, .. } => {
                let function = MAKER_SWAP_V2.function(FN_ERC20_MAKER_PAYMENT)?;
                let decoded = decode_contract_call(function, &tx.data)?;
                verify_erc20_maker_calldata(&decoded, &inputs, function, token_addr)
            },
            // R-D1: TRON is supported only on the version-1 swap path; the
            // version-2 HTLC protocol is permanently out of scope for TRON.
            EthCoinType::Tron | EthCoinType::Trc20 { .. } => MmError::err(ValidatePaymentError::InternalError(
                "TRON is not supported on the version-2 swap path".to_owned(),
            )),
        }
    }
}

// Refund maker payment (timelock + secret) ---------------------------------

impl EthCoin {
    pub(crate) async fn refund_maker_payment_v2_timelock_impl(
        &self,
        args: RefundMakerPaymentTimelockArgs<'_>,
    ) -> Result<SignedEthTx, TransactionErr> {
        let token_address = self.token_address_for_maker()?;
        let maker_swap_v2_contract = self.resolve_maker_v2_contract()?;
        let gas_limit = self.maker_gas_limit(PaymentMethod::RefundTimelock)?;

        let (maker_secret_hash, taker_secret_hash) = match args.tx_type_with_secret_hash {
            SwapTxTypeWithSecretHash::MakerPaymentV2 {
                maker_secret_hash,
                taker_secret_hash,
            } => (maker_secret_hash, taker_secret_hash),
            _ => {
                return Err(TransactionErr::Plain(ERRL!(
                    "Unsupported swap tx type for timelock refund"
                )))
            },
        };

        let payment_amount = try_tx_s!(u256_from_big_decimal(&args.amount, self.decimals));
        let inputs = MakerTimelockRefundInputs {
            payment_amount,
            taker_address: public_to_address(&Public::from_slice(args.taker_pub)),
            taker_secret_hash: try_tx_s!(taker_secret_hash.try_into()),
            maker_secret_hash: try_tx_s!(maker_secret_hash.try_into()),
            payment_time_lock: args.time_lock,
            token_address,
        };
        let data = try_tx_s!(self.prepare_refund_maker_payment_timelock_data(inputs).await);

        self.sign_and_send_transaction(
            U256::from(ZERO_VALUE),
            Action::Call(maker_swap_v2_contract),
            data,
            U256::from(gas_limit),
        )
        .compat()
        .await
    }

    pub(crate) async fn refund_maker_payment_v2_secret_impl(
        &self,
        args: RefundMakerPaymentSecretArgs<'_, Self>,
    ) -> Result<SignedEthTx, TransactionErr> {
        let token_address = self.token_address_for_maker()?;
        let maker_swap_v2_contract = self.resolve_maker_v2_contract()?;
        let gas_limit = self.maker_gas_limit(PaymentMethod::RefundSecret)?;

        let payment_amount = try_tx_s!(u256_from_big_decimal(&args.amount, self.decimals));
        let inputs = MakerSecretRefundInputs {
            payment_amount,
            taker_address: public_to_address(args.taker_pub),
            taker_secret: args.taker_secret,
            maker_secret_hash: try_tx_s!(args.maker_secret_hash.try_into()),
            payment_time_lock: args.time_lock,
            token_address,
        };
        let data = try_tx_s!(self.prepare_refund_maker_payment_secret_data(inputs).await);

        self.sign_and_send_transaction(
            U256::from(ZERO_VALUE),
            Action::Call(maker_swap_v2_contract),
            data,
            U256::from(gas_limit),
        )
        .compat()
        .await
    }
}

// Spend maker payment (taker side) ----------------------------------------

impl EthCoin {
    pub(crate) async fn spend_maker_payment_v2_impl(
        &self,
        args: SpendMakerPaymentArgs<'_, Self>,
    ) -> Result<SignedEthTx, TransactionErr> {
        let token_address = self.token_address_for_maker()?;
        let maker_swap_v2_contract = self.resolve_maker_v2_contract()?;
        let gas_limit = self.maker_gas_limit(PaymentMethod::Spend)?;

        let data = try_tx_s!(self.prepare_spend_maker_payment_data(args, token_address).await);

        self.sign_and_send_transaction(
            U256::from(ZERO_VALUE),
            Action::Call(maker_swap_v2_contract),
            data,
            U256::from(gas_limit),
        )
        .compat()
        .await
    }
}

// Calldata builders --------------------------------------------------------

impl EthCoin {
    /// ABI-encodes a call to `ethMakerPayment(id, taker, takerSecretHash,
    /// makerSecretHash, paymentLockTime)`.
    async fn prepare_maker_eth_payment_data(&self, args: &MakerSendInputs<'_>) -> Result<Vec<u8>, PrepareTxDataError> {
        let function = MAKER_SWAP_V2.function(FN_ETH_MAKER_PAYMENT)?;
        let id = self.etomic_swap_id_v2(args.payment_time_lock, args.maker_secret_hash);
        Ok(function.encode_input(&[
            Token::FixedBytes(id),
            Token::Address(args.taker_address),
            Token::FixedBytes(args.taker_secret_hash.to_vec()),
            Token::FixedBytes(args.maker_secret_hash.to_vec()),
            Token::Uint(args.payment_time_lock.into()),
        ])?)
    }

    /// ABI-encodes a call to `erc20MakerPayment(id, amount, token, taker,
    /// takerSecretHash, makerSecretHash, paymentLockTime)`.
    async fn prepare_maker_erc20_payment_data(
        &self,
        args: &MakerSendInputs<'_>,
        payment_amount: U256,
        token_address: Address,
    ) -> Result<Vec<u8>, PrepareTxDataError> {
        let function = MAKER_SWAP_V2.function(FN_ERC20_MAKER_PAYMENT)?;
        let id = self.etomic_swap_id_v2(args.payment_time_lock, args.maker_secret_hash);
        Ok(function.encode_input(&[
            Token::FixedBytes(id),
            Token::Uint(payment_amount),
            Token::Address(token_address),
            Token::Address(args.taker_address),
            Token::FixedBytes(args.taker_secret_hash.to_vec()),
            Token::FixedBytes(args.maker_secret_hash.to_vec()),
            Token::Uint(args.payment_time_lock.into()),
        ])?)
    }

    /// ABI-encodes a call to `refundMakerPaymentTimelock(id, amount, taker,
    /// takerSecretHash, makerSecretHash, tokenAddress)`.
    async fn prepare_refund_maker_payment_timelock_data(
        &self,
        args: MakerTimelockRefundInputs<'_>,
    ) -> Result<Vec<u8>, PrepareTxDataError> {
        let function = MAKER_SWAP_V2.function(FN_REFUND_MAKER_PAYMENT_TIMELOCK)?;
        let id = self.etomic_swap_id_v2(args.payment_time_lock, args.maker_secret_hash);
        Ok(function.encode_input(&[
            Token::FixedBytes(id),
            Token::Uint(args.payment_amount),
            Token::Address(args.taker_address),
            Token::FixedBytes(args.taker_secret_hash.to_vec()),
            Token::FixedBytes(args.maker_secret_hash.to_vec()),
            Token::Address(args.token_address),
        ])?)
    }

    /// ABI-encodes a call to `refundMakerPaymentSecret(id, amount, taker,
    /// takerSecret, makerSecretHash, tokenAddress)`.
    async fn prepare_refund_maker_payment_secret_data(
        &self,
        args: MakerSecretRefundInputs<'_>,
    ) -> Result<Vec<u8>, PrepareTxDataError> {
        let function = MAKER_SWAP_V2.function(FN_REFUND_MAKER_PAYMENT_SECRET)?;
        let id = self.etomic_swap_id_v2(args.payment_time_lock, args.maker_secret_hash);
        Ok(function.encode_input(&[
            Token::FixedBytes(id),
            Token::Uint(args.payment_amount),
            Token::Address(args.taker_address),
            Token::FixedBytes(args.taker_secret.to_vec()),
            Token::FixedBytes(args.maker_secret_hash.to_vec()),
            Token::Address(args.token_address),
        ])?)
    }

    /// ABI-encodes a call to `spendMakerPayment(id, amount, maker,
    /// takerSecretHash, makerSecret, tokenAddress)`.
    async fn prepare_spend_maker_payment_data(
        &self,
        args: SpendMakerPaymentArgs<'_, Self>,
        token_address: Address,
    ) -> Result<Vec<u8>, PrepareTxDataError> {
        let function = MAKER_SWAP_V2.function(FN_SPEND_MAKER_PAYMENT)?;
        let id = self.etomic_swap_id_v2(args.time_lock, args.maker_secret_hash);
        let maker_address = public_to_address(args.maker_pub);
        let payment_amount = u256_from_big_decimal(&args.amount, self.decimals)
            .map_err(|e| PrepareTxDataError::Internal(e.to_string()))?;
        Ok(function.encode_input(&[
            Token::FixedBytes(id),
            Token::Uint(payment_amount),
            Token::Address(maker_address),
            Token::FixedBytes(args.taker_secret_hash.to_vec()),
            Token::FixedBytes(args.maker_secret.to_vec()),
            Token::Address(token_address),
        ])?)
    }
}

// Calldata verifiers -------------------------------------------------------

/// One row of the `(decoded_index, expected_token, field_name)`
/// verification table consumed by [`verify_token_table`].
type VerifyRow<'a> = (usize, Token, &'a str);

/// Walks a verification table over a decoded calldata vector and
/// raises `WrongPaymentTx` on the first mismatch.
fn verify_token_table(
    decoded: &[Token],
    func: &Function,
    rows: &[VerifyRow<'_>],
    contract_label: &str,
) -> Result<(), MmError<ValidatePaymentError>> {
    for (index, expected_token, field_name) in rows {
        let actual = get_function_input_data(decoded, func, *index).map_to_mm(ValidatePaymentError::InternalError)?;
        if actual != *expected_token {
            return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
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

fn verify_eth_maker_calldata(
    decoded: &[Token],
    args: &MakerVerifyInputs<'_>,
    func: &Function,
    tx_value: U256,
) -> Result<(), MmError<ValidatePaymentError>> {
    let rows: [VerifyRow; 5] = [
        (0, Token::FixedBytes(args.swap_id.clone()), "id"),
        (1, Token::Address(args.taker), "taker"),
        (2, Token::FixedBytes(args.taker_secret_hash.to_vec()), "takerSecretHash"),
        (3, Token::FixedBytes(args.maker_secret_hash.to_vec()), "makerSecretHash"),
        (4, Token::Uint(U256::from(args.payment_time_lock)), "paymentLockTime"),
    ];
    verify_token_table(decoded, func, &rows, "ETH Maker Payment")?;

    if args.amount != tx_value {
        return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
            "ETH Maker Payment amount is invalid, expected {:?}, got {:?}",
            args.amount, tx_value
        )));
    }
    Ok(())
}

fn verify_erc20_maker_calldata(
    decoded: &[Token],
    args: &MakerVerifyInputs<'_>,
    func: &Function,
    token_addr: Address,
) -> Result<(), MmError<ValidatePaymentError>> {
    let rows: [VerifyRow; 7] = [
        (0, Token::FixedBytes(args.swap_id.clone()), "id"),
        (1, Token::Uint(args.amount), "amount"),
        (2, Token::Address(token_addr), "tokenAddress"),
        (3, Token::Address(args.taker), "taker"),
        (4, Token::FixedBytes(args.taker_secret_hash.to_vec()), "takerSecretHash"),
        (5, Token::FixedBytes(args.maker_secret_hash.to_vec()), "makerSecretHash"),
        (6, Token::Uint(U256::from(args.payment_time_lock)), "paymentLockTime"),
    ];
    verify_token_table(decoded, func, &rows, "ERC20 Maker Payment")
}
