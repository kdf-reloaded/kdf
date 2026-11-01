use crate::hd_wallet::{HDAccountsMap, HDAccountsMutex};
use crate::hd_wallet_storage::{HDWalletCoinStorage, HDWalletStorageError};
use crate::rpc_command::get_my_address::PathToAddress;
use crate::utxo::header_layout_fallback::HeaderReadPolicy;
use crate::utxo::rpc_clients::electrum_pool::{electrum_block_interval, electrum_server_order, electrum_spread_reads};
use crate::utxo::rpc_clients::{ElectrumClient, ElectrumClientImpl, ElectrumPoolOptions, ElectrumPoolTimings,
                               ElectrumRpcRequest, EstimateFeeMethod, UtxoRpcClientEnum};
use crate::utxo::tx_cache::{UtxoVerboseCacheOps, UtxoVerboseCacheShared};
use crate::utxo::utxo_block_header_storage::{BlockHeaderStorage, InitBlockHeaderStorageOps};
use crate::utxo::utxo_builder::utxo_conf_builder::{header_layout_fallback_families, unrecognised_enum_policy,
                                                   UtxoConfBuilder, UtxoConfError, UtxoConfResult};
use crate::utxo::{output_script, utxo_common, ElectrumBuilderArgs, ElectrumProtoVerifier, RecentlySpentOutPoints,
                  SPVConf, TxFee, UtxoCoinConf, UtxoCoinFields, UtxoHDAccount, UtxoHDWallet, UtxoRpcMode,
                  DEFAULT_GAP_LIMIT, UTXO_DUST_AMOUNT};
use crate::{BlockchainNetwork, CoinTransportMetrics, DerivationMethod, HistorySyncState, PrivKeyBuildPolicy,
            PrivKeyPolicy, RpcClientType, UtxoActivationParams};
use async_trait::async_trait;
use chain::TxHashAlgo;
use common::executor::{spawn, Timer};
use crypto::GlobalHDAccountArc;
use crypto::{Bip32DerPathError, Bip32DerPathOps, Bip44DerPathError, CryptoCtx, CryptoCtxError, CryptoInitError,
             HDPathToCoin, HardwareWalletArc, HwWalletType};
use derive_more::Display;
use futures::channel::mpsc;
use futures::compat::Future01CompatExt;
use futures::lock::Mutex as AsyncMutex;
use futures::StreamExt;
use keys::bytes::Bytes;
pub use keys::{Address, AddressFormat as UtxoAddressFormat, AddressHashEnum, KeyPair, Private, Public, Secret,
               Type as ScriptType};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use primitives::hash::H256;
use serde_json::{self as json, Value as Json};
use serialization::CoinVariant;
use std::sync::{Arc, Mutex, Weak};

cfg_native! {
    use crate::utxo::coin_daemon_data_dir;
    use crate::utxo::rpc_clients::{ConcurrentRequestMap, NativeClient, NativeClientImpl};
    use dirs::home_dir;
    use std::path::{Path, PathBuf};
}

pub type UtxoCoinBuildResult<T> = Result<T, MmError<UtxoCoinBuildError>>;

/// CRD ch.38 R38.6.6a: the fee policy a coins-configuration entry declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TxFeeConfClass {
    /// Plain fixed, `r` base units per kilobyte (proportional, truncated).
    Fixed(u64),
    /// Whole-kilobyte fixed, `r` base units per started kilobyte (`dingo_fee: true`).
    WholeKbFixed(u64),
    /// `txfee: 0`, the rate is estimated per transaction.
    Dynamic,
}

/// Classifies the `txfee` and `dingo_fee` keys of a coins-configuration entry (CRD ch.38 R38.6.6a).
/// An absent `txfee`, or one that is not a non-negative JSON integer, is plain fixed at 1,000;
/// `dingo_fee` matters only for a positive `txfee` and only as the JSON boolean `true`.
pub(crate) fn classify_tx_fee_conf(conf: &Json) -> TxFeeConfClass {
    match conf["txfee"].as_u64() {
        None => TxFeeConfClass::Fixed(1000),
        Some(0) => TxFeeConfClass::Dynamic,
        Some(fee) if conf["dingo_fee"].as_bool() == Some(true) => TxFeeConfClass::WholeKbFixed(fee),
        Some(fee) => TxFeeConfClass::Fixed(fee),
    }
}

