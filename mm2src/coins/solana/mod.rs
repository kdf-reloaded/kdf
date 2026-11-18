//! # Purpose
//! Solana platform-coin and SPL-token support.
//!
//! # Sub-modules
//! - `rpc_client` — KDF-original async JSON-RPC client (P14).
//! - `rpc_pool` — failover-aware multi-endpoint dispatch layer (P14.11).
//! - `solana_types` — constants, traits, errors, `SolanaCoin` struct.
//! - `solana_common` — shared transfer/balance helpers.
//! - `solana_helpers` — inherent methods on `SolanaCoin`.
//! - `solana_swap_ops` — `SwapOps` and `WatcherOps` impls.
//! - `solana_market_ops` — `MarketCoinOps` impl.
//! - `solana_mm_coin` — `MmCoin` impl plus the SOL-side `withdraw`.
//! - `spl` — `SplToken` and its `MmCoin` / `MarketCoinOps` impls.
//! - `solana_decode_tx_helpers` — Solana JSON tx-history decoders.

pub mod rpc_client;
pub mod rpc_pool;

pub(crate) use super::{CoinBalance, HistorySyncState, MarketCoinOps, MmCoin, SwapOps, TradeFee, TransactionEnum,
                       WatcherOps};
pub(crate) use crate::solana::rpc_client::{RpcError, RpcErrorKind, SolanaRpcClient, TokenAccountsFilter};
pub(crate) use crate::solana::rpc_pool::SolanaRpcPool;
pub(crate) use crate::solana::solana_common::{lamports_to_sol, PrepareTransferData, SufficientBalanceError};
pub(crate) use crate::solana::spl::SplTokenInfo;
pub(crate) use crate::{BalanceError, BalanceFut, DexFee, FeeApproxStage, FoundSwapTxSpend,
                       NegotiateSwapContractAddrErr, RawTransactionError, RawTransactionFut, RawTransactionRequest,
                       SignatureResult, TradePreimageError, TradePreimageFut, TradePreimageResult, TradePreimageValue,
                       TransactionDetails, TransactionErr, TransactionFut, TransactionType,
                       UnexpectedDerivationMethod, ValidateAddressResult, ValidateFeeArgs, ValidatePaymentInput,
                       VerificationResult, WithdrawError, WithdrawFut, WithdrawRequest, WithdrawResult};
pub(crate) use async_trait::async_trait;
pub(crate) use base58::ToBase58;
pub(crate) use bigdecimal::BigDecimal;
pub(crate) use bincode::{deserialize, serialize};
pub(crate) use common::{async_blocking, mm_number::MmNumber, now_ms};
pub(crate) use derive_more::Display;
pub(crate) use futures::{FutureExt, TryFutureExt};
pub(crate) use futures01::Future;
pub(crate) use keys::KeyPair;
pub(crate) use mm2_core::mm_ctx::MmArc;
pub(crate) use mm2_err_handle::prelude::*;
pub(crate) use rpc::v1::types::Bytes as BytesJson;
pub(crate) use serde_json::{self as json, Value as Json};
pub(crate) use solana_commitment_config::{CommitmentConfig, CommitmentLevel};
pub(crate) use solana_keypair::{keypair_from_seed, Keypair};
pub(crate) use solana_program_error::ProgramError;
pub(crate) use solana_pubkey::{ParsePubkeyError, Pubkey};
pub(crate) use solana_signer::Signer;
pub(crate) use solana_transaction::Transaction;
pub(crate) use std::collections::HashMap;
pub(crate) use std::str::FromStr;
pub(crate) use std::sync::Mutex;
pub(crate) use std::{convert::TryFrom,
                     fmt::{Debug, Formatter, Result as FmtResult},
                     ops::Deref,
                     sync::Arc};

pub mod solana_common;
#[cfg(test)] mod solana_common_tests;
mod solana_decode_tx_helpers;
#[cfg(test)] mod solana_disable_coin_tests;
#[cfg(test)] mod solana_tests;
#[cfg(test)] mod solana_wallet_answers_tests;
pub mod spl;
#[cfg(test)] mod spl_tests;

mod solana_helpers;
mod solana_market_ops;
mod solana_mm_coin;
mod solana_swap_ops;
mod solana_types;

pub use solana_types::*;
