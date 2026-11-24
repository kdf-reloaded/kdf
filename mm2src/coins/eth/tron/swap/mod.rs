//! TRON V1 swap interop: what a client of the public Tron-VM V1 swap contract has to exchange with it (CRD ch.21
//! §21.11B.2, §21.11C).
//!
//! - [`ids`]: the swap id, the payment hash the contract stores, and the 32-byte rule of the secret hash.
//! - [`contract_abi`]: the contract's call, read and event surface, and the token approval calls.
//! - [`tx`]: [`TronSwapTx`], the transaction type a swap record keeps for a TRON leg.
//! - [`fee_limit`]: the `fee_limit` of a swap contract call.
//! - [`trigger`]: building, signing and broadcasting a contract call, and its energy dry run.
//! - [`send`]: the sending side of a swap: the payment with its approval, the DEX fee, the own-payment question
//!   and the fee figures.
//!
//! # Invariants
//!
//! - The secret hash of a TRON leg is `SHA-256(secret)`, 32 bytes, and is passed to the contract as it is: no
//!   function of this module narrows it (CRD ch.21 R21.B6, ch.51 R72A).
//! - No error of this module carries a secret, and no error stands for "not spent" or "unspent".
//! - [`trigger::send_trigger`] decides no fee: its caller gives the `fee_limit`, which [`fee_limit`] works out
//!   from the dry run.

pub mod contract_abi;
pub mod fee_limit;
pub mod ids;
pub(crate) mod send;
pub mod trigger;
pub mod tx;
pub mod watch;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod activation_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod contract_abi_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod fee_limit_tests;
#[cfg(all(test, not(target_arch = "wasm32")))] mod ids_tests;
#[cfg(all(test, not(target_arch = "wasm32")))] mod send_tests;
#[cfg(all(test, not(target_arch = "wasm32")))] mod trigger_tests;
#[cfg(all(test, not(target_arch = "wasm32")))] mod tx_tests;
#[cfg(all(test, not(target_arch = "wasm32")))] mod watch_tests;

pub use contract_abi::{decode_allowance, decode_call, decode_event, decode_payments_output, encode_allowance_query,
                       encode_approve, encode_call, encode_payments_query, PaymentRecord, PaymentState, TronSwapCall,
                       TronSwapEvent};
pub use fee_limit::{swap_call_fee_limit, swap_fee_limit_sun, SWAP_FEE_LIMIT_MARGIN_PERCENT};
pub use ids::{require_32_byte_hash, tron_payment_hash, tron_swap_id};
pub use trigger::{dry_run_energy, send_trigger, SWAP_HEAD_TIME_TOLERANCE_SECS};
pub use tx::{call_of, TronSwapTx, TronTrigger};

use super::api::TronApiError;
use super::fee::TronFeeError;
use super::sign::TronSignError;
use crate::eth::abi::AbiError;
use std::fmt;

