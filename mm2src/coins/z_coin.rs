#[cfg(not(target_arch = "wasm32"))]
use crate::rpc_command::init_withdraw::{InitWithdrawCoin, WithdrawInProgressStatus, WithdrawTaskHandle};
use crate::utxo::rpc_clients::{ElectrumRpcRequest, UnspentInfo, UtxoRpcClientEnum, UtxoRpcClientOps, UtxoRpcError,
                               UtxoRpcFut, UtxoRpcResult};
use crate::utxo::utxo_builder::{UtxoCoinBuildError, UtxoCoinBuildResult, UtxoCoinBuilderCommonOps,
                                UtxoCoinWithIguanaPrivKeyBuilder, UtxoFieldsWithIguanaPrivKeyBuilder};
use crate::utxo::utxo_common::{big_decimal_from_sat_unsigned, payment_script};
#[cfg(not(target_arch = "wasm32"))]
use crate::utxo::zcash_params_path;
use crate::utxo::{sat_from_big_decimal, utxo_common, ActualTxFee, AdditionalTxData, Address, BroadcastTxErr,
                  FeePolicy, GetUtxoListOps, HistoryUtxoTx, HistoryUtxoTxMap, MatureUnspentList,
                  RecentlySpentOutPointsGuard, UtxoActivationParams, UtxoAddressFormat, UtxoArc, UtxoCoinFields,
                  UtxoCommonOps, UtxoFeeDetails, UtxoTxBroadcastOps, UtxoTxGenerationOps, UtxoWeak,
                  VerboseTransactionFrom};
use crate::SpendSafeAmountError;
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
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, Weak};
#[cfg(not(target_arch = "wasm32"))]
use zcash_client_backend::data_api::{wallet::{input_selection::LockFilter, ConfirmationsPolicy},
                                     InputSource, SentTransaction, TargetValue, WalletCommitmentTrees, WalletRead,
                                     WalletWrite};
use zcash_client_backend::decrypt_transaction;
use zcash_keys::encoding::{decode_payment_address, encode_extended_spending_key, encode_payment_address};
#[cfg(not(target_arch = "wasm32"))]
use zcash_keys::keys::UnifiedFullViewingKey;
use zcash_primitives::merkle_tree::read_commitment_tree;
#[cfg(not(target_arch = "wasm32"))]
use zcash_primitives::transaction::builder::{BuildConfig, Builder as ZTxBuilder, BundlePadding};
#[cfg(not(target_arch = "wasm32"))]
use zcash_primitives::transaction::fees::fixed::FeeRule as FixedFeeRule;
use zcash_primitives::transaction::Transaction as ZTransaction;
use zcash_protocol::consensus::{self, BlockHeight, BranchId, NetworkType, NetworkUpgrade, Parameters, H0};
use zcash_protocol::constants::mainnet as z_mainnet_constants;
use zcash_protocol::memo::MemoBytes;
use zcash_protocol::value::Zatoshis as Amount;
#[cfg(not(target_arch = "wasm32"))]
use zcash_protocol::ShieldedPool;
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
#[cfg(all(test, not(target_arch = "wasm32")))]
mod z_htlc_live_probe;
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
use z_coin_wallet_db::{unmined_value, UnminedNote, ZCoinShieldedHistory};
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod z_lightwalletd;
#[cfg(not(target_arch = "wasm32"))]
pub use z_lightwalletd::{LightwalletdClient, LightwalletdError};

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
#[cfg(not(target_arch = "wasm32"))] mod z_electrum_fallback;
#[cfg(not(target_arch = "wasm32"))] mod z_ironwood;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod z_note_lock_tests;
#[cfg(not(target_arch = "wasm32"))] mod z_refund_gate;
#[cfg(not(target_arch = "wasm32"))] mod z_swap_ops;
#[cfg(target_arch = "wasm32")] mod z_swap_ops_wasm;
#[cfg(not(target_arch = "wasm32"))] mod z_tx_read;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod z_tx_read_fixtures;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod z_v6_construction_tests;

#[cfg(all(test, feature = "zhtlc-native-tests"))]
mod z_coin_tests;

/// Whether this build can construct the transaction format the Ironwood network
/// upgrade requires.
///
/// `true`: the shielded transaction builder emits version-6 transactions from the Ironwood
/// activation height on (R39.6.4e) and the swap-path parsers read them (R39.11.16), so the swap
/// freeze (R39.6.4b) and the build refusal (R39.6.4c) are inert. The gates and their rules are
/// kept, and tested with this flag modelled as `false`, because they are the behaviour of a build
/// that cannot construct the format. Do not set this back without also leaving the builder able to
/// emit version 4 before activation.
const IRONWOOD_V6_SUPPORTED: bool = true;

/// Pirate Chain mainnet Ironwood activation: Sat 3 Oct 2026 19:00:00 UTC.
///
/// Read from Pirate's own `KOMODO_IRONWOOD_ACTIVATION` (`src/komodo_defs.h`) and
/// confirmed by the Pirate maintainer on 2026-09-28, who also confirmed that no
/// activation *height* will be published ahead of the upgrade and that there will
/// be no standardness grace period.
const ARRR_IRONWOOD_ACTIVATION_TIME: u32 = 1_791_054_000;

/// Sapling activation height of the real ARRR chain, which identifies it among chains that present
/// the same address prefixes (see [`builtin_ironwood_activation_time`]).
const ARRR_SAPLING_ACTIVATION_HEIGHT: u32 = 152_855;

/// The Ironwood activation time this build knows for a coin, used when the coin
/// configuration does not supply one.
///
/// Every published ARRR coin configuration omits `ironwood_activation_time` — the
/// field is an extension introduced by this project, and upstream KDF has no
/// equivalent — so without a built-in value the two gates below would never engage
/// on any deployment but our own. The upgrade is a fact about the network rather
/// than an operator preference, so the value is carried here and a configuration
/// entry overrides it (R39.6.4d).
///
/// Keyed on ticker **and on the chain's identity** (R39.6.4d). A ticker alone is not enough, and
/// neither are the address prefixes: Pirate's own Ironwood test chain presents exactly as mainnet
/// (mainnet prefixes `zs` and `pirate`, `chainName` `main`) while running a different history, so
/// "mainnet-shaped parameters" does not mean "the real ARRR chain". What does differ, and cannot be
/// shared by two chains that diverge from genesis, is the Sapling activation height, so that is part
/// of the key.
fn builtin_ironwood_activation_time(ticker: &str, params: &ZcoinConsensusParams) -> Option<u32> {
    match ticker {
        "ARRR" if params.sapling_activation_height == ARRR_SAPLING_ACTIVATION_HEIGHT => {
            Some(ARRR_IRONWOOD_ACTIVATION_TIME)
        },
        _ => None,
    }
}

