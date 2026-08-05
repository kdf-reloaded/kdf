#[cfg(not(target_arch = "wasm32"))]
use crate::rpc_command::init_withdraw::{InitWithdrawCoin, WithdrawInProgressStatus, WithdrawTaskHandle};
use crate::utxo::rpc_clients::{UnspentInfo, UtxoRpcClientEnum, UtxoRpcClientOps, UtxoRpcError, UtxoRpcFut,
                               UtxoRpcResult};
use crate::utxo::utxo_builder::{UtxoCoinBuilderCommonOps, UtxoCoinWithIguanaPrivKeyBuilder,
                                UtxoFieldsWithIguanaPrivKeyBuilder};
use crate::utxo::utxo_common::{big_decimal_from_sat_unsigned, payment_script};
#[cfg(not(target_arch = "wasm32"))]
use crate::utxo::zcash_params_path;
use crate::utxo::{sat_from_big_decimal, utxo_common, ActualTxFee, AdditionalTxData, Address, BroadcastTxErr,
                  FeePolicy, GetUtxoListOps, HistoryUtxoTx, HistoryUtxoTxMap, MatureUnspentList,
                  RecentlySpentOutPointsGuard, UtxoActivationParams, UtxoAddressFormat, UtxoArc, UtxoCoinFields,
                  UtxoCommonOps, UtxoFeeDetails, UtxoTxBroadcastOps, UtxoTxGenerationOps, UtxoWeak,
                  VerboseTransactionFrom};
use crate::{BalanceFut, CoinBalance, DexFee, FeeApproxStage, FoundSwapTxSpend, HistorySyncState, MarketCoinOps,
            MmCoin, NegotiateSwapContractAddrErr, NumConversError, RawTransactionFut, RawTransactionRequest,
            SignRawTransactionRequest, SignatureError, SignatureResult, SwapOps, TradeFee, TradePreimageFut,
            TradePreimageResult, TradePreimageValue, TransactionDetails, TransactionEnum, TransactionFut,
            TxFeeDetails, UnexpectedDerivationMethod, ValidateAddressResult, ValidateFeeArgs, ValidatePaymentInput,
            VerificationError, VerificationResult, WatcherOps, WithdrawFut, WithdrawRequest};
use crate::{Transaction, WithdrawError};
use async_trait::async_trait;
use chain::constants::SEQUENCE_FINAL;
use chain::{Transaction as UtxoTx, TransactionOutput};
use common::executor::{spawn, Timer};
use common::mm_number::{BigDecimal, MmNumber};
use common::{log, now_ms};
use crypto::privkey::key_pair_from_secret;
use crypto::{CryptoCtx, HDPathToCoin, KeyPairPolicy};
use futures::compat::Future01CompatExt;
use futures::lock::Mutex as AsyncMutex;
use futures::{FutureExt, TryFutureExt};
use futures01::Future;
use kdf_crypto::dhash160;
use keys::hash::H256;
use keys::{KeyPair, Public};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
#[cfg(test)] use mocktopus::macros::*;
use primitives::bytes::Bytes;
use rpc::v1::types::{Bytes as BytesJson, ToTxHash, Transaction as RpcTransaction, H256 as H256Json};
use sapling::keys::{FullViewingKey, OutgoingViewingKey};
use sapling::note_encryption::try_sapling_output_recovery;
use sapling::zip32::{ExtendedFullViewingKey, ExtendedSpendingKey};
use sapling::{CommitmentTree, IncrementalWitness, Node, Note, PaymentAddress};
use script::{Builder as ScriptBuilder, Opcode, Script, TransactionInputSigner};
use serde_json::{json, Value as Json};
use serialization::{deserialize, CoinVariant};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Weak};
#[cfg(not(target_arch = "wasm32"))]
use zcash_client_backend::data_api::{wallet::ConfirmationsPolicy, InputSource, TargetValue, WalletCommitmentTrees,
                                     WalletRead};
use zcash_client_backend::decrypt_transaction;
use zcash_keys::encoding::{decode_payment_address, encode_extended_spending_key, encode_payment_address};
#[cfg(not(target_arch = "wasm32"))]
use zcash_keys::keys::UnifiedFullViewingKey;
use zcash_primitives::merkle_tree::read_commitment_tree;
#[cfg(not(target_arch = "wasm32"))]
use zcash_primitives::transaction::builder::{BuildConfig, Builder as ZTxBuilder};
#[cfg(not(target_arch = "wasm32"))]
use zcash_primitives::transaction::fees::fixed::FeeRule as FixedFeeRule;
use zcash_primitives::transaction::Transaction as ZTransaction;
use zcash_protocol::consensus::{self, BlockHeight, BranchId, NetworkType, NetworkUpgrade, Parameters, H0};
use zcash_protocol::constants::mainnet as z_mainnet_constants;
use zcash_protocol::memo::MemoBytes;
use zcash_protocol::value::Zatoshis as Amount;
#[cfg(not(target_arch = "wasm32"))]
use zcash_protocol::ShieldedProtocol;
#[cfg(not(target_arch = "wasm32"))]
use zcash_transparent::builder::TransparentSigningSet;
use zcash_transparent::bundle::TxOut;
use zip32::ChildIndex;
// Native-only imports
#[cfg(not(target_arch = "wasm32"))] use std::fs::File;
#[cfg(not(target_arch = "wasm32"))] use std::io::Read;
#[cfg(not(target_arch = "wasm32"))] use std::num::NonZeroU32;
#[cfg(not(target_arch = "wasm32"))]
use std::path::{Path, PathBuf};
#[cfg(not(target_arch = "wasm32"))]
use zcash_client_sqlite::WalletDb;
#[cfg(not(target_arch = "wasm32"))]
use zcash_proofs::prover::LocalTxProver;

#[cfg(not(target_arch = "wasm32"))] mod z_htlc;
#[cfg(not(target_arch = "wasm32"))]
use z_htlc::{z_p2sh_spend, z_send_dex_fee, z_send_htlc};

#[cfg(not(target_arch = "wasm32"))] mod z_rpc;
#[cfg(not(target_arch = "wasm32"))]
use z_rpc::{ZRpcOps, ZUnspent};

mod z_coin_errors;
pub use z_coin_errors::*;

pub(crate) mod z_coin_sapling_cache;
#[cfg(target_arch = "wasm32")]
use z_coin_sapling_cache::ZCoinIdbSaplingCache;
#[cfg(not(target_arch = "wasm32"))]
use z_coin_sapling_cache::ZCoinSqliteSaplingCache;
use z_coin_sapling_cache::{SaplingBlockState, SaplingStateCacheOps, ZCoinSaplingCacheError};
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod z_coin_wallet_db;
#[cfg(not(target_arch = "wasm32"))]
use z_coin_wallet_db::ZCoinShieldedHistory;

/// `ZP2SHSpendError` compatible `TransactionErr` handling macro.
#[cfg(not(target_arch = "wasm32"))]
macro_rules! try_ztx_s {
    ($e: expr) => {
        match $e {
            Ok(ok) => ok,
            Err(err) => {
                if let Some(tx) = err.get_inner().get_tx() {
                    return Err(crate::TransactionErr::TxRecoverable(
                        tx,
                        format!("{}:{}] {:?}", file!(), line!(), err),
                    ));
                }

                return Err(crate::TransactionErr::Plain(ERRL!("{:?}", err)));
            },
        }
    };
}

mod z_coin_ops;
#[cfg(not(target_arch = "wasm32"))] mod z_swap_ops;

#[cfg(all(test, feature = "zhtlc-native-tests"))]
mod z_coin_tests;