/// Why a TRON swap interop step failed.
///
/// The messages name the fact and no coin, and never a secret. A caller maps each kind to the error type of the
/// swap operation it serves.
#[derive(Debug)]
pub enum TronSwapError {
    /// A node request failed, or its answer was not usable.
    Node(TronApiError),
    /// Call data, return data or a log entry does not fit the contract's ABI.
    Abi(AbiError),
    /// The call data does not start with the selector of a swap-contract method.
    WrongSelector,
    /// A secret hash that is not 32 bytes (CRD ch.21 R21.B6).
    HashWidth {
        /// The width given, in bytes.
        got: usize,
    },
    /// A contract call that did not succeed: the execution result of its receipt, or the revert reason of its
    /// dry run.
    Reverted {
        /// The result or reason, as the node names it.
        result: String,
    },
    /// The data asked for is not yet at the depth the step needs.
    NotYetFinal,
    /// A `fee_limit` that cannot be used or cannot be worked out: the energy cost is above the maximum, a figure
    /// is beyond the 64-bit range, or a chain price is not usable.
    FeeLimit(TronFeeError),
    /// The head block a node reported has a time too far from the local time to build a transaction on.
    HeadTimeOutOfTolerance {
        /// The head block's time, in milliseconds.
        head_time_ms: i64,
        /// The local time, in milliseconds.
        local_time_ms: i64,
    },
    /// A log entry whose first topic is not an event of the swap contract.
    UnknownEvent,
    /// A value of a transaction or of the contract's state differs from the one the swap expects.
    Mismatch {
        /// The name of the value.
        field: &'static str,
    },
    /// The chain time has not reached the lock time.
    TooEarly {
        /// The chain time, in seconds.
        chain_time: u64,
        /// The lock time, in seconds.
        lock_time: u64,
    },
    /// The contract reports a payment state outside 0 to 3.
    UnknownPaymentState(u64),
    /// Bytes that are not a signed TRON transaction of the form a swap exchanges.
    Transaction(String),
    /// The transaction is not one smart-contract call.
    NotAContractCall,
    /// A call value below zero.
    NegativeCallValue,
    /// The coin has no local private key (a hardware or an external wallet signs for it).
    NoLocalKey,
    /// The coin is not a TRX or TRC20 coin with a TRON node client.
    NotTronCoin,
    /// The transaction could not be signed.
    Signing(TronSignError),
    /// The signed transaction was built and its broadcast did not end with the node's acceptance. A node that did
    /// not answer may still have taken it, so the transaction is kept.
    Broadcast {
        /// The signed transaction.
        tx: Box<TronSwapTx>,
        /// Why the broadcast did not end with an acceptance.
        error: TronApiError,
    },
    /// A read that needs two nodes to agree (CRD ch.51 R100) is undetermined: the nodes disagree, fewer than two
    /// answered, or the trusted node did not. Carries the core's text, [`crate::swap_evidence::Undetermined::to_err_string`],
    /// so that the swap engine's [`crate::swap_evidence::is_undetermined`] recognizes it and waits (ch.51 R100
    /// item 9, ch.21 R21.11D.1), followed by the fix for the reason. The text names nodes as `host[:port]` only.
    Undetermined(String),
    /// A search for the transaction that consumed a payment reached its cap without finding it (CRD ch.21 D21.B8).
    SearchCapped {
        /// The source that was read up to its cap.
        source: &'static str,
    },
}

impl fmt::Display for TronSwapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TronSwapError::Node(e) => write!(f, "TRON node request failed: {e}"),
            TronSwapError::Abi(e) => write!(f, "TRON swap contract data does not fit its ABI: {e}"),
            TronSwapError::WrongSelector => {
                write!(f, "the call data is not a call of the expected swap contract method")
            },
            TronSwapError::HashWidth { got } => {
                write!(f, "a TRON swap needs a 32-byte secret hash, got {got} bytes")
            },
            TronSwapError::Reverted { result } => write!(f, "the contract call did not succeed: {result}"),
            TronSwapError::NotYetFinal => write!(f, "the data is not yet at the required depth"),
            TronSwapError::FeeLimit(reason) => write!(f, "unusable fee_limit: {reason}"),
            TronSwapError::HeadTimeOutOfTolerance {
                head_time_ms,
                local_time_ms,
            } => write!(
                f,
                "the head block's time {head_time_ms} ms is too far from the local time {local_time_ms} ms on every \
                 node that was asked: the local clock may be wrong"
            ),
            TronSwapError::UnknownEvent => write!(f, "the log entry is not an event of the swap contract"),
            TronSwapError::Mismatch { field } => write!(f, "{field} differs from the value the swap expects"),
            TronSwapError::TooEarly { chain_time, lock_time } => {
                write!(
                    f,
                    "the chain time {chain_time} has not reached the lock time {lock_time}"
                )
            },
            TronSwapError::UnknownPaymentState(state) => {
                write!(f, "the swap contract reports the unknown payment state {state}")
            },
            TronSwapError::Transaction(reason) => write!(f, "not a usable signed TRON transaction: {reason}"),
            TronSwapError::NotAContractCall => write!(f, "the transaction is not one smart-contract call"),
            TronSwapError::NegativeCallValue => write!(f, "the call value is below zero"),
            TronSwapError::NoLocalKey => write!(f, "the coin has no local private key to sign a TRON transaction with"),
            TronSwapError::NotTronCoin => write!(f, "the coin is not a TRON coin with a node client"),
            TronSwapError::Signing(e) => write!(f, "TRON signing failed: {e}"),
            TronSwapError::Broadcast { tx, error } => {
                write!(
                    f,
                    "the broadcast of the TRON transaction {:x} failed: {error}",
                    tx.txid()
                )
            },
            // The core text first and unchanged (so the engine's marker test passes), then the fix for the reason;
            // both are built in `watch::evidence`.
            TronSwapError::Undetermined(text) => f.write_str(text),
            TronSwapError::SearchCapped { source } => {
                write!(f, "the search through the {source} reached its cap without an answer")
            },
        }
    }
}

impl std::error::Error for TronSwapError {}

impl From<AbiError> for TronSwapError {
    fn from(e: AbiError) -> Self { TronSwapError::Abi(e) }
}
