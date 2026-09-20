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
use std::sync::{Arc, Mutex, Weak};
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

/// Whether this build can construct the transaction format the Ironwood network
/// upgrade requires.
///
/// `false` until the shielded transaction builder is able to emit version-6
/// transactions. While it is `false`, a coin that declares an Ironwood upgrade
/// stops accepting *new* swaps ahead of activation and stops building
/// transactions once activation passes; receiving, balance and history are
/// unaffected, and swaps already under way are never interrupted.
const IRONWOOD_V6_SUPPORTED: bool = false;

/// How far ahead of Ironwood activation a coin that cannot build v6 transactions
/// stops entering new swaps, in seconds.
///
/// A swap's HTLC must stay spendable *and* refundable for its whole lifetime. A
/// payment funded before activation in the old transaction format needs a spend
/// or refund after it, which a build without v6 support cannot produce — so the
/// cut-off must cover everything that has to happen after the last tradeable
/// instant, not merely the lock itself:
///
/// | Component | Seconds | Why |
/// |---|---|---|
/// | Longest maker payment lock | 156 000 | `PAYMENT_LOCKTIME` (7 800) x 10 (legacy slow-coin rule, reachable whenever a peer negotiates without confirmation settings) x 2 (the maker leg) |
/// | Refund grace | 3 700 | the swap machines wait `payment_lock + 3700` before refunding (`wait_refund_until`) |
/// | Mining allowance | 600 | ~10 blocks at Pirate's 60 s target, so the refund is *mined*, not merely broadcast |
///
/// The value is duplicated here rather than derived because the locktime constant
/// lives in the swap layer, which depends on this crate and not the other way
/// round. `payment_locktime_covers_ironwood_freeze_margin` in `mm2_main`'s swap
/// module fails if the two ever drift apart — it is what caught the first draft of
/// this constant, which covered the lock but not the refund grace.
pub const IRONWOOD_SWAP_FREEZE_MARGIN_SECS: u64 = 156_000 + 3_700 + 600;

/// Whether a coin declaring Ironwood activation at `activation_time` must refuse
/// to enter new swaps as of `now_sec`.
///
/// Split out from the callers so the rule is testable without a clock: the trait
/// methods supply `now_ms() / 1000`.
fn ironwood_swap_freeze_active_at(
    activation_time: Option<u32>,
    v6_supported: bool,
    freeze_margin_secs: u64,
    now_sec: u64,
) -> bool {
    if v6_supported {
        return false;
    }
    // A coin with no declared Ironwood upgrade is never frozen.
    let Some(activation_time) = activation_time else {
        return false;
    };
    // Saturating: an activation time inside the margin of the epoch would
    // otherwise wrap and freeze the coin forever.
    now_sec >= u64::from(activation_time).saturating_sub(freeze_margin_secs)
}

/// Whether a coin declaring Ironwood activation at `activation_time` must refuse
/// to build any transaction as of `now_sec`.
///
/// From activation the network accepts only the new transaction format, so a
/// build without that support can produce nothing spendable. Refusing locally
/// says why; the alternative is a transaction the network drops, surfaced as an
/// opaque broadcast failure after the user has already been charged the wait.
///
/// Distinct from [`ironwood_swap_freeze_active_at`], which starts *earlier* and
/// only blocks entering new swaps: a swap begun before activation must still be
/// able to spend or refund, so the two gates cannot share a cut-off.
fn ironwood_build_refused_at(activation_time: Option<u32>, v6_supported: bool, now_sec: u64) -> bool {
    if v6_supported {
        return false;
    }
    let Some(activation_time) = activation_time else {
        return false;
    };
    now_sec >= u64::from(activation_time)
}

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
    /// Wall-clock timestamp (Unix seconds) from which the coin's Ironwood
    /// network upgrade activates, or `null` when the coin has no such upgrade.
    ///
    /// Pirate does not fix an Ironwood activation *height* in advance: each node
    /// derives it at runtime from the first block whose time exceeds this value
    /// (plus a settling margin), so the height is not knowable until shortly
    /// before it takes effect. This field carries the only part of the rule that
    /// can be published ahead of time. Optional and additive; absent for every
    /// coin that has no Ironwood upgrade, and ignored by builds that predate it.
    ///
    /// **Compatibility:** GLEEC KDF has no equivalent and applies no upgrade
    /// gating. Omit this field to retain GLEEC-equivalent behaviour for a coin;
    /// when present it drives both the swap freeze and the build refusal. See
    /// `docs/GLEEC_COMPATIBILITY.md`.
    #[serde(default)]
    ironwood_activation_time: Option<u32>,
    /// Ironwood activation height, once the network has derived and published it.
    ///
    /// Optional: the height is unknown until the upgrade is imminent, and a
    /// wallet shipping an older coin configuration will not carry it at all.
    #[serde(default)]
    ironwood_activation_height: Option<u32>,
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

    /// Wall-clock timestamp from which Ironwood activates, when the coin declares one.
    #[allow(dead_code)] // Consumed by the Ironwood build guard and swap freeze.
    pub(crate) fn ironwood_activation_time(&self) -> Option<u32> { self.ironwood_activation_time }

    /// Ironwood activation height, when the network has derived and published one.
    #[allow(dead_code)] // Consumed by the Ironwood build guard and swap freeze.
    pub(crate) fn ironwood_activation_height(&self) -> Option<u32> { self.ironwood_activation_height }

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

