//! # Purpose
//! Cross-cutting EVM helpers shared by the maker, taker, and NFT V2
//! swap modules: protocol-side enums, ABI shape descriptors, on-chain
//! polling primitives, and tx-shape validators.
//!
//! # Public exports
//! - [`EthPaymentType`], [`PaymentMethod`] — protocol-side enums passed
//!   to gas-table lookups and contract dispatchers.
//! - [`SpendTxSearchParams`] — argument bag for [`EthCoin::find_transaction_hash_by_event`].
//! - [`PrepareTxDataError`], [`ValidatePaymentV2Err`] — error taxonomies.
//! - [`ZERO_VALUE`] — sentinel for "value is encoded in calldata, not msg.value".
//! - [`validate_from_to_addresses`], [`extract_id_from_tx_data`] — free helpers.
//!
//! # Invariants
//! - [`EthPaymentType::as_str`] returns the literal solidity field
//!   name (`makerPayments` / `takerPayments`); changing these strings
//!   breaks ABI dispatch.
//! - [`ZERO_VALUE`] is consumed by the eth_*_swap_v2 modules and by
//!   the NFT swap_v2 module; renaming requires updating all call sites.

use crate::eth::abi::{Contract, Token};
use crate::eth::legacy_tx::{Action, SignedTransaction as SignedEthTx};
use crate::eth::{decode_contract_call, signed_tx_from_alloy_tx, EthCoin, EthCoinType, Log, Transaction, TransactionErr};
use crate::{FindPaymentSpendError, MarketCoinOps};
use bigdecimal::BigDecimal;
use common::executor::Timer;
use common::log::{error, info};
use common::now_ms;
use derive_more::Display;
use ethereum_types::{Address, H256, U256};
use futures::compat::Future01CompatExt;
use mm2_err_handle::prelude::{MmError, MmResult};
use num_traits::Signed;

pub(crate) mod eth_maker_swap_v2;
pub(crate) mod eth_taker_swap_v2;
pub(crate) mod nft_swap_v2;

// Protocol-side enums and constants ----------------------------------------

/// Used by V2 contract calls where the asset amount is carried inside
/// the calldata rather than as `msg.value`. Passing `0` for `msg.value`
/// in those calls is intentional, not a bug.
pub(crate) const ZERO_VALUE: u32 = 0;

/// Which side of the swap a payment originates from.
///
/// Selects between the `makerPayments` and `takerPayments` mapping in
/// the V2 swap contract.
pub enum EthPaymentType {
    MakerPayments,
    TakerPayments,
}

impl EthPaymentType {
    /// Returns the solidity-side field name. The string is consumed
    /// by ABI lookups so it must match the contract verbatim.
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            EthPaymentType::MakerPayments => "makerPayments",
            EthPaymentType::TakerPayments => "takerPayments",
        }
    }
}

/// Which on-chain entrypoint a particular call is targeting.
///
/// The four variants map 1:1 to the V2 swap contract's user-facing
/// methods and are used as a key into the per-protocol gas tables.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PaymentMethod {
    Send,
    Spend,
    RefundTimelock,
    RefundSecret,
}

// Error taxonomies ---------------------------------------------------------

/// Failure mode returned by [`validate_from_to_addresses`].
#[derive(Debug, Display)]
pub(crate) enum ValidatePaymentV2Err {
    WrongPaymentTx(String),
}

/// Failure modes raised while preparing or decoding contract calldata.
#[derive(Debug, Display)]
pub(crate) enum PrepareTxDataError {
    #[display(fmt = "ABI error: {}", _0)]
    ABIError(String),
    #[display(fmt = "Internal error: {}", _0)]
    Internal(String),
    #[display(fmt = "Invalid data error: {}", _0)]
    InvalidData(String),
}

impl From<crate::eth::abi::AbiError> for PrepareTxDataError {
    fn from(err: crate::eth::abi::AbiError) -> Self { PrepareTxDataError::ABIError(err.to_string()) }
}

// On-chain polling helpers -------------------------------------------------