#[derive(Debug, Display)]
pub enum UtxoCoinBuildError {
    ConfError(UtxoConfError),
    #[display(fmt = "Native RPC client is only supported in native mode")]
    NativeRpcNotSupportedInWasm,
    ErrorReadingNativeModeConf(String),
    #[display(fmt = "Rpc port is not set neither in `coins` file nor in native daemon config")]
    RpcPortIsNotSet,
    ErrorDetectingFeeMethod(String),
    ErrorDetectingDecimals(String),
    InvalidBlockchainNetwork(String),
    /// No Electrum server could be connected within the call budget; the text names the coin and each server
    /// with its failure (CRD ch.38 R38.6.4k-R item 5, R38.6.4i).
    #[display(fmt = "{}", _0)]
    FailedToConnectToElectrums(String),
    #[display(
        fmt = "No usable Electrum servers for {}: none are configured, or every configured server was rejected as \
               unusable by this build (native builds cannot use 'ws'/'wss' servers).",
        ticker
    )]
    NoUsableElectrumServers {
        ticker: String,
    },
    /// A Light-mode shielded coin needs at least one lightwalletd endpoint (CRD ch.39 R39.11.5).
    #[display(
        fmt = "{}: `light_wallet_d_servers` must list at least one lightwalletd endpoint",
        ticker
    )]
    NoLightwalletdServers {
        ticker: String,
    },
    /// lightwalletd exposes no fee estimation, so `txfee: 0` (estimate) is refused (CRD ch.39 R39.11.1).
    #[display(
        fmt = "{}: `txfee: 0` requests fee estimation, which lightwalletd cannot do; declare a fixed `txfee` or omit it",
        ticker
    )]
    NoFixedFeeDeclared {
        ticker: String,
    },
    #[display(fmt = "Can not detect the user home directory")]
    CantDetectUserHome,
    #[display(fmt = "Unexpected derivation method: {}", _0)]
    UnexpectedDerivationMethod(String),
    #[display(fmt = "Hardware Wallet context is not initialized")]
    HwContextNotInitialized,
    HDWalletStorageError(HDWalletStorageError),
    #[display(
        fmt = "Coin doesn't support Trezor hardware wallet. Please consider adding the 'trezor_coin' field to the coins config"
    )]
    CoinDoesntSupportTrezor,
    #[display(fmt = "Internal error: {}", _0)]
    Internal(String),
}

impl From<UtxoConfError> for UtxoCoinBuildError {
    fn from(e: UtxoConfError) -> Self { UtxoCoinBuildError::ConfError(e) }
}

impl From<CryptoInitError> for UtxoCoinBuildError {
    /// `CryptoCtx` is expected to be initialized already.
    fn from(crypto_err: CryptoInitError) -> Self { UtxoCoinBuildError::Internal(crypto_err.to_string()) }
}

impl From<CryptoCtxError> for UtxoCoinBuildError {
    fn from(e: CryptoCtxError) -> Self { UtxoCoinBuildError::Internal(e.to_string()) }
}

impl From<Bip32DerPathError> for UtxoCoinBuildError {
    fn from(e: Bip32DerPathError) -> Self { UtxoCoinBuildError::Internal(Bip44DerPathError::from(e).to_string()) }
}

impl From<HDWalletStorageError> for UtxoCoinBuildError {
    fn from(e: HDWalletStorageError) -> Self { UtxoCoinBuildError::HDWalletStorageError(e) }
}

#[async_trait]
pub trait UtxoCoinBuilder:
    UtxoFieldsWithIguanaPrivKeyBuilder + UtxoFieldsWithGlobalHDBuilder + UtxoFieldsWithHardwareWalletBuilder
{
    type ResultCoin;
    type Error: NotMmError;

    fn priv_key_policy(&self) -> PrivKeyBuildPolicy;

    async fn build(self) -> MmResult<Self::ResultCoin, Self::Error>;

    async fn build_utxo_fields(&self) -> UtxoCoinBuildResult<UtxoCoinFields> {
        match self.priv_key_policy() {
            PrivKeyBuildPolicy::IguanaPrivKey(priv_key) => {
                self.build_utxo_fields_with_iguana_priv_key(priv_key.as_slice()).await
            },
            PrivKeyBuildPolicy::GlobalHDAccount(global_hd_ctx) => {
                self.build_utxo_fields_with_global_hd(global_hd_ctx).await
            },
            PrivKeyBuildPolicy::Trezor => self.build_utxo_fields_with_trezor().await,
        }
    }
}

#[async_trait]
pub trait UtxoCoinWithIguanaPrivKeyBuilder: UtxoFieldsWithIguanaPrivKeyBuilder {
    type ResultCoin;
    type Error: NotMmError;

    fn priv_key(&self) -> &[u8];

    async fn build(self) -> MmResult<Self::ResultCoin, Self::Error>;
}