/// The Ironwood activation time in force for a coin: the configured value, else the
/// built-in one, unless the configuration opts out.
///
/// Precedence is configuration over built-in so that a moved activation time stays a
/// data change — the Pirate maintainer restated 3 Oct 2026 19:00 UTC as fixed, but a
/// compiled-in date that could not be overridden would refuse every transaction of
/// the coin, and freeze its market, until a new binary shipped.
///
/// The built-in value is withheld unless the parameters describe the chain it was read from
/// ([`builtin_ironwood_activation_time`]). That covers the test and regtest networks, which
/// activate Ironwood at a *height* with no timestamp at all, and also Pirate's own test chain, which
/// carries mainnet address prefixes and so cannot be told from the real chain by them.
fn effective_ironwood_activation_time(params: &ZcoinConsensusParams, ticker: &str) -> Option<u32> {
    if params.ironwood_gate_disabled() {
        return None;
    }
    configured_or_builtin_ironwood_time(params, ticker)
}

/// The Ironwood activation time used to *discover* the activation height: the configured value,
/// else the built-in one for the real chain, regardless of `ironwood_gate_disabled`. That switch
/// opts a coin out of the swap freeze and the build refusal; with v6 construction supported neither
/// exists, and a coin that opts out must still be able to build the transaction format its network
/// requires.
fn configured_or_builtin_ironwood_time(params: &ZcoinConsensusParams, ticker: &str) -> Option<u32> {
    params.ironwood_activation_time().or_else(|| {
        if matches!(params.network_type_hint(), NetworkType::Main) {
            builtin_ironwood_activation_time(ticker, params)
        } else {
            None
        }
    })
}

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
    /// When present this value is used; when absent, a built-in value for the
    /// coin's ticker applies on mainnet (R39.6.4d) — for ARRR, 3 Oct 2026
    /// 19:00 UTC. Configuration therefore *overrides* rather than *enables*, so a
    /// moved activation time remains a data change.
    ///
    /// **Compatibility:** GLEEC KDF has no equivalent and applies no upgrade
    /// gating. Because a built-in value applies when this field is absent, omitting
    /// it is *not* sufficient to match GLEEC KDF — set `ironwood_gate_disabled` to
    /// `true` for that. See `docs/GLEEC_COMPATIBILITY.md`.
    #[serde(default)]
    ironwood_activation_time: Option<u32>,
    /// Suppresses the Ironwood swap freeze and build refusal for this coin
    /// entirely, including the built-in activation time (R39.6.4d).
    ///
    /// The escape hatch for an operator who would rather meet the network's own
    /// rejection than this build's refusal — and the switch that restores
    /// GLEEC-equivalent behaviour, since simply omitting
    /// `ironwood_activation_time` no longer does so.
    ///
    /// **Compatibility:** set to `true` to match GLEEC KDF exactly. Defaults to
    /// `false`. See `docs/GLEEC_COMPATIBILITY.md`.
    #[serde(default)]
    ironwood_gate_disabled: bool,
    /// Ironwood activation height, once the network has derived and published it.
    ///
    /// Optional: the height is unknown until the upgrade is imminent, and a
    /// wallet shipping an older coin configuration will not carry it at all.
    #[serde(default)]
    ironwood_activation_height: Option<u32>,
    /// The Ironwood activation height learned at runtime (0 = not known), shared by every copy of
    /// these parameters so a height discovered after the coin was built reaches the transaction
    /// builder and the wallet database alike. Never serialized: it is a fact about the live chain,
    /// not configuration (R39.6.4e).
    #[serde(skip)]
    ironwood_discovered_height: Arc<AtomicU32>,
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

    /// The Ironwood activation height in force: the configured one, else the one discovered at
    /// runtime. `None` means the upgrade is not known to have a height yet, so the builder stays on
    /// the pre-Ironwood transaction version.
    pub(crate) fn effective_ironwood_activation_height(&self) -> Option<u32> {
        self.ironwood_activation_height
            .or_else(|| match self.ironwood_discovered_height.load(AtomicOrdering::SeqCst) {
                0 => None,
                discovered => Some(discovered),
            })
    }

    /// Records a height discovered at runtime. A configured height always wins, and zero is not a
    /// height the chain can activate at.
    pub(crate) fn record_discovered_ironwood_height(&self, height: u32) {
        if height != 0 {
            self.ironwood_discovered_height.store(height, AtomicOrdering::SeqCst);
        }
    }

    /// Whether this coin opts out of Ironwood gating altogether.
    pub(crate) fn ironwood_gate_disabled(&self) -> bool { self.ironwood_gate_disabled }

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
            // Ironwood (NU6.3) is the one post-Sapling upgrade a Pirate chain has. Until its height
            // is configured or discovered it reports `None`, which keeps the builder on the
            // Sapling-era versions; from the height on, the builder selects version 6 (R39.6.4e).
            NetworkUpgrade::Nu6_3 => self.effective_ironwood_activation_height().map(BlockHeight::from_u32),
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

/// Which backend serves a shielded coin (CRD ch.39 R39.11.4g).
///
/// The coin carries this explicitly. Every Light-only behaviour (the shielded balance source, the
/// background sync task, the pending-receipt surface, skipping the native commitment-tree
/// maintenance) is selected by it, never by which arm of the UTXO RPC client enumeration the coin
/// happens to hold: a discriminator that silently stops matching would select the Native path for a
/// Light coin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZcoinMode {
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    Native,
    Light,
}

impl ZcoinMode {
    pub fn is_light(self) -> bool { self == ZcoinMode::Light }
}

/// How a shielded coin is to reach the chain, as requested at activation.
#[derive(Clone, Debug)]
pub enum ZcoinBackend {
    /// A full node reached through its RPC. Native builds only.
    #[cfg(not(target_arch = "wasm32"))]
    Native,
    /// The configured `light_wallet_d_servers`, which serve the whole coin, and the activation's
    /// `electrum_servers`, which are kept for the one thing they are used for: the fallback of three
    /// swap-path questions (R39.11.25). No Electrum connection is opened, attempted or probed at
    /// activation (R39.11.5, R39.11.7); an empty list means there is no fallback.
    Light {
        light_wallet_d_servers: Vec<String>,
        electrum_servers: Vec<ElectrumRpcRequest>,
    },
}

impl ZcoinBackend {
    /// The Electrum servers of the activation, for the fallback of R39.11.25; none for a Native coin.
    #[cfg(not(target_arch = "wasm32"))]
    fn electrum_servers(&self) -> Vec<ElectrumRpcRequest> {
        match self {
            ZcoinBackend::Native => Vec::new(),
            ZcoinBackend::Light { electrum_servers, .. } => electrum_servers.clone(),
        }
    }

    fn mode(&self) -> ZcoinMode {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            ZcoinBackend::Native => ZcoinMode::Native,
            ZcoinBackend::Light { .. } => ZcoinMode::Light,
        }
    }
}

/// What an in-flight shielded spend has committed: the value of the notes it
/// spends and the height after which it can no longer be mined.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InFlightSpend {
    pub(crate) value: u64,
    pub(crate) expiry_height: u32,
}

