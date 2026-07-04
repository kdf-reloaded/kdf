/******************************************************************************
 * Copyright © 2014-2018 The SuperNET Developers.                             *
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
//  coins.rs
//  marketmaker
//

#![allow(uncommon_codepoints)]
#![allow(dead_code)]
#![allow(unused_parens)]
#![allow(unused_imports)]
#![allow(mismatched_lifetime_syntaxes)]
// Suppress clippy warnings in this large, inherited codebase.
// These will be gradually addressed as the code is refactored.
#![allow(clippy::all)]

#[macro_use] extern crate common;
#[macro_use] extern crate fomat_macros;
#[macro_use] extern crate gstuff;
#[macro_use] extern crate mm2_metrics;
#[macro_use] extern crate lazy_static;
#[macro_use] extern crate serde_derive;
#[macro_use] extern crate serde_json;
#[macro_use] extern crate ser_error_derive;

pub(crate) use async_trait::async_trait;
pub(crate) use base58::FromBase58Error;
pub(crate) use bigdecimal::{BigDecimal, ParseBigDecimalError, Zero};
pub(crate) use common::executor::{spawn, Timer};
pub(crate) use common::mm_number::MmNumber;
pub(crate) use common::{calc_total_pages, now_ms, ten, HttpStatusCode};
pub(crate) use crypto::GlobalHDAccountArc;
pub(crate) use crypto::{Bip32Error, CryptoCtx, CryptoCtxError, DerivationPath, KeyPairPolicy};
pub(crate) use derive_more::Display;
pub(crate) use futures::compat::Future01CompatExt;
pub(crate) use futures::lock::Mutex as AsyncMutex;
pub(crate) use futures::{FutureExt, TryFutureExt};
pub(crate) use futures01::Future;
pub(crate) use http::{Response, StatusCode};
pub(crate) use keys::{AddressFormat as UtxoAddressFormat, KeyPair, NetworkPrefix as CashAddrPrefix};
pub(crate) use mm2_core::mm_ctx::{from_ctx, MmArc, MmWeak};
pub(crate) use mm2_err_handle::prelude::*;
pub(crate) use mm2_metrics::MetricsWeak;
pub(crate) use rpc::v1::types::{Bytes as BytesJson, H256 as H256Json};
pub(crate) use serde::{Deserialize, Deserializer, Serialize};
pub(crate) use serde_json::{self as json, Value as Json};
pub(crate) use std::collections::hash_map::HashMap;
pub(crate) use std::fmt;
pub(crate) use std::num::NonZeroUsize;
pub(crate) use std::ops::{Add, Deref};
pub(crate) use std::path::PathBuf;
pub(crate) use std::str::FromStr;
pub(crate) use std::sync::Arc;
pub(crate) use std::time::Duration;
pub(crate) use utxo_signer::with_key_pair::UtxoSignWithKeyPairError;

cfg_native! {
    pub(crate) use crate::lightning::LightningCoin;
    pub(crate) use crate::lightning::ln_conf::PlatformCoinConfirmations;
    pub(crate) use async_std::fs;
    pub(crate) use futures::AsyncWriteExt;
    pub(crate) use std::io;
    pub(crate) use zcash_primitives::transaction::Transaction as ZTransaction;
}

cfg_wasm32! {
    pub(crate) use mm2_db::indexed_db::{ConstructibleDb, DbLocked, SharedDb};
    pub(crate) use hd_wallet_storage::HDWalletDb;
    pub(crate) use tx_history_db::TxHistoryDb;
    pub(crate) use utxo::utxo_indexedb_block_header_storage::BlockHeaderStorageDb;

    pub type TxHistoryDbLocked<'a> = DbLocked<'a, TxHistoryDb>;
}

// using custom copy of try_fus as futures crate was renamed to futures01
macro_rules! try_fus {
    ($e: expr) => {
        match $e {
            Ok(ok) => ok,
            Err(err) => return Box::new(futures01::future::err(ERRL!("{}", err))),
        }
    };
}

macro_rules! try_f {
    ($e: expr) => {
        match $e {
            Ok(ok) => ok,
            Err(e) => return Box::new(futures01::future::err(e)),
        }
    };
}

/// `TransactionErr` compatible `try_fus` macro.
macro_rules! try_tx_fus {
    ($e: expr) => {
        match $e {
            Ok(ok) => ok,
            Err(err) => return Box::new(futures01::future::err(crate::TransactionErr::Plain(ERRL!("{:?}", err)))),
        }
    };
    ($e: expr, $tx: expr) => {
        match $e {
            Ok(ok) => ok,
            Err(err) => {
                return Box::new(futures01::future::err(crate::TransactionErr::TxRecoverable(
                    TransactionEnum::from($tx),
                    ERRL!("{:?}", err),
                )))
            },
        }
    };
}

/// `TransactionErr` compatible `try_s` macro.
macro_rules! try_tx_s {
    ($e: expr) => {
        match $e {
            Ok(ok) => ok,
            Err(err) => {
                return Err(crate::TransactionErr::Plain(format!(
                    "{}:{}] {:?}",
                    file!(),
                    line!(),
                    err
                )))
            },
        }
    };
    ($e: expr, $tx: expr) => {
        match $e {
            Ok(ok) => ok,
            Err(err) => {
                return Err(crate::TransactionErr::TxRecoverable(
                    TransactionEnum::from($tx),
                    format!("{}:{}] {:?}", file!(), line!(), err),
                ))
            },
        }
    };
}

/// `TransactionErr:Plain` compatible `ERR` macro.
macro_rules! TX_PLAIN_ERR {
    ($format: expr, $($args: tt)+) => { Err(crate::TransactionErr::Plain((ERRL!($format, $($args)+)))) };
    ($format: expr) => { Err(crate::TransactionErr::Plain(ERRL!($format))) }
}

/// `TransactionErr:TxRecoverable` compatible `ERR` macro.
#[allow(unused_macros)]
macro_rules! TX_RECOVERABLE_ERR {
    ($tx: expr, $format: expr, $($args: tt)+) => {
        Err(crate::TransactionErr::TxRecoverable(TransactionEnum::from($tx), ERRL!($format, $($args)+)))
    };
    ($tx: expr, $format: expr) => {
        Err(crate::TransactionErr::TxRecoverable(TransactionEnum::from($tx), ERRL!($format)))
    };
}

macro_rules! ok_or_continue_after_sleep {
    ($e:expr, $delay: ident) => {
        match $e {
            Ok(res) => res,
            Err(e) => {
                error!("error {:?}", e);
                Timer::sleep($delay).await;
                continue;
            },
        }
    };
}

#[cfg(not(target_arch = "wasm32"))]
macro_rules! ok_or_retry_after_sleep {
    ($e:expr, $delay: ident) => {
        loop {
            match $e {
                Ok(res) => break res,
                Err(e) => {
                    error!("error {:?}", e);
                    Timer::sleep($delay).await;
                    continue;
                },
            }
        }
    };
}

#[cfg(not(target_arch = "wasm32"))]
macro_rules! ok_or_retry_after_sleep_sync {
    ($e:expr, $delay: ident) => {
        loop {
            match $e {
                Ok(res) => break res,
                Err(e) => {
                    error!("error {:?}", e);
                    std::thread::sleep(core::time::Duration::from_secs($delay));
                    continue;
                },
            }
        }
    };
}

pub mod coin_balance;
#[doc(hidden)]
#[cfg(test)]
pub mod coins_tests;
pub mod eth;
pub mod hd_pubkey;
pub mod hd_wallet;
pub mod hd_wallet_storage;
#[cfg(not(target_arch = "wasm32"))] pub mod lightning;
#[cfg_attr(target_arch = "wasm32", allow(dead_code, unused_imports))]
pub mod my_tx_history_v2;
pub mod nft;
pub mod qrc20;
pub mod rpc_command;
#[doc(hidden)]
#[cfg(test)]
mod rpc_response_tests;
#[cfg(not(target_arch = "wasm32"))]
pub mod sql_tx_history_storage;
#[doc(hidden)]
#[allow(unused_variables)]
pub mod test_coin;
pub use test_coin::TestCoin;

#[doc(hidden)]
#[allow(unused_variables)]
#[cfg(not(target_arch = "wasm32"))]
pub mod solana;
#[cfg(not(target_arch = "wasm32"))]
pub use solana::spl::SplToken;
#[cfg(not(target_arch = "wasm32"))]
pub use solana::{solana_coin_from_conf_and_params, SolanaActivationParams, SolanaCoin, SolanaFeeDetails};

pub mod siacoin;
pub mod tendermint;
#[cfg(target_arch = "wasm32")] pub mod tx_history_db;
pub mod tx_history_streaming;
pub mod utxo;
#[cfg(not(target_arch = "wasm32"))] pub mod z_coin;

pub(crate) use eth::{eth_coin_from_conf_and_request, EthCoin, EthTxFeeDetails, SignedEthTx};
pub(crate) use hd_wallet::{HDAddress, HDAddressId};
pub(crate) use qrc20::Qrc20ActivationParams;
pub(crate) use qrc20::{qrc20_coin_from_conf_and_params, Qrc20Coin, Qrc20FeeDetails};
pub(crate) use qtum::{Qrc20AddressError, ScriptHashTypeNotSupported};
pub(crate) use rpc_command::init_account_balance::{AccountBalanceTaskManager, AccountBalanceTaskManagerShared};
pub(crate) use rpc_command::init_create_account::{CreateAccountTaskManager, CreateAccountTaskManagerShared};
pub(crate) use rpc_command::init_scan_for_new_addresses::{ScanAddressesTaskManager, ScanAddressesTaskManagerShared};
pub(crate) use rpc_command::init_withdraw::{WithdrawTaskManager, WithdrawTaskManagerShared};
pub(crate) use utxo::bch::{bch_coin_from_conf_and_params, BchActivationRequest, BchCoin};
pub(crate) use utxo::qtum::{self, qtum_coin_with_priv_key, QtumCoin};
pub(crate) use utxo::qtum::{QtumDelegationOps, QtumDelegationRequest, QtumStakingInfosDetails};
pub(crate) use utxo::rpc_clients::UtxoRpcError;
pub(crate) use utxo::slp::SlpToken;
pub(crate) use utxo::slp::{slp_addr_from_pubkey_str, SlpFeeDetails};
pub(crate) use utxo::utxo_common::big_decimal_from_sat_unsigned;
pub(crate) use utxo::utxo_standard::{utxo_standard_coin_with_priv_key, UtxoStandardCoin};
pub(crate) use utxo::UtxoActivationParams;
pub(crate) use utxo::{BlockchainNetwork, GenerateTxError, UtxoFeeDetails, UtxoTx};
#[cfg(not(target_arch = "wasm32"))] pub(crate) use z_coin::ZCoin;

// ---- Split sub-modules (extracted from monolithic lp_coins.rs) ----
mod lp_coins_context;
mod lp_coins_errors;
mod lp_coins_ops;
mod lp_coins_traits;
mod lp_coins_types;

pub use lp_coins_context::*;
pub use lp_coins_errors::*;
pub use lp_coins_ops::*;
pub use lp_coins_traits::*;
pub use lp_coins_types::*;