#[async_trait]
pub trait UtxoFieldsWithIguanaPrivKeyBuilder: UtxoCoinBuilderCommonOps {
    async fn build_utxo_fields_with_iguana_priv_key(&self, priv_key: &[u8]) -> UtxoCoinBuildResult<UtxoCoinFields> {
        let conf = UtxoConfBuilder::new(self.conf(), self.activation_params(), self.ticker())
            .with_header_layout_fallback_families(header_layout_fallback_families(self.ctx()).mm_err(Into::into)?)
            .with_unrecognised_enum_policy(unrecognised_enum_policy(self.ctx()))
            .build()
            .mm_err(Into::into)?;

        let private = Private {
            prefix: conf.wif_prefix,
            secret: H256::from(priv_key),
            compressed: true,
            checksum_type: conf.checksum_type,
        };
        let key_pair = KeyPair::from_private(private).map_to_mm(|e| UtxoCoinBuildError::Internal(e.to_string()))?;
        let addr_format = self.address_format()?;
        let my_address = Address {
            prefix: conf.pub_addr_prefix,
            t_addr_prefix: conf.pub_t_addr_prefix,
            hash: AddressHashEnum::AddressHash(key_pair.public().address_hash()),
            checksum_type: conf.checksum_type,
            hrp: conf.bech32_hrp.clone(),
            addr_format,
        };

        let my_script_pubkey = output_script(&my_address, ScriptType::P2PKH).to_bytes();
        let derivation_method = DerivationMethod::Iguana(my_address);
        let priv_key_policy = PrivKeyPolicy::KeyPair(key_pair);

        let rpc_client = self.rpc_client().await?;
        let tx_fee = self.tx_fee(&rpc_client).await?;
        let decimals = self.decimals(&rpc_client).await?;
        let dust_amount = self.dust_amount();

        let initial_history_state = self.initial_history_state();
        let tx_hash_algo = self.tx_hash_algo();
        let check_utxo_maturity = self.check_utxo_maturity();
        let tx_cache = self.tx_cache();
        let block_headers_storage = self.block_headers_storage(HeaderReadPolicy::for_conf(&conf))?;

        let coin = UtxoCoinFields {
            conf,
            decimals,
            dust_amount,
            rpc_client,
            priv_key_policy,
            hw_ctx: None,
            derivation_method,
            history_sync_state: Mutex::new(initial_history_state),
            tx_cache,
            block_headers_storage,
            recently_spent_outpoints: AsyncMutex::new(RecentlySpentOutPoints::new(my_script_pubkey)),
            tx_fee,
            tx_hash_algo,
            check_utxo_maturity,
        };
        Ok(coin)
    }
}

/// The secp256k1 secret of the address a UTXO coin activates under the global HD wallet:
/// `m/<derivation_path>/<account>'/<chain>/<address_id>` of `path_to_address` (by default account 0,
/// the external chain and address 0, e.g. `m/44'/141'/0'/0/0`).
///
/// A shielded coin takes its transparent key from this same derivation (CRD ch.39 R39.6.4,
/// "Transparent key under HD").
///
/// # Errors
///
/// Fails when a child number is out of range or the derivation fails.
pub fn derive_activated_hd_secret(
    global_hd_ctx: &GlobalHDAccountArc,
    derivation_path: &HDPathToCoin,
    path_to_address: &PathToAddress,
) -> UtxoCoinBuildResult<crypto::Secp256k1Secret> {
    let mut full_path = derivation_path.to_derivation_path();
    // account (hardened)
    full_path.push(
        crypto::ChildNumber::new(path_to_address.account_id, true)
            .map_err(|_| UtxoCoinBuildError::Internal("failed to create hardened child number".to_owned()))?,
    );
    // chain (non-hardened)
    full_path.push(
        crypto::ChildNumber::new(path_to_address.chain as u32, false)
            .map_err(|_| UtxoCoinBuildError::Internal("failed to create child number".to_owned()))?,
    );
    // address index (non-hardened)
    full_path.push(
        crypto::ChildNumber::new(path_to_address.address_id, false)
            .map_err(|_| UtxoCoinBuildError::Internal("failed to create child number".to_owned()))?,
    );

    global_hd_ctx
        .derive_secp256k1_secret(&full_path)
        .mm_err(|e| UtxoCoinBuildError::Internal(e.to_string()))
}

