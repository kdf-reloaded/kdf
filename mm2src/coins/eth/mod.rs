/******************************************************************************
 * Copyright © 2014-2019 The SuperNET Developers.                             *
 *                                                                            *
 * See the AUTHORS, DEVELOPER-AGREEMENT and LICENSE files at                  *
 * the top-level directory of this distribution for the individual copyright  *
 * holder information and the developer policies on copyright and licensing.  *
 *                                                                            *
 * Unless otherwise agreed in a custom licensing agreement, no part of the    *
 * SuperNET software, including this file may be copied, modified, propagated *
 * or distributed except according to the terms contained in the LICENSE file *
 *                                                                            *
 * Removal or modification of this copyright notice is prohibited.            *
 *                                                                            *
 ******************************************************************************/
//
//  eth — Ethereum and ERC-20 coin support
//
//  Split into sub-modules for maintainability:
//    eth_types      – constants, ABIs, structs, enums, error conversions
//    eth_impl       – EthCoinImpl methods, transaction helpers, utility fns
//    eth_swap_ops   – SwapOps + WatcherOps trait impls
//    eth_market_ops – MarketCoinOps trait impl
//    eth_mm_coin    – MmCoin, ParseCoinAssocTypes, V2 swap trait impls
//

// ─── Imports (pub(crate) so child modules inherit via `use super::*`) ───────

pub(crate) use async_trait::async_trait;
pub(crate) use bigdecimal::BigDecimal;
pub(crate) use common::custom_futures::TimedAsyncMutex;
pub(crate) use common::executor::Timer;
pub(crate) use common::log::error;
pub(crate) use common::{now_ms, small_rng};
pub(crate) use derive_more::Display;
pub(crate) use ethereum_types::{Address, H160, H256, U256};
pub(crate) use futures::compat::Future01CompatExt;
pub(crate) use futures::future::{join_all, select, Either, FutureExt, TryFutureExt};
pub(crate) use futures01::Future;
pub(crate) use http::StatusCode;
pub(crate) use kdf_crypto::{keccak256, sha256};
pub(crate) use legacy_tx::{Action, Transaction as UnSignedEthTx, UnverifiedTransaction};
pub(crate) use mm2_core::mm_ctx::{MmArc, MmWeak};
pub(crate) use mm2_err_handle::prelude::*;
pub(crate) use mm2_eth::keys::{public_to_address, KeyPair, Public, Signature};
pub(crate) use mm2_net::transport::{slurp_url, SlurpError};
#[cfg(test)] pub(crate) use mocktopus::macros::*;
pub(crate) use rand::seq::SliceRandom;
pub(crate) use rpc::v1::types::Bytes as BytesJson;
pub(crate) use secp256k1::PublicKey;
pub(crate) use serde_json::{self as json, Value as Json};
pub(crate) use sha3::{Digest, Keccak256};
pub(crate) use std::cmp::Ordering;
pub(crate) use std::collections::HashMap;
pub(crate) use std::fmt;
pub(crate) use std::ops::Deref;
pub(crate) use std::path::PathBuf;
pub(crate) use std::str::FromStr;
pub(crate) use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
pub(crate) use std::sync::{Arc, Mutex};
pub(crate) use wire_types::{Action as TraceAction, BlockNumber, Bytes, CallRequest, FilterBuilder, Log, Trace,
                            TraceFilterBuilder};

pub(crate) use super::{BalanceError, BalanceFut, CoinBalance, CoinProtocol, CoinTransportMetrics, CoinsContext,
                       FeeApproxStage, FoundSwapTxSpend, HistorySyncState, MarketCoinOps, MmCoin,
                       NegotiateSwapContractAddrErr, NumConversError, NumConversResult, RawTransactionError,
                       RawTransactionFut, RawTransactionRequest, RawTransactionRes, RawTransactionResult,
                       RpcClientType, RpcTransportEventHandler, RpcTransportEventHandlerShared,
                       SignEthTransactionParams, SignRawTransactionEnum, SignRawTransactionRequest, SignatureError,
                       SignatureResult, SwapOps, TradeFee, TradePreimageError, TradePreimageFut, TradePreimageResult,
                       TradePreimageValue, Transaction, TransactionDetails, TransactionEnum,
                       UnexpectedDerivationMethod, ValidateAddressResult, VerificationError, VerificationResult,
                       WithdrawError, WithdrawFee, WithdrawFut, WithdrawRequest, WithdrawResult};