/// Zcash consensus/network parameters for a shielded coin, sourced from the
/// coin config's `protocol.protocol_data.consensus_params` (R39.1.3, R39.6.4).
///
/// This is the single authority for the coin's network-parameter lookups
/// (activation heights, `coin_type`, the `hrp_sapling_*` prefixes and the two
/// `b58_*` transparent-address version prefixes) and replaces the previously
/// hardcoded Zcash-mainnet constant set. It implements the zcash
/// [`consensus::Parameters`] trait so it can be handed directly to the Sapling
/// transaction builder, note trial-decryption and output-recovery routines.
///
/// The serde field names are dictated config/wire interop and must not change.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ZcoinConsensusParams {
    /// Overwinter network-upgrade activation height.
    overwinter_activation_height: u32,
    /// Sapling activation height; also the lower floor for any sync start point.
    sapling_activation_height: u32,
    /// Blossom activation height, or `null` if not applicable.
    blossom_activation_height: Option<u32>,
    /// Heartwood activation height, or `null`.
    heartwood_activation_height: Option<u32>,
    /// Canopy activation height, or `null`.
    canopy_activation_height: Option<u32>,
    /// SLIP-44 coin type used in shielded HD derivation.
    coin_type: u32,
    /// Bech32 human-readable prefix for extended spending keys.
    hrp_sapling_extended_spending_key: String,
    /// Bech32 HRP for extended full-viewing keys.
    hrp_sapling_extended_full_viewing_key: String,
    /// Bech32 HRP for shielded payment addresses.
    hrp_sapling_payment_address: String,
    /// Base58Check version prefix for transparent p2pkh addresses.
    b58_pubkey_address_prefix: [u8; 2],
    /// Base58Check version prefix for transparent p2sh addresses.
    b58_script_address_prefix: [u8; 2],
}

impl ZcoinConsensusParams {
    fn network_type_hint(&self) -> NetworkType {
        use zcash_protocol::constants::{regtest, testnet};

        if self.hrp_sapling_payment_address == testnet::HRP_SAPLING_PAYMENT_ADDRESS
            && self.hrp_sapling_extended_spending_key == testnet::HRP_SAPLING_EXTENDED_SPENDING_KEY
        {
            NetworkType::Test
        } else if self.hrp_sapling_payment_address == regtest::HRP_SAPLING_PAYMENT_ADDRESS
            && self.hrp_sapling_extended_spending_key == regtest::HRP_SAPLING_EXTENDED_SPENDING_KEY
        {
            NetworkType::Regtest
        } else {
            // KDF-family production Z-coins use independently configured
            // Sapling encodings. Modern librustzcash requires one stock network
            // type for internal unified-key persistence; all externally visible
            // Sapling encodings continue to use the configured values below.
            NetworkType::Main
        }
    }

    fn coin_type(&self) -> u32 { self.coin_type }

    fn hrp_sapling_extended_spending_key(&self) -> &str { &self.hrp_sapling_extended_spending_key }

    fn hrp_sapling_extended_full_viewing_key(&self) -> &str { &self.hrp_sapling_extended_full_viewing_key }

    fn hrp_sapling_payment_address(&self) -> &str { &self.hrp_sapling_payment_address }

    fn b58_pubkey_address_prefix(&self) -> [u8; 2] { self.b58_pubkey_address_prefix }

    fn b58_script_address_prefix(&self) -> [u8; 2] { self.b58_script_address_prefix }
}

impl consensus::Parameters for ZcoinConsensusParams {
    fn network_type(&self) -> NetworkType { self.network_type_hint() }

    fn activation_height(&self, nu: NetworkUpgrade) -> Option<BlockHeight> {
        match nu {
            NetworkUpgrade::Overwinter => Some(BlockHeight::from_u32(self.overwinter_activation_height)),
            NetworkUpgrade::Sapling => Some(BlockHeight::from_u32(self.sapling_activation_height)),
            NetworkUpgrade::Blossom => self.blossom_activation_height.map(BlockHeight::from_u32),
            NetworkUpgrade::Heartwood => self.heartwood_activation_height.map(BlockHeight::from_u32),
            NetworkUpgrade::Canopy => self.canopy_activation_height.map(BlockHeight::from_u32),
            NetworkUpgrade::Nu5 | NetworkUpgrade::Nu6 | NetworkUpgrade::Nu6_1 | NetworkUpgrade::Nu6_2 => None,
        }
    }
}

/// Sync-anchor block descriptor from `protocol.protocol_data.check_point_block`
/// (R39.1.4). When present it is the sync-start anchor: in native mode the
/// wallet-DB commitment-tree cache is anchored at `height` (seeded from
/// `sapling_tree`) instead of replaying from Sapling activation.
///
/// The serde field names are dictated config/wire interop and must not change.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckPointBlockInfo {
    /// Block height of the checkpoint.
    pub height: u32,
    /// 32-byte block hash, hex-encoded.
    pub hash: H256Json,
    /// Block timestamp (Unix seconds).
    pub time: u32,
    /// Hex-encoded Sapling commitment-tree state as of this block.
    pub sapling_tree: BytesJson,
}

/// Shielded protocol-info payload carried by `CoinProtocol::ZHTLC`
/// (R39.1.2). Deserialized from `protocol.protocol_data`; `consensus_params`
/// is required, the checkpoint and derivation path are optional.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ZcoinProtocolInfo {
    /// The Zcash consensus parameters for the coin (R39.1.3).
    pub consensus_params: ZcoinConsensusParams,
    /// Optional sync-anchor block descriptor (R39.1.4).
    pub check_point_block: Option<CheckPointBlockInfo>,
    /// Sync throughput tuning: blocks to process per iteration (R39.6.2).
    /// Defaults to 1000 (the dictated activation default). Higher values batch
    /// multiple blocks per cycle.
    #[serde(default = "default_blocks_per_iteration")]
    pub blocks_per_iteration: u32,
    /// Sync pacing: milliseconds to sleep between iterations (R39.6.2).
    /// Defaults to 0 (no sleep). Positive values rate-limit the sync loop.
    #[serde(default)]
    pub inter_iteration_interval_ms: u64,
    /// Optional coin-level ZIP32/BIP32 HD path (e.g. `m/32'/133'`) used for
    /// shielded key derivation when the key policy is HD-derived (R39.1.4).
    pub z_derivation_path: Option<HDPathToCoin>,
}

fn default_blocks_per_iteration() -> u32 { 1000 }

/// Outgoing Viewing Key (OVK) used to encrypt the outgoing-cipher portion of
/// every Sapling output that pays a swap dex-fee on Pirate Chain (ARRR).
///
/// The byte value `[7; 32]` is **shared protocol convention**, not an
/// authorial choice:
///
/// 1. **Audit / interoperability convention.** Every atomic-swap participant
///    on the ARRR shielded fee path must use the same OVK so that anyone can
///    decrypt the outgoing memos of fee outputs and verify that fees were
///    actually paid to the expected address. Changing the value would
///    silently break the audit convention shared with all other
///    implementations of the same swap protocol (AtomicDEX, GLEEC, KDF-
///    Reloaded, and any third-party participant).
/// 2. **Baseline continuity.** The value, type, and source location are
///    preserved from the shared GPLv2 baseline `c1d46c0c1` to maintain
///    protocol-level continuity across legacy and current participants.
///
/// Do not refactor away or replace this constant without a protocol-level
/// compatibility analysis of the ARRR shielded-fee path.
const DEX_FEE_OVK: OutgoingViewingKey = OutgoingViewingKey([7; 32]);

const SAPLING_SPEND_NAME: &str = "sapling-spend.params";
const SAPLING_OUTPUT_NAME: &str = "sapling-output.params";
#[cfg(not(target_arch = "wasm32"))]
const SAPLING_SPEND_HASH: &str =
    "8270785a1a0d0bc77196f000ee6d221c9c9894f55307bd9357c3f0105d31ca63991ab91324160d8f53e2bbd3c2633a6eb8bdf5205d822e7f3f73edac51b2b70c";
#[cfg(not(target_arch = "wasm32"))]
const SAPLING_OUTPUT_HASH: &str =
    "657e3d38dbb5cb5e7dd2970e8b03d69b4787dd907285b5a7f0790dcc8072f60bf593b32cc2d1c030e00ff5ae64bf84c5c3beb84ddc841d48264b4a171744d028";

#[cfg(not(target_arch = "wasm32"))]
fn blake2b_file_hash(path: &Path) -> Result<String, std::io::Error> {
    let mut file = File::open(path)?;
    let mut state = blake2b_simd::State::new();
    let mut buffer = [0_u8; 1024 * 1024];

    loop {
        let read_bytes = file.read(&mut buffer)?;
        if read_bytes == 0 {
            break;
        }
        state.update(&buffer[..read_bytes]);
    }

    Ok(state.finalize().to_hex().to_string())
}