#[async_trait]
pub trait UtxoFieldsWithGlobalHDBuilder: UtxoCoinBuilderCommonOps + UtxoFieldsWithHardwareWalletBuilder {
    async fn build_utxo_fields_with_global_hd(
        &self,
        global_hd_ctx: GlobalHDAccountArc,
    ) -> UtxoCoinBuildResult<UtxoCoinFields> {
        let ticker = self.ticker().to_owned();
        let conf = UtxoConfBuilder::new(self.conf(), self.activation_params(), &ticker)
            .with_header_layout_fallback_families(header_layout_fallback_families(self.ctx()).mm_err(Into::into)?)
            .with_unrecognised_enum_policy(unrecognised_enum_policy(self.ctx()))
            .build()
            .mm_err(Into::into)?;

        // Derive key pair at the activated address: `path_to_address` of the request,
        // defaulting to account 0, the external chain and address 0 (m/44'/coin_type'/0'/0/0).
        let derivation_path = self.derivation_path().mm_err(Into::into)?;
        let path_to_address = self.activation_params().path_to_address.clone().unwrap_or_default();

        let secret = derive_activated_hd_secret(&global_hd_ctx, &derivation_path, &path_to_address)?;

        let private = Private {
            prefix: conf.wif_prefix,
            secret,
            compressed: true,
            checksum_type: conf.checksum_type,
        };
        let key_pair = KeyPair::from_private(private).map_to_mm(|e| UtxoCoinBuildError::Internal(e.to_string()))?;

        let addr_format = self.address_format()?;
        let activated_address = Address {
            prefix: conf.pub_addr_prefix,
            t_addr_prefix: conf.pub_t_addr_prefix,
            hash: AddressHashEnum::AddressHash(key_pair.public().address_hash()),
            checksum_type: conf.checksum_type,
            hrp: conf.bech32_hrp.clone(),
            addr_format: addr_format.clone(),
        };
        let my_script_pubkey = output_script(&activated_address, ScriptType::P2PKH).to_bytes();

        // Store both the active key pair and the root extended key for future derivation.
        let priv_key_policy = PrivKeyPolicy::HDWallet {
            activated_key: key_pair,
            bip39_secp_priv_key: global_hd_ctx.root_priv_key().clone(),
        };

        // Build the HD wallet structure for multi-account support.
        let hd_wallet_storage = HDWalletCoinStorage::init(self.ctx(), ticker).await.mm_err(Into::into)?;
        let accounts = self
            .load_hd_wallet_accounts(&hd_wallet_storage, &derivation_path)
            .await?;
        let gap_limit = self.gap_limit();
        let hd_wallet = UtxoHDWallet {
            hd_wallet_storage,
            address_format: addr_format,
            derivation_path,
            accounts: HDAccountsMutex::new(accounts),
            gap_limit,
        };

        let derivation_method = DerivationMethod::HDWallet(hd_wallet);

        let rpc_client = self.rpc_client().await?;
        let tx_fee = self.tx_fee(&rpc_client).await?;
        let decimals = self.decimals(&rpc_client).await?;
        let dust_amount = self.dust_amount();

        let initial_history_state = self.initial_history_state();
        let tx_hash_algo = self.tx_hash_algo();
        let check_utxo_maturity = self.check_utxo_maturity();
        let tx_cache = self.tx_cache();
        let block_headers_storage = self.block_headers_storage(HeaderReadPolicy::for_conf(&conf))?;

        let coin = UtxoCoinFields {
            conf,
            decimals,
            dust_amount,
            rpc_client,
            priv_key_policy,
            hw_ctx: None,
            derivation_method,
            history_sync_state: Mutex::new(initial_history_state),
            tx_cache,
            block_headers_storage,
            recently_spent_outpoints: AsyncMutex::new(RecentlySpentOutPoints::new(my_script_pubkey)),
            tx_fee,
            tx_hash_algo,
            check_utxo_maturity,
        };
        Ok(coin)
    }
}

#[async_trait]
pub trait UtxoFieldsWithHardwareWalletBuilder: UtxoCoinBuilderCommonOps {
    async fn build_utxo_fields_with_trezor(&self) -> UtxoCoinBuildResult<UtxoCoinFields> {
        let ticker = self.ticker().to_owned();
        let conf = UtxoConfBuilder::new(self.conf(), self.activation_params(), &ticker)
            .with_header_layout_fallback_families(header_layout_fallback_families(self.ctx()).mm_err(Into::into)?)
            .with_unrecognised_enum_policy(unrecognised_enum_policy(self.ctx()))
            .build()
            .mm_err(Into::into)?;

        if !self.supports_trezor(&conf) {
            return MmError::err(UtxoCoinBuildError::CoinDoesntSupportTrezor);
        }
        let hw_ctx = self.trezor_hw_ctx()?;

        // For now, use a default script pubkey.
        // TODO change the type of `recently_spent_outpoints` to `AsyncMutex<HashMap<Bytes, RecentlySpentOutPoints>>`
        let my_script_pubkey = Bytes::new();
        let recently_spent_outpoints = AsyncMutex::new(RecentlySpentOutPoints::new(my_script_pubkey));

        let address_format = self.address_format()?;
        let derivation_path = self.derivation_path().mm_err(Into::into)?;

        let hd_wallet_storage = HDWalletCoinStorage::init(self.ctx(), ticker).await.mm_err(Into::into)?;

        let accounts = self
            .load_hd_wallet_accounts(&hd_wallet_storage, &derivation_path)
            .await?;
        let gap_limit = self.gap_limit();
        let hd_wallet = UtxoHDWallet {
            hd_wallet_storage,
            address_format,
            derivation_path,
            accounts: HDAccountsMutex::new(accounts),
            gap_limit,
        };

        let rpc_client = self.rpc_client().await?;
        let tx_fee = self.tx_fee(&rpc_client).await?;
        let decimals = self.decimals(&rpc_client).await?;
        let dust_amount = self.dust_amount();

        let initial_history_state = self.initial_history_state();
        let tx_hash_algo = self.tx_hash_algo();
        let check_utxo_maturity = self.check_utxo_maturity();
        let tx_cache = self.tx_cache();
        let block_headers_storage = self.block_headers_storage(HeaderReadPolicy::for_conf(&conf))?;

        let coin = UtxoCoinFields {
            conf,
            decimals,
            dust_amount,
            rpc_client,
            priv_key_policy: PrivKeyPolicy::Trezor,
            hw_ctx: Some(hw_ctx),
            derivation_method: DerivationMethod::HDWallet(hd_wallet),
            history_sync_state: Mutex::new(initial_history_state),
            block_headers_storage,
            tx_cache,
            recently_spent_outpoints,
            tx_fee,
            tx_hash_algo,
            check_utxo_maturity,
        };
        Ok(coin)
    }

