// utxo_common — Hub module for common UTXO operations.
//
// Split into sub-modules by concern:
// - swap:    HTLC/swap operations, payment scripts, validation
// - tx:      transaction building, signing, fee estimation, UTXO management
// - hd:      HD wallet derivation, scanning, account management
// - history: transaction history processing, KMD rewards
// - spv:     SPV proof validation, block header management
// - helpers: address utilities, balance queries, config, withdraw

// Macro must be defined before sub-module declarations so they can use it.
macro_rules! true_or {
    ($cond: expr, $etype: expr) => {
        if !$cond {
            return Err(MmError::new($etype));
        }
    };
}

mod utxo_common_hd;
mod utxo_common_helpers;
mod utxo_common_history;
mod utxo_common_spv;
mod utxo_common_swap;
mod utxo_common_tx;

pub use utxo_common_hd::*;
pub use utxo_common_helpers::*;
pub use utxo_common_history::*;
pub use utxo_common_spv::*;
pub use utxo_common_swap::*;
// `UtxoMergeParams` (defined in `utxo_common_tx`) is also reachable through the
// broad `pub(crate) use super::*;` shared-import re-export below, so rustc flags
// an ambiguous glob re-export. Both paths resolve to the single definition, so
// the ambiguity is harmless; allow it rather than enumerate the ~80-name shared
// re-export explicitly.
#[allow(ambiguous_glob_reexports)] pub use utxo_common_tx::*;

// ---------- shared imports for sub-modules (accessible via `use super::*;`) ----------

pub(crate) use super::rpc_clients::TxMerkleBranch;
pub(crate) use super::*;

pub(crate) use crate::coin_balance::{AddressBalanceStatus, HDAddressBalance, HDWalletBalanceOps};
pub(crate) use crate::hd_pubkey::{ExtractExtendedPubkey, HDExtractPubkeyError, HDXPubExtractor};
pub(crate) use crate::hd_wallet::{AccountUpdatingError, AddressDerivingError, HDAccountMut, HDAccountsMap,
                                  NewAccountCreatingError};
pub(crate) use crate::hd_wallet_storage::{HDWalletCoinWithStorageOps, HDWalletStorageResult};
pub(crate) use crate::rpc_command::init_withdraw::WithdrawTaskHandle;
pub(crate) use crate::utxo::rpc_clients::{electrum_script_hash, BlockHashOrHeight, UnspentInfo, UnspentMap,
                                          UtxoRpcClientEnum, UtxoRpcClientOps, UtxoRpcResult};
pub(crate) use crate::utxo::tx_cache::TxCacheResult;
pub(crate) use crate::utxo::utxo_withdraw::{InitUtxoWithdraw, StandardUtxoWithdraw, UtxoWithdraw};
pub(crate) use crate::{CanRefundHtlc, CoinBalance, CoinWithDerivationMethod, DexFee, DexFeeBurnDestination,
                       GetWithdrawSenderAddress, HDAddressId, RawTransactionError, RawTransactionRequest,
                       RawTransactionRes, RawTransactionResult, SignatureError, SignatureResult, TradePreimageValue,
                       TransactionFut, TxFeeDetails, ValidateAddressResult, ValidateFeeArgs, ValidatePaymentInput,
                       VerificationError, VerificationResult, WithdrawFrom, WithdrawResult, WithdrawSenderAddress};

pub(crate) use bigdecimal::BigDecimal;
pub(crate) use chain::constants::SEQUENCE_FINAL;
pub(crate) use chain::{BlockHeader, OutPoint, RawBlockHeader, TransactionOutput};
pub(crate) use common::executor::Timer;
pub(crate) use common::jsonrpc_client::JsonRpcErrorType;
pub(crate) use common::log::{debug, error, info, warn};
pub(crate) use common::mm_number::MmNumber;
pub(crate) use common::{now_ms, one_hundred, ten_f64};
pub(crate) use crypto::privkey::key_pair_from_secret;
pub(crate) use crypto::{Bip32DerPathOps, Bip44Chain, Bip44DerPathError, Bip44DerivationPath, RpcDerivationPath};
pub(crate) use futures::compat::Future01CompatExt;
pub(crate) use futures::future::{FutureExt, TryFutureExt};
pub(crate) use futures01::future::Either;
pub(crate) use itertools::Itertools;
pub(crate) use kdf_crypto::dhash256;
pub use kdf_crypto::{dhash160, sha256, ChecksumType};
pub(crate) use keys::bytes::Bytes;
pub(crate) use keys::{Address, AddressFormat as UtxoAddressFormat, AddressHashEnum, CompactSignature, Public,
                      SegwitAddress, Type as ScriptType};
pub(crate) use mm2_core::mm_ctx::MmArc;
pub(crate) use mm2_err_handle::prelude::*;
pub(crate) use mm2_metrics::MetricsArc;
pub(crate) use primitives::hash::H512;
pub(crate) use rpc::v1::types::{Bytes as BytesJson, ToTxHash, TransactionInputEnum, H256 as H256Json};
pub(crate) use script::{Builder, Opcode, Script, ScriptAddress, TransactionInputSigner, UnsignedTransactionInput};
pub(crate) use secp256k1::{PublicKey, Signature};
pub(crate) use serde_json::{self as json};
pub(crate) use serialization::{deserialize, serialize, serialize_list, serialize_with_flags, CoinVariant,
                               CompactInteger, Serializable, Stream, SERIALIZE_TRANSACTION_WITNESS};
pub(crate) use spv_validation::helpers_validation::validate_headers;
pub(crate) use spv_validation::helpers_validation::SPVError;
pub(crate) use spv_validation::spv_proof::{SPVProof, TRY_SPV_PROOF_INTERVAL};
pub(crate) use std::cmp::Ordering;
pub(crate) use std::collections::hash_map::{Entry, HashMap};
pub(crate) use std::str::FromStr;
pub(crate) use std::sync::atomic::Ordering as AtomicOrdering;
pub(crate) use utxo_block_header_storage::BlockHeaderStorageOps;
pub(crate) use utxo_signer::with_key_pair::p2sh_spend;
pub(crate) use utxo_signer::UtxoSignerOps;

pub use chain::Transaction as UtxoTx;