#[cfg(not(target_arch = "wasm32"))]
fn verify_zcash_params_integrity(params_dir: &Path) -> MmResult<(), ZCoinBuildError> {
    if !params_dir.exists() {
        return MmError::err(ZCoinBuildError::ZCashParamsDirNotFound {
            path: params_dir.display().to_string(),
        });
    }

    let verify = |file_name: &str, expected_hash: &str| {
        let path = params_dir.join(file_name);
        if !path.exists() {
            return MmError::err(ZCoinBuildError::ZCashParamsNotFound);
        }

        let actual_hash = blake2b_file_hash(&path).map_to_mm(|error| ZCoinBuildError::ZCashParamsReadError {
            file: file_name.to_string(),
            path: path.display().to_string(),
            error,
        })?;

        if actual_hash != expected_hash {
            return MmError::err(ZCoinBuildError::ZCashParamsHashMismatch {
                file: file_name.to_string(),
                path: path.display().to_string(),
                expected: expected_hash.to_string(),
                actual: actual_hash,
            });
        }

        Ok(())
    };

    verify(SAPLING_SPEND_NAME, SAPLING_SPEND_HASH)?;
    verify(SAPLING_OUTPUT_NAME, SAPLING_OUTPUT_HASH)?;
    Ok(())
}

pub struct ZCoinFields {
    dex_fee_addr: PaymentAddress,
    my_z_addr: PaymentAddress,
    my_z_addr_encoded: String,
    z_spending_key: ExtendedSpendingKey,
    /// Transaction prover: loads the sapling spend/output parameters.
    /// Native only — WASM cannot build shielded transactions (no param files).
    #[cfg(not(target_arch = "wasm32"))]
    z_tx_prover: LocalTxProver,
    /// Mutex preventing concurrent transaction generation/same input usage
    z_unspent_mutex: AsyncMutex<()>,
    sapling_state_synced: AtomicBool,
    /// Platform-agnostic sapling state cache (SQLite on native, IndexedDB on WASM).
    sapling_cache: Arc<dyn SaplingStateCacheOps + Send + Sync>,
    /// Native zcash_client_sqlite-compatible compact-block cache and wallet database.
    #[cfg(not(target_arch = "wasm32"))]
    shielded_history: Arc<ZCoinShieldedHistory>,
    /// True only after the zcash_client_sqlite wallet DB has scanned through
    /// the activation tip. This is intentionally separate from the legacy
    /// Sapling commitment-cache flag (R39.8.0a/b).
    #[cfg(not(target_arch = "wasm32"))]
    wallet_db_scan_complete: AtomicBool,
    #[cfg(not(target_arch = "wasm32"))]
    wallet_db_scanned_through: AtomicU64,
    /// Zcash consensus/network parameters sourced from `protocol_data`; the
    /// single authority for this coin's network-parameter lookups (R39.6.4).
    consensus_params: ZcoinConsensusParams,
    /// Optional sync-anchor checkpoint sourced from `protocol_data` (R39.1.4).
    check_point_block: Option<CheckPointBlockInfo>,
    /// Sync throughput tuning: blocks to process per iteration (R39.6.2).
    /// Defaults to 1000. Higher values batch multiple blocks per cycle.
    pub blocks_per_iteration: u32,
    /// Sync pacing: milliseconds to sleep between iterations (R39.6.2).
    /// Defaults to 0 (no sleep). Positive values rate-limit the sync loop.
    pub inter_iteration_interval_ms: u64,
}

impl std::fmt::Debug for ZCoinFields {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "ZCoinFields {{ my_z_addr: {:?}, my_z_addr_encoded: {} }}",
            self.my_z_addr, self.my_z_addr_encoded
        )
    }
}

impl Transaction for ZTransaction {
    fn tx_hex(&self) -> Vec<u8> {
        let mut hex = Vec::with_capacity(1024);
        self.write(&mut hex).expect("Writing should not fail");
        hex
    }