    async fn load_hd_wallet_accounts(
        &self,
        hd_wallet_storage: &HDWalletCoinStorage,
        derivation_path: &HDPathToCoin,
    ) -> UtxoCoinBuildResult<HDAccountsMap<UtxoHDAccount>> {
        utxo_common::load_hd_accounts_from_storage(hd_wallet_storage, derivation_path)
            .await
            .mm_err(UtxoCoinBuildError::from)
    }

    #[inline]
    fn derivation_path(&self) -> UtxoConfResult<HDPathToCoin> {
        if self.conf()["derivation_path"].is_null() {
            return MmError::err(UtxoConfError::DerivationPathIsNotSet);
        }
        json::from_value(self.conf()["derivation_path"].clone())
            .map_to_mm(|e| UtxoConfError::ErrorDeserializingDerivationPath(e.to_string()))
    }

    #[inline]
    fn gap_limit(&self) -> u32 { self.activation_params().gap_limit.unwrap_or(DEFAULT_GAP_LIMIT) }

    #[inline]
    fn supports_trezor(&self, conf: &UtxoCoinConf) -> bool { conf.trezor_coin.is_some() }

    #[inline]
    fn trezor_hw_ctx(&self) -> UtxoCoinBuildResult<HardwareWalletArc> {
        let crypto_ctx = CryptoCtx::from_ctx(self.ctx()).mm_err(Into::into)?;
        let hw_ctx = crypto_ctx
            .hw_ctx()
            .or_mm_err(|| UtxoCoinBuildError::HwContextNotInitialized)?;
        match hw_ctx.hw_wallet_type() {
            HwWalletType::Trezor => Ok(hw_ctx.clone()),
        }
    }
}

/// Builds the Electrum client of a coin: the shared pool of CRD ch.38 R38.6.4b--R38.6.4k, probed once so the build
/// waits for the first connection, then maintained in the background. Both the activation of an
/// Electrum-backed UTXO coin and the lazily built fallback client of a shielded coin (CRD ch.39 R39.11.25) come
/// through here.
pub(crate) async fn build_electrum_client(
    ctx: &MmArc,
    ticker: &str,
    coin_conf: &Json,
    args: ElectrumBuilderArgs,
    servers: Vec<ElectrumRpcRequest>,
    min_connected: Option<usize>,
    max_connected: Option<usize>,
) -> UtxoCoinBuildResult<ElectrumClient> {
    let mut event_handlers = vec![];
    if args.collect_metrics {
        event_handlers.push(
            CoinTransportMetrics::new(ctx.metrics.weak(), ticker.to_owned(), RpcClientType::Electrum).into_shared(),
        );
    }

    // Distinguish "nothing to try" from "everything we tried failed": an empty server
    // list means the coin's `electrum` entry is missing.
    if servers.is_empty() {
        return MmError::err(UtxoCoinBuildError::NoUsableElectrumServers {
            ticker: ticker.to_owned(),
        });
    }

    // The activation probe (CRD ch.38 R38.6.4k-R) connects every listed server at once and the build
    // waits for the first connection; the pool then maintains its set in the background.
    let client_name = args
        .negotiate_version
        .then(|| format!("{} GUI/MM2 {}", ctx.gui().unwrap_or("UNKNOWN"), ctx.mm_version()));
    let mut client = ElectrumClientImpl::new(ticker.to_owned(), event_handlers);
    client.configure_pool(ElectrumPoolOptions {
        min_connected: min_connected.unwrap_or(1),
        max_connected,
        client_name,
        timings: ElectrumPoolTimings::default(),
        order: electrum_server_order(ctx),
        spread_reads: electrum_spread_reads(ctx),
        block_interval: electrum_block_interval(coin_conf),
    });
    for (server, e) in client.add_servers(&servers) {
        // A server is named as `host:port` or by a placeholder, never by its configured string (CRD ch.38
        // R38.6.9a).
        log!("Error " (e) " using " (server) ". Address won't be used");
    }
    // A server this build cannot use at all (native builds skip 'ws'/'wss' servers, so a WSS-only
    // coin lands here) leaves the pool; with none left there is nothing to wait for.
    if client.count_connections().await == 0 {
        return MmError::err(UtxoCoinBuildError::NoUsableElectrumServers {
            ticker: ticker.to_owned(),
        });
    }

    let client = Arc::new(client);
    client
        .activation_probe()
        .await
        .map_to_mm(UtxoCoinBuildError::FailedToConnectToElectrums)?;
    client.start_pool(args.spawn_ping);

    Ok(ElectrumClient(client))
}

