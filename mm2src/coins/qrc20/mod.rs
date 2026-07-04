// qrc20 module — QRC20 token support on Qtum-based chains.
//
// Split into sub-modules for maintainability:
//   qrc20_types      – constants, structs, enums, error types, utility functions
//   qrc20_helpers    – constructor, builder, internal methods, UTXO trait impls
//   qrc20_swap_ops   – SwapOps and WatcherOps trait implementations
//   qrc20_market_ops – MarketCoinOps trait implementation
//   qrc20_mm_coin    – MmCoin trait implementation and withdraw logic
//

// ─── Imports (pub(crate) so child modules inherit via `use super::*`) ───────

pub(crate) use crate::eth::abi::{Function, Token};
pub(crate) use crate::eth::{self, u256_to_big_decimal, wei_from_big_decimal, TryToAddress};
pub(crate) use crate::qrc20::rpc_clients::{LogEntry, Qrc20ElectrumOps, Qrc20NativeOps, Qrc20RpcOps, TopicFilter,
                                           TxReceipt, ViewContractCallType};
pub(crate) use crate::utxo::qtum::QtumBasedCoin;
pub(crate) use crate::utxo::rpc_clients::{ElectrumClient, NativeClient, UnspentInfo, UtxoRpcClientEnum,
                                          UtxoRpcClientOps, UtxoRpcError, UtxoRpcFut, UtxoRpcResult};
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use crate::utxo::tx_cache::{UtxoVerboseCacheOps, UtxoVerboseCacheShared};
pub(crate) use crate::utxo::utxo_builder::{UtxoCoinBuildError, UtxoCoinBuildResult, UtxoCoinBuilderCommonOps,
                                           UtxoCoinWithIguanaPrivKeyBuilder, UtxoFieldsWithIguanaPrivKeyBuilder};
pub(crate) use crate::utxo::utxo_common::{self, big_decimal_from_sat, check_all_inputs_signed_by_pub, UtxoTxBuilder};
pub(crate) use crate::utxo::{qtum, ActualTxFee, AdditionalTxData, BroadcastTxErr, FeePolicy, GenerateTxError,
                             GetUtxoListOps, HistoryUtxoTx, HistoryUtxoTxMap, MatureUnspentList,
                             RecentlySpentOutPointsGuard, UtxoActivationParams, UtxoAddressFormat, UtxoCoinFields,
                             UtxoCommonOps, UtxoFromLegacyReqErr, UtxoTx, UtxoTxBroadcastOps, UtxoTxGenerationOps,
                             VerboseTransactionFrom, UTXO_LOCK};
pub(crate) use crate::{BalanceError, BalanceFut, CoinBalance, DexFee, FeeApproxStage, FoundSwapTxSpend,
                       HistorySyncState, MarketCoinOps, MmCoin, NegotiateSwapContractAddrErr, PrivKeyNotAllowed,
                       RawTransactionFut, RawTransactionRequest, SignRawTransactionRequest, SignatureResult, SwapOps,
                       TradeFee, TradePreimageError, TradePreimageFut, TradePreimageResult, TradePreimageValue,
                       TransactionDetails, TransactionEnum, TransactionErr, TransactionFut, TransactionType,
                       UnexpectedDerivationMethod, ValidateAddressResult, ValidateFeeArgs, ValidatePaymentInput,
                       VerificationResult, WatcherOps, WithdrawError, WithdrawFee, WithdrawFut, WithdrawRequest,
                       WithdrawResult};
pub(crate) use async_trait::async_trait;
pub(crate) use bigdecimal::BigDecimal;
pub(crate) use chain::TransactionOutput;
pub(crate) use common::executor::Timer;
pub(crate) use common::jsonrpc_client::{JsonRpcClient, JsonRpcRequest, RpcRes};
pub(crate) use common::log::{error, warn};
pub(crate) use common::mm_number::MmNumber;
pub(crate) use common::now_ms;
pub(crate) use derive_more::Display;
pub(crate) use ethereum_types::{H160, U256};
pub(crate) use futures::compat::Future01CompatExt;
pub(crate) use futures::{FutureExt, TryFutureExt};
pub(crate) use futures01::Future;
pub(crate) use kdf_crypto::{dhash160, sha256};
pub(crate) use keys::bytes::Bytes as ScriptBytes;
pub(crate) use keys::{Address as UtxoAddress, Address, KeyPair, Public};
pub(crate) use mm2_core::mm_ctx::MmArc;
pub(crate) use mm2_err_handle::prelude::*;
#[cfg(test)] pub(crate) use mocktopus::macros::*;
pub(crate) use rpc::v1::types::{Bytes as BytesJson, ToTxHash, Transaction as RpcTransaction, H160 as H160Json,
                                H256 as H256Json};
pub(crate) use script::{Builder as ScriptBuilder, Opcode, Script, TransactionInputSigner};
pub(crate) use script_pubkey::generate_contract_call_script_pubkey;
pub(crate) use serde_json::{self as json, Value as Json};
pub(crate) use serialization::{deserialize, serialize, CoinVariant};
pub(crate) use std::collections::{HashMap, HashSet};
pub(crate) use std::ops::{Deref, Neg};
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use std::path::PathBuf;
pub(crate) use std::str::FromStr;
pub(crate) use std::sync::Arc;
pub(crate) use utxo_signer::with_key_pair::{sign_tx, UtxoSignWithKeyPairError};

// ─── Existing sub-modules ───────────────────────────────────────────────────

mod history;
#[cfg(test)] mod qrc20_tests;
pub mod rpc_clients;
pub mod script_pubkey;
mod swap;

// ─── Split sub-modules ─────────────────────────────────────────────────────

mod qrc20_helpers;
mod qrc20_market_ops;
mod qrc20_mm_coin;
mod qrc20_swap_ops;
mod qrc20_types;

// Re-export split module contents for backward-compatible access paths
pub use qrc20_helpers::qrc20_coin_from_conf_and_params;
pub use qrc20_types::*;