    fn tx_hash(&self) -> BytesJson {
        let mut bytes = self.txid().as_ref().to_vec();
        bytes.reverse();
        bytes.into()
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn z_coin_history_sync_status(
    wallet_db_scan_complete: bool,
    wallet_db_scanned_through: u64,
    sapling_state_synced: bool,
) -> HistorySyncState {
    if !wallet_db_scan_complete {
        return HistorySyncState::InProgress(json!({
            "type": "shielded_wallet_db_scan",
            "scanned_through": wallet_db_scanned_through
        }));
    }

    if sapling_state_synced {
        HistorySyncState::Finished
    } else {
        HistorySyncState::InProgress(json!({ "type": "sapling_state_cache_scan" }))
    }
}

#[derive(Clone, Debug)]
pub struct ZCoin {
    utxo_arc: UtxoArc,
    z_fields: Arc<ZCoinFields>,
}

pub struct ZOutput {
    pub to_addr: PaymentAddress,
    pub amount: Amount,
    pub viewing_key: Option<OutgoingViewingKey>,
    pub memo: Option<MemoBytes>,
}

#[cfg(not(target_arch = "wasm32"))]
fn native_sapling_cache_path(ticker: &str, mut db_dir_path: PathBuf) -> PathBuf {
    db_dir_path.push(format!("{}_CACHE.db", ticker));
    db_dir_path
}

#[cfg(not(target_arch = "wasm32"))]
fn open_or_create_native_sapling_cache(path: PathBuf) -> MmResult<ZCoinSqliteSaplingCache, ZCoinBuildError> {
    use db_common::sqlite::rusqlite::Connection;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path).map_err(|e| MmError::new(ZCoinBuildError::from(e)))?;
    ZCoinSqliteSaplingCache::open(conn).map_err(|e| MmError::new(ZCoinBuildError::SaplingCacheError(e.to_string())))
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod native_sapling_cache_tests {
    use super::*;

    #[test]
    fn missing_native_sapling_cache_is_created_in_dbdir() {
        let db_dir = std::env::temp_dir().join(format!("kdf-zcoin-cache-test-{}-{}", std::process::id(), now_ms()));
        let cache_path = native_sapling_cache_path("ARRR", db_dir.clone());

        assert!(!cache_path.exists());
        let _cache = open_or_create_native_sapling_cache(cache_path.clone()).unwrap();
        assert!(cache_path.exists());

        let _ = std::fs::remove_dir_all(db_dir);
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod shielded_history_status_tests {
    use super::*;

    #[test]
    fn unscanned_wallet_db_does_not_report_finished_even_if_sapling_cache_synced() {
        let status = z_coin_history_sync_status(false, 10, true);
        match status {
            HistorySyncState::InProgress(info) => {
                assert_eq!(info["type"], "shielded_wallet_db_scan");
                assert_eq!(info["scanned_through"], 10);
            },
            HistorySyncState::Finished => panic!("wallet DB scan completion must gate Finished status"),
            other => panic!("unexpected status {:?}", other),
        }
    }

    #[test]
    fn finished_requires_wallet_db_and_sapling_cache_completion() {
        assert!(matches!(
            z_coin_history_sync_status(true, 10, true),
            HistorySyncState::Finished
        ));

        let status = z_coin_history_sync_status(true, 10, false);
        match status {
            HistorySyncState::InProgress(info) => assert_eq!(info["type"], "sapling_state_cache_scan"),
            HistorySyncState::Finished => panic!("sapling cache completion is still required"),
            other => panic!("unexpected status {:?}", other),
        }
    }
}

impl AsRef<UtxoCoinFields> for ZCoin {
    fn as_ref(&self) -> &UtxoCoinFields { &self.utxo_arc }
}

pub async fn z_coin_from_conf_and_params(
    ctx: &MmArc,
    ticker: &str,
    conf: &Json,
    params: &UtxoActivationParams,
    secp_priv_key: &[u8],
    account: u32,
    #[cfg(not(target_arch = "wasm32"))] zcash_params_path: Option<PathBuf>,
    protocol_info: ZcoinProtocolInfo,
) -> Result<ZCoin, MmError<ZCoinBuildError>> {
    let z_key = shielded_spending_key_for_policy(ctx, secp_priv_key, account, &protocol_info)?;
    z_coin_from_conf_and_params_with_z_key(
        ctx,
        ticker,
        conf,
        params,
        secp_priv_key,
        #[cfg(not(target_arch = "wasm32"))]
        ctx.dbdir(),
        #[cfg(not(target_arch = "wasm32"))]
        zcash_params_path,
        z_key,
        protocol_info,
    )
    .await
}

/// Selects the shielded (Sapling ZIP32) spending key according to the active key
/// policy (R39.6.4 §2):
///
/// - **Iguana / legacy passphrase:** the spending key is the ZIP32 master derived
///   directly from the coin's iguana secret (the deployed behavior).
/// - **HD (BIP39) wallet:** the spending key is derived from the wallet's BIP39
///   seed along the coin's `z_derivation_path` with the activation `account`
///   appended as a hardened child (`m/<z_derivation_path>/account'`). In this
///   policy `z_derivation_path` is required; its absence is an error.
///
/// # Errors
/// Returns [`ZCoinBuildError::HdDerivationError`] when the crypto context is
/// unavailable or the HD policy is active but `z_derivation_path` is absent.
fn shielded_spending_key_for_policy(
    ctx: &MmArc,
    secp_priv_key: &[u8],
    account: u32,
    protocol_info: &ZcoinProtocolInfo,
) -> Result<ExtendedSpendingKey, MmError<ZCoinBuildError>> {
    let crypto_ctx = CryptoCtx::from_ctx(ctx).mm_err(|e| ZCoinBuildError::HdDerivationError(e.to_string()))?;
    match crypto_ctx.key_pair_policy() {
        KeyPairPolicy::Iguana => Ok(ExtendedSpendingKey::master(secp_priv_key)),
        KeyPairPolicy::GlobalHDAccount(hd_ctx) => {
            let z_derivation_path = protocol_info.z_derivation_path.as_ref().or_mm_err(|| {
                ZCoinBuildError::HdDerivationError(
                    "z_derivation_path is required for HD-derived shielded key policy".to_owned(),
                )
            })?;
            Ok(derive_hd_shielded_spending_key(
                hd_ctx.root_seed_bytes(),
                z_derivation_path,
                account,
            ))
        },
    }
}

/// Derives the shielded (Sapling ZIP32) extended spending key for an HD wallet
/// along the coin's `z_derivation_path` with the activation `account` appended
/// as a hardened child: `m/<z_derivation_path>/account'` (R39.6.4 §2).
///
/// `z_derivation_path` is the coin-level path (purpose' / coin_type'); both of
/// its levels are hardened per ZIP32/BIP44, as is the appended account.
pub fn derive_hd_shielded_spending_key(
    root_seed: &[u8],
    z_derivation_path: &HDPathToCoin,
    account: u32,
) -> ExtendedSpendingKey {
    let master = ExtendedSpendingKey::master(root_seed);
    ExtendedSpendingKey::from_path(&master, &[
        ChildIndex::hardened(z_derivation_path.purpose() as u32),
        ChildIndex::hardened(z_derivation_path.coin_type()),
        ChildIndex::hardened(account),
    ])
}

#[cfg(test)]
mod hd_shielded_key_tests {
    use super::*;
    use std::str::FromStr;
    use zcash_client_backend::encoding::encode_extended_spending_key;

    #[test]
    fn hd_shielded_spending_key_uses_zip32_path_with_hardened_account() {
        let seed = [7u8; 64];
        let path = HDPathToCoin::from_str("m/32'/133'").unwrap();
        let hrp = "secret-extended-key-main";

        // The account is appended to `z_derivation_path` as a hardened child, i.e.
        // `m/32'/133'/account'` (R39.6.4 §2).
        let derived0 = encode_extended_spending_key(hrp, &derive_hd_shielded_spending_key(&seed, &path, 0));
        let expected0 = encode_extended_spending_key(
            hrp,
            &ExtendedSpendingKey::from_path(&ExtendedSpendingKey::master(&seed), &[
                ChildIndex::hardened(32),
                ChildIndex::hardened(133),
                ChildIndex::hardened(0),
            ]),
        );
        assert_eq!(derived0, expected0);

        // Distinct accounts derive distinct spending keys.
        let derived1 = encode_extended_spending_key(hrp, &derive_hd_shielded_spending_key(&seed, &path, 1));
        assert_ne!(derived0, derived1);
    }
}

async fn sapling_state_cache_loop(coin: ZCoin) {
    // Determine the starting height and tree state from the cache (R39.6.1).
    let query = coin.z_fields.sapling_cache.query_latest_block().await;
    let (mut processed_height, mut current_tree) = match query {
        Ok(Some(state)) => {
            let mut tree = state.prev_tree_state;
            for cmu in state.cmus {
                let node = match Option::from(Node::from_bytes(cmu.take())) {
                    Some(node) => node,
                    None => {
                        log::error!("Invalid Sapling note commitment in the local state cache");
                        return;
                    },
                };
                if tree.append(node).is_err() {
                    log::error!("Sapling commitment tree is full while restoring the local state cache");
                    return;
                }
            }
            (state.height, tree)
        },
        // Cache is empty (Ok(None)) or an error occurred: anchor from checkpoint or genesis.
        //
        // When a `check_point_block` is declared, seed the commitment tree from
        // its `sapling_tree` and resume at the block after the checkpoint;
        // otherwise start at the Sapling activation height (the floor below
        // which no shielded outputs exist) with an empty tree.
        //
        // Light mode seeds the modern shielded wallet scanner directly from
        // `check_point_block`; this legacy commitment-tree cache loop exits for
        // Electrum-backed activations and is not the lightwalletd scan path.
        Ok(None) | Err(_) => match coin.z_fields.check_point_block.as_ref() {
            Some(check_point) => match read_commitment_tree(check_point.sapling_tree.0.as_slice()) {
                Ok(tree) => (check_point.height + 1, tree),
                Err(e) => {
                    log::error!(
                        "Failed to seed commitment tree from check_point_block.sapling_tree: {}; \
                         falling back to sapling_activation_height",
                        e
                    );
                    (
                        coin.z_fields.consensus_params.sapling_activation_height,
                        CommitmentTree::empty(),
                    )
                },
            },
            None => (
                coin.z_fields.consensus_params.sapling_activation_height,
                CommitmentTree::empty(),
            ),
        },
    };

    let (utxo_weak, z_fields_weak) = coin.into_weak_parts();

    let zero_root = Some(H256Json::default());
    while let Some(coin) = ZCoin::from_weak_parts(&utxo_weak, &z_fields_weak) {
        coin.z_fields.sapling_state_synced.store(false, AtomicOrdering::Relaxed);
        let current_block = match coin.rpc_client().get_block_count().compat().await {
            Ok(b) => b,
            Err(e) => {
                log::error!("Error {} on getting block count", e);
                Timer::sleep(10.).await;
                continue;
            },
        };

        let native_client = match coin.rpc_client() {
            UtxoRpcClientEnum::Native(n) => n,
            UtxoRpcClientEnum::Electrum(_) => {
                log::debug!(
                    "Light-mode ZCoin legacy commitment-tree cache loop skipped for {}; \
                     wallet-history scanning is handled by the modern lightwalletd wallet scanner",
                    coin.ticker()
                );
                coin.z_fields.sapling_state_synced.store(true, AtomicOrdering::Relaxed);
                return;
            },
        };

        // Extract sync parameters for this iteration (R39.6.2)
        let blocks_per_iteration = coin.z_fields.blocks_per_iteration.max(1) as u64;
        let inter_iteration_interval_ms = coin.z_fields.inter_iteration_interval_ms;

        while processed_height as u64 <= current_block {
            // Process up to blocks_per_iteration blocks in this iteration (R39.6.2)
            let batch_end = std::cmp::min(current_block, processed_height as u64 + blocks_per_iteration - 1);

            while processed_height as u64 <= batch_end {
                let block = match native_client.get_block_by_height(processed_height as u64).await {
                    Ok(b) => b,
                    Err(e) => {
                        log::error!("Error {} on getting block", e);
                        Timer::sleep(1.).await;
                        continue;
                    },
                };
                let root_bytes = current_tree.root().to_bytes();

                let current_sapling_root = Some(H256::from(root_bytes).reversed().into());
                if current_sapling_root != block.final_sapling_root && block.final_sapling_root != zero_root {
                    let prev_tree_state = current_tree.clone();
                    let mut cmus = Vec::new();
                    for hash in block.tx {
                        let tx = native_client
                            .get_transaction_bytes(&hash)
                            .compat()
                            .await
                            .expect("Panic here to avoid storing invalid tree state to the DB");
                        let tx: UtxoTx = deserialize(tx.as_slice()).expect("Panic here to avoid invalid tree state");
                        for output in tx.shielded_outputs {
                            let node = match Option::from(Node::from_bytes(output.cmu.take())) {
                                Some(node) => node,
                                None => {
                                    log::error!(
                                        "Invalid Sapling note commitment in transaction {:?} at height {}",
                                        hash,
                                        processed_height
                                    );
                                    return;
                                },
                            };
                            if current_tree.append(node).is_err() {
                                log::error!(
                                    "Sapling commitment tree is full while processing transaction {:?} at height {}",
                                    hash,
                                    processed_height
                                );
                                return;
                            }
                            cmus.push(output.cmu);
                        }
                    }

                    let state_to_insert = SaplingBlockState {
                        height: processed_height + 1,
                        prev_tree_state,
                        cmus,
                    };
                    coin.z_fields
                        .sapling_cache
                        .insert_block_state(state_to_insert)
                        .await
                        .expect("Insertion should not fail");
                }
                processed_height += 1;
            }

            // Apply inter-iteration sleep for pacing control (R39.6.2)
            if inter_iteration_interval_ms > 0 {
                Timer::sleep((inter_iteration_interval_ms as f64) / 1000.0).await;
            }
        }
        coin.z_fields.sapling_state_synced.store(true, AtomicOrdering::Relaxed);
        drop(coin);
        Timer::sleep(10.).await;
    }
}

pub struct ZCoinBuilder<'a> {
    ctx: &'a MmArc,
    ticker: &'a str,
    conf: &'a Json,
    params: &'a UtxoActivationParams,
    secp_priv_key: &'a [u8],
    #[cfg(not(target_arch = "wasm32"))]
    db_dir_path: PathBuf,
    /// Optional caller-supplied Sapling parameter directory (R39.6.2
    /// `zcash_params_path`). When `None` the fixed platform default is used.
    #[cfg(not(target_arch = "wasm32"))]
    zcash_params_path: Option<PathBuf>,
    z_spending_key: ExtendedSpendingKey,
    protocol_info: ZcoinProtocolInfo,
}

impl<'a> UtxoCoinBuilderCommonOps for ZCoinBuilder<'a> {
    fn ctx(&self) -> &MmArc { self.ctx }

    fn conf(&self) -> &Json { self.conf }

    fn activation_params(&self) -> &UtxoActivationParams { self.params }

    fn ticker(&self) -> &str { self.ticker }
}

#[async_trait]
impl<'a> UtxoFieldsWithIguanaPrivKeyBuilder for ZCoinBuilder<'a> {}

#[async_trait]
impl<'a> UtxoCoinWithIguanaPrivKeyBuilder for ZCoinBuilder<'a> {
    type ResultCoin = ZCoin;
    type Error = ZCoinBuildError;

    fn priv_key(&self) -> &[u8] { self.secp_priv_key }

    async fn build(self) -> MmResult<Self::ResultCoin, Self::Error> {
        let utxo = self
            .build_utxo_fields_with_iguana_priv_key(self.priv_key())
            .await
            .mm_err(Into::into)?;
        let utxo_arc = UtxoArc::new(utxo);

        // ── Sapling state cache backend (R39.6.1) ─────────────────────────
        // Native: open the SQLite file; WASM: open IndexedDB.
        let sapling_cache: Arc<dyn SaplingStateCacheOps + Send + Sync> = {
            #[cfg(not(target_arch = "wasm32"))]
            {
                let cache_path = native_sapling_cache_path(self.ticker, self.db_dir_path.clone());
                Arc::new(tokio::task::block_in_place(move || {
                    open_or_create_native_sapling_cache(cache_path)
                })?)
            }
            #[cfg(target_arch = "wasm32")]
            {
                Arc::new(ZCoinIdbSaplingCache::new(self.ticker.to_owned(), self.ctx))
            }
        };

        let (_, my_z_addr) = self.z_spending_key.default_address();
        #[cfg(not(target_arch = "wasm32"))]
        #[allow(deprecated)]
        let extfvk = self.z_spending_key.to_extended_full_viewing_key();

        // All network parameters are sourced from the coin config's
        // `protocol_data.consensus_params` (R39.6.4) rather than hardcoded
        // Zcash-mainnet constants.
        let consensus_params = self.protocol_info.consensus_params;

        #[cfg(not(target_arch = "wasm32"))]
        let (shielded_history, wallet_db_scanned_through) = {
            let shielded_history = ZCoinShieldedHistory::open_or_create(
                self.ticker,
                self.db_dir_path.clone(),
                consensus_params.clone(),
                &extfvk,
                self.protocol_info.check_point_block.as_ref(),
            )?;
            let scanned_through = shielded_history
                .scanned_height()
                .map_err(|e| MmError::new(ZCoinBuildError::SaplingCacheError(e)))?
                .unwrap_or(0);
            (Arc::new(shielded_history), scanned_through)
        };

        let dex_fee_z_addr = mm2_net_config::net_config_or_panic(self.ctx.netid()).dex_fee_z_addr();
        let dex_fee_addr = decode_payment_address(consensus_params.hrp_sapling_payment_address(), dex_fee_z_addr)
            .expect("NetConfig dex_fee_z_addr must be a valid z-address");

        // Verify and load the sapling prover parameters (native only — WASM
        // cannot build shielded transactions without the param files).
        #[cfg(not(target_arch = "wasm32"))]
        let z_tx_prover = {
            let params_dir = self.zcash_params_path.clone().unwrap_or_else(zcash_params_path);
            verify_zcash_params_integrity(&params_dir)?;
            tokio::task::block_in_place(|| {
                LocalTxProver::new(
                    &params_dir.join(SAPLING_SPEND_NAME),
                    &params_dir.join(SAPLING_OUTPUT_NAME),
                )
            })
        };

        let my_z_addr_encoded = encode_payment_address(consensus_params.hrp_sapling_payment_address(), &my_z_addr);
        #[cfg(not(target_arch = "wasm32"))]
        let my_z_key_encoded = encode_extended_spending_key(
            consensus_params.hrp_sapling_extended_spending_key(),
            &self.z_spending_key,
        );

        let z_fields = ZCoinFields {
            dex_fee_addr,
            my_z_addr,
            my_z_addr_encoded,
            z_spending_key: self.z_spending_key,
            #[cfg(not(target_arch = "wasm32"))]
            z_tx_prover,
            z_unspent_mutex: AsyncMutex::new(()),
            sapling_state_synced: AtomicBool::new(false),
            sapling_cache,
            #[cfg(not(target_arch = "wasm32"))]
            shielded_history,
            #[cfg(not(target_arch = "wasm32"))]
            wallet_db_scan_complete: AtomicBool::new(false),
            #[cfg(not(target_arch = "wasm32"))]
            wallet_db_scanned_through: AtomicU64::new(wallet_db_scanned_through),
            consensus_params,
            check_point_block: self.protocol_info.check_point_block,
            blocks_per_iteration: self.protocol_info.blocks_per_iteration,
            inter_iteration_interval_ms: self.protocol_info.inter_iteration_interval_ms,
        };
        // The shielded spending key is selected by key policy before the builder
        // runs: under the HD (BIP39) policy it is derived from the wallet seed
        // along `protocol_info.z_derivation_path` with the activation account
        // (R39.6.4 §2, see `shielded_spending_key_for_policy`); under the legacy
        // Iguana policy it is the ZIP32 master of the iguana secret.

        let z_coin = ZCoin {
            utxo_arc,
            z_fields: Arc::new(z_fields),
        };

        #[cfg(not(target_arch = "wasm32"))]
        {
            if let UtxoRpcClientEnum::Native(_) = z_coin.rpc_client() {
                z_coin
                    .z_rpc()
                    .z_import_key(&my_z_key_encoded)
                    .compat()
                    .await
                    .mm_err(Into::into)?;
            }
        }
        spawn(sapling_state_cache_loop(z_coin.clone()));
        Ok(z_coin)
    }
}

impl<'a> ZCoinBuilder<'a> {
    pub fn new(
        ctx: &'a MmArc,
        ticker: &'a str,
        conf: &'a Json,
        params: &'a UtxoActivationParams,
        secp_priv_key: &'a [u8],
        #[cfg(not(target_arch = "wasm32"))] db_dir_path: PathBuf,
        #[cfg(not(target_arch = "wasm32"))] zcash_params_path: Option<PathBuf>,
        z_spending_key: ExtendedSpendingKey,
        protocol_info: ZcoinProtocolInfo,
    ) -> ZCoinBuilder<'a> {
        ZCoinBuilder {
            ctx,
            ticker,
            conf,
            params,
            secp_priv_key,
            #[cfg(not(target_arch = "wasm32"))]
            db_dir_path,
            #[cfg(not(target_arch = "wasm32"))]
            zcash_params_path,
            z_spending_key,
            protocol_info,
        }
    }
}

async fn z_coin_from_conf_and_params_with_z_key(
    ctx: &MmArc,
    ticker: &str,
    conf: &Json,
    params: &UtxoActivationParams,
    secp_priv_key: &[u8],
    #[cfg(not(target_arch = "wasm32"))] db_dir_path: PathBuf,
    #[cfg(not(target_arch = "wasm32"))] zcash_params_path: Option<PathBuf>,
    z_spending_key: ExtendedSpendingKey,
    protocol_info: ZcoinProtocolInfo,
) -> Result<ZCoin, MmError<ZCoinBuildError>> {
    let builder = ZCoinBuilder::new(
        ctx,
        ticker,
        conf,
        params,
        secp_priv_key,
        #[cfg(not(target_arch = "wasm32"))]
        db_dir_path,
        #[cfg(not(target_arch = "wasm32"))]
        zcash_params_path,
        z_spending_key,
        protocol_info,
    );
    builder.build().await
}

impl MarketCoinOps for ZCoin {
    fn ticker(&self) -> &str { &self.utxo_arc.conf.ticker }

    fn my_address(&self) -> Result<String, String> { Ok(self.z_fields.my_z_addr_encoded.clone()) }

    fn get_public_key(&self) -> Result<String, MmError<UnexpectedDerivationMethod>> {
        let pubkey = self.my_public_key()?;
        Ok(pubkey.to_string())
    }

    fn sign_message_hash(&self, _message: &str) -> Option<[u8; 32]> { None }

    fn sign_message(&self, _message: &str) -> SignatureResult<String> {
        MmError::err(SignatureError::InvalidRequest(
            "Message signing is not supported by the given coin type".to_string(),
        ))
    }

    fn verify_message(&self, _signature_base64: &str, _message: &str, _address: &str) -> VerificationResult<bool> {
        MmError::err(VerificationError::InvalidRequest(
            "Message verification is not supported by the given coin type".to_string(),
        ))
    }

    fn my_balance(&self) -> BalanceFut<CoinBalance> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let coin = self.clone();
            let fut = async move {
                if let UtxoRpcClientEnum::Electrum(_) = coin.rpc_client() {
                    if !coin.shielded_wallet_db_scan_complete() {
                        log::warn!(
                            "ZCoin light-mode balance requested before shielded wallet DB scan completed for {}",
                            coin.ticker()
                        );
                    }
                    let balance_sat = coin
                        .shielded_history()
                        .balance(coin.z_fields.consensus_params.clone())
                        .map_err(|e| MmError::new(crate::BalanceError::Internal(e)))?;
                    return Ok(CoinBalance {
                        spendable: big_decimal_from_sat_unsigned(balance_sat, coin.decimals()),
                        unspendable: BigDecimal::from(0),
                    });
                }
                let unspents = coin.my_z_unspents_ordered().await.mm_err(Into::into)?;
                let (spendable, unspendable) = unspents.iter().fold(
                    (BigDecimal::from(0), BigDecimal::from(0)),
                    |(cur_spendable, cur_unspendable), unspent| {
                        if unspent.confirmations > 0 {
                            (cur_spendable + unspent.amount.to_decimal(), cur_unspendable)
                        } else {
                            (cur_spendable, cur_unspendable + unspent.amount.to_decimal())
                        }
                    },
                );
                Ok(CoinBalance { spendable, unspendable })
            };
            Box::new(fut.boxed().compat())
        }

        #[cfg(target_arch = "wasm32")]
        {
            // WASM: ZCoin balance tracking from RPC is not supported; return zero balance.
            // Balance information would be maintained client-side from wallet state/sapling cache in the UI.
            Box::new(
                futures::future::ok(CoinBalance {
                    spendable: BigDecimal::from(0),
                    unspendable: BigDecimal::from(0),
                })
                .boxed()
                .compat(),
            )
        }
    }