#[async_trait]
pub trait UtxoCoinBuilderCommonOps {
    fn ctx(&self) -> &MmArc;

    fn conf(&self) -> &Json;

    fn activation_params(&self) -> &UtxoActivationParams;

    fn ticker(&self) -> &str;

    #[inline]
    fn block_headers_storage(&self, policy: HeaderReadPolicy) -> UtxoCoinBuildResult<Option<BlockHeaderStorage>> {
        let conf: Option<SPVConf> = json::from_value(self.conf()["spv_conf"].clone())
            .map_to_mm(|e| UtxoConfError::InvalidSpvConf(e.to_string()))
            .mm_err(Into::into)?;
        match conf {
            None => Ok(None),
            Some(conf) => {
                conf.validate()
                    .map_to_mm(UtxoConfError::InvalidSpvConf)
                    .mm_err(Into::into)?;
                Ok(BlockHeaderStorage::new_from_ctx(self.ctx().clone(), conf, policy))
            },
        }
    }

    fn address_format(&self) -> UtxoCoinBuildResult<UtxoAddressFormat> {
        let format_from_req = self.activation_params().address_format.clone();
        let format_from_conf = json::from_value::<Option<UtxoAddressFormat>>(self.conf()["address_format"].clone())
            .map_to_mm(|e| UtxoConfError::InvalidAddressFormat(e.to_string()))
            .mm_err(Into::into)?
            .unwrap_or(UtxoAddressFormat::Standard);

        let mut address_format = match format_from_req {
            Some(from_req) => {
                if from_req.is_segwit() != format_from_conf.is_segwit() {
                    let error = format!(
                        "Both conf {:?} and request {:?} must be either Segwit or Standard/CashAddress",
                        format_from_conf, from_req
                    );
                    return MmError::err(UtxoCoinBuildError::from(UtxoConfError::InvalidAddressFormat(error)));
                } else {
                    from_req
                }
            },
            None => format_from_conf,
        };

        if let UtxoAddressFormat::CashAddress {
            network: _,
            ref mut pub_addr_prefix,
            ref mut p2sh_addr_prefix,
        } = address_format
        {
            *pub_addr_prefix = self.pub_addr_prefix();
            *p2sh_addr_prefix = self.p2sh_address_prefix();
        }

        let is_segwit_in_conf = self.conf()["segwit"].as_bool().unwrap_or(false);
        if address_format.is_segwit() && (!is_segwit_in_conf || self.conf()["bech32_hrp"].is_null()) {
            let error =
                "Cannot use Segwit address format for coin without segwit support or bech32_hrp in config".to_owned();
            return MmError::err(UtxoCoinBuildError::from(UtxoConfError::InvalidAddressFormat(error)));
        }
        Ok(address_format)
    }

    #[inline]
    fn pub_addr_prefix(&self) -> u8 {
        let pubtype = self.conf()["pubtype"]
            .as_u64()
            .unwrap_or(if self.ticker() == "BTC" { 0 } else { 60 });
        pubtype as u8
    }

    #[inline]
    fn p2sh_address_prefix(&self) -> u8 {
        self.conf()["p2shtype"]
            .as_u64()
            .unwrap_or(if self.ticker() == "BTC" { 5 } else { 85 }) as u8
    }

    #[inline]
    fn dust_amount(&self) -> u64 { json::from_value(self.conf()["dust"].clone()).unwrap_or(UTXO_DUST_AMOUNT) }

    #[inline]
    fn network(&self) -> UtxoCoinBuildResult<BlockchainNetwork> {
        let conf = self.conf();
        if !conf["network"].is_null() {
            return json::from_value(conf["network"].clone())
                .map_to_mm(|e| UtxoCoinBuildError::InvalidBlockchainNetwork(e.to_string()));
        }
        Ok(BlockchainNetwork::Mainnet)
    }

    #[inline]
    async fn decimals(&self, _rpc_client: &UtxoRpcClientEnum) -> UtxoCoinBuildResult<u8> {
        Ok(self.conf()["decimals"].as_u64().unwrap_or(8) as u8)
    }