/// Argument bag for [`EthCoin::find_transaction_hash_by_event`].
///
/// Bundles the contract address, event name, ABI handle, expected
/// `swap_id`, and the polling parameters into one struct so that the
/// search function keeps a stable signature as the search criteria
/// evolve.
pub(crate) struct SpendTxSearchParams<'a> {
    pub(crate) swap_contract_address: Address,
    pub(crate) event_name: &'a str,
    pub(crate) abi_contract: &'a Contract,
    pub(crate) swap_id: &'a [u8; 32],
    pub(crate) from_block: u64,
    pub(crate) wait_until: u64,
    pub(crate) check_every: f64,
}

/// Predicate matching an event whose first 32 bytes equal the given
/// `swap_id`. Pulled out so the scanning loop reads as one
/// declarative check instead of an inline closure.
fn event_matches_swap_id(event: &Log, swap_id: &[u8; 32]) -> bool {
    event.data.0.len() >= 32 && &event.data.0[..32] == swap_id
}

impl EthCoin {
    /// Polls the chain in fixed-size block windows until an event
    /// carrying the supplied `swap_id` shows up, then returns the
    /// transaction hash that emitted it.
    ///
    /// Honours `wait_until` (UNIX seconds) as a hard deadline. Each
    /// iteration sleeps `check_every` seconds before re-polling.
    /// Block-fetch and event-fetch failures are logged and retried
    /// rather than propagated, because transient RPC errors are
    /// expected during a long swap.
    ///
    /// The 32-byte `swap_id` is assumed to be the first 32 bytes of
    /// the event data — this matches the V2 swap contract's emitted
    /// events.
    pub(crate) async fn find_transaction_hash_by_event(
        &self,
        params: SpendTxSearchParams<'_>,
    ) -> MmResult<H256, FindPaymentSpendError> {
        loop {
            let now_secs = now_ms() / 1000;
            if now_secs > params.wait_until {
                return MmError::err(FindPaymentSpendError::Timeout {
                    wait_until: params.wait_until,
                    now: now_secs,
                });
            }

            let current_block = match self.current_block().compat().await {
                Ok(block) => block,
                Err(e) => {
                    error!("Error getting block number: {}", e);
                    Timer::sleep(params.check_every).await;
                    continue;
                },
            };

            let mut window_start = params.from_block;
            while window_start <= current_block {
                let window_end = std::cmp::min(window_start + self.logs_block_range - 1, current_block);

                let events = match self
                    .events_from_block(
                        params.swap_contract_address,
                        params.event_name,
                        window_start,
                        Some(window_end),
                        params.abi_contract,
                    )
                    .await
                {
                    Ok(events) => events,
                    Err(e) => {
                        error!(
                            "Error getting {} events from {} to {} block: {}",
                            params.event_name, window_start, window_end, e
                        );
                        Timer::sleep(params.check_every).await;
                        continue;
                    },
                };

                if let Some(matched) = events.into_iter().find(|ev| event_matches_swap_id(ev, params.swap_id)) {
                    if let Some(hash) = matched.transaction_hash {
                        return Ok(hash);
                    }
                }

                window_start += self.logs_block_range;
            }

            Timer::sleep(params.check_every).await;
        }
    }

    /// Polls `eth_getTransactionByHash` until the node reports the tx,
    /// or until `wait_until` (UNIX seconds) passes.
    pub(crate) async fn wait_for_transaction(
        &self,
        tx_hash: H256,
        wait_until: u64,
        check_every: f64,
    ) -> MmResult<SignedEthTx, FindPaymentSpendError> {
        loop {
            let now_secs = now_ms() / 1000;
            if now_secs > wait_until {
                return MmError::err(FindPaymentSpendError::Timeout {
                    wait_until,
                    now: now_secs,
                });
            }

            // LP-17: alloy `Provider::get_transaction_by_hash` replaces
            // `web3.eth().transaction(...)`. Wire-level RPC method
            // `eth_getTransactionByHash` is unchanged. The fetched
            // alloy `Transaction` is round-tripped through
            // `signed_tx_from_alloy_tx` to keep the legacy
            // `SignedEthTx` shape that downstream V2 swap state
            // machines persist.
            use crate::eth::alloy_compat::assert_send_future;
            use alloy::providers::Provider;

            let provider = self.alloy_provider();
            let alloy_hash = alloy::primitives::B256::from_slice(&tx_hash.0);
            match assert_send_future(provider.get_transaction_by_hash(alloy_hash)).await {
                Ok(Some(raw_tx)) => {
                    let signed = signed_tx_from_alloy_tx(raw_tx).map_err(FindPaymentSpendError::Internal)?;
                    return Ok(signed);
                },
                Ok(None) => info!("Transaction {} not found yet", tx_hash),
                Err(e) => error!("Get transaction {} error: {}", tx_hash, e),
            };

            Timer::sleep(check_every).await;
        }
    }