    fn base_coin_balance(&self) -> BalanceFut<BigDecimal> { utxo_common::base_coin_balance(self) }

    fn platform_ticker(&self) -> &str { self.ticker() }

    #[inline(always)]
    fn send_raw_tx(&self, tx: &str) -> Box<dyn Future<Item = String, Error = String> + Send> {
        utxo_common::send_raw_tx(self.as_ref(), tx)
    }

    #[inline(always)]
    fn send_raw_tx_bytes(&self, tx: &[u8]) -> Box<dyn Future<Item = String, Error = String> + Send> {
        utxo_common::send_raw_tx_bytes(self.as_ref(), tx)
    }

    fn wait_for_confirmations(
        &self,
        tx: &[u8],
        confirmations: u64,
        requires_nota: bool,
        wait_until: u64,
        check_every: u64,
    ) -> Box<dyn Future<Item = (), Error = String> + Send> {
        utxo_common::wait_for_confirmations(self.as_ref(), tx, confirmations, requires_nota, wait_until, check_every)
    }

    fn wait_for_tx_spend(
        &self,
        transaction: &[u8],
        wait_until: u64,
        from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        utxo_common::wait_for_output_spend(
            self.as_ref(),
            transaction,
            utxo_common::DEFAULT_SWAP_VOUT,
            from_block,
            wait_until,
        )
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn tx_enum_from_bytes(&self, bytes: &[u8]) -> Result<TransactionEnum, String> {
        ZTransaction::read(bytes, BranchId::Sapling)
            .map(|tx| tx.into())
            .map_err(|e| e.to_string())
    }

    #[cfg(target_arch = "wasm32")]
    fn tx_enum_from_bytes(&self, _bytes: &[u8]) -> Result<TransactionEnum, String> {
        // WASM: Transaction parsing is not supported; ZCoin operations are read-only
        Err("Transaction parsing is not supported on WASM for ZCoin".to_owned())
    }

    fn current_block(&self) -> Box<dyn Future<Item = u64, Error = String> + Send> {
        utxo_common::current_block(&self.utxo_arc)
    }

    fn display_priv_key(&self) -> Result<String, String> {
        Ok(encode_extended_spending_key(
            self.z_fields.consensus_params.hrp_sapling_extended_spending_key(),
            &self.z_fields.z_spending_key,
        ))
    }

    fn min_tx_amount(&self) -> BigDecimal { utxo_common::min_tx_amount(self.as_ref()) }

    fn min_trading_vol(&self) -> MmNumber { utxo_common::min_trading_vol(self.as_ref()) }

    fn sign_raw_tx(&self, args: &SignRawTransactionRequest) -> RawTransactionFut {
        Box::new(utxo_common::sign_raw_tx(self.clone(), args.clone()).boxed().compat())
    }

    fn is_privacy(&self) -> bool { true }
}

#[cfg(not(target_arch = "wasm32"))]
#[async_trait]
impl InitWithdrawCoin for ZCoin {
    async fn init_withdraw(
        &self,
        _ctx: MmArc,
        req: WithdrawRequest,
        task_handle: &WithdrawTaskHandle,
    ) -> Result<TransactionDetails, MmError<WithdrawError>> {
        task_handle
            .update_in_progress_status(WithdrawInProgressStatus::GeneratingTransaction)
            .mm_err(WithdrawError::from)?;
        self.withdraw(req).compat().await
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[async_trait]
impl MmCoin for ZCoin {
    fn is_asset_chain(&self) -> bool { self.utxo_arc.conf.asset_chain }

    fn withdraw(&self, req: WithdrawRequest) -> WithdrawFut {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let coin = self.clone();
            let fut = async move {
                if req.fee.is_some() {
                    return MmError::err(WithdrawError::InternalError(
                        "Setting a custom withdraw fee is not supported for ZCoin yet".to_owned(),
                    ));
                }

                let to_addr =
                    decode_payment_address(coin.z_fields.consensus_params.hrp_sapling_payment_address(), &req.to)
                        .map_to_mm(|e| WithdrawError::InvalidAddress(format!("{}", e)))?;
                let amount = if req.max {
                    let fee = coin.get_one_kbyte_tx_fee().await.mm_err(Into::into)?;
                    let balance = coin.my_balance().compat().await.mm_err(Into::into)?;
                    balance.spendable - fee
                } else {
                    req.amount
                };
                let satoshi = sat_from_big_decimal(&amount, coin.decimals()).mm_err(Into::into)?;
                let z_output = ZOutput {
                    to_addr,
                    amount: Amount::from_u64(satoshi)
                        .map_to_mm(|_| NumConversError(format!("Failed to get ZCash amount from {}", amount)))
                        .mm_err(Into::into)?,
                    // TODO add optional viewing_key and memo fields to the WithdrawRequest
                    viewing_key: None,
                    memo: None,
                };

                let (tx, data) = coin.gen_tx(vec![], vec![z_output]).await.mm_err(Into::into)?;
                let mut tx_bytes = Vec::with_capacity(1024);
                tx.write(&mut tx_bytes)
                    .map_to_mm(|e| WithdrawError::InternalError(e.to_string()))?;
                let mut tx_hash = tx.txid().as_ref().to_vec();
                tx_hash.reverse();

                let my_balance_change = data.spent_by_me - data.received_by_me;

                Ok(TransactionDetails {
                    tx_hex: tx_bytes.into(),
                    tx_hash: tx_hash.to_tx_hash(),
                    from: vec![coin.z_fields.my_z_addr_encoded.clone()],
                    to: vec![req.to],
                    total_amount: big_decimal_from_sat_unsigned(data.spent_by_me, coin.decimals()),
                    spent_by_me: big_decimal_from_sat_unsigned(data.spent_by_me, coin.decimals()),
                    received_by_me: big_decimal_from_sat_unsigned(data.received_by_me, coin.decimals()),
                    my_balance_change: big_decimal_from_sat_unsigned(my_balance_change, coin.decimals()),
                    block_height: 0,
                    timestamp: 0,
                    fee_details: Some(TxFeeDetails::Utxo(UtxoFeeDetails {
                        coin: Some(coin.utxo_arc.conf.ticker.clone()),
                        amount: big_decimal_from_sat_unsigned(data.fee_amount, coin.decimals()),
                    })),
                    coin: coin.ticker().to_owned(),
                    internal_id: tx_hash.into(),
                    kmd_rewards: None,
                    transaction_type: Default::default(),
                })
            };
            Box::new(fut.boxed().compat())
        }

        #[cfg(target_arch = "wasm32")]
        {
            Box::new(
                futures::future::err(MmError::new(WithdrawError::InternalError(
                    "ZCoin shielded transaction generation (gen_tx) is not available on WASM; withdraw not supported"
                        .to_owned(),
                )))
                .boxed()
                .compat(),
            )
        }
    }

    fn get_raw_transaction(&self, req: RawTransactionRequest) -> RawTransactionFut {
        Box::new(utxo_common::get_raw_transaction(&self.utxo_arc, req).boxed().compat())
    }

    fn decimals(&self) -> u8 { self.utxo_arc.decimals }

    fn convert_to_address(&self, _from: &str, _to_address_format: Json) -> Result<String, String> {
        Err(MmError::new("Address conversion is not available for ZCoin".to_string()).to_string())
    }

    fn validate_address(&self, address: &str) -> ValidateAddressResult {
        match decode_payment_address(self.z_fields.consensus_params.hrp_sapling_payment_address(), address) {
            Ok(_) => ValidateAddressResult {
                is_valid: true,
                reason: None,
            },
            Err(e) => ValidateAddressResult {
                is_valid: false,
                reason: Some(format!("Error {} on decode_payment_address", e)),
            },
        }
    }

    fn process_history_loop(&self, _ctx: MmArc) -> Box<dyn Future<Item = (), Error = ()> + Send> {
        log::warn!("process_history_loop is not implemented for ZCoin yet!");
        Box::new(futures01::future::err(()))
    }

    fn history_sync_status(&self) -> HistorySyncState {
        #[cfg(not(target_arch = "wasm32"))]
        {
            z_coin_history_sync_status(
                self.z_fields.wallet_db_scan_complete.load(AtomicOrdering::Relaxed),
                self.z_fields.wallet_db_scanned_through.load(AtomicOrdering::Relaxed),
                self.is_sapling_state_synced(),
            )
        }

        #[cfg(target_arch = "wasm32")]
        {
            if self.is_sapling_state_synced() {
                HistorySyncState::Finished
            } else {
                HistorySyncState::InProgress(json!({ "type": "sapling_state_cache_scan" }))
            }
        }
    }

    fn get_trade_fee(&self) -> Box<dyn Future<Item = TradeFee, Error = String> + Send> {
        utxo_common::get_trade_fee(self.clone())
    }

    #[cfg(not(target_arch = "wasm32"))]
    async fn get_sender_trade_fee(
        &self,
        _value: TradePreimageValue,
        _stage: FeeApproxStage,
    ) -> TradePreimageResult<TradeFee> {
        Ok(TradeFee {
            coin: self.ticker().to_owned(),
            amount: self.get_one_kbyte_tx_fee().await.mm_err(Into::into)?.into(),
            paid_from_trading_vol: false,
        })
    }

    #[cfg(target_arch = "wasm32")]
    async fn get_sender_trade_fee(
        &self,
        _value: TradePreimageValue,
        _stage: FeeApproxStage,
    ) -> TradePreimageResult<TradeFee> {
        // WASM: Trade fee estimation not supported for ZCoin
        Ok(TradeFee {
            coin: self.ticker().to_owned(),
            amount: MmNumber::from(0),
            paid_from_trading_vol: false,
        })
    }

    fn get_receiver_trade_fee(&self, _stage: FeeApproxStage) -> TradePreimageFut<TradeFee> {
        utxo_common::get_receiver_trade_fee(self.clone())
    }

    #[cfg(not(target_arch = "wasm32"))]
    async fn get_fee_to_send_taker_fee(
        &self,
        _dex_fee_amount: BigDecimal,
        _stage: FeeApproxStage,
    ) -> TradePreimageResult<TradeFee> {
        Ok(TradeFee {
            coin: self.ticker().to_owned(),
            amount: self.get_one_kbyte_tx_fee().await.mm_err(Into::into)?.into(),
            paid_from_trading_vol: false,
        })
    }

    #[cfg(target_arch = "wasm32")]
    async fn get_fee_to_send_taker_fee(
        &self,
        _dex_fee_amount: BigDecimal,
        _stage: FeeApproxStage,
    ) -> TradePreimageResult<TradeFee> {
        // WASM: Trade fee estimation not supported for ZCoin
        Ok(TradeFee {
            coin: self.ticker().to_owned(),
            amount: MmNumber::from(0),
            paid_from_trading_vol: false,
        })
    }

    fn required_confirmations(&self) -> u64 { utxo_common::required_confirmations(&self.utxo_arc) }

    fn requires_notarization(&self) -> bool { utxo_common::requires_notarization(&self.utxo_arc) }

    fn set_required_confirmations(&self, confirmations: u64) {
        utxo_common::set_required_confirmations(&self.utxo_arc, confirmations)
    }

    fn set_requires_notarization(&self, requires_nota: bool) {
        utxo_common::set_requires_notarization(&self.utxo_arc, requires_nota)
    }

    fn swap_contract_address(&self) -> Option<BytesJson> { utxo_common::swap_contract_address() }

    fn mature_confirmations(&self) -> Option<u32> { Some(self.utxo_arc.conf.mature_confirmations) }

    fn coin_protocol_info(&self) -> Vec<u8> { utxo_common::coin_protocol_info(self) }

    fn is_coin_protocol_supported(&self, info: &Option<Vec<u8>>) -> bool {
        utxo_common::is_coin_protocol_supported(self, info)
    }
}

#[async_trait]
impl UtxoTxGenerationOps for ZCoin {
    async fn get_tx_fee(&self) -> UtxoRpcResult<ActualTxFee> { utxo_common::get_tx_fee(&self.utxo_arc).await }