    async fn tx_fee(&self, rpc_client: &UtxoRpcClientEnum) -> UtxoCoinBuildResult<TxFee> {
        // lightwalletd has no fee estimation: an explicit request for it (`txfee: 0`) is refused.
        // An undeclared fee keeps the shared fixed default below, as the published ARRR entry relies
        // on (CRD ch.39 R39.11.1).
        #[cfg(not(target_arch = "wasm32"))]
        if matches!(rpc_client, UtxoRpcClientEnum::LightWalletD(_)) && self.conf()["txfee"].as_u64() == Some(0) {
            return MmError::err(UtxoCoinBuildError::NoFixedFeeDeclared {
                ticker: self.ticker().to_owned(),
            });
        }
        let tx_fee = match classify_tx_fee_conf(self.conf()) {
            TxFeeConfClass::Fixed(fee) => TxFee::FixedPerKb(fee),
            TxFeeConfClass::WholeKbFixed(fee) => TxFee::FixedPerStartedKb(fee),
            TxFeeConfClass::Dynamic => {
                let fee_method = match &rpc_client {
                    UtxoRpcClientEnum::Electrum(_) => EstimateFeeMethod::Standard,
                    #[cfg(not(target_arch = "wasm32"))]
                    UtxoRpcClientEnum::LightWalletD(_) => EstimateFeeMethod::Standard,
                    UtxoRpcClientEnum::Native(client) => client
                        .detect_fee_method()
                        .compat()
                        .await
                        .map_to_mm(UtxoCoinBuildError::ErrorDetectingFeeMethod)?,
                };
                TxFee::Dynamic(fee_method)
            },
        };
        Ok(tx_fee)
    }

    #[inline]
    fn initial_history_state(&self) -> HistorySyncState {
        if self.activation_params().tx_history {
            HistorySyncState::NotStarted
        } else {
            HistorySyncState::NotEnabled
        }
    }

    async fn rpc_client(&self) -> UtxoCoinBuildResult<UtxoRpcClientEnum> {
        match self.activation_params().mode.clone() {
            UtxoRpcMode::Native => {
                #[cfg(target_arch = "wasm32")]
                {
                    MmError::err(UtxoCoinBuildError::NativeRpcNotSupportedInWasm)
                }
                #[cfg(not(target_arch = "wasm32"))]
                {
                    let native = self.native_client()?;
                    Ok(UtxoRpcClientEnum::Native(native))
                }
            },
            UtxoRpcMode::Electrum {
                servers,
                min_connected,
                max_connected,
            } => {
                let electrum = self
                    .electrum_client(ElectrumBuilderArgs::default(), servers, min_connected, max_connected)
                    .await?;
                Ok(UtxoRpcClientEnum::Electrum(electrum))
            },
        }
    }

    async fn electrum_client(
        &self,
        args: ElectrumBuilderArgs,
        servers: Vec<ElectrumRpcRequest>,
        min_connected: Option<usize>,
        max_connected: Option<usize>,
    ) -> UtxoCoinBuildResult<ElectrumClient> {
        build_electrum_client(
            self.ctx(),
            self.ticker(),
            self.conf(),
            args,
            servers,
            min_connected,
            max_connected,
        )
        .await
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn native_client(&self) -> UtxoCoinBuildResult<NativeClient> {
        use base64::{encode_config as base64_encode, URL_SAFE};

        let native_conf_path = self.confpath()?;
        let network = self.network()?;
        let (rpc_port, rpc_user, rpc_password) = read_native_mode_conf(&native_conf_path, &network)
            .map_to_mm(UtxoCoinBuildError::ErrorReadingNativeModeConf)?;
        let auth_str = fomat!((rpc_user)":"(rpc_password));
        let rpc_port = match rpc_port {
            Some(p) => p,
            None => self.conf()["rpcport"]
                .as_u64()
                .or_mm_err(|| UtxoCoinBuildError::RpcPortIsNotSet)? as u16,
        };

        let ctx = self.ctx();
        let coin_ticker = self.ticker().to_owned();
        let event_handlers =
            vec![
                CoinTransportMetrics::new(ctx.metrics.weak(), coin_ticker.clone(), RpcClientType::Native).into_shared(),
            ];
        let client = Arc::new(NativeClientImpl {
            coin_ticker,
            uri: fomat!("http://127.0.0.1:"(rpc_port)),
            auth: format!("Basic {}", base64_encode(&auth_str, URL_SAFE)),
            event_handlers,
            request_id: 0u64.into(),
            list_unspent_concurrent_map: ConcurrentRequestMap::new(),
        });

        Ok(NativeClient(client))
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn confpath(&self) -> UtxoCoinBuildResult<PathBuf> {
        let conf = self.conf();
        // Documented at https://github.com/jl777/coins#bitcoin-protocol-specific-json
        // "USERHOME/" prefix should be replaced with the user's home folder.
        let declared_confpath = match self.conf()["confpath"].as_str() {
            Some(path) if !path.is_empty() => path.trim(),
            _ => {
                let (name, is_asset_chain) = {
                    match conf["asset"].as_str() {
                        Some(a) => (a, true),
                        None => {
                            let name = conf["name"]
                                .as_str()
                                .or_mm_err(|| UtxoConfError::CurrencyNameIsNotSet)
                                .mm_err(Into::into)?;
                            (name, false)
                        },
                    }
                };
                let data_dir = coin_daemon_data_dir(name, is_asset_chain);
                let confname = format!("{}.conf", name);

                return Ok(data_dir.join(&confname[..]));
            },
        };

        let (confpath, rel_to_home) = match declared_confpath.strip_prefix("~/") {
            Some(stripped) => (stripped, true),
            None => match declared_confpath.strip_prefix("USERHOME/") {
                Some(stripped) => (stripped, true),
                None => (declared_confpath, false),
            },
        };

        if rel_to_home {
            let home = home_dir().or_mm_err(|| UtxoCoinBuildError::CantDetectUserHome)?;
            Ok(home.join(confpath))
        } else {
            Ok(confpath.into())
        }
    }

    #[inline]
    fn tx_hash_algo(&self) -> TxHashAlgo {
        if self.ticker() == "GRS" {
            TxHashAlgo::SHA256
        } else {
            TxHashAlgo::DSHA256
        }
    }

    #[inline]
    fn check_utxo_maturity(&self) -> bool { self.activation_params().check_utxo_maturity.unwrap_or_default() }

    #[inline]
    #[cfg(target_arch = "wasm32")]
    fn tx_cache(&self) -> UtxoVerboseCacheShared {
        crate::utxo::tx_cache::wasm_tx_cache::WasmVerboseCache::default().into_shared()
    }

    #[inline]
    #[cfg(not(target_arch = "wasm32"))]
    fn tx_cache(&self) -> UtxoVerboseCacheShared {
        crate::utxo::tx_cache::fs_tx_cache::FsVerboseCache::new(self.ticker().to_owned(), self.tx_cache_path())
            .into_shared()
    }

    #[inline]
    #[cfg(not(target_arch = "wasm32"))]
    fn tx_cache_path(&self) -> PathBuf { self.ctx().dbdir().join("TX_CACHE") }
}

/// Attempts to parse native daemon conf file and return rpcport, rpcuser and rpcpassword
#[cfg(not(target_arch = "wasm32"))]
fn read_native_mode_conf(
    filename: &dyn AsRef<Path>,
    network: &BlockchainNetwork,
) -> Result<(Option<u16>, String, String), String> {
    use ini::Ini;

    fn read_property<'a>(conf: &'a ini::Ini, network: &BlockchainNetwork, property: &str) -> Option<&'a String> {
        let subsection = match network {
            BlockchainNetwork::Mainnet => None,
            BlockchainNetwork::Testnet => conf.section(Some("test")),
            BlockchainNetwork::Regtest => conf.section(Some("regtest")),
        };
        subsection
            .and_then(|props| props.get(property))
            .or_else(|| conf.general_section().get(property))
    }