    /// Approves the swap contract to spend up to `payment_amount` of
    /// the underlying ERC-20 if the existing allowance is insufficient.
    ///
    /// Approves `U256::max_value()` to avoid having to re-approve on
    /// every swap; this matches established ERC-20 wallet patterns.
    /// Waits for the on-chain allowance to update before returning so
    /// that the caller's subsequent `transferFrom` does not race the
    /// approval.
    async fn handle_allowance(
        &self,
        swap_contract: Address,
        payment_amount: U256,
        time_lock: u64,
    ) -> Result<(), TransactionErr> {
        let allowed = self
            .allowance(swap_contract)
            .compat()
            .await
            .map_err(|e| TransactionErr::Plain(ERRL!("{}", e)))?;

        if allowed < payment_amount {
            let approved_tx = self.approve(swap_contract, U256::max_value()).compat().await?;
            self.wait_for_required_allowance(swap_contract, payment_amount, time_lock)
                .compat()
                .await
                .map_err(|e| {
                    TransactionErr::Plain(ERRL!(
                        "Allowed value was not updated in time after sending approve transaction {:?}: {}",
                        approved_tx.hash,
                        e
                    ))
                })?;
        }
        Ok(())
    }
}

// Free helpers -------------------------------------------------------------

/// Verifies that a signed tx has the expected `from` and `to` addresses.
///
/// `Action::Create` (contract-creation tx) is rejected outright: the V2
/// swap protocol never creates contracts via user payments.
pub(crate) fn validate_from_to_addresses(
    signed_tx: &SignedEthTx,
    expected_from: Address,
    expected_to: Address,
) -> Result<(), MmError<ValidatePaymentV2Err>> {
    let actual_from = signed_tx.sender();
    if actual_from != expected_from {
        return MmError::err(ValidatePaymentV2Err::WrongPaymentTx(format!(
            "Payment tx {signed_tx:?} was sent from wrong address, expected {expected_from:#02x}, got {actual_from:#02x}"
        )));
    }

    match signed_tx.action {
        Action::Call(ref actual_to) if actual_to == &expected_to => Ok(()),
        Action::Call(ref actual_to) => MmError::err(ValidatePaymentV2Err::WrongPaymentTx(format!(
            "Payment tx was sent to wrong address, expected {expected_to:#02x}, got {actual_to:#02x}"
        ))),
        Action::Create => MmError::err(ValidatePaymentV2Err::WrongPaymentTx(
            "Tx action must be Call, found Create instead".to_string(),
        )),
    }
}

/// Rejects zero or negative trading amounts.
fn validate_amount(trading_amount: &BigDecimal) -> Result<(), String> {
    if trading_amount.is_positive() {
        Ok(())
    } else {
        Err("trading_amount must be a positive value".to_string())
    }
}

/// Asserts that an ABI decoder produced exactly `expected_len` tokens.
fn check_decoded_length(decoded: &[Token], expected_len: usize) -> Result<(), PrepareTxDataError> {
    if decoded.len() == expected_len {
        Ok(())
    } else {
        Err(PrepareTxDataError::Internal(format!(
            "Invalid number of tokens in decoded. Expected {}, found {}",
            expected_len,
            decoded.len()
        )))
    }
}

/// Decodes one ABI-encoded contract call and returns the first token
/// as a 32-byte fixed-bytes payload — used to extract the `swap_id`
/// from a payment transaction's calldata.
pub(crate) async fn extract_id_from_tx_data(
    tx_data: &[u8],
    abi_contract: &Contract,
    func_name: &str,
) -> Result<Vec<u8>, FindPaymentSpendError> {
    let func = abi_contract.function(func_name)?;
    let decoded = decode_contract_call(func, tx_data)?;
    match decoded.first() {
        Some(Token::FixedBytes(bytes)) => Ok(bytes.clone()),
        invalid_token => Err(FindPaymentSpendError::InvalidData(format!(
            "Expected Token::FixedBytes, got {invalid_token:?}"
        ))),
    }
}