pub use legacy_tx::rlp;
pub use legacy_tx::SignedTransaction as SignedEthTx;

// ─── Sub-modules ─────────────────────────────────────────────────────────

pub(crate) mod alloy_compat;
pub mod eth_hd_wallet;
pub(crate) mod eth_swap_v2;
pub mod fee_estimation;
pub mod legacy_tx;
pub mod tron;

/// Public re-export of the NFT swap V2 surface so mm2_main's swap
/// state-machine driver can construct calls and inspect errors
/// (P10.3.7.d).
pub mod nft_swap_v2 {
    pub use crate::eth::eth_swap_v2::nft_swap_v2::*;
}

mod eth_impl;
mod eth_market_ops;
mod eth_mm_coin;
mod eth_swap_ops;
// EVM Trezor hardware-wallet activation (device-sourced address/pubkey). Native,
// non-iOS only — the Trezor signing policy exists only there (CRD §50).
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
mod eth_trezor_activation;
// EVM Trezor hardware-wallet withdrawal (device-driven signing). Native, non-iOS
// only — the Trezor signing policy exists only there (CRD §50).
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
mod eth_trezor_withdraw;
mod eth_types;
pub mod wc_integration;
mod wire_types;
// Re-export split module contents for backward-compatible access paths
pub use eth_impl::*;
pub use eth_mm_coin::EthTxFeeDetails;
pub use eth_types::*;
// EVM Trezor activation entrypoints (native, non-iOS).
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
pub use eth_trezor_activation::{eth_coin_activate_with_trezor, eth_coin_from_conf_and_request_with_trezor};

pub(crate) use crate::DerivationMethod;
pub(crate) use crate::{CommonSwapOpsV2, DexFee, FindPaymentSpendError, FundingTxSpend, GenPreimageResult,
                       GenTakerFundingSpendArgs, GenTakerPaymentSpendArgs, MakerCoinSwapOpsV2, ParseCoinAssocTypes,
                       RefundFundingSecretArgs, RefundMakerPaymentSecretArgs, RefundMakerPaymentTimelockArgs,
                       RefundTakerPaymentArgs, SearchForFundingSpendErr, SendMakerPaymentArgs, SendTakerFundingArgs,
                       SpendMakerPaymentArgs, SwapTxTypeWithSecretHash, TakerCoinSwapOpsV2, ToBytes, TransactionErr,
                       TransactionFut, TxGenError, TxPreimageWithSig, ValidateFeeArgs, ValidateMakerPaymentArgs,
                       ValidatePaymentInput, ValidateSwapV2TxError, ValidateSwapV2TxResult, ValidateTakerFundingArgs,
                       ValidateTakerFundingSpendPreimageResult, ValidateTakerPaymentSpendPreimageResult, WatcherOps};
pub(crate) use common::mm_number::MmNumber;
pub(crate) use eth_hd_wallet::EthHDWallet;
pub(crate) use mm2_eth::keys::{sign, verify_address};
pub(crate) use serialization::{CompactInteger, Serializable, Stream};

// Alloy-backed ABI facade — the crate-wide `Contract`/`Token`/`Function`/
// `AbiError` come from here (replacing ethabi).
pub(crate) mod abi;
pub(crate) use abi::{AbiError, Contract, Function, Token};

#[cfg(test)] mod abi_golden_tests;
#[cfg(test)] mod eth_tests;
// Local `geth --dev` integration tests for the v1 ETH/ERC20 HTLC payment +
// refund path. Native only; they skip themselves when `geth` is not on PATH.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod eth_swap_dev_tests;
// Emulator-gated EVM Trezor signing integration tests (CRD §50.8). Native,
// non-iOS, and only when the `trezor-emulator-tests` feature is on; they drive a
// real `task::withdraw` against a running Trezor emulator.
#[cfg(all(
    test,
    not(target_arch = "wasm32"),
    not(target_os = "ios"),
    feature = "trezor-emulator-tests"
))]
mod eth_trezor_emulator_tests;
#[cfg(target_arch = "wasm32")] mod eth_wasm_tests;
// ─── EthCoin newtype ────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct EthCoin(Arc<EthCoinImpl>);
impl Deref for EthCoin {
    type Target = EthCoinImpl;
    fn deref(&self) -> &EthCoinImpl { &*self.0 }
}