    let conf: Ini = match Ini::load_from_file(&filename) {
        Ok(ini) => ini,
        Err(err) => {
            return ERR!(
                "Error parsing the native wallet configuration '{}': {}",
                filename.as_ref().display(),
                err
            )
        },
    };
    let rpc_port = match read_property(&conf, network, "rpcport") {
        Some(port) => port.parse::<u16>().ok(),
        None => None,
    };
    let rpc_user = try_s!(read_property(&conf, network, "rpcuser").ok_or(ERRL!(
        "Conf file {} doesn't have the rpcuser key",
        filename.as_ref().display()
    )));
    let rpc_password = try_s!(read_property(&conf, network, "rpcpassword").ok_or(ERRL!(
        "Conf file {} doesn't have the rpcpassword key",
        filename.as_ref().display()
    )));
    Ok((rpc_port, rpc_user.clone(), rpc_password.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CRD ch.38 R38.6.6a: the classification table.
    #[test]
    fn test_classify_tx_fee_conf_r38_6_6a() {
        use serde_json::json;
        let cases = [
            (json!({}), TxFeeConfClass::Fixed(1000)),
            (json!({"txfee": "1000"}), TxFeeConfClass::Fixed(1000)),
            (json!({"txfee": -5}), TxFeeConfClass::Fixed(1000)),
            (json!({"txfee": 1000.5}), TxFeeConfClass::Fixed(1000)),
            (json!({"txfee": null, "dingo_fee": true}), TxFeeConfClass::Fixed(1000)),
            (json!({"dingo_fee": true}), TxFeeConfClass::Fixed(1000)),
            (json!({"txfee": 0}), TxFeeConfClass::Dynamic),
            (json!({"txfee": 0, "dingo_fee": true}), TxFeeConfClass::Dynamic),
            (
                json!({"txfee": 100_000_000, "dingo_fee": true}),
                TxFeeConfClass::WholeKbFixed(100_000_000),
            ),
            (
                json!({"txfee": 300_000, "dingo_fee": false}),
                TxFeeConfClass::Fixed(300_000),
            ),
            (json!({"txfee": 300_000}), TxFeeConfClass::Fixed(300_000)),
            (
                json!({"txfee": 300_000, "dingo_fee": "true"}),
                TxFeeConfClass::Fixed(300_000),
            ),
            (
                json!({"txfee": 300_000, "dingo_fee": 1}),
                TxFeeConfClass::Fixed(300_000),
            ),
        ];
        for (conf, expected) in cases {
            assert_eq!(classify_tx_fee_conf(&conf), expected, "{conf}");
        }
    }
}
