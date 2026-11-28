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
use crate::eth::eth_swap_evidence::V2PaymentSpec;
use crate::eth::legacy_tx::Action;
use crate::eth::{decode_contract_call, get_function_input_data, u256_from_big_decimal, EthCoin, EthCoinType,
                 ParseCoinAssocTypes, RefundFundingSecretArgs, RefundTakerPaymentArgs, SendTakerFundingArgs,
                 SignedEthTx, SwapTxTypeWithSecretHash, TakerPaymentStateV2, TransactionErr, ValidateSwapV2TxError,
                 ValidateSwapV2TxResult, ValidateTakerFundingArgs, TAKER_SWAP_V2};
use crate::{FindPaymentSpendError, FundingTxSpend, GenTakerFundingSpendArgs, GenTakerPaymentSpendArgs,
            SearchForFundingSpendErr};
use common::log::debug;
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
            // TRON coins are wallet-only while
            // `TRON_SWAPS_AVAILABLE` is `false` (CRD ch.21 R-D1, R21.B3), so a swap never reaches this branch.
            EthCoinType::Tron | EthCoinType::Trc20 { .. } => Err(TransactionErr::Plain(ERRL!(
                "{}",
                crate::eth::tron_swaps_unsupported("taker funding")
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
            // TRON coins are wallet-only while
            // `TRON_SWAPS_AVAILABLE` is `false` (CRD ch.21 R-D1, R21.B3), so a swap never reaches this branch.
            EthCoinType::Tron | EthCoinType::Trc20 { .. } => MmError::err(ValidateSwapV2TxError::InternalError(
                crate::eth::tron_swaps_unsupported("taker funding validation"),
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
                    "{}",
                    crate::eth::tron_swaps_unsupported("taker payment approve")
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

/// The positions of the maker's secret hash and of the payment lock time in the decoded funding call, for an ETH
/// and for an ERC20 funding (the rows of [`verify_eth_taker_calldata`] and [`verify_erc20_taker_calldata`]).
fn funding_call_positions(coin_type: &EthCoinType) -> (usize, usize) {
    match coin_type {
        EthCoinType::Eth => (4, 6),
        _ => (6, 8),
    }
}

/// Reads the maker's secret hash and the payment lock time from the decoded funding call.
fn maker_secret_hash_and_payment_lock(decoded: &[Token], positions: (usize, usize)) -> Result<(Vec<u8>, u64), String> {
    let maker_secret_hash = match decoded.get(positions.0) {
        Some(Token::FixedBytes(hash)) => hash.clone(),
        _ => return Err("the funding call holds no maker secret hash".to_owned()),
    };
    let payment_time_lock = match decoded.get(positions.1) {
        Some(Token::Uint(lock)) if *lock <= U256::from(u64::MAX) => lock.low_u64(),
        _ => return Err("the funding call holds no payment lock time".to_owned()),
    };
    Ok((maker_secret_hash, payment_time_lock))
}

impl EthCoin {
    /// Refunds the taker funding by its timelock. The caller passes the funding, its own lock time and
    /// `TakerFunding`, as for any coin. The contract keys the funding and the taker payment by the payment lock
    /// time and the maker's secret hash, so both are read from the funding call and the contract's timelock
    /// refund is called with them.
    pub(crate) async fn refund_taker_funding_timelock_impl(
        &self,
        args: RefundTakerPaymentArgs<'_>,
    ) -> Result<SignedEthTx, TransactionErr> {
        let taker_secret_hash = match args.tx_type_with_secret_hash {
            SwapTxTypeWithSecretHash::TakerFunding { taker_secret_hash } => taker_secret_hash,
            _ => {
                return Err(TransactionErr::Plain(ERRL!(
                    "Unsupported swap tx type for funding timelock refund"
                )))
            },
        };
        let funding_tx = try_tx_s!(self.parse_tx(args.payment_tx));
        let (decoded, _) = try_tx_s!(self.get_funding_decoded_and_swap_contract(&funding_tx).await);
        let (maker_secret_hash, payment_time_lock) = try_tx_s!(maker_secret_hash_and_payment_lock(
            &decoded,
            funding_call_positions(&self.coin_type)
        ));
        let payment_args = RefundTakerPaymentArgs {
            time_lock: payment_time_lock,
            tx_type_with_secret_hash: SwapTxTypeWithSecretHash::TakerPaymentV2 {
                maker_secret_hash: &maker_secret_hash,
                taker_secret_hash,
            },
            ..args
        };
        self.refund_taker_payment_with_timelock_impl(payment_args).await
    }

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

        // CRD ch.51 R100: under an adopted policy the state is read from two nodes (or the trusted node), each at its
        // own latest block; an undetermined read is a transient `Rpc` error with the marker, never "not spent".
        if let Some(policy) = self.agreed_swap_reads() {
            let Some(Token::FixedBytes(id)) = decoded.first() else {
                return Err(SearchForFundingSpendErr::Internal(
                    "the funding call has no payment id".to_owned(),
                ));
            };
            let spec = self.taker_payment_spec(taker_swap_v2_contract);
            let approved = self
                .agreed_v2_payment_is(&policy, &spec, id, TakerPaymentStateV2::TakerApproved as u8)
                .await
                .map_err(SearchForFundingSpendErr::Rpc)?;
            return Ok(approved.then(|| FundingTxSpend::TransferredToTakerPayment(tx.clone())));
        }

        let taker_status = self
            .payment_status_v2(
                taker_swap_v2_contract,
                decoded[0].clone(),
                &TAKER_SWAP_V2,
                EthPaymentType::TakerPayments,
                TAKER_PAYMENT_STATE_INDEX,
            )
            .await
            .map_err(funding_spend_search_err)?;

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
        // CRD ch.51 R100: under an adopted policy the spend is shown by two nodes (or the trusted node).
        if let Some(policy) = self.agreed_swap_reads() {
            let spec = self.taker_payment_spec(taker_swap_v2_contract);
            return self
                .wait_for_agreed_v2_spend(&policy, &spec, &id_array, from_block, wait_until, check_every)
                .await;
        }
        let tx_hash = self.find_transaction_hash_by_event(params).await?;
        self.wait_for_transaction(tx_hash, wait_until, check_every).await
    }

    /// Where the payments of the taker swap contract at `contract` are read (see [`V2PaymentSpec`]).
    fn taker_payment_spec(&self, contract: Address) -> V2PaymentSpec {
        V2PaymentSpec {
            contract,
            abi: &TAKER_SWAP_V2,
            getter: EthPaymentType::TakerPayments.as_str(),
            state_index: TAKER_PAYMENT_STATE_INDEX,
            spent_state: TakerPaymentStateV2::MakerSpent as u8,
            spent_event: EVT_TAKER_PAYMENT_SPENT,
        }
    }

    /// Pulls the maker's secret out of the calldata of a
    /// `spendTakerPayment` tx. The secret is returned only when its hash
    /// equals `secret_hash`.
    pub(crate) async fn extract_secret_v2_impl(
        &self,
        secret_hash: &[u8],
        spend_tx: &SignedEthTx,
    ) -> Result<[u8; 32], ExtractSecretV2Err> {
        secret_of_taker_payment_spend(&spend_tx.data, secret_hash)
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
                crate::eth::tron_swaps_unsupported("taker payment approve data"),
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
                crate::eth::tron_swaps_unsupported("spend taker payment data"),
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
                    "{}",
                    crate::eth::tron_swaps_unsupported("swap contract details")
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
                return Err(PrepareTxDataError::Internal(crate::eth::tron_swaps_unsupported(
                    "funding decoding",
                )));
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
        let decoded_tokens = function
            .decode_output(&bytes.0)
            .map_err(|e| PaymentStatusErr::NodeAnswer(e.to_string()))?;

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
    /// The node answered, and its answer does not decode as the payment state.
    #[display(fmt = "Undecodable answer of the node: {}", _0)]
    NodeAnswer(String),
    #[display(fmt = "Internal error: {}", _0)]
    Internal(String),
    #[display(fmt = "Invalid data error: {}", _0)]
    InvalidData(String),
}

impl From<crate::eth::abi::AbiError> for PaymentStatusErr {
    fn from(err: crate::eth::abi::AbiError) -> Self { PaymentStatusErr::ABIError(err.to_string()) }
}

/// The fixed reason of a search for the funding spend whose read failed.
const FUNDING_STATE_READ_FAILED: &str = "the read of the funding payment state failed";

/// How a failed read of the payment state is reported by the search for the funding spend (CRD ch.52 R18).
/// A failure to reach the node, and an answer of the node that does not decode, are the transient `Rpc`
/// kind, after which the search is repeated. Every other failure is not a node error.
fn funding_spend_search_err(err: PaymentStatusErr) -> SearchForFundingSpendErr {
    match err {
        PaymentStatusErr::Transport(_) | PaymentStatusErr::NodeAnswer(_) => {
            // The text of the node goes to the debug log only; the read is undetermined, never "not spent".
            debug!("the read of the funding payment state failed: {}", err);
            SearchForFundingSpendErr::Rpc(crate::swap_evidence::undetermined(FUNDING_STATE_READ_FAILED, &[]))
        },
        PaymentStatusErr::ABIError(_) | PaymentStatusErr::Internal(_) | PaymentStatusErr::InvalidData(_) => {
            SearchForFundingSpendErr::Internal(err.to_string())
        },
    }
}

/// Why no secret is taken from a spend of the taker payment.
#[derive(Debug, Display, PartialEq)]
pub(crate) enum ExtractSecretV2Err {
    /// The transaction is not a well-formed `spendTakerPayment` call.
    #[display(fmt = "Invalid spend tx: {}", _0)]
    InvalidSpendTx(String),
    /// The value the call carries is not the secret of the swap's secret hash.
    #[display(fmt = "The secret of the spend does not match the secret hash {}", _0)]
    SecretHashMismatch(String),
    /// The secret hash has an unsupported width.
    #[display(fmt = "Invalid secret hash: {}", _0)]
    InvalidSecretHash(String),
}

/// The secret of `secret_hash` in the call input of a `spendTakerPayment` transaction (CRD ch.52 R32). The
/// value the call carries is returned only when its hash equals `secret_hash`: `sha256` for a 32-byte hash
/// and `dhash160` for a 20-byte one, as for every swap secret.
///
/// # Errors
///
/// The input is not such a call, the value is not the secret of `secret_hash`, or `secret_hash` has an
/// unsupported width.
fn secret_of_taker_payment_spend(call_input: &[u8], secret_hash: &[u8]) -> Result<[u8; 32], ExtractSecretV2Err> {
    let function = TAKER_SWAP_V2
        .function(FN_SPEND_TAKER_PAYMENT)
        .map_err(|e| ExtractSecretV2Err::InvalidSpendTx(e.to_string()))?;
    let expected_signature = function.short_signature();
    let found_signature = call_input.get(..4);
    if found_signature != Some(expected_signature.as_slice()) {
        return Err(ExtractSecretV2Err::InvalidSpendTx(format!(
            "Expected 'spendTakerPayment' contract call signature: {expected_signature:?}, found {found_signature:?}"
        )));
    }

    let decoded =
        decode_contract_call(function, call_input).map_err(|e| ExtractSecretV2Err::InvalidSpendTx(e.to_string()))?;
    if decoded.len() < 7 {
        return Err(ExtractSecretV2Err::InvalidSpendTx(format!(
            "Invalid arguments in 'spendTakerPayment' call: {decoded:?}"
        )));
    }

    let secret = match &decoded[5] {
        Token::FixedBytes(secret) => secret,
        other => {
            return Err(ExtractSecretV2Err::InvalidSpendTx(format!(
                "Expected secret to be fixed bytes, but decoded function data is {other:?}"
            )))
        },
    };
    let matches = crypto::secret_hash_algo::secret_matches_hash(secret, secret_hash)
        .map_err(ExtractSecretV2Err::InvalidSecretHash)?;
    if !matches {
        return Err(ExtractSecretV2Err::SecretHashMismatch(hex::encode(secret_hash)));
    }
    secret
        .as_slice()
        .try_into()
        .map_err(|_| ExtractSecretV2Err::InvalidSpendTx(format!("The secret is {} bytes long", secret.len())))
}

/// Whether a call input is a call of one of the contract's taker payment refund functions (by the function
/// selector), that is, the taker's refund of the taker payment. Any other input, a claim among them, is not.
pub(crate) fn is_taker_payment_refund_call(call_input: &[u8]) -> bool {
    let Some(selector) = call_input.get(..4) else {
        return false;
    };
    [FN_REFUND_TAKER_PAYMENT_TIMELOCK, FN_REFUND_TAKER_PAYMENT_SECRET]
        .iter()
        .filter_map(|name| TAKER_SWAP_V2.function(name).ok())
        .any(|function| function.short_signature().as_slice() == selector)
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

#[cfg(test)]
mod funding_spend_search_and_secret_tests {
    use super::*;
    use crypto::secret_hash_algo::SecretHashAlgo;

    const SECRET: [u8; 32] = [0x44; 32];

    /// The call input of a `spendTakerPayment` transaction that carries `secret`.
    fn spend_call_input(secret: &[u8]) -> Vec<u8> {
        TAKER_SWAP_V2
            .function(FN_SPEND_TAKER_PAYMENT)
            .unwrap()
            .encode_input(&[
                Token::FixedBytes(vec![0x11; 32]),
                Token::Uint(U256::from(1000u64)),
                Token::Uint(U256::from(10u64)),
                Token::Address(Address::from([0x22; 20])),
                Token::FixedBytes(vec![0x33; 32]),
                Token::FixedBytes(secret.to_vec()),
                Token::Address(Address::default()),
            ])
            .unwrap()
    }

    /// The taker's refund calls are recognized by their selector; a claim and unreadable input are not refunds.
    #[test]
    fn a_refund_call_of_the_taker_payment_is_recognized_by_its_selector() {
        for name in [FN_REFUND_TAKER_PAYMENT_TIMELOCK, FN_REFUND_TAKER_PAYMENT_SECRET] {
            let selector = TAKER_SWAP_V2.function(name).unwrap().short_signature();
            let mut input = selector.to_vec();
            input.extend_from_slice(&[0u8; 64]);
            assert!(is_taker_payment_refund_call(&input), "{name}");
        }
        assert!(!is_taker_payment_refund_call(&spend_call_input(&SECRET)));
        assert!(!is_taker_payment_refund_call(&[0x01, 0x02]));
        assert!(!is_taker_payment_refund_call(&[]));
    }

    /// CRD ch.52 R18: a failure to reach the node is the transient kind of search error.
    #[test]
    fn node_error_of_the_payment_state_read_is_a_transient_search_error() {
        for err in [
            PaymentStatusErr::Transport("connection refused".into()),
            PaymentStatusErr::NodeAnswer("empty answer".into()),
        ] {
            let mapped = funding_spend_search_err(err);
            assert!(matches!(mapped, SearchForFundingSpendErr::Rpc(_)), "{mapped}");
        }
    }

    /// CRD ch.51 R100, ch.52 R18: a failed read of the payment state is undetermined and carries no text of the
    /// node.
    #[test]
    fn a_failed_payment_state_read_is_marked_and_carries_no_node_text() {
        for err in [
            PaymentStatusErr::Transport("connection to node.example refused".into()),
            PaymentStatusErr::NodeAnswer("empty answer from node.example".into()),
        ] {
            let SearchForFundingSpendErr::Rpc(text) = funding_spend_search_err(err) else {
                panic!("a transient search error");
            };
            assert!(crate::swap_evidence::is_undetermined(&text), "{text}");
            assert!(!text.contains("node.example"), "{text}");
        }
    }

    /// CRD ch.52 R18: a failure that is not the node's is not the transient kind.
    #[test]
    fn other_error_of_the_payment_state_read_is_not_a_transient_search_error() {
        for err in [
            PaymentStatusErr::ABIError("no such function".into()),
            PaymentStatusErr::Internal("no state token".into()),
            PaymentStatusErr::InvalidData("state is not a number".into()),
        ] {
            let mapped = funding_spend_search_err(err);
            assert!(matches!(mapped, SearchForFundingSpendErr::Internal(_)), "{mapped}");
        }
    }

    /// CRD ch.52 R32: the secret of the secret hash is returned.
    #[test]
    fn secret_of_the_spend_is_returned_when_its_hash_matches() {
        let secret_hash = SecretHashAlgo::SHA256.hash_secret(&SECRET);
        assert!(
            secret_of_taker_payment_spend(&spend_call_input(&SECRET), &secret_hash) == Ok(SECRET),
            "the secret of a 32-byte hash is returned"
        );
        let short_hash = SecretHashAlgo::DHASH160.hash_secret(&SECRET);
        assert!(
            secret_of_taker_payment_spend(&spend_call_input(&SECRET), &short_hash) == Ok(SECRET),
            "the secret of a 20-byte hash is returned"
        );
    }

    /// The timelock refund of the funding reads the maker's secret hash and the payment lock time from the
    /// funding call, for an ETH and for an ERC20 funding, and refuses a call that does not hold them.
    #[test]
    fn the_funding_timelock_refund_reads_its_keys_from_the_funding_call() {
        let hash = vec![0x42u8; 32];
        let eth_call = vec![
            Token::FixedBytes(vec![0; 32]),
            Token::Uint(U256::from(1)),
            Token::Address(Address::zero()),
            Token::FixedBytes(vec![0x41; 32]),
            Token::FixedBytes(hash.clone()),
            Token::Uint(U256::from(1_000)),
            Token::Uint(U256::from(2_000)),
        ];
        let read = maker_secret_hash_and_payment_lock(&eth_call, funding_call_positions(&EthCoinType::Eth));
        assert!(read == Ok((hash.clone(), 2_000)), "the ETH funding call gives its keys");

        let erc20_call = vec![
            Token::FixedBytes(vec![0; 32]),
            Token::Uint(U256::from(5)),
            Token::Uint(U256::from(1)),
            Token::Address(Address::zero()),
            Token::Address(Address::zero()),
            Token::FixedBytes(vec![0x41; 32]),
            Token::FixedBytes(hash.clone()),
            Token::Uint(U256::from(1_000)),
            Token::Uint(U256::from(2_000)),
        ];
        let erc20 = EthCoinType::Erc20 {
            platform: "ETH".into(),
            token_addr: Address::zero(),
        };
        let read = maker_secret_hash_and_payment_lock(&erc20_call, funding_call_positions(&erc20));
        assert!(read == Ok((hash, 2_000)), "the ERC20 funding call gives its keys");

        let mut too_large = eth_call.clone();
        too_large[6] = Token::Uint(U256::max_value());
        assert!(
            maker_secret_hash_and_payment_lock(&too_large, funding_call_positions(&EthCoinType::Eth)).is_err(),
            "a call without the keys is refused"
        );
    }

    /// CRD ch.52 R32: a value whose hash is not the secret hash is refused.
    #[test]
    fn secret_of_the_spend_is_refused_when_its_hash_does_not_match() {
        let secret_hash = SecretHashAlgo::SHA256.hash_secret(&SECRET);
        let other = [0x55u8; 32];
        let refused = secret_of_taker_payment_spend(&spend_call_input(&other), &secret_hash);
        assert!(
            refused == Err(ExtractSecretV2Err::SecretHashMismatch(hex::encode(&secret_hash))),
            "a non-matching secret is refused"
        );
    }

    /// A secret hash of an unsupported width and an input that is not the spend call are refused.
    #[test]
    fn secret_of_the_spend_is_refused_for_a_bad_hash_width_or_a_foreign_call() {
        let refused = secret_of_taker_payment_spend(&spend_call_input(&SECRET), &[0u8; 21]);
        assert!(
            matches!(refused, Err(ExtractSecretV2Err::InvalidSecretHash(_))),
            "a secret hash of an unsupported width is refused"
        );

        let secret_hash = SecretHashAlgo::SHA256.hash_secret(&SECRET);
        for input in [&[][..], &[0x01, 0x02][..], &[0xde, 0xad, 0xbe, 0xef, 0x00][..]] {
            let refused = secret_of_taker_payment_spend(input, &secret_hash);
            assert!(
                matches!(refused, Err(ExtractSecretV2Err::InvalidSpendTx(_))),
                "an input that is not the spend call is refused"
            );
        }
    }
}