    async fn calc_interest_if_required(
        &self,
        unsigned: TransactionInputSigner,
        data: AdditionalTxData,
        my_script_pub: Bytes,
    ) -> UtxoRpcResult<(TransactionInputSigner, AdditionalTxData)> {
        utxo_common::calc_interest_if_required(self, unsigned, data, my_script_pub).await
    }
}

#[async_trait]
impl UtxoTxBroadcastOps for ZCoin {
    async fn broadcast_tx(&self, tx: &UtxoTx) -> Result<H256Json, MmError<BroadcastTxErr>> {
        utxo_common::broadcast_tx(self, tx).await
    }
}

/// Please note `ZCoin` is not assumed to work with transparent UTXOs.
/// Remove implementation of the `GetUtxoListOps` trait for `ZCoin`
/// when [`ZCoin::preimage_trade_fee_required_to_send_outputs`] is refactored.
#[async_trait]
#[cfg_attr(test, mockable)]
impl GetUtxoListOps for ZCoin {
    async fn get_unspent_ordered_list(
        &self,
        address: &Address,
    ) -> UtxoRpcResult<(Vec<UnspentInfo>, RecentlySpentOutPointsGuard<'_>)> {
        utxo_common::get_unspent_ordered_list(self, address).await
    }

    async fn get_all_unspent_ordered_list(
        &self,
        address: &Address,
    ) -> UtxoRpcResult<(Vec<UnspentInfo>, RecentlySpentOutPointsGuard<'_>)> {
        utxo_common::get_all_unspent_ordered_list(self, address).await
    }

