// Lightning module — split into sub-modules for maintainability.
//
// Note: this entire module is gated by #[cfg(not(target_arch = "wasm32"))] at the
// declaration site in the parent (lp_coins.rs), so individual submodules do NOT
// need that guard.
//
// Split sub-modules:
//   lightning_types      – LightningCoin struct, LightningParams, start_lightning activation
//   lightning_helpers    – internal inherent methods on LightningCoin
//   lightning_swap_ops   – SwapOps + WatcherOps trait impls
//   lightning_market_ops – MarketCoinOps trait impl
//   lightning_mm_coin    – MmCoin trait impl
//   lightning_rpcs       – RPC handler functions and request/response types
//

// ─── Existing sub-modules ───────────────────────────────────────────────────

pub mod ln_conf;
pub mod ln_errors;
mod ln_events;
mod ln_p2p;
mod ln_platform;
mod ln_serialization;
mod ln_utils;

// ─── Imports (pub(crate) so child modules inherit via `use super::*`) ───────

pub(crate) use super::{lp_coinfind_or_err, DerivationMethod, MmCoinEnum};
pub(crate) use crate::utxo::rpc_clients::UtxoRpcClientEnum;
pub(crate) use crate::utxo::utxo_common::{big_decimal_from_sat_unsigned, UtxoTxBuilder};
pub(crate) use crate::utxo::{sat_from_big_decimal, BlockchainNetwork, FeePolicy, GetUtxoListOps, UtxoTxGenerationOps};
pub(crate) use crate::{BalanceFut, CoinBalance, DexFee, FeeApproxStage, FoundSwapTxSpend, HistorySyncState,
                       MarketCoinOps, MmCoin, NegotiateSwapContractAddrErr, RawTransactionFut, RawTransactionRequest,
                       SignatureError, SignatureResult, SwapOps, TradeFee, TradePreimageFut, TradePreimageResult,
                       TradePreimageValue, TransactionEnum, TransactionFut, UnexpectedDerivationMethod,
                       UtxoStandardCoin, ValidateAddressResult, ValidateFeeArgs, ValidatePaymentInput,
                       VerificationError, VerificationResult, WatcherOps, WithdrawError, WithdrawFut, WithdrawRequest};
pub(crate) use async_trait::async_trait;
pub(crate) use bigdecimal::BigDecimal;
pub(crate) use bitcoin::hashes::Hash;
pub(crate) use bitcoin_hashes::sha256::Hash as Sha256;
pub(crate) use chain::TransactionOutput;
pub(crate) use common::executor::spawn;
pub(crate) use common::log::{LogOnError, LogState};
pub(crate) use common::mm_number::MmNumber;
pub(crate) use common::{async_blocking, calc_total_pages, log, now_ms, ten, PagingOptionsEnum};
pub(crate) use futures::{FutureExt, TryFutureExt};
pub(crate) use futures01::Future;
pub(crate) use kdf_crypto::dhash256;
pub(crate) use kdf_crypto::ChecksumType;
pub(crate) use keys::{hash::H256, AddressHashEnum, CompactSignature, KeyPair, Private, Public};
pub(crate) use lightning::chain::channelmonitor::Balance;
pub(crate) use lightning::chain::keysinterface::{KeysInterface, KeysManager, Recipient};
pub(crate) use lightning::chain::Access;
pub(crate) use lightning::ln::channelmanager::{ChannelDetails, MIN_FINAL_CLTV_EXPIRY};
pub(crate) use lightning::ln::{PaymentHash, PaymentPreimage};
pub(crate) use lightning::routing::network_graph::{NetGraphMsgHandler, NetworkGraph};
pub(crate) use lightning::util::config::{ChannelConfig, UserConfig};
pub(crate) use lightning_background_processor::BackgroundProcessor;
pub(crate) use lightning_invoice::payment;
pub(crate) use lightning_invoice::utils::{create_invoice_from_channelmanager, DefaultRouter};
pub(crate) use lightning_invoice::{Invoice, InvoiceDescription};
pub(crate) use lightning_persister::storage::{ClosedChannelsFilter, DbStorage, FileSystemStorage, HTLCStatus,
                                              NodesAddressesMapShared, PaymentInfo, PaymentType, PaymentsFilter,
                                              Scorer, SqlChannelDetails, TrustedNodesShared};
pub(crate) use lightning_persister::LightningPersister;
pub(crate) use ln_conf::{ChannelOptions, LightningCoinConf, LightningProtocolConf, PlatformCoinConfirmations};
pub(crate) use ln_errors::{ClaimableBalancesError, ClaimableBalancesResult, CloseChannelError, CloseChannelResult,
                           ConnectToNodeError, ConnectToNodeResult, EnableLightningError, EnableLightningResult,
                           GenerateInvoiceError, GenerateInvoiceResult, GetChannelDetailsError,
                           GetChannelDetailsResult, GetPaymentDetailsError, GetPaymentDetailsResult,
                           ListChannelsError, ListChannelsResult, ListPaymentsError, ListPaymentsResult,
                           OpenChannelError, OpenChannelResult, SendPaymentError, SendPaymentResult, TrustedNodeError,
                           TrustedNodeResult, UpdateChannelError, UpdateChannelResult};
pub(crate) use ln_events::LightningEventHandler;
pub(crate) use ln_p2p::{connect_to_node, ConnectToNodeRes, PeerManager};
pub(crate) use ln_platform::{h256_json_from_txid, Platform};
pub(crate) use ln_serialization::{InvoiceForRPC, NodeAddress, PublicKeyForRPC};
pub(crate) use ln_utils::{ChainMonitor, ChannelManager};
pub(crate) use mm2_core::mm_ctx::MmArc;
pub(crate) use mm2_err_handle::prelude::*;
pub(crate) use mm2_net::ip_addr::myipaddr;
pub(crate) use parking_lot::Mutex as PaMutex;
pub(crate) use rpc::v1::types::{Bytes as BytesJson, H256 as H256Json};
pub(crate) use script::{Builder, TransactionInputSigner};
pub(crate) use secp256k1::PublicKey;
pub(crate) use serde::{Deserialize, Serialize};
pub(crate) use serde_json::Value as Json;
pub(crate) use std::collections::hash_map::Entry;
pub(crate) use std::collections::{HashMap, HashSet};
pub(crate) use std::fmt;
pub(crate) use std::net::SocketAddr;
pub(crate) use std::str::FromStr;
pub(crate) use std::sync::{Arc, Mutex};

// ─── Split sub-modules ─────────────────────────────────────────────────────

mod lightning_helpers;
mod lightning_market_ops;
mod lightning_mm_coin;
mod lightning_rpcs;
mod lightning_swap_ops;
mod lightning_types;

// Re-export split module contents for backward-compatible access paths
pub use lightning_rpcs::*;
pub use lightning_types::*;