/// How `send_outputs` paces its waits. The field exists for test control: production installs
/// a 600 s in-flight wait retried every 15 s (aligned with the reference's budget) and a 4000 s
/// confirmation wait polled every 10 s, through
/// [`SendPacing::default`], and tests shorten them so the bounded in-flight wait (R39.8.0at) can be
/// exercised in milliseconds.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SendPacing {
    /// Budget for a send that is short only because in-flight spends are unscanned, in seconds.
    pub(crate) in_flight_wait_secs: u64,
    /// Gap between re-selection attempts inside that budget, in seconds.
    pub(crate) in_flight_retry_secs: f64,
    /// How long a broadcast transaction is awaited for its first confirmation, in seconds.
    pub(crate) confirmation_wait_secs: u64,
    /// Gap between confirmation checks, in seconds.
    pub(crate) confirmation_check_secs: u64,
}

pub struct ZCoinFields {
    mode: ZcoinMode,
    dex_fee_addr: PaymentAddress,
    my_z_addr: PaymentAddress,
    my_z_addr_encoded: String,
    z_spending_key: ExtendedSpendingKey,
    /// Transaction prover: loads the sapling spend/output parameters.
    /// Native only — WASM cannot build shielded transactions (no param files).
    #[cfg(not(target_arch = "wasm32"))]
    z_tx_prover: LocalTxProver,
    /// Serializes note selection, construction, broadcast and in-flight recording
    /// into one critical section per shielded wallet (CRD ch.39 R39.8.0at).
    z_unspent_mutex: AsyncMutex<()>,
    /// The notes each transaction this process broadcast, or built for a caller to
    /// broadcast (including a withdrawal the caller submits, D39.8.0e), committed, keyed by txid, retained until
    /// the transaction is observed as scanned or its expiry height has passed. The
    /// authoritative exclusion is the wallet database record written by R39.8.0ap;
    /// this map exists only to tell a shortfall that an in-flight spend will clear
    /// from one that no amount of waiting can fix, so a request that is unfundable
    /// even with every note unexcluded fails at once instead of consuming the wait
    /// budget (R39.8.0at), and to name the in-flight spend when a withdrawal is
    /// refused for it. Being process-local costs nothing after a restart: the map
    /// is empty, so a shortfall simply fails fast.
    #[cfg(not(target_arch = "wasm32"))]
    in_flight_spends: Mutex<HashMap<[u8; 32], InFlightSpend>>,
    /// The waits `send_outputs` makes; see [`SendPacing`].
    #[cfg(not(target_arch = "wasm32"))]
    send_pacing: SendPacing,
    /// The Electrum fallback of the three swap-path questions of a Light-mode coin (CRD ch.39 R39.11.25).
    /// Holds the activation's `electrum_servers` and connects to none of them until a question needs it.
    #[cfg(not(target_arch = "wasm32"))]
    electrum_fallback: z_electrum_fallback::ElectrumFallback,
    /// Message from the most recent shielded sync failure, cleared by the next
    /// successful scan. Without it a failed scan is reported as a scan still in
    /// progress, because both leave `wallet_db_scan_complete` false (R39.8.0au).
    #[cfg(not(target_arch = "wasm32"))]
    shielded_sync_error: Mutex<Option<String>>,
    sapling_state_synced: AtomicBool,
    /// Message from the most recent failure of the commitment-tree sync loop (the chain tip or a
    /// block could not be read), cleared as soon as the loop reads from the backend again. It
    /// lets activation report the failure as a retriable `TemporaryError` instead of silently
    /// waiting (CRD ch.39 R39.10.14). Kept apart from `shielded_sync_error`, which describes the
    /// wallet-database scan and drives the reported history-sync state.
    sapling_loop_error: Mutex<Option<String>>,
    /// Set once the commitment-tree sync loop has ended, whether it finished, bailed out on bad
    /// data or panicked. A stopped loop that never reported completion is what activation reports
    /// as an `Internal` error (CRD ch.39 R39.10.14, "shielded sync process stops").
    sapling_loop_stopped: Arc<AtomicBool>,
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
    last_sync_error: Option<&str>,
) -> HistorySyncState {
    // Reported ahead of the scan-completion check, because a failed scan also
    // leaves the scan incomplete: without this the two are indistinguishable and
    // a wallet that can no longer sync looks like one that is still catching up
    // (R39.8.0au).
    if let Some(message) = last_sync_error {
        return HistorySyncState::Error(json!({
            "code": 0,
            "message": message,
            "scanned_through": wallet_db_scanned_through
        }));
    }

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

#[derive(Clone)]
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

    /// What the gate functions are given to model a build that cannot construct version 6. The
    /// production build can, so the gates never engage in it (`IRONWOOD_V6_SUPPORTED`); these tests
    /// pin the rules for the build the gates exist for.
    const BUILD_WITHOUT_V6: bool = false;

    /// Pirate mainnet Ironwood activation: Sat 3 Oct 2026 19:00:00 UTC.
    ///
    /// Aliases the production constant so the two cannot drift.
    const ARRR_IRONWOOD_ACTIVATION: u32 = ARRR_IRONWOOD_ACTIVATION_TIME;
    /// The moment the freeze engages for that activation: 1 Oct 2026 23:40:00 UTC.
    const ARRR_FREEZE_START: u64 = ARRR_IRONWOOD_ACTIVATION as u64 - IRONWOOD_SWAP_FREEZE_MARGIN_SECS;

    fn frozen_at(now_sec: u64) -> bool {
        ironwood_swap_freeze_active_at(
            Some(ARRR_IRONWOOD_ACTIVATION),
            BUILD_WITHOUT_V6,
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
                BUILD_WITHOUT_V6,
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
        let refused = |now| ironwood_build_refused_at(Some(ARRR_IRONWOOD_ACTIVATION), BUILD_WITHOUT_V6, now);
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
            assert!(!ironwood_build_refused_at(None, BUILD_WITHOUT_V6, now));
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
            BUILD_WITHOUT_V6,
            IRONWOOD_SWAP_FREEZE_MARGIN_SECS,
            midpoint
        ));
        assert!(
            !ironwood_build_refused_at(Some(ARRR_IRONWOOD_ACTIVATION), BUILD_WITHOUT_V6, midpoint),
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
                ironwood_swap_freeze_active_at(Some(10), BUILD_WITHOUT_V6, IRONWOOD_SWAP_FREEZE_MARGIN_SECS, now),
                "an activation already in the past must freeze, not wrap (now={})",
                now
            );
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod ironwood_consensus_param_tests {
    use super::*;

    /// See `ironwood_swap_freeze_tests::BUILD_WITHOUT_V6`.
    const BUILD_WITHOUT_V6: bool = false;

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

    /// Every published ARRR coin configuration omits `ironwood_activation_time`, so
    /// without a built-in value neither gate would ever engage in production. The
    /// built-in one must arm them from the coin file as it actually ships.
    #[test]
    fn builtin_activation_time_arms_the_gates_when_the_coin_file_omits_it() {
        let params: ZcoinConsensusParams = serde_json::from_value(arrr_params_without_ironwood()).unwrap();
        // The field really is absent — the point of the test.
        assert_eq!(params.ironwood_activation_time(), None);

        let effective = effective_ironwood_activation_time(&params, "ARRR");
        assert_eq!(effective, Some(ARRR_IRONWOOD_ACTIVATION_TIME));

        // And the gates fire off it, on the same schedule as a configured value.
        let freeze_start = ARRR_IRONWOOD_ACTIVATION_TIME as u64 - IRONWOOD_SWAP_FREEZE_MARGIN_SECS;
        assert!(!ironwood_swap_freeze_active_at(
            effective,
            BUILD_WITHOUT_V6,
            IRONWOOD_SWAP_FREEZE_MARGIN_SECS,
            freeze_start - 1,
        ));
        assert!(ironwood_swap_freeze_active_at(
            effective,
            BUILD_WITHOUT_V6,
            IRONWOOD_SWAP_FREEZE_MARGIN_SECS,
            freeze_start,
        ));
        assert!(!ironwood_build_refused_at(
            effective,
            BUILD_WITHOUT_V6,
            ARRR_IRONWOOD_ACTIVATION_TIME as u64 - 1
        ));
        assert!(ironwood_build_refused_at(
            effective,
            BUILD_WITHOUT_V6,
            ARRR_IRONWOOD_ACTIVATION_TIME as u64
        ));
    }

    /// Configuration overrides the built-in, so a moved activation time stays a data
    /// change rather than a release.
    #[test]
    fn configured_activation_time_overrides_the_builtin() {
        let moved = ARRR_IRONWOOD_ACTIVATION_TIME + 86_400;
        let mut cfg = arrr_params_without_ironwood();
        cfg["ironwood_activation_time"] = json!(moved);
        let params: ZcoinConsensusParams = serde_json::from_value(cfg).unwrap();

        assert_eq!(effective_ironwood_activation_time(&params, "ARRR"), Some(moved));
        // The built-in must not win, or a delayed upgrade could not be followed.
        assert_ne!(
            effective_ironwood_activation_time(&params, "ARRR"),
            Some(ARRR_IRONWOOD_ACTIVATION_TIME)
        );
    }

    /// The opt-out that restores GLEEC-equivalent behaviour. It must beat both the
    /// built-in value and an explicitly configured one, otherwise an operator who
    /// asked for no gating would still be gated.
    #[test]
    fn ironwood_gate_disabled_suppresses_both_the_builtin_and_a_configured_time() {
        let mut off = arrr_params_without_ironwood();
        off["ironwood_gate_disabled"] = json!(true);
        let params: ZcoinConsensusParams = serde_json::from_value(off).unwrap();
        assert!(params.ironwood_gate_disabled());
        assert_eq!(effective_ironwood_activation_time(&params, "ARRR"), None);

        let mut off_with_time = arrr_params_without_ironwood();
        off_with_time["ironwood_gate_disabled"] = json!(true);
        off_with_time["ironwood_activation_time"] = json!(ARRR_IRONWOOD_ACTIVATION_TIME);
        let params: ZcoinConsensusParams = serde_json::from_value(off_with_time).unwrap();
        assert_eq!(effective_ironwood_activation_time(&params, "ARRR"), None);

        // Defaulting matters as much as the switch: an existing coin file that says
        // nothing must not be read as opting out.
        let silent: ZcoinConsensusParams = serde_json::from_value(arrr_params_without_ironwood()).unwrap();
        assert!(!silent.ironwood_gate_disabled());
    }

    /// The built-in value is keyed on ticker, so a shielded coin with no Ironwood
    /// upgrade must not inherit Pirate's activation time from ARRR-shaped parameters.
    #[test]
    fn builtin_applies_only_to_known_tickers() {
        let params: ZcoinConsensusParams = serde_json::from_value(arrr_params_without_ironwood()).unwrap();
        for ticker in ["ZOMBIE", "ARRR-segwit", "arrr", "", "KMD"] {
            assert_eq!(
                effective_ironwood_activation_time(&params, ticker),
                None,
                "{ticker} must not inherit Pirate's activation time"
            );
        }
    }

    /// Pirate's test and regtest networks activate Ironwood at a *height* with no
    /// timestamp (testnet at 297), so the mainnet timestamp describes neither. A
    /// build-time default must not gate them off a value that does not apply.
    #[test]
    fn builtin_is_withheld_from_test_and_regtest_parameters() {
        use zcash_protocol::constants::{regtest, testnet};

        for (hrp_addr, hrp_key) in [
            (
                testnet::HRP_SAPLING_PAYMENT_ADDRESS,
                testnet::HRP_SAPLING_EXTENDED_SPENDING_KEY,
            ),
            (
                regtest::HRP_SAPLING_PAYMENT_ADDRESS,
                regtest::HRP_SAPLING_EXTENDED_SPENDING_KEY,
            ),
        ] {
            let mut cfg = arrr_params_without_ironwood();
            cfg["hrp_sapling_payment_address"] = json!(hrp_addr);
            cfg["hrp_sapling_extended_spending_key"] = json!(hrp_key);
            let params: ZcoinConsensusParams = serde_json::from_value(cfg).unwrap();
            assert!(!matches!(params.network_type_hint(), NetworkType::Main));
            assert_eq!(
                effective_ironwood_activation_time(&params, "ARRR"),
                None,
                "{hrp_addr} parameters must not take the mainnet built-in"
            );
        }

        // An explicit configured value still applies on those networks — the
        // withholding is of the *default*, not of the feature.
        let mut cfg = arrr_params_without_ironwood();
        cfg["hrp_sapling_payment_address"] = json!(testnet::HRP_SAPLING_PAYMENT_ADDRESS);
        cfg["hrp_sapling_extended_spending_key"] = json!(testnet::HRP_SAPLING_EXTENDED_SPENDING_KEY);
        cfg["ironwood_activation_time"] = json!(1_700_000_000u32);
        let params: ZcoinConsensusParams = serde_json::from_value(cfg).unwrap();
        assert_eq!(effective_ironwood_activation_time(&params, "ARRR"), Some(1_700_000_000));
    }

    /// The refusal is a condition the user resolves by upgrading, not a server
    /// fault, so it must not reach a caller as an internal error / HTTP 500 --
    /// which is how it first shipped. After 3 Oct every ARRR withdrawal takes this
    /// path, and a GUI showing "Internal error" would read as a broken wallet
    /// rather than an actionable prompt.
    #[test]
    fn the_ironwood_refusal_is_reported_as_unsupported_not_as_an_internal_error() {
        use crate::lp_coins_errors::WithdrawError;
        use common::HttpStatusCode;

        let refusal = GenTxError::IronwoodUpgradeUnsupported {
            coin: "ARRR".to_owned(),
            activation_time: ARRR_IRONWOOD_ACTIVATION_TIME,
        };
        let message = refusal.to_string();
        let withdraw: WithdrawError = refusal.into();

        assert!(
            matches!(withdraw, WithdrawError::UnsupportedAfterNetworkUpgrade(_)),
            "expected UnsupportedAfterNetworkUpgrade, got {withdraw:?}"
        );
        assert_eq!(withdraw.status_code(), common::StatusCode::BAD_REQUEST);
        // The reason must survive the conversion: it names the coin and the time.
        let reported = withdraw.to_string();
        assert!(reported.contains("ARRR"), "{reported}");
        assert!(
            reported.contains(&ARRR_IRONWOOD_ACTIVATION_TIME.to_string()),
            "{reported}"
        );
        assert!(reported.contains(&message), "{reported}");
    }

    /// Step E binds `Nu6_3` to the Ironwood height, and only that upgrade: the unused Zcash
    /// upgrades (Nu5 .. Nu6_2) stay unmapped, because the builder derives the consensus branch ID
    /// from exactly these lookups and Pirate has none of them. Below the height the branch is
    /// Sapling's, from it Ironwood's.
    #[test]
    fn the_ironwood_height_moves_the_branch_id_and_nothing_else_does() {
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
        assert_eq!(
            params.activation_height(NetworkUpgrade::Nu6_3),
            Some(BlockHeight::from_u32(4_141_710))
        );
        assert_eq!(
            BranchId::for_height(&params, BlockHeight::from_u32(4_141_709)),
            BranchId::Sapling
        );
        assert_eq!(
            BranchId::for_height(&params, BlockHeight::from_u32(4_141_710)),
            BranchId::Nu6_3
        );

        // The activation *time* alone moves nothing: it is data for discovery, not a height.
        let mut time_only = arrr_params_without_ironwood();
        time_only["ironwood_activation_time"] = json!(1_791_054_000u32);
        let time_only: ZcoinConsensusParams = serde_json::from_value(time_only).unwrap();
        assert_eq!(time_only.activation_height(NetworkUpgrade::Nu6_3), None);
        assert_eq!(
            BranchId::for_height(&time_only, BlockHeight::from_u32(4_200_000)),
            BranchId::Sapling
        );
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod shielded_history_status_tests {
    use super::*;

    #[test]
    fn unscanned_wallet_db_does_not_report_finished_even_if_sapling_cache_synced() {
        let status = z_coin_history_sync_status(false, 10, true, None);
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
            z_coin_history_sync_status(true, 10, true, None),
            HistorySyncState::Finished
        ));

        let status = z_coin_history_sync_status(true, 10, false, None);
        match status {
            HistorySyncState::InProgress(info) => assert_eq!(info["type"], "sapling_state_cache_scan"),
            HistorySyncState::Finished => panic!("sapling cache completion is still required"),
            other => panic!("unexpected status {:?}", other),
        }
    }

    /// A failed scan and a scan still running both leave the wallet database
    /// incomplete, so without the recorded failure the two report identically and
    /// a wallet that can no longer sync is indistinguishable from one catching up
    /// (R39.8.0au).
    #[test]
    fn a_failed_sync_is_reported_as_error_not_as_progress() {
        let status = z_coin_history_sync_status(false, 10, true, Some("all lightwalletd servers failed"));
        match status {
            HistorySyncState::Error(info) => {
                assert_eq!(info["message"], "all lightwalletd servers failed");
                assert_eq!(info["scanned_through"], 10);
            },
            other => panic!("a failed sync must not be reported as {:?}", other),
        }
    }

    /// The failure is reported even once the scan flag has been set, because a
    /// periodic sync can fail after the initial scan already completed.
    #[test]
    fn a_failure_after_a_completed_scan_still_reports_error() {
        assert!(matches!(
            z_coin_history_sync_status(true, 10, true, Some("could not reach the chain tip")),
            HistorySyncState::Error(_)
        ));
    }

    /// A later successful scan clears the record, so the state returns to normal
    /// rather than latching on the first failure.
    #[test]
    fn clearing_the_failure_restores_the_normal_state() {
        assert!(matches!(
            z_coin_history_sync_status(true, 10, true, None),
            HistorySyncState::Finished
        ));
    }
}

/// CRD ch.39 §39.11: a Light-mode coin is built with lightwalletd as its only backend (T39.11.a,
/// T39.11.b, R39.11.4g).
#[cfg(all(test, not(target_arch = "wasm32")))]
mod light_backend_tests {
    use super::*;
    use common::block_on;
    use mm2_core::mm_ctx::MmCtxBuilder;

    fn arrr_protocol_info() -> ZcoinProtocolInfo {
        serde_json::from_value(json!({
            "consensus_params": {
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
            },
            "z_derivation_path": "m/32'/133'"
        }))
        .unwrap()
    }

    fn builder_for<'a>(
        ctx: &'a MmArc,
        conf: &'a Json,
        params: &'a UtxoActivationParams,
        backend: ZcoinBackend,
    ) -> ZCoinBuilder<'a> {
        ZCoinBuilder::new(
            ctx,
            "ARRR",
            conf,
            params,
            &[1; 32],
            PathBuf::from("./for_tests"),
            None,
            ExtendedSpendingKey::master(&[7; 32]),
            arrr_protocol_info(),
            backend,
        )
    }

    /// The request still carries Electrum servers (deployed wallets send them unconditionally), but
    /// the client built for the coin is the lightwalletd one: nothing in the build path can open an
    /// Electrum connection for a ZHTLC coin.
    #[test]
    fn a_light_coin_is_built_on_lightwalletd_whatever_the_request_says_about_electrum() {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        let conf = json!({ "coin": "ARRR", "txfee": 10000 });
        let params = UtxoActivationParams::from_legacy_req(&json!({
            "method": "electrum",
            "servers": [{ "url": "unreachable.invalid:10008" }]
        }))
        .unwrap();
        assert!(matches!(params.mode, crate::utxo::UtxoRpcMode::Electrum { .. }));

        let backend = ZcoinBackend::Light {
            light_wallet_d_servers: vec!["https://lightd1.example:443".to_owned()],
            electrum_servers: Vec::new(),
        };
        assert!(backend.mode().is_light());
        let builder = builder_for(&ctx, &conf, &params, backend);
        let client = block_on(builder.rpc_client()).unwrap();
        assert!(matches!(client, UtxoRpcClientEnum::LightWalletD(_)), "{client:?}");

        // A fixed fee is declared, so it is kept.
        let fee = block_on(builder.tx_fee(&client)).unwrap();
        assert!(matches!(fee, crate::utxo::TxFee::FixedPerKb(10000)));
    }

    /// T39.11.p / R39.11.5, R39.11.25(e): the activation keeps the Electrum servers for the fallback and
    /// opens no connection to them. A listening server sees no connection while the coin's client is
    /// built and while the fallback is made from the activation's list.
    #[test]
    fn a_light_coin_with_electrum_servers_opens_no_electrum_connection_at_activation() {
        use crate::utxo::rpc_clients::electrum_stub_server::{StubBehavior, StubServer};
        let ctx = MmCtxBuilder::default().into_mm_arc();
        let conf = json!({ "coin": "ARRR", "txfee": 10000 });
        let params = UtxoActivationParams::from_legacy_req(&json!({ "method": "enable" })).unwrap();
        block_on(async {
            let listening = StubServer::start(StubBehavior::Answer).await;
            let unreachable = ElectrumRpcRequest {
                url: "127.0.0.1:1".to_owned(),
                protocol: Default::default(),
                disable_cert_verification: false,
                timeout_sec: None,
            };
            let backend = ZcoinBackend::Light {
                light_wallet_d_servers: vec!["https://lightd1.example:443".to_owned()],
                electrum_servers: vec![listening.request(), unreachable],
            };
            assert_eq!(backend.electrum_servers().len(), 2, "the list is kept as listed");
            let fallback =
                z_electrum_fallback::ElectrumFallback::for_activation(ctx.weak(), "ARRR", backend.electrum_servers());
            let builder = builder_for(&ctx, &conf, &params, backend);
            let client = builder
                .rpc_client()
                .await
                .expect("activation does not depend on Electrum");
            assert!(matches!(client, UtxoRpcClientEnum::LightWalletD(_)), "{client:?}");

            assert_eq!(listening.accepted(), 0, "no connection was opened");
            drop(fallback);
            assert_eq!(listening.accepted(), 0);
        });
    }

    /// The published mainnet ARRR entry has no `txfee`: it must build and keep the shared fixed
    /// default (no estimation is involved).
    #[test]
    fn a_light_coin_with_no_txfee_key_keeps_the_fixed_default() {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        let params = UtxoActivationParams::from_legacy_req(&json!({ "method": "enable" })).unwrap();
        let conf = json!({ "coin": "ARRR" });
        let backend = ZcoinBackend::Light {
            light_wallet_d_servers: vec!["https://lightd1.example:443".to_owned()],
            electrum_servers: Vec::new(),
        };
        let builder = builder_for(&ctx, &conf, &params, backend);
        let client = block_on(builder.rpc_client()).unwrap();
        let fee = block_on(builder.tx_fee(&client)).unwrap();
        assert!(matches!(fee, crate::utxo::TxFee::FixedPerKb(1000)));
    }

    /// lightwalletd has no fee estimation: an explicit request for it (`txfee: 0`) is refused.
    #[test]
    fn a_light_coin_requesting_fee_estimation_is_refused() {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        let params = UtxoActivationParams::from_legacy_req(&json!({ "method": "enable" })).unwrap();
        for conf in [json!({ "coin": "ARRR", "txfee": 0 })] {
            let backend = ZcoinBackend::Light {
                light_wallet_d_servers: vec!["https://lightd1.example:443".to_owned()],
                electrum_servers: Vec::new(),
            };
            let builder = builder_for(&ctx, &conf, &params, backend);
            let client = block_on(builder.rpc_client()).unwrap();
            let err = block_on(builder.tx_fee(&client)).unwrap_err();
            assert!(
                matches!(err.get_inner(), UtxoCoinBuildError::NoFixedFeeDeclared { .. }),
                "{err:?}"
            );
        }
    }

    #[test]
    fn a_light_coin_without_a_lightwalletd_endpoint_is_not_built() {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        let conf = json!({ "coin": "ARRR", "txfee": 10000 });
        let params = UtxoActivationParams::from_legacy_req(&json!({ "method": "enable" })).unwrap();
        let builder = builder_for(&ctx, &conf, &params, ZcoinBackend::Light {
            light_wallet_d_servers: Vec::new(),
            electrum_servers: Vec::new(),
        });
        let err = block_on(builder.rpc_client()).unwrap_err();
        assert!(
            matches!(err.get_inner(), UtxoCoinBuildError::NoLightwalletdServers { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn the_mode_is_carried_by_the_backend_and_not_inferred_from_the_client() {
        assert_eq!(ZcoinBackend::Native.mode(), ZcoinMode::Native);
        assert!(!ZcoinBackend::Native.mode().is_light());
        assert!(ZcoinBackend::Light {
            light_wallet_d_servers: vec!["https://lightd1.example:443".to_owned()],
            electrum_servers: Vec::new(),
        }
        .mode()
        .is_light());
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
    backend: ZcoinBackend,
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
        backend,
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

/// Marks the commitment-tree sync loop as ended when dropped (normal return, early exit or panic).
struct SaplingLoopStoppedGuard(Arc<AtomicBool>);

impl Drop for SaplingLoopStoppedGuard {
    fn drop(&mut self) { self.0.store(true, AtomicOrdering::SeqCst); }
}

async fn sapling_state_cache_loop(coin: ZCoin) {
    // A Light coin has no commitment-tree cache to maintain: its tree comes from lightwalletd and
    // its notes from the shielded wallet database. Decided by the coin's explicit mode, not by the
    // identity of its RPC client (R39.11.4g).
    if coin.is_light() {
        log::debug!(
            "Light-mode ZCoin legacy commitment-tree cache loop skipped for {}; \
             wallet-history scanning is handled by the modern lightwalletd wallet scanner",
            coin.ticker()
        );
        coin.z_fields.sapling_state_synced.store(true, AtomicOrdering::Relaxed);
        return;
    }
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
        // Light-mode activations and is not the lightwalletd scan path.
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
                coin.set_sapling_loop_error(format!("could not get the block count: {e}"));
                Timer::sleep(10.).await;
                continue;
            },
        };
        coin.clear_sapling_loop_error();

        let native_client = match coin.rpc_client() {
            UtxoRpcClientEnum::Native(n) => n,
            other => {
                log::error!(
                    "ZCoin {} is in Native mode but holds a {:?} RPC client; the commitment-tree loop cannot run",
                    coin.ticker(),
                    other
                );
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
                        coin.set_sapling_loop_error(format!("could not get block {processed_height}: {e}"));
                        Timer::sleep(1.).await;
                        continue;
                    },
                };
                coin.clear_sapling_loop_error();
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
                        // The Zcash reader, not the shared UTXO parser: the latter misreads version 5 and 6,
                        // and a misread here would corrupt the commitment tree (R39.11.16a).
                        let tx = ZTransaction::read(tx.as_slice(), BranchId::Sapling)
                            .expect("Panic here to avoid invalid tree state");
                        let cmus_of_tx = tx
                            .sapling_bundle()
                            .map(|bundle| {
                                bundle
                                    .shielded_outputs()
                                    .iter()
                                    .map(|output| output.cmu().to_bytes())
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default();
                        for cmu in cmus_of_tx {
                            let node = match Option::from(Node::from_bytes(cmu)) {
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
                            cmus.push(H256::from(cmu));
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
/// early for Light-mode activations.
///
/// Each pass resumes from the wallet's own local sync state
/// (`requested_start_height: None` plus `skip_sync_params: true`), so it tops up
/// from the last scanned height and never re-anchors the wallet — anchor changes
/// remain activation-only per R39.8.0g/h.
#[cfg(not(target_arch = "wasm32"))]
async fn post_activation_shielded_sync(coin: ZCoin) {
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
                coin.set_shielded_sync_error(&format!("could not reach the chain tip: {e}"));
                continue;
            },
        };

        if tip <= coin.z_fields.wallet_db_scanned_through.load(AtomicOrdering::Relaxed) {
            continue;
        }

        let no_progress = |_: u64, _: u64| {};
        if let Err(e) = coin
            .fetch_lightwalletd_compact_blocks_to_height(tip, None, true, &no_progress)
            .await
        {
            log::warn!("ZCoin periodic sync for {ticker}: compact block fetch to height {tip} failed: {e}");
            coin.set_shielded_sync_error(&format!("compact block fetch to height {tip} failed: {e}"));
            continue;
        }

        match coin.scan_shielded_wallet_db_to_height(tip, no_progress) {
            Ok(scanned_height) => {
                log::debug!("ZCoin periodic sync for {ticker}: wallet DB scanned through {scanned_height}");
                // Deliberately after the scan: a transaction mined between the
                // two reads is then already excluded by the scanned-state
                // check below, so its value can never be counted from both the
                // wallet database and the pending set (R39.8.0ah).
                coin.refresh_pending_receipts(tip).await;
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
            self.effective_ironwood_activation_time(),
            IRONWOOD_V6_SUPPORTED,
            now_ms() / 1000,
        )
    }

    fn ironwood_swap_freeze_active(&self) -> bool {
        ironwood_swap_freeze_active_at(
            self.effective_ironwood_activation_time(),
            IRONWOOD_V6_SUPPORTED,
            IRONWOOD_SWAP_FREEZE_MARGIN_SECS,
            now_ms() / 1000,
        )
    }

    /// The Ironwood activation time in force for this coin — configured value,
    /// else the built-in one for its ticker, unless gating is switched off.
    pub(crate) fn effective_ironwood_activation_time(&self) -> Option<u32> {
        effective_ironwood_activation_time(&self.z_fields.consensus_params, &self.utxo_arc.conf.ticker)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl ZCoin {
    /// Starts the periodic light-mode shielded wallet sync described on
    /// [`post_activation_shielded_sync`]. Call once, after the activation scan has
    /// completed, for Light-mode activations only.
    pub fn spawn_post_activation_shielded_sync(&self) { spawn(post_activation_shielded_sync(self.clone())); }

    /// Re-derive the pending-receipt set from the backend's current mempool.
    ///
    /// The set is rebuilt from scratch on every poll rather than mutated, which
    /// is what makes invalidation free: a transaction that was mined, dropped,
    /// or expired simply does not reappear (R39.8.0ah). Receipts whose
    /// transaction the wallet database has already scanned are excluded here so
    /// their value is never counted from both sources.
    async fn refresh_pending_receipts(&self, tip: u64) {
        // The pending view is advisory and degrades to zero (R39.8.0ai): with no admitted endpoint
        // there is nothing to poll.
        let servers = match self.admitted_lightwalletd_servers().await {
            Ok(servers) => servers,
            Err(e) => {
                log::debug!("ZCoin pending receipts for {}: {e}", self.ticker());
                Vec::new()
            },
        };
        let observed = self
            .z_fields
            .shielded_history
            .fetch_pending_receipts(&servers, tip)
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

    /// The non-spendable balance, in zatoshi: the pending wallet-owned mempool outputs
    /// (R39.8.0ag) plus the value of `unmined_notes`, the wallet's own unmined outputs the wallet
    /// database holds (R39.8.0ag1). A pending receipt for an output that is also one of
    /// `unmined_notes` is not added again, so one output is counted once.
    ///
    /// The pending view is advisory and degrades to zero rather than failing the balance
    /// (R39.8.0ai): if its lock is poisoned the pending part is 0, while the unmined notes, which
    /// come from the wallet database, are still counted. An arithmetic overflow is an error.
    pub(crate) fn unspendable_total(&self, unmined_notes: &[UnminedNote]) -> Result<u64, String> {
        let pending_only = match self.z_fields.pending_receipts.lock() {
            Ok(pending) => pending
                .iter()
                .filter(|((txid, output_index), _)| {
                    !unmined_notes
                        .iter()
                        .any(|note| note.txid == *txid && note.output_index == *output_index)
                })
                .try_fold(0u64, |total, (_, value)| total.checked_add(*value))
                .ok_or_else(|| "Pending shielded receipts overflow".to_owned())?,
            Err(e) => {
                log::warn!(
                    "ZCoin pending receipts for {}: lock poisoned ({e}), reporting the pending amount as zero",
                    self.ticker()
                );
                0
            },
        };
        unmined_value(unmined_notes)?
            .checked_add(pending_only)
            .ok_or_else(|| "Shielded non-spendable balance overflow".to_owned())
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
    backend: ZcoinBackend,
}

#[async_trait]
impl<'a> UtxoCoinBuilderCommonOps for ZCoinBuilder<'a> {
    fn ctx(&self) -> &MmArc { self.ctx }

    fn conf(&self) -> &Json { self.conf }

    fn activation_params(&self) -> &UtxoActivationParams { self.params }

    fn ticker(&self) -> &str { self.ticker }

    /// The RPC client is selected by the requested backend, never by `activation_params().mode`: a
    /// Light-mode coin is served by lightwalletd alone and no Electrum connection is built for it
    /// (R39.11.1, R39.11.5).
    async fn rpc_client(&self) -> UtxoCoinBuildResult<UtxoRpcClientEnum> {
        match &self.backend {
            #[cfg(not(target_arch = "wasm32"))]
            ZcoinBackend::Native => Ok(UtxoRpcClientEnum::Native(self.native_client()?)),
            ZcoinBackend::Light {
                light_wallet_d_servers, ..
            } => {
                #[cfg(target_arch = "wasm32")]
                {
                    let _ = light_wallet_d_servers;
                    MmError::err(UtxoCoinBuildError::NativeRpcNotSupportedInWasm)
                }
                #[cfg(not(target_arch = "wasm32"))]
                {
                    if light_wallet_d_servers.is_empty() {
                        return MmError::err(UtxoCoinBuildError::NoLightwalletdServers {
                            ticker: self.ticker.to_owned(),
                        });
                    }
                    Ok(UtxoRpcClientEnum::LightWalletD(LightwalletdClient::connect(
                        light_wallet_d_servers.clone(),
                        &self.protocol_info.consensus_params,
                        configured_or_builtin_ironwood_time(&self.protocol_info.consensus_params, self.ticker),
                        z_lightwalletd::HtlcAddressing {
                            p2sh_prefix: self.p2sh_address_prefix(),
                            p2sh_t_prefix: self.conf()["taddr"].as_u64().unwrap_or(0) as u8,
                            pubkey_prefix: self.pub_addr_prefix(),
                            pubkey_t_prefix: self.conf()["taddr"].as_u64().unwrap_or(0) as u8,
                            // Every shielded-coin family is double-SHA256.
                            checksum_type: kdf_crypto::ChecksumType::DSHA256,
                        },
                    )))
                }
            },
        }
    }
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
            mode: self.backend.mode(),
            dex_fee_addr,
            my_z_addr,
            my_z_addr_encoded,
            z_spending_key: self.z_spending_key,
            #[cfg(not(target_arch = "wasm32"))]
            z_tx_prover,
            z_unspent_mutex: AsyncMutex::new(()),
            #[cfg(not(target_arch = "wasm32"))]
            in_flight_spends: Mutex::new(HashMap::new()),
            #[cfg(not(target_arch = "wasm32"))]
            send_pacing: SendPacing::default(),
            #[cfg(not(target_arch = "wasm32"))]
            electrum_fallback: z_electrum_fallback::ElectrumFallback::for_activation(
                self.ctx.weak(),
                self.ticker,
                self.backend.electrum_servers(),
            ),
            #[cfg(not(target_arch = "wasm32"))]
            shielded_sync_error: Mutex::new(None),
            sapling_state_synced: AtomicBool::new(false),
            sapling_loop_error: Mutex::new(None),
            sapling_loop_stopped: Arc::new(AtomicBool::new(false)),
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
        // The Light backend decides which endpoints may be used from the chain the wallet already
        // follows, so it needs the wallet database (R39.11.7a).
        #[cfg(not(target_arch = "wasm32"))]
        if let UtxoRpcClientEnum::LightWalletD(client) = z_coin.rpc_client() {
            client
                .backend()
                .bind_wallet(Arc::new(z_lightwalletd::FileBranchStore::new(
                    (*z_coin.z_fields.shielded_history).clone(),
                )));
        }
        let loop_stopped = z_coin.z_fields.sapling_loop_stopped.clone();
        let loop_coin = z_coin.clone();
        spawn(async move {
            // Drop guard: the flag is also set when the loop panics.
            let _stopped = SaplingLoopStoppedGuard(loop_stopped);
            sapling_state_cache_loop(loop_coin).await;
        });
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
        backend: ZcoinBackend,
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
            backend,
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
    backend: ZcoinBackend,
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
        backend,
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
                if coin.is_light() {
                    if !coin.shielded_wallet_db_scan_complete() {
                        log::warn!(
                            "ZCoin light-mode balance requested before shielded wallet DB scan completed for {}",
                            coin.ticker()
                        );
                    }
                    let scanned = coin
                        .shielded_history()
                        .scanned_balance(coin.z_fields.consensus_params.clone())
                        .map_err(|e| MmError::new(crate::BalanceError::Internal(e)))?;
                    // Pending mempool receipts (R39.8.0ag) and the unmined notes of
                    // this wallet's own sends, the change included (R39.8.0ag1), are
                    // reported here and only here: `my_spendable_balance` reads the
                    // spendable field, so an unmined note can never size a trade or
                    // fund a swap. Native mode reaches the same result by counting
                    // its own zero-confirmation notes as unspendable.
                    let unspendable_sat = coin
                        .unspendable_total(&scanned.unmined_notes)
                        .map_err(|e| MmError::new(crate::BalanceError::Internal(e)))?;
                    return Ok(CoinBalance {
                        spendable: big_decimal_from_sat_unsigned(scanned.spendable, coin.decimals()),
                        unspendable: big_decimal_from_sat_unsigned(unspendable_sat, coin.decimals()),
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

    fn send_raw_tx(&self, tx: &str) -> Box<dyn Future<Item = String, Error = String> + Send> {
        let bytes = try_fus!(hex::decode(tx));
        self.send_raw_tx_bytes(&bytes)
    }

    /// Broadcasts through the shared UTXO path, then records what the backend accepted.
    ///
    /// This is where a withdrawal's second step lands (`send_raw_transaction`), so it is the one
    /// place this daemon observes that broadcast. The hook lives on `ZCoin` rather than in
    /// `utxo_common`, which is shared by every UTXO coin and knows nothing of shielded notes
    /// (R39.8.0ap, D39.8.0e). Recording happens only after the backend accepted the transaction,
    /// and a failure to record never fails the broadcast.
    fn send_raw_tx_bytes(&self, tx: &[u8]) -> Box<dyn Future<Item = String, Error = String> + Send> {
        let fut = utxo_common::send_raw_tx_bytes(self.as_ref(), tx);
        #[cfg(not(target_arch = "wasm32"))]
        {
            let coin = self.clone();
            let bytes = tx.to_vec();
            Box::new(
                async move {
                    let hash = fut.compat().await?;
                    coin.record_broadcast_shielded_tx(&bytes);
                    Ok(hash)
                }
                .boxed()
                .compat(),
            )
        }
        #[cfg(target_arch = "wasm32")]
        fut
    }

    fn wait_for_confirmations(
        &self,
        tx: &[u8],
        confirmations: u64,
        requires_nota: bool,
        wait_until: u64,
        check_every: u64,
    ) -> Box<dyn Future<Item = (), Error = String> + Send> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            // Read through the Zcash reader: the shared UTXO parser misreads version 5 and 6
            // (R39.11.16a).
            z_tx_read::wait_for_confirmations(self, tx, confirmations, requires_nota, wait_until, check_every)
        }
        #[cfg(target_arch = "wasm32")]
        {
            utxo_common::wait_for_confirmations(
                self.as_ref(),
                tx,
                confirmations,
                requires_nota,
                wait_until,
                check_every,
            )
        }
    }

    fn wait_for_tx_spend(
        &self,
        transaction: &[u8],
        wait_until: u64,
        from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        #[cfg(not(target_arch = "wasm32"))]
        {
            z_tx_read::wait_for_tx_spend(self, transaction, from_block, wait_until)
        }
        #[cfg(target_arch = "wasm32")]
        {
            utxo_common::wait_for_output_spend(
                self.as_ref(),
                transaction,
                utxo_common::DEFAULT_SWAP_VOUT,
                from_block,
                wait_until,
            )
        }
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
        // The WASM build can hold, sync and report the shielded coin but cannot build the
        // shielded transactions a swap needs (CRD ch. 39 R39.6.1), so it is never tradeable there.
        if cfg!(target_arch = "wasm32") {
            return true;
        }
        if self.ironwood_swap_freeze_active() {
            return true;
        }
        let coin_conf = crate::coin_conf(ctx, self.ticker());
        coin_conf["wallet_only"].as_bool().unwrap_or(false)
    }

    fn is_asset_chain(&self) -> bool { self.utxo_arc.conf.asset_chain }

    #[cfg_attr(target_arch = "wasm32", allow(unused_variables))]
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
                    // Nothing to send when the spendable balance does not cover the fee; subtracting
                    // anyway yields a negative amount that only fails later, as an internal error.
                    if balance.spendable <= BigDecimal::from(0u8) {
                        return MmError::err(WithdrawError::ZeroBalanceToWithdrawMax);
                    }
                    if balance.spendable <= fee {
                        return MmError::err(WithdrawError::NotSufficientBalance {
                            coin: coin.ticker().to_owned(),
                            available: balance.spendable,
                            required: fee,
                        });
                    }
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
                    tx_json: None,
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
            let last_sync_error = self.shielded_sync_error();
            z_coin_history_sync_status(
                self.z_fields.wallet_db_scan_complete.load(AtomicOrdering::Relaxed),
                self.z_fields.wallet_db_scanned_through.load(AtomicOrdering::Relaxed),
                self.is_sapling_state_synced(),
                last_sync_error.as_deref(),
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

    async fn spend_safe_amount(&self) -> MmResult<Option<MmNumber>, SpendSafeAmountError> {
        utxo_common::spend_safe_amount(self).await
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

    async fn get_current_mtp(&self) -> UtxoRpcResult<u32> { utxo_common::get_current_mtp(&self.utxo_arc).await }

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