/// Network parameters for **trial-decrypting notes only**.
///
/// Pirate accepts both the pre- and post-ZIP-212 note plaintext versions at every
/// height -- its own `plaintext_version_is_valid` says so in as many words, and it
/// has no Canopy upgrade at all. Upstream librustzcash derives ZIP-212 enforcement
/// solely from Canopy, so with Canopy absent it reports
/// [`Zip212Enforcement::Off`], which accepts **only** the `0x01` lead byte and
/// silently discards every note a modern Pirate wallet sends. The note does not
/// fail to decrypt loudly; it simply never appears, so the payment is invisible.
///
/// Reporting Canopy as active with a far-future activation height puts upstream
/// permanently inside its grace period, which accepts `0x01` and `0x02` alike --
/// exactly Pirate's rule.
///
/// # This type must never reach transaction construction
///
/// [`BranchId::for_height`] returns the branch of the *last active* upgrade, so a
/// Canopy that reports active would silently move our transactions off the Sapling
/// consensus branch and make every one of them invalid. Decryption never consults
/// the branch id, which is why the lie is safe here and nowhere else. Construct it
/// only at a decryption call site.
#[derive(Clone, Debug)]
pub(crate) struct ZcoinDecryptionParams(ZcoinConsensusParams);

/// Kept far enough above any real chain height that
/// `activation_height(Canopy) + ZIP212_GRACE_PERIOD` cannot be reached, so
/// enforcement stays in the grace period for the life of the chain, and low enough
/// that the addition cannot overflow.
const ZCOIN_DECRYPTION_CANOPY_HEIGHT: u32 = 0xF000_0000;

impl ZcoinDecryptionParams {
    pub(crate) fn new(params: ZcoinConsensusParams) -> Self { Self(params) }
}

impl consensus::Parameters for ZcoinDecryptionParams {
    fn network_type(&self) -> NetworkType { self.0.network_type() }

    fn activation_height(&self, nu: NetworkUpgrade) -> Option<BlockHeight> {
        match nu {
            // Reported as a height no chain reaches, so `is_nu_active` below is the
            // only thing that makes Canopy "active" and the grace-period window
            // never closes.
            NetworkUpgrade::Canopy => Some(BlockHeight::from_u32(ZCOIN_DECRYPTION_CANOPY_HEIGHT)),
            other => self.0.activation_height(other),
        }
    }