    async fn get_mature_unspent_ordered_list(
        &self,
        address: &Address,
    ) -> UtxoRpcResult<(MatureUnspentList, RecentlySpentOutPointsGuard<'_>)> {
        utxo_common::get_mature_unspent_ordered_list(self, address).await
    }
}

#[async_trait]
impl UtxoCommonOps for ZCoin {
    async fn get_htlc_spend_fee(&self, tx_size: u64) -> UtxoRpcResult<u64> {
        utxo_common::get_htlc_spend_fee(self, tx_size).await
    }

    fn addresses_from_script(&self, script: &Script) -> Result<Vec<Address>, String> {
        utxo_common::addresses_from_script(self, script)
    }

    fn denominate_satoshis(&self, satoshi: i64) -> f64 { utxo_common::denominate_satoshis(&self.utxo_arc, satoshi) }

    fn my_public_key(&self) -> Result<&Public, MmError<UnexpectedDerivationMethod>> {
        utxo_common::my_public_key(self.as_ref())
    }

    fn address_from_str(&self, address: &str) -> Result<Address, String> {
        utxo_common::checked_address_from_str(self, address)
    }

    async fn get_current_mtp(&self) -> UtxoRpcResult<u32> {
        utxo_common::get_current_mtp(&self.utxo_arc, CoinVariant::Standard).await
    }

    fn is_unspent_mature(&self, output: &RpcTransaction) -> bool {
        utxo_common::is_unspent_mature(self.utxo_arc.conf.mature_confirmations, output)
    }

    async fn calc_interest_of_tx(
        &self,
        _tx: &UtxoTx,
        _input_transactions: &mut HistoryUtxoTxMap,
    ) -> UtxoRpcResult<u64> {
        MmError::err(UtxoRpcError::Internal(
            "ZCoin doesn't support transaction rewards".to_owned(),
        ))
    }

    async fn get_mut_verbose_transaction_from_map_or_rpc<'a, 'b>(
        &'a self,
        tx_hash: H256Json,
        utxo_tx_map: &'b mut HistoryUtxoTxMap,
    ) -> UtxoRpcResult<&'b mut HistoryUtxoTx> {
        utxo_common::get_mut_verbose_transaction_from_map_or_rpc(self, tx_hash, utxo_tx_map).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn p2sh_spending_tx(
        &self,
        prev_transaction: UtxoTx,
        redeem_script: Bytes,
        outputs: Vec<TransactionOutput>,
        script_data: Script,
        sequence: u32,
        lock_time: u32,
        keypair: &KeyPair,
    ) -> Result<UtxoTx, String> {
        utxo_common::p2sh_spending_tx(
            self,
            prev_transaction,
            redeem_script,
            outputs,
            script_data,
            sequence,
            lock_time,
            keypair,
        )
        .await
    }

    fn get_verbose_transactions_from_cache_or_rpc(
        &self,
        tx_ids: HashSet<H256Json>,
    ) -> UtxoRpcFut<HashMap<H256Json, VerboseTransactionFrom>> {
        let selfi = self.clone();
        let fut = async move { utxo_common::get_verbose_transactions_from_cache_or_rpc(&selfi.utxo_arc, tx_ids).await };
        Box::new(fut.boxed().compat())
    }

    async fn preimage_trade_fee_required_to_send_outputs(
        &self,
        outputs: Vec<TransactionOutput>,
        fee_policy: FeePolicy,
        gas_fee: Option<u64>,
        stage: &FeeApproxStage,
    ) -> TradePreimageResult<BigDecimal> {
        utxo_common::preimage_trade_fee_required_to_send_outputs(self, outputs, fee_policy, gas_fee, stage).await
    }

    fn increase_dynamic_fee_by_stage(&self, dynamic_fee: u64, stage: &FeeApproxStage) -> u64 {
        utxo_common::increase_dynamic_fee_by_stage(self, dynamic_fee, stage)
    }

    async fn p2sh_tx_locktime(&self, htlc_locktime: u32) -> Result<u32, MmError<UtxoRpcError>> {
        utxo_common::p2sh_tx_locktime(self, self.ticker(), htlc_locktime).await
    }

    fn addr_format(&self) -> &UtxoAddressFormat { utxo_common::addr_format(self) }

    fn addr_format_for_standard_scripts(&self) -> UtxoAddressFormat {
        utxo_common::addr_format_for_standard_scripts(self)
    }

    fn address_from_pubkey(&self, pubkey: &Public) -> Address {
        let conf = &self.utxo_arc.conf;
        utxo_common::address_from_pubkey(
            pubkey,
            conf.pub_addr_prefix,
            conf.pub_t_addr_prefix,
            conf.checksum_type,
            conf.bech32_hrp.clone(),
            self.addr_format().clone(),
        )
    }
}

#[test]
fn derive_z_key_from_mm_seed() {
    use crypto::privkey::key_pair_from_seed;
    use zcash_client_backend::encoding::encode_extended_spending_key;

    let seed = "spice describe gravity federal blast come thank unfair canal monkey style afraid";
    let secp_keypair = key_pair_from_seed(seed).unwrap();
    let z_spending_key = ExtendedSpendingKey::master(&*secp_keypair.private().secret);
    let encoded = encode_extended_spending_key(z_mainnet_constants::HRP_SAPLING_EXTENDED_SPENDING_KEY, &z_spending_key);
    assert_eq!(encoded, "secret-extended-key-main1qqqqqqqqqqqqqqytwz2zjt587n63kyz6jawmflttqu5rxavvqx3lzfs0tdr0w7g5tgntxzf5erd3jtvva5s52qx0ms598r89vrmv30r69zehxy2r3vesghtqd6dfwdtnauzuj8u8eeqfx7qpglzu6z54uzque6nzzgnejkgq569ax4lmk0v95rfhxzxlq3zrrj2z2kqylx2jp8g68lqu6alczdxd59lzp4hlfuj3jp54fp06xsaaay0uyass992g507tdd7psua5w6q76dyq3");

    let (_, address) = z_spending_key.default_address();
    let encoded_addr = encode_payment_address(z_mainnet_constants::HRP_SAPLING_PAYMENT_ADDRESS, &address);
    assert_eq!(
        encoded_addr,
        "zs182ht30wnnnr8jjhj2j9v5dkx3qsknnr5r00jfwk2nczdtqy7w0v836kyy840kv2r8xle5gcl549"
    );

    let seed = "also shoot benefit prefer juice shell elder veteran woman mimic image kidney";
    let secp_keypair = key_pair_from_seed(seed).unwrap();
    let z_spending_key = ExtendedSpendingKey::master(&*secp_keypair.private().secret);
    let encoded = encode_extended_spending_key(z_mainnet_constants::HRP_SAPLING_EXTENDED_SPENDING_KEY, &z_spending_key);
    assert_eq!(encoded, "secret-extended-key-main1qqqqqqqqqqqqqq8jnhc9stsqwts6pu5ayzgy4szplvy03u227e50n3u8e6dwn5l0q5s3s8xfc03r5wmyh5s5dq536ufwn2k89ngdhnxy64sd989elwas6kr7ygztsdkw6k6xqyvhtu6e0dhm4mav8rus0fy8g0hgy9vt97cfjmus0m2m87p4qz5a00um7gwjwk494gul0uvt3gqyjujcclsqry72z57kr265jsajactgfn9m3vclqvx8fsdnwp4jwj57ffw560vvwks9g9hpu");

    let (_, address) = z_spending_key.default_address();
    let encoded_addr = encode_payment_address(z_mainnet_constants::HRP_SAPLING_PAYMENT_ADDRESS, &address);
    assert_eq!(
        encoded_addr,
        "zs1funuwrjr2stlr6fnhkdh7fyz3p7n0p8rxase9jnezdhc286v5mhs6q3myw0phzvad5mvqgfxpam"
    );
}