    fn is_nu_active(&self, nu: NetworkUpgrade, height: BlockHeight) -> bool {
        match nu {
            NetworkUpgrade::Canopy => true,
            other => self.0.activation_height(other).is_some_and(|h| h <= height),
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
    /// Wallet-owned mempool outputs not yet scanned into the wallet database,
    /// keyed by (txid, output index) so repeated observation across polls
    /// contributes a value at most once (CRD ch.39 R39.8.0af/ah). Replaced
    /// wholesale by each poll, so a dropped or mined transaction disappears
    /// without bespoke invalidation.
    #[cfg(not(target_arch = "wasm32"))]
    pending_receipts: Mutex<HashMap<([u8; 32], u32), u64>>,
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
mod zip212_decryption_tests {
    use super::*;
    use rand::rngs::OsRng;
    use sapling::keys::PreparedIncomingViewingKey;
    use sapling::note_encryption::{sapling_note_encryption, try_sapling_compact_note_decryption,
                                   CompactOutputDescription, Zip212Enforcement};
    use sapling::util::generate_random_rseed;
    use sapling::value::NoteValue;
    use sapling::{Note, Rseed};
    use zcash_note_encryption::{Domain, COMPACT_NOTE_SIZE};
    use zcash_primitives::transaction::components::sapling::zip212_enforcement;

    /// ARRR's real mainnet parameters: Pirate has no Blossom, Heartwood or Canopy
    /// upgrade at all, so all three are `null` in the coin configuration.
    fn arrr_params() -> ZcoinConsensusParams {
        serde_json::from_value(json!({
            "overwinter_activation_height": 152855,
            "sapling_activation_height": 152855,
            "blossom_activation_height": null,
            "heartwood_activation_height": null,
            "canopy_activation_height": null,
            "coin_type": 133,
            "hrp_sapling_extended_spending_key": "secret-extended-key-main",
            "hrp_sapling_extended_full_viewing_key": "zxviews",
            "hrp_sapling_payment_address": "zs",
            "b58_pubkey_address_prefix": [0x1c, 0xb8],
            "b58_script_address_prefix": [0x1c, 0xbd]
        }))
        .unwrap()
    }

    /// Builds a compact output for `rseed`, as a sending wallet would, and reports
    /// whether it trial-decrypts under `enforcement`.
    /// The rseed a sending wallet produces under `sender_enforcement`:
    /// pre-ZIP-212 when it is `Off`, post-ZIP-212 otherwise.
    fn rseed_from_sender(sender_enforcement: Zip212Enforcement) -> Rseed {
        generate_random_rseed(sender_enforcement, &mut OsRng)
    }

    fn note_decrypts(rseed: Rseed, enforcement: Zip212Enforcement) -> bool {
        let extsk = sapling::zip32::ExtendedSpendingKey::master(&[7u8; 32]);
        #[allow(deprecated)]
        let extfvk = extsk.to_extended_full_viewing_key();
        let (_, address) = extfvk.default_address();
        let note = Note::from_parts(address, NoteValue::from_raw(100_000), rseed);
        let encryptor = sapling_note_encryption(None, note.clone(), [0u8; 512], &mut OsRng);
        let enc = encryptor.encrypt_note_plaintext();
        let compact = CompactOutputDescription {
            ephemeral_key: sapling::note_encryption::SaplingDomain::epk_bytes(encryptor.epk()),
            cmu: note.cmu(),
            enc_ciphertext: enc[..COMPACT_NOTE_SIZE].try_into().unwrap(),
        };
        let ivk = PreparedIncomingViewingKey::new(&extfvk.fvk.vk.ivk());
        try_sapling_compact_note_decryption(&ivk, &compact, enforcement).is_some()
    }

    /// The defect: ARRR has no Canopy, upstream derives ZIP-212 enforcement solely
    /// from Canopy, and so the coin's own parameters yield `Off` -- which accepts
    /// only the pre-ZIP-212 lead byte. Every note a modern Pirate wallet sends
    /// carries the post-ZIP-212 byte and is discarded without an error, so the
    /// payment never appears. This pins the cause, so the fix below cannot be
    /// mistaken for a no-op.
    #[test]
    fn the_coins_own_parameters_reject_post_zip212_notes() {
        let height = BlockHeight::from_u32(4_138_653);
        assert_eq!(zip212_enforcement(&arrr_params(), height), Zip212Enforcement::Off);
        assert!(
            note_decrypts(rseed_from_sender(Zip212Enforcement::Off), Zip212Enforcement::Off),
            "a pre-ZIP-212 note must still decrypt"
        );
        assert!(
            !note_decrypts(rseed_from_sender(Zip212Enforcement::On), Zip212Enforcement::Off),
            "this is the bug being fixed: a post-ZIP-212 note is silently dropped"
        );
    }

    /// Pirate accepts both plaintext versions at every height. The decryption
    /// parameters must reproduce that, which upstream expresses as the grace
    /// period (R39.8.0am).
    #[test]
    fn decryption_parameters_accept_both_note_plaintext_versions() {
        let params = ZcoinDecryptionParams::new(arrr_params());
        for height in [152_855u32, 4_138_653, 0xEFFF_FFFF] {
            assert_eq!(
                zip212_enforcement(&params, BlockHeight::from_u32(height)),
                Zip212Enforcement::GracePeriod,
                "enforcement must stay in the grace period at height {}",
                height
            );
        }
        assert!(note_decrypts(
            rseed_from_sender(Zip212Enforcement::Off),
            Zip212Enforcement::GracePeriod
        ));
        assert!(note_decrypts(
            rseed_from_sender(Zip212Enforcement::On),
            Zip212Enforcement::GracePeriod
        ));
    }

    /// The hazard that rules out simply lying to the shared parameters:
    /// `BranchId::for_height` returns the branch of the last *active* upgrade, so a
    /// Canopy reporting active would move every transaction we sign off the Sapling
    /// branch and make it invalid. The decryption parameters are allowed to report
    /// Canopy active precisely because nothing that builds a transaction ever sees
    /// them -- and the coin's real parameters must keep resolving to Sapling.
    #[test]
    fn the_real_parameters_still_resolve_to_the_sapling_branch() {
        let height = BlockHeight::from_u32(4_138_653);
        assert_eq!(BranchId::for_height(&arrr_params(), height), BranchId::Sapling);
        // And the decryption wrapper would not be safe to build with, which is why
        // it is confined to decryption call sites.
        assert_ne!(
            BranchId::for_height(&ZcoinDecryptionParams::new(arrr_params()), height),
            BranchId::Sapling,
            "if this ever becomes Sapling the confinement rule can be relaxed -- until then it must not be"
        );
    }
}

#[cfg(test)]
mod ironwood_swap_freeze_tests {
    use super::*;

    /// Pirate mainnet Ironwood activation: Sat 3 Oct 2026 19:00:00 UTC.
    const ARRR_IRONWOOD_ACTIVATION: u32 = 1_791_054_000;
    /// The moment the freeze engages for that activation: 1 Oct 2026 23:40:00 UTC.
    const ARRR_FREEZE_START: u64 = ARRR_IRONWOOD_ACTIVATION as u64 - IRONWOOD_SWAP_FREEZE_MARGIN_SECS;

    fn frozen_at(now_sec: u64) -> bool {
        ironwood_swap_freeze_active_at(
            Some(ARRR_IRONWOOD_ACTIVATION),
            IRONWOOD_V6_SUPPORTED,
            IRONWOOD_SWAP_FREEZE_MARGIN_SECS,
            now_sec,
        )
    }

    /// A coin that declares no Ironwood upgrade is never frozen, whatever the clock
    /// says. Every non-Pirate shielded coin depends on this.
    #[test]
    fn a_coin_without_an_ironwood_upgrade_is_never_frozen() {
        for now in [0, ARRR_FREEZE_START, u64::MAX] {
            assert!(!ironwood_swap_freeze_active_at(
                None,
                IRONWOOD_V6_SUPPORTED,
                IRONWOOD_SWAP_FREEZE_MARGIN_SECS,
                now
            ));
        }
    }

    /// Once the builder can emit v6 the freeze must lift entirely, including after
    /// activation -- otherwise flipping the capability flag would leave the coin
    /// permanently untradeable.
    #[test]
    fn a_v6_capable_build_is_never_frozen() {
        for now in [ARRR_FREEZE_START, ARRR_IRONWOOD_ACTIVATION as u64 + 86_400] {
            assert!(!ironwood_swap_freeze_active_at(
                Some(ARRR_IRONWOOD_ACTIVATION),
                true,
                IRONWOOD_SWAP_FREEZE_MARGIN_SECS,
                now
            ));
        }
    }

    /// The boundary is exact and one-way: trading right up to the cut-off, frozen
    /// from it onwards, and it never lifts by itself after activation.
    #[test]
    fn the_freeze_engages_at_the_cutoff_and_does_not_lift() {
        assert!(
            !frozen_at(ARRR_FREEZE_START - 1),
            "a second before the cut-off must still trade"
        );
        assert!(frozen_at(ARRR_FREEZE_START), "the cut-off itself must freeze");
        assert!(frozen_at(ARRR_FREEZE_START + 1));
        assert!(
            frozen_at(ARRR_IRONWOOD_ACTIVATION as u64),
            "activation itself stays frozen"
        );
        assert!(
            frozen_at(ARRR_IRONWOOD_ACTIVATION as u64 + 365 * 86_400),
            "the freeze must not lift on its own long after activation"
        );
    }

    /// The margin must cover the longest HTLC this framework can produce, so a
    /// payment made in the last tradeable second is still refundable before
    /// activation. 156 000 s = PAYMENT_LOCKTIME(7 800) * 10 (legacy slow-coin rule)
    /// * 2 (the maker leg). `mm2_main` holds the matching guard against the live
    /// constant.
    #[test]
    fn the_margin_covers_the_longest_maker_payment_lock() {
        const PAYMENT_LOCKTIME: u64 = 3600 * 2 + 300 * 2;
        assert_eq!(PAYMENT_LOCKTIME, 7_800);
        let longest_lock = PAYMENT_LOCKTIME * 10 * 2;
        assert!(
            IRONWOOD_SWAP_FREEZE_MARGIN_SECS >= longest_lock,
            "freeze margin {} must cover the longest maker payment lock {}",
            IRONWOOD_SWAP_FREEZE_MARGIN_SECS,
            longest_lock
        );
        // The lock alone is not enough: the swap machines wait `lock + 3700` before
        // refunding, and the refund still has to be mined.
        assert!(
            IRONWOOD_SWAP_FREEZE_MARGIN_SECS >= longest_lock + 3_700,
            "freeze margin {} must also cover the refund grace",
            IRONWOOD_SWAP_FREEZE_MARGIN_SECS
        );
        // The whole point: a payment made at the last tradeable instant must still
        // be refundable, and mined, before activation.
        assert!((ARRR_FREEZE_START - 1) + longest_lock + 3_700 < ARRR_IRONWOOD_ACTIVATION as u64);
    }

    /// From activation the network accepts only the new transaction format, so a
    /// build without it must refuse rather than emit something unspendable.
    #[test]
    fn building_is_refused_from_activation_onwards() {
        let refused = |now| ironwood_build_refused_at(Some(ARRR_IRONWOOD_ACTIVATION), IRONWOOD_V6_SUPPORTED, now);
        assert!(
            !refused(ARRR_IRONWOOD_ACTIVATION as u64 - 1),
            "a second before activation must still build"
        );
        assert!(
            refused(ARRR_IRONWOOD_ACTIVATION as u64),
            "activation itself must refuse"
        );
        assert!(refused(ARRR_IRONWOOD_ACTIVATION as u64 + 365 * 86_400));
    }

    /// A coin with no Ironwood upgrade, and a build that can produce the new
    /// format, must both be unaffected -- otherwise flipping the capability flag
    /// would leave the coin permanently unable to transact.
    #[test]
    fn building_is_never_refused_without_an_upgrade_or_with_v6_support() {
        for now in [0, ARRR_IRONWOOD_ACTIVATION as u64, u64::MAX] {
            assert!(!ironwood_build_refused_at(None, IRONWOOD_V6_SUPPORTED, now));
            assert!(!ironwood_build_refused_at(Some(ARRR_IRONWOOD_ACTIVATION), true, now));
        }
    }

    /// The two gates are deliberately staggered, and the order matters: trading
    /// stops first so that no swap is still in flight when building stops. If the
    /// build gate ever moved earlier than the freeze, a swap begun just before the
    /// freeze could be unable to spend or refund itself.
    #[test]
    fn the_swap_freeze_starts_before_building_is_refused() {
        let freeze_start = ARRR_IRONWOOD_ACTIVATION as u64 - IRONWOOD_SWAP_FREEZE_MARGIN_SECS;
        let build_stop = ARRR_IRONWOOD_ACTIVATION as u64;
        assert!(freeze_start < build_stop, "trading must stop before building does");

        // In the window between them: no new swaps, but spends and refunds of
        // existing ones still build -- which is the entire purpose of the window.
        let midpoint = freeze_start + (build_stop - freeze_start) / 2;
        assert!(ironwood_swap_freeze_active_at(
            Some(ARRR_IRONWOOD_ACTIVATION),
            IRONWOOD_V6_SUPPORTED,
            IRONWOOD_SWAP_FREEZE_MARGIN_SECS,
            midpoint
        ));
        assert!(
            !ironwood_build_refused_at(Some(ARRR_IRONWOOD_ACTIVATION), IRONWOOD_V6_SUPPORTED, midpoint),
            "an in-flight swap must still be able to spend or refund during the freeze window"
        );
        // And the window is wide enough for the longest payment lock to expire.
        assert!(build_stop - freeze_start >= IRONWOOD_SWAP_FREEZE_MARGIN_SECS);
    }

    /// An activation time closer to the epoch than the margin must clamp rather
    /// than wrap. Such a time is already in the past, so the coin being frozen
    /// throughout is the correct answer -- the point is that the subtraction must
    /// not underflow into a cut-off near `u64::MAX`, which would leave the coin
    /// permanently *tradeable* right through its own upgrade.
    #[test]
    fn an_activation_time_inside_the_margin_clamps_instead_of_wrapping() {
        for now in [0, 1, ARRR_FREEZE_START] {
            assert!(
                ironwood_swap_freeze_active_at(Some(10), IRONWOOD_V6_SUPPORTED, IRONWOOD_SWAP_FREEZE_MARGIN_SECS, now),
                "an activation already in the past must freeze, not wrap (now={})",
                now
            );
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod ironwood_consensus_param_tests {
    use super::*;

    /// The ARRR mainnet `consensus_params` as published in `GLEECBTC/coins`, which
    /// carries no Ironwood keys. It must keep parsing, with both fields absent.
    fn arrr_params_without_ironwood() -> Json {
        json!({
            "overwinter_activation_height": 152855,
            "sapling_activation_height": 152855,
            "blossom_activation_height": null,
            "heartwood_activation_height": null,
            "canopy_activation_height": null,
            "coin_type": 133,
            "hrp_sapling_extended_spending_key": "secret-extended-key-main",
            "hrp_sapling_extended_full_viewing_key": "zxviews",
            "hrp_sapling_payment_address": "zs",
            "b58_pubkey_address_prefix": [0x1c, 0xb8],
            "b58_script_address_prefix": [0x1c, 0xbd]
        })
    }

    /// An older binary must not choke on a newer coin file, and a newer binary must
    /// not require one: both fields are optional and defaulted.
    #[test]
    fn ironwood_params_are_optional_in_both_directions() {
        let without: ZcoinConsensusParams = serde_json::from_value(arrr_params_without_ironwood()).unwrap();
        assert_eq!(without.ironwood_activation_time(), None);
        assert_eq!(without.ironwood_activation_height(), None);

        let mut with_time = arrr_params_without_ironwood();
        with_time["ironwood_activation_time"] = json!(1_791_054_000u32);
        let parsed: ZcoinConsensusParams = serde_json::from_value(with_time).unwrap();
        assert_eq!(parsed.ironwood_activation_time(), Some(1_791_054_000));
        assert_eq!(parsed.ironwood_activation_height(), None);

        let mut with_both = arrr_params_without_ironwood();
        with_both["ironwood_activation_time"] = json!(1_791_054_000u32);
        with_both["ironwood_activation_height"] = json!(4_141_710u32);
        let parsed: ZcoinConsensusParams = serde_json::from_value(with_both).unwrap();
        assert_eq!(parsed.ironwood_activation_time(), Some(1_791_054_000));
        assert_eq!(parsed.ironwood_activation_height(), Some(4_141_710));

        // Explicit nulls are equivalent to absence.
        let mut nulls = arrr_params_without_ironwood();
        nulls["ironwood_activation_time"] = Json::Null;
        nulls["ironwood_activation_height"] = Json::Null;
        let parsed: ZcoinConsensusParams = serde_json::from_value(nulls).unwrap();
        assert_eq!(parsed.ironwood_activation_time(), None);
        assert_eq!(parsed.ironwood_activation_height(), None);
    }

    /// A2 only carries the data. Until Step 3 maps it, no post-Sapling upgrade may
    /// report an activation height, because the transaction builder derives the
    /// consensus branch ID from exactly these lookups: a premature mapping would
    /// change the transactions this build signs.
    #[test]
    fn carrying_the_ironwood_height_does_not_yet_move_the_branch_id() {
        let mut with_height = arrr_params_without_ironwood();
        with_height["ironwood_activation_time"] = json!(1_791_054_000u32);
        with_height["ironwood_activation_height"] = json!(4_141_710u32);
        let params: ZcoinConsensusParams = serde_json::from_value(with_height).unwrap();

        for nu in [
            NetworkUpgrade::Nu5,
            NetworkUpgrade::Nu6,
            NetworkUpgrade::Nu6_1,
            NetworkUpgrade::Nu6_2,
        ] {
            assert_eq!(params.activation_height(nu), None, "{:?} must stay unmapped", nu);
        }
        assert_eq!(
            params.activation_height(NetworkUpgrade::Sapling),
            Some(BlockHeight::from_u32(152_855))
        );
        // Far above the declared Ironwood height, the branch in force is still Sapling.
        assert_eq!(
            BranchId::for_height(&params, BlockHeight::from_u32(4_200_000)),
            BranchId::Sapling
        );
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

/// Poll period for [`post_activation_shielded_sync`].
///
/// R39.8.0ab fixes no particular number; it requires a named constant that is
/// strictly positive, finite, and no larger than the coin's nominal block
/// interval, giving a latency budget of one block plus one period. Pirate's
/// nominal interval is 60 s, so half of that keeps a pass per block without
/// polling the backend harder than the chain produces work.
#[cfg(not(target_arch = "wasm32"))]
const SHIELDED_SYNC_POLL_PERIOD_SECS: f64 = 30.;

/// Keeps a Light-mode shielded wallet database current after activation
/// (CRD ch.39 §39.8.0.5).
///
/// Activation scans the wallet database once, up to whatever the chain tip was
/// at that moment. Nothing else advances it in Light mode, so without this task
/// the database stays anchored at that height for the rest of the session and
/// incoming shielded transactions stay invisible until the coin is activated
/// again. Native mode is served by its own commitment-tree task, which returns
/// early for Electrum-backed activations.
///
/// Each pass resumes from the wallet's own local sync state
/// (`requested_start_height: None` plus `skip_sync_params: true`), so it tops up
/// from the last scanned height and never re-anchors the wallet — anchor changes
/// remain activation-only per R39.8.0g/h.
#[cfg(not(target_arch = "wasm32"))]
async fn post_activation_shielded_sync(coin: ZCoin, light_wallet_d_servers: Vec<String>) {
    let ticker = coin.ticker().to_owned();
    let (utxo_weak, z_fields_weak) = coin.into_weak_parts();

    // Terminates by itself once the coin is disabled and the last strong
    // reference is dropped.
    loop {
        Timer::sleep(SHIELDED_SYNC_POLL_PERIOD_SECS).await;
        let coin = match ZCoin::from_weak_parts(&utxo_weak, &z_fields_weak) {
            Some(coin) => coin,
            None => return,
        };

        let tip = match coin.rpc_client().get_block_count().compat().await {
            Ok(tip) => tip,
            Err(e) => {
                log::warn!("ZCoin periodic sync for {ticker}: could not get block count: {e}");
                continue;
            },
        };

        if tip <= coin.z_fields.wallet_db_scanned_through.load(AtomicOrdering::Relaxed) {
            continue;
        }

        let no_progress = |_: u64, _: u64| {};
        if let Err(e) = coin
            .fetch_lightwalletd_compact_blocks_to_height(&light_wallet_d_servers, tip, None, true, &no_progress)
            .await
        {
            log::warn!("ZCoin periodic sync for {ticker}: compact block fetch to height {tip} failed: {e}");
            continue;
        }

        match coin.scan_shielded_wallet_db_to_height(tip, no_progress) {
            Ok(scanned_height) => {
                log::debug!("ZCoin periodic sync for {ticker}: wallet DB scanned through {scanned_height}");
                // Deliberately after the scan: a transaction mined between the
                // two reads is then already excluded by the scanned-state
                // check below, so its value can never be counted from both the
                // wallet database and the pending set (R39.8.0ah).
                coin.refresh_pending_receipts(&light_wallet_d_servers, tip).await;
            },
            // `scan_shielded_wallet_db_to_height` clears the scan-complete flag on
            // failure, which blocks spending until a later pass succeeds. That is
            // the intended conservative behaviour: the wallet's view of its own
            // notes is incomplete, so it must not build transactions from it.
            Err(e) => log::warn!("ZCoin periodic sync for {ticker}: wallet DB scan to height {tip} failed: {e}"),
        }
    }
}

impl ZCoin {
    /// Whether this coin must refuse to enter new swaps right now, because its
    /// Ironwood activation is near enough that a payment made today could still be
    /// awaiting a spend or refund when the upgrade lands.
    ///
    /// Not wasm-gated: the two trait methods that consult it are compiled on every
    /// target.
    /// Whether this coin must refuse to build a transaction right now, because its
    /// Ironwood upgrade has activated and this build cannot produce the format the
    /// network now requires.
    pub(crate) fn ironwood_build_refused(&self) -> bool {
        ironwood_build_refused_at(
            self.z_fields.consensus_params.ironwood_activation_time(),
            IRONWOOD_V6_SUPPORTED,
            now_ms() / 1000,
        )
    }

    fn ironwood_swap_freeze_active(&self) -> bool {
        ironwood_swap_freeze_active_at(
            self.z_fields.consensus_params.ironwood_activation_time(),
            IRONWOOD_V6_SUPPORTED,
            IRONWOOD_SWAP_FREEZE_MARGIN_SECS,
            now_ms() / 1000,
        )
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl ZCoin {
    /// Starts the periodic light-mode shielded wallet sync described on
    /// [`post_activation_shielded_sync`]. Call once, after the activation scan has
    /// completed, for Light-mode activations only.
    pub fn spawn_post_activation_shielded_sync(&self, light_wallet_d_servers: Vec<String>) {
        spawn(post_activation_shielded_sync(self.clone(), light_wallet_d_servers));
    }

    /// Re-derive the pending-receipt set from the backend's current mempool.
    ///
    /// The set is rebuilt from scratch on every poll rather than mutated, which
    /// is what makes invalidation free: a transaction that was mined, dropped,
    /// or expired simply does not reappear (R39.8.0ah). Receipts whose
    /// transaction the wallet database has already scanned are excluded here so
    /// their value is never counted from both sources.
    async fn refresh_pending_receipts(&self, light_wallet_d_servers: &[String], tip: u64) {
        let observed = self
            .z_fields
            .shielded_history
            .fetch_pending_receipts(light_wallet_d_servers, tip)
            .await;

        let mut rebuilt = HashMap::with_capacity(observed.len());
        for receipt in observed {
            match self.z_fields.shielded_history.transaction_is_scanned(&receipt.txid) {
                Ok(true) => continue,
                Ok(false) => {},
                // If we cannot tell whether it is already scanned, leave it out:
                // under-reporting a pending amount is a display gap, whereas
                // over-reporting risks showing the same value twice.
                Err(e) => {
                    log::debug!(
                        "ZCoin pending receipts for {}: scan-state check failed: {e}",
                        self.ticker()
                    );
                    continue;
                },
            }
            rebuilt.insert((receipt.txid, receipt.output_index), receipt.value);
        }

        // One assignment under the lock, so a concurrent balance query sees
        // either the whole previous set or the whole new one.
        *self.z_fields.pending_receipts.lock().unwrap() = rebuilt;
    }

    /// Total value of currently-pending wallet-owned mempool outputs, in
    /// zatoshi. Reported only as non-spendable balance (R39.8.0ag).
    pub(crate) fn pending_receipts_total(&self) -> u64 {
        self.z_fields
            .pending_receipts
            .lock()
            .map(|set| set.values().copied().fold(0u64, |acc, v| acc.saturating_add(v)))
            .unwrap_or(0)
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
            #[cfg(not(target_arch = "wasm32"))]
            pending_receipts: Mutex::new(HashMap::new()),
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
                    // Pending mempool receipts are reported here and only here
                    // (R39.8.0ag): `my_spendable_balance` reads the spendable
                    // field, so an unmined note can never size a trade or fund
                    // a swap. Native mode reaches the same result by counting
                    // its own zero-confirmation notes as unspendable.
                    return Ok(CoinBalance {
                        spendable: big_decimal_from_sat_unsigned(balance_sat, coin.decimals()),
                        unspendable: big_decimal_from_sat_unsigned(coin.pending_receipts_total(), coin.decimals()),
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
    /// Reports the coin as non-tradeable while the Ironwood swap freeze is in
    /// force, which rejects it at order placement (`buy`, `sell`, `setprice` all
    /// gate on this) with a clean error instead of letting a swap start that this
    /// build could not later spend or refund.
    ///
    /// Deliberately narrow: balance, address, `withdraw`, history and every swap
    /// already in flight are untouched, because nothing re-checks this once a swap
    /// has begun.
    fn wallet_only(&self, ctx: &MmArc) -> bool {
        if self.ironwood_swap_freeze_active() {
            return true;
        }
        let coin_conf = crate::coin_conf(ctx, self.ticker());
        coin_conf["wallet_only"].as_bool().unwrap_or(false)
    }

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
        // `wallet_only` gates the locally initiated paths (`buy`, `sell`,
        // `setprice`) but is not consulted when a remote peer matches an order we
        // already have posted. This predicate is, on both of those paths, so the
        // freeze has to be repeated here or a counterparty could still pull this
        // coin into a new swap. Declining is silent on the wire by design, hence
        // the log line.
        if self.ironwood_swap_freeze_active() {
            log::warn!(
                "{}: declining a swap match -- trading is paused ahead of the Ironwood network upgrade",
                self.ticker()
            );
            return false;
        }
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
