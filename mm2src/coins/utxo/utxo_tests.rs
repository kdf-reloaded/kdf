use super::*;
use crate::coin_balance::HDAddressBalance;
use crate::hd_wallet::HDAccountsMap;
use crate::hd_wallet_storage::{HDWalletMockStorage, HDWalletStorageInternalOps};
use crate::rpc_command::account_balance::{AccountBalanceParams, AccountBalanceRpcOps, HDAccountBalanceResponse};
use crate::rpc_command::init_scan_for_new_addresses::{InitScanAddressesRpcOps, ScanAddressesParams,
                                                      ScanAddressesResponse};
use crate::utxo::header_layout_fallback::HeaderReadPolicy;
use crate::utxo::qtum::{qtum_coin_with_priv_key, QtumCoin, QtumDelegationOps, QtumDelegationRequest};
use crate::utxo::rpc_clients::{BlockHashOrHeight, ElectrumBalance, ElectrumClient, ElectrumClientImpl,
                               GetAddressInfoRes, ListSinceBlockRes, ListTransactionsItem, NativeClient,
                               NativeClientImpl, NativeUnspent, NetworkInfo, UtxoRpcClientOps, ValidateAddressRes,
                               VerboseBlock};
use crate::utxo::tx_cache::dummy_tx_cache::DummyVerboseCache;
use crate::utxo::tx_cache::UtxoVerboseCacheOps;
use crate::utxo::utxo_builder::{UtxoArcBuilder, UtxoCoinBuilderCommonOps, UtxoConfBuilder, UtxoConfError,
                                UtxoConfResult};
use crate::utxo::utxo_common::calc_interest_of_tx;
use crate::utxo::utxo_common::{address_from_pubkey, get_htlc_key_pair_v2, my_public_key,
                               trade_preimage_sender_address, UtxoTxBuilder};
use crate::utxo::utxo_common_tests;
use crate::utxo::utxo_standard::{utxo_standard_coin_with_priv_key, UtxoStandardCoin};
#[cfg(not(target_arch = "wasm32"))] use crate::WithdrawFee;
use crate::{CoinBalance, CommonSwapOpsV2, ParseCoinAssocTypes, PrivKeyBuildPolicy, PrivKeyPolicy, StakingInfosDetails,
            SwapOps, TradePreimageValue, TxFeeDetails};
use crate::{DexFee, ValidateFeeArgs};
use bigdecimal::{BigDecimal, Signed};
use chain::{OutPoint, TransactionInput};
use common::executor::Timer;
use common::{block_on, now_ms, OrdRange, PagingOptionsEnum};
use crypto::{privkey::key_pair_from_seed, Bip44Chain, RpcDerivationPath};
use futures::future::join_all;
use futures::TryFutureExt;
use mm2_core::mm_ctx::MmCtxBuilder;
use mm2_number::MmNumber;
use mocktopus::mocking::*;
use rpc::v1::types::H256 as H256Json;
use serialization::{deserialize, CoinVariant};
use std::convert::TryFrom;
use std::iter;
use std::mem::discriminant;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};

// The server list of every tQTUM test (the tests that activate the testnet Qtum coin, here and in
// `qrc20_tests.rs`).
//
// There is exactly one public Qtum TESTNET Electrum server, `s6.electrum.qtum.site`, so this is the only entry:
// - The `s1`, `s3` and `s4` hosts of `qtum.info` on port 50001 serve Qtum MAINNET. A tQTUM test pointed at them
//   reads the wrong chain (empty wallets, no delegation, no staker), which is why they are not listed.
// - Port 51002 of the same host is TLS, but its certificate is X.509 version 1. This project refuses a version 1
//   certificate in every mode, `disable_cert_verification` included (CRD ch.38 R38.6.4m item 5), so that port can
//   never connect and is not listed. Hence plain TCP on port 51001.
// - Its history answers can be slow, and the answer for a busy address may miss the 20 s a request gets once the
//   connection is up (R38.6.4e item 2). That bound is fixed: the server entry's `timeout_sec` bounds only
//   establishing the connection, including the version negotiation (R38.6.4e item 1), and the activation of a coin
//   builds its pool with the default timings, so a test cannot raise it. A slow answer shows up as a failure of
//   that call; run the test again before suspecting the code.
//
// Add a second testnet server here if one appears.
const TQTUM_TEST_SERVER: &str = "s6.electrum.qtum.site:51001";

/// The `servers` value of an Electrum activation request of a tQTUM test; see [`TQTUM_TEST_SERVER`].
pub(crate) fn tqtum_test_servers() -> serde_json::Value { json!([{ "url": TQTUM_TEST_SERVER }]) }

/// Test-only DEX-fee destination pubkey, resolved through `mm2_net_config`
/// for the community netid. Replaces direct use of
/// `common::DEX_FEE_ADDR_RAW_PUBKEY` so test fixtures go through the same
/// per-netid registry as production code (LP-3F.A1).
fn test_dex_fee_addr_raw_pubkey() -> &'static [u8] {
    mm2_net_config::net_config_or_panic(8762).dex_fee_addr_raw_pubkey()
}

const TEST_COIN_NAME: &'static str = "DOC";
// Made-up hrp for doc to test p2wpkh script
const TEST_COIN_HRP: &'static str = "rck";
const DOC_ELECTRUM_ADDRS: &[&str] = &[
    "doc.electrum1.cipig.net:10020",
    "doc.electrum2.cipig.net:10020",
    "doc.electrum3.cipig.net:10020",
];
const TEST_COIN_DECIMALS: u8 = 8;

pub fn electrum_client_for_test(servers: &[&str]) -> ElectrumClient {
    let ctx = MmCtxBuilder::default().into_mm_arc();
    let servers: Vec<_> = servers.iter().map(|server| json!({ "url": server })).collect();
    let req = json!({
        "method": "electrum",
        "servers": servers,
    });
    let params = UtxoActivationParams::from_legacy_req(&req).unwrap();
    let priv_key_policy = PrivKeyBuildPolicy::IguanaPrivKey(keys::Secret::default());
    let builder = UtxoArcBuilder::new(
        &ctx,
        TEST_COIN_NAME,
        &Json::Null,
        &params,
        priv_key_policy,
        UtxoStandardCoin::from,
    );
    let args = ElectrumBuilderArgs {
        spawn_ping: false,
        negotiate_version: true,
        collect_metrics: false,
    };

    let servers = servers.into_iter().map(|s| json::from_value(s).unwrap()).collect();
    block_on(builder.electrum_client(args, servers, None, None)).unwrap()
}

/// Returned client won't work by default, requires some mocks to be usable
#[cfg(not(target_arch = "wasm32"))]
fn native_client_for_test() -> NativeClient { NativeClient(Arc::new(NativeClientImpl::default())) }

#[test]
fn test_utxo_activation_params_min_addresses_number() {
    let params: UtxoActivationParams = json::from_value(json!({
        "mode": { "rpc": "Native" },
        "min_addresses_number": 1
    }))
    .unwrap();
    assert_eq!(params.min_addresses_number, Some(1));

    let params_without_minimum: UtxoActivationParams =
        json::from_value(json!({ "mode": { "rpc": "Native" } })).unwrap();
    assert_eq!(params_without_minimum.min_addresses_number, None);
}

pub(crate) fn utxo_coin_fields_for_test(
    rpc_client: UtxoRpcClientEnum,
    force_seed: Option<&str>,
    is_segwit_coin: bool,
) -> UtxoCoinFields {
    let checksum_type = ChecksumType::DSHA256;
    let default_seed = "spice describe gravity federal blast come thank unfair canal monkey style afraid";
    let seed = match force_seed {
        Some(s) => s.into(),
        None => match std::env::var("BOB_PASSPHRASE") {
            Ok(p) => {
                if p.is_empty() {
                    default_seed.into()
                } else {
                    p
                }
            },
            Err(_) => default_seed.into(),
        },
    };
    let key_pair = key_pair_from_seed(&seed).unwrap();
    let my_address = Address {
        prefix: 60,
        hash: key_pair.public().address_hash().into(),
        t_addr_prefix: 0,
        checksum_type,
        hrp: if is_segwit_coin {
            Some(TEST_COIN_HRP.to_string())
        } else {
            None
        },
        addr_format: if is_segwit_coin {
            UtxoAddressFormat::Segwit
        } else {
            UtxoAddressFormat::Standard
        },
    };
    let my_script_pubkey = Builder::build_p2pkh(&my_address.hash).to_bytes();

    let priv_key_policy = PrivKeyPolicy::KeyPair(key_pair);
    let derivation_method = DerivationMethod::Iguana(my_address);

    let bech32_hrp = if is_segwit_coin {
        Some(TEST_COIN_HRP.to_string())
    } else {
        None
    };

    UtxoCoinFields {
        conf: UtxoCoinConf {
            is_pos: false,
            requires_notarization: false.into(),
            overwintered: true,
            segwit: true,
            tx_version: 4,
            default_address_format: UtxoAddressFormat::Standard,
            asset_chain: true,
            p2sh_addr_prefix: 85,
            p2sh_t_addr_prefix: 0,
            pub_addr_prefix: 60,
            pub_t_addr_prefix: 0,
            sign_message_prefix: Some(String::from("Komodo Signed Message:\n")),
            bech32_hrp,
            ticker: TEST_COIN_NAME.into(),
            wif_prefix: 0,
            tx_fee_volatility_percent: DEFAULT_DYNAMIC_FEE_VOLATILITY_PERCENT,
            version_group_id: 0x892f2085,
            consensus_branch_id: 0x76b809bb,
            zcash: true,
            checksum_type,
            fork_id: 0,
            signature_version: SignatureVersion::Base,
            sighash_suffix: 0,
            sighash_suffix_height: None,
            require_suffix_counterparty: true,
            required_confirmations: 1.into(),
            force_min_relay_fee: false,
            mtp_block_count: NonZeroU64::new(11).unwrap(),
            estimate_fee_mode: None,
            mature_confirmations: MATURE_CONFIRMATIONS_DEFAULT,
            estimate_fee_blocks: 1,
            trezor_coin: None,
            enable_spv_proof: false,
            header_layout: None,
            chain_variant: None,
            protocol_header_reader: CoinVariant::Standard,
            header_layout_fallback_families: vec!["BTC".to_owned()],
            header_layout_fallback_logged: Default::default(),
            header_time_only_logged: Default::default(),
        },
        decimals: TEST_COIN_DECIMALS,
        dust_amount: UTXO_DUST_AMOUNT,
        tx_fee: TxFee::FixedPerKb(1000),
        rpc_client,
        priv_key_policy,
        hw_ctx: None,
        derivation_method,
        history_sync_state: Mutex::new(HistorySyncState::NotEnabled),
        tx_cache: DummyVerboseCache::default().into_shared(),
        block_headers_storage: None,
        recently_spent_outpoints: AsyncMutex::new(RecentlySpentOutPoints::new(my_script_pubkey)),
        tx_hash_algo: TxHashAlgo::DSHA256,
        check_utxo_maturity: false,
    }
}

pub(crate) fn utxo_coin_from_fields(coin: UtxoCoinFields) -> UtxoStandardCoin {
    let arc: UtxoArc = coin.into();
    arc.into()
}

/// A single-key software coin (Iguana derivation, key-pair policy) of `ticker`, for tests outside this module.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn software_coin_for_test(ticker: &str) -> UtxoStandardCoin {
    let mut fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(native_client_for_test()), None, false);
    fields.conf.ticker = ticker.to_owned();
    utxo_coin_from_fields(fields)
}

/// A coin of `ticker` under the Trezor key policy, for tests outside this module.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn trezor_policy_coin_for_test(ticker: &str) -> UtxoStandardCoin {
    let mut fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(native_client_for_test()), None, false);
    fields.conf.ticker = ticker.to_owned();
    fields.priv_key_policy = PrivKeyPolicy::Trezor;
    utxo_coin_from_fields(fields)
}

fn utxo_coin_for_test(
    rpc_client: UtxoRpcClientEnum,
    force_seed: Option<&str>,
    is_segwit_coin: bool,
) -> UtxoStandardCoin {
    utxo_coin_from_fields(utxo_coin_fields_for_test(rpc_client, force_seed, is_segwit_coin))
}

#[cfg(not(target_arch = "wasm32"))]
fn floor_test_coin(ticker: &str, fee: TxFee, dust: u64, decimals: u8) -> UtxoStandardCoin {
    let client = native_client_for_test();
    let mut fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(client), None, false);
    fields.conf.ticker = ticker.to_owned();
    fields.tx_fee = fee;
    fields.dust_amount = dust;
    fields.decimals = decimals;
    utxo_coin_from_fields(fields)
}

/// CRD ch.09 R17: the UTXO-style families do not opt in to watching until their watcher methods exist, so a node
/// neither subscribes to their watcher topics nor starts a watcher for their swaps.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn test_utxo_families_do_not_opt_in_to_watchers() {
    use crate::WatcherOps;

    let fields = || utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(native_client_for_test()), None, false);
    assert!(!utxo_coin_from_fields(fields()).is_supported_by_watchers());
    assert!(!QtumCoin::from(UtxoArc::new(fields())).is_supported_by_watchers());
    assert!(!crate::utxo::bch::bch_coin_without_network_for_test().is_supported_by_watchers());
}

/// T60.1: exact minimum trading volume values, ticker-independent (R38.6.6e, R38.6.6f).
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn test_utxo_min_trading_vol_values() {
    let dynamic = || TxFee::Dynamic(EstimateFeeMethod::Standard);
    let cases: Vec<(&str, TxFee, u64, &str)> = vec![
        ("KMD", TxFee::FixedPerKb(1000), 1000, "0.0001"),
        ("LTC", TxFee::FixedPerKb(1000), 5460, "0.000546"),
        ("DOGE", TxFee::FixedPerKb(1_000_000), 1_000_000, "0.1"),
        ("RVN", TxFee::FixedPerKb(1_000_000), 1000, "0.0496"),
        ("ZEC", TxFee::FixedPerKb(100_000), 1000, "0.00496"),
        ("ZEC", TxFee::FixedPerKb(70_000), 1000, "0.003472"),
        ("QTUM", TxFee::FixedPerKb(400_000), 72_800, "0.01984"),
        ("BTC", dynamic(), 1000, "0.0001"),
        ("BTC-segwit", dynamic(), 1000, "0.0001"),
        ("ARRR", TxFee::FixedPerKb(1000), 1000, "0.0001"),
        ("SYNTH-A", TxFee::FixedPerKb(1_001), 1000, "0.0001"),
        ("SYNTH-B", TxFee::FixedPerKb(30_001), 1000, "0.001488"),
        ("SYNTH-C", dynamic(), 5460, "0.000546"),
    ];
    for (ticker, fee, dust, expected) in cases {
        let coin = floor_test_coin(ticker, fee, dust, 8);
        assert_eq!(coin.min_trading_vol(), MmNumber::from(expected), "{ticker}");
    }
}

/// T60.2: the UTXO minimum transaction amount equals the dust.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn test_utxo_min_tx_amount_equals_dust() {
    let kmd = floor_test_coin("KMD", TxFee::FixedPerKb(1000), 1000, 8);
    assert_eq!(kmd.min_tx_amount(), BigDecimal::from_str("0.00001").unwrap());
    let ltc = floor_test_coin("LTC", TxFee::FixedPerKb(1000), 5460, 8);
    assert_eq!(ltc.min_tx_amount(), BigDecimal::from_str("0.0000546").unwrap());
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn test_trade_preimage_sender_address_uses_enabled_hd_address_for_hd_wallet() {
    let client = native_client_for_test();
    let mut fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(client), None, false);
    let activated_pubkey = *my_public_key(&fields).unwrap();
    let activated_address = address_from_pubkey(
        &activated_pubkey,
        fields.conf.pub_addr_prefix,
        fields.conf.pub_t_addr_prefix,
        fields.conf.checksum_type,
        fields.conf.bech32_hrp.clone(),
        UtxoAddressFormat::Standard,
    );
    let hd_account = UtxoHDAccount {
        account_id: 0,
        extended_pubkey: Secp256k1ExtendedPublicKey::from_str(
            "xpub6DEHSksajpRPM59RPw7Eg6PKdU7E2ehxJWtYdrfQ6JFmMGBsrR6jA78ANCLgzKYm4s5UqQ4ydLEYPbh3TRVvn5oAZVtWfi4qJLMntpZ8uGJ",
        )
        .unwrap(),
        account_derivation_path: HDPathToAccount::from_str("m/44'/141'/0'").unwrap(),
        external_addresses_number: 1,
        internal_addresses_number: 0,
    };
    let derived_pubkey = hd_account
        .extended_pubkey
        .derive_child(Bip44Chain::External.to_child_number())
        .unwrap()
        .derive_child(ChildNumber::from(0))
        .unwrap();
    let enabled_pubkey = Public::Compressed(H264::from(derived_pubkey.public_key().serialize()));
    let expected_enabled_address = address_from_pubkey(
        &enabled_pubkey,
        fields.conf.pub_addr_prefix,
        fields.conf.pub_t_addr_prefix,
        fields.conf.checksum_type,
        fields.conf.bech32_hrp.clone(),
        UtxoAddressFormat::Standard,
    );

    fields.derivation_method = DerivationMethod::HDWallet(UtxoHDWallet {
        hd_wallet_storage: HDWalletCoinStorage::default(),
        address_format: UtxoAddressFormat::Standard,
        derivation_path: HDPathToCoin::from_str("m/44'/141'").unwrap(),
        accounts: HDAccountsMutex::new(HDAccountsMap::from([(0, hd_account)])),
        gap_limit: 3,
    });

    let actual = block_on(trade_preimage_sender_address(&fields)).unwrap();
    assert_ne!(expected_enabled_address, activated_address);
    assert_eq!(actual, expected_enabled_address);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn test_v2_my_addr_uses_enabled_hd_address_for_hd_wallet() {
    let client = native_client_for_test();
    let mut fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(client), None, false);
    let activated_pubkey = *my_public_key(&fields).unwrap();
    let activated_address = address_from_pubkey(
        &activated_pubkey,
        fields.conf.pub_addr_prefix,
        fields.conf.pub_t_addr_prefix,
        fields.conf.checksum_type,
        fields.conf.bech32_hrp.clone(),
        UtxoAddressFormat::Standard,
    );
    let hd_account = UtxoHDAccount {
        account_id: 0,
        extended_pubkey: Secp256k1ExtendedPublicKey::from_str(
            "xpub6DEHSksajpRPM59RPw7Eg6PKdU7E2ehxJWtYdrfQ6JFmMGBsrR6jA78ANCLgzKYm4s5UqQ4ydLEYPbh3TRVvn5oAZVtWfi4qJLMntpZ8uGJ",
        )
        .unwrap(),
        account_derivation_path: HDPathToAccount::from_str("m/44'/141'/0'").unwrap(),
        external_addresses_number: 1,
        internal_addresses_number: 0,
    };
    let derived_pubkey = hd_account
        .extended_pubkey
        .derive_child(Bip44Chain::External.to_child_number())
        .unwrap()
        .derive_child(ChildNumber::from(0))
        .unwrap();
    let enabled_pubkey = Public::Compressed(H264::from(derived_pubkey.public_key().serialize()));
    let expected_enabled_address = address_from_pubkey(
        &enabled_pubkey,
        fields.conf.pub_addr_prefix,
        fields.conf.pub_t_addr_prefix,
        fields.conf.checksum_type,
        fields.conf.bech32_hrp.clone(),
        UtxoAddressFormat::Standard,
    );

    fields.derivation_method = DerivationMethod::HDWallet(UtxoHDWallet {
        hd_wallet_storage: HDWalletCoinStorage::default(),
        address_format: UtxoAddressFormat::Standard,
        derivation_path: HDPathToCoin::from_str("m/44'/141'").unwrap(),
        accounts: HDAccountsMutex::new(HDAccountsMap::from([(0, hd_account)])),
        gap_limit: 3,
    });

    let coin = utxo_coin_from_fields(fields);
    let actual = block_on(coin.try_my_addr()).unwrap();

    assert_ne!(expected_enabled_address, activated_address);
    assert_eq!(actual, expected_enabled_address);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn test_v2_try_my_addr_errors_without_enabled_hd_address() {
    let client = native_client_for_test();
    let mut fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(client), None, false);
    fields.priv_key_policy = PrivKeyPolicy::Trezor;
    fields.derivation_method = DerivationMethod::HDWallet(UtxoHDWallet {
        hd_wallet_storage: HDWalletCoinStorage::default(),
        address_format: UtxoAddressFormat::Standard,
        derivation_path: HDPathToCoin::from_str("m/44'/141'").unwrap(),
        accounts: HDAccountsMutex::new(HDAccountsMap::from([(
            0,
            UtxoHDAccount {
                account_id: 0,
                extended_pubkey: Secp256k1ExtendedPublicKey::from_str(
                    "xpub6DEHSksajpRPM59RPw7Eg6PKdU7E2ehxJWtYdrfQ6JFmMGBsrR6jA78ANCLgzKYm4s5UqQ4ydLEYPbh3TRVvn5oAZVtWfi4qJLMntpZ8uGJ",
                )
                .unwrap(),
                account_derivation_path: HDPathToAccount::from_str("m/44'/141'/0'").unwrap(),
                external_addresses_number: 0,
                internal_addresses_number: 0,
            },
        )])),
        gap_limit: 3,
    });

    let coin = utxo_coin_from_fields(fields);
    let err = block_on(coin.try_my_addr()).unwrap_err();
    assert!(err.contains("No enabled HD address found"));

    let err = coin.try_derive_htlc_pubkey_v2(b"swap-v2-unique-data").unwrap_err();
    assert!(err.contains("No enabled HD address found"));
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn test_v2_trezor_accessors_use_enabled_hardware_hd_address() {
    let client = native_client_for_test();
    let mut fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(client), None, false);
    fields.priv_key_policy = PrivKeyPolicy::Trezor;
    fields.conf.trezor_coin = Some(TrezorUtxoCoin::Komodo);
    let hd_account = UtxoHDAccount {
        account_id: 0,
        extended_pubkey: Secp256k1ExtendedPublicKey::from_str(
            "xpub6DEHSksajpRPM59RPw7Eg6PKdU7E2ehxJWtYdrfQ6JFmMGBsrR6jA78ANCLgzKYm4s5UqQ4ydLEYPbh3TRVvn5oAZVtWfi4qJLMntpZ8uGJ",
        )
        .unwrap(),
        account_derivation_path: HDPathToAccount::from_str("m/44'/141'/0'").unwrap(),
        external_addresses_number: 1,
        internal_addresses_number: 0,
    };
    let derived_pubkey = hd_account
        .extended_pubkey
        .derive_child(Bip44Chain::External.to_child_number())
        .unwrap()
        .derive_child(ChildNumber::from(0))
        .unwrap();
    let enabled_pubkey = Public::Compressed(H264::from(derived_pubkey.public_key().serialize()));
    let expected_enabled_address = address_from_pubkey(
        &enabled_pubkey,
        fields.conf.pub_addr_prefix,
        fields.conf.pub_t_addr_prefix,
        fields.conf.checksum_type,
        fields.conf.bech32_hrp.clone(),
        UtxoAddressFormat::Standard,
    );
    fields.derivation_method = DerivationMethod::HDWallet(UtxoHDWallet {
        hd_wallet_storage: HDWalletCoinStorage::default(),
        address_format: UtxoAddressFormat::Standard,
        derivation_path: HDPathToCoin::from_str("m/44'/141'").unwrap(),
        accounts: HDAccountsMutex::new(HDAccountsMap::from([(0, hd_account)])),
        gap_limit: 3,
    });

    let coin = utxo_coin_from_fields(fields);

    let actual_addr = block_on(coin.try_my_addr()).unwrap();
    assert_eq!(actual_addr, expected_enabled_address);

    let actual_pubkey = coin.try_derive_htlc_pubkey_v2(b"swap-v2-unique-data").unwrap();
    assert_eq!(actual_pubkey, enabled_pubkey);
    assert_eq!(
        coin.try_derive_htlc_pubkey_v2_bytes(b"swap-v2-unique-data").unwrap(),
        enabled_pubkey.to_vec()
    );
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn test_v2_trezor_enabled_pubkey_tracks_enabled_hardware_address_record() {
    let client = native_client_for_test();
    let mut first = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(client.clone()), None, false);
    first.priv_key_policy = PrivKeyPolicy::Trezor;
    first.conf.trezor_coin = Some(TrezorUtxoCoin::Komodo);
    first.derivation_method = DerivationMethod::HDWallet(UtxoHDWallet {
        hd_wallet_storage: HDWalletCoinStorage::default(),
        address_format: UtxoAddressFormat::Standard,
        derivation_path: HDPathToCoin::from_str("m/44'/141'").unwrap(),
        accounts: HDAccountsMutex::new(HDAccountsMap::from([(
            0,
            UtxoHDAccount {
                account_id: 0,
                extended_pubkey: Secp256k1ExtendedPublicKey::from_str(
                    "xpub6DEHSksajpRPM59RPw7Eg6PKdU7E2ehxJWtYdrfQ6JFmMGBsrR6jA78ANCLgzKYm4s5UqQ4ydLEYPbh3TRVvn5oAZVtWfi4qJLMntpZ8uGJ",
                )
                .unwrap(),
                account_derivation_path: HDPathToAccount::from_str("m/44'/141'/0'").unwrap(),
                external_addresses_number: 1,
                internal_addresses_number: 0,
            },
        )])),
        gap_limit: 3,
    });

    let mut second = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(client), None, false);
    second.priv_key_policy = PrivKeyPolicy::Trezor;
    second.conf.trezor_coin = Some(TrezorUtxoCoin::Komodo);
    second.derivation_method = DerivationMethod::HDWallet(UtxoHDWallet {
        hd_wallet_storage: HDWalletCoinStorage::default(),
        address_format: UtxoAddressFormat::Standard,
        derivation_path: HDPathToCoin::from_str("m/44'/141'").unwrap(),
        accounts: HDAccountsMutex::new(HDAccountsMap::from([(
            0,
            UtxoHDAccount {
                account_id: 0,
                extended_pubkey: Secp256k1ExtendedPublicKey::from_str(
                    "xpub6DEHSksajpRPQq2FdGT6JoieiQZUpTZ3WZn8fcuLJhFVmtCpXbuXxp5aPzaokwcLV2V9LE55Dwt8JYkpuMv7jXKwmyD28WbHYjBH2zhbW2p",
                )
                .unwrap(),
                account_derivation_path: HDPathToAccount::from_str("m/44'/141'/1'").unwrap(),
                external_addresses_number: 1,
                internal_addresses_number: 0,
            },
        )])),
        gap_limit: 3,
    });

    let first_pubkey = utxo_coin_from_fields(first)
        .try_derive_htlc_pubkey_v2(b"swap-v2-unique-data")
        .unwrap();
    let second_pubkey = utxo_coin_from_fields(second)
        .try_derive_htlc_pubkey_v2(b"swap-v2-unique-data")
        .unwrap();

    assert_ne!(first_pubkey, second_pubkey);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn test_v2_trezor_htlc_keypair_reports_unsupported_script_mode() {
    let client = native_client_for_test();
    let mut fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(client), None, false);
    fields.priv_key_policy = PrivKeyPolicy::Trezor;
    fields.conf.trezor_coin = Some(TrezorUtxoCoin::Komodo);

    let coin = utxo_coin_from_fields(fields);
    let err = get_htlc_key_pair_v2(&coin, b"swap-v2-unique-data").unwrap_err();
    assert!(err.contains("hardware_wallet:unsupported_script_signing_mode"));
    assert!(!err.contains("deferred"));
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_extract_secret() {
    let client = electrum_client_for_test(DOC_ELECTRUM_ADDRS);
    let coin = utxo_coin_for_test(client.into(), None, false);

    let tx_hex = hex::decode("0100000001de7aa8d29524906b2b54ee2e0281f3607f75662cbc9080df81d1047b78e21dbc00000000d7473044022079b6c50820040b1fbbe9251ced32ab334d33830f6f8d0bf0a40c7f1336b67d5b0220142ccf723ddabb34e542ed65c395abc1fbf5b6c3e730396f15d25c49b668a1a401209da937e5609680cb30bff4a7661364ca1d1851c2506fa80c443f00a3d3bf7365004c6b6304f62b0e5cb175210270e75970bb20029b3879ec76c4acd320a8d0589e003636264d01a7d566504bfbac6782012088a9142fb610d856c19fd57f2d0cffe8dff689074b3d8a882103f368228456c940ac113e53dad5c104cf209f2f102a409207269383b6ab9b03deac68ffffffff01d0dc9800000000001976a9146d9d2b554d768232320587df75c4338ecc8bf37d88ac40280e5c").unwrap();
    let expected_secret = hex::decode("9da937e5609680cb30bff4a7661364ca1d1851c2506fa80c443f00a3d3bf7365").unwrap();
    let secret_hash = &*dhash160(&expected_secret);
    let secret = coin.extract_secret(secret_hash, &tx_hex).unwrap();
    assert_eq!(secret, expected_secret);
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_send_maker_spends_taker_payment_recoverable_tx() {
    let client = electrum_client_for_test(DOC_ELECTRUM_ADDRS);
    let coin = utxo_coin_for_test(client.into(), None, false);
    let tx_hex = hex::decode("0100000001de7aa8d29524906b2b54ee2e0281f3607f75662cbc9080df81d1047b78e21dbc00000000d7473044022079b6c50820040b1fbbe9251ced32ab334d33830f6f8d0bf0a40c7f1336b67d5b0220142ccf723ddabb34e542ed65c395abc1fbf5b6c3e730396f15d25c49b668a1a401209da937e5609680cb30bff4a7661364ca1d1851c2506fa80c443f00a3d3bf7365004c6b6304f62b0e5cb175210270e75970bb20029b3879ec76c4acd320a8d0589e003636264d01a7d566504bfbac6782012088a9142fb610d856c19fd57f2d0cffe8dff689074b3d8a882103f368228456c940ac113e53dad5c104cf209f2f102a409207269383b6ab9b03deac68ffffffff01d0dc9800000000001976a9146d9d2b554d768232320587df75c4338ecc8bf37d88ac40280e5c").unwrap();
    let secret = hex::decode("9da937e5609680cb30bff4a7661364ca1d1851c2506fa80c443f00a3d3bf7365").unwrap();
    let keypair = key_pair_from_seed("").unwrap();

    let tx_err = coin
        .send_maker_spends_taker_payment(
            &tx_hex,
            777,
            &coin.my_public_key().unwrap().to_vec(),
            &secret,
            &*keypair.private().secret,
            &coin.swap_contract_address(),
        )
        .wait()
        .unwrap_err();

    let tx: UtxoTx = deserialize(tx_hex.as_slice()).unwrap();

    // The error variant should equal to `TxRecoverable`
    assert_eq!(
        discriminant(&tx_err),
        discriminant(&TransactionErr::TxRecoverable(TransactionEnum::from(tx), String::new()))
    );
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_generate_transaction() {
    let client = electrum_client_for_test(DOC_ELECTRUM_ADDRS);
    let coin = utxo_coin_for_test(client.into(), None, false);
    let unspents = vec![UnspentInfo {
        value: 10000000000,
        outpoint: OutPoint::default(),
        height: Default::default(),
    }];

    let outputs = vec![TransactionOutput {
        script_pubkey: vec![].into(),
        value: 999,
    }];

    let builder = UtxoTxBuilder::new(&coin)
        .add_available_inputs(unspents)
        .add_outputs(outputs);
    let generated = block_on(builder.build());
    // must not allow to use output with value < dust
    generated.unwrap_err();

    let unspents = vec![UnspentInfo {
        value: 100000,
        outpoint: OutPoint::default(),
        height: Default::default(),
    }];

    let outputs = vec![TransactionOutput {
        script_pubkey: vec![].into(),
        value: 98900,
    }];

    let builder = UtxoTxBuilder::new(&coin)
        .add_available_inputs(unspents)
        .add_outputs(outputs);
    let generated = block_on(builder.build()).unwrap();
    // the change that is less than dust must be included to miner fee
    // so no extra outputs should appear in generated transaction
    assert_eq!(generated.0.outputs.len(), 1);

    // plain fixed 1,000 per kB charges the transaction's own 186 bytes, not a whole kilobyte (R38.6.6b)
    assert_eq!(generated.1.fee_amount, 186);
    assert_eq!(generated.1.unused_change, Some(914));
    assert_eq!(generated.1.received_by_me, 0);
    assert_eq!(generated.1.spent_by_me, 100000);

    let unspents = vec![UnspentInfo {
        value: 100000,
        outpoint: OutPoint::default(),
        height: Default::default(),
    }];

    let outputs = vec![TransactionOutput {
        script_pubkey: Builder::build_p2pkh(&coin.as_ref().derivation_method.unwrap_iguana().hash).to_bytes(),
        value: 100000,
    }];

    // test that fee is properly deducted from output amount equal to input amount (max withdraw case)
    let builder = UtxoTxBuilder::new(&coin)
        .add_available_inputs(unspents)
        .add_outputs(outputs)
        .with_fee_policy(FeePolicy::DeductFromOutput(0));

    let generated = block_on(builder.build()).unwrap();
    assert_eq!(generated.0.outputs.len(), 1);

    assert_eq!(generated.1.fee_amount, 211);
    assert_eq!(generated.1.unused_change, None);
    assert_eq!(generated.1.received_by_me, 99789);
    assert_eq!(generated.1.spent_by_me, 100000);
    assert_eq!(generated.0.outputs[0].value, 99789);

    let unspents = vec![UnspentInfo {
        value: 100000,
        outpoint: OutPoint::default(),
        height: Default::default(),
    }];

    let outputs = vec![TransactionOutput {
        script_pubkey: vec![].into(),
        value: 100000,
    }];

    // test that generate_transaction returns an error when input amount is not sufficient to cover output + fee
    let builder = UtxoTxBuilder::new(&coin)
        .add_available_inputs(unspents)
        .add_outputs(outputs);

    block_on(builder.build()).unwrap_err();
}

/// The fixed-rate, 8-decimal fixture of the R38.6.6n builder tests: 1,000 base units per kilobyte, dust 1,000.
#[cfg(not(target_arch = "wasm32"))]
fn required_inputs_test_coin() -> UtxoStandardCoin { floor_test_coin("TEST", TxFee::FixedPerKb(1000), 1000, 8) }

#[cfg(not(target_arch = "wasm32"))]
fn required_inputs_test_unspent(seed: u8, value: u64) -> UnspentInfo {
    UnspentInfo {
        value,
        outpoint: OutPoint {
            hash: H256::from([seed; 32]),
            index: 0,
        },
        height: Default::default(),
    }
}

/// T38.6.6n: the value of a required input counts toward the sufficiency check, the change, `spent_by_me` and the
/// reported fee, so that the fee equals the inputs minus the outputs and the input's value returns as change.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_generate_transaction_required_input_value_counts_r38_6_6n() {
    let coin = required_inputs_test_coin();
    let outputs = vec![TransactionOutput {
        script_pubkey: vec![].into(),
        value: 3000,
    }];
    let builder = UtxoTxBuilder::new(&coin)
        .add_required_inputs(vec![required_inputs_test_unspent(1, 2000)])
        .add_available_inputs(vec![required_inputs_test_unspent(2, 100_000)])
        .add_outputs(outputs);
    let (tx, data) = block_on(builder.build()).unwrap();

    // The required input first, then the one available input the outputs and fee needed on top of it.
    assert_eq!(tx.inputs.len(), 2);
    assert_eq!(tx.inputs[0].amount, 2000);
    let inputs_sum = 102_000;
    let outputs_sum: u64 = tx.outputs.iter().map(|output| output.value).sum();
    // The payment and the change output, which includes the required input's 2,000.
    assert_eq!(tx.outputs.len(), 2);
    assert_eq!(tx.outputs[1].value, inputs_sum - 3000 - data.fee_amount);
    assert_eq!(data.spent_by_me, inputs_sum);
    assert_eq!(data.received_by_me, tx.outputs[1].value);
    assert_eq!(data.unused_change, None);
    // The fee reported is the inputs minus the outputs.
    assert_eq!(data.fee_amount, inputs_sum - outputs_sum);
}

/// T38.6.6n: required inputs that cover the outputs and the fee on their own are enough; no available input is
/// added, and the fee is computed for the transaction as built.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_generate_transaction_required_inputs_alone_suffice_r38_6_6n() {
    let coin = required_inputs_test_coin();
    let outputs = || {
        vec![TransactionOutput {
            script_pubkey: vec![].into(),
            value: 3000,
        }]
    };
    for with_available in [false, true] {
        let mut builder = UtxoTxBuilder::new(&coin)
            .add_required_inputs(vec![required_inputs_test_unspent(1, 100_000)])
            .add_outputs(outputs());
        if with_available {
            builder = builder.add_available_inputs(vec![required_inputs_test_unspent(2, 50_000)]);
        }
        let (tx, data) = block_on(builder.build()).unwrap();
        assert_eq!(tx.inputs.len(), 1, "available inputs: {with_available}");
        let outputs_sum: u64 = tx.outputs.iter().map(|output| output.value).sum();
        assert_eq!(tx.outputs.len(), 2);
        assert_eq!(data.spent_by_me, 100_000);
        assert!(data.fee_amount > 0);
        assert_eq!(data.fee_amount, 100_000 - outputs_sum);
    }

    // Required inputs that do not cover the outputs, with nothing available, are not enough.
    let builder = UtxoTxBuilder::new(&coin)
        .add_required_inputs(vec![required_inputs_test_unspent(1, 2000)])
        .add_outputs(outputs());
    assert!(matches!(
        block_on(builder.build()).unwrap_err().into_inner(),
        GenerateTxError::NotEnoughUtxos { sum_utxos: 2000, .. }
    ));
}

/// T38.6.6n: a change exactly equal to the dust amount becomes a change output; only a change strictly below dust
/// is left to the miner.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_generate_transaction_change_equal_to_dust_is_an_output_r38_6_6n() {
    let coin = required_inputs_test_coin();
    // One input and an empty-script payment make a 186-byte transaction, 220 bytes with the change output, so the
    // fee with a change output is 220 base units.
    let build = |value: u64| {
        let builder = UtxoTxBuilder::new(&coin)
            .add_available_inputs(vec![required_inputs_test_unspent(1, 100_000)])
            .add_outputs(vec![TransactionOutput {
                script_pubkey: vec![].into(),
                value,
            }]);
        block_on(builder.build()).unwrap()
    };

    // 100,000 - 98,780 - 220 = 1,000: the change is the dust amount exactly.
    let (tx, data) = build(98_780);
    assert_eq!(data.fee_amount, 220);
    assert_eq!(tx.outputs.len(), 2);
    assert_eq!(tx.outputs[1].value, 1000);
    assert_eq!(data.received_by_me, 1000);
    assert_eq!(data.unused_change, None);

    // One base unit less is below dust: no change output, the remainder is left to the miner.
    let (tx, data) = build(98_781);
    assert_eq!(tx.outputs.len(), 1);
    assert_eq!(data.unused_change, Some(999));
    assert_eq!(data.received_by_me, 0);
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_addresses_from_script() {
    let client = electrum_client_for_test(DOC_ELECTRUM_ADDRS);
    let coin = utxo_coin_for_test(client.into(), None, false);
    // P2PKH
    let script: Script = "76a91405aab5342166f8594baf17a7d9bef5d56744332788ac".into();
    let expected_addr: Vec<Address> = vec!["R9o9xTocqr6CeEDGDH6mEYpwLoMz6jNjMW".into()];
    let actual_addr = coin.addresses_from_script(&script).unwrap();
    assert_eq!(expected_addr, actual_addr);

    // P2SH
    let script: Script = "a914e71a6120653ebd526e0f9d7a29cde5969db362d487".into();
    let expected_addr: Vec<Address> = vec!["bZoEPR7DjTqSDiQTeRFNDJuQPTRY2335LD".into()];
    let actual_addr = coin.addresses_from_script(&script).unwrap();
    assert_eq!(expected_addr, actual_addr);
}

#[test]
fn test_kmd_interest() {
    let height = Some(1000001);
    let value = 64605500822;
    let lock_time = 1556623906;
    let current_time = 1556623906 + 3600 + 300;

    let expected = 36870;
    let actual = kmd_interest(height, value, lock_time, current_time).unwrap();
    assert_eq!(expected, actual);

    // UTXO amount must be at least 10 KMD to be eligible for interest
    let actual = kmd_interest(height, 999999999, lock_time, current_time);
    assert_eq!(actual, Err(KmdRewardsNotAccruedReason::UtxoAmountLessThanTen));

    // Transaction is not mined yet (height is None)
    let actual = kmd_interest(None, value, lock_time, current_time);
    assert_eq!(actual, Err(KmdRewardsNotAccruedReason::TransactionInMempool));

    // Locktime is not set
    let actual = kmd_interest(height, value, 0, current_time);
    assert_eq!(actual, Err(KmdRewardsNotAccruedReason::LocktimeNotSet));

    // interest will stop accrue after block 7_777_777
    let actual = kmd_interest(Some(7_777_778), value, lock_time, current_time);
    assert_eq!(actual, Err(KmdRewardsNotAccruedReason::UtxoHeightGreaterThanEndOfEra));

    // interest doesn't accrue for lock_time < 500_000_000
    let actual = kmd_interest(height, value, 499_999_999, current_time);
    assert_eq!(actual, Err(KmdRewardsNotAccruedReason::LocktimeLessThanThreshold));

    // current time must be greater than tx lock_time
    let actual = kmd_interest(height, value, lock_time, lock_time - 1);
    assert_eq!(actual, Err(KmdRewardsNotAccruedReason::OneHourNotPassedYet));

    // at least 1 hour should pass
    let actual = kmd_interest(height, value, lock_time, lock_time + 30);
    assert_eq!(actual, Err(KmdRewardsNotAccruedReason::OneHourNotPassedYet));

    // CRD ch.38 R38.4.3 -- KIP-0001 reward reduction (issue #11).
    // Issue #11 vector, post-fork: the pre-reduction figure (1,744,260) MUST NOT
    // be produced; the KIP-0001-reduced figure (3,488) MUST be.
    let issue11_value = 1_473_232_138;
    let issue11_height = Some(5_108_257);
    let issue11_lock_time = 1_789_721_423;
    let issue11_ref_time = 1_790_472_552;
    let actual = kmd_interest(issue11_height, issue11_value, issue11_lock_time, issue11_ref_time).unwrap();
    assert_eq!(actual, 3_488);

    // Same vector, pre-fork height: the reward keeps the pre-reduction figure.
    let actual = kmd_interest(Some(3_484_957), issue11_value, issue11_lock_time, issue11_ref_time).unwrap();
    assert_eq!(actual, 1_744_260);

    // Fork boundary: height exactly 3,484,958 already applies the reduction.
    let actual = kmd_interest(Some(3_484_958), issue11_value, issue11_lock_time, issue11_ref_time).unwrap();
    assert_eq!(actual, 3_488);
}

#[test]
fn test_kmd_interest_accrue_stop_at() {
    let lock_time = 1595845640;
    let height = 1000001;

    let expected = lock_time + 31 * 24 * 60 * 60;
    let actual = kmd_interest_accrue_stop_at(height, lock_time);
    assert_eq!(expected, actual);

    let height = 999999;

    let expected = lock_time + 365 * 24 * 60 * 60;
    let actual = kmd_interest_accrue_stop_at(height, lock_time);
    assert_eq!(expected, actual);
}

#[test]
fn test_sat_from_big_decimal() {
    let amount = "0.000001".parse().unwrap();
    let sat = sat_from_big_decimal(&amount, 18).unwrap();
    let expected_sat = 1000000000000;
    assert_eq!(expected_sat, sat);

    let amount = "0.12345678".parse().unwrap();
    let sat = sat_from_big_decimal(&amount, 8).unwrap();
    let expected_sat = 12345678;
    assert_eq!(expected_sat, sat);

    let amount = "1.000001".parse().unwrap();
    let sat = sat_from_big_decimal(&amount, 18).unwrap();
    let expected_sat = 1000001000000000000;
    assert_eq!(expected_sat, sat);

    let amount = 1.into();
    let sat = sat_from_big_decimal(&amount, 18).unwrap();
    let expected_sat = 1000000000000000000;
    assert_eq!(expected_sat, sat);

    let amount = "0.000000000000000001".parse().unwrap();
    let sat = sat_from_big_decimal(&amount, 18).unwrap();
    let expected_sat = 1u64;
    assert_eq!(expected_sat, sat);

    let amount = 1234.into();
    let sat = sat_from_big_decimal(&amount, 9).unwrap();
    let expected_sat = 1234000000000;
    assert_eq!(expected_sat, sat);

    let amount = 1234.into();
    let sat = sat_from_big_decimal(&amount, 0).unwrap();
    let expected_sat = 1234;
    assert_eq!(expected_sat, sat);

    let amount = 1234.into();
    let sat = sat_from_big_decimal(&amount, 1).unwrap();
    let expected_sat = 12340;
    assert_eq!(expected_sat, sat);

    let amount = "1234.12345".parse().unwrap();
    let sat = sat_from_big_decimal(&amount, 1).unwrap();
    let expected_sat = 12341;
    assert_eq!(expected_sat, sat);
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_wait_for_payment_spend_timeout_native() {
    let client = NativeClientImpl::default();

    static mut OUTPUT_SPEND_CALLED: bool = false;
    NativeClient::find_output_spend.mock_safe(|_, _, _, _, _| {
        unsafe { OUTPUT_SPEND_CALLED = true };
        MockResult::Return(Box::new(futures01::future::ok(None)))
    });
    let client = UtxoRpcClientEnum::Native(NativeClient(Arc::new(client)));
    let coin = utxo_coin_for_test(client, None, false);
    let transaction = hex::decode("01000000000102fff7f7881a8099afa6940d42d1e7f6362bec38171ea3edf433541db4e4ad969f00000000494830450221008b9d1dc26ba6a9cb62127b02742fa9d754cd3bebf337f7a55d114c8e5cdd30be022040529b194ba3f9281a99f2b1c0a19c0489bc22ede944ccf4ecbab4cc618ef3ed01eeffffffef51e1b804cc89d182d279655c3aa89e815b1b309fe287d9b2b55d57b90ec68a0100000000ffffffff02202cb206000000001976a9148280b37df378db99f66f85c95a783a76ac7a6d5988ac9093510d000000001976a9143bde42dbee7e4dbe6a21b2d50ce2f0167faa815988ac000247304402203609e17b84f6a7d30c80bfa610b5b4542f32a8a0d5447a12fb1366d7f01cc44a0220573a954c4518331561406f90300e8f3358f51928d43c212a8caed02de67eebee0121025476c2e83188368da1ff3e292e7acafcdb3566bb0ad253f62fc70f07aeee635711000000")
        .unwrap();
    let wait_until = now_ms() / 1000 - 1;
    let from_block = 1000;

    assert!(coin
        .wait_for_tx_spend(&transaction, wait_until, from_block, &None)
        .wait()
        .is_err());
    assert!(unsafe { OUTPUT_SPEND_CALLED });
}

#[test]
fn test_wait_for_payment_spend_timeout_electrum() {
    static mut OUTPUT_SPEND_CALLED: bool = false;
    ElectrumClient::find_output_spend.mock_safe(|_, _, _, _, _| {
        unsafe { OUTPUT_SPEND_CALLED = true };
        MockResult::Return(Box::new(futures01::future::ok(None)))
    });

    let client = ElectrumClientImpl::new(TEST_COIN_NAME.into(), Default::default());
    let client = UtxoRpcClientEnum::Electrum(ElectrumClient(Arc::new(client)));
    let coin = utxo_coin_for_test(client, None, false);
    let transaction = hex::decode("01000000000102fff7f7881a8099afa6940d42d1e7f6362bec38171ea3edf433541db4e4ad969f00000000494830450221008b9d1dc26ba6a9cb62127b02742fa9d754cd3bebf337f7a55d114c8e5cdd30be022040529b194ba3f9281a99f2b1c0a19c0489bc22ede944ccf4ecbab4cc618ef3ed01eeffffffef51e1b804cc89d182d279655c3aa89e815b1b309fe287d9b2b55d57b90ec68a0100000000ffffffff02202cb206000000001976a9148280b37df378db99f66f85c95a783a76ac7a6d5988ac9093510d000000001976a9143bde42dbee7e4dbe6a21b2d50ce2f0167faa815988ac000247304402203609e17b84f6a7d30c80bfa610b5b4542f32a8a0d5447a12fb1366d7f01cc44a0220573a954c4518331561406f90300e8f3358f51928d43c212a8caed02de67eebee0121025476c2e83188368da1ff3e292e7acafcdb3566bb0ad253f62fc70f07aeee635711000000")
        .unwrap();
    let wait_until = now_ms() / 1000 - 1;
    let from_block = 1000;

    assert!(coin
        .wait_for_tx_spend(&transaction, wait_until, from_block, &None)
        .wait()
        .is_err());
    assert!(unsafe { OUTPUT_SPEND_CALLED });
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_search_for_swap_tx_spend_electrum_was_spent() {
    let client = electrum_client_for_test(DOC_ELECTRUM_ADDRS);
    let coin = utxo_coin_for_test(
        client.into(),
        Some("spice describe gravity federal blast come thank unfair canal monkey style afraid"),
        false,
    );

    // A DOC swap payment made by this key (DOC height 478093), spent by the counterparty,
    // which revealed the secret: payment 9f817411b36b9a57847d3ed9f04eee73a0239af2590e312d00d4361059706aa7,
    // spend e469afa4933bb306124b09fa25ca6e449c82b499b450bec19670189b21f9585b.
    let secret = hex::decode("23f843429d0c94d4cfc338d566f4b77ba9085aa43f1def3bb11de81f58628036").unwrap();
    let other_pub = hex::decode("02d74dc5ec4c823f40dae5c563d7b22aab52c80f9f18226f47ea6d83107618df62").unwrap();
    let payment_tx_bytes = hex::decode("0400008085202f8901b91fa7b21c70acf4209821df8227b97585f56b873d2b56d6dce3a66258f1cdc2020000006a473044022019711a750e1cad2a969f41415363bd53de3600511e19192b71b1ec151998a2690220154c1a2c43cee2edb777beb043f901b3dc0ba8590549576f5f600d74cbf7fdc1012102031d4256c4bc9f99ac88bf3dba21773132281f65f9bf23a59928bce08961e2f3ffffffff03404b4c000000000017a914563b96f3c2b757f5013b84c9276bdbd71d0a9205870000000000000000166a14b2788846eca0392f3adbb8751d7703a0a03f948ee2f83a7b000000001976a91405aab5342166f8594baf17a7d9bef5d56744332788ac8513fc65000000000000000000000000000000")
        .unwrap();
    let spend_tx_bytes = hex::decode("0400008085202f8901a76a70591036d4002d310e59f29a23a073ee4ef0d93e7d84579a6bb31174819f00000000d74730440220167039ebed8a54a10ec21e42ec1593c159f8046d73884ecacf64f702add54c4d0220229451757215fbe92673d76949e461230f26e9331d52f85cfdc2e71e000b8e32012023f843429d0c94d4cfc338d566f4b77ba9085aa43f1def3bb11de81f58628036004c6b63047250fc65b1752102031d4256c4bc9f99ac88bf3dba21773132281f65f9bf23a59928bce08961e2f3ac6782012088a914b2788846eca0392f3adbb8751d7703a0a03f948e882102d74dc5ec4c823f40dae5c563d7b22aab52c80f9f18226f47ea6d83107618df62ac68ffffffff0158474c00000000001976a914f26650dc9aa4e4505978ad635cdb15491cee70e188ac7250fc65000000000000000000000000000000")
        .unwrap();
    let spend_tx = TransactionEnum::UtxoTx(deserialize(spend_tx_bytes.as_slice()).unwrap());

    let found = block_on(coin.search_for_swap_tx_spend_my(
        1711034482,
        coin.my_public_key().unwrap(),
        &other_pub,
        &*dhash160(&secret),
        &payment_tx_bytes,
        0,
        &None,
    ))
    .unwrap()
    .unwrap();
    assert_eq!(FoundSwapTxSpend::Spent(spend_tx), found);
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_search_for_swap_tx_spend_electrum_was_refunded() {
    let client = electrum_client_for_test(DOC_ELECTRUM_ADDRS);
    let coin = utxo_coin_for_test(
        client.into(),
        Some("spice describe gravity federal blast come thank unfair canal monkey style afraid"),
        false,
    );

    // A DOC swap payment made by this key (DOC height 480241) that the counterparty never
    // claimed, refunded after its lock time: payment ba08822b2da3f7120f5ad90cd999d5f0f4be4a63de496f4f76af202c68e4f5eb,
    // refund 1b4e30f0e3101374464ec159c2f35b03412034fadee8c5769e8adcd5c91359bd.
    let secret_hash = hex::decode("7a752434d4564c11b9333743122dab3a0aa21bd9").unwrap();
    let other_pub = hex::decode("02d74dc5ec4c823f40dae5c563d7b22aab52c80f9f18226f47ea6d83107618df62").unwrap();
    let payment_tx_bytes = hex::decode("0400008085202f8901b2781d994b79be8e1f687a7f376109063bbe8b51ab36d04b35d4ff437b21d2a5010000006a47304402201832294ceb2b62a197bc2049218dcee69dcabb414249403efcc35ad65c17e73d0220348fbbb2b40880408bdef604e5a6cba34f514b84d5d6a9a4c4713669cd765fd2012102031d4256c4bc9f99ac88bf3dba21773132281f65f9bf23a59928bce08961e2f3ffffffff0320a107000000000017a91489dd2a32ae17a0575759581afb0002176296777b870000000000000000166a147a752434d4564c11b9333743122dab3a0aa21bd94af2de7a000000001976a91405aab5342166f8594baf17a7d9bef5d56744332788ac2b0efe65000000000000000000000000000000")
        .unwrap();
    let refund_tx_bytes = hex::decode("0400008085202f8901ebf5e4682c20af764f6f49de634abef4f0d599d90cd95a0f12f7a32d2b8208ba00000000b647304402205bd140728b1b6de7b891025873d552a439c488dd7fe4234fd982eaa193e5776602205ad9fea8bc771d94de9d2fdc450186ea21f4fc81f28c2f2f75b5d2ad84891d8f01514c6b63049f0efe65b1752102031d4256c4bc9f99ac88bf3dba21773132281f65f9bf23a59928bce08961e2f3ac6782012088a9147a752434d4564c11b9333743122dab3a0aa21bd9882102d74dc5ec4c823f40dae5c563d7b22aab52c80f9f18226f47ea6d83107618df62ac68feffffff01389d0700000000001976a91405aab5342166f8594baf17a7d9bef5d56744332788acb80efe65000000000000000000000000000000")
        .unwrap();
    let refund_tx = TransactionEnum::UtxoTx(deserialize(refund_tx_bytes.as_slice()).unwrap());

    let found = block_on(coin.search_for_swap_tx_spend_my(
        1711148703,
        coin.my_public_key().unwrap(),
        &other_pub,
        &secret_hash,
        &payment_tx_bytes,
        0,
        &None,
    ))
    .unwrap()
    .unwrap();
    assert_eq!(FoundSwapTxSpend::Refunded(refund_tx), found);
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_withdraw_impl_set_fixed_fee() {
    UtxoStandardCoin::get_unspent_ordered_list.mock_safe(|coin, _| {
        let cache = block_on(coin.as_ref().recently_spent_outpoints.lock());
        let unspents = vec![UnspentInfo {
            outpoint: OutPoint {
                hash: 1.into(),
                index: 0,
            },
            value: 1000000000,
            height: Default::default(),
        }];
        MockResult::Return(Box::pin(futures::future::ok((unspents, cache))))
    });

    let client = NativeClient(Arc::new(NativeClientImpl::default()));

    let coin = utxo_coin_for_test(UtxoRpcClientEnum::Native(client), None, false);

    let withdraw_req = WithdrawRequest {
        amount: 1u64.into(),
        from: None,
        to: "RQq6fWoy8aGGMLjvRfMY5mBNVm2RQxJyLa".to_string(),
        coin: TEST_COIN_NAME.into(),
        max: false,
        fee: Some(WithdrawFee::UtxoFixed {
            amount: "0.1".parse().unwrap(),
        }),
    };
    // An explicit `UtxoFixed` fee is a rate per 1,000 bytes, applied exactly like `UtxoPerKbyte`:
    // floor(a * s / 1000), reference arithmetic (CRD ch.38 R38.6.6l item 2, maintainer decision 27).
    // The resulting transaction size might be 244 or 245 bytes depending on signature size
    // MM2 always expects the worst case during fee calculation
    // 0.1 * 245 / 1000 ~ 0.0245
    let expected = Some(
        UtxoFeeDetails {
            coin: Some(TEST_COIN_NAME.into()),
            amount: "0.0245".parse().unwrap(),
        }
        .into(),
    );
    let tx_details = coin.withdraw(withdraw_req).wait().unwrap();
    assert_eq!(expected, tx_details.fee_details);
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_withdraw_impl_sat_per_kb_fee() {
    UtxoStandardCoin::get_unspent_ordered_list.mock_safe(|coin, _| {
        let cache = block_on(coin.as_ref().recently_spent_outpoints.lock());
        let unspents = vec![UnspentInfo {
            outpoint: OutPoint {
                hash: 1.into(),
                index: 0,
            },
            value: 1000000000,
            height: Default::default(),
        }];
        MockResult::Return(Box::pin(futures::future::ok((unspents, cache))))
    });

    let client = NativeClient(Arc::new(NativeClientImpl::default()));

    let coin = utxo_coin_for_test(UtxoRpcClientEnum::Native(client), None, false);

    let withdraw_req = WithdrawRequest {
        amount: 1u64.into(),
        from: None,
        to: "RQq6fWoy8aGGMLjvRfMY5mBNVm2RQxJyLa".to_string(),
        coin: TEST_COIN_NAME.into(),
        max: false,
        fee: Some(WithdrawFee::UtxoPerKbyte {
            amount: "0.1".parse().unwrap(),
        }),
    };
    // The resulting transaction size might be 244 or 245 bytes depending on signature size
    // MM2 always expects the worst case during fee calculation
    // 0.1 * 245 / 1000 ~ 0.0245
    let expected = Some(
        UtxoFeeDetails {
            coin: Some(TEST_COIN_NAME.into()),
            amount: "0.0245".parse().unwrap(),
        }
        .into(),
    );
    let tx_details = coin.withdraw(withdraw_req).wait().unwrap();
    assert_eq!(expected, tx_details.fee_details);
}

/// What an explicit-fee withdraw of one coin produced, read from the signed transaction.
#[cfg(not(target_arch = "wasm32"))]
struct ExplicitFeeWithdraw {
    /// Inputs minus outputs of the signed transaction, in base units.
    fee: u64,
    /// Serialized size of the signed transaction in bytes.
    signed_size: u64,
}

/// Withdraws 1 coin with an explicit `UtxoFixed` (`fixed == true`) or `UtxoPerKbyte` fee of `rate` coin units
/// per kilobyte from `input_count` mocked inputs of `input_value` base units each, on a coin whose own policy is
/// the plain fixed one (`FixedPerKb`) or the whole-kilobyte one (`FixedPerStartedKb`). Asserts that the
/// transaction has exactly the intended inputs and a change output, and that `fee_details.amount` equals the
/// inputs minus the outputs of the signed transaction.
#[cfg(not(target_arch = "wasm32"))]
fn explicit_fee_withdraw(
    whole_kilobyte_coin: bool,
    fixed: bool,
    rate: &str,
    input_count: u8,
    input_value: u64,
) -> ExplicitFeeWithdraw {
    UtxoStandardCoin::get_unspent_ordered_list.mock_safe(move |coin, _| {
        let cache = block_on(coin.as_ref().recently_spent_outpoints.lock());
        let unspents: Vec<UnspentInfo> = (0..input_count)
            .map(|i| UnspentInfo {
                outpoint: OutPoint {
                    hash: (i + 1).into(),
                    index: 0,
                },
                value: input_value,
                height: Default::default(),
            })
            .collect();
        MockResult::Return(Box::pin(futures::future::ok((unspents, cache))))
    });

    // The coin's own rate differs from every tested descriptor rate: the descriptor replaces the policy.
    let tx_fee = if whole_kilobyte_coin {
        TxFee::FixedPerStartedKb(50_000)
    } else {
        TxFee::FixedPerKb(50_000)
    };
    let coin = floor_test_coin(TEST_COIN_NAME, tx_fee, UTXO_DUST_AMOUNT, TEST_COIN_DECIMALS);

    let amount: BigDecimal = rate.parse().unwrap();
    let withdraw_req = WithdrawRequest {
        amount: 1u64.into(),
        from: None,
        to: "RQq6fWoy8aGGMLjvRfMY5mBNVm2RQxJyLa".to_string(),
        coin: TEST_COIN_NAME.into(),
        max: false,
        fee: Some(if fixed {
            WithdrawFee::UtxoFixed { amount }
        } else {
            WithdrawFee::UtxoPerKbyte { amount }
        }),
    };
    let tx_details = coin.withdraw(withdraw_req).wait().unwrap();

    let signed: UtxoTx = deserialize(tx_details.tx_hex.as_slice()).unwrap();
    assert_eq!(signed.inputs.len(), input_count as usize);
    // The withdrawn output and the change output.
    assert_eq!(signed.outputs.len(), 2);
    let inputs_sum = input_value * u64::from(input_count);
    let outputs_sum: u64 = signed.outputs.iter().map(|output| output.value).sum();
    let fee = inputs_sum - outputs_sum;

    let expected_details = Some(
        UtxoFeeDetails {
            coin: Some(TEST_COIN_NAME.into()),
            amount: crate::utxo::utxo_common::big_decimal_from_sat(fee as i64, TEST_COIN_DECIMALS),
        }
        .into(),
    );
    assert_eq!(expected_details, tx_details.fee_details);

    ExplicitFeeWithdraw {
        fee,
        signed_size: tx_details.tx_hex.len() as u64,
    }
}

/// The transaction shapes of T38.6.6l and T38.6.6b.1: `(inputs, value of one input, worst-case size in bytes)`.
/// The size the fee is computed for is the worst case of an unsigned transaction with a change output:
/// 29 bytes of overwintered overhead and two P2PKH outputs (68 bytes), plus 148 bytes (41 + 107) per input.
/// One input gives 245 bytes. The several-input case has 9 inputs and 1,429 bytes: the input size does
/// not allow exactly 1,404 bytes. The 9 inputs of 0.12 coin are all needed to cover the 1 coin withdrawn.
/// The 4 and 13 inputs (689 and 2,021 bytes) are shapes at which the separately rounded change output of
/// R38.6.6b differs from the single rounding at 1,234 base units per 1,000 bytes.
#[cfg(not(target_arch = "wasm32"))]
const EXPLICIT_FEE_SHAPES: [(u8, u64, u64); 4] = [
    (1, 1_000_000_000, 245),
    (9, 12_000_000, 1429),
    (4, 26_000_000, 689),
    (13, 8_000_000, 2021),
];

/// The signed transaction is the worst-case size or shorter by a few signature bytes per input.
#[cfg(not(target_arch = "wasm32"))]
fn assert_signed_size_near_worst_case(signed_size: u64, worst_case: u64, input_count: u8) {
    assert!(signed_size <= worst_case, "{signed_size} > {worst_case}");
    assert!(
        worst_case - signed_size <= 2 * u64::from(input_count),
        "{signed_size} too far below {worst_case}"
    );
}

/// T38.6.6l: reference arithmetic (R38.6.6l item 2, maintainer decision 27), asserting the fee
/// floor(a x s / 1000), once over the whole size with the change output (R38.6.6b), for `UtxoFixed` and
/// `UtxoPerKbyte` alike, on a plain-fixed and a whole-kilobyte coin, at the worst-case sizes `s` of 245, 689,
/// 1,429 and 2,021 bytes (1,429 stands in for 1,404). At a = 1001 base units the single rounding and a
/// separately rounded change output coincide: 245, 689, 1430 and 2023.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_withdraw_explicit_fee_reference_arithmetic() {
    let rate_sat: u64 = 1001;
    for (input_count, input_value, worst_case) in EXPLICIT_FEE_SHAPES {
        // floor(a x s / 1000).
        let expected = rate_sat * worst_case / 1000;
        for whole_kilobyte_coin in [false, true] {
            let fixed = explicit_fee_withdraw(whole_kilobyte_coin, true, "0.00001001", input_count, input_value);
            let per_kbyte = explicit_fee_withdraw(whole_kilobyte_coin, false, "0.00001001", input_count, input_value);
            assert_signed_size_near_worst_case(fixed.signed_size, worst_case, input_count);
            assert_signed_size_near_worst_case(per_kbyte.signed_size, worst_case, input_count);
            assert_eq!(
                fixed.fee, expected,
                "UtxoFixed, {worst_case} bytes, whole-kilobyte coin: {whole_kilobyte_coin}"
            );
            assert_eq!(
                per_kbyte.fee, expected,
                "UtxoPerKbyte, {worst_case} bytes, whole-kilobyte coin: {whole_kilobyte_coin}"
            );
        }
    }
}

/// T38.6.6l, T38.6.6b.1: reference arithmetic (R38.6.6l item 2, maintainer decision 27). The change output is
/// part of the size the fee is computed for, so the fee is floor(a x s / 1000) once, not
/// floor(a x (s - 34) / 1000) + floor(a x 34 / 1000): at a = 1234 base units it asserts 302 at 245 bytes, 850 at
/// 689, 1763 at 1,429 and 2493 at 2,021 bytes (301, 849, 1762 and 2492 if the change output's share were
/// rounded on its own).
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_withdraw_explicit_fee_change_output_is_rounded_once() {
    let rate_sat: u64 = 1234;
    let expected_fees = [302, 1763, 850, 2493];
    for ((input_count, input_value, worst_case), expected_fee) in EXPLICIT_FEE_SHAPES.into_iter().zip(expected_fees) {
        let without_change = worst_case - 34;
        let expected = rate_sat * worst_case / 1000;
        let separately_rounded = rate_sat * without_change / 1000 + rate_sat * 34 / 1000;
        let plain_fixed_mapping = ActualTxFee::FixedPerKb(rate_sat).fee_for_size(without_change)
            + ActualTxFee::FixedPerKb(rate_sat).change_output_increase(without_change);
        assert_eq!(expected, expected_fee);
        assert_eq!(expected - separately_rounded, 1);
        assert_eq!(plain_fixed_mapping, expected);
        for whole_kilobyte_coin in [false, true] {
            let fixed = explicit_fee_withdraw(whole_kilobyte_coin, true, "0.00001234", input_count, input_value);
            let per_kbyte = explicit_fee_withdraw(whole_kilobyte_coin, false, "0.00001234", input_count, input_value);
            assert_signed_size_near_worst_case(fixed.signed_size, worst_case, input_count);
            assert_eq!(fixed.fee, expected, "UtxoFixed, {worst_case} bytes");
            assert_eq!(per_kbyte.fee, expected, "UtxoPerKbyte, {worst_case} bytes");
            assert_ne!(fixed.fee, separately_rounded);
        }
    }
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_withdraw_impl_sat_per_kb_fee_amount_equal_to_max() {
    UtxoStandardCoin::get_unspent_ordered_list.mock_safe(|coin, _| {
        let cache = block_on(coin.as_ref().recently_spent_outpoints.lock());
        let unspents = vec![UnspentInfo {
            outpoint: OutPoint {
                hash: 1.into(),
                index: 0,
            },
            value: 1000000000,
            height: Default::default(),
        }];
        MockResult::Return(Box::pin(futures::future::ok((unspents, cache))))
    });

    let client = NativeClient(Arc::new(NativeClientImpl::default()));

    let coin = utxo_coin_for_test(UtxoRpcClientEnum::Native(client), None, false);

    let withdraw_req = WithdrawRequest {
        amount: "9.9789".parse().unwrap(),
        from: None,
        to: "RQq6fWoy8aGGMLjvRfMY5mBNVm2RQxJyLa".to_string(),
        coin: TEST_COIN_NAME.into(),
        max: false,
        fee: Some(WithdrawFee::UtxoPerKbyte {
            amount: "0.1".parse().unwrap(),
        }),
    };
    let tx_details = coin.withdraw(withdraw_req).wait().unwrap();
    // The resulting transaction size might be 210 or 211 bytes depending on signature size
    // MM2 always expects the worst case during fee calculation
    // 0.1 * 211 / 1000 = 0.0211
    let expected_fee = Some(
        UtxoFeeDetails {
            coin: Some(TEST_COIN_NAME.into()),
            amount: "0.0211".parse().unwrap(),
        }
        .into(),
    );
    assert_eq!(expected_fee, tx_details.fee_details);
    let expected_balance_change = BigDecimal::from(-10i32);
    assert_eq!(expected_balance_change, tx_details.my_balance_change);
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_withdraw_impl_sat_per_kb_fee_amount_equal_to_max_dust_included_to_fee() {
    UtxoStandardCoin::get_unspent_ordered_list.mock_safe(|coin, _| {
        let cache = block_on(coin.as_ref().recently_spent_outpoints.lock());
        let unspents = vec![UnspentInfo {
            outpoint: OutPoint {
                hash: 1.into(),
                index: 0,
            },
            value: 1000000000,
            height: Default::default(),
        }];
        MockResult::Return(Box::pin(futures::future::ok((unspents, cache))))
    });

    let client = NativeClient(Arc::new(NativeClientImpl::default()));

    let coin = utxo_coin_for_test(UtxoRpcClientEnum::Native(client), None, false);

    let withdraw_req = WithdrawRequest {
        amount: "9.9789".parse().unwrap(),
        from: None,
        to: "RQq6fWoy8aGGMLjvRfMY5mBNVm2RQxJyLa".to_string(),
        coin: TEST_COIN_NAME.into(),
        max: false,
        fee: Some(WithdrawFee::UtxoPerKbyte {
            amount: "0.09999999".parse().unwrap(),
        }),
    };
    let tx_details = coin.withdraw(withdraw_req).wait().unwrap();
    // The resulting transaction size might be 210 or 211 bytes depending on signature size
    // MM2 always expects the worst case during fee calculation
    // 0.1 * 211 / 1000 = 0.0211
    let expected_fee = Some(
        UtxoFeeDetails {
            coin: Some(TEST_COIN_NAME.into()),
            amount: "0.0211".parse().unwrap(),
        }
        .into(),
    );
    assert_eq!(expected_fee, tx_details.fee_details);
    let expected_balance_change = BigDecimal::from(-10i32);
    assert_eq!(expected_balance_change, tx_details.my_balance_change);
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_withdraw_impl_sat_per_kb_fee_amount_over_max() {
    UtxoStandardCoin::get_unspent_ordered_list.mock_safe(|coin, _| {
        let cache = block_on(coin.as_ref().recently_spent_outpoints.lock());
        let unspents = vec![UnspentInfo {
            outpoint: OutPoint {
                hash: 1.into(),
                index: 0,
            },
            value: 1000000000,
            height: Default::default(),
        }];
        MockResult::Return(Box::pin(futures::future::ok((unspents, cache))))
    });

    let client = NativeClient(Arc::new(NativeClientImpl::default()));

    let coin = utxo_coin_for_test(UtxoRpcClientEnum::Native(client), None, false);

    let withdraw_req = WithdrawRequest {
        amount: "9.97939455".parse().unwrap(),
        from: None,
        to: "RQq6fWoy8aGGMLjvRfMY5mBNVm2RQxJyLa".to_string(),
        coin: TEST_COIN_NAME.into(),
        max: false,
        fee: Some(WithdrawFee::UtxoPerKbyte {
            amount: "0.1".parse().unwrap(),
        }),
    };
    coin.withdraw(withdraw_req).wait().unwrap_err();
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_withdraw_impl_sat_per_kb_fee_max() {
    UtxoStandardCoin::get_unspent_ordered_list.mock_safe(|coin, _| {
        let cache = block_on(coin.as_ref().recently_spent_outpoints.lock());
        let unspents = vec![UnspentInfo {
            outpoint: OutPoint {
                hash: 1.into(),
                index: 0,
            },
            value: 1000000000,
            height: Default::default(),
        }];
        MockResult::Return(Box::pin(futures::future::ok((unspents, cache))))
    });

    let client = NativeClient(Arc::new(NativeClientImpl::default()));

    let coin = utxo_coin_for_test(UtxoRpcClientEnum::Native(client), None, false);

    let withdraw_req = WithdrawRequest {
        amount: 0u64.into(),
        from: None,
        to: "RQq6fWoy8aGGMLjvRfMY5mBNVm2RQxJyLa".to_string(),
        coin: TEST_COIN_NAME.into(),
        max: true,
        fee: Some(WithdrawFee::UtxoPerKbyte {
            amount: "0.1".parse().unwrap(),
        }),
    };
    // The resulting transaction size might be 210 or 211 bytes depending on signature size
    // MM2 always expects the worst case during fee calculation
    // 0.1 * 211 / 1000 = 0.0211
    let expected = Some(
        UtxoFeeDetails {
            coin: Some(TEST_COIN_NAME.into()),
            amount: "0.0211".parse().unwrap(),
        }
        .into(),
    );
    let tx_details = coin.withdraw(withdraw_req).wait().unwrap();
    assert_eq!(expected, tx_details.fee_details);
}

#[cfg(not(target_arch = "wasm32"))]
fn test_withdraw_kmd_rewards_impl(
    tx_hash: &'static str,
    tx_hex: &'static str,
    verbose_serialized: &str,
    current_mtp: u32,
    expected_rewards: Option<BigDecimal>,
) {
    let verbose: RpcTransaction = json::from_str(verbose_serialized).unwrap();
    let unspent_height = verbose.height;
    UtxoStandardCoin::get_unspent_ordered_list.mock_safe(move |coin, _| {
        let tx: UtxoTx = tx_hex.into();
        let unspents = vec![UnspentInfo {
            outpoint: OutPoint {
                hash: tx.hash(),
                index: 0,
            },
            value: tx.outputs[0].value,
            height: unspent_height,
        }];
        let cache = block_on(coin.as_ref().recently_spent_outpoints.lock());
        MockResult::Return(Box::pin(futures::future::ok((unspents, cache))))
    });
    UtxoStandardCoin::get_current_mtp
        .mock_safe(move |_fields| MockResult::Return(Box::pin(futures::future::ok(current_mtp))));
    NativeClient::get_verbose_transaction.mock_safe(move |_coin, txid| {
        let expected: H256Json = hex::decode(tx_hash).unwrap().as_slice().into();
        assert_eq!(*txid, expected);
        MockResult::Return(Box::new(futures01::future::ok(verbose.clone())))
    });

    let client = NativeClient(Arc::new(NativeClientImpl::default()));

    let mut fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(client), None, false);
    fields.conf.ticker = "KMD".to_owned();
    let coin = utxo_coin_from_fields(fields);

    let withdraw_req = WithdrawRequest {
        amount: BigDecimal::from_str("0.00001").unwrap(),
        from: None,
        to: "RQq6fWoy8aGGMLjvRfMY5mBNVm2RQxJyLa".to_string(),
        coin: "KMD".to_owned(),
        max: false,
        fee: None,
    };
    let expected_fee = TxFeeDetails::Utxo(UtxoFeeDetails {
        coin: Some("KMD".into()),
        amount: "0.00000245".parse().unwrap(),
    });
    let tx_details = coin.withdraw(withdraw_req).wait().unwrap();
    assert_eq!(tx_details.fee_details, Some(expected_fee));

    let expected_rewards = expected_rewards.map(|amount| KmdRewardsDetails {
        amount,
        claimed_by_me: true,
    });
    assert_eq!(tx_details.kmd_rewards, expected_rewards);
}

/// https://kmdexplorer.io/tx/535ffa3387d3fca14f4a4d373daf7edf00e463982755afce89bc8c48d8168024
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_withdraw_kmd_rewards() {
    const TX_HASH: &str = "535ffa3387d3fca14f4a4d373daf7edf00e463982755afce89bc8c48d8168024";
    const TX_HEX: &str = "0400008085202f8901afcadb73880bc1c9e7ce96b8274c2e2a4547415e649f425f98791685be009b73020000006b483045022100b8fbb77efea482b656ad16fc53c5a01d289054c2e429bf1d7bab16c3e822a83602200b87368a95c046b2ce6d0d092185138a3f234a7eb0d7f8227b196ef32358b93f012103b1e544ce2d860219bc91314b5483421a553a7b33044659eff0be9214ed58adddffffffff01dd15c293000000001976a91483762a373935ca241d557dfce89171d582b486de88ac99fe9960000000000000000000000000000000";
    const VERBOSE_SERIALIZED: &str = r#"{"hex":"0400008085202f8901afcadb73880bc1c9e7ce96b8274c2e2a4547415e649f425f98791685be009b73020000006b483045022100b8fbb77efea482b656ad16fc53c5a01d289054c2e429bf1d7bab16c3e822a83602200b87368a95c046b2ce6d0d092185138a3f234a7eb0d7f8227b196ef32358b93f012103b1e544ce2d860219bc91314b5483421a553a7b33044659eff0be9214ed58adddffffffff01dd15c293000000001976a91483762a373935ca241d557dfce89171d582b486de88ac99fe9960000000000000000000000000000000","txid":"535ffa3387d3fca14f4a4d373daf7edf00e463982755afce89bc8c48d8168024","hash":null,"size":null,"vsize":null,"version":4,"locktime":1620704921,"vin":[{"txid":"739b00be851679985f429f645e4147452a2e4c27b896cee7c9c10b8873dbcaaf","vout":2,"scriptSig":{"asm":"3045022100b8fbb77efea482b656ad16fc53c5a01d289054c2e429bf1d7bab16c3e822a83602200b87368a95c046b2ce6d0d092185138a3f234a7eb0d7f8227b196ef32358b93f[ALL] 03b1e544ce2d860219bc91314b5483421a553a7b33044659eff0be9214ed58addd","hex":"483045022100b8fbb77efea482b656ad16fc53c5a01d289054c2e429bf1d7bab16c3e822a83602200b87368a95c046b2ce6d0d092185138a3f234a7eb0d7f8227b196ef32358b93f012103b1e544ce2d860219bc91314b5483421a553a7b33044659eff0be9214ed58addd"},"sequence":4294967295,"txinwitness":null}],"vout":[{"value":24.78970333,"n":0,"scriptPubKey":{"asm":"OP_DUP OP_HASH160 83762a373935ca241d557dfce89171d582b486de OP_EQUALVERIFY OP_CHECKSIG","hex":"76a91483762a373935ca241d557dfce89171d582b486de88ac","reqSigs":1,"type":"pubkeyhash","addresses":["RMGJ9tRST45RnwEKHPGgBLuY3moSYP7Mhk"]}}],"blockhash":"0b438a8e50afddb38fb1c7be4536ffc7f7723b76bbc5edf7c28f2c17924dbdfa","confirmations":33186,"rawconfirmations":33186,"time":1620705483,"blocktime":1620705483,"height":2387532}"#;
    const CURRENT_MTP: u32 = 1622724281;

    let expected_rewards = BigDecimal::from_str("0.07895295").unwrap();
    test_withdraw_kmd_rewards_impl(TX_HASH, TX_HEX, VERBOSE_SERIALIZED, CURRENT_MTP, Some(expected_rewards));
}

/// If the ticker is `KMD` AND no rewards were accrued due to a value less than 10 or for any other reasons,
/// then `TransactionDetails::kmd_rewards` has to be `Some(0)`, not `None`.
/// https://kmdexplorer.io/tx/8c43e5a0402648faa5d0ae3550137544507ab1553425fa1b6f481a66a53f7a2d
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_withdraw_kmd_rewards_zero() {
    const TX_HASH: &str = "8c43e5a0402648faa5d0ae3550137544507ab1553425fa1b6f481a66a53f7a2d";
    const TX_HEX: &str = "0400008085202f8901c3651b6fb9ddf372e7a9d4d829c27eeea6cdfaab4f2e6e3527905c2a14f3702b010000006a47304402206819b3e51f076841ed5946bc9a48b9d75024b60abd8e854bfe50cbdfae8a268e022001a3648d2a4b33a761090676e4a8c676ee67cb602f29fef74ea5bbb8b516a178012103832b54342019dd5ecc08f1143757fbcf4ac6c8696653d456a84b40f34653c9a8ffffffff0200e1f505000000001976a91483762a373935ca241d557dfce89171d582b486de88ac60040c35000000001976a9142b33504039790fde428e4ab084aa1baf6aee209288acb0edd45f000000000000000000000000000000";
    const VERBOSE_SERIALIZED: &str = r#"{"hex":"0400008085202f8901c3651b6fb9ddf372e7a9d4d829c27eeea6cdfaab4f2e6e3527905c2a14f3702b010000006a47304402206819b3e51f076841ed5946bc9a48b9d75024b60abd8e854bfe50cbdfae8a268e022001a3648d2a4b33a761090676e4a8c676ee67cb602f29fef74ea5bbb8b516a178012103832b54342019dd5ecc08f1143757fbcf4ac6c8696653d456a84b40f34653c9a8ffffffff0200e1f505000000001976a91483762a373935ca241d557dfce89171d582b486de88ac60040c35000000001976a9142b33504039790fde428e4ab084aa1baf6aee209288acb0edd45f000000000000000000000000000000","txid":"8c43e5a0402648faa5d0ae3550137544507ab1553425fa1b6f481a66a53f7a2d","hash":null,"size":null,"vsize":null,"version":4,"locktime":1607790000,"vin":[{"txid":"2b70f3142a5c9027356e2e4fabfacda6ee7ec229d8d4a9e772f3ddb96f1b65c3","vout":1,"scriptSig":{"asm":"304402206819b3e51f076841ed5946bc9a48b9d75024b60abd8e854bfe50cbdfae8a268e022001a3648d2a4b33a761090676e4a8c676ee67cb602f29fef74ea5bbb8b516a178[ALL] 03832b54342019dd5ecc08f1143757fbcf4ac6c8696653d456a84b40f34653c9a8","hex":"47304402206819b3e51f076841ed5946bc9a48b9d75024b60abd8e854bfe50cbdfae8a268e022001a3648d2a4b33a761090676e4a8c676ee67cb602f29fef74ea5bbb8b516a178012103832b54342019dd5ecc08f1143757fbcf4ac6c8696653d456a84b40f34653c9a8"},"sequence":4294967295,"txinwitness":null}],"vout":[{"value":1.0,"n":0,"scriptPubKey":{"asm":"OP_DUP OP_HASH160 83762a373935ca241d557dfce89171d582b486de OP_EQUALVERIFY OP_CHECKSIG","hex":"76a91483762a373935ca241d557dfce89171d582b486de88ac","reqSigs":1,"type":"pubkeyhash","addresses":["RMGJ9tRST45RnwEKHPGgBLuY3moSYP7Mhk"]}},{"value":8.8998,"n":1,"scriptPubKey":{"asm":"OP_DUP OP_HASH160 2b33504039790fde428e4ab084aa1baf6aee2092 OP_EQUALVERIFY OP_CHECKSIG","hex":"76a9142b33504039790fde428e4ab084aa1baf6aee209288ac","reqSigs":1,"type":"pubkeyhash","addresses":["RDDcc63q27t6k95LrysuDwtwrxuAXqNiXe"]}}],"blockhash":"0000000054ed9fc7a4316430659e127eac5776ebc2d2382db0cb9be3eb970d7b","confirmations":243859,"rawconfirmations":243859,"time":1607790977,"blocktime":1607790977,"height":2177114}"#;
    const CURRENT_MTP: u32 = 1622724281;

    let expected_rewards = BigDecimal::from(0);
    test_withdraw_kmd_rewards_impl(TX_HASH, TX_HEX, VERBOSE_SERIALIZED, CURRENT_MTP, Some(expected_rewards));
}

/// CRD ch.38 R38.4.3 "build-surface integration" vector (issue #11): a KMD
/// spend of a post-KIP-0001 UTXO must claim the reduced reward (3,488 base
/// units, i.e. 0.00003488 KMD), not the pre-reduction figure (1,744,260 /
/// 0.0174426 KMD).
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_withdraw_kmd_rewards_post_fork_reduction() {
    const UTXO_VALUE: u64 = 1_473_232_138;
    const UTXO_HEIGHT: u64 = 5_108_257;
    const UTXO_LOCKTIME: u32 = 1_789_721_423;
    const REF_TIME: u32 = 1_790_472_552;

    let mut prev_tx = UtxoTx::default();
    prev_tx.version = 4;
    prev_tx.outputs.push(TransactionOutput {
        value: UTXO_VALUE,
        script_pubkey: vec![0xaa; 23].into(),
    });
    let prev_tx_hash = prev_tx.hash();

    UtxoStandardCoin::get_unspent_ordered_list.mock_safe(move |coin, _| {
        let unspents = vec![UnspentInfo {
            outpoint: OutPoint {
                hash: prev_tx_hash,
                index: 0,
            },
            value: UTXO_VALUE,
            height: Some(UTXO_HEIGHT),
        }];
        let cache = block_on(coin.as_ref().recently_spent_outpoints.lock());
        MockResult::Return(Box::pin(futures::future::ok((unspents, cache))))
    });
    UtxoStandardCoin::get_current_mtp
        .mock_safe(move |_fields| MockResult::Return(Box::pin(futures::future::ok(REF_TIME))));
    NativeClient::get_verbose_transaction.mock_safe(move |_coin, _txid| {
        let verbose = RpcTransaction {
            hex: Default::default(),
            txid: "0000000000000000000000000000000000000000000000000000000000000000".into(),
            hash: None,
            size: Default::default(),
            vsize: Default::default(),
            version: 4,
            locktime: UTXO_LOCKTIME,
            vin: vec![],
            vout: vec![],
            blockhash: "0000000000000000000000000000000000000000000000000000000000000000".into(),
            confirmations: 100,
            rawconfirmations: None,
            time: UTXO_LOCKTIME,
            blocktime: UTXO_LOCKTIME,
            height: Some(UTXO_HEIGHT),
        };
        MockResult::Return(Box::new(futures01::future::ok(verbose)))
    });

    let client = NativeClient(Arc::new(NativeClientImpl::default()));
    let mut fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(client), None, false);
    fields.conf.ticker = "KMD".to_owned();
    let coin = utxo_coin_from_fields(fields);

    let withdraw_req = WithdrawRequest {
        amount: BigDecimal::from_str("0.00001").unwrap(),
        from: None,
        to: "RQq6fWoy8aGGMLjvRfMY5mBNVm2RQxJyLa".to_string(),
        coin: "KMD".to_owned(),
        max: false,
        fee: None,
    };
    let tx_details = coin.withdraw(withdraw_req).wait().unwrap();

    let expected_rewards = Some(KmdRewardsDetails {
        amount: BigDecimal::from_str("0.00003488").unwrap(),
        claimed_by_me: true,
    });
    assert_eq!(tx_details.kmd_rewards, expected_rewards);
}

/// CRD ch.38 R38.4.3 "surface agreement": the transaction-history reward
/// (`calc_interest_of_tx`) MUST equal the R38.4.1 value for the same UTXO and
/// reference time used by the build-surface test above.
#[test]
fn test_calc_interest_of_tx_agrees_with_kmd_interest_post_fork() {
    use std::collections::HashMap;

    const UTXO_VALUE: u64 = 1_473_232_138;
    const UTXO_HEIGHT: u64 = 5_108_257;
    const UTXO_LOCKTIME: u32 = 1_789_721_423;
    const REF_TIME: u32 = 1_790_472_552;

    let mut prev_tx = UtxoTx::default();
    prev_tx.version = 4;
    prev_tx.lock_time = UTXO_LOCKTIME;
    prev_tx.outputs.push(TransactionOutput {
        value: UTXO_VALUE,
        script_pubkey: vec![0xaa; 23].into(),
    });
    let prev_tx_hash = prev_tx.hash();

    let mut input_transactions: HistoryUtxoTxMap = HashMap::new();
    input_transactions.insert(prev_tx_hash.reversed().into(), HistoryUtxoTx {
        tx: prev_tx,
        height: Some(UTXO_HEIGHT),
    });

    let mut spending_tx = UtxoTx::default();
    spending_tx.lock_time = REF_TIME;
    spending_tx.inputs.push(TransactionInput {
        previous_output: OutPoint {
            hash: prev_tx_hash,
            index: 0,
        },
        script_sig: vec![].into(),
        sequence: 0,
        script_witness: vec![],
    });

    let mut fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(native_client_for_test()), None, false);
    fields.conf.ticker = "KMD".to_owned();
    let coin = utxo_coin_from_fields(fields);

    let history_reward = block_on(calc_interest_of_tx(&coin, &spending_tx, &mut input_transactions)).unwrap();
    let direct_reward = kmd_interest(Some(UTXO_HEIGHT), UTXO_VALUE, UTXO_LOCKTIME as u64, REF_TIME as u64).unwrap();
    assert_eq!(history_reward, 3_488);
    assert_eq!(history_reward, direct_reward);
}

/// A transaction whose only input spends output 5 of a transaction that has a single output, and
/// the history map that already holds that previous transaction (CRD ch.38 R38.5A.9, T38.5A.7).
fn spend_of_a_missing_output() -> (UtxoTx, HistoryUtxoTxMap) {
    let mut prev_tx = UtxoTx::default();
    prev_tx.outputs.push(TransactionOutput {
        value: 1_000,
        script_pubkey: vec![0xaa; 23].into(),
    });
    let prev_tx_hash = prev_tx.hash();

    let mut input_transactions = HistoryUtxoTxMap::default();
    input_transactions.insert(prev_tx_hash.reversed().into(), HistoryUtxoTx {
        tx: prev_tx,
        height: Some(100),
    });

    let mut spending_tx = UtxoTx::default();
    spending_tx.inputs.push(TransactionInput {
        previous_output: OutPoint {
            hash: prev_tx_hash,
            index: 5,
        },
        script_sig: vec![].into(),
        sequence: 0,
        script_witness: vec![],
    });
    (spending_tx, input_transactions)
}

#[test]
fn tx_details_prev_output_index_out_of_range_is_an_error() {
    let (spending_tx, mut input_transactions) = spend_of_a_missing_output();
    let spending_hex = BytesJson::from(serialization::serialize(&spending_tx).take());
    NativeClient::get_verbose_transaction.mock_safe(move |_coin, _txid| {
        let verbose = RpcTransaction {
            hex: spending_hex.clone(),
            txid: "0000000000000000000000000000000000000000000000000000000000000000".into(),
            hash: None,
            size: Default::default(),
            vsize: Default::default(),
            version: 1,
            locktime: 0,
            vin: vec![],
            vout: vec![],
            blockhash: "0000000000000000000000000000000000000000000000000000000000000000".into(),
            confirmations: 1,
            rawconfirmations: None,
            time: 1,
            blocktime: 1,
            height: Some(101),
        };
        MockResult::Return(Box::new(futures01::future::ok(verbose)))
    });
    let client = NativeClient(Arc::new(NativeClientImpl::default()));
    let coin = utxo_coin_from_fields(utxo_coin_fields_for_test(
        UtxoRpcClientEnum::Native(client),
        None,
        false,
    ));

    let hash = spending_tx.hash().reversed().to_vec();
    let err = block_on(coin.tx_details_by_hash(&hash, &mut input_transactions)).unwrap_err();
    assert!(
        err.contains("output 5 of transaction") && err.contains("it has 1 outputs"),
        "unexpected error: {}",
        err
    );
}

#[test]
fn kmd_rewards_prev_output_index_out_of_range_is_an_error() {
    let (spending_tx, mut input_transactions) = spend_of_a_missing_output();

    let mut fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(native_client_for_test()), None, false);
    fields.conf.ticker = "KMD".to_owned();
    let coin = utxo_coin_from_fields(fields);

    let err = block_on(calc_interest_of_tx(&coin, &spending_tx, &mut input_transactions)).unwrap_err();
    match err.into_inner() {
        UtxoRpcError::InvalidResponse(msg) => {
            assert!(msg.contains("output 5 of transaction"), "unexpected error: {}", msg)
        },
        other => panic!("expected InvalidResponse, got {:?}", other),
    }
}

/// CRD ch.38 R38.4.3 "surface agreement": `kmd_rewards_info`'s
/// `accrued_rewards` MUST equal the same R38.4.1 value for the same UTXO and
/// reference time.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_kmd_rewards_info_agrees_with_kmd_interest_post_fork() {
    const UTXO_VALUE: u64 = 1_473_232_138;
    const UTXO_HEIGHT: u64 = 5_108_257;
    const UTXO_LOCKTIME: u32 = 1_789_721_423;
    const REF_TIME: u32 = 1_790_472_552;

    let mut prev_tx = UtxoTx::default();
    prev_tx.version = 4;
    prev_tx.outputs.push(TransactionOutput {
        value: UTXO_VALUE,
        script_pubkey: vec![0xaa; 23].into(),
    });
    let prev_tx_hash = prev_tx.hash();

    NativeClient::list_unspent.mock_safe(move |_coin, _address, _decimals| {
        let unspents = vec![UnspentInfo {
            outpoint: OutPoint {
                hash: prev_tx_hash,
                index: 0,
            },
            value: UTXO_VALUE,
            height: Some(UTXO_HEIGHT),
        }];
        MockResult::Return(Box::new(futures01::future::ok(unspents)))
    });
    NativeClient::get_verbose_transaction.mock_safe(move |_coin, _txid| {
        let verbose = RpcTransaction {
            hex: Default::default(),
            txid: "0000000000000000000000000000000000000000000000000000000000000000".into(),
            hash: None,
            size: Default::default(),
            vsize: Default::default(),
            version: 4,
            locktime: UTXO_LOCKTIME,
            vin: vec![],
            vout: vec![],
            blockhash: "0000000000000000000000000000000000000000000000000000000000000000".into(),
            confirmations: 100,
            rawconfirmations: None,
            time: UTXO_LOCKTIME,
            blocktime: UTXO_LOCKTIME,
            height: Some(UTXO_HEIGHT),
        };
        MockResult::Return(Box::new(futures01::future::ok(verbose)))
    });
    UtxoStandardCoin::get_current_mtp
        .mock_safe(move |_fields| MockResult::Return(Box::pin(futures::future::ok(REF_TIME))));

    let client = NativeClient(Arc::new(NativeClientImpl::default()));
    let mut fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(client), None, false);
    fields.conf.ticker = "KMD".to_owned();
    let coin = utxo_coin_from_fields(fields);

    let info = block_on(kmd_rewards_info(&coin)).unwrap();
    assert_eq!(info.len(), 1);
    match &info[0].accrued_rewards {
        KmdRewardsAccrueInfo::Accrued(amount) => {
            assert_eq!(*amount, BigDecimal::from_str("0.00003488").unwrap());
        },
        KmdRewardsAccrueInfo::NotAccruedReason(_) => panic!("expected Accrued"),
    }
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_withdraw_doc_rewards_none() {
    // https://rick.explorer.dexstats.info/tx/7181400be323acc6b5f3164240e6c4601ff4c252f40ce7649f87e81634330209
    const TX_HEX: &str = "0400008085202f8901df8119c507aa61d32332cd246dbfeb3818a4f96e76492454c1fbba5aa097977e000000004847304402205a7e229ea6929c97fd6dde254c19e4eb890a90353249721701ae7a1c477d99c402206a8b7c5bf42b5095585731d6b4c589ce557f63c20aed69ff242eca22ecfcdc7a01feffffff02d04d1bffbc050000232102afdbba3e3c90db5f0f4064118f79cf308f926c68afd64ea7afc930975663e4c4ac402dd913000000001976a9143e17014eca06281ee600adffa34b4afb0922a22288ac2bdab86035a00e000000000000000000000000";

    UtxoStandardCoin::get_unspent_ordered_list.mock_safe(move |coin, _| {
        let tx: UtxoTx = TX_HEX.into();
        let unspents = vec![UnspentInfo {
            outpoint: OutPoint {
                hash: tx.hash(),
                index: 0,
            },
            value: tx.outputs[0].value,
            height: Some(1431628),
        }];
        let cache = block_on(coin.as_ref().recently_spent_outpoints.lock());
        MockResult::Return(Box::pin(futures::future::ok((unspents, cache))))
    });

    let client = NativeClient(Arc::new(NativeClientImpl::default()));

    let coin = utxo_coin_for_test(UtxoRpcClientEnum::Native(client), None, false);

    let withdraw_req = WithdrawRequest {
        amount: BigDecimal::from_str("0.00001").unwrap(),
        from: None,
        to: "RQq6fWoy8aGGMLjvRfMY5mBNVm2RQxJyLa".to_string(),
        coin: "DOC".to_owned(),
        max: false,
        fee: None,
    };
    let expected_fee = TxFeeDetails::Utxo(UtxoFeeDetails {
        coin: Some(TEST_COIN_NAME.into()),
        amount: "0.00000245".parse().unwrap(),
    });
    let tx_details = coin.withdraw(withdraw_req).wait().unwrap();
    assert_eq!(tx_details.fee_details, Some(expected_fee));
    assert_eq!(tx_details.kmd_rewards, None);
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_utxo_lock() {
    // send several transactions concurrently to check that they are not using same inputs
    let client = electrum_client_for_test(DOC_ELECTRUM_ADDRS);
    let coin = utxo_coin_for_test(client.into(), None, false);
    let output = TransactionOutput {
        value: 1000000,
        script_pubkey: Builder::build_p2pkh(&coin.as_ref().derivation_method.unwrap_iguana().hash).to_bytes(),
    };
    let mut futures = vec![];
    for _ in 0..5 {
        futures.push(send_outputs_from_my_address_impl(coin.clone(), vec![output.clone()]));
    }
    let results = block_on(join_all(futures));
    for result in results {
        result.unwrap();
    }
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_spv_proof() {
    // RICK, the chain this test used before, is retired. DOC is its successor
    // test chain in the same family (Komodo asset chain, Equihash headers,
    // overwintered v4 transactions), so the proof exercises the same path.
    let client = electrum_client_for_test(DOC_ELECTRUM_ADDRS);
    let coin = utxo_coin_for_test(
        client.into(),
        Some("spice describe gravity federal blast come thank unfair canal monkey style afraid"),
        false,
    );

    // DOC height 1357312, position 5 in its block (a three-level Merkle branch):
    // txid 193831c822b3e879e872f766236bc1a20fc2bbb1ce9b6dab9e571069147afc9d
    let tx_str = "0400008085202f8902c55b1d7ebd78ed1f533a9e12f86981abb2a0022ca06cc36cfa4884bdd4cb1db3000000006a47304402207e3fbd998bac475f4bc7ebc762afbf7a73920115467b1cef88bdcd90a2846ace02205daa4bc6efdf95e124f5d387ea5058744d0d5dd4be1e188c0eda72d9f269f415012102031d4256c4bc9f99ac88bf3dba21773132281f65f9bf23a59928bce08961e2f3ffffffffc55b1d7ebd78ed1f533a9e12f86981abb2a0022ca06cc36cfa4884bdd4cb1db3010000006a47304402207cde81386bee3f1e16c82f56073889b5afe4d00dd5a7d074beac3932fb365769022060d3acd30b61cbabf0dd1d0dd17da8952933bc8b2beb8525717238720ace7109012102031d4256c4bc9f99ac88bf3dba21773132281f65f9bf23a59928bce08961e2f3ffffffff0240420f00000000001976a91405aab5342166f8594baf17a7d9bef5d56744332788ac11825300000000001976a91405aab5342166f8594baf17a7d9bef5d56744332788acd177ab69000000000000000000000000000000";
    let tx: UtxoTx = tx_str.into();

    let res = block_on(utxo_common::validate_spv_proof(coin.clone(), tx, now_ms() / 1000 + 30));
    res.unwrap()
}

#[test]
fn list_since_block_btc_serde() {
    // https://github.com/KomodoPlatform/atomicDEX-API/issues/563
    let input = r#"{"lastblock":"000000000000000000066f896cca2a6c667ca85fff28ed6731d64e3c39ecb119","removed":[],"transactions":[{"abandoned":false,"address":"1Q3kQ1jsB2VyH83PJT1NXJqEaEcR6Yuknn","amount":-0.01788867,"bip125-replaceable":"no","blockhash":"0000000000000000000db4be4c2df08790e1027326832cc90889554bbebc69b7","blockindex":437,"blocktime":1572174214,"category":"send","confirmations":197,"fee":-0.00012924,"involvesWatchonly":true,"time":1572173721,"timereceived":1572173721,"txid":"29606e6780c69a39767b56dc758e6af31ced5232491ad62dcf25275684cb7701","vout":0,"walletconflicts":[]},{"address":"1JsAjr6d21j9T8EMsYnQ6GXf1mM523JAv1","amount":0.1995,"bip125-replaceable":"no","blockhash":"0000000000000000000e75b33bbb27e6af2fc3898108c93c03c293fd72a86c6f","blockindex":157,"blocktime":1572179171,"category":"receive","confirmations":190,"label":"","time":1572178251,"timereceived":1572178251,"txid":"da651c6addc8da7c4b2bec21d43022852a93a9f2882a827704b318eb2966b82e","vout":19,"walletconflicts":[]},{"abandoned":false,"address":"14RXkMTyH4NyK48DbhTQyMBoMb2UkbBEPr","amount":-0.0208,"bip125-replaceable":"no","blockhash":"0000000000000000000611bfe0b3f7612239264459f4f6e7169f8d1a67e1b08f","blockindex":286,"blocktime":1572189657,"category":"send","confirmations":178,"fee":-0.0002,"involvesWatchonly":true,"time":1572189100,"timereceived":1572189100,"txid":"8d10920ce70aeb6c7e61c8d47f3cd903fb69946edd08d8907472a90761965943","vout":0,"walletconflicts":[]},{"abandoned":false,"address":"361JVximBAqkLZERT7XB1rykgLePEHAP7B","amount":-0.01801791,"bip125-replaceable":"no","blockhash":"00000000000000000011e9293c1f07f9711e677389ac101b93116d239ac38c33","blockindex":274,"blocktime":1572173649,"category":"send","confirmations":198,"fee":-0.0000965,"involvesWatchonly":true,"label":"361JVximBAqkLZERT7XB1rykgLePEHAP7B","time":1572173458,"timereceived":1572173458,"txid":"7983cae1afeb7fe58e020878aaedea0fee15be9319bc49c81f3b9ad466782950","vout":0,"walletconflicts":[]},{"abandoned":false,"address":"1JsAjr6d21j9T8EMsYnQ6GXf1mM523JAv1","amount":-0.0003447,"bip125-replaceable":"no","blockhash":"00000000000000000011e9293c1f07f9711e677389ac101b93116d239ac38c33","blockindex":274,"blocktime":1572173649,"category":"send","confirmations":198,"fee":-0.0000965,"label":"","time":1572173458,"timereceived":1572173458,"txid":"7983cae1afeb7fe58e020878aaedea0fee15be9319bc49c81f3b9ad466782950","vout":1,"walletconflicts":[]},{"address":"361JVximBAqkLZERT7XB1rykgLePEHAP7B","amount":0.01801791,"bip125-replaceable":"no","blockhash":"00000000000000000011e9293c1f07f9711e677389ac101b93116d239ac38c33","blockindex":274,"blocktime":1572173649,"category":"receive","confirmations":198,"involvesWatchonly":true,"label":"361JVximBAqkLZERT7XB1rykgLePEHAP7B","time":1572173458,"timereceived":1572173458,"txid":"7983cae1afeb7fe58e020878aaedea0fee15be9319bc49c81f3b9ad466782950","vout":0,"walletconflicts":[]},{"address":"1JsAjr6d21j9T8EMsYnQ6GXf1mM523JAv1","amount":0.0003447,"bip125-replaceable":"no","blockhash":"00000000000000000011e9293c1f07f9711e677389ac101b93116d239ac38c33","blockindex":274,"blocktime":1572173649,"category":"receive","confirmations":198,"label":"","time":1572173458,"timereceived":1572173458,"txid":"7983cae1afeb7fe58e020878aaedea0fee15be9319bc49c81f3b9ad466782950","vout":1,"walletconflicts":[]},{"abandoned":false,"address":"3B3q1GTLQQ7Fspo6ATy3cd3tg5yu97hkve","amount":-0.021,"bip125-replaceable":"no","blockhash":"0000000000000000000debf11962f89e2ae08f8ff75803b0da6170af6c5c346b","blockindex":2618,"blocktime":1572188894,"category":"send","confirmations":179,"fee":-0.00016026,"involvesWatchonly":true,"label":"3B3q1GTLQQ7Fspo6ATy3cd3tg5yu97hkve","time":1572186009,"timereceived":1572186009,"txid":"54b159ac3a656bbaaf3bf0263b8deafad03b376ec0c2e9c715d0cf1caaf3495e","vout":0,"walletconflicts":[]},{"abandoned":false,"address":"1JsAjr6d21j9T8EMsYnQ6GXf1mM523JAv1","amount":-0.17868444,"bip125-replaceable":"no","blockhash":"0000000000000000000debf11962f89e2ae08f8ff75803b0da6170af6c5c346b","blockindex":2618,"blocktime":1572188894,"category":"send","confirmations":179,"fee":-0.00016026,"label":"","time":1572186009,"timereceived":1572186009,"txid":"54b159ac3a656bbaaf3bf0263b8deafad03b376ec0c2e9c715d0cf1caaf3495e","vout":1,"walletconflicts":[]},{"address":"3B3q1GTLQQ7Fspo6ATy3cd3tg5yu97hkve","amount":0.021,"bip125-replaceable":"no","blockhash":"0000000000000000000debf11962f89e2ae08f8ff75803b0da6170af6c5c346b","blockindex":2618,"blocktime":1572188894,"category":"receive","confirmations":179,"involvesWatchonly":true,"label":"3B3q1GTLQQ7Fspo6ATy3cd3tg5yu97hkve","time":1572186009,"timereceived":1572186009,"txid":"54b159ac3a656bbaaf3bf0263b8deafad03b376ec0c2e9c715d0cf1caaf3495e","vout":0,"walletconflicts":[]},{"address":"1JsAjr6d21j9T8EMsYnQ6GXf1mM523JAv1","amount":0.17868444,"bip125-replaceable":"no","blockhash":"0000000000000000000debf11962f89e2ae08f8ff75803b0da6170af6c5c346b","blockindex":2618,"blocktime":1572188894,"category":"receive","confirmations":179,"label":"","time":1572186009,"timereceived":1572186009,"txid":"54b159ac3a656bbaaf3bf0263b8deafad03b376ec0c2e9c715d0cf1caaf3495e","vout":1,"walletconflicts":[]},{"abandoned":false,"address":"3AC6k1Y54knEdkgWjX3TjmWGjDHtJCNZZY","amount":-0.17822795,"bip125-replaceable":"no","blockhash":"00000000000000000009a60478f29f4910e29224ea5ed63d77321ac8c624ec45","blockindex":2377,"blocktime":1572190637,"category":"send","confirmations":177,"fee":-0.00009985,"involvesWatchonly":true,"label":"3AC6k1Y54knEdkgWjX3TjmWGjDHtJCNZZY","time":1572189626,"timereceived":1572189626,"txid":"eabc01e45db89ea8cf623f8e22847e4023c69bed3c7d396d573b89dec3fe17a7","vout":0,"walletconflicts":[]},{"abandoned":false,"address":"1JsAjr6d21j9T8EMsYnQ6GXf1mM523JAv1","amount":-0.00035664,"bip125-replaceable":"no","blockhash":"00000000000000000009a60478f29f4910e29224ea5ed63d77321ac8c624ec45","blockindex":2377,"blocktime":1572190637,"category":"send","confirmations":177,"fee":-0.00009985,"label":"","time":1572189626,"timereceived":1572189626,"txid":"eabc01e45db89ea8cf623f8e22847e4023c69bed3c7d396d573b89dec3fe17a7","vout":1,"walletconflicts":[]},{"address":"3AC6k1Y54knEdkgWjX3TjmWGjDHtJCNZZY","amount":0.17822795,"bip125-replaceable":"no","blockhash":"00000000000000000009a60478f29f4910e29224ea5ed63d77321ac8c624ec45","blockindex":2377,"blocktime":1572190637,"category":"receive","confirmations":177,"involvesWatchonly":true,"label":"3AC6k1Y54knEdkgWjX3TjmWGjDHtJCNZZY","time":1572189626,"timereceived":1572189626,"txid":"eabc01e45db89ea8cf623f8e22847e4023c69bed3c7d396d573b89dec3fe17a7","vout":0,"walletconflicts":[]},{"address":"1JsAjr6d21j9T8EMsYnQ6GXf1mM523JAv1","amount":0.00035664,"bip125-replaceable":"no","blockhash":"00000000000000000009a60478f29f4910e29224ea5ed63d77321ac8c624ec45","blockindex":2377,"blocktime":1572190637,"category":"receive","confirmations":177,"label":"","time":1572189626,"timereceived":1572189626,"txid":"eabc01e45db89ea8cf623f8e22847e4023c69bed3c7d396d573b89dec3fe17a7","vout":1,"walletconflicts":[]},{"abandoned":false,"address":"1Q3kQ1jsB2VyH83PJT1NXJqEaEcR6Yuknn","amount":-0.17809412,"bip125-replaceable":"no","blockhash":"000000000000000000125e17a9540ac901d70e92e987d59a1cf87ca36ebca830","blockindex":1680,"blocktime":1572191122,"category":"send","confirmations":176,"fee":-0.00013383,"involvesWatchonly":true,"time":1572190821,"timereceived":1572190821,"txid":"d3579f7be169ea8fd1358d0eda85bad31ce8080a6020dcd224eac8a663dc9bf7","vout":0,"walletconflicts":[]},{"abandoned":false,"address":"326VCyLKV1w4SxeYs81jQU1SC11njcL1eG","amount":-0.039676,"bip125-replaceable":"no","blockhash":"0000000000000000000d61630db06ed5d3054a39bf71a706efeaa9e86866b9d4","blockindex":2193,"blocktime":1572053656,"category":"send","confirmations":380,"fee":-0.00005653,"involvesWatchonly":true,"label":"326VCyLKV1w4SxeYs81jQU1SC11njcL1eG","time":1572052431,"timereceived":1572052431,"txid":"37b57fb36312e21ec7d069a55ab9bffc6abc7fe3731ed38502c5329025a9edf9","vout":0,"walletconflicts":[]},{"abandoned":false,"address":"1JsAjr6d21j9T8EMsYnQ6GXf1mM523JAv1","amount":-0.01845911,"bip125-replaceable":"no","blockhash":"0000000000000000000d61630db06ed5d3054a39bf71a706efeaa9e86866b9d4","blockindex":2193,"blocktime":1572053656,"category":"send","confirmations":380,"fee":-0.00005653,"label":"","time":1572052431,"timereceived":1572052431,"txid":"37b57fb36312e21ec7d069a55ab9bffc6abc7fe3731ed38502c5329025a9edf9","vout":1,"walletconflicts":[]},{"address":"326VCyLKV1w4SxeYs81jQU1SC11njcL1eG","amount":0.039676,"bip125-replaceable":"no","blockhash":"0000000000000000000d61630db06ed5d3054a39bf71a706efeaa9e86866b9d4","blockindex":2193,"blocktime":1572053656,"category":"receive","confirmations":380,"involvesWatchonly":true,"label":"326VCyLKV1w4SxeYs81jQU1SC11njcL1eG","time":1572052431,"timereceived":1572052431,"txid":"37b57fb36312e21ec7d069a55ab9bffc6abc7fe3731ed38502c5329025a9edf9","vout":0,"walletconflicts":[]},{"address":"1JsAjr6d21j9T8EMsYnQ6GXf1mM523JAv1","amount":0.01845911,"bip125-replaceable":"no","blockhash":"0000000000000000000d61630db06ed5d3054a39bf71a706efeaa9e86866b9d4","blockindex":2193,"blocktime":1572053656,"category":"receive","confirmations":380,"label":"","time":1572052431,"timereceived":1572052431,"txid":"37b57fb36312e21ec7d069a55ab9bffc6abc7fe3731ed38502c5329025a9edf9","vout":1,"walletconflicts":[]}]}"#;
    let _res: ListSinceBlockRes = json::from_str(input).unwrap();
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
// https://github.com/KomodoPlatform/atomicDEX-API/issues/587
fn get_tx_details_coinbase_transaction() {
    let client = electrum_client_for_test(&[
        "marty.electrum1.cipig.net:10021",
        "marty.electrum2.cipig.net:10021",
        "marty.electrum3.cipig.net:10021",
    ]);
    let coin = utxo_coin_for_test(
        client.into(),
        Some("spice describe gravity federal blast come thank unfair canal monkey style afraid"),
        false,
    );

    let fut = async move {
        // hash of a MARTY coinbase transaction (height 1389279). The coinbase the test used before is no longer
        // served by the MARTY servers; any coinbase transaction that they serve does as well.
        let hash = hex::decode("2338196dcc82061707803c6ec9f78d27aafd09fd1a6ad08907939897d93a949c").unwrap();

        let mut input_transactions = HistoryUtxoTxMap::new();
        let tx_details = coin.tx_details_by_hash(&hash, &mut input_transactions).await.unwrap();
        assert!(tx_details.from.is_empty());
    };

    block_on(fut);
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_electrum_rpc_client_error() {
    let client = electrum_client_for_test(&["electrum1.cipig.net:10060"]);

    let empty_hash = H256Json::default();
    let err = client.get_verbose_transaction(&empty_hash).wait().unwrap_err();

    let actual = format!("{}", err);
    let debug_actual = format!("{:?}", err);

    assert!(actual.contains("JsonRpcError { client_info: coin: DOC"));
    // The pool draws its request ids (the version negotiation included) from one counter, so the id is not 1.
    assert!(actual.contains("request: blockchain.transaction.get id="));
    assert!(actual.contains(" params=2"));
    assert!(actual.contains("response from electrum1.cipig.net:10060"));
    assert!(actual.contains("No such mempool or blockchain transaction"));
    assert!(!actual.contains("params: [String("));
    assert!(debug_actual.contains("params: [String("));
}

#[test]
fn test_network_info_deserialization() {
    let network_info_kmd = r#"{
        "connections": 1,
        "localaddresses": [],
        "localservices": "0000000070000005",
        "networks": [
            {
                "limited": false,
                "name": "ipv4",
                "proxy": "",
                "proxy_randomize_credentials": false,
                "reachable": true
            },
            {
                "limited": false,
                "name": "ipv6",
                "proxy": "",
                "proxy_randomize_credentials": false,
                "reachable": true
            },
            {
                "limited": true,
                "name": "onion",
                "proxy": "",
                "proxy_randomize_credentials": false,
                "reachable": false
            }
        ],
        "protocolversion": 170007,
        "relayfee": 1e-06,
        "subversion": "/MagicBean:2.0.15-rc2/",
        "timeoffset": 0,
        "version": 2001526,
        "warnings": ""
    }"#;
    json::from_str::<NetworkInfo>(network_info_kmd).unwrap();

    let network_info_btc = r#"{
        "version": 180000,
        "subversion": "\/Satoshi:0.18.0\/",
        "protocolversion": 70015,
        "localservices": "000000000000040d",
        "localrelay": true,
        "timeoffset": 0,
        "networkactive": true,
        "connections": 124,
        "networks": [
            {
                "name": "ipv4",
                "limited": false,
                "reachable": true,
                "proxy": "",
                "proxy_randomize_credentials": false
            },
            {
                "name": "ipv6",
                "limited": false,
                "reachable": true,
                "proxy": "",
                "proxy_randomize_credentials": false
            },
            {
                "name": "onion",
                "limited": true,
                "reachable": false,
                "proxy": "",
                "proxy_randomize_credentials": false
            }
        ],
        "relayfee": 1.0e-5,
        "incrementalfee": 1.0e-5,
        "localaddresses": [
            {
                "address": "96.57.248.252",
                "port": 8333,
                "score": 618294
            }
        ],
        "warnings": ""
    }"#;
    json::from_str::<NetworkInfo>(network_info_btc).unwrap();
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
// https://github.com/KomodoPlatform/atomicDEX-API/issues/617
fn test_generate_transaction_relay_fee_is_used_when_dynamic_fee_is_lower() {
    let client = NativeClientImpl::default();

    static mut GET_RELAY_FEE_CALLED: bool = false;
    NativeClient::get_relay_fee.mock_safe(|_| {
        unsafe { GET_RELAY_FEE_CALLED = true };
        MockResult::Return(Box::new(futures01::future::ok("1.0".parse().unwrap())))
    });
    let client = UtxoRpcClientEnum::Native(NativeClient(Arc::new(client)));
    let mut coin = utxo_coin_fields_for_test(client, None, false);
    coin.conf.force_min_relay_fee = true;
    let coin = utxo_coin_from_fields(coin);
    let unspents = vec![UnspentInfo {
        value: 1000000000,
        outpoint: OutPoint::default(),
        height: Default::default(),
    }];

    let outputs = vec![TransactionOutput {
        script_pubkey: vec![].into(),
        value: 900000000,
    }];

    let builder = UtxoTxBuilder::new(&coin)
        .add_available_inputs(unspents)
        .add_outputs(outputs)
        .with_fee(ActualTxFee::Dynamic(100));

    let generated = block_on(builder.build()).unwrap();
    // payment and change outputs
    assert_eq!(generated.0.outputs.len(), 2);

    // the fee is the proportional relay fee (relay rate 1.0 per kB, 220 bytes with the change output)
    // because the calculated dynamic fee is lower (R38.6.6b)
    assert_eq!(generated.1.fee_amount, 22000000);
    assert_eq!(generated.1.unused_change, None);
    assert_eq!(generated.1.received_by_me, 78000000);
    assert_eq!(generated.1.spent_by_me, 1000000000);
    assert!(unsafe { GET_RELAY_FEE_CALLED });
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
// https://github.com/KomodoPlatform/atomicDEX-API/issues/1037
fn test_generate_transaction_relay_fee_is_used_when_dynamic_fee_is_lower_and_deduct_from_output() {
    let client = NativeClientImpl::default();

    static mut GET_RELAY_FEE_CALLED: bool = false;
    NativeClient::get_relay_fee.mock_safe(|_| {
        unsafe { GET_RELAY_FEE_CALLED = true };
        MockResult::Return(Box::new(futures01::future::ok("1.0".parse().unwrap())))
    });
    let client = UtxoRpcClientEnum::Native(NativeClient(Arc::new(client)));
    let mut coin = utxo_coin_fields_for_test(client, None, false);
    coin.conf.force_min_relay_fee = true;
    let coin = utxo_coin_from_fields(coin);
    let unspents = vec![UnspentInfo {
        value: 1000000000,
        outpoint: OutPoint::default(),
        height: Default::default(),
    }];

    let outputs = vec![TransactionOutput {
        script_pubkey: vec![].into(),
        value: 1000000000,
    }];

    let tx_builder = UtxoTxBuilder::new(&coin)
        .add_available_inputs(unspents)
        .add_outputs(outputs)
        .with_fee_policy(FeePolicy::DeductFromOutput(0))
        .with_fee(ActualTxFee::Dynamic(100));

    let generated = block_on(tx_builder.build()).unwrap();
    assert_eq!(generated.0.outputs.len(), 1);
    // `output (= 10.0) - fee_amount (= 0.186)`
    assert_eq!(generated.0.outputs[0].value, 981400000);

    // the fee is the proportional relay fee (relay rate 1.0 per kB, 186 bytes) because the calculated
    // dynamic fee is lower (R38.6.6b)
    assert_eq!(generated.1.fee_amount, 18600000);
    assert_eq!(generated.1.unused_change, None);
    assert_eq!(generated.1.received_by_me, 0);
    assert_eq!(generated.1.spent_by_me, 1000000000);
    assert!(unsafe { GET_RELAY_FEE_CALLED });
}

/// CRD ch.38 R38.6.6b: the forced minimum relay fee is proportional to the transaction size, so a
/// 226-byte transaction with a relay rate of 1,000 per kB pays 226, not a whole kilobyte.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_generate_transaction_forced_relay_fee_is_proportional_r38_6_6b() {
    NativeClient::get_relay_fee
        .mock_safe(|_| MockResult::Return(Box::new(futures01::future::ok("0.00001".parse().unwrap()))));
    let client = UtxoRpcClientEnum::Native(NativeClient(Arc::new(NativeClientImpl::default())));
    let mut coin = utxo_coin_fields_for_test(client, None, false);
    coin.conf.ticker = "LTC".to_owned();
    coin.conf.force_min_relay_fee = true;
    let coin = utxo_coin_from_fields(coin);
    let unspents = vec![UnspentInfo {
        value: 100_000,
        outpoint: OutPoint::default(),
        height: Default::default(),
    }];
    // one input and a 40-byte output script make a 226-byte transaction without a change output
    let outputs = vec![TransactionOutput {
        script_pubkey: vec![0x51; 40].into(),
        value: 99_000,
    }];
    let builder = UtxoTxBuilder::new(&coin)
        .add_available_inputs(unspents)
        .add_outputs(outputs)
        .with_fee(ActualTxFee::FixedPerKb(100));
    let (tx, data) = block_on(builder.build()).unwrap();
    assert_eq!(
        utxo_common::tx_size_in_v_bytes(&UtxoAddressFormat::Standard, &UtxoTx::from(tx)),
        226
    );
    assert_eq!(data.fee_amount, 226);
    assert_eq!(data.unused_change, Some(774));
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
// https://github.com/KomodoPlatform/atomicDEX-API/issues/617
fn test_generate_tx_fee_is_correct_when_dynamic_fee_is_larger_than_relay() {
    let client = NativeClientImpl::default();

    static mut GET_RELAY_FEE_CALLED: bool = false;
    NativeClient::get_relay_fee.mock_safe(|_| {
        unsafe { GET_RELAY_FEE_CALLED = true };
        MockResult::Return(Box::new(futures01::future::ok("0.00001".parse().unwrap())))
    });
    let client = UtxoRpcClientEnum::Native(NativeClient(Arc::new(client)));
    let mut coin = utxo_coin_fields_for_test(client, None, false);
    coin.conf.force_min_relay_fee = true;
    let coin = utxo_coin_from_fields(coin);
    let unspents = vec![
        UnspentInfo {
            value: 1000000000,
            outpoint: OutPoint::default(),
            height: Default::default(),
        };
        20
    ];

    let outputs = vec![TransactionOutput {
        script_pubkey: vec![].into(),
        value: 19000000000,
    }];

    let builder = UtxoTxBuilder::new(&coin)
        .add_available_inputs(unspents)
        .add_outputs(outputs)
        .with_fee(ActualTxFee::Dynamic(1000));

    let generated = block_on(builder.build()).unwrap();

    assert_eq!(generated.0.outputs.len(), 2);
    assert_eq!(generated.0.inputs.len(), 20);

    // resulting signed transaction size would be 3032 bytes so fee is 3032 sat
    assert_eq!(generated.1.fee_amount, 3032);
    assert_eq!(generated.1.unused_change, None);
    assert_eq!(generated.1.received_by_me, 999996968);
    assert_eq!(generated.1.spent_by_me, 20000000000);
    assert!(unsafe { GET_RELAY_FEE_CALLED });
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_get_median_time_past_from_electrum_kmd() {
    let client = electrum_client_for_test(&[
        "electrum1.cipig.net:10001",
        "electrum2.cipig.net:10001",
        "electrum3.cipig.net:10001",
    ]);

    let mtp = client
        .get_median_time_past(1773390, KMD_MTP_BLOCK_COUNT, CoinVariant::Standard)
        .wait()
        .unwrap();
    // the MTP is block time of 1773385 in this case
    assert_eq!(1583159915, mtp);
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_get_median_time_past_from_electrum_btc() {
    let client = electrum_client_for_test(&[
        "electrum1.cipig.net:10000",
        "electrum2.cipig.net:10000",
        "electrum3.cipig.net:10000",
    ]);

    let mtp = client
        .get_median_time_past(632858, KMD_MTP_BLOCK_COUNT, CoinVariant::Standard)
        .wait()
        .unwrap();
    assert_eq!(1591173041, mtp);
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_get_median_time_past_from_native_has_median_in_get_block() {
    let client = native_client_for_test();
    NativeClientImpl::get_block_hash.mock_safe(|_, block_num| {
        assert_eq!(block_num, 632858);
        MockResult::Return(Box::new(futures01::future::ok(
            "00000000000000000002eb7892b8fdfd7b8e0f089e5cdf96436de680b7e695e3".into(),
        )))
    });

    NativeClientImpl::get_block.mock_safe(|_, block_hash| {
        assert_eq!(block_hash, "00000000000000000002eb7892b8fdfd7b8e0f089e5cdf96436de680b7e695e3".into());
        let block_data_str = r#"{"hash":"00000000000000000002eb7892b8fdfd7b8e0f089e5cdf96436de680b7e695e3","confirmations":1,"strippedsize":833287,"size":1493229,"weight":3993090,"height":632858,"version":549453824,"versionHex":"20c00000","merkleroot":"7e20760d227465d2a84fbb2617b2962f77364daa66f06b48d1010fa27923b940","tx":[],"time":1591174568,"mediantime":1591173041,"nonce":"1594651477","bits":"171297f6","difficulty":15138043247082.88,"chainwork":"00000000000000000000000000000000000000000fff2e35384d3c16f53adda4","nTx":1601,"previousblockhash":"00000000000000000009a54084d9f4eafa3ca07af646ff8fa9031d0ac72a92aa"}"#;
        let block_data = json::from_str(block_data_str).unwrap();
        MockResult::Return(
            Box::new(futures01::future::ok(block_data))
        )
    });

    let mtp = client
        .get_median_time_past(632858, KMD_MTP_BLOCK_COUNT, CoinVariant::Standard)
        .wait()
        .unwrap();
    assert_eq!(1591173041, mtp);
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_get_median_time_past_from_native_does_not_have_median_in_get_block() {
    use std::collections::HashMap;

    let blocks_json_str = r#"
    [
        {"hash":"00000000000000000002eb7892b8fdfd7b8e0f089e5cdf96436de680b7e695e3","confirmations":1,"strippedsize":833287,"size":1493229,"weight":3993090,"height":632858,"version":549453824,"versionHex":"20c00000","merkleroot":"7e20760d227465d2a84fbb2617b2962f77364daa66f06b48d1010fa27923b940","tx":[],"time":1591173090,"nonce":"1594651477","bits":"171297f6","difficulty":15138043247082.88,"chainwork":"00000000000000000000000000000000000000000fff2e35384d3c16f53adda4","nTx":1601,"previousblockhash":"00000000000000000009a54084d9f4eafa3ca07af646ff8fa9031d0ac72a92aa"},
        {"hash":"00000000000000000002eb7892b8fdfd7b8e0f089e5cdf96436de680b7e695e4","confirmations":1,"strippedsize":833287,"size":1493229,"weight":3993090,"height":632857,"version":549453824,"versionHex":"20c00000","merkleroot":"7e20760d227465d2a84fbb2617b2962f77364daa66f06b48d1010fa27923b940","tx":[],"time":1591173080,"nonce":"1594651477","bits":"171297f6","difficulty":15138043247082.88,"chainwork":"00000000000000000000000000000000000000000fff2e35384d3c16f53adda4","nTx":1601,"previousblockhash":"00000000000000000009a54084d9f4eafa3ca07af646ff8fa9031d0ac72a92aa"},
        {"hash":"00000000000000000002eb7892b8fdfd7b8e0f089e5cdf96436de680b7e695e5","confirmations":1,"strippedsize":833287,"size":1493229,"weight":3993090,"height":632856,"version":549453824,"versionHex":"20c00000","merkleroot":"7e20760d227465d2a84fbb2617b2962f77364daa66f06b48d1010fa27923b940","tx":[],"time":1591173070,"nonce":"1594651477","bits":"171297f6","difficulty":15138043247082.88,"chainwork":"00000000000000000000000000000000000000000fff2e35384d3c16f53adda4","nTx":1601,"previousblockhash":"00000000000000000009a54084d9f4eafa3ca07af646ff8fa9031d0ac72a92aa"},
        {"hash":"00000000000000000002eb7892b8fdfd7b8e0f089e5cdf96436de680b7e695e6","confirmations":1,"strippedsize":833287,"size":1493229,"weight":3993090,"height":632855,"version":549453824,"versionHex":"20c00000","merkleroot":"7e20760d227465d2a84fbb2617b2962f77364daa66f06b48d1010fa27923b940","tx":[],"time":1591173058,"nonce":"1594651477","bits":"171297f6","difficulty":15138043247082.88,"chainwork":"00000000000000000000000000000000000000000fff2e35384d3c16f53adda4","nTx":1601,"previousblockhash":"00000000000000000009a54084d9f4eafa3ca07af646ff8fa9031d0ac72a92aa"},
        {"hash":"00000000000000000002eb7892b8fdfd7b8e0f089e5cdf96436de680b7e695e7","confirmations":1,"strippedsize":833287,"size":1493229,"weight":3993090,"height":632854,"version":549453824,"versionHex":"20c00000","merkleroot":"7e20760d227465d2a84fbb2617b2962f77364daa66f06b48d1010fa27923b940","tx":[],"time":1591173050,"nonce":"1594651477","bits":"171297f6","difficulty":15138043247082.88,"chainwork":"00000000000000000000000000000000000000000fff2e35384d3c16f53adda4","nTx":1601,"previousblockhash":"00000000000000000009a54084d9f4eafa3ca07af646ff8fa9031d0ac72a92aa"},
        {"hash":"00000000000000000002eb7892b8fdfd7b8e0f089e5cdf96436de680b7e695e8","confirmations":1,"strippedsize":833287,"size":1493229,"weight":3993090,"height":632853,"version":549453824,"versionHex":"20c00000","merkleroot":"7e20760d227465d2a84fbb2617b2962f77364daa66f06b48d1010fa27923b940","tx":[],"time":1591173041,"nonce":"1594651477","bits":"171297f6","difficulty":15138043247082.88,"chainwork":"00000000000000000000000000000000000000000fff2e35384d3c16f53adda4","nTx":1601,"previousblockhash":"00000000000000000009a54084d9f4eafa3ca07af646ff8fa9031d0ac72a92aa"},
        {"hash":"00000000000000000002eb7892b8fdfd7b8e0f089e5cdf96436de680b7e695e9","confirmations":1,"strippedsize":833287,"size":1493229,"weight":3993090,"height":632852,"version":549453824,"versionHex":"20c00000","merkleroot":"7e20760d227465d2a84fbb2617b2962f77364daa66f06b48d1010fa27923b940","tx":[],"time":1591173040,"nonce":"1594651477","bits":"171297f6","difficulty":15138043247082.88,"chainwork":"00000000000000000000000000000000000000000fff2e35384d3c16f53adda4","nTx":1601,"previousblockhash":"00000000000000000009a54084d9f4eafa3ca07af646ff8fa9031d0ac72a92aa"},
        {"hash":"00000000000000000002eb7892b8fdfd7b8e0f089e5cdf96436de680b7e695f0","confirmations":1,"strippedsize":833287,"size":1493229,"weight":3993090,"height":632851,"version":549453824,"versionHex":"20c00000","merkleroot":"7e20760d227465d2a84fbb2617b2962f77364daa66f06b48d1010fa27923b940","tx":[],"time":1591173039,"nonce":"1594651477","bits":"171297f6","difficulty":15138043247082.88,"chainwork":"00000000000000000000000000000000000000000fff2e35384d3c16f53adda4","nTx":1601,"previousblockhash":"00000000000000000009a54084d9f4eafa3ca07af646ff8fa9031d0ac72a92aa"},
        {"hash":"00000000000000000002eb7892b8fdfd7b8e0f089e5cdf96436de680b7e695f1","confirmations":1,"strippedsize":833287,"size":1493229,"weight":3993090,"height":632850,"version":549453824,"versionHex":"20c00000","merkleroot":"7e20760d227465d2a84fbb2617b2962f77364daa66f06b48d1010fa27923b940","tx":[],"time":1591173038,"nonce":"1594651477","bits":"171297f6","difficulty":15138043247082.88,"chainwork":"00000000000000000000000000000000000000000fff2e35384d3c16f53adda4","nTx":1601,"previousblockhash":"00000000000000000009a54084d9f4eafa3ca07af646ff8fa9031d0ac72a92aa"},
        {"hash":"00000000000000000002eb7892b8fdfd7b8e0f089e5cdf96436de680b7e695f2","confirmations":1,"strippedsize":833287,"size":1493229,"weight":3993090,"height":632849,"version":549453824,"versionHex":"20c00000","merkleroot":"7e20760d227465d2a84fbb2617b2962f77364daa66f06b48d1010fa27923b940","tx":[],"time":1591173037,"nonce":"1594651477","bits":"171297f6","difficulty":15138043247082.88,"chainwork":"00000000000000000000000000000000000000000fff2e35384d3c16f53adda4","nTx":1601,"previousblockhash":"00000000000000000009a54084d9f4eafa3ca07af646ff8fa9031d0ac72a92aa"},
        {"hash":"00000000000000000002eb7892b8fdfd7b8e0f089e5cdf96436de680b7e695f3","confirmations":1,"strippedsize":833287,"size":1493229,"weight":3993090,"height":632848,"version":549453824,"versionHex":"20c00000","merkleroot":"7e20760d227465d2a84fbb2617b2962f77364daa66f06b48d1010fa27923b940","tx":[],"time":1591173030,"nonce":1594651477,"bits":"171297f6","difficulty":15138043247082.88,"chainwork":"00000000000000000000000000000000000000000fff2e35384d3c16f53adda4","nTx":1601,"previousblockhash":"00000000000000000009a54084d9f4eafa3ca07af646ff8fa9031d0ac72a92aa"}
    ]
    "#;

    let blocks: Vec<VerboseBlock> = json::from_str(blocks_json_str).unwrap();
    let mut block_hashes: HashMap<_, _> = blocks
        .iter()
        .map(|block| (block.height.unwrap() as u64, block.hash.clone()))
        .collect();
    let mut blocks: HashMap<_, _> = blocks.into_iter().map(|block| (block.hash.clone(), block)).collect();
    let client = native_client_for_test();

    NativeClientImpl::get_block_hash.mock_safe(move |_, block_num| {
        let hash = block_hashes.remove(&block_num).unwrap();
        MockResult::Return(Box::new(futures01::future::ok(hash)))
    });

    NativeClientImpl::get_block.mock_safe(move |_, block_hash| {
        let block = blocks.remove(&block_hash).unwrap();
        MockResult::Return(Box::new(futures01::future::ok(block)))
    });

    let mtp = client
        .get_median_time_past(632858, KMD_MTP_BLOCK_COUNT, CoinVariant::Standard)
        .wait()
        .unwrap();
    assert_eq!(1591173041, mtp);
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_cashaddresses_in_tx_details_by_hash() {
    let conf = json!({
        "coin": "BCH",
        "pubtype": 0,
        "p2shtype": 5,
        "mm2": 1,
        "address_format":{"format":"cashaddress","network":"bchtest"},
    });
    let req = json!({
         "method": "electrum",
         "servers": [
             {"url":"bitcoin-cash.devmole.eu:5001"},
             {"url":"tbch.loping.net:60001"},
             {"url":"blackie.c3-soft.com:60001"},
             {"url":"bch0.kister.net:51001"},
             {"url":"testnet.imaginary.cash:50001"}
         ],
    });

    let ctx = MmCtxBuilder::new().into_mm_arc();
    let params = UtxoActivationParams::from_legacy_req(&req).unwrap();

    let coin = block_on(utxo_standard_coin_with_priv_key(
        &ctx, "BCH", &conf, &params, &[1u8; 32],
    ))
    .unwrap();

    let hash = hex::decode("0f2f6e0c8f440c641895023782783426c3aca1acc78d7c0db7751995e8aa5751").unwrap();
    let fut = async {
        let mut input_transactions = HistoryUtxoTxMap::new();
        let tx_details = coin.tx_details_by_hash(&hash, &mut input_transactions).await.unwrap();
        log!([tx_details]);

        assert!(tx_details
            .from
            .iter()
            .any(|addr| addr == "bchtest:qze8g4gx3z428jjcxzpycpxl7ke7d947gca2a7n2la"));
        assert!(tx_details
            .to
            .iter()
            .any(|addr| addr == "bchtest:qr39na5d25wdeecgw3euh9fkd4ygvd4pnsury96597"));
    };

    block_on(fut);
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_address_from_str_with_cashaddress_activated() {
    let conf = json!({
        "coin": "BCH",
        "pubtype": 0,
        "p2shtype": 5,
        "mm2": 1,
        "address_format":{"format":"cashaddress","network":"bitcoincash"},
    });
    let req = json!({
         "method": "electrum",
         "servers": [
             {"url":"bitcoin-cash.devmole.eu:5001"},
             {"url":"tbch.loping.net:60001"},
             {"url":"blackie.c3-soft.com:60001"},
             {"url":"bch0.kister.net:51001"},
             {"url":"testnet.imaginary.cash:50001"}
         ],
    });

    let ctx = MmCtxBuilder::new().into_mm_arc();
    let params = UtxoActivationParams::from_legacy_req(&req).unwrap();

    let coin = block_on(utxo_standard_coin_with_priv_key(
        &ctx, "BCH", &conf, &params, &[1u8; 32],
    ))
    .unwrap();

    // other error on parse
    let error = coin
        .address_from_str("bitcoincash:000000000000000000000000000000000000000000")
        .err()
        .unwrap();
    assert!(error.contains("Invalid address: bitcoincash:000000000000000000000000000000000000000000"));
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_address_from_str_with_legacy_address_activated() {
    let conf = json!({
        "coin": "BCH",
        "pubtype": 0,
        "p2shtype": 5,
        "mm2": 1,
    });
    let req = json!({
         "method": "electrum",
         "servers": [
             {"url":"bitcoin-cash.devmole.eu:5001"},
             {"url":"tbch.loping.net:60001"},
             {"url":"blackie.c3-soft.com:60001"},
             {"url":"bch0.kister.net:51001"},
             {"url":"testnet.imaginary.cash:50001"}
         ],
    });

    let ctx = MmCtxBuilder::new().into_mm_arc();
    let params = UtxoActivationParams::from_legacy_req(&req).unwrap();

    let coin = block_on(utxo_standard_coin_with_priv_key(
        &ctx, "BCH", &conf, &params, &[1u8; 32],
    ))
    .unwrap();

    let error = coin
        .address_from_str("bitcoincash:qzxqqt9lh4feptf0mplnk58gnajfepzwcq9f2rxk55")
        .err()
        .unwrap();
    assert!(error.contains("Legacy address format activated for BCH, but CashAddress format used instead"));

    // other error on parse
    let error = coin
        .address_from_str("0000000000000000000000000000000000")
        .err()
        .unwrap();
    assert!(error.contains("Invalid address: 0000000000000000000000000000000000"));
}

#[test]
// https://github.com/KomodoPlatform/atomicDEX-API/issues/673
fn test_network_info_negative_time_offset() {
    let info_str = r#"{"version":1140200,"subversion":"/Shibetoshi:1.14.2/","protocolversion":70015,"localservices":"0000000000000005","localrelay":true,"timeoffset":-1,"networkactive":true,"connections":12,"networks":[{"name":"ipv4","limited":false,"reachable":true,"proxy":"","proxy_randomize_credentials":false},{"name":"ipv6","limited":false,"reachable":true,"proxy":"","proxy_randomize_credentials":false},{"name":"onion","limited":false,"reachable":true,"proxy":"127.0.0.1:9050","proxy_randomize_credentials":true}],"relayfee":1.00000000,"incrementalfee":0.00001000,"localaddresses":[],"warnings":""}"#;
    let _info: NetworkInfo = json::from_str(&info_str).unwrap();
}

/// A server that answers a protocol version outside the client's range is removed from the pool; with no other
/// server the activation probe fails (CRD ch.38 R38.6.4e item 1, R38.6.4k-R item 5), naming the server.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_unavailable_electrum_proto_version() {
    use crate::utxo::rpc_clients::electrum_stub_server::{StubBehavior, StubServer};

    ElectrumClientImpl::new.mock_safe(|coin_ticker, event_handlers| {
        MockResult::Return(ElectrumClientImpl::with_protocol_version(
            coin_ticker,
            event_handlers,
            OrdRange::new(1.8, 1.9).unwrap(),
        ))
    });

    let server = block_on(StubServer::start(StubBehavior::Answer));
    let conf = json!({"coin":"DOC","asset":"DOC","rpcport":62415});
    let req = json!({
         "method": "electrum",
         "servers": [{"url": server.addr()}],
    });

    let ctx = MmCtxBuilder::new().into_mm_arc();
    let params = UtxoActivationParams::from_legacy_req(&req).unwrap();
    let started = std::time::Instant::now();
    let error = block_on(utxo_standard_coin_with_priv_key(
        &ctx, "DOC", &conf, &params, &[1u8; 32],
    ))
    .err()
    .unwrap();
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    let error = error.to_string();
    log!("Error: "(error));
    assert!(error.contains("DOC"), "{error}");
    assert!(error.contains(&server.addr()), "{error}");
    assert!(error.contains("unsupported protocol version 1.4"), "{error}");
}

#[test]
#[ignore]
// The test provided to dimxy to recreate "stuck mempool" problem of komodod on DOC chain.
// Leaving this test here for a while because it might be still useful
fn test_spam_doc() {
    let conf = json!({"coin":"DOC","asset":"DOC","fname":"DOC (TESTCOIN)","rpcport":25435,"txversion":4,"overwintered":1,"mm2":1,"required_confirmations":1,"avg_blocktime":1,"protocol":{"type":"UTXO"}});
    let req = json!({
         "method": "enable",
         "coin": "DOC",
    });

    let key_pair = key_pair_from_seed("my_seed").unwrap();
    let ctx = MmCtxBuilder::new().into_mm_arc();
    let params = UtxoActivationParams::from_legacy_req(&req).unwrap();
    let coin = block_on(utxo_standard_coin_with_priv_key(
        &ctx,
        "DOC",
        &conf,
        &params,
        &*key_pair.private().secret,
    ))
    .unwrap();

    let output = TransactionOutput {
        value: 1000000,
        script_pubkey: Builder::build_p2pkh(&coin.as_ref().derivation_method.unwrap_iguana().hash).to_bytes(),
    };
    let mut futures = vec![];
    for _ in 0..5 {
        futures.push(send_outputs_from_my_address_impl(coin.clone(), vec![output.clone()]));
    }
    let results = block_on(join_all(futures));
    for result in results {
        result.unwrap();
    }
}

#[test]
#[ignore = "deprecated: this test only exercised electrum protocol v1.2 negotiation via \
            electrum-mona.bitbank.cc:50001, which is offline. Protocol v1.2 is dead; KDF \
            requires v1.4, and no live server advertises v1.2-only, so this scenario is no \
            longer reproducible."]
fn test_one_unavailable_electrum_proto_version() {
    // check if the electrum-mona.bitbank.cc:50001 doesn't support the protocol version 1.4
    let client = electrum_client_for_test(&["electrum-mona.bitbank.cc:50001"]);
    let result = client
        .server_version(
            "electrum-mona.bitbank.cc:50001",
            "AtomicDEX",
            &OrdRange::new(1.4, 1.4).unwrap(),
        )
        .wait();
    assert!(result
        .err()
        .unwrap()
        .to_string()
        .contains("unsupported protocol version"));

    drop(client);
    log!("Run BTC coin to test the server.version loop");

    let conf = json!({"coin":"BTC","asset":"BTC","rpcport":8332});
    let req = json!({
         "method": "electrum",
         // electrum-mona.bitbank.cc:50001 supports only 1.2 protocol version
         "servers": [{"url":"electrum1.cipig.net:10000"},{"url":"electrum-mona.bitbank.cc:50001"}],
    });

    let ctx = MmCtxBuilder::new().into_mm_arc();
    let params = UtxoActivationParams::from_legacy_req(&req).unwrap();

    let coin = block_on(utxo_standard_coin_with_priv_key(
        &ctx, "BTC", &conf, &params, &[1u8; 32],
    ))
    .unwrap();

    block_on(async { Timer::sleep(0.5).await });

    assert!(coin.as_ref().rpc_client.get_block_count().wait().is_ok());
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_qtum_generate_pod() {
    let priv_key = [
        3, 98, 177, 3, 108, 39, 234, 144, 131, 178, 103, 103, 127, 80, 230, 166, 53, 68, 147, 215, 42, 216, 144, 72,
        172, 110, 180, 13, 123, 179, 10, 49,
    ];
    let conf = json!({"coin":"tQTUM","rpcport":13889,"pubtype":120,"p2shtype":110});
    let req = json!({
        "method": "electrum",
        "servers": tqtum_test_servers(),
    });

    let ctx = MmCtxBuilder::new().into_mm_arc();

    let params = UtxoActivationParams::from_legacy_req(&req).unwrap();
    let coin = block_on(qtum_coin_with_priv_key(&ctx, "tQTUM", &conf, &params, &priv_key)).unwrap();
    let expected_res = "20086d757b34c01deacfef97a391f8ed2ca761c72a08d5000adc3d187b1007aca86a03bc5131b1f99b66873a12b51f8603213cdc1aa74c05ca5d48fe164b82152b";
    let address = Address::from_str("qcyBHeSct7Wr4mAw18iuQ1zW5mMFYmtmBE").unwrap();
    let res = coin.generate_pod(address.hash).unwrap();
    assert_eq!(expected_res, res.to_string());
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_qtum_add_delegation() {
    let keypair = key_pair_from_seed("asthma turtle lizard tone genuine tube hunt valley soap cloth urge alpha amazing frost faculty cycle mammal leaf normal bright topple avoid pulse buffalo").unwrap();
    let conf = json!({"coin":"tQTUM","rpcport":13889,"pubtype":120,"p2shtype":110, "mature_confirmations":1});
    let req = json!({
        "method": "electrum",
        "servers": tqtum_test_servers(),
    });

    let ctx = MmCtxBuilder::new().into_mm_arc();
    let params = UtxoActivationParams::from_legacy_req(&req).unwrap();
    let coin = block_on(qtum_coin_with_priv_key(
        &ctx,
        "tQTUM",
        &conf,
        &params,
        keypair.private().secret.as_slice(),
    ))
    .unwrap();
    let address = Address::from_str("qcyBHeSct7Wr4mAw18iuQ1zW5mMFYmtmBE").unwrap();
    let request = QtumDelegationRequest {
        address: address.to_string(),
        fee: Some(10),
    };
    let res = coin.add_delegation(request).wait().unwrap();
    // Eligible for delegation
    assert_eq!(res.my_balance_change.is_negative(), true);
    assert_eq!(res.total_amount, res.spent_by_me);
    assert!(res.spent_by_me > res.received_by_me);

    let request = QtumDelegationRequest {
        address: "fake_address".to_string(),
        fee: Some(10),
    };
    let res = coin.add_delegation(request).wait();
    // Wrong address
    assert_eq!(res.is_err(), true);
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_qtum_add_delegation_on_already_delegating() {
    let keypair = key_pair_from_seed("federal stay trigger hour exist success game vapor become comfort action phone bright ill target wild nasty crumble dune close rare fabric hen iron").unwrap();
    let conf = json!({"coin":"tQTUM","rpcport":13889,"pubtype":120,"p2shtype":110, "mature_confirmations":1});
    let req = json!({
        "method": "electrum",
        "servers": tqtum_test_servers(),
    });

    let ctx = MmCtxBuilder::new().into_mm_arc();
    let params = UtxoActivationParams::from_legacy_req(&req).unwrap();
    let coin = block_on(qtum_coin_with_priv_key(
        &ctx,
        "tQTUM",
        &conf,
        &params,
        keypair.private().secret.as_slice(),
    ))
    .unwrap();
    let address = Address::from_str("qcyBHeSct7Wr4mAw18iuQ1zW5mMFYmtmBE").unwrap();
    let request = QtumDelegationRequest {
        address: address.to_string(),
        fee: Some(10),
    };
    let res = coin.add_delegation(request).wait();
    // Already Delegating
    assert_eq!(res.is_err(), true);
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_qtum_get_delegation_infos() {
    let keypair =
        key_pair_from_seed("federal stay trigger hour exist success game vapor become comfort action phone bright ill target wild nasty crumble dune close rare fabric hen iron").unwrap();
    let conf = json!({"coin":"tQTUM","rpcport":13889,"pubtype":120,"p2shtype":110, "mature_confirmations":1});
    let req = json!({
        "method": "electrum",
        "servers": tqtum_test_servers(),
    });

    let ctx = MmCtxBuilder::new().into_mm_arc();

    let params = UtxoActivationParams::from_legacy_req(&req).unwrap();
    let coin = block_on(qtum_coin_with_priv_key(
        &ctx,
        "tQTUM",
        &conf,
        &params,
        keypair.private().secret.as_slice(),
    ))
    .unwrap();
    let staking_infos = coin.get_delegation_infos().wait().unwrap();
    match staking_infos.staking_infos_details {
        StakingInfosDetails::Qtum(staking_details) => {
            assert_eq!(staking_details.am_i_staking, true);
            assert_eq!(staking_details.staker.unwrap(), "qcyBHeSct7Wr4mAw18iuQ1zW5mMFYmtmBE");
            // Will return false for segwit.
            assert_eq!(staking_details.is_staking_supported, true);
        },
    };
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_qtum_remove_delegation() {
    let keypair = key_pair_from_seed("federal stay trigger hour exist success game vapor become comfort action phone bright ill target wild nasty crumble dune close rare fabric hen iron").unwrap();
    let conf = json!({"coin":"tQTUM","rpcport":13889,"pubtype":120,"p2shtype":110, "mature_confirmations":1});
    let req = json!({
        "method": "electrum",
        "servers": tqtum_test_servers(),
    });

    let ctx = MmCtxBuilder::new().into_mm_arc();
    let params = UtxoActivationParams::from_legacy_req(&req).unwrap();
    let coin = block_on(qtum_coin_with_priv_key(
        &ctx,
        "tQTUM",
        &conf,
        &params,
        keypair.private().secret.as_slice(),
    ))
    .unwrap();
    let res = coin.remove_delegation().wait();
    assert_eq!(res.is_err(), false);
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_qtum_my_balance() {
    QtumCoin::get_mature_unspent_ordered_list.mock_safe(move |coin, _address| {
        let cache = block_on(coin.as_ref().recently_spent_outpoints.lock());
        // spendable balance (66.0)
        let mature = vec![
            UnspentInfo {
                outpoint: OutPoint {
                    hash: 1.into(),
                    index: 0,
                },
                value: 5000000000,
                height: Default::default(),
            },
            UnspentInfo {
                outpoint: OutPoint {
                    hash: 1.into(),
                    index: 0,
                },
                value: 1600000000,
                height: Default::default(),
            },
        ];
        // unspendable (2.0)
        let immature = vec![UnspentInfo {
            outpoint: OutPoint {
                hash: 1.into(),
                index: 0,
            },
            value: 200000000,
            height: Default::default(),
        }];
        MockResult::Return(Box::pin(futures::future::ok((
            MatureUnspentList { mature, immature },
            cache,
        ))))
    });

    let conf = json!({"coin":"tQTUM","rpcport":13889,"pubtype":120,"p2shtype":110});
    let req = json!({
        "method": "electrum",
        "servers": tqtum_test_servers(),
    });

    let ctx = MmCtxBuilder::new().into_mm_arc();

    let priv_key = [
        184, 199, 116, 240, 113, 222, 8, 199, 253, 143, 98, 185, 127, 26, 87, 38, 246, 206, 159, 27, 207, 20, 27, 112,
        184, 102, 137, 37, 78, 214, 113, 78,
    ];

    let params = UtxoActivationParams::from_legacy_req(&req).unwrap();
    let coin = block_on(qtum_coin_with_priv_key(&ctx, "tQTUM", &conf, &params, &priv_key)).unwrap();

    let CoinBalance { spendable, unspendable } = coin.my_balance().wait().unwrap();
    let expected_spendable = BigDecimal::from(66);
    let expected_unspendable = BigDecimal::from(2);
    assert_eq!(spendable, expected_spendable);
    assert_eq!(unspendable, expected_unspendable);
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_qtum_my_balance_with_check_utxo_maturity_false() {
    const DISPLAY_BALANCE: u64 = 68;
    // The qtum address is legacy and belongs to the activated key, so its P2PKH balance and P2PK lookup are one
    // Electrum request (R38.6.3a); this is the client method that carries them.
    ElectrumClient::display_balance_with_p2pk.mock_safe(move |_, _, _, _| {
        MockResult::Return(Box::new(futures01::future::ok((
            BigDecimal::from(DISPLAY_BALANCE),
            Vec::new(),
        ))))
    });
    QtumCoin::get_all_unspent_ordered_list.mock_safe(move |_, _| {
        panic!(
            "'QtumCoin::get_all_unspent_ordered_list' is not expected to be called when `check_utxo_maturity` is false"
        )
    });

    let conf = json!({"coin":"tQTUM","rpcport":13889,"pubtype":120,"p2shtype":110});
    let req = json!({
        "method": "electrum",
        "servers": tqtum_test_servers(),
        "check_utxo_maturity": false,
    });

    let ctx = MmCtxBuilder::new().into_mm_arc();

    let priv_key = [
        184, 199, 116, 240, 113, 222, 8, 199, 253, 143, 98, 185, 127, 26, 87, 38, 246, 206, 159, 27, 207, 20, 27, 112,
        184, 102, 137, 37, 78, 214, 113, 78,
    ];

    let params = UtxoActivationParams::from_legacy_req(&req).unwrap();
    let coin = block_on(qtum_coin_with_priv_key(&ctx, "tQTUM", &conf, &params, &priv_key)).unwrap();

    let CoinBalance { spendable, unspendable } = coin.my_balance().wait().unwrap();
    let expected_spendable = BigDecimal::from(DISPLAY_BALANCE);
    let expected_unspendable = BigDecimal::from(0);
    assert_eq!(spendable, expected_spendable);
    assert_eq!(unspendable, expected_unspendable);
}

fn test_get_mature_unspent_ordered_map_from_cache_impl(
    unspent_height: Option<u64>,
    cached_height: Option<u64>,
    cached_confs: u32,
    block_count: u64,
    expected_height: Option<u64>,
    expected_confs: u32,
) {
    // A DOC coinbase transaction (height 1405347, one output). The transaction the test used before is no
    // longer served by the DOC servers. The unspent list, the block count, the cached verbose transaction and
    // the maturity check are mocked below, so the live server is asked only for `get_verbose_transaction`
    // of this hash, and any transaction the servers serve as verbose does.
    const TX_HASH: &str = "ee6afde5fb0e4898f3ad9991d3ffb98561201b32ef4fd2453364fedd66bf479e";
    let tx_hash: H256Json = hex::decode(TX_HASH).unwrap().as_slice().into();
    let client = electrum_client_for_test(DOC_ELECTRUM_ADDRS);
    let mut verbose = client.get_verbose_transaction(&tx_hash).wait().unwrap();
    verbose.confirmations = cached_confs;
    verbose.height = cached_height;

    // prepare mocks
    // The address is the coin's own legacy address, so `get_all_unspent_ordered_list` reads the unspents through
    // `ElectrumClient::list_unspent_with_p2pk` and never calls `UtxoRpcClientOps::list_unspent`.
    ElectrumClient::list_unspent_with_p2pk.mock_safe(move |_, _, _| {
        let unspents = vec![UnspentInfo {
            outpoint: OutPoint {
                hash: H256::from_reversed_str(TX_HASH),
                index: 0,
            },
            value: 1000000000,
            height: unspent_height,
        }];
        MockResult::Return(Box::new(futures01::future::ok(unspents)))
    });
    ElectrumClient::get_block_count
        .mock_safe(move |_| MockResult::Return(Box::new(futures01::future::ok(block_count))));
    UtxoStandardCoin::get_verbose_transactions_from_cache_or_rpc.mock_safe(move |_, tx_ids| {
        itertools::assert_equal(tx_ids, iter::once(tx_hash));
        let result: HashMap<_, _> = iter::once((tx_hash, VerboseTransactionFrom::Cache(verbose.clone()))).collect();
        MockResult::Return(Box::new(futures01::future::ok(result)))
    });
    static mut IS_UNSPENT_MATURE_CALLED: bool = false;
    UtxoStandardCoin::is_unspent_mature.mock_safe(move |_, tx: &RpcTransaction| {
        // check if the transaction height and confirmations are expected
        assert_eq!(tx.height, expected_height);
        assert_eq!(tx.confirmations, expected_confs);
        unsafe { IS_UNSPENT_MATURE_CALLED = true }
        MockResult::Return(false)
    });

    // run test
    let coin = utxo_coin_for_test(UtxoRpcClientEnum::Electrum(client), None, false);
    let (unspents, _) =
        block_on(coin.get_mature_unspent_ordered_list(&Address::from("R9o9xTocqr6CeEDGDH6mEYpwLoMz6jNjMW")))
            .expect("Expected an empty unspent list");
    // unspents should be empty because `is_unspent_mature()` always returns false
    assert!(unsafe { IS_UNSPENT_MATURE_CALLED == true });
    assert!(unspents.mature.is_empty());
    assert_eq!(unspents.immature.len(), 1);
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_get_mature_unspents_ordered_map_from_cache() {
    let unspent_height = None;
    let cached_height = None;
    let cached_confs = 0;
    let block_count = 1000;
    let expected_height = None; // is unknown
    let expected_confs = 0; // is not changed because height is unknown
    test_get_mature_unspent_ordered_map_from_cache_impl(
        unspent_height,
        cached_height,
        cached_confs,
        block_count,
        expected_height,
        expected_confs,
    );

    let unspent_height = None;
    let cached_height = None;
    let cached_confs = 5;
    let block_count = 1000;
    let expected_height = None; // is unknown
    let expected_confs = 5; // is not changed because height is unknown
    test_get_mature_unspent_ordered_map_from_cache_impl(
        unspent_height,
        cached_height,
        cached_confs,
        block_count,
        expected_height,
        expected_confs,
    );

    let unspent_height = Some(998);
    let cached_height = None;
    let cached_confs = 0;
    let block_count = 1000;
    let expected_height = Some(998); // as the unspent_height
    let expected_confs = 3; // 1000 - 998 + 1
    test_get_mature_unspent_ordered_map_from_cache_impl(
        unspent_height,
        cached_height,
        cached_confs,
        block_count,
        expected_height,
        expected_confs,
    );

    let unspent_height = None;
    let cached_height = Some(998);
    let cached_confs = 0;
    let block_count = 1000;
    let expected_height = Some(998); // as the cached_height
    let expected_confs = 3; // 1000 - 998 + 1
    test_get_mature_unspent_ordered_map_from_cache_impl(
        unspent_height,
        cached_height,
        cached_confs,
        block_count,
        expected_height,
        expected_confs,
    );

    let unspent_height = Some(998);
    let cached_height = Some(997);
    let cached_confs = 0;
    let block_count = 1000;
    let expected_height = Some(998); // as the unspent_height
    let expected_confs = 3; // 1000 - 998 + 1
    test_get_mature_unspent_ordered_map_from_cache_impl(
        unspent_height,
        cached_height,
        cached_confs,
        block_count,
        expected_height,
        expected_confs,
    );

    // block_count < tx_height
    let unspent_height = None;
    let cached_height = Some(1000);
    let cached_confs = 1;
    let block_count = 999;
    let expected_height = Some(1000); // as the cached_height
    let expected_confs = 1; // is not changed because height cannot be calculated
    test_get_mature_unspent_ordered_map_from_cache_impl(
        unspent_height,
        cached_height,
        cached_confs,
        block_count,
        expected_height,
        expected_confs,
    );

    // block_count == tx_height
    let unspent_height = None;
    let cached_height = Some(1000);
    let cached_confs = 1;
    let block_count = 1000;
    let expected_height = Some(1000); // as the cached_height
    let expected_confs = 1; // 1000 - 1000 + 1
    test_get_mature_unspent_ordered_map_from_cache_impl(
        unspent_height,
        cached_height,
        cached_confs,
        block_count,
        expected_height,
        expected_confs,
    );

    // tx_height == 0
    let unspent_height = Some(0);
    let cached_height = None;
    let cached_confs = 1;
    let block_count = 1000;
    let expected_height = Some(0); // as the cached_height
    let expected_confs = 1; // is not changed because tx_height is expected to be not zero
    test_get_mature_unspent_ordered_map_from_cache_impl(
        unspent_height,
        cached_height,
        cached_confs,
        block_count,
        expected_height,
        expected_confs,
    );
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_native_client_unspents_filtered_using_tx_cache_single_tx_in_cache() {
    let client = native_client_for_test();
    let coin = utxo_coin_for_test(UtxoRpcClientEnum::Native(client), None, false);

    let address: Address = "RGfFZaaNV68uVe1uMf6Y37Y8E1i2SyYZBN".into();
    block_on(coin.as_ref().recently_spent_outpoints.lock()).for_script_pubkey =
        Builder::build_p2pkh(&address.hash).to_bytes();

    // https://morty.explorer.dexstats.info/tx/31c7aaae89ab1c39febae164a3190a86ed7c6c6f8c9dc98ec28d508b7929d347
    let tx: UtxoTx = "0400008085202f89027f57730fcbbc2c72fb18bcc3766a713044831a117bb1cade3ed88644864f7333020000006a47304402206e3737b2fcf078b61b16fa67340cc3e79c5d5e2dc9ffda09608371552a3887450220460a332aa1b8ad8f2de92d319666f70751078b221199951f80265b4f7cef8543012102d8c948c6af848c588517288168faa397d6ba3ea924596d03d1d84f224b5123c2ffffffff42b916a80430b80a77e114445b08cf120735447a524de10742fac8f6a9d4170f000000006a473044022004aa053edafb9d161ea8146e0c21ed1593aa6b9404dd44294bcdf920a1695fd902202365eac15dbcc5e9f83e2eed56a8f2f0e5aded36206f9c3fabc668fd4665fa2d012102d8c948c6af848c588517288168faa397d6ba3ea924596d03d1d84f224b5123c2ffffffff03547b16000000000017a9143e8ad0e2bf573d32cb0b3d3a304d9ebcd0c2023b870000000000000000166a144e2b3c0323ab3c2dc6f86dc5ec0729f11e42f56103970400000000001976a91450f4f098306f988d8843004689fae28c83ef16e888ac89c5925f000000000000000000000000000000".into();
    let spent_by_tx = vec![
        UnspentInfo {
            outpoint: tx.inputs[0].previous_output.clone(),
            value: 886737,
            height: Some(642293),
        },
        UnspentInfo {
            outpoint: tx.inputs[1].previous_output.clone(),
            value: 88843,
            height: Some(642293),
        },
    ];

    block_on(coin.as_ref().recently_spent_outpoints.lock()).add_spent(
        spent_by_tx.clone(),
        tx.hash(),
        tx.outputs.clone(),
    );
    NativeClient::list_unspent
        .mock_safe(move |_, _, _| MockResult::Return(Box::new(futures01::future::ok(spent_by_tx.clone()))));

    let (unspents_ordered, _) = block_on(coin.get_unspent_ordered_list(&address)).unwrap();
    // output 2 is change so it must be returned
    let expected_unspent = UnspentInfo {
        outpoint: OutPoint {
            hash: tx.hash(),
            index: 2,
        },
        value: tx.outputs[2].value,
        height: None,
    };
    assert_eq!(vec![expected_unspent], unspents_ordered);
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_native_client_unspents_filtered_using_tx_cache_single_several_chained_txs_in_cache() {
    let client = native_client_for_test();
    let coin = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(client), None, false);

    let address: Address = "RGfFZaaNV68uVe1uMf6Y37Y8E1i2SyYZBN".into();
    block_on(coin.recently_spent_outpoints.lock()).for_script_pubkey = Builder::build_p2pkh(&address.hash).to_bytes();
    let coin = utxo_coin_from_fields(coin);

    // https://morty.explorer.dexstats.info/tx/31c7aaae89ab1c39febae164a3190a86ed7c6c6f8c9dc98ec28d508b7929d347
    let tx_0: UtxoTx = "0400008085202f89027f57730fcbbc2c72fb18bcc3766a713044831a117bb1cade3ed88644864f7333020000006a47304402206e3737b2fcf078b61b16fa67340cc3e79c5d5e2dc9ffda09608371552a3887450220460a332aa1b8ad8f2de92d319666f70751078b221199951f80265b4f7cef8543012102d8c948c6af848c588517288168faa397d6ba3ea924596d03d1d84f224b5123c2ffffffff42b916a80430b80a77e114445b08cf120735447a524de10742fac8f6a9d4170f000000006a473044022004aa053edafb9d161ea8146e0c21ed1593aa6b9404dd44294bcdf920a1695fd902202365eac15dbcc5e9f83e2eed56a8f2f0e5aded36206f9c3fabc668fd4665fa2d012102d8c948c6af848c588517288168faa397d6ba3ea924596d03d1d84f224b5123c2ffffffff03547b16000000000017a9143e8ad0e2bf573d32cb0b3d3a304d9ebcd0c2023b870000000000000000166a144e2b3c0323ab3c2dc6f86dc5ec0729f11e42f56103970400000000001976a91450f4f098306f988d8843004689fae28c83ef16e888ac89c5925f000000000000000000000000000000".into();
    let spent_by_tx_0 = vec![
        UnspentInfo {
            outpoint: tx_0.inputs[0].previous_output.clone(),
            value: 886737,
            height: Some(642293),
        },
        UnspentInfo {
            outpoint: tx_0.inputs[1].previous_output.clone(),
            value: 88843,
            height: Some(642293),
        },
    ];
    block_on(coin.as_ref().recently_spent_outpoints.lock()).add_spent(
        spent_by_tx_0.clone(),
        tx_0.hash(),
        tx_0.outputs.clone(),
    );

    // https://morty.explorer.dexstats.info/tx/dbfc821e482747a3512ee6d5734f9df2aa73dab07e2fcd86abeadb462e795bf9
    let tx_1: UtxoTx = "0400008085202f890347d329798b508dc28ec99d8c6f6c7ced860a19a364e1bafe391cab89aeaac731020000006a47304402203ea8b380d0a7e64348869ef7c4c2bfa966fc7b148633003332fa8d0ab0c1bc5602202cc63fabdd2a6578c52d8f4f549069b16505f2ead48edc2b8de299be15aadf9a012102d8c948c6af848c588517288168faa397d6ba3ea924596d03d1d84f224b5123c2ffffffff1d1fd3a6b01710647a7f4a08c6de6075cb8e78d5069fa50f10c4a2a10ded2a95000000006a47304402203868945edc0f6dc2ee43d70a69ee4ec46ca188dc493173ce58924ba9bf6ee7a50220648ff99ce458ca72800758f6a1bd3800cd05ff9c3122f23f3653c25e09d22c79012102d8c948c6af848c588517288168faa397d6ba3ea924596d03d1d84f224b5123c2ffffffff7932150df8b4a1852b8b84b89b0d5322bf74665fb7f76a728369fd6895d3fd48000000006a4730440220127918c6f79c11f7f2376a6f3b750ed4c7103183181ad1218afcb2625ece9599022028c05e88d3a2f97cebd84a718cda33b62b48b18f16278fa8e531fd2155e61ee8012102d8c948c6af848c588517288168faa397d6ba3ea924596d03d1d84f224b5123c2ffffffff0329fd12000000000017a914cafb62e3e8bdb8db3735c39b92743ac6ebc9ef20870000000000000000166a14a7416b070c9bb98f4bafae55616f005a2a30bd6014b40c00000000001976a91450f4f098306f988d8843004689fae28c83ef16e888ac8cc5925f000000000000000000000000000000".into();
    let spent_by_tx_1 = vec![
        UnspentInfo {
            outpoint: tx_1.inputs[0].previous_output.clone(),
            value: 300803,
            height: Some(642293),
        },
        UnspentInfo {
            outpoint: tx_1.inputs[1].previous_output.clone(),
            value: 888544,
            height: Some(642293),
        },
        UnspentInfo {
            outpoint: tx_1.inputs[2].previous_output.clone(),
            value: 888642,
            height: Some(642293),
        },
    ];
    block_on(coin.as_ref().recently_spent_outpoints.lock()).add_spent(
        spent_by_tx_1.clone(),
        tx_1.hash(),
        tx_1.outputs.clone(),
    );
    // https://morty.explorer.dexstats.info/tx/12ea22a7cde9efb66b76f9b84345ddfc4c34870e293bfa8eac68d7df83dffa4b
    let tx_2: UtxoTx = "0400008085202f8902f95b792e46dbeaab86cd2f7eb0da73aaf29d4f73d5e62e51a34727481e82fcdb020000006a4730440220347adefe33ed5afbbb8e5d453afd527319f9a50ab790023296a981da095ca4a2022029a68ef6fd5a4decf3793d4c33994eb8658408f3b14a6d439c4753b2dde954ee012102d8c948c6af848c588517288168faa397d6ba3ea924596d03d1d84f224b5123c2ffffffff75bd4348594f8ff2a216e5ad7533b37d47d2a2767b0b88d43972ad51895355e2000000006a473044022069b36c0f65d56e02bc179f7442806374c4163d07939090aba1da736abad9a77d022006dc39adf48e02033ae9d4a48540752ae3b3841e3ec60d2e86dececb88b9e518012102d8c948c6af848c588517288168faa397d6ba3ea924596d03d1d84f224b5123c2ffffffff03414111000000000017a914a153024c826a3a42c2e501eca5d7dacd3fc59976870000000000000000166a14db0e6f4d418d68dce8e5beb26cc5078e01e2e3ace2fe0800000000001976a91450f4f098306f988d8843004689fae28c83ef16e888ac8fc5925f000000000000000000000000000000".into();
    let spent_by_tx_2 = vec![
        UnspentInfo {
            outpoint: tx_2.inputs[0].previous_output.clone(),
            value: 832532,
            height: Some(642293),
        },
        UnspentInfo {
            outpoint: tx_2.inputs[1].previous_output.clone(),
            value: 888823,
            height: Some(642293),
        },
    ];
    block_on(coin.as_ref().recently_spent_outpoints.lock()).add_spent(
        spent_by_tx_2.clone(),
        tx_2.hash(),
        tx_2.outputs.clone(),
    );

    let mut unspents_to_return = spent_by_tx_0;
    unspents_to_return.extend(spent_by_tx_1);
    unspents_to_return.extend(spent_by_tx_2);

    NativeClient::list_unspent
        .mock_safe(move |_, _, _| MockResult::Return(Box::new(futures01::future::ok(unspents_to_return.clone()))));

    let (unspents_ordered, _) = block_on(coin.get_unspent_ordered_list(&address)).unwrap();

    // output 2 is change so it must be returned
    let expected_unspent = UnspentInfo {
        outpoint: OutPoint {
            hash: tx_2.hash(),
            index: 2,
        },
        value: tx_2.outputs[2].value,
        height: None,
    };
    assert_eq!(vec![expected_unspent], unspents_ordered);
}

#[test]
fn validate_address_res_format() {
    let btc_017_and_above_response = json!({
      "isvalid": true,
      "address": "1JsAjr6d21j9T8EMsYnQ6GXf1mM523JAv1",
      "scriptPubKey": "76a914c3f710deb7320b0efa6edb14e3ebeeb9155fa90d88ac",
      "isscript": false,
      "iswitness": false
    });

    let _: ValidateAddressRes = json::from_value(btc_017_and_above_response).unwrap();

    let btc_016_response = json!({
      "isvalid": true,
      "address": "RT9MpMyucqXiX8bZLimXBnrrn2ofmdGNKd",
      "scriptPubKey": "76a914c3f710deb7320b0efa6edb14e3ebeeb9155fa90d88ac",
      "ismine": false,
      "iswatchonly": true,
      "isscript": false,
      "account": "RT9MpMyucqXiX8bZLimXBnrrn2ofmdGNKd",
      "timestamp": 0
    });

    let _: ValidateAddressRes = json::from_value(btc_016_response).unwrap();
}

#[test]
fn get_address_info_format() {
    let response = json!({
      "address": "Ld6814QT6fyChvvX3gmhNHbRDyiMBvPr9s",
      "scriptPubKey": "76a914c3f710deb7320b0efa6edb14e3ebeeb9155fa90d88ac",
      "ismine": false,
      "solvable": false,
      "iswatchonly": true,
      "isscript": false,
      "iswitness": false,
      "label": "Ld6814QT6fyChvvX3gmhNHbRDyiMBvPr9s",
      "ischange": false,
      "timestamp": 0,
      "labels": [
        {
          "name": "Ld6814QT6fyChvvX3gmhNHbRDyiMBvPr9s",
          "purpose": "receive"
        }
      ]
    });

    let _: GetAddressInfoRes = json::from_value(response).unwrap();
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_native_is_address_imported_validate_address_is_mine() {
    let client = native_client_for_test();
    NativeClientImpl::validate_address.mock_safe(|_, _| {
        let result = ValidateAddressRes {
            is_valid: false,
            address: "".to_string(),
            script_pub_key: Default::default(),
            seg_id: None,
            is_mine: Some(true),
            is_watch_only: Some(false),
            is_script: false,
            account: None,
        };
        MockResult::Return(Box::new(futures01::future::ok(result)))
    });

    let address = "";
    let imported = block_on(client.is_address_imported(address)).unwrap();
    assert!(imported);
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_native_is_address_imported_validate_address_is_watch_only() {
    let client = native_client_for_test();
    NativeClientImpl::validate_address.mock_safe(|_, _| {
        let result = ValidateAddressRes {
            is_valid: false,
            address: "".to_string(),
            script_pub_key: Default::default(),
            seg_id: None,
            is_mine: Some(false),
            is_watch_only: Some(true),
            is_script: false,
            account: None,
        };
        MockResult::Return(Box::new(futures01::future::ok(result)))
    });

    let address = "";
    let imported = block_on(client.is_address_imported(address)).unwrap();
    assert!(imported);
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_native_is_address_imported_validate_address_false() {
    let client = native_client_for_test();
    NativeClientImpl::validate_address.mock_safe(|_, _| {
        let result = ValidateAddressRes {
            is_valid: false,
            address: "".to_string(),
            script_pub_key: Default::default(),
            seg_id: None,
            is_mine: Some(false),
            is_watch_only: Some(false),
            is_script: false,
            account: None,
        };
        MockResult::Return(Box::new(futures01::future::ok(result)))
    });

    let address = "";
    let imported = block_on(client.is_address_imported(address)).unwrap();
    assert!(!imported);
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_native_is_address_imported_fallback_to_address_info_is_mine() {
    let client = native_client_for_test();
    NativeClientImpl::validate_address.mock_safe(|_, _| {
        let result = ValidateAddressRes {
            is_valid: false,
            address: "".to_string(),
            script_pub_key: Default::default(),
            seg_id: None,
            is_mine: None,
            is_watch_only: None,
            is_script: false,
            account: None,
        };
        MockResult::Return(Box::new(futures01::future::ok(result)))
    });

    NativeClientImpl::get_address_info.mock_safe(|_, _| {
        let result = GetAddressInfoRes {
            is_mine: true,
            is_watch_only: false,
        };
        MockResult::Return(Box::new(futures01::future::ok(result)))
    });

    let address = "";
    let imported = block_on(client.is_address_imported(address)).unwrap();
    assert!(imported);
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_native_is_address_imported_fallback_to_address_info_is_watch_only() {
    let client = native_client_for_test();
    NativeClientImpl::validate_address.mock_safe(|_, _| {
        let result = ValidateAddressRes {
            is_valid: false,
            address: "".to_string(),
            script_pub_key: Default::default(),
            seg_id: None,
            is_mine: None,
            is_watch_only: None,
            is_script: false,
            account: None,
        };
        MockResult::Return(Box::new(futures01::future::ok(result)))
    });

    NativeClientImpl::get_address_info.mock_safe(|_, _| {
        let result = GetAddressInfoRes {
            is_mine: false,
            is_watch_only: true,
        };
        MockResult::Return(Box::new(futures01::future::ok(result)))
    });

    let address = "";
    let imported = block_on(client.is_address_imported(address)).unwrap();
    assert!(imported);
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_native_is_address_imported_fallback_to_address_info_false() {
    let client = native_client_for_test();
    NativeClientImpl::validate_address.mock_safe(|_, _| {
        let result = ValidateAddressRes {
            is_valid: false,
            address: "".to_string(),
            script_pub_key: Default::default(),
            seg_id: None,
            is_mine: None,
            is_watch_only: None,
            is_script: false,
            account: None,
        };
        MockResult::Return(Box::new(futures01::future::ok(result)))
    });

    NativeClientImpl::get_address_info.mock_safe(|_, _| {
        let result = GetAddressInfoRes {
            is_mine: false,
            is_watch_only: false,
        };
        MockResult::Return(Box::new(futures01::future::ok(result)))
    });

    let address = "";
    let imported = block_on(client.is_address_imported(address)).unwrap();
    assert!(!imported);
}

/// Test if the [`NativeClient::find_output_spend`] handle the conflicting transactions correctly.
/// https://github.com/KomodoPlatform/atomicDEX-API/pull/775
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_find_output_spend_skips_conflicting_transactions() {
    const LIST_SINCE_BLOCK_JSON: &str = r#"{"transactions":[{"involvesWatchonly":true,"account":"","address":"RAsbVN52LC2hEp3UWWSLbV8pJ8CneKjW9F","category":"send","amount":-0.01537462,"vout":0,"fee":-0.00001000,"rawconfirmations":-1,"confirmations":-1,"txid":"220c337006b2581c3da734ef9f1106601e8538ebab823d0dd6719a4d4580fd04","walletconflicts":["a2144bee4eac4b41ab1aed2dd8f854785b3ddebd617d48696dd84e62d129544b"],"time":1607831631,"timereceived":1607831631,"vjoinsplit":[],"size":320},{"involvesWatchonly":true,"account":"","address":"RAsbVN52LC2hEp3UWWSLbV8pJ8CneKjW9F","category":"send","amount":-0.01537462,"vout":0,"fee":-0.00001000,"rawconfirmations":-1,"confirmations":-1,"txid":"6fb83afb1bf309515fa429814bf07552eea951656fdee913f3aa687d513cd720","walletconflicts":["4aad6471f59e5912349cd7679bc029bfbd5da54d34c235d20500249f98f549e4"],"time":1607831556,"timereceived":1607831556,"vjoinsplit":[],"size":320},{"account":"","address":"RT9MpMyucqXiX8bZLimXBnrrn2ofmdGNKd","category":"receive","amount":0.54623851,"vout":2,"rawconfirmations":1617,"confirmations":1617,"blockhash":"000000000c33a387d73180220a5a8f2fe6081bad9bdfc0dba5a9985abcee8294","blockindex":7,"blocktime":1607957613,"expiryheight":0,"txid":"45e4900a2b330800a356a74ce2a97370596ad3a25e689e3ed5c36e421d12bbf7","walletconflicts":[],"time":1607957175,"timereceived":1607957175,"vjoinsplit":[],"size":567},{"involvesWatchonly":true,"account":"","address":"RT9MpMyucqXiX8bZLimXBnrrn2ofmdGNKd","category":"send","amount":-0.00797200,"vout":0,"fee":-0.00001000,"rawconfirmations":-1,"confirmations":-1,"txid":"bfc99c06d1a060cdbeba05620dc1c6fdb7351eb4c04b7aae578688ca6aeaeafd","walletconflicts":[],"time":1607957792,"timereceived":1607957792,"vjoinsplit":[],"size":286}],"lastblock":"06082d363f78174fd13b126994210d3c3ad9d073ee3983ad59fe8b76e6e3e071"}"#;
    // in the json above this transaction is only one not conflicting
    const NON_CONFLICTING_TXID: &str = "45e4900a2b330800a356a74ce2a97370596ad3a25e689e3ed5c36e421d12bbf7";
    let expected_txid: H256Json = hex::decode(NON_CONFLICTING_TXID).unwrap().as_slice().into();

    NativeClientImpl::get_block_hash.mock_safe(|_, _| {
        // no matter what we return here
        let blockhash: H256Json = hex::decode("000000000c33a387d73180220a5a8f2fe6081bad9bdfc0dba5a9985abcee8294")
            .unwrap()
            .as_slice()
            .into();
        MockResult::Return(Box::new(futures01::future::ok(blockhash)))
    });

    NativeClientImpl::list_since_block.mock_safe(|_, _| {
        let listsinceblockres: ListSinceBlockRes =
            json::from_str(LIST_SINCE_BLOCK_JSON).expect("Json is expected to be valid");
        MockResult::Return(Box::new(futures01::future::ok(listsinceblockres)))
    });

    static mut GET_RAW_TRANSACTION_BYTES_CALLED: usize = 0;
    NativeClientImpl::get_raw_transaction_bytes.mock_safe(move |_, txid| {
        unsafe { GET_RAW_TRANSACTION_BYTES_CALLED += 1 };
        assert_eq!(*txid, expected_txid);
        // no matter what we return here
        let bytes: BytesJson = hex::decode("0400008085202f890347d329798b508dc28ec99d8c6f6c7ced860a19a364e1bafe391cab89aeaac731020000006a47304402203ea8b380d0a7e64348869ef7c4c2bfa966fc7b148633003332fa8d0ab0c1bc5602202cc63fabdd2a6578c52d8f4f549069b16505f2ead48edc2b8de299be15aadf9a012102d8c948c6af848c588517288168faa397d6ba3ea924596d03d1d84f224b5123c2ffffffff1d1fd3a6b01710647a7f4a08c6de6075cb8e78d5069fa50f10c4a2a10ded2a95000000006a47304402203868945edc0f6dc2ee43d70a69ee4ec46ca188dc493173ce58924ba9bf6ee7a50220648ff99ce458ca72800758f6a1bd3800cd05ff9c3122f23f3653c25e09d22c79012102d8c948c6af848c588517288168faa397d6ba3ea924596d03d1d84f224b5123c2ffffffff7932150df8b4a1852b8b84b89b0d5322bf74665fb7f76a728369fd6895d3fd48000000006a4730440220127918c6f79c11f7f2376a6f3b750ed4c7103183181ad1218afcb2625ece9599022028c05e88d3a2f97cebd84a718cda33b62b48b18f16278fa8e531fd2155e61ee8012102d8c948c6af848c588517288168faa397d6ba3ea924596d03d1d84f224b5123c2ffffffff0329fd12000000000017a914cafb62e3e8bdb8db3735c39b92743ac6ebc9ef20870000000000000000166a14a7416b070c9bb98f4bafae55616f005a2a30bd6014b40c00000000001976a91450f4f098306f988d8843004689fae28c83ef16e888ac8cc5925f000000000000000000000000000000").unwrap().into();
        MockResult::Return(Box::new(futures01::future::ok(bytes)))
    });
    let client = native_client_for_test();

    // no matter what arguments we will pass to the function because of the mocks above
    let tx: UtxoTx = "0400008085202f89027f57730fcbbc2c72fb18bcc3766a713044831a117bb1cade3ed88644864f7333020000006a47304402206e3737b2fcf078b61b16fa67340cc3e79c5d5e2dc9ffda09608371552a3887450220460a332aa1b8ad8f2de92d319666f70751078b221199951f80265b4f7cef8543012102d8c948c6af848c588517288168faa397d6ba3ea924596d03d1d84f224b5123c2ffffffff42b916a80430b80a77e114445b08cf120735447a524de10742fac8f6a9d4170f000000006a473044022004aa053edafb9d161ea8146e0c21ed1593aa6b9404dd44294bcdf920a1695fd902202365eac15dbcc5e9f83e2eed56a8f2f0e5aded36206f9c3fabc668fd4665fa2d012102d8c948c6af848c588517288168faa397d6ba3ea924596d03d1d84f224b5123c2ffffffff03547b16000000000017a9143e8ad0e2bf573d32cb0b3d3a304d9ebcd0c2023b870000000000000000166a144e2b3c0323ab3c2dc6f86dc5ec0729f11e42f56103970400000000001976a91450f4f098306f988d8843004689fae28c83ef16e888ac89c5925f000000000000000000000000000000".into();
    let vout = 0;
    let from_block = 0;
    let actual = client
        .find_output_spend(
            tx.hash(),
            &tx.outputs[vout].script_pubkey,
            vout,
            BlockHashOrHeight::Height(from_block),
        )
        .wait();
    assert_eq!(actual, Ok(None));
    assert_eq!(unsafe { GET_RAW_TRANSACTION_BYTES_CALLED }, 1);
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_qtum_is_unspent_mature() {
    use crate::utxo::qtum::QtumBasedCoin;
    use rpc::v1::types::{ScriptType, SignedTransactionOutput, TransactionOutputScript};

    let mut coin_fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(native_client_for_test()), None, false);
    // Qtum's mature confirmations is 500 blocks
    coin_fields.conf.mature_confirmations = 500;
    let arc: UtxoArc = coin_fields.into();
    let coin = QtumCoin::from(arc);

    let empty_output = SignedTransactionOutput {
        value: Some(0.),
        n: 0,
        script: TransactionOutputScript {
            asm: "".into(),
            hex: "".into(),
            req_sigs: 0,
            script_type: ScriptType::NonStandard,
            addresses: vec![],
        },
    };
    let real_output = SignedTransactionOutput {
        value: Some(117.02430015),
        n: 1,
        script: TransactionOutputScript {
            asm: "03e71b9c152bb233ddfe58f20056715c51b054a1823e0aba108e6f1cea0ceb89c8 OP_CHECKSIG".into(),
            hex: "2103e71b9c152bb233ddfe58f20056715c51b054a1823e0aba108e6f1cea0ceb89c8ac".into(),
            req_sigs: 0,
            script_type: ScriptType::PubKey,
            addresses: vec![],
        },
    };

    let mut tx = RpcTransaction {
        hex: Default::default(),
        txid: "47d983175720ba2a67f36d0e1115a129351a2f340bdde6ecb6d6029e138fe920".into(),
        hash: None,
        size: Default::default(),
        vsize: Default::default(),
        version: 2,
        locktime: 0,
        vin: vec![],
        vout: vec![empty_output, real_output],
        blockhash: "c23882939ff695be36546ea998eb585e962b043396e4d91959477b9796ceb9e1".into(),
        confirmations: 421,
        rawconfirmations: None,
        time: 1590671504,
        blocktime: 1590671504,
        height: None,
    };

    // output is coinbase and has confirmations < QTUM_MATURE_CONFIRMATIONS
    assert!(!coin.is_qtum_unspent_mature(&tx));

    tx.confirmations = 501;
    // output is coinbase but has confirmations > QTUM_MATURE_CONFIRMATIONS
    assert!(coin.is_qtum_unspent_mature(&tx));

    tx.confirmations = 421;
    // remove empty output
    tx.vout.remove(0);
    // output is not coinbase
    assert!(coin.is_qtum_unspent_mature(&tx));
}

#[test]
fn test_get_sender_trade_fee_dynamic_tx_fee() {
    const DYNAMIC_FEE: u64 = 2055837;

    UtxoStandardCoin::get_tx_fee
        .mock_safe(|_| MockResult::Return(Box::pin(futures::future::ok(ActualTxFee::Dynamic(DYNAMIC_FEE)))));
    UtxoStandardCoin::get_unspent_ordered_list.mock_safe(|coin, _| {
        let cache = block_on(coin.as_ref().recently_spent_outpoints.lock());
        let unspents = vec![UnspentInfo {
            outpoint: OutPoint {
                hash: 1.into(),
                index: 0,
            },
            value: 222_222_000,
            height: Default::default(),
        }];
        MockResult::Return(Box::pin(futures::future::ok((unspents, cache))))
    });

    let client = NativeClient(Arc::new(NativeClientImpl::default()));
    let mut coin_fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(client), None, false);
    coin_fields.tx_fee = TxFee::Dynamic(EstimateFeeMethod::Standard);
    let coin = utxo_coin_from_fields(coin_fields);

    let my_balance = BigDecimal::from_str("2.22222").expect("!BigDecimal::from_str");

    // The mocks call `block_on` internally, so the outer driver must not be the tokio
    // `common::block_on` (nested runtime). `futures::executor::block_on` has no reactor
    // requirement here because every RPC on the path is mocked.
    let fee1 = futures::executor::block_on(coin.get_sender_trade_fee(
        TradePreimageValue::UpperBound(my_balance.clone()),
        FeeApproxStage::WithoutApprox,
    ))
    .expect("!get_sender_trade_fee");

    let value_without_fee = &my_balance - &fee1.amount.to_decimal();
    log!("value_without_fee "(value_without_fee));
    let fee2 = futures::executor::block_on(coin.get_sender_trade_fee(
        TradePreimageValue::Exact(value_without_fee),
        FeeApproxStage::WithoutApprox,
    ))
    .expect("!get_sender_trade_fee");
    log!("fee1 "(fee1.amount.to_decimal())" fee2 "(fee2.amount.to_decimal()));
    assert_eq!(fee1, fee2);

    // A much smaller Exact amount leaves a large change, so the builder materialises a
    // change output (the other branch of the fee estimator). The fee must still match:
    // the transaction shape (one input, one payment output, one change output) is the same.
    let fee3 = futures::executor::block_on(coin.get_sender_trade_fee(
        TradePreimageValue::Exact(BigDecimal::from(1)),
        FeeApproxStage::WithoutApprox,
    ))
    .expect("!get_sender_trade_fee");
    log!("fee3 "(fee3.amount.to_decimal()));
    assert_eq!(fee1, fee3);
}

/// Sender trade-fee preview (a swap payment of `amount` coins) for a fixed-fee policy, from `count` unspents of
/// `unspent_value` base units each.
#[cfg(not(target_arch = "wasm32"))]
fn fixed_policy_preview_fee_sat(policy: ActualTxFee, count: u64, unspent_value: u64, amount: &str) -> u64 {
    UtxoStandardCoin::get_tx_fee.mock_safe(move |_| MockResult::Return(Box::pin(futures::future::ok(policy))));
    UtxoStandardCoin::get_unspent_ordered_list.mock_safe(move |coin, _| {
        let cache = block_on(coin.as_ref().recently_spent_outpoints.lock());
        let unspents = (0..count)
            .map(|index| UnspentInfo {
                outpoint: OutPoint {
                    hash: 1.into(),
                    index: index as u32,
                },
                value: unspent_value,
                height: Default::default(),
            })
            .collect();
        MockResult::Return(Box::pin(futures::future::ok((unspents, cache))))
    });
    let client = NativeClient(Arc::new(NativeClientImpl::default()));
    let mut coin_fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(client), None, false);
    coin_fields.tx_fee = match policy {
        ActualTxFee::Dynamic(_) => TxFee::Dynamic(EstimateFeeMethod::Standard),
        ActualTxFee::FixedPerKb(rate) => TxFee::FixedPerKb(rate),
        ActualTxFee::FixedPerStartedKb(rate) => TxFee::FixedPerStartedKb(rate),
    };
    let coin = utxo_coin_from_fields(coin_fields);
    let fee = futures::executor::block_on(coin.get_sender_trade_fee(
        TradePreimageValue::Exact(BigDecimal::from_str(amount).unwrap()),
        FeeApproxStage::WithoutApprox,
    ))
    .expect("!get_sender_trade_fee");
    sat_from_big_decimal(&fee.amount.to_decimal(), TEST_COIN_DECIMALS).unwrap()
}

/// CRD ch.38 R38.6.6b: the fee of a transaction of s = 226, 1000 and 1001 bytes under each fixed policy.
#[test]
fn test_actual_tx_fee_arithmetic_r38_6_6b() {
    let plain = ActualTxFee::FixedPerKb(1000);
    assert_eq!([226, 1000, 1001].map(|s| plain.fee_for_size(s)), [226, 1000, 1001]);
    let whole_kb = ActualTxFee::FixedPerStartedKb(1000);
    assert_eq!([226, 1000, 1001].map(|s| whole_kb.fee_for_size(s)), [1000, 1000, 2000]);
    // Plain fixed truncates and the dynamic rate keeps its proportional rule.
    assert_eq!(ActualTxFee::FixedPerKb(1001).fee_for_size(999), 999);
    assert_eq!(ActualTxFee::Dynamic(20_000).fee_for_size(496), 9_920);
    // A zero-size transaction costs nothing under either rule; large products saturate.
    assert_eq!(whole_kb.fee_for_size(0), 0);
    assert_eq!(ActualTxFee::FixedPerKb(u64::MAX).fee_for_size(u64::MAX), u64::MAX);
    assert_eq!(ActualTxFee::FixedPerStartedKb(u64::MAX).fee_for_size(2000), u64::MAX);
    // The 34-byte change output increase.
    assert_eq!(plain.change_output_increase(226), 34);
    assert_eq!(whole_kb.change_output_increase(226), 0);
    assert_eq!(whole_kb.change_output_increase(966), 0);
    assert_eq!(whole_kb.change_output_increase(967), 1000);
    assert_eq!(whole_kb.change_output_increase(1000), 1000);
    assert_eq!(whole_kb.change_output_increase(1001), 0);
}

/// CRD ch.38 R38.6.6b: the builder charges the fee of the final transaction (change output included) under
/// each fixed policy, and a plain fixed coin no longer pays for a whole kilobyte.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn test_tx_builder_fixed_policies_r38_6_6b() {
    let coin = floor_test_coin("LTC", TxFee::FixedPerKb(1000), 1000, 8);
    let build = |policy: ActualTxFee| {
        let unspents = vec![UnspentInfo {
            value: 10_000_000,
            outpoint: OutPoint::default(),
            height: None,
        }];
        let outputs = vec![TransactionOutput {
            value: 100_000,
            script_pubkey: vec![0x51; 25].into(),
        }];
        let (tx, data) = block_on(
            UtxoTxBuilder::new(&coin)
                .add_available_inputs(unspents)
                .add_outputs(outputs)
                .with_fee(policy)
                .build(),
        )
        .unwrap();
        let size = utxo_common::tx_size_in_v_bytes(&UtxoAddressFormat::Standard, &UtxoTx::from(tx)) as u64;
        (data.fee_amount, size)
    };
    let (plain_fee, size) = build(ActualTxFee::FixedPerKb(1000));
    assert_eq!(
        plain_fee, size,
        "plain fixed 1,000 charges one base unit per byte of the final transaction"
    );
    assert!(size < 1000);
    let (whole_kb_fee, whole_kb_size) = build(ActualTxFee::FixedPerStartedKb(1000));
    assert_eq!((whole_kb_fee, whole_kb_size), (1000, size));
}

/// CRD ch.38 T38.6.6b.1: a transaction with a change output pays the policy's fee once over its whole size,
/// change output included. At a rate of 1,234 base units per 1,000 bytes a dynamic rate and the plain fixed
/// policy charge 826 at an estimated size of 670 bytes (4 inputs of 148 bytes, a 15-byte payment output and
/// the 34-byte change output) and 2,470 at 2,002 bytes (13 inputs), not the 825 and 2,469 of a separately
/// rounded change output. The whole-kilobyte policy adds one more rate only when the change output crosses a
/// kilobyte boundary.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn test_tx_builder_change_output_is_rounded_once_t38_6_6b_1() {
    let coin = floor_test_coin("TEST", TxFee::FixedPerKb(50_000), 1000, 8);
    // Builds a payment from `input_count` inputs of 100,000 base units each, all of them needed, with a payment
    // output whose script has `script_len` bytes. Returns the fee and the size of the transaction with the
    // change output.
    let build = |policy: ActualTxFee, input_count: u8, script_len: usize| {
        let unspents = (0..input_count)
            .map(|i| UnspentInfo {
                value: 100_000,
                outpoint: OutPoint {
                    hash: H256::from([i + 1; 32]),
                    index: 0,
                },
                height: None,
            })
            .collect::<Vec<_>>();
        let outputs = vec![TransactionOutput {
            value: u64::from(input_count - 1) * 100_000 + 50_000,
            script_pubkey: vec![0x51; script_len].into(),
        }];
        let (tx, data) = block_on(
            UtxoTxBuilder::new(&coin)
                .add_available_inputs(unspents)
                .add_outputs(outputs)
                .with_fee(policy)
                .build(),
        )
        .unwrap();
        assert_eq!(tx.inputs.len(), input_count as usize);
        // The payment and the change output.
        assert_eq!(tx.outputs.len(), 2);
        let size = utxo_common::tx_size_in_v_bytes(&UtxoAddressFormat::Standard, &UtxoTx::from(tx)) as u64;
        (data.fee_amount, size)
    };

    // (inputs, estimated size with the change output, fee of the single rounding, fee of the split rounding)
    let cases = [(4u8, 670u64, 826u64, 825u64), (13, 2002, 2470, 2469)];
    for (input_count, size, expected, split) in cases {
        assert_eq!(1234 * size / 1000, expected);
        assert_eq!(1234 * (size - 34) / 1000 + 1234 * 34 / 1000, split);
        for policy in [ActualTxFee::Dynamic(1234), ActualTxFee::FixedPerKb(1234)] {
            let (fee, built_size) = build(policy, input_count, 6);
            assert_eq!(built_size, size);
            assert_eq!(fee, expected, "{input_count} inputs, size {size}");
            assert_ne!(fee, split);
        }
    }

    // Whole-kilobyte policy: 636 bytes become 670 and 932 become 966, still one started kilobyte; 970 bytes
    // become 1,004, a further started kilobyte, and the change output adds one more rate.
    let whole_kb = ActualTxFee::FixedPerStartedKb(1234);
    assert_eq!(build(whole_kb, 4, 6), (1234, 670));
    assert_eq!(build(whole_kb, 6, 6), (1234, 966));
    assert_eq!(build(whole_kb, 6, 44), (2 * 1234, 1004));
    assert_eq!(build(whole_kb, 13, 6), (3 * 1234, 2002));
}

/// CRD ch.38 R38.6.6b: the trade-fee preview allows for one change output under each policy's own rule.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn test_sender_trade_fee_preview_fixed_policies_r38_6_6b() {
    // (unspents of 0.1 coin each, payment amount, plain fixed 1,000, whole-kilobyte 1,000)
    let cases = [
        // One input: 240 bytes plus the 34-byte change allowance, far below one kilobyte.
        (1u64, "0.099", 274, 1000),
        // Five inputs: 866 bytes with the change allowance, still one started kilobyte.
        (5, "0.499", 866, 1000),
        // Six inputs: 980 bytes without the change output, 1,014 with it. The allowance crosses into a
        // further started kilobyte, so the whole-kilobyte policy adds one more rate.
        (6, "0.599", 1014, 2000),
    ];
    for (count, amount, plain_expected, whole_kb_expected) in cases {
        let plain = fixed_policy_preview_fee_sat(ActualTxFee::FixedPerKb(1000), count, 10_000_000, amount);
        let whole_kb = fixed_policy_preview_fee_sat(ActualTxFee::FixedPerStartedKb(1000), count, 10_000_000, amount);
        assert_eq!(plain, plain_expected, "plain fixed, {count} inputs");
        assert_eq!(whole_kb, whole_kb_expected, "whole-kilobyte fixed, {count} inputs");
    }
}

/// CRD ch.60 T60.1 whole-kilobyte cases (R38.6.6e): F = r, not floor(0.496 r).
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn test_utxo_min_trading_vol_whole_kb_values() {
    // DINGO: whole-kilobyte 100,000,000, dust 100,000,000: max(10 * dust, 10 * r) = 10 coins.
    let dingo = floor_test_coin("DINGO", TxFee::FixedPerStartedKb(100_000_000), 100_000_000, 8);
    assert_eq!(dingo.min_trading_vol(), MmNumber::from("10"));
    // whole-kilobyte 300,000 with dust 1,000: 10 * 300,000 = 3,000,000 base units = 0.03.
    let coin = floor_test_coin("SYNTH-W", TxFee::FixedPerStartedKb(300_000), 1000, 8);
    assert_eq!(coin.min_trading_vol(), MmNumber::from("0.03"));
    // The same rate as plain fixed keeps the truncated 0.496 share.
    let plain = floor_test_coin("SYNTH-P", TxFee::FixedPerKb(300_000), 1000, 8);
    assert_eq!(plain.min_trading_vol(), MmNumber::from("0.01488"));
}

/// CRD ch.38 R38.6.6c: the legacy HTLC spend and refund estimate is R38.6.6b over 496 bytes.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn test_htlc_spend_fee_r38_6_6c() {
    assert_eq!(utxo_common::DEFAULT_SWAP_TX_SPEND_SIZE, 496);
    let size = utxo_common::DEFAULT_SWAP_TX_SPEND_SIZE;
    let cases = [
        // KMD, plain fixed 1,000.
        (ActualTxFee::FixedPerKb(1000), 496),
        // Whole-kilobyte r.
        (ActualTxFee::FixedPerStartedKb(1_000), 1_000),
        (ActualTxFee::FixedPerStartedKb(100_000_000), 100_000_000),
        // Dynamic e = 20,000: floor(0.496 * e), no volatility increase.
        (ActualTxFee::Dynamic(20_000), 9_920),
    ];
    for (policy, expected) in cases {
        UtxoStandardCoin::get_tx_fee.mock_safe(move |_| MockResult::Return(Box::pin(futures::future::ok(policy))));
        let coin = floor_test_coin("KMD", TxFee::FixedPerKb(1000), 1000, 8);
        let legacy = block_on(utxo_common::get_htlc_spend_fee(&coin, size)).unwrap();
        assert_eq!(legacy, expected, "legacy estimate under {policy:?}");
        // CRD ch.16 R14A: the version-two estimate is the same figure, r for the whole-kilobyte policy.
        let v2 = block_on(utxo_common::get_v2_swap_spend_fee(
            &coin,
            utxo_common::V2_SWAP_SPEND_TX_SIZE,
        ))
        .unwrap();
        assert_eq!(v2, expected, "version-two estimate under {policy:?}");
    }
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_validate_fee_wrong_sender() {
    let rpc_client = electrum_client_for_test(&[
        "marty.electrum1.cipig.net:10021",
        "marty.electrum2.cipig.net:10021",
        "marty.electrum3.cipig.net:10021",
    ]);
    let coin = utxo_coin_for_test(UtxoRpcClientEnum::Electrum(rpc_client), None, false);
    // A dex fee payment on MARTY (height 1338797): fae33048eb2941fd376d3af78acb44c5e254d6b58f574ebb0a360173235d5fdd
    let tx_bytes = hex::decode("0400008085202f8901f23ad270f0a668e30b9cf9c995c7c76ae536ba95e3a41205d1651f2eaf12e07e020000006a47304402206d1e0883b2917b8b9a0c481ccf8ef9ebe81257a83ea9e6123449b52f6ce9b7df0220424ae158c4d405ab2a80ceb9aca684893d8eda101a6dd959ad33ff1a6f840ca3012103de7fc0bcf2f3392441ce737d519e1db542364d6389ebdaf74a7ba877d9da301affffffff02bcf60100000000001976a914ca1e04745e8ca0c60d8c5881531d51bec470743f88ac7af9f702000000001976a9147551b5ce335bcc3818f9e7ff45538d9c08b70d1d88acf47ed169000000000000000000000000000000").unwrap();
    let taker_fee_tx = coin.tx_enum_from_bytes(&tx_bytes).unwrap();
    let amount: BigDecimal = "0.001287".parse().unwrap();
    let validate_err = coin
        .validate_fee(ValidateFeeArgs {
            fee_tx: &taker_fee_tx,
            expected_sender: test_dex_fee_addr_raw_pubkey(),
            fee_addr: test_dex_fee_addr_raw_pubkey(),
            dex_fee: &DexFee::Standard(amount.into()),
            min_block_number: 0,
            uuid: &[],
        })
        .wait()
        .unwrap_err();
    assert!(validate_err.contains("was sent from wrong address"));
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_validate_fee_min_block() {
    let rpc_client = electrum_client_for_test(&[
        "marty.electrum1.cipig.net:10021",
        "marty.electrum2.cipig.net:10021",
        "marty.electrum3.cipig.net:10021",
    ]);
    let coin = utxo_coin_for_test(UtxoRpcClientEnum::Electrum(rpc_client), None, false);
    // A dex fee payment on MARTY (height 1338797): fae33048eb2941fd376d3af78acb44c5e254d6b58f574ebb0a360173235d5fdd
    let tx_bytes = hex::decode("0400008085202f8901f23ad270f0a668e30b9cf9c995c7c76ae536ba95e3a41205d1651f2eaf12e07e020000006a47304402206d1e0883b2917b8b9a0c481ccf8ef9ebe81257a83ea9e6123449b52f6ce9b7df0220424ae158c4d405ab2a80ceb9aca684893d8eda101a6dd959ad33ff1a6f840ca3012103de7fc0bcf2f3392441ce737d519e1db542364d6389ebdaf74a7ba877d9da301affffffff02bcf60100000000001976a914ca1e04745e8ca0c60d8c5881531d51bec470743f88ac7af9f702000000001976a9147551b5ce335bcc3818f9e7ff45538d9c08b70d1d88acf47ed169000000000000000000000000000000").unwrap();
    let taker_fee_tx = coin.tx_enum_from_bytes(&tx_bytes).unwrap();
    let amount: BigDecimal = "0.001287".parse().unwrap();
    let sender_pub = hex::decode("03de7fc0bcf2f3392441ce737d519e1db542364d6389ebdaf74a7ba877d9da301a").unwrap();
    let validate_err = coin
        .validate_fee(ValidateFeeArgs {
            fee_tx: &taker_fee_tx,
            expected_sender: &sender_pub,
            fee_addr: test_dex_fee_addr_raw_pubkey(),
            dex_fee: &DexFee::Standard(amount.into()),
            min_block_number: 1338798,
            uuid: &[],
        })
        .wait()
        .unwrap_err();
    assert!(validate_err.contains("confirmed before min_block"));
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
// https://github.com/KomodoPlatform/atomicDEX-API/issues/857
fn test_validate_fee_bch_70_bytes_signature() {
    let rpc_client = electrum_client_for_test(&[
        "electrum1.cipig.net:10055",
        "electrum2.cipig.net:10055",
        "cashnode.bch.ninja:50001",
    ]);
    let coin = utxo_coin_for_test(UtxoRpcClientEnum::Electrum(rpc_client), None, false);
    // https://blockchair.com/bitcoin-cash/transaction/ccee05a6b5bbc6f50d2a65a5a3a04690d3e2d81082ad57d3ab471189f53dd70d
    let tx_bytes = hex::decode("0100000002cae89775f264e50f14238be86a7184b7f77bfe26f54067b794c546ec5eb9c91a020000006b483045022100d6ed080f722a0637a37552382f462230cc438984bc564bdb4b7094f06cfa38fa022062304a52602df1fbb3bebac4f56e1632ad456f62d9031f4983f07e546c8ec4d8412102ae7dc4ef1b49aadeff79cfad56664105f4d114e1716bc4f930cb27dbd309e521ffffffff11f386a6fe8f0431cb84f549b59be00f05e78f4a8a926c5e023a0d5f9112e8200000000069463043021f17eb93ed20a6f2cd357eabb41a4ec6329000ddc6d5b42ecbe642c5d41b206a022026bc4920c4ce3af751283574baa8e4a3efd4dad0d8fe6ba3ddf5d75628d36fda412102ae7dc4ef1b49aadeff79cfad56664105f4d114e1716bc4f930cb27dbd309e521ffffffff0210270000000000001976a914ca1e04745e8ca0c60d8c5881531d51bec470743f88ac57481c00000000001976a914bac11ce4cd2b1df2769c470d09b54f86df737e3c88ac035b4a60").unwrap();
    let taker_fee_tx = coin.tx_enum_from_bytes(&tx_bytes).unwrap();
    let amount: BigDecimal = "0.0001".parse().unwrap();
    let sender_pub = hex::decode("02ae7dc4ef1b49aadeff79cfad56664105f4d114e1716bc4f930cb27dbd309e521").unwrap();
    coin.validate_fee(ValidateFeeArgs {
        fee_tx: &taker_fee_tx,
        expected_sender: &sender_pub,
        fee_addr: test_dex_fee_addr_raw_pubkey(),
        dex_fee: &DexFee::Standard(amount.into()),
        min_block_number: 0,
        uuid: &[],
    })
    .wait()
    .unwrap();
}

#[test]
fn firo_verbose_block_deserialize() {
    let json = json!({
       "hash":"e21ea157b142270ba479a0aeb5571144b2a06f66a693c20675c624a6f211de0a",
       "confirmations":1,
       "strippedsize":234913,
       "size":234913,
       "weight":234913,
       "height":348355,
       "version":536875008,
       "versionHex":"20001000",
       "merkleroot":"b7fa3ce26f5b493397302c260905ca6f8c9ade56cab7cb314dc6f8a1d4c69245",
       "tx":[
          "166d2e6c6b8e1f29192737be5b0df79f7ccb286a898a3bf7253aa091e1002756",
          "f0bcbf10f2aa20d6891c14fdf64eb336df2d4466ebbc6bd5349c61478be77bd3",
          "0305f0fed2286b4504907bd2588dec5205f0807f11d003489b6748437728b6dc",
          "17f69f35b125de65e140de9bffe873702a4550379fb0ae4fe371f703c739e268",
          "ca60309ee4f846f607295aabcea2d0680ca23a7fbb8699ad1b597255ad6c5a73",
          "5aec101f7b2452d293c1a1c3889861bc8e96081f3ecd328859bc005c14d2737e",
          "bd9a8a2fdbad3db6c38e6472fd2e50d452a98553c8a105cb10afc85b5eaadee0",
          "0a52a67bf6ca3784f81b828616cda6bdca314402cded278d98f94b546784a58d",
          "55e6f918b2e7af2886499919b1c4a2ba341180934a4691a1a7166d6dadfcf8b9",
          "7a2d8b10b3bfc3037ee884699ca4770d96575b2d39179801d760d1c86377ff58",
          "ded160f1ec3e978daa2d8adb0b611223946db1c1155522cf9f0796e6f6c081fe"
       ],
       "cbTx":{
          "version":2,
          "height":348355,
          "merkleRootMNList":"5bd9041001ba65e1aea7a8d3982bb7fc2a8a561a1898d4e176a2cc4d242107b0",
          "merkleRootQuorums":"bfe0f35ec169f3b96eb66097138e70d1e52a66a2fc31a057df6298bbbc790fce"
       },
       "time":1614002775,
       "mediantime":1614001062,
       "nonce":43516489,
       "bits":"1b6d4183",
       "difficulty":599.8302783653238,
       "chainwork":"000000000000000000000000000000000000000000000000bb39407cfc6d253a",
       "previousblockhash":"71b81ff345f062e5c6eacbda63f64295590667a8d72428e4e71011675fe531e1",
       "chainlock":true
    });
    let _block: VerboseBlock = json::from_value(json).unwrap();
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn firo_lelantus_tx() {
    // https://explorer.firo.org/tx/06ed4b75010edcf404a315be70903473f44050c978bc37fbcee90e0b49114ba8
    let tx_hash = "06ed4b75010edcf404a315be70903473f44050c978bc37fbcee90e0b49114ba8".into();
    let electrum = electrum_client_for_test(&[
        "electrumx01.firo.org:50001",
        "electrumx02.firo.org:50001",
        "electrumx03.firo.org:50001",
    ]);
    let _tx = electrum.get_verbose_transaction(&tx_hash).wait().unwrap();
}

#[test]
#[ignore = "stale: the live Firo answer no longer carries nFees; see docs/DISABLED_TESTS.md. \
            The offline test firo_lelantus_tx_details_offline covers the fee path."]
fn firo_lelantus_tx_details() {
    // https://explorer.firo.org/tx/06ed4b75010edcf404a315be70903473f44050c978bc37fbcee90e0b49114ba8
    let electrum = electrum_client_for_test(&[
        "electrumx01.firo.org:50001",
        "electrumx02.firo.org:50001",
        "electrumx03.firo.org:50001",
    ]);
    let coin = utxo_coin_for_test(electrum.into(), None, false);
    let mut map = HashMap::new();

    let tx_hash = hex::decode("ad812911f5cba3eab7c193b6cd7020ea02fb5c25634ae64959c3171a6bd5a74d").unwrap();
    let tx_details = block_on(coin.tx_details_by_hash(&tx_hash, &mut map)).unwrap();

    let expected_fee = TxFeeDetails::Utxo(UtxoFeeDetails {
        coin: Some(TEST_COIN_NAME.into()),
        amount: "0.00003793".parse().unwrap(),
    });
    assert_eq!(Some(expected_fee), tx_details.fee_details);

    let tx_hash = hex::decode("06ed4b75010edcf404a315be70903473f44050c978bc37fbcee90e0b49114ba8").unwrap();
    let tx_details = block_on(coin.tx_details_by_hash(&tx_hash, &mut map)).unwrap();

    let expected_fee = TxFeeDetails::Utxo(UtxoFeeDetails {
        coin: Some(TEST_COIN_NAME.into()),
        amount: "0.00045778".parse().unwrap(),
    });
    assert_eq!(Some(expected_fee), tx_details.fee_details);
}

/// The fee of a FIRO Lelantus spend, without a server.
///
/// A Lelantus spend has no previous output to read an amount from: its input is a null previous output, so the
/// input amount of the transaction is zero, and its verbose form carries the fee of each such input in `nFees`.
/// `tx_details_by_hash` takes the fee as the sum of the `nFees` of the Lelantus inputs in that case. This runs
/// that real function over a verbose answer of the shape Firo gave, with the node call mocked.
///
/// `firo_lelantus_tx_details` made the same check against a live Firo server, whose answer no longer carries
/// `nFees` (see docs/DISABLED_TESTS.md).
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn firo_lelantus_tx_details_offline() {
    const P2PKH: &str = "76a91405aab5342166f8594baf17a7d9bef5d56744332788ac";
    // A null previous output, a one-byte scriptSig (the Lelantus spend opcode) and the final sequence.
    let null_input = format!("{}ffffffff01c9ffffffff", "00".repeat(32));
    // Version 1, two such inputs, one output of 100000 satoshi to a P2PKH script, lock time 0.
    let tx_hex = format!("0100000002{null_input}{null_input}01a08601000000000019{P2PKH}00000000");

    let lelantus_input = |n_fees: f64, serial: &str| {
        json!({
            "scriptSig": { "asm": "OP_LELANTUSJOINSPLIT", "hex": "c9" },
            "nFees": n_fees,
            "serials": [serial],
            "sequence": 4294967295u32,
        })
    };
    let verbose: RpcTransaction = json::from_value(json!({
        "hex": tx_hex,
        "txid": "4242424242424242424242424242424242424242424242424242424242424242",
        "version": 1,
        "locktime": 0,
        "vin": [
            lelantus_input(0.00003793, "0a0b0c0d"),
            lelantus_input(0.00045778, "0e0f1011"),
        ],
        "vout": [{
            "value": 0.001,
            "n": 0,
            "scriptPubKey": {
                "asm": "OP_DUP OP_HASH160 05aab5342166f8594baf17a7d9bef5d567443327 OP_EQUALVERIFY OP_CHECKSIG",
                "hex": P2PKH,
                "reqSigs": 1,
                "type": "pubkeyhash",
                "addresses": ["R9o9xTocqr6CeEDGDH6mEYpwLoMz6jNjMW"],
            },
        }],
        "blockhash": "4343434343434343434343434343434343434343434343434343434343434343",
        "confirmations": 10,
        "time": 1700000000,
        "blocktime": 1700000000,
        "height": 1000,
    }))
    .unwrap();
    // Both inputs parse as the Lelantus variant, not as a signed, coinbase or Sigma input.
    assert_eq!(verbose.vin.len(), 2);
    assert!(verbose
        .vin
        .iter()
        .all(|input| matches!(input, rpc::v1::types::TransactionInputEnum::Lelantus(_))));

    let tx_hash = [0x42u8; 32];
    NativeClient::get_verbose_transaction.mock_safe(move |_client, txid| {
        assert_eq!(*txid, H256Json::from(&tx_hash[..]));
        MockResult::Return(Box::new(futures01::future::ok(verbose.clone())))
    });

    let client = NativeClient(Arc::new(NativeClientImpl::default()));
    let coin = utxo_coin_for_test(UtxoRpcClientEnum::Native(client), None, false);
    let mut map = HashMap::new();
    let tx_details = block_on(coin.tx_details_by_hash(&tx_hash, &mut map)).unwrap();

    // No input has an amount to read, so the fee is the sum of the `nFees` of the two inputs.
    let expected_fee = TxFeeDetails::Utxo(UtxoFeeDetails {
        coin: Some(TEST_COIN_NAME.into()),
        amount: "0.00049571".parse().unwrap(),
    });
    assert_eq!(Some(expected_fee), tx_details.fee_details);
    assert_eq!(tx_details.total_amount, BigDecimal::from(0));
    assert_eq!(tx_details.spent_by_me, BigDecimal::from(0));
    assert!(tx_details.from.is_empty());
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_generate_tx_doge_fee() {
    // DOGE is plain fixed 1,000,000 per kB with a forced minimum relay fee; both are proportional (R38.6.6b).
    let config = json!({
        "coin": "DOGE",
        "name": "dogecoin",
        "fname": "Dogecoin",
        "rpcport": 22555,
        "pubtype": 30,
        "p2shtype": 22,
        "wiftype": 158,
        "txfee": 1000000,
        "force_min_relay_fee": true,
        "mm2": 1,
        "required_confirmations": 2,
        "avg_blocktime": 1,
        "protocol": {
            "type": "UTXO"
        }
    });
    let request = json!({
        "method": "electrum",
        "coin": "DOGE",
        "servers": [{"url": "electrum1.cipig.net:10060"},{"url": "electrum2.cipig.net:10060"},{"url": "electrum3.cipig.net:10060"}],
    });
    let ctx = MmCtxBuilder::default().into_mm_arc();
    let params = UtxoActivationParams::from_legacy_req(&request).unwrap();

    let doge = block_on(utxo_standard_coin_with_priv_key(
        &ctx, "DOGE", &config, &params, &[1; 32],
    ))
    .unwrap();

    // CRD ch.38 R38.6.6b: the fee is proportional to the size of the final transaction, never below the
    // forced minimum relay fee: max(floor(txfee * size / 1000), floor(relay_rate * size / 1000)).
    let relay_rate_sat = {
        let relay = block_on(doge.as_ref().rpc_client.get_relay_fee().compat()).unwrap();
        sat_from_big_decimal(&relay, doge.as_ref().decimals).unwrap()
    };
    for outputs_count in [1usize, 40, 60] {
        let unspents = vec![UnspentInfo {
            outpoint: Default::default(),
            value: 1000000000000,
            height: None,
        }];
        let outputs = vec![
            TransactionOutput {
                value: 100000000,
                script_pubkey: vec![0; 26].into(),
            };
            outputs_count
        ];
        let builder = UtxoTxBuilder::new(&doge)
            .add_available_inputs(unspents)
            .add_outputs(outputs);
        let (tx, data) = block_on(builder.build()).unwrap();
        let size = utxo_common::tx_size_in_v_bytes(&UtxoAddressFormat::Standard, &UtxoTx::from(tx)) as u64;
        let expected_fee = ActualTxFee::FixedPerKb(1000000)
            .fee_for_size(size)
            .max(ActualTxFee::Dynamic(relay_rate_sat).fee_for_size(size));
        assert_eq!(expected_fee, data.fee_amount, "{outputs_count} outputs, {size} bytes");
    }
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn doge_mtp() {
    let electrum = electrum_client_for_test(&[
        "electrum1.cipig.net:10060",
        "electrum2.cipig.net:10060",
        "electrum3.cipig.net:10060",
    ]);
    let mtp = electrum
        .get_median_time_past(3631820, NonZeroU64::new(11).unwrap(), CoinVariant::Standard)
        .wait()
        .unwrap();
    assert_eq!(mtp, 1614849084);
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn firo_mtp() {
    let electrum = electrum_client_for_test(&[
        "electrumx01.firo.org:50001",
        "electrumx02.firo.org:50001",
        "electrumx03.firo.org:50001",
    ]);
    let mtp = electrum
        .get_median_time_past(356730, NonZeroU64::new(11).unwrap(), CoinVariant::Standard)
        .wait()
        .unwrap();
    assert_eq!(mtp, 1616492629);
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn verus_mtp() {
    let electrum = electrum_client_for_test(&["el0.verus.io:17485", "el1.verus.io:17485", "el2.verus.io:17485"]);
    let mtp = electrum
        .get_median_time_past(1480113, NonZeroU64::new(11).unwrap(), CoinVariant::Standard)
        .wait()
        .unwrap();
    assert_eq!(mtp, 1618579909);
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn sys_mtp() {
    let electrum = electrum_client_for_test(&[
        "electrum1.cipig.net:10064",
        "electrum2.cipig.net:10064",
        "electrum3.cipig.net:10064",
    ]);
    let mtp = electrum
        .get_median_time_past(1006678, NonZeroU64::new(11).unwrap(), CoinVariant::Standard)
        .wait()
        .unwrap();
    assert_eq!(mtp, 1620019628);
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn btc_mtp() {
    let electrum = electrum_client_for_test(&[
        "electrum1.cipig.net:10000",
        "electrum2.cipig.net:10000",
        "electrum3.cipig.net:10000",
    ]);
    let mtp = electrum
        .get_median_time_past(681659, NonZeroU64::new(11).unwrap(), CoinVariant::Standard)
        .wait()
        .unwrap();
    assert_eq!(mtp, 1620019527);
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn rvn_mtp() {
    let electrum = electrum_client_for_test(&[
        "electrum1.cipig.net:10051",
        "electrum2.cipig.net:10051",
        "electrum3.cipig.net:10051",
    ]);
    let mtp = electrum
        .get_median_time_past(1968120, NonZeroU64::new(11).unwrap(), CoinVariant::Standard)
        .wait()
        .unwrap();
    assert_eq!(mtp, 1633946264);
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn qtum_mtp() {
    let electrum = electrum_client_for_test(&[
        "electrum1.cipig.net:10050",
        "electrum2.cipig.net:10050",
        "electrum3.cipig.net:10050",
    ]);
    let mtp = electrum
        .get_median_time_past(681659, NonZeroU64::new(11).unwrap(), CoinVariant::Qtum)
        .wait()
        .unwrap();
    assert_eq!(mtp, 1598854128);
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn zer_mtp() {
    let electrum = electrum_client_for_test(&[
        "electrum1.cipig.net:10065",
        "electrum2.cipig.net:10065",
        "electrum3.cipig.net:10065",
    ]);
    let mtp = electrum
        .get_median_time_past(1130915, NonZeroU64::new(11).unwrap(), CoinVariant::Standard)
        .wait()
        .unwrap();
    assert_eq!(mtp, 1623240214);
}

#[test]
#[ignore]
fn test_tx_details_kmd_rewards() {
    let electrum = electrum_client_for_test(&[
        "electrum1.cipig.net:10001",
        "electrum2.cipig.net:10001",
        "electrum3.cipig.net:10001",
    ]);
    let mut fields = utxo_coin_fields_for_test(electrum.into(), None, false);
    fields.conf.ticker = "KMD".to_owned();
    fields.derivation_method = DerivationMethod::Iguana(Address::from("RMGJ9tRST45RnwEKHPGgBLuY3moSYP7Mhk"));
    let coin = utxo_coin_from_fields(fields);

    let mut input_transactions = HistoryUtxoTxMap::new();
    let hash = hex::decode("535ffa3387d3fca14f4a4d373daf7edf00e463982755afce89bc8c48d8168024").unwrap();
    let tx_details = block_on(coin.tx_details_by_hash(&hash, &mut input_transactions)).expect("!tx_details_by_hash");

    let expected_fee = TxFeeDetails::Utxo(UtxoFeeDetails {
        coin: Some("KMD".into()),
        amount: BigDecimal::from_str("0.00001").unwrap(),
    });
    assert_eq!(tx_details.fee_details, Some(expected_fee));

    let expected_kmd_rewards = KmdRewardsDetails {
        amount: BigDecimal::from_str("0.10431954").unwrap(),
        claimed_by_me: true,
    };
    assert_eq!(tx_details.kmd_rewards, Some(expected_kmd_rewards));
}

/// If the ticker is `KMD` AND no rewards were accrued due to a value less than 10 or for any other reasons,
/// then `TransactionDetails::kmd_rewards` has to be `Some(0)`, not `None`.
/// https://kmdexplorer.io/tx/f09e8894959e74c1e727ffa5a753a30bf2dc6d5d677cc1f24b7ee5bb64e32c7d
#[test]
#[ignore]
#[cfg(not(target_arch = "wasm32"))]
fn test_tx_details_kmd_rewards_claimed_by_other() {
    let electrum = electrum_client_for_test(&[
        "electrum1.cipig.net:10001",
        "electrum2.cipig.net:10001",
        "electrum3.cipig.net:10001",
    ]);
    let mut fields = utxo_coin_fields_for_test(electrum.into(), None, false);
    fields.conf.ticker = "KMD".to_owned();
    fields.derivation_method = DerivationMethod::Iguana(Address::from("RMGJ9tRST45RnwEKHPGgBLuY3moSYP7Mhk"));
    let coin = utxo_coin_from_fields(fields);

    let mut input_transactions = HistoryUtxoTxMap::new();
    let hash = hex::decode("f09e8894959e74c1e727ffa5a753a30bf2dc6d5d677cc1f24b7ee5bb64e32c7d").unwrap();
    let tx_details = block_on(coin.tx_details_by_hash(&hash, &mut input_transactions)).expect("!tx_details_by_hash");

    let expected_fee = TxFeeDetails::Utxo(UtxoFeeDetails {
        coin: Some("KMD".into()),
        amount: BigDecimal::from_str("0.00001").unwrap(),
    });
    assert_eq!(tx_details.fee_details, Some(expected_fee));

    let expected_kmd_rewards = KmdRewardsDetails {
        amount: BigDecimal::from_str("0.00022428").unwrap(),
        claimed_by_me: false,
    };
    assert_eq!(tx_details.kmd_rewards, Some(expected_kmd_rewards));
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_tx_details_bch_no_rewards() {
    let electrum = electrum_client_for_test(&[
        "bitcoin-cash.devmole.eu:5001",
        "tbch.loping.net:60001",
        "blackie.c3-soft.com:60001",
        "bch0.kister.net:51001",
        "testnet.imaginary.cash:50001",
    ]);
    let coin = utxo_coin_for_test(electrum.into(), None, false);

    let mut input_transactions = HistoryUtxoTxMap::new();
    let hash = hex::decode("eb13d926f15cbb896e0bcc7a1a77a4ec63504e57a1524c13a7a9b80f43ecb05c").unwrap();
    let tx_details = block_on(coin.tx_details_by_hash(&hash, &mut input_transactions)).expect("!tx_details_by_hash");

    let expected_fee = TxFeeDetails::Utxo(UtxoFeeDetails {
        coin: Some(TEST_COIN_NAME.into()),
        amount: BigDecimal::from_str("0.00000452").unwrap(),
    });
    assert_eq!(tx_details.fee_details, Some(expected_fee));
    assert_eq!(tx_details.kmd_rewards, None);
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_update_kmd_rewards() {
    // 535ffa3387d3fca14f4a4d373daf7edf00e463982755afce89bc8c48d8168024
    const OUTDATED_TX_DETAILS: &str = r#"{"tx_hex":"0400008085202f8901afcadb73880bc1c9e7ce96b8274c2e2a4547415e649f425f98791685be009b73020000006b483045022100b8fbb77efea482b656ad16fc53c5a01d289054c2e429bf1d7bab16c3e822a83602200b87368a95c046b2ce6d0d092185138a3f234a7eb0d7f8227b196ef32358b93f012103b1e544ce2d860219bc91314b5483421a553a7b33044659eff0be9214ed58adddffffffff01dd15c293000000001976a91483762a373935ca241d557dfce89171d582b486de88ac99fe9960000000000000000000000000000000","tx_hash":"535ffa3387d3fca14f4a4d373daf7edf00e463982755afce89bc8c48d8168024","from":["RMGJ9tRST45RnwEKHPGgBLuY3moSYP7Mhk"],"to":["RMGJ9tRST45RnwEKHPGgBLuY3moSYP7Mhk"],"total_amount":"24.68539379","spent_by_me":"24.68539379","received_by_me":"24.78970333","my_balance_change":"0.10430954","block_height":2387532,"timestamp":1620705483,"fee_details":{"type":"Utxo","amount":"-0.10430954"},"coin":"KMD","internal_id":"535ffa3387d3fca14f4a4d373daf7edf00e463982755afce89bc8c48d8168024"}"#;

    let electrum = electrum_client_for_test(&[
        "electrum1.cipig.net:10001",
        "electrum2.cipig.net:10001",
        "electrum3.cipig.net:10001",
    ]);
    let mut fields = utxo_coin_fields_for_test(electrum.into(), None, false);
    fields.conf.ticker = "KMD".to_owned();
    fields.derivation_method = DerivationMethod::Iguana(Address::from("RMGJ9tRST45RnwEKHPGgBLuY3moSYP7Mhk"));
    let coin = utxo_coin_from_fields(fields);

    let mut input_transactions = HistoryUtxoTxMap::default();
    let mut tx_details: TransactionDetails = json::from_str(OUTDATED_TX_DETAILS).unwrap();
    block_on(coin.update_kmd_rewards(&mut tx_details, &mut input_transactions)).expect("!update_kmd_rewards");

    let expected_rewards = KmdRewardsDetails {
        amount: BigDecimal::from_str("0.10431954").unwrap(),
        claimed_by_me: true,
    };
    assert_eq!(tx_details.kmd_rewards, Some(expected_rewards));

    let expected_fee_details = TxFeeDetails::Utxo(UtxoFeeDetails {
        coin: Some("KMD".into()),
        amount: BigDecimal::from_str("0.00001").unwrap(),
    });
    assert_eq!(tx_details.fee_details, Some(expected_fee_details));
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_update_kmd_rewards_claimed_not_by_me() {
    // The custom 535ffa3387d3fca14f4a4d373daf7edf00e463982755afce89bc8c48d8168024 transaction with the additional 'from' address.
    const OUTDATED_TX_DETAILS: &str = r#"{"tx_hex":"0400008085202f8901afcadb73880bc1c9e7ce96b8274c2e2a4547415e649f425f98791685be009b73020000006b483045022100b8fbb77efea482b656ad16fc53c5a01d289054c2e429bf1d7bab16c3e822a83602200b87368a95c046b2ce6d0d092185138a3f234a7eb0d7f8227b196ef32358b93f012103b1e544ce2d860219bc91314b5483421a553a7b33044659eff0be9214ed58adddffffffff01dd15c293000000001976a91483762a373935ca241d557dfce89171d582b486de88ac99fe9960000000000000000000000000000000","tx_hash":"535ffa3387d3fca14f4a4d373daf7edf00e463982755afce89bc8c48d8168024","from":["RMGJ9tRST45RnwEKHPGgBLuY3moSYP7Mhk", "RMDc4fvQeekJwrXxuaw1R2b7CTPEuVguMP"],"to":["RMGJ9tRST45RnwEKHPGgBLuY3moSYP7Mhk"],"total_amount":"24.68539379","spent_by_me":"24.68539379","received_by_me":"24.78970333","my_balance_change":"0.10430954","block_height":2387532,"timestamp":1620705483,"fee_details":{"type":"Utxo","amount":"-0.10430954"},"coin":"KMD","internal_id":"535ffa3387d3fca14f4a4d373daf7edf00e463982755afce89bc8c48d8168024"}"#;

    let electrum = electrum_client_for_test(&[
        "electrum1.cipig.net:10001",
        "electrum2.cipig.net:10001",
        "electrum3.cipig.net:10001",
    ]);
    let mut fields = utxo_coin_fields_for_test(electrum.into(), None, false);
    fields.conf.ticker = "KMD".to_owned();
    fields.derivation_method = DerivationMethod::Iguana(Address::from("RMGJ9tRST45RnwEKHPGgBLuY3moSYP7Mhk"));
    let coin = utxo_coin_from_fields(fields);

    let mut input_transactions = HistoryUtxoTxMap::default();
    let mut tx_details: TransactionDetails = json::from_str(OUTDATED_TX_DETAILS).unwrap();
    block_on(coin.update_kmd_rewards(&mut tx_details, &mut input_transactions)).expect("!update_kmd_rewards");

    let expected_rewards = KmdRewardsDetails {
        amount: BigDecimal::from_str("0.10431954").unwrap(),
        claimed_by_me: false,
    };
    assert_eq!(tx_details.kmd_rewards, Some(expected_rewards));

    let expected_fee_details = TxFeeDetails::Utxo(UtxoFeeDetails {
        coin: Some("KMD".into()),
        amount: BigDecimal::from_str("0.00001").unwrap(),
    });
    assert_eq!(tx_details.fee_details, Some(expected_fee_details));
}

/// https://github.com/KomodoPlatform/atomicDEX-API/issues/966
#[test]
fn test_parse_tx_with_huge_locktime() {
    let verbose = r#"{"hex":"0400008085202f89010c03a2b3d8f97139a623f0759224c657513752b705b5c689a256d52b8f8279f200000000d8483045022100fa07821f4739890fa3518c73ecb4917f4a8e7a1c7a803a0d0aea28f991f14f84022041ac557507d6c9786128828c7b2fca7d5c345ba57c8050e3edb29be0c1e5d2660120bdb3d550a68dfaeebe4c416e5750d20d27617bbfb29756843d605a0570ae787b004c6b63046576ba60b17521039ef1b42c635c32440099910bbe1c5e8b0c9373274c3f21cf1003750fc88d3499ac6782012088a914a4f9f1009dcb778bf1c26052258284b32c9075098821031bb83b58ec130e28e0a6d5d2acf2eb01b0d3f1670e021d47d31db8a858219da8ac68ffffffff014ddbf305000000001976a914c3f710deb7320b0efa6edb14e3ebeeb9155fa90d88acf5b98899000000000000000000000000000000","txid":"3b666753b77e28da8a4d858339825315f32516cc147fa743329c7248bd0c6902","overwintered":true,"version":4,"versiongroupid":"892f2085","locktime":2575874549,"expiryheight":0,"vin":[{"txid":"f279828f2bd556a289c6b505b752375157c6249275f023a63971f9d8b3a2030c","vout":0,"scriptSig":{"asm":"3045022100fa07821f4739890fa3518c73ecb4917f4a8e7a1c7a803a0d0aea28f991f14f84022041ac557507d6c9786128828c7b2fca7d5c345ba57c8050e3edb29be0c1e5d266[ALL]bdb3d550a68dfaeebe4c416e5750d20d27617bbfb29756843d605a0570ae787b063046576ba60b17521039ef1b42c635c32440099910bbe1c5e8b0c9373274c3f21cf1003750fc88d3499ac6782012088a914a4f9f1009dcb778bf1c26052258284b32c9075098821031bb83b58ec130e28e0a6d5d2acf2eb01b0d3f1670e021d47d31db8a858219da8ac68","hex":"483045022100fa07821f4739890fa3518c73ecb4917f4a8e7a1c7a803a0d0aea28f991f14f84022041ac557507d6c9786128828c7b2fca7d5c345ba57c8050e3edb29be0c1e5d2660120bdb3d550a68dfaeebe4c416e5750d20d27617bbfb29756843d605a0570ae787b004c6b63046576ba60b17521039ef1b42c635c32440099910bbe1c5e8b0c9373274c3f21cf1003750fc88d3499ac6782012088a914a4f9f1009dcb778bf1c26052258284b32c9075098821031bb83b58ec130e28e0a6d5d2acf2eb01b0d3f1670e021d47d31db8a858219da8ac68"},"sequence":4294967295}],"vout":[{"value":0.99867469,"valueZat":99867469,"valueSat":99867469,"n":0,"scriptPubKey":{"asm":"OP_DUPOP_HASH160c3f710deb7320b0efa6edb14e3ebeeb9155fa90dOP_EQUALVERIFYOP_CHECKSIG","hex":"76a914c3f710deb7320b0efa6edb14e3ebeeb9155fa90d88ac","reqSigs":1,"type":"pubkeyhash","addresses":["t1bjmkBWkzLWk3mHFoybXE5daGRY9pk1fxF"]}}],"vjoinsplit":[],"valueBalance":0.0,"valueBalanceZat":0,"vShieldedSpend":[],"vShieldedOutput":[],"blockhash":"0000077e33e838d9967427018a6e7049d8619ae556acb3e80c070990e90b67fc","height":1127478,"confirmations":2197,"time":1622825622,"blocktime":1622825622}"#;
    let verbose_tx: RpcTransaction = json::from_str(verbose).expect("!json::from_str");
    let _: UtxoTx = deserialize(verbose_tx.hex.as_slice()).unwrap();
}

#[test]
fn tbch_electroncash_verbose_tx() {
    let verbose = r#"{"blockhash":"00000000000d93dbc9c6e95c37044d584be959d24e514533b3a82f0f61dddc03","blocktime":1626262632,"confirmations":3708,"hash":"e64531613f909647651ac3f8fd72f3e6f72ac6e01c5a1d923884a10476f56a7f","height":1456230,"hex":"0100000002ebc10f58f220ec1bad5d634684ae649aa7bdd2f9c9081d36e5384e579caa95c2020000006a4730440220639ac218f572520c7d8addae74be6bfdefa9c86bc91474b6dedd7e117d232085022015a92f45f9ae5cee08c188e01fc614b77c461a41733649a55abfcc3e7ca207444121036879df230663db4cd083c8eeb0f293f46abc460ad3c299b0089b72e6d472202cffffffffebc10f58f220ec1bad5d634684ae649aa7bdd2f9c9081d36e5384e579caa95c2030000006a47304402204c27a2c04df44f34bd71ec69cc0a24291a96f265217473affb3c3fce2dbd937202202c2ad2e6cfaac3901c807d9b048ccb2b5e7b0dbd922f2066e637f6bbf459313a4121036879df230663db4cd083c8eeb0f293f46abc460ad3c299b0089b72e6d472202cffffffff040000000000000000406a04534c500001010453454e4420bb309e48930671582bea508f9a1d9b491e49b69be3d6f372dc08da2ac6e90eb70800000000000003e808000000000000f5fee80300000000000017a9146569d9a853a1934c642223a9432f18c3b3f2a64b87e8030000000000001976a9148cfffc2409d063437d6aa8b75a009b9ba51b71fc88ac67a84601000000001976a9148cfffc2409d063437d6aa8b75a009b9ba51b71fc88ac87caee60","locktime":1626262151,"size":477,"time":1626262632,"txid":"e64531613f909647651ac3f8fd72f3e6f72ac6e01c5a1d923884a10476f56a7f","version":1,"vin":[{"coinbase":null,"scriptSig":{"asm":"OP_PUSHBYTES_71 30440220639ac218f572520c7d8addae74be6bfdefa9c86bc91474b6dedd7e117d232085022015a92f45f9ae5cee08c188e01fc614b77c461a41733649a55abfcc3e7ca2074441 OP_PUSHBYTES_33 036879df230663db4cd083c8eeb0f293f46abc460ad3c299b0089b72e6d472202c","hex":"4730440220639ac218f572520c7d8addae74be6bfdefa9c86bc91474b6dedd7e117d232085022015a92f45f9ae5cee08c188e01fc614b77c461a41733649a55abfcc3e7ca207444121036879df230663db4cd083c8eeb0f293f46abc460ad3c299b0089b72e6d472202c"},"sequence":4294967295,"txid":"c295aa9c574e38e5361d08c9f9d2bda79a64ae8446635dad1bec20f2580fc1eb","vout":2},{"coinbase":null,"scriptSig":{"asm":"OP_PUSHBYTES_71 304402204c27a2c04df44f34bd71ec69cc0a24291a96f265217473affb3c3fce2dbd937202202c2ad2e6cfaac3901c807d9b048ccb2b5e7b0dbd922f2066e637f6bbf459313a41 OP_PUSHBYTES_33 036879df230663db4cd083c8eeb0f293f46abc460ad3c299b0089b72e6d472202c","hex":"47304402204c27a2c04df44f34bd71ec69cc0a24291a96f265217473affb3c3fce2dbd937202202c2ad2e6cfaac3901c807d9b048ccb2b5e7b0dbd922f2066e637f6bbf459313a4121036879df230663db4cd083c8eeb0f293f46abc460ad3c299b0089b72e6d472202c"},"sequence":4294967295,"txid":"c295aa9c574e38e5361d08c9f9d2bda79a64ae8446635dad1bec20f2580fc1eb","vout":3}],"vout":[{"n":0,"scriptPubKey":{"addresses":[],"asm":"OP_RETURN OP_PUSHBYTES_4 534c5000 OP_PUSHBYTES_1 01 OP_PUSHBYTES_4 53454e44 OP_PUSHBYTES_32 bb309e48930671582bea508f9a1d9b491e49b69be3d6f372dc08da2ac6e90eb7 OP_PUSHBYTES_8 00000000000003e8 OP_PUSHBYTES_8 000000000000f5fe","hex":"6a04534c500001010453454e4420bb309e48930671582bea508f9a1d9b491e49b69be3d6f372dc08da2ac6e90eb70800000000000003e808000000000000f5fe","type":"nulldata"},"value_coin":0.0,"value_satoshi":0},{"n":1,"scriptPubKey":{"addresses":["bchtest:ppjknkdg2wsexnryyg36jse0rrpm8u4xfv9hwa0rgl"],"asm":"OP_HASH160 OP_PUSHBYTES_20 6569d9a853a1934c642223a9432f18c3b3f2a64b OP_EQUAL","hex":"a9146569d9a853a1934c642223a9432f18c3b3f2a64b87","type":"scripthash"},"value_coin":0.00001,"value_satoshi":1000},{"n":2,"scriptPubKey":{"addresses":["bchtest:qzx0llpyp8gxxsmad25twksqnwd62xm3lsnnczzt66"],"asm":"OP_DUP OP_HASH160 OP_PUSHBYTES_20 8cfffc2409d063437d6aa8b75a009b9ba51b71fc OP_EQUALVERIFY OP_CHECKSIG","hex":"76a9148cfffc2409d063437d6aa8b75a009b9ba51b71fc88ac","type":"pubkeyhash"},"value_coin":0.00001,"value_satoshi":1000},{"n":3,"scriptPubKey":{"addresses":["bchtest:qzx0llpyp8gxxsmad25twksqnwd62xm3lsnnczzt66"],"asm":"OP_DUP OP_HASH160 OP_PUSHBYTES_20 8cfffc2409d063437d6aa8b75a009b9ba51b71fc OP_EQUALVERIFY OP_CHECKSIG","hex":"76a9148cfffc2409d063437d6aa8b75a009b9ba51b71fc88ac","type":"pubkeyhash"},"value_coin":0.21407847,"value_satoshi":21407847}]}"#;
    let _: RpcTransaction = json::from_str(verbose).expect("!json::from_str");
}

#[test]
fn tbch_electroncash_verbose_tx_unconfirmed() {
    let verbose = r#"{"blockhash":null,"blocktime":null,"confirmations":null,"hash":"e5c9ec5013fca3a62fdf880d1a98f1096a00d20ceaeb6a4cb88ddbea6f1e185a","height":null,"hex":"01000000017f6af57604a18438921d5a1ce0c62af7e6f372fdf8c31a654796903f613145e6030000006b483045022100c335dd0f22e047b806a9d84e02b70aab609093e960888f6f1878e605a173e3da02201c274ce4983d8e519a47c4bd17aeca897b084954ce7a9d77033100e06aa999304121036879df230663db4cd083c8eeb0f293f46abc460ad3c299b0089b72e6d472202cffffffff0280969800000000001976a914eed5d3ad264ffc68fc0a6454e1696a30d8f405be88acbe0dae00000000001976a9148cfffc2409d063437d6aa8b75a009b9ba51b71fc88ac7a361261","locktime":1628583546,"size":226,"time":null,"txid":"e5c9ec5013fca3a62fdf880d1a98f1096a00d20ceaeb6a4cb88ddbea6f1e185a","version":1,"vin":[{"coinbase":null,"scriptSig":{"asm":"OP_PUSHBYTES_72 3045022100c335dd0f22e047b806a9d84e02b70aab609093e960888f6f1878e605a173e3da02201c274ce4983d8e519a47c4bd17aeca897b084954ce7a9d77033100e06aa9993041 OP_PUSHBYTES_33 036879df230663db4cd083c8eeb0f293f46abc460ad3c299b0089b72e6d472202c","hex":"483045022100c335dd0f22e047b806a9d84e02b70aab609093e960888f6f1878e605a173e3da02201c274ce4983d8e519a47c4bd17aeca897b084954ce7a9d77033100e06aa999304121036879df230663db4cd083c8eeb0f293f46abc460ad3c299b0089b72e6d472202c"},"sequence":4294967295,"txid":"e64531613f909647651ac3f8fd72f3e6f72ac6e01c5a1d923884a10476f56a7f","vout":3}],"vout":[{"n":0,"scriptPubKey":{"addresses":["bchtest:qrhdt5adye8lc68upfj9fctfdgcd3aq9hctf8ft6md"],"asm":"OP_DUP OP_HASH160 OP_PUSHBYTES_20 eed5d3ad264ffc68fc0a6454e1696a30d8f405be OP_EQUALVERIFY OP_CHECKSIG","hex":"76a914eed5d3ad264ffc68fc0a6454e1696a30d8f405be88ac","type":"pubkeyhash"},"value_coin":0.1,"value_satoshi":10000000},{"n":1,"scriptPubKey":{"addresses":["bchtest:qzx0llpyp8gxxsmad25twksqnwd62xm3lsnnczzt66"],"asm":"OP_DUP OP_HASH160 OP_PUSHBYTES_20 8cfffc2409d063437d6aa8b75a009b9ba51b71fc OP_EQUALVERIFY OP_CHECKSIG","hex":"76a9148cfffc2409d063437d6aa8b75a009b9ba51b71fc88ac","type":"pubkeyhash"},"value_coin":0.11406782,"value_satoshi":11406782}]}"#;
    let _: RpcTransaction = json::from_str(verbose).expect("!json::from_str");
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_withdraw_to_p2pkh() {
    UtxoStandardCoin::get_unspent_ordered_list.mock_safe(|coin, _| {
        let cache = block_on(coin.as_ref().recently_spent_outpoints.lock());
        let unspents = vec![UnspentInfo {
            outpoint: OutPoint {
                hash: 1.into(),
                index: 0,
            },
            value: 1000000000,
            height: Default::default(),
        }];
        MockResult::Return(Box::pin(futures::future::ok((unspents, cache))))
    });

    let client = NativeClient(Arc::new(NativeClientImpl::default()));

    let coin = utxo_coin_for_test(UtxoRpcClientEnum::Native(client.clone()), None, false);

    // Create a p2pkh address for the test coin
    let p2pkh_address = Address {
        prefix: coin.as_ref().conf.pub_addr_prefix,
        hash: coin.as_ref().derivation_method.unwrap_iguana().hash.clone(),
        t_addr_prefix: coin.as_ref().conf.pub_t_addr_prefix,
        checksum_type: coin.as_ref().derivation_method.unwrap_iguana().checksum_type,
        hrp: coin.as_ref().conf.bech32_hrp.clone(),
        addr_format: UtxoAddressFormat::Standard,
    };

    let withdraw_req = WithdrawRequest {
        amount: 1.into(),
        from: None,
        to: p2pkh_address.to_string(),
        coin: TEST_COIN_NAME.into(),
        max: false,
        fee: None,
    };
    let tx_details = coin.withdraw(withdraw_req).wait().unwrap();
    let transaction: UtxoTx = deserialize(tx_details.tx_hex.as_slice()).unwrap();
    let output_script: Script = transaction.outputs[0].script_pubkey.clone().into();

    let expected_script = Builder::build_p2pkh(&p2pkh_address.hash);

    assert_eq!(output_script, expected_script);
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_withdraw_to_p2sh() {
    UtxoStandardCoin::get_unspent_ordered_list.mock_safe(|coin, _| {
        let cache = block_on(coin.as_ref().recently_spent_outpoints.lock());
        let unspents = vec![UnspentInfo {
            outpoint: OutPoint {
                hash: 1.into(),
                index: 0,
            },
            value: 1000000000,
            height: Default::default(),
        }];
        MockResult::Return(Box::pin(futures::future::ok((unspents, cache))))
    });

    let client = NativeClient(Arc::new(NativeClientImpl::default()));

    let coin = utxo_coin_for_test(UtxoRpcClientEnum::Native(client.clone()), None, false);

    // Create a p2sh address for the test coin
    let p2sh_address = Address {
        prefix: coin.as_ref().conf.p2sh_addr_prefix,
        hash: coin.as_ref().derivation_method.unwrap_iguana().hash.clone(),
        t_addr_prefix: coin.as_ref().conf.p2sh_t_addr_prefix,
        checksum_type: coin.as_ref().derivation_method.unwrap_iguana().checksum_type,
        hrp: coin.as_ref().conf.bech32_hrp.clone(),
        addr_format: UtxoAddressFormat::Standard,
    };

    let withdraw_req = WithdrawRequest {
        amount: 1.into(),
        from: None,
        to: p2sh_address.to_string(),
        coin: TEST_COIN_NAME.into(),
        max: false,
        fee: None,
    };
    let tx_details = coin.withdraw(withdraw_req).wait().unwrap();
    let transaction: UtxoTx = deserialize(tx_details.tx_hex.as_slice()).unwrap();
    let output_script: Script = transaction.outputs[0].script_pubkey.clone().into();

    let expected_script = Builder::build_p2sh(&p2sh_address.hash);

    assert_eq!(output_script, expected_script);
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_withdraw_to_p2wpkh() {
    UtxoStandardCoin::get_unspent_ordered_list.mock_safe(|coin, _| {
        let cache = block_on(coin.as_ref().recently_spent_outpoints.lock());
        let unspents = vec![UnspentInfo {
            outpoint: OutPoint {
                hash: 1.into(),
                index: 0,
            },
            value: 1000000000,
            height: Default::default(),
        }];
        MockResult::Return(Box::pin(futures::future::ok((unspents, cache))))
    });

    let client = NativeClient(Arc::new(NativeClientImpl::default()));

    let coin = utxo_coin_for_test(UtxoRpcClientEnum::Native(client.clone()), None, true);

    // Create a p2wpkh address for the test coin
    let p2wpkh_address = Address {
        prefix: coin.as_ref().conf.pub_addr_prefix,
        hash: coin.as_ref().derivation_method.unwrap_iguana().hash.clone(),
        t_addr_prefix: coin.as_ref().conf.pub_t_addr_prefix,
        checksum_type: coin.as_ref().derivation_method.unwrap_iguana().checksum_type,
        hrp: coin.as_ref().conf.bech32_hrp.clone(),
        addr_format: UtxoAddressFormat::Segwit,
    };

    let withdraw_req = WithdrawRequest {
        amount: 1.into(),
        from: None,
        to: p2wpkh_address.to_string(),
        coin: TEST_COIN_NAME.into(),
        max: false,
        fee: None,
    };
    let tx_details = coin.withdraw(withdraw_req).wait().unwrap();
    let transaction: UtxoTx = deserialize(tx_details.tx_hex.as_slice()).unwrap();
    let output_script: Script = transaction.outputs[0].script_pubkey.clone().into();

    let expected_script = Builder::build_witness_script(&p2wpkh_address.hash);

    assert_eq!(output_script, expected_script);
}

/// `UtxoStandardCoin` has to check UTXO maturity if `check_utxo_maturity` is `true`.
/// https://github.com/KomodoPlatform/atomicDEX-API/issues/1181
#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_utxo_standard_with_check_utxo_maturity_true() {
    /// Whether [`UtxoStandardCoin::get_mature_unspent_ordered_list`] is called or not.
    static mut GET_MATURE_UNSPENT_ORDERED_LIST_CALLED: bool = false;

    UtxoStandardCoin::get_mature_unspent_ordered_list.mock_safe(|coin, _| {
        unsafe { GET_MATURE_UNSPENT_ORDERED_LIST_CALLED = true };
        let cache = block_on(coin.as_ref().recently_spent_outpoints.lock());
        MockResult::Return(Box::pin(futures::future::ok((MatureUnspentList::default(), cache))))
    });

    let conf = json!({"coin":"DOC","asset":"DOC","rpcport":25435,"txversion":4,"overwintered":1,"mm2":1,"protocol":{"type":"UTXO"}});
    let req = json!({
         "method": "electrum",
         "servers": [
             {"url":"doc.electrum1.cipig.net:10020"},
             {"url":"doc.electrum2.cipig.net:10020"},
             {"url":"doc.electrum3.cipig.net:10020"},
         ],
        "check_utxo_maturity": true,
    });

    let ctx = MmCtxBuilder::new().into_mm_arc();
    let params = UtxoActivationParams::from_legacy_req(&req).unwrap();

    let coin = block_on(utxo_standard_coin_with_priv_key(
        &ctx, "DOC", &conf, &params, &[1u8; 32],
    ))
    .unwrap();

    let address = Address::from("R9o9xTocqr6CeEDGDH6mEYpwLoMz6jNjMW");
    // Don't use `block_on` here because it's used within a mock of [`GetUtxoListOps::get_mature_unspent_ordered_list`].
    coin.get_unspent_ordered_list(&address).compat().wait().unwrap();
    assert!(unsafe { GET_MATURE_UNSPENT_ORDERED_LIST_CALLED });
}

/// `UtxoStandardCoin` hasn't to check UTXO maturity if `check_utxo_maturity` is not set.
/// https://github.com/KomodoPlatform/atomicDEX-API/issues/1181
#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_utxo_standard_without_check_utxo_maturity() {
    /// Whether [`UtxoStandardCoin::get_all_unspent_ordered_list`] is called or not.
    static mut GET_ALL_UNSPENT_ORDERED_LIST_CALLED: bool = false;

    UtxoStandardCoin::get_all_unspent_ordered_list.mock_safe(|coin, _| {
        unsafe { GET_ALL_UNSPENT_ORDERED_LIST_CALLED = true };
        let cache = block_on(coin.as_ref().recently_spent_outpoints.lock());
        let unspents = Vec::new();
        MockResult::Return(Box::pin(futures::future::ok((unspents, cache))))
    });

    UtxoStandardCoin::get_mature_unspent_ordered_list.mock_safe(|_, _| {
        panic!("'UtxoStandardCoin::get_mature_unspent_ordered_list' is not expected to be called when `check_utxo_maturity` is not set")
    });

    let conf = json!({"coin":"DOC","asset":"DOC","rpcport":25435,"txversion":4,"overwintered":1,"mm2":1,"protocol":{"type":"UTXO"}});
    let req = json!({
         "method": "electrum",
         "servers": [
             {"url":"doc.electrum1.cipig.net:10020"},
             {"url":"doc.electrum2.cipig.net:10020"},
             {"url":"doc.electrum3.cipig.net:10020"},
         ]
    });

    let ctx = MmCtxBuilder::new().into_mm_arc();
    let params = UtxoActivationParams::from_legacy_req(&req).unwrap();

    let coin = block_on(utxo_standard_coin_with_priv_key(
        &ctx, "DOC", &conf, &params, &[1u8; 32],
    ))
    .unwrap();

    let address = Address::from("R9o9xTocqr6CeEDGDH6mEYpwLoMz6jNjMW");
    // Don't use `block_on` here because it's used within a mock of [`UtxoStandardCoin::get_all_unspent_ordered_list`].
    coin.get_unspent_ordered_list(&address).compat().wait().unwrap();
    assert!(unsafe { GET_ALL_UNSPENT_ORDERED_LIST_CALLED });
}

/// `QtumCoin` has to check UTXO maturity if `check_utxo_maturity` is not set.
/// https://github.com/KomodoPlatform/atomicDEX-API/issues/1181
#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_qtum_without_check_utxo_maturity() {
    /// Whether [`QtumCoin::get_mature_unspent_ordered_list`] is called or not.
    static mut GET_MATURE_UNSPENT_ORDERED_LIST_CALLED: bool = false;

    QtumCoin::get_mature_unspent_ordered_list.mock_safe(|coin, _| {
        unsafe { GET_MATURE_UNSPENT_ORDERED_LIST_CALLED = true };
        let cache = block_on(coin.as_ref().recently_spent_outpoints.lock());
        MockResult::Return(Box::pin(futures::future::ok((MatureUnspentList::default(), cache))))
    });

    let conf = json!({"coin":"tQTUM","rpcport":13889,"pubtype":120,"p2shtype":110});
    let req = json!({
        "method": "electrum",
        "servers": tqtum_test_servers(),
    });

    let ctx = MmCtxBuilder::new().into_mm_arc();
    let params = UtxoActivationParams::from_legacy_req(&req).unwrap();

    let coin = block_on(qtum_coin_with_priv_key(&ctx, "QTUM", &conf, &params, &[1u8; 32])).unwrap();

    let address = Address::from("qcyBHeSct7Wr4mAw18iuQ1zW5mMFYmtmBE");
    // Don't use `block_on` here because it's used within a mock of [`QtumCoin::get_mature_unspent_ordered_list`].
    coin.get_unspent_ordered_list(&address).compat().wait().unwrap();
    assert!(unsafe { GET_MATURE_UNSPENT_ORDERED_LIST_CALLED });
}

/// `QtumCoin` hasn't to check UTXO maturity if `check_utxo_maturity` is `false`.
/// https://github.com/KomodoPlatform/atomicDEX-API/issues/1181
#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_qtum_with_check_utxo_maturity_false() {
    /// Whether [`QtumCoin::get_all_unspent_ordered_list`] is called or not.
    static mut GET_ALL_UNSPENT_ORDERED_LIST_CALLED: bool = false;

    QtumCoin::get_all_unspent_ordered_list.mock_safe(|coin, _address| {
        unsafe { GET_ALL_UNSPENT_ORDERED_LIST_CALLED = true };
        let cache = block_on(coin.as_ref().recently_spent_outpoints.lock());
        let unspents = Vec::new();
        MockResult::Return(Box::pin(futures::future::ok((unspents, cache))))
    });
    QtumCoin::get_mature_unspent_ordered_list.mock_safe(|_, _| {
        panic!(
            "'QtumCoin::get_mature_unspent_ordered_list' is not expected to be called when `check_utxo_maturity` is false"
        )
    });

    let conf = json!({"coin":"tQTUM","rpcport":13889,"pubtype":120,"p2shtype":110});
    let req = json!({
        "method": "electrum",
        "servers": tqtum_test_servers(),
        "check_utxo_maturity": false,
    });

    let ctx = MmCtxBuilder::new().into_mm_arc();
    let params = UtxoActivationParams::from_legacy_req(&req).unwrap();

    let coin = block_on(qtum_coin_with_priv_key(&ctx, "QTUM", &conf, &params, &[1u8; 32])).unwrap();

    let address = Address::from("qcyBHeSct7Wr4mAw18iuQ1zW5mMFYmtmBE");
    // Don't use `block_on` here because it's used within a mock of [`QtumCoin::get_all_unspent_ordered_list`].
    coin.get_unspent_ordered_list(&address).compat().wait().unwrap();
    assert!(unsafe { GET_ALL_UNSPENT_ORDERED_LIST_CALLED });
}

#[test]
fn test_minimum_external_addresses_are_activated_once() {
    let storage_updates = Arc::new(AtomicUsize::new(0));
    let storage_updates_mock = storage_updates.clone();
    HDWalletMockStorage::update_external_addresses_number.mock_safe(
        move |_, _, account_id, new_external_addresses_number| {
            assert_eq!(account_id, 0);
            assert_eq!(new_external_addresses_number, 1);
            storage_updates_mock.fetch_add(1, Ordering::SeqCst);
            MockResult::Return(Box::pin(futures::future::ok(())))
        },
    );

    let balance_requests = Arc::new(AtomicUsize::new(0));
    let balance_requests_mock = balance_requests.clone();
    NativeClient::display_balances.mock_safe(move |_, addresses: Vec<Address>, _| {
        assert_eq!(addresses.len(), 1);
        assert_eq!(addresses[0].to_string(), "RRqF4cYniMwYs66S4QDUUZ4GJQFQF69rBE");
        balance_requests_mock.fetch_add(1, Ordering::SeqCst);
        let balances = addresses
            .into_iter()
            .map(|address| (address, BigDecimal::from(0)))
            .collect();
        MockResult::Return(Box::new(futures01::future::ok(balances)))
    });

    let client = NativeClient(Arc::new(NativeClientImpl::default()));
    let coin = utxo_coin_from_fields(utxo_coin_fields_for_test(
        UtxoRpcClientEnum::Native(client),
        None,
        false,
    ));
    let hd_wallet = UtxoHDWallet {
        hd_wallet_storage: HDWalletCoinStorage::default(),
        address_format: UtxoAddressFormat::Standard,
        derivation_path: HDPathToCoin::from_str("m/44'/141'").unwrap(),
        accounts: HDAccountsMutex::new(HDAccountsMap::new()),
        gap_limit: 20,
    };
    let mut hd_account = UtxoHDAccount {
        account_id: 0,
        extended_pubkey: Secp256k1ExtendedPublicKey::from_str(
            "xpub6DEHSksajpRPM59RPw7Eg6PKdU7E2ehxJWtYdrfQ6JFmMGBsrR6jA78ANCLgzKYm4s5UqQ4ydLEYPbh3TRVvn5oAZVtWfi4qJLMntpZ8uGJ",
        )
        .unwrap(),
        account_derivation_path: HDPathToAccount::from_str("m/44'/141'/0'").unwrap(),
        external_addresses_number: 0,
        internal_addresses_number: 0,
    };
    let mut addresses = Vec::new();

    block_on(crate::coin_balance::common_impl::ensure_minimum_addresses(
        &coin,
        &hd_wallet,
        &mut hd_account,
        Bip44Chain::External,
        0,
        &mut addresses,
    ))
    .unwrap();

    assert_eq!(hd_account.external_addresses_number, 0);
    assert!(addresses.is_empty());
    assert_eq!(storage_updates.load(Ordering::SeqCst), 0);
    assert_eq!(balance_requests.load(Ordering::SeqCst), 0);

    block_on(crate::coin_balance::common_impl::ensure_minimum_addresses(
        &coin,
        &hd_wallet,
        &mut hd_account,
        Bip44Chain::External,
        1,
        &mut addresses,
    ))
    .unwrap();

    assert_eq!(hd_account.external_addresses_number, 1);
    assert_eq!(addresses.len(), 1);
    assert_eq!(addresses[0].address, "RRqF4cYniMwYs66S4QDUUZ4GJQFQF69rBE");
    assert_eq!(
        addresses[0].derivation_path,
        RpcDerivationPath(DerivationPath::from_str("m/44'/141'/0'/0/0").unwrap())
    );
    assert_eq!(addresses[0].chain, Bip44Chain::External);
    assert_eq!(
        addresses[0].balance,
        crate::coin_balance::coin_balance_map_for_ticker(TEST_COIN_NAME, CoinBalance::default())
    );

    block_on(crate::coin_balance::common_impl::ensure_minimum_addresses(
        &coin,
        &hd_wallet,
        &mut hd_account,
        Bip44Chain::External,
        1,
        &mut addresses,
    ))
    .unwrap();

    assert_eq!(addresses.len(), 1);
    assert_eq!(storage_updates.load(Ordering::SeqCst), 1);
    assert_eq!(balance_requests.load(Ordering::SeqCst), 1);
}

#[test]
fn test_account_balance_rpc() {
    let mut addresses_map: HashMap<String, u64> = HashMap::new();
    let mut balances_by_der_path: HashMap<String, HDAddressBalance> = HashMap::new();

    macro_rules! known_address {
        ($der_path:literal, $address:literal, $chain:expr, balance = $balance:literal) => {
            addresses_map.insert($address.to_string(), $balance);
            balances_by_der_path.insert($der_path.to_string(), HDAddressBalance {
                address: $address.to_string(),
                derivation_path: RpcDerivationPath(DerivationPath::from_str($der_path).unwrap()),
                chain: $chain,
                balance: crate::coin_balance::coin_balance_map_for_ticker(
                    TEST_COIN_NAME,
                    CoinBalance::new(BigDecimal::from($balance)),
                ),
            })
        };
    }

    macro_rules! get_balances {
        ($($der_paths:literal),*) => {
            [$($der_paths),*].iter().map(|der_path| balances_by_der_path.get(*der_path).unwrap().clone()).collect()
        };
    }

    // rustfmt::skip - formatting intentional
    {
        // Account#0, external addresses.
        known_address!(
            "m/44'/141'/0'/0/0",
            "RRqF4cYniMwYs66S4QDUUZ4GJQFQF69rBE",
            Bip44Chain::External,
            balance = 0
        );
        known_address!(
            "m/44'/141'/0'/0/1",
            "RSVLsjXc9LJ8fm9Jq7gXjeubfja3bbgSDf",
            Bip44Chain::External,
            balance = 0
        );
        known_address!(
            "m/44'/141'/0'/0/2",
            "RSSZjtgfnLzvqF4cZQJJEpN5gvK3pWmd3h",
            Bip44Chain::External,
            balance = 0
        );
        known_address!(
            "m/44'/141'/0'/0/3",
            "RU1gRFXWXNx7uPRAEJ7wdZAW1RZ4TE6Vv1",
            Bip44Chain::External,
            balance = 98
        );
        known_address!(
            "m/44'/141'/0'/0/4",
            "RUkEvRzb7mtwfVeKiSFEbYupLkcvU5KJBw",
            Bip44Chain::External,
            balance = 1
        );
        known_address!(
            "m/44'/141'/0'/0/5",
            "RP8deqVfjBbkvxbGbsQ2EGdamMaP1wxizR",
            Bip44Chain::External,
            balance = 0
        );
        known_address!(
            "m/44'/141'/0'/0/6",
            "RSvKMMegKGP5e2EanH7fnD4yNsxdJvLAmL",
            Bip44Chain::External,
            balance = 32
        );

        // Account#0, internal addresses.
        known_address!(
            "m/44'/141'/0'/1/0",
            "RLZxcZSYtKe74JZd1hBAmmD9PNHZqb72oL",
            Bip44Chain::Internal,
            balance = 13
        );
        known_address!(
            "m/44'/141'/0'/1/1",
            "RPj9JXUVnewWwVpxZDeqGB25qVqz5qJzwP",
            Bip44Chain::Internal,
            balance = 44
        );
        known_address!(
            "m/44'/141'/0'/1/2",
            "RSYdSLRYWuzBson2GDbWBa632q2PmFnCaH",
            Bip44Chain::Internal,
            balance = 10
        );

        // Account#1, internal addresses.
        known_address!(
            "m/44'/141'/1'/1/0",
            "RGo7sYzivPtzv8aRQ4A6vRJDxoqkRRBRhZ",
            Bip44Chain::Internal,
            balance = 0
        );
    }

    NativeClient::display_balances.mock_safe(move |_, addresses: Vec<Address>, _| {
        let result: Vec<_> = addresses
            .into_iter()
            .map(|address| {
                let address_str = address.to_string();
                let balance = addresses_map
                    .remove(&address_str)
                    .expect(&format!("Unexpected address: {}", address_str));
                (address, BigDecimal::from(balance))
            })
            .collect();
        MockResult::Return(Box::new(futures01::future::ok(result)))
    });

    let client = NativeClient(Arc::new(NativeClientImpl::default()));
    let mut fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(client), None, false);
    let mut hd_accounts = HDAccountsMap::new();
    hd_accounts.insert(0, UtxoHDAccount {
        account_id: 0,
        extended_pubkey: Secp256k1ExtendedPublicKey::from_str("xpub6DEHSksajpRPM59RPw7Eg6PKdU7E2ehxJWtYdrfQ6JFmMGBsrR6jA78ANCLgzKYm4s5UqQ4ydLEYPbh3TRVvn5oAZVtWfi4qJLMntpZ8uGJ").unwrap(),
        account_derivation_path: HDPathToAccount::from_str("m/44'/141'/0'").unwrap(),
        external_addresses_number: 7,
        internal_addresses_number: 3,
    });
    hd_accounts.insert(1, UtxoHDAccount {
        account_id: 1,
        extended_pubkey: Secp256k1ExtendedPublicKey::from_str("xpub6DEHSksajpRPQq2FdGT6JoieiQZUpTZ3WZn8fcuLJhFVmtCpXbuXxp5aPzaokwcLV2V9LE55Dwt8JYkpuMv7jXKwmyD28WbHYjBH2zhbW2p").unwrap(),
        account_derivation_path: HDPathToAccount::from_str("m/44'/141'/1'").unwrap(),
        external_addresses_number: 0,
        internal_addresses_number: 1,
    });
    fields.derivation_method = DerivationMethod::HDWallet(UtxoHDWallet {
        hd_wallet_storage: HDWalletCoinStorage::default(),
        address_format: UtxoAddressFormat::Standard,
        derivation_path: HDPathToCoin::from_str("m/44'/141'").unwrap(),
        accounts: HDAccountsMutex::new(hd_accounts),
        gap_limit: 3,
    });
    let coin = utxo_coin_from_fields(fields);

    // Request a balance of Account#0, external addresses, 1st page

    let params = AccountBalanceParams {
        account_index: 0,
        chain: Bip44Chain::External,
        limit: 3,
        paging_options: PagingOptionsEnum::PageNumber(NonZeroUsize::new(1).unwrap()),
    };
    let actual = block_on(coin.account_balance_rpc(params)).expect("!account_balance_rpc");
    let expected = HDAccountBalanceResponse {
        account_index: 0,
        derivation_path: DerivationPath::from_str("m/44'/141'/0'").unwrap().into(),
        addresses: get_balances!("m/44'/141'/0'/0/0", "m/44'/141'/0'/0/1", "m/44'/141'/0'/0/2"),
        page_balance: CoinBalance::new(BigDecimal::from(0)),
        limit: 3,
        skipped: 0,
        total: 7,
        total_pages: 3,
        paging_options: PagingOptionsEnum::PageNumber(NonZeroUsize::new(1).unwrap()),
    };
    assert_eq!(actual, expected);

    // Request a balance of Account#0, external addresses, 2nd page

    let params = AccountBalanceParams {
        account_index: 0,
        chain: Bip44Chain::External,
        limit: 3,
        paging_options: PagingOptionsEnum::PageNumber(NonZeroUsize::new(2).unwrap()),
    };
    let actual = block_on(coin.account_balance_rpc(params)).expect("!account_balance_rpc");
    let expected = HDAccountBalanceResponse {
        account_index: 0,
        derivation_path: DerivationPath::from_str("m/44'/141'/0'").unwrap().into(),
        addresses: get_balances!("m/44'/141'/0'/0/3", "m/44'/141'/0'/0/4", "m/44'/141'/0'/0/5"),
        page_balance: CoinBalance::new(BigDecimal::from(99)),
        limit: 3,
        skipped: 3,
        total: 7,
        total_pages: 3,
        paging_options: PagingOptionsEnum::PageNumber(NonZeroUsize::new(2).unwrap()),
    };
    assert_eq!(actual, expected);

    // Request a balance of Account#0, external addresses, 3rd page

    let params = AccountBalanceParams {
        account_index: 0,
        chain: Bip44Chain::External,
        limit: 3,
        paging_options: PagingOptionsEnum::PageNumber(NonZeroUsize::new(3).unwrap()),
    };
    let actual = block_on(coin.account_balance_rpc(params)).expect("!account_balance_rpc");
    let expected = HDAccountBalanceResponse {
        account_index: 0,
        derivation_path: DerivationPath::from_str("m/44'/141'/0'").unwrap().into(),
        addresses: get_balances!("m/44'/141'/0'/0/6"),
        page_balance: CoinBalance::new(BigDecimal::from(32)),
        limit: 3,
        skipped: 6,
        total: 7,
        total_pages: 3,
        paging_options: PagingOptionsEnum::PageNumber(NonZeroUsize::new(3).unwrap()),
    };
    assert_eq!(actual, expected);

    // Request a balance of Account#0, external addresses, page 4 (out of bound)

    let params = AccountBalanceParams {
        account_index: 0,
        chain: Bip44Chain::External,
        limit: 3,
        paging_options: PagingOptionsEnum::PageNumber(NonZeroUsize::new(4).unwrap()),
    };
    let actual = block_on(coin.account_balance_rpc(params)).expect("!account_balance_rpc");
    let expected = HDAccountBalanceResponse {
        account_index: 0,
        derivation_path: DerivationPath::from_str("m/44'/141'/0'").unwrap().into(),
        addresses: Vec::new(),
        page_balance: CoinBalance::default(),
        limit: 3,
        skipped: 7,
        total: 7,
        total_pages: 3,
        paging_options: PagingOptionsEnum::PageNumber(NonZeroUsize::new(4).unwrap()),
    };
    assert_eq!(actual, expected);

    // Request a balance of Account#0, internal addresses, where idx > 0

    let params = AccountBalanceParams {
        account_index: 0,
        chain: Bip44Chain::Internal,
        limit: 3,
        paging_options: PagingOptionsEnum::FromId(0),
    };
    let actual = block_on(coin.account_balance_rpc(params)).expect("!account_balance_rpc");
    let expected = HDAccountBalanceResponse {
        account_index: 0,
        derivation_path: DerivationPath::from_str("m/44'/141'/0'").unwrap().into(),
        addresses: get_balances!("m/44'/141'/0'/1/1", "m/44'/141'/0'/1/2"),
        page_balance: CoinBalance::new(BigDecimal::from(54)),
        limit: 3,
        skipped: 1,
        total: 3,
        total_pages: 1,
        paging_options: PagingOptionsEnum::FromId(0),
    };
    assert_eq!(actual, expected);

    // Request a balance of Account#1, external addresses, page 1 (out of bound)

    let params = AccountBalanceParams {
        account_index: 1,
        chain: Bip44Chain::External,
        limit: 3,
        paging_options: PagingOptionsEnum::PageNumber(NonZeroUsize::new(1).unwrap()),
    };
    let actual = block_on(coin.account_balance_rpc(params)).expect("!account_balance_rpc");
    let expected = HDAccountBalanceResponse {
        account_index: 1,
        derivation_path: DerivationPath::from_str("m/44'/141'/1'").unwrap().into(),
        addresses: Vec::new(),
        page_balance: CoinBalance::default(),
        limit: 3,
        skipped: 0,
        total: 0,
        total_pages: 0,
        paging_options: PagingOptionsEnum::PageNumber(NonZeroUsize::new(1).unwrap()),
    };
    assert_eq!(actual, expected);

    // Request a balance of Account#1, external addresses, page 1

    let params = AccountBalanceParams {
        account_index: 1,
        chain: Bip44Chain::Internal,
        limit: 3,
        paging_options: PagingOptionsEnum::PageNumber(NonZeroUsize::new(1).unwrap()),
    };
    let actual = block_on(coin.account_balance_rpc(params)).expect("!account_balance_rpc");
    let expected = HDAccountBalanceResponse {
        account_index: 1,
        derivation_path: DerivationPath::from_str("m/44'/141'/1'").unwrap().into(),
        addresses: get_balances!("m/44'/141'/1'/1/0"),
        page_balance: CoinBalance::new(BigDecimal::from(0)),
        limit: 3,
        skipped: 0,
        total: 1,
        total_pages: 1,
        paging_options: PagingOptionsEnum::PageNumber(NonZeroUsize::new(1).unwrap()),
    };
    assert_eq!(actual, expected);

    // Request a balance of Account#1, external addresses, where idx > 0 (out of bound)

    let params = AccountBalanceParams {
        account_index: 1,
        chain: Bip44Chain::Internal,
        limit: 3,
        paging_options: PagingOptionsEnum::FromId(0),
    };
    let actual = block_on(coin.account_balance_rpc(params)).expect("!account_balance_rpc");
    let expected = HDAccountBalanceResponse {
        account_index: 1,
        derivation_path: DerivationPath::from_str("m/44'/141'/1'").unwrap().into(),
        addresses: Vec::new(),
        page_balance: CoinBalance::default(),
        limit: 3,
        skipped: 1,
        total: 1,
        total_pages: 1,
        paging_options: PagingOptionsEnum::FromId(0),
    };
    assert_eq!(actual, expected);
}

#[test]
fn test_scan_for_new_addresses() {
    static mut ACCOUNT_ID: u32 = 0;
    static mut NEW_EXTERNAL_ADDRESSES_NUMBER: u32 = 0;
    static mut NEW_INTERNAL_ADDRESSES_NUMBER: u32 = 0;

    HDWalletMockStorage::update_external_addresses_number.mock_safe(
        |_, _, account_id, new_external_addresses_number| {
            assert_eq!(account_id, unsafe { ACCOUNT_ID });
            assert_eq!(new_external_addresses_number, unsafe { NEW_EXTERNAL_ADDRESSES_NUMBER });
            MockResult::Return(Box::pin(futures::future::ok(())))
        },
    );

    HDWalletMockStorage::update_internal_addresses_number.mock_safe(
        |_, _, account_id, new_internal_addresses_number| {
            assert_eq!(account_id, unsafe { ACCOUNT_ID });
            assert_eq!(new_internal_addresses_number, unsafe { NEW_INTERNAL_ADDRESSES_NUMBER });
            MockResult::Return(Box::pin(futures::future::ok(())))
        },
    );

    let mut checking_addresses: HashMap<String, Option<u64>> = HashMap::new();
    let mut non_empty_addresses: Vec<String> = Vec::new();
    let mut balances_by_der_path: HashMap<String, HDAddressBalance> = HashMap::new();

    macro_rules! new_address {
        ($der_path:literal, $address:literal, $chain:expr, balance = $balance:expr) => {{
            let balance = $balance;
            checking_addresses.insert($address.to_string(), balance);
            balances_by_der_path.insert($der_path.to_string(), HDAddressBalance {
                address: $address.to_string(),
                derivation_path: RpcDerivationPath(DerivationPath::from_str($der_path).unwrap()),
                chain: $chain,
                balance: crate::coin_balance::coin_balance_map_for_ticker(
                    TEST_COIN_NAME,
                    CoinBalance::new(BigDecimal::from(balance.unwrap_or(0))),
                ),
            });
            if balance.is_some() {
                non_empty_addresses.push($address.to_string());
            }
        }};
    }

    macro_rules! unused_address {
        ($_der_path:literal, $address:literal) => {
            checking_addresses.insert($address.to_string(), None)
        };
    }

    macro_rules! get_balances {
        ($($der_paths:literal),*) => {
            [$($der_paths),*].iter().map(|der_path| balances_by_der_path.get(*der_path).unwrap().clone()).collect()
        };
    }

    // rustfmt::skip - formatting intentional
    {
        // Account#0, external addresses.
        new_address!(
            "m/44'/141'/0'/0/3",
            "RU1gRFXWXNx7uPRAEJ7wdZAW1RZ4TE6Vv1",
            Bip44Chain::External,
            balance = Some(98)
        );
        unused_address!("m/44'/141'/0'/0/4", "RUkEvRzb7mtwfVeKiSFEbYupLkcvU5KJBw");
        unused_address!("m/44'/141'/0'/0/5", "RP8deqVfjBbkvxbGbsQ2EGdamMaP1wxizR");
        unused_address!("m/44'/141'/0'/0/6", "RSvKMMegKGP5e2EanH7fnD4yNsxdJvLAmL"); // Stop searching for a non-empty address (gap_limit = 3).

        // Account#0, internal addresses.
        new_address!(
            "m/44'/141'/0'/1/1",
            "RPj9JXUVnewWwVpxZDeqGB25qVqz5qJzwP",
            Bip44Chain::Internal,
            balance = Some(98)
        );
        new_address!(
            "m/44'/141'/0'/1/2",
            "RSYdSLRYWuzBson2GDbWBa632q2PmFnCaH",
            Bip44Chain::Internal,
            balance = None
        );
        new_address!(
            "m/44'/141'/0'/1/3",
            "RQstQeTUEZLh6c3YWJDkeVTTQoZUsfvNCr",
            Bip44Chain::Internal,
            balance = Some(14)
        );
        unused_address!("m/44'/141'/0'/1/4", "RT54m6pfj9scqwSLmYdfbmPcrpxnWGAe9J");
        unused_address!("m/44'/141'/0'/1/5", "RYWfEFxqA6zya9c891Dj7vxiDojCmuWR9T");
        unused_address!("m/44'/141'/0'/1/6", "RSkY6twW8knTcn6wGACUAG9crJHcuQ2kEH"); // Stop searching for a non-empty address (gap_limit = 3).

        // Account#1, external addresses.
        new_address!(
            "m/44'/141'/1'/0/0",
            "RBQFLwJ88gVcnfkYvJETeTAB6AAYLow12K",
            Bip44Chain::External,
            balance = Some(9)
        );
        new_address!(
            "m/44'/141'/1'/0/1",
            "RCyy77sRWFa2oiFPpyimeTQfenM1aRoiZs",
            Bip44Chain::External,
            balance = Some(7)
        );
        new_address!(
            "m/44'/141'/1'/0/2",
            "RDnNa3pQmisfi42KiTZrfYfuxkLC91PoTJ",
            Bip44Chain::External,
            balance = None
        );
        new_address!(
            "m/44'/141'/1'/0/3",
            "RQRGgXcGJz93CoAfQJoLgBz2r9HtJYMX3Z",
            Bip44Chain::External,
            balance = None
        );
        new_address!(
            "m/44'/141'/1'/0/4",
            "RM6cqSFCFZ4J1LngLzqKkwo2ouipbDZUbm",
            Bip44Chain::External,
            balance = Some(11)
        );
        unused_address!("m/44'/141'/1'/0/5", "RX2fGBZjNZMNdNcnc5QBRXvmsXTvadvTPN");
        unused_address!("m/44'/141'/1'/0/6", "RJJ7muUETyp59vxVXna9KAZ9uQ1QSqmcjE");
        unused_address!("m/44'/141'/1'/0/7", "RYJ6vbhxFre5yChCMiJJFNTTBhAQbKM9AY"); // Stop searching for a non-empty address (gap_limit = 3).

        // Account#1, internal addresses.
        unused_address!("m/44'/141'/1'/0/2", "RCjRDibDAXKYpVYSUeJXrbTzZ1UEKYAwJa");
        unused_address!("m/44'/141'/1'/0/3", "REs1NRzg8XjwN3v8Jp1wQUAyQb3TzeT8EB");
        unused_address!("m/44'/141'/1'/0/4", "RS4UZtkwZ8eYaTL1xodXgFNryJoTbPJYE5");
        // Stop searching for a non-empty address (gap_limit = 3).
    }

    NativeClient::display_balance.mock_safe(move |_, address: Address, _| {
        let address = address.to_string();
        let balance = checking_addresses
            .remove(&address)
            .expect(&format!("Unexpected address: {}", address))
            .expect(&format!(
                "'{}' address is empty. 'NativeClient::display_balance' must not be called for this address",
                address
            ));
        MockResult::Return(Box::new(futures01::future::ok(BigDecimal::from(balance))))
    });

    NativeClient::list_all_transactions.mock_safe(move |_, _| {
        let tx_history = non_empty_addresses
            .clone()
            .into_iter()
            .map(|address| ListTransactionsItem {
                address,
                ..ListTransactionsItem::default()
            })
            .collect();
        MockResult::Return(Box::new(futures01::future::ok(tx_history)))
    });

    let client = NativeClient(Arc::new(NativeClientImpl::default()));
    let mut fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(client), None, false);
    let mut hd_accounts = HDAccountsMap::new();
    hd_accounts.insert(0, UtxoHDAccount {
        account_id: 0,
        extended_pubkey: Secp256k1ExtendedPublicKey::from_str("xpub6DEHSksajpRPM59RPw7Eg6PKdU7E2ehxJWtYdrfQ6JFmMGBsrR6jA78ANCLgzKYm4s5UqQ4ydLEYPbh3TRVvn5oAZVtWfi4qJLMntpZ8uGJ").unwrap(),
        account_derivation_path: HDPathToAccount::from_str("m/44'/141'/0'").unwrap(),
        external_addresses_number: 3,
        internal_addresses_number: 1,
    });
    hd_accounts.insert(1, UtxoHDAccount {
        account_id: 1,
        extended_pubkey: Secp256k1ExtendedPublicKey::from_str("xpub6DEHSksajpRPQq2FdGT6JoieiQZUpTZ3WZn8fcuLJhFVmtCpXbuXxp5aPzaokwcLV2V9LE55Dwt8JYkpuMv7jXKwmyD28WbHYjBH2zhbW2p").unwrap(),
        account_derivation_path: HDPathToAccount::from_str("m/44'/141'/1'").unwrap(),
        external_addresses_number: 0,
        internal_addresses_number: 2,
    });
    fields.derivation_method = DerivationMethod::HDWallet(UtxoHDWallet {
        hd_wallet_storage: HDWalletCoinStorage::default(),
        address_format: UtxoAddressFormat::Standard,
        derivation_path: HDPathToCoin::from_str("m/44'/141'").unwrap(),
        accounts: HDAccountsMutex::new(hd_accounts),
        gap_limit: 3,
    });
    let coin = utxo_coin_from_fields(fields);

    // Check balance of Account#0

    unsafe {
        ACCOUNT_ID = 0;
        NEW_EXTERNAL_ADDRESSES_NUMBER = 4;
        NEW_INTERNAL_ADDRESSES_NUMBER = 4;
    }

    let params = ScanAddressesParams {
        account_index: 0,
        gap_limit: Some(3),
    };
    let actual = block_on(coin.init_scan_for_new_addresses_rpc(params)).expect("!account_balance_rpc");
    let expected = ScanAddressesResponse {
        account_index: 0,
        derivation_path: DerivationPath::from_str("m/44'/141'/0'").unwrap().into(),
        new_addresses: get_balances!(
            "m/44'/141'/0'/0/3",
            "m/44'/141'/0'/1/1",
            "m/44'/141'/0'/1/2",
            "m/44'/141'/0'/1/3"
        ),
    };
    assert_eq!(actual, expected);

    // Check balance of Account#1

    unsafe {
        ACCOUNT_ID = 1;
        NEW_EXTERNAL_ADDRESSES_NUMBER = 5;
        NEW_INTERNAL_ADDRESSES_NUMBER = 2;
    }

    let params = ScanAddressesParams {
        account_index: 1,
        gap_limit: None,
    };
    let actual = block_on(coin.init_scan_for_new_addresses_rpc(params)).expect("!account_balance_rpc");
    let expected = ScanAddressesResponse {
        account_index: 1,
        derivation_path: DerivationPath::from_str("m/44'/141'/1'").unwrap().into(),
        new_addresses: get_balances!(
            "m/44'/141'/1'/0/0",
            "m/44'/141'/1'/0/1",
            "m/44'/141'/1'/0/2",
            "m/44'/141'/1'/0/3",
            "m/44'/141'/1'/0/4"
        ),
    };
    assert_eq!(actual, expected);

    let accounts = match coin.as_ref().derivation_method {
        DerivationMethod::HDWallet(UtxoHDWallet { ref accounts, .. }) => block_on(accounts.lock()).clone(),
        _ => unreachable!(),
    };
    assert_eq!(accounts[&0].external_addresses_number, 4);
    assert_eq!(accounts[&0].internal_addresses_number, 4);
    assert_eq!(accounts[&1].external_addresses_number, 5);
    assert_eq!(accounts[&1].internal_addresses_number, 2);
}

/// https://github.com/KomodoPlatform/atomicDEX-API/issues/1196
#[test]
fn test_electrum_balance_deserializing() {
    let serialized = r#"{"confirmed": 988937858554305, "unconfirmed": 18446720562229577551}"#;
    let actual: ElectrumBalance = json::from_str(serialized).unwrap();
    assert_eq!(actual.confirmed, 988937858554305i128);
    assert_eq!(actual.unconfirmed, 18446720562229577551i128);

    let serialized = r#"{"confirmed": -170141183460469231731687303715884105728, "unconfirmed": 170141183460469231731687303715884105727}"#;
    let actual: ElectrumBalance = json::from_str(serialized).unwrap();
    assert_eq!(actual.confirmed, i128::MIN);
    assert_eq!(actual.unconfirmed, i128::MAX);
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_electrum_display_balances() {
    let rpc_client = electrum_client_for_test(DOC_ELECTRUM_ADDRS);
    block_on(utxo_common_tests::test_electrum_display_balances(&rpc_client));
}

#[test]
fn test_native_display_balances() {
    let unspents = vec![
        NativeUnspent {
            address: "RG278CfeNPFtNztFZQir8cgdWexVhViYVy".to_owned(),
            amount: "4.77699".into(),
            ..NativeUnspent::default()
        },
        NativeUnspent {
            address: "RJeDDtDRtKUoL8BCKdH7TNCHqUKr7kQRsi".to_owned(),
            amount: "0.77699".into(),
            ..NativeUnspent::default()
        },
        NativeUnspent {
            address: "RQHn9VPHBqNjYwyKfJbZCiaxVrWPKGQjeF".to_owned(),
            amount: "0.99998".into(),
            ..NativeUnspent::default()
        },
        NativeUnspent {
            address: "RG278CfeNPFtNztFZQir8cgdWexVhViYVy".to_owned(),
            amount: "1".into(),
            ..NativeUnspent::default()
        },
    ];

    NativeClient::list_unspent_impl
        .mock_safe(move |_, _, _, _| MockResult::Return(Box::new(futures01::future::ok(unspents.clone()))));

    let rpc_client = native_client_for_test();

    let addresses = vec![
        "RG278CfeNPFtNztFZQir8cgdWexVhViYVy".into(),
        "RYPz6Lr4muj4gcFzpMdv3ks1NCGn3mkDPN".into(),
        "RJeDDtDRtKUoL8BCKdH7TNCHqUKr7kQRsi".into(),
        "RQHn9VPHBqNjYwyKfJbZCiaxVrWPKGQjeF".into(),
    ];
    let actual = rpc_client
        .display_balances(addresses, TEST_COIN_DECIMALS)
        .wait()
        .unwrap();

    let expected: Vec<(Address, BigDecimal)> = vec![
        (
            "RG278CfeNPFtNztFZQir8cgdWexVhViYVy".into(),
            BigDecimal::try_from(5.77699).unwrap(),
        ),
        ("RYPz6Lr4muj4gcFzpMdv3ks1NCGn3mkDPN".into(), BigDecimal::from(0)),
        (
            "RJeDDtDRtKUoL8BCKdH7TNCHqUKr7kQRsi".into(),
            BigDecimal::try_from(0.77699).unwrap(),
        ),
        (
            "RQHn9VPHBqNjYwyKfJbZCiaxVrWPKGQjeF".into(),
            BigDecimal::try_from(0.99998).unwrap(),
        ),
    ];
    assert_eq!(actual, expected);
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_message_hash() {
    let client = electrum_client_for_test(DOC_ELECTRUM_ADDRS);
    let coin = utxo_coin_for_test(
        client.into(),
        Some("spice describe gravity federal blast come thank unfair canal monkey style afraid"),
        false,
    );
    let expected = H256::from_reversed_str("5aef9b67485adba55a2cd935269e73f2f9876382f1eada02418797ae76c07e18");
    let result = coin.sign_message_hash("test");
    assert!(result.is_some());
    assert_eq!(H256::from(result.unwrap()), expected);
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_sign_verify_message() {
    let client = electrum_client_for_test(DOC_ELECTRUM_ADDRS);
    let coin = utxo_coin_for_test(
        client.into(),
        Some("spice describe gravity federal blast come thank unfair canal monkey style afraid"),
        false,
    );

    let message = "test";
    let signature = coin.sign_message(message).unwrap();
    assert_eq!(
        signature,
        "HzetbqVj9gnUOznon9bvE61qRlmjH5R+rNgkxu8uyce3UBbOu+2aGh7r/GGSVFGZjRnaYC60hdwtdirTKLb7bE4="
    );

    let address = "R9o9xTocqr6CeEDGDH6mEYpwLoMz6jNjMW";
    let is_valid = coin.verify_message(&signature, message, address).unwrap();
    assert!(is_valid);
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_sign_verify_message_segwit() {
    let client = electrum_client_for_test(DOC_ELECTRUM_ADDRS);
    let coin = utxo_coin_for_test(
        client.into(),
        Some("spice describe gravity federal blast come thank unfair canal monkey style afraid"),
        true,
    );

    let message = "test";
    let signature = coin.sign_message(message).unwrap();
    assert_eq!(
        signature,
        "HzetbqVj9gnUOznon9bvE61qRlmjH5R+rNgkxu8uyce3UBbOu+2aGh7r/GGSVFGZjRnaYC60hdwtdirTKLb7bE4="
    );

    let is_valid = coin
        .verify_message(&signature, message, "rck1qqk4t2dppvmu9jja0z7nan0h464n5gve8h7nhay")
        .unwrap();
    assert!(is_valid);

    let is_valid = coin
        .verify_message(&signature, message, "R9o9xTocqr6CeEDGDH6mEYpwLoMz6jNjMW")
        .unwrap();
    assert!(is_valid);
}

// ─── V2 swap-protocol script-layout tests (chapter 15 §15.10 item 1) ───────
mod swap_proto_v2_script_tests {
    use crate::utxo::swap_proto_v2_scripts::{maker_payment_script, taker_funding_script, taker_payment_script};
    use keys::Public;
    use script::Opcode;

    // Compressed (33-byte) secp256k1 pubkeys, distinct & valid as data pushes.
    const TAKER_PUB_HEX: &str = "02031d4256c4bc9f99ac88bf3dba21773132281f65f9bf23a59928bce08961e2f3";
    const MAKER_PUB_HEX: &str = "03f8f8d75dbbbd3f7e1eedf7b2c3b2a5b59ed46e22b8eef0e2ef62e0c1aab9a3a3";
    // 32-byte sha256(secret) digests. Content is arbitrary; the script
    // applies ripemd160 internally.
    const TAKER_SECRET_HASH: [u8; 32] = [0xaa; 32];
    const MAKER_SECRET_HASH: [u8; 32] = [0xbb; 32];
    const LOCKTIME: u32 = 0x6800_0000; // future, fits in u32

    fn taker_pub() -> Public { Public::from_slice(&hex::decode(TAKER_PUB_HEX).unwrap()).unwrap() }
    fn maker_pub() -> Public { Public::from_slice(&hex::decode(MAKER_PUB_HEX).unwrap()).unwrap() }

    /// Collect every opcode in the script in order, ignoring push payloads.
    fn opcodes(script: &script::Script) -> Vec<Opcode> {
        script.iter().filter_map(|i| i.ok()).map(|i| i.opcode).collect()
    }

    #[test]
    fn should_build_taker_funding_script_with_expected_layout() {
        let s = taker_funding_script(LOCKTIME, &TAKER_SECRET_HASH, &taker_pub(), &maker_pub());
        let ops = opcodes(&s);

        // Layout: IF <push locktime> CLTV DROP <push pub> CHECKSIG
        //         ELSE
        //           IF <push pub> CHECKSIGVERIFY <push pub> CHECKSIG
        //           ELSE
        //             SIZE <push 32> EQUALVERIFY HASH160 <push r160> EQUALVERIFY <push pub> CHECKSIG
        //           ENDIF
        //         ENDIF
        let expected = [
            // IF branch (timelock refund)
            Opcode::OP_IF,
            Opcode::OP_PUSHBYTES_4, // 4-byte locktime little-endian
            Opcode::OP_CHECKLOCKTIMEVERIFY,
            Opcode::OP_DROP,
            Opcode::OP_PUSHBYTES_33, // taker pub
            Opcode::OP_CHECKSIG,
            // ELSE — outer
            Opcode::OP_ELSE,
            // inner IF (cooperative co-sig)
            Opcode::OP_IF,
            Opcode::OP_PUSHBYTES_33, // taker pub
            Opcode::OP_CHECKSIGVERIFY,
            Opcode::OP_PUSHBYTES_33, // maker pub
            Opcode::OP_CHECKSIG,
            // inner ELSE (secret-reveal refund)
            Opcode::OP_ELSE,
            Opcode::OP_SIZE,
            Opcode::OP_PUSHBYTES_1, // literal 32
            Opcode::OP_EQUALVERIFY,
            Opcode::OP_HASH160,
            Opcode::OP_PUSHBYTES_20, // r160(secret_hash)
            Opcode::OP_EQUALVERIFY,
            Opcode::OP_PUSHBYTES_33, // taker pub
            Opcode::OP_CHECKSIG,
            Opcode::OP_ENDIF,
            Opcode::OP_ENDIF,
        ];
        assert_eq!(ops, expected, "taker_funding_script layout mismatch");
    }

    #[test]
    fn should_build_taker_payment_script_with_expected_layout() {
        let s = taker_payment_script(LOCKTIME, &MAKER_SECRET_HASH, &taker_pub(), &maker_pub());
        let ops = opcodes(&s);
        let expected = [
            Opcode::OP_IF,
            Opcode::OP_PUSHBYTES_4,
            Opcode::OP_CHECKLOCKTIMEVERIFY,
            Opcode::OP_DROP,
            Opcode::OP_PUSHBYTES_33,
            Opcode::OP_CHECKSIG,
            Opcode::OP_ELSE,
            Opcode::OP_SIZE,
            Opcode::OP_PUSHBYTES_1,
            Opcode::OP_EQUALVERIFY,
            Opcode::OP_HASH160,
            Opcode::OP_PUSHBYTES_20,
            Opcode::OP_EQUALVERIFY,
            Opcode::OP_PUSHBYTES_33,
            Opcode::OP_CHECKSIGVERIFY,
            Opcode::OP_PUSHBYTES_33,
            Opcode::OP_CHECKSIG,
            Opcode::OP_ENDIF,
        ];
        assert_eq!(ops, expected, "taker_payment_script layout mismatch");
    }

    #[test]
    fn should_build_maker_payment_script_with_expected_layout() {
        let s = maker_payment_script(
            LOCKTIME,
            &MAKER_SECRET_HASH,
            &TAKER_SECRET_HASH,
            &maker_pub(),
            &taker_pub(),
        );
        let ops = opcodes(&s);
        // Layout: IF <push locktime> CLTV DROP <push maker_pub> CHECKSIG
        //         ELSE
        //           IF SIZE <32> EQUALVERIFY HASH160 <r160 maker_secret> EQUALVERIFY
        //              <push taker_pub> CHECKSIG
        //           ELSE
        //              SIZE <32> EQUALVERIFY HASH160 <r160 taker_secret> EQUALVERIFY
        //              <push maker_pub> CHECKSIG
        //           ENDIF
        //         ENDIF
        let expected = [
            Opcode::OP_IF,
            Opcode::OP_PUSHBYTES_4,
            Opcode::OP_CHECKLOCKTIMEVERIFY,
            Opcode::OP_DROP,
            Opcode::OP_PUSHBYTES_33,
            Opcode::OP_CHECKSIG,
            Opcode::OP_ELSE,
            Opcode::OP_IF,
            Opcode::OP_SIZE,
            Opcode::OP_PUSHBYTES_1,
            Opcode::OP_EQUALVERIFY,
            Opcode::OP_HASH160,
            Opcode::OP_PUSHBYTES_20,
            Opcode::OP_EQUALVERIFY,
            Opcode::OP_PUSHBYTES_33,
            Opcode::OP_CHECKSIG,
            Opcode::OP_ELSE,
            Opcode::OP_SIZE,
            Opcode::OP_PUSHBYTES_1,
            Opcode::OP_EQUALVERIFY,
            Opcode::OP_HASH160,
            Opcode::OP_PUSHBYTES_20,
            Opcode::OP_EQUALVERIFY,
            Opcode::OP_PUSHBYTES_33,
            Opcode::OP_CHECKSIG,
            Opcode::OP_ENDIF,
            Opcode::OP_ENDIF,
        ];
        assert_eq!(ops, expected, "maker_payment_script layout mismatch");
    }

    #[test]
    fn should_embed_ripemd160_of_provided_secret_hash() {
        use kdf_crypto::ripemd160;

        let s = taker_payment_script(LOCKTIME, &MAKER_SECRET_HASH, &taker_pub(), &maker_pub());
        let pushes: Vec<&[u8]> = s.iter().filter_map(|i| i.ok()).filter_map(|i| i.data).collect();
        let expected_r160 = ripemd160(&MAKER_SECRET_HASH);
        assert!(
            pushes.iter().any(|p| *p == expected_r160.as_slice()),
            "expected ripemd160(maker_secret_hash) as a push in taker_payment_script"
        );
    }
}

// ─── V2 maker-payment trait wiring tests (chapter 15 §15.4 IMPL block) ─────
mod swap_v2_maker_tests {
    use crate::utxo::swap_proto_v2_scripts::maker_payment_script;
    use crate::SwapTxTypeWithSecretHash;
    use keys::Public;
    use script::{Builder, Opcode, Script};

    const TAKER_PUB_HEX: &str = "02031d4256c4bc9f99ac88bf3dba21773132281f65f9bf23a59928bce08961e2f3";
    const MAKER_PUB_HEX: &str = "03f8f8d75dbbbd3f7e1eedf7b2c3b2a5b59ed46e22b8eef0e2ef62e0c1aab9a3a3";
    const TAKER_SECRET: [u8; 32] = [0x11; 32];
    const MAKER_SECRET: [u8; 32] = [0x22; 32];
    const TAKER_SECRET_HASH: [u8; 32] = [0xaa; 32];
    const MAKER_SECRET_HASH: [u8; 32] = [0xbb; 32];
    const LOCKTIME: u32 = 0x6800_0000;

    fn taker_pub() -> Public { Public::from_slice(&hex::decode(TAKER_PUB_HEX).unwrap()).unwrap() }
    fn maker_pub() -> Public { Public::from_slice(&hex::decode(MAKER_PUB_HEX).unwrap()).unwrap() }

    /// The dispatch table must produce the exact same bytes as the direct
    /// builder, so refund paths and validators agree on the redeem script.
    #[test]
    fn should_dispatch_redeem_script_for_maker_payment_v2_to_correct_builder() {
        let direct = maker_payment_script(
            LOCKTIME,
            &MAKER_SECRET_HASH,
            &TAKER_SECRET_HASH,
            &maker_pub(),
            &taker_pub(),
        );
        let via_dispatch = SwapTxTypeWithSecretHash::MakerPaymentV2 {
            maker_secret_hash: &MAKER_SECRET_HASH,
            taker_secret_hash: &TAKER_SECRET_HASH,
        }
        .redeem_script(LOCKTIME, &maker_pub(), &taker_pub());
        assert_eq!(direct.to_bytes(), via_dispatch.to_bytes());
    }

    /// §15.4.5 script_data must be `[push maker_secret, OP_1, OP_0]` so the
    /// outer-ELSE / inner-IF (taker-spends-with-maker-secret) branch is selected.
    #[test]
    fn should_select_secret_branch_for_spend_maker_payment_v2_script_data() {
        let script_data: Script = Builder::default()
            .push_data(&MAKER_SECRET)
            .push_opcode(Opcode::OP_1)
            .push_opcode(Opcode::OP_0)
            .into_script();
        let ops: Vec<Opcode> = script_data.iter().filter_map(|i| i.ok()).map(|i| i.opcode).collect();
        assert_eq!(ops, vec![Opcode::OP_PUSHBYTES_32, Opcode::OP_1, Opcode::OP_0]);
        let pushed = script_data
            .iter()
            .filter_map(|i| i.ok())
            .find_map(|i| i.data.map(|d| d.to_vec()))
            .expect("expected a single 32-byte push of the maker secret");
        assert_eq!(pushed.as_slice(), &MAKER_SECRET);
    }

    /// §15.4.4 script_data must be `[push taker_secret, OP_0, OP_0]` so the
    /// outer-ELSE / inner-ELSE (maker-refunds-with-taker-secret) branch is selected.
    #[test]
    fn should_select_taker_secret_branch_for_refund_maker_payment_v2_secret_script_data() {
        let script_data: Script = Builder::default()
            .push_data(&TAKER_SECRET)
            .push_opcode(Opcode::OP_0)
            .push_opcode(Opcode::OP_0)
            .into_script();
        let ops: Vec<Opcode> = script_data.iter().filter_map(|i| i.ok()).map(|i| i.opcode).collect();
        assert_eq!(ops, vec![Opcode::OP_PUSHBYTES_32, Opcode::OP_0, Opcode::OP_0]);
        let pushed = script_data
            .iter()
            .filter_map(|i| i.ok())
            .find_map(|i| i.data.map(|d| d.to_vec()))
            .expect("expected a single 32-byte push of the taker secret");
        assert_eq!(pushed.as_slice(), &TAKER_SECRET);
    }
}

// ─── V2 taker-funding trait wiring tests (chapter 15 §15.5 IMPL block 2B) ──
mod swap_v2_taker_funding_tests {
    use crate::utxo::swap_proto_v2_scripts::taker_funding_script;
    use crate::utxo::utxo_common::{classify_funding_spend_script_sig, extract_secret_v2, FundingSpendBranchTag};
    use crate::utxo::UtxoTx;
    use crate::SwapTxTypeWithSecretHash;
    use chain::{TransactionInput, TransactionOutput};
    use kdf_crypto::dhash160;
    use keys::Public;
    use script::{Builder, Opcode, Script};

    const TAKER_PUB_HEX: &str = "02031d4256c4bc9f99ac88bf3dba21773132281f65f9bf23a59928bce08961e2f3";
    const MAKER_PUB_HEX: &str = "03f8f8d75dbbbd3f7e1eedf7b2c3b2a5b59ed46e22b8eef0e2ef62e0c1aab9a3a3";
    const TAKER_SECRET: [u8; 32] = [0x11; 32];
    const TAKER_SECRET_HASH: [u8; 32] = [0xaa; 32];
    const LOCKTIME: u32 = 0x6800_0000;

    fn taker_pub() -> Public { Public::from_slice(&hex::decode(TAKER_PUB_HEX).unwrap()).unwrap() }
    fn maker_pub() -> Public { Public::from_slice(&hex::decode(MAKER_PUB_HEX).unwrap()).unwrap() }

    /// The dispatch table must produce the exact same bytes as the direct
    /// builder, so refund paths and validators agree on the redeem script.
    #[test]
    fn should_dispatch_redeem_script_for_taker_funding_to_correct_builder() {
        let direct = taker_funding_script(LOCKTIME, &TAKER_SECRET_HASH, &taker_pub(), &maker_pub());
        let via_dispatch = SwapTxTypeWithSecretHash::TakerFunding {
            taker_secret_hash: &TAKER_SECRET_HASH,
        }
        .redeem_script(LOCKTIME, &taker_pub(), &maker_pub());
        assert_eq!(direct.to_bytes(), via_dispatch.to_bytes());
    }

    /// §15.5.3 script_data must be `[OP_1, OP_0]` so the funding script's
    /// outer-IF (timelock) branch is selected.
    #[test]
    fn should_select_timelock_branch_for_refund_taker_funding_timelock_script_data() {
        let script_data: Script = Builder::default()
            .push_opcode(Opcode::OP_1)
            .push_opcode(Opcode::OP_0)
            .into_script();
        let ops: Vec<Opcode> = script_data.iter().filter_map(|i| i.ok()).map(|i| i.opcode).collect();
        assert_eq!(ops, vec![Opcode::OP_1, Opcode::OP_0]);
    }

    /// §15.5.4 script_data must be `[push taker_secret, OP_0, OP_0]` so the
    /// outer-ELSE / inner-ELSE (secret-reveal refund) branch is selected.
    #[test]
    fn should_select_secret_branch_for_refund_taker_funding_secret_script_data() {
        let script_data: Script = Builder::default()
            .push_data(&TAKER_SECRET)
            .push_opcode(Opcode::OP_0)
            .push_opcode(Opcode::OP_0)
            .into_script();
        let ops: Vec<Opcode> = script_data.iter().filter_map(|i| i.ok()).map(|i| i.opcode).collect();
        assert_eq!(ops, vec![Opcode::OP_PUSHBYTES_32, Opcode::OP_0, Opcode::OP_0]);
        let pushed = script_data
            .iter()
            .filter_map(|i| i.ok())
            .find_map(|i| i.data.map(|d| d.to_vec()))
            .expect("expected a single 32-byte push of the taker secret");
        assert_eq!(pushed.as_slice(), &TAKER_SECRET);
    }

    /// §15.5.15: walk the spend tx's input[0] script_sig and recover the
    /// 32-byte secret whose `dhash160` matches the supplied hash.
    #[test]
    fn should_extract_secret_from_spend_tx_script_sig() {
        let secret = [0x33u8; 32];
        let secret_hash = dhash160(&secret);
        // Fabricate a script_sig: [signature, secret_32_bytes, OP_0, OP_0, redeem]
        let fake_sig = vec![0x42u8; 72]; // OP_PUSHBYTES_72-sized blob
        let fake_redeem = vec![0xAAu8; 25]; // small placeholder redeem script
        let script_sig: Script = Builder::default()
            .push_data(&fake_sig)
            .push_data(&secret)
            .push_opcode(Opcode::OP_0)
            .push_opcode(Opcode::OP_0)
            .push_data(&fake_redeem)
            .into_script();
        let mut tx = UtxoTx::default();
        tx.inputs.push(TransactionInput {
            script_sig: script_sig.to_bytes(),
            ..Default::default()
        });
        tx.outputs.push(TransactionOutput::default());
        let recovered = extract_secret_v2(secret_hash.as_slice(), &tx).unwrap();
        assert_eq!(recovered, secret);
    }

    /// §15.5.5: classifier helper must identify a script_sig whose instruction
    /// at index 1 is `OP_1` as the timelock-refund branch.
    #[test]
    fn should_classify_funding_spend_with_op1_flag_as_timelock_refund() {
        let fake_sig = vec![0x42u8; 72];
        let fake_redeem = vec![0xAAu8; 25];
        let script_sig: Script = Builder::default()
            .push_data(&fake_sig)
            .push_opcode(Opcode::OP_1)
            .push_opcode(Opcode::OP_0)
            .push_data(&fake_redeem)
            .into_script();
        let tag = classify_funding_spend_script_sig(&script_sig.to_bytes());
        assert_eq!(tag, FundingSpendBranchTag::Timelock);
    }

    /// Companion sanity check: a 32-byte raw push at index 1 → Secret branch.
    #[test]
    fn should_classify_funding_spend_with_32byte_push_as_secret_refund() {
        let fake_sig = vec![0x42u8; 72];
        let secret = [0x55u8; 32];
        let fake_redeem = vec![0xAAu8; 25];
        let script_sig: Script = Builder::default()
            .push_data(&fake_sig)
            .push_data(&secret)
            .push_opcode(Opcode::OP_0)
            .push_opcode(Opcode::OP_0)
            .push_data(&fake_redeem)
            .into_script();
        let tag = classify_funding_spend_script_sig(&script_sig.to_bytes());
        assert_eq!(tag, FundingSpendBranchTag::Secret(secret));
    }
}

// ─── V2 cooperative funding-spend tests (chapter 15 §15.5.6 — §15.5.9) ─────
mod swap_v2_funding_spend_tests {
    use crate::utxo::rpc_clients::UtxoRpcClientEnum;
    use crate::utxo::swap_proto_v2_scripts::{taker_funding_script, taker_payment_script};
    use crate::utxo::utxo_common::{build_funding_spend_preimage_tx, sign_funding_spend_input};
    use crate::utxo::utxo_tests::{native_client_for_test, utxo_coin_fields_for_test};
    use crate::utxo::UtxoTx;
    use chain::TransactionOutput;
    use crypto::privkey::key_pair_from_seed;
    use kdf_crypto::dhash160;
    use keys::{KeyPair, Public};
    use script::{Builder, Opcode, Script};

    const TAKER_SECRET_HASH: [u8; 32] = [0xaa; 32];
    const MAKER_SECRET_HASH: [u8; 32] = [0xbb; 32];
    const FUNDING_TIME_LOCK: u32 = 0x6800_0000;
    const TAKER_PAYMENT_TIME_LOCK: u32 = 0x6810_0000;
    const FUNDING_VALUE: u64 = 1_000_000;
    const FEE: u64 = 1_000;

    fn taker_kp() -> KeyPair { key_pair_from_seed("ch15 funding-spend taker").unwrap() }
    fn maker_kp() -> KeyPair { key_pair_from_seed("ch15 funding-spend maker").unwrap() }

    fn synthetic_funding_tx() -> UtxoTx {
        let mut tx = UtxoTx::default();
        tx.version = 4;
        tx.outputs.push(TransactionOutput {
            value: FUNDING_VALUE,
            script_pubkey: vec![0xaa; 23].into(),
        });
        tx
    }

    /// §15.5.6 step 3 — the preimage has a single output whose `script_pubkey`
    /// is P2SH of the taker-payment redeem script, with value `funding_value - fee`.
    #[test]
    fn should_build_funding_spend_preimage_with_expected_output_layout() {
        let fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(native_client_for_test()), None, false);
        let funding = synthetic_funding_tx();
        let taker_payment_redeem = taker_payment_script(
            TAKER_PAYMENT_TIME_LOCK,
            &MAKER_SECRET_HASH,
            taker_kp().public(),
            maker_kp().public(),
        );
        let signer =
            build_funding_spend_preimage_tx(&fields, &funding, &taker_payment_redeem, FEE).expect("build preimage");

        assert_eq!(signer.outputs.len(), 1, "expected single P2SH(taker_payment) output");
        let expected_p2sh = Builder::build_p2sh(&dhash160(&taker_payment_redeem).into()).to_bytes();
        assert_eq!(signer.outputs[0].script_pubkey, expected_p2sh);
        assert_eq!(signer.outputs[0].value, FUNDING_VALUE - FEE);

        assert_eq!(signer.inputs.len(), 1);
        assert_eq!(signer.inputs[0].previous_output.hash, funding.hash());
        assert_eq!(signer.inputs[0].previous_output.index, 0);
    }

    /// §15.5.7 — the partial signature returned by `sign_funding_spend_input`
    /// verifies (via `keys::Public::verify`) against the signing party's pub
    /// for the funding script's cooperative branch sighash.
    #[test]
    fn should_recover_partial_signature_from_funding_spend_preimage() {
        let fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(native_client_for_test()), None, false);
        let funding = synthetic_funding_tx();
        let kp = taker_kp();
        let taker_payment_redeem = taker_payment_script(
            TAKER_PAYMENT_TIME_LOCK,
            &MAKER_SECRET_HASH,
            kp.public(),
            maker_kp().public(),
        );
        let funding_redeem =
            taker_funding_script(FUNDING_TIME_LOCK, &TAKER_SECRET_HASH, kp.public(), maker_kp().public());
        let signer =
            build_funding_spend_preimage_tx(&fields, &funding, &taker_payment_redeem, FEE).expect("build preimage");
        let sig = sign_funding_spend_input(&signer, &funding_redeem, &kp, &fields).expect("sign");

        // Recompute the cooperative-branch sighash and verify the supplied sig.
        let sighash_type = 1u32 | fields.conf.fork_id;
        let digest = signer.signature_hash(
            0,
            signer.inputs[0].amount,
            &funding_redeem,
            fields.conf.signature_version,
            sighash_type,
        );
        assert!(
            kp.public().verify(&digest, &sig).expect("verify"),
            "partial sig must verify against signer's pub"
        );

        // Sanity check: it must NOT verify against an unrelated pub.
        let other_pub: Public = *maker_kp().public();
        assert!(
            !other_pub.verify(&digest, &sig).expect("verify"),
            "partial sig should not verify against an unrelated pub"
        );
    }

    /// §15.5.9 — script_data for `refund_combined_taker_payment` is just
    /// `[OP_1]` (the taker-payment script has a single OP_IF whose true arm
    /// is the timelock refund branch).
    #[test]
    fn should_select_timelock_branch_for_refund_combined_taker_payment_script_data() {
        let script_data: Script = Builder::default().push_opcode(Opcode::OP_1).into_script();
        let ops: Vec<Opcode> = script_data.iter().filter_map(|i| i.ok()).map(|i| i.opcode).collect();
        assert_eq!(ops, vec![Opcode::OP_1]);
    }
}

#[cfg(test)]
mod swap_v2_taker_payment_spend_tests {
    use crate::utxo::rpc_clients::UtxoRpcClientEnum;
    use crate::utxo::swap_proto_v2_scripts::taker_payment_script;
    use crate::utxo::utxo_common::{address_from_raw_pubkey, build_taker_payment_spend_cooperative_script_sig,
                                   build_taker_payment_spend_preimage_tx, gen_taker_payment_spend_preimage,
                                   sign_taker_payment_spend_input};
    use crate::utxo::utxo_tests::{native_client_for_test, utxo_coin_fields_for_test, utxo_coin_from_fields};
    use crate::utxo::{output_script, ScriptType, UtxoCommonOps, UtxoTx};
    use crate::{DexFee, GenTakerPaymentSpendArgs};
    use chain::TransactionOutput;
    use common::block_on;
    use common::mm_number::MmNumber;
    use crypto::privkey::key_pair_from_seed;
    use kdf_crypto::ChecksumType;
    use keys::{Address, AddressFormat as UtxoAddressFormat, KeyPair, Public};
    use script::{Builder, Opcode, Script};

    const MAKER_SECRET_HASH: [u8; 32] = [0xbb; 32];
    const MAKER_SECRET: [u8; 32] = [0x42; 32];
    const TAKER_PAYMENT_TIME_LOCK: u32 = 0x6810_0000;
    const TAKER_PAYMENT_VALUE: u64 = 1_000_000;
    const DEX_FEE_SAT: u64 = 10_000;
    const SPEND_FEE: u64 = 1_000;
    const SWAP_UNIQUE_DATA: &[u8] = b"ch16 payment-spend tests";

    fn taker_kp() -> KeyPair { key_pair_from_seed("ch15 payment-spend taker").unwrap() }
    fn maker_kp() -> KeyPair { key_pair_from_seed("ch15 payment-spend maker").unwrap() }
    fn fee_kp() -> KeyPair { key_pair_from_seed("ch16 payment-spend fee").unwrap() }

    fn maker_address() -> Address {
        Address {
            prefix: 60,
            hash: maker_kp().public().address_hash().into(),
            t_addr_prefix: 0,
            checksum_type: ChecksumType::DSHA256,
            hrp: None,
            addr_format: UtxoAddressFormat::Standard,
        }
    }

    fn synthetic_taker_payment_tx() -> UtxoTx {
        let mut tx = UtxoTx::default();
        tx.version = 4;
        // P2SH of the taker-payment redeem — value matters; script_pubkey contents
        // are not consulted by the preimage builder.
        tx.outputs.push(TransactionOutput {
            value: TAKER_PAYMENT_VALUE,
            script_pubkey: vec![0xaa; 23].into(),
        });
        tx
    }

    /// CRD ch.16 R14 — a `DexFee::Standard` preimage has a single output: the
    /// fee-collection P2PKH for the full converted fee amount. The maker
    /// payout is not part of the preimage; the maker appends it later (R18).
    /// Replaces the withdrawn pre-correction test of the same name, which
    /// asserted the maker payout was output 0 (the earlier, wrong contract;
    /// see ch.16's "Compatibility correction").
    #[test]
    fn should_build_taker_payment_spend_preimage_with_expected_output_to_maker_address() {
        let coin = utxo_coin_from_fields(utxo_coin_fields_for_test(
            UtxoRpcClientEnum::Native(native_client_for_test()),
            None,
            false,
        ));
        let taker_payment = synthetic_taker_payment_tx();
        let maker_addr = maker_address();
        let taker_pub = *taker_kp().public();
        let maker_pub = *maker_kp().public();
        let dex_fee_addr_raw_pubkey = *fee_kp().public();
        let dex_fee = DexFee::Standard(MmNumber::from((DEX_FEE_SAT, 100_000_000u64)));
        let args = GenTakerPaymentSpendArgs {
            taker_tx: &taker_payment,
            time_lock: TAKER_PAYMENT_TIME_LOCK as u64,
            maker_secret_hash: &MAKER_SECRET_HASH,
            maker_pub: &maker_pub,
            maker_address: &maker_addr,
            taker_pub: &taker_pub,
            dex_fee: &dex_fee,
            premium_amount: 0u64.into(),
            trading_amount: 1u64.into(),
            dex_fee_addr_raw_pubkey: dex_fee_addr_raw_pubkey.as_ref(),
        };
        let preimage = block_on(gen_taker_payment_spend_preimage(&coin, &args, SWAP_UNIQUE_DATA)).expect("preimage");
        let signer = preimage.preimage.0;

        let fee_address = address_from_raw_pubkey(
            dex_fee_addr_raw_pubkey.as_ref(),
            coin.as_ref().conf.pub_addr_prefix,
            coin.as_ref().conf.pub_t_addr_prefix,
            coin.as_ref().conf.checksum_type,
            coin.as_ref().conf.bech32_hrp.clone(),
            coin.addr_format().clone(),
        )
        .unwrap();
        let expected_script_pubkey = output_script(&fee_address, ScriptType::P2PKH).to_bytes();

        assert_eq!(signer.outputs.len(), 1, "Standard preimage has exactly one output");
        assert_eq!(signer.outputs[0].script_pubkey, expected_script_pubkey);
        assert_eq!(signer.outputs[0].value, DEX_FEE_SAT);

        assert_eq!(signer.inputs.len(), 1);
        assert_eq!(signer.inputs[0].previous_output.hash, taker_payment.hash());
        assert_eq!(signer.inputs[0].previous_output.index, 0);
        assert_eq!(signer.inputs[0].amount, TAKER_PAYMENT_VALUE);
    }

    /// Reviewed for CRD ch.16 R14/R15 (issue #11): this exercises the
    /// generic `sign_taker_payment_spend_input` sign/verify mechanics against
    /// an arbitrary single-output preimage under the `SINGLE` flag -- it does
    /// not assert which output that is, so it is unaffected by the ch.16
    /// output-ordering correction. The partial signature returned by
    /// `sign_taker_payment_spend_input` verifies (via `keys::Public::verify`)
    /// against the signing party's pub for the cooperative-branch sighash of
    /// the taker-payment redeem script.
    #[test]
    fn should_recover_partial_signature_from_taker_payment_spend_preimage() {
        let fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(native_client_for_test()), None, false);
        let taker_payment = synthetic_taker_payment_tx();
        let taker = taker_kp();
        let maker = maker_kp();
        let redeem = taker_payment_script(
            TAKER_PAYMENT_TIME_LOCK,
            &MAKER_SECRET_HASH,
            taker.public(),
            maker.public(),
        );
        let maker_addr = maker_address();
        let outputs = vec![TransactionOutput {
            value: TAKER_PAYMENT_VALUE - DEX_FEE_SAT - SPEND_FEE,
            script_pubkey: output_script(&maker_addr, ScriptType::P2PKH).to_bytes(),
        }];
        let signer = build_taker_payment_spend_preimage_tx(&fields, &taker_payment, outputs).expect("build preimage");

        // SIGHASH_SINGLE (3) for the DexFee::Standard path.
        let sighash_type = 3u32 | fields.conf.fork_id;
        let sig = sign_taker_payment_spend_input(&signer, &redeem, &taker, &fields, sighash_type).expect("sign");
        let digest = signer.signature_hash(
            0,
            signer.inputs[0].amount,
            &redeem,
            fields.conf.signature_version,
            sighash_type,
        );
        assert!(
            taker.public().verify(&digest, &sig).expect("verify"),
            "taker partial sig must verify against the cooperative-branch sighash"
        );

        // Sanity: must NOT verify against an unrelated pub.
        let other_pub: Public = *maker.public();
        assert!(
            !other_pub.verify(&digest, &sig).expect("verify"),
            "partial sig should not verify against an unrelated pub"
        );
    }

    /// CRD ch.16 R15/R18 — cooperative-branch script_sig pushes (bottom→top):
    /// `maker_sig, taker_sig, maker_secret, OP_0, redeem`. No leading OP_0
    /// stuffer because the redeem uses CHECKSIGVERIFY/CHECKSIG, not
    /// OP_CHECKMULTISIG. Each signature carries its own flag byte -- the
    /// maker's ALL (0x01) and the taker's SINGLE (0x03) here, proving the two
    /// can differ (the `Standard` case). A single shared byte (the withdrawn
    /// pre-correction contract) could not represent this.
    #[test]
    fn should_select_maker_secret_branch_for_taker_payment_spend_script_data() {
        let redeem = taker_payment_script(
            TAKER_PAYMENT_TIME_LOCK,
            &MAKER_SECRET_HASH,
            taker_kp().public(),
            maker_kp().public(),
        );
        let maker_sig_der = vec![0x30, 0x44, 0xAA, 0xBB];
        let taker_sig_der = vec![0x30, 0x44, 0xCC, 0xDD];
        let maker_sighash_byte = 0x01u8;
        let taker_sighash_byte = 0x03u8;

        let script_sig_bytes = build_taker_payment_spend_cooperative_script_sig(
            &maker_sig_der,
            &taker_sig_der,
            &MAKER_SECRET,
            maker_sighash_byte,
            taker_sighash_byte,
            &redeem,
        );
        let script: Script = script_sig_bytes.into();
        let instrs: Vec<_> = script.iter().collect::<Result<Vec<_>, _>>().expect("script parses");

        assert_eq!(instrs.len(), 5, "expected 5 stack contributions");
        // instrs[0]: maker_sig + its own sighash byte
        let mut expected_maker = maker_sig_der.clone();
        expected_maker.push(maker_sighash_byte);
        assert_eq!(instrs[0].data, Some(expected_maker.as_slice()));
        // instrs[1]: taker_sig + its own (different) sighash byte
        let mut expected_taker = taker_sig_der.clone();
        expected_taker.push(taker_sighash_byte);
        assert_eq!(instrs[1].data, Some(expected_taker.as_slice()));
        // instrs[2]: 32-byte maker secret
        assert_eq!(instrs[2].opcode, Opcode::OP_PUSHBYTES_32);
        assert_eq!(instrs[2].data, Some(MAKER_SECRET.as_slice()));
        // instrs[3]: OP_0 selects the cooperative branch of the outer OP_IF
        assert_eq!(instrs[3].opcode, Opcode::OP_0);
        // instrs[4]: redeem script push
        assert_eq!(instrs[4].data, Some(redeem.to_bytes().as_slice()));
    }
}

/// §16.6 — Pre-burn output unit tests for the V2 taker-payment-spend.
#[cfg(test)]
mod swap_v2_pre_burn_tests {
    use crate::utxo::rpc_clients::{NativeClient, UnspentInfo, UtxoRpcClientEnum, UtxoRpcClientOps};
    use crate::utxo::utxo_common;
    use crate::utxo::utxo_standard::UtxoStandardCoin;
    use crate::utxo::utxo_tests::{native_client_for_test, utxo_coin_fields_for_test, utxo_coin_from_fields};
    use crate::utxo::{output_script, sat_from_big_decimal, ActualTxFee, GenerateTxError, ScriptType, UtxoCommonOps,
                      UtxoTx, UtxoTxGenerationOps};
    use crate::{calc_dex_fee_for_burn_account, calc_dex_fee_for_op_return, DexFee, DexFeeBurnDestination,
                GenTakerPaymentSpendArgs, MmCoin, ValidateTakerPaymentSpendPreimageError};
    use chain::{OutPoint, TransactionOutput};
    use common::block_on;
    use common::mm_number::MmNumber;
    use kdf_crypto::ChecksumType;
    use keys::{Address, AddressFormat as UtxoAddressFormat};
    use mm2_net_config::net_config_or_panic;
    use mocktopus::mocking::*;
    use rpc::v1::types::H256 as H256Json;
    use script::{Builder, Opcode, Script};

    const MAKER_SECRET_HASH: [u8; 32] = [0xbb; 32];
    const TAKER_PAYMENT_TIME_LOCK: u32 = 0x6810_0000;
    const TAKER_PAYMENT_VALUE: u64 = 100_000_000;
    const SWAP_UNIQUE_DATA: &[u8] = b"ch16 pre-burn unit tests";

    /// Build a non-KMD `UtxoStandardCoin` (ticker = "DOC") with the standard
    /// test fixture; both burn opt-ins are false.
    fn doc_coin() -> UtxoStandardCoin {
        let fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(native_client_for_test()), None, false);
        utxo_coin_from_fields(fields)
    }

    /// Build a KMD-flavoured fixture (ticker = "KMD") for the
    /// `should_burn_directly = true` path.
    fn kmd_coin() -> UtxoStandardCoin {
        let mut fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(native_client_for_test()), None, false);
        fields.conf.ticker = "KMD".to_owned();
        utxo_coin_from_fields(fields)
    }

    fn synthetic_taker_payment_tx() -> UtxoTx {
        let mut tx = UtxoTx::default();
        tx.version = 4;
        tx.outputs.push(TransactionOutput {
            value: TAKER_PAYMENT_VALUE,
            script_pubkey: vec![0xaa; 23].into(),
        });
        tx
    }

    fn maker_address_for(coin: &UtxoStandardCoin) -> Address {
        Address {
            prefix: coin.as_ref().conf.pub_addr_prefix,
            hash: coin
                .as_ref()
                .priv_key_policy
                .key_pair_or_err()
                .unwrap()
                .public()
                .address_hash()
                .into(),
            t_addr_prefix: coin.as_ref().conf.pub_t_addr_prefix,
            checksum_type: ChecksumType::DSHA256,
            hrp: None,
            addr_format: UtxoAddressFormat::Standard,
        }
    }

    /// A non-KMD UTXO taker on netid 8762 retains the single-output form.
    #[test]
    fn should_keep_non_kmd_fee_standard_on_netid_8762() {
        let coin = doc_coin();
        let net_cfg = net_config_or_panic(8762);
        let total = MmNumber::from("1");
        let dex_fee = DexFee::new_from_taker_coin(&coin as &dyn MmCoin, net_cfg, total.clone());
        assert_eq!(dex_fee, DexFee::Standard(total));
    }

    /// The dormant burn-account helper retains its 75/25 split and dust guard.
    #[test]
    fn should_fall_back_to_standard_when_burn_account_share_is_dust() {
        let burn_pubkey = vec![0x02; 33];
        let valid_total = MmNumber::from("1");
        let valid = calc_dex_fee_for_burn_account(
            valid_total.clone(),
            MmNumber::from("0.00001"),
            MmNumber::from((3, 4)),
            burn_pubkey.clone(),
        );
        assert_eq!(valid, DexFee::WithBurn {
            fee_amount: &valid_total * &MmNumber::from((3, 4)),
            burn_amount: &valid_total * &MmNumber::from((1, 4)),
            burn_destination: DexFeeBurnDestination::PreBurnAccount { burn_pubkey },
        });

        let total = MmNumber::from("0.00002");
        let dex_fee = calc_dex_fee_for_burn_account(
            total.clone(),
            MmNumber::from("0.00001"),
            MmNumber::from((3, 4)),
            vec![0x02; 33],
        );
        assert_eq!(dex_fee, DexFee::Standard(total));
    }

    #[test]
    fn should_fall_back_to_standard_for_non_positive_op_return_split() {
        let total = MmNumber::from("1");
        let dex_fee = calc_dex_fee_for_op_return(total.clone(), MmNumber::from("0.00001"), MmNumber::from(1));
        assert_eq!(dex_fee, DexFee::Standard(total));
    }

    /// Netid 8762 KMD uses the legacy 75/25 OP_RETURN split.
    #[test]
    fn should_emit_kmd_op_return_split_on_netid_8762() {
        let coin = kmd_coin();
        let net_cfg = net_config_or_panic(8762);
        let total = MmNumber::from("1");
        let dex_fee = DexFee::new_from_taker_coin(&coin as &dyn MmCoin, net_cfg, total.clone());
        match dex_fee {
            DexFee::WithBurn {
                fee_amount,
                burn_amount,
                burn_destination: DexFeeBurnDestination::KmdOpReturn,
            } => {
                assert_eq!(fee_amount, &total * &MmNumber::from((3, 4)));
                assert_eq!(burn_amount, &total * &MmNumber::from((1, 4)));
            },
            other => panic!("expected WithBurn{{KmdOpReturn}}, got {:?}", other),
        }
    }

    /// Netid 6133 keeps the standard form even for a coin whose direct-burn
    /// predicate is true.
    #[test]
    fn should_keep_kmd_fee_standard_on_netid_6133() {
        let coin = kmd_coin();
        let net_cfg = net_config_or_panic(6133);
        let total = MmNumber::from("1");
        let dex_fee = DexFee::new_from_taker_coin(&coin as &dyn MmCoin, net_cfg, total.clone());
        assert_eq!(dex_fee, DexFee::Standard(total));
    }

    /// Issue #1 wire regression: the descriptor must become the two outputs
    /// accepted by a v2.6.0-beta netid-8762 counterparty.
    #[test]
    fn should_build_v2_6_0_beta_kmd_taker_fee_outputs() {
        let coin = kmd_coin();
        let net_cfg = net_config_or_panic(8762);
        let trade_amount = MmNumber::from("15.86");
        let total = &trade_amount * &MmNumber::from((9, 7770));
        let dex_fee = DexFee::new_from_taker_coin(&coin as &dyn MmCoin, net_cfg, total);
        let fee_address = maker_address_for(&coin);

        let outputs = utxo_common::generate_taker_fee_tx_outputs(&coin, &dex_fee, &fee_address).unwrap();

        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0].value, 1_377_799);
        assert_eq!(
            outputs[0].script_pubkey,
            output_script(&fee_address, ScriptType::P2PKH).to_bytes()
        );
        assert_eq!(outputs[1].value, 459_266);
        assert_eq!(
            outputs[1].script_pubkey,
            Builder::default().push_opcode(Opcode::OP_RETURN).into_bytes()
        );
    }

    /// CRD ch.08 R8 / T5A -- the three exact-rational ranges of the KMD
    /// direct-burn split, netid 8762, eight decimals, dust 1,000 base units.
    /// Replaces the withdrawn pre-correction test that asserted the 868/289
    /// split and a builder dust exemption (issue #11): the reference emits
    /// 1,000 + 158 for a 0.01 KMD taker, not 868 + 289. No descriptor this
    /// factory emits needs any builder dust exemption any more (R15A).
    #[test]
    fn should_apply_kmd_direct_burn_ranges_t5a() {
        let kmd = kmd_coin();
        let net_cfg = net_config_or_panic(8762);
        let fee_address = maker_address_for(&kmd);
        let discounted_rate = MmNumber::from((9, 7770));

        // (trade amount as an exact fraction, expected fee-output value,
        // expected burn value or None for a single-output Standard(dust)
        // descriptor).
        let cases: &[((u64, u64), u64, Option<u64>)] = &[
            ((84, 10000), 1_000, None),
            // 259/30000 KMD is the exact range-1/range-3 boundary (product == dust).
            ((259, 30000), 1_000, None),
            ((1, 100), 1_000, Some(158)),
            ((115, 10000), 1_000, Some(332)),
            ((116, 10000), 1_007, Some(335)),
        ];

        for (trade_amount, expected_fee, expected_burn) in cases {
            let trade_amount = MmNumber::from(*trade_amount);
            let total = &trade_amount * &discounted_rate;
            let dex_fee = DexFee::new_from_taker_coin(&kmd as &dyn MmCoin, net_cfg, total);
            let outputs = utxo_common::generate_taker_fee_tx_outputs(&kmd, &dex_fee, &fee_address).unwrap();

            match expected_burn {
                None => {
                    assert_eq!(outputs.len(), 1, "trade {}", trade_amount);
                    assert_eq!(outputs[0].value, *expected_fee, "trade {}", trade_amount);
                    assert_eq!(
                        outputs[0].script_pubkey,
                        output_script(&fee_address, ScriptType::P2PKH).to_bytes(),
                        "trade {}",
                        trade_amount
                    );
                },
                Some(expected_burn) => {
                    assert_eq!(outputs.len(), 2, "trade {}", trade_amount);
                    assert_eq!(outputs[0].value, *expected_fee, "trade {}", trade_amount);
                    assert_eq!(
                        outputs[0].script_pubkey,
                        output_script(&fee_address, ScriptType::P2PKH).to_bytes(),
                        "trade {}",
                        trade_amount
                    );
                    assert_eq!(outputs[1].value, *expected_burn, "trade {}", trade_amount);
                    assert_eq!(
                        outputs[1].script_pubkey,
                        Builder::default().push_opcode(Opcode::OP_RETURN).into_bytes(),
                        "trade {}",
                        trade_amount
                    );
                },
            }

            // None of these descriptors needs a builder dust exemption any more.
            let builder_coin = doc_coin();
            let unspents = vec![UnspentInfo {
                value: 10_000_000,
                outpoint: OutPoint::default(),
                height: None,
            }];
            let (tx, _) = block_on(
                utxo_common::UtxoTxBuilder::new(&builder_coin)
                    .add_available_inputs(unspents)
                    .add_outputs(outputs.clone())
                    .with_fee(ActualTxFee::FixedPerKb(1_000))
                    .build(),
            )
            .unwrap();
            assert_eq!(tx.outputs.len(), outputs.len() + 1, "trade {} (+ change)", trade_amount);
        }
    }

    /// CRD ch.08 T5B -- a maker validating the 0.01 KMD descriptor of T5A
    /// MUST accept the `v2.6.0-beta` on-wire shape (1,000 + 158) and MUST
    /// reject the withdrawn pre-correction shape (868 + 289).
    #[test]
    fn should_validate_kmd_taker_fee_per_reference_wire_shape_t5b() {
        let kmd = kmd_coin();
        let net_cfg = net_config_or_panic(8762);
        let fee_address = maker_address_for(&kmd);
        let trade_amount = MmNumber::from("0.01");
        let total = &trade_amount * &MmNumber::from((9, 7770));
        let dex_fee = DexFee::new_from_taker_coin(&kmd as &dyn MmCoin, net_cfg, total);

        // v2.6.0-beta wire shape: 1,000 to the fee address, then 158 on a
        // bare OP_RETURN, then change.
        let reference_outputs = vec![
            TransactionOutput {
                value: 1_000,
                script_pubkey: output_script(&fee_address, ScriptType::P2PKH).to_bytes(),
            },
            TransactionOutput {
                value: 158,
                script_pubkey: Builder::default().push_opcode(Opcode::OP_RETURN).into_bytes(),
            },
            TransactionOutput {
                value: 5_000,
                script_pubkey: output_script(&fee_address, ScriptType::P2PKH).to_bytes(),
            },
        ];
        let mut reference_tx = UtxoTx::default();
        reference_tx.version = 4;
        reference_tx.outputs = reference_outputs;

        match &dex_fee {
            DexFee::WithBurn {
                fee_amount,
                burn_amount,
                burn_destination: DexFeeBurnDestination::KmdOpReturn,
            } => {
                let decimals = kmd.as_ref().decimals;
                let fee_sat = sat_from_big_decimal(&fee_amount.to_decimal(), decimals).unwrap();
                let burn_sat = sat_from_big_decimal(&burn_amount.to_decimal(), decimals).unwrap();
                assert_eq!(fee_sat, 1_000);
                assert_eq!(burn_sat, 158);
                assert!(reference_tx.outputs[0].value >= fee_sat);
                assert!(reference_tx.outputs[1].value >= burn_sat);

                // The withdrawn pre-correction shape (868 fee) MUST be rejected:
                // its value is below the now-expected 1,000.
                let old_fee_output = TransactionOutput {
                    value: 868,
                    script_pubkey: output_script(&fee_address, ScriptType::P2PKH).to_bytes(),
                };
                assert!(old_fee_output.value < fee_sat);
            },
            other => panic!("expected WithBurn{{KmdOpReturn}}, got {:?}", other),
        }
    }

    /// CRD ch.08 R15C / ch.16 R7 (issue #11): a taker whose taker-coin swap
    /// public key equals the network burn/waiver key pays no fee, on both
    /// production networks, independently of the burn gate. This inverts the
    /// withdrawn `should_not_waive_fee_for_inactive_burn_key_on_netid_6133`
    /// (which asserted the opposite before the R7 correction).
    #[test]
    fn should_waive_fee_for_burn_key_taker_on_both_netids() {
        for netid in [8762u16, 6133u16] {
            let coin = doc_coin();
            let net_cfg = net_config_or_panic(netid);
            let total = MmNumber::from("1");
            let burn_pubkey = net_cfg.burn_addr_raw_pubkey().to_vec();
            assert!(!burn_pubkey.is_empty(), "netid {} burn key must be non-empty", netid);
            let dex_fee = DexFee::new_with_taker_pubkey(&coin as &dyn MmCoin, net_cfg, total, &burn_pubkey);
            assert_eq!(dex_fee, DexFee::NoFee, "netid {}", netid);
        }
    }

    /// A key differing in one byte from the waiver key MUST NOT waive the fee.
    #[test]
    fn should_not_waive_fee_for_a_different_pubkey() {
        let coin = doc_coin();
        let net_cfg = net_config_or_panic(8762);
        let total = MmNumber::from("1");
        let mut not_burn_pubkey = net_cfg.burn_addr_raw_pubkey().to_vec();
        let last = not_burn_pubkey.len() - 1;
        not_burn_pubkey[last] ^= 0x01;
        let dex_fee = DexFee::new_with_taker_pubkey(&coin as &dyn MmCoin, net_cfg, total.clone(), &not_burn_pubkey);
        assert_eq!(dex_fee, DexFee::Standard(total));
    }

    /// CRD ch.16 R14/R20 — `gen_taker_payment_spend_preimage` for
    /// `WithBurn{PreBurnAccount}` produces three outputs in order: fee, burn,
    /// maker. Replaces the withdrawn pre-correction test of the same name,
    /// which asserted the old maker-first order.
    #[test]
    fn should_build_taker_payment_spend_preimage_with_three_outputs_for_with_burn() {
        let coin = doc_coin();
        let net_cfg = net_config_or_panic(6133);
        let taker_payment = synthetic_taker_payment_tx();
        let maker_addr = maker_address_for(&coin);
        let taker_pub = *coin.as_ref().priv_key_policy.key_pair_or_err().unwrap().public();
        let maker_pub = taker_pub;
        let dex_fee = DexFee::WithBurn {
            fee_amount: MmNumber::from("0.0075"),
            burn_amount: MmNumber::from("0.0025"),
            burn_destination: DexFeeBurnDestination::PreBurnAccount {
                burn_pubkey: net_cfg.burn_addr_raw_pubkey().to_vec(),
            },
        };
        let args = GenTakerPaymentSpendArgs {
            taker_tx: &taker_payment,
            time_lock: TAKER_PAYMENT_TIME_LOCK as u64,
            maker_secret_hash: &MAKER_SECRET_HASH,
            maker_pub: &maker_pub,
            maker_address: &maker_addr,
            taker_pub: &taker_pub,
            dex_fee: &dex_fee,
            premium_amount: 0u64.into(),
            trading_amount: 1u64.into(),
            dex_fee_addr_raw_pubkey: net_cfg.dex_fee_addr_raw_pubkey(),
        };
        let preimage = block_on(utxo_common::gen_taker_payment_spend_preimage(
            &coin,
            &args,
            SWAP_UNIQUE_DATA,
        ))
        .expect("preimage");
        let signer = preimage.preimage.0;
        assert_eq!(signer.outputs.len(), 3, "WithBurn preimage has 3 outputs");
        // Output 0 — fee P2PKH to the network's fee address, value 750_000 sat.
        let expected_fee = output_script(
            &utxo_common::address_from_raw_pubkey(
                net_cfg.dex_fee_addr_raw_pubkey(),
                coin.as_ref().conf.pub_addr_prefix,
                coin.as_ref().conf.pub_t_addr_prefix,
                coin.as_ref().conf.checksum_type,
                coin.as_ref().conf.bech32_hrp.clone(),
                coin.addr_format().clone(),
            )
            .unwrap(),
            ScriptType::P2PKH,
        )
        .to_bytes();
        assert_eq!(signer.outputs[0].script_pubkey, expected_fee);
        assert_eq!(signer.outputs[0].value, 750_000);
        // Output 1 — P2PKH for the burn pubkey, value = 250_000 sat (0.0025 * 10^8).
        assert_eq!(signer.outputs[1].value, 250_000);
        assert_eq!(signer.outputs[1].script_pubkey.len(), 25, "P2PKH is 25 bytes");
        // Output 2 — maker P2PKH, value = 100_000_000 - 750_000 - 250_000 - 496 (S).
        let expected_maker = output_script(&maker_addr, ScriptType::P2PKH).to_bytes();
        assert_eq!(signer.outputs[2].script_pubkey, expected_maker);
        assert_eq!(signer.outputs[2].value, TAKER_PAYMENT_VALUE - 750_000 - 250_000 - 496);
    }

    /// CRD ch.16 R14/R20 — KMD `WithBurn{KmdOpReturn}` path: output 1 is a
    /// bare `OP_RETURN` (no data push) whose value carries the burned
    /// amount. Replaces the withdrawn pre-correction test, which asserted a
    /// zero-value `OP_RETURN` at output 2.
    #[test]
    fn should_build_taker_payment_spend_preimage_with_op_return_for_kmd_burn() {
        let coin = kmd_coin();
        let net_cfg = net_config_or_panic(8762);
        let taker_payment = synthetic_taker_payment_tx();
        let maker_addr = maker_address_for(&coin);
        let taker_pub = *coin.as_ref().priv_key_policy.key_pair_or_err().unwrap().public();
        let maker_pub = taker_pub;
        let dex_fee = DexFee::WithBurn {
            fee_amount: MmNumber::from(0),
            burn_amount: MmNumber::from("0.01"),
            burn_destination: DexFeeBurnDestination::KmdOpReturn,
        };
        let args = GenTakerPaymentSpendArgs {
            taker_tx: &taker_payment,
            time_lock: TAKER_PAYMENT_TIME_LOCK as u64,
            maker_secret_hash: &MAKER_SECRET_HASH,
            maker_pub: &maker_pub,
            maker_address: &maker_addr,
            taker_pub: &taker_pub,
            dex_fee: &dex_fee,
            premium_amount: 0u64.into(),
            trading_amount: 1u64.into(),
            dex_fee_addr_raw_pubkey: net_cfg.dex_fee_addr_raw_pubkey(),
        };
        let preimage = block_on(utxo_common::gen_taker_payment_spend_preimage(
            &coin,
            &args,
            SWAP_UNIQUE_DATA,
        ))
        .expect("preimage");
        let signer = preimage.preimage.0;
        assert_eq!(signer.outputs.len(), 3, "WithBurn preimage has 3 outputs");
        // Output 1 is the OP_RETURN burn leg: value carries the burned
        // amount, script is the bare opcode with no data push (R20).
        assert_eq!(
            signer.outputs[1].value, 1_000_000,
            "OP_RETURN output value carries the burned amount"
        );
        assert_eq!(
            signer.outputs[1].script_pubkey.len(),
            1,
            "OP_RETURN script must be the bare opcode with no data push"
        );
        assert_eq!(
            signer.outputs[1].script_pubkey[0],
            Opcode::OP_RETURN as u8,
            "OP_RETURN opcode must lead the burn script"
        );
        // Output 2 is the maker payout.
        let expected_maker = output_script(&maker_addr, ScriptType::P2PKH).to_bytes();
        assert_eq!(signer.outputs[2].script_pubkey, expected_maker);
        assert_eq!(signer.outputs[2].value, TAKER_PAYMENT_VALUE - 1_000_000 - 496);
    }

    /// §16.5.2 — the taker's partial signature on a `WithBurn` preimage
    /// verifies under `SIGHASH_ALL_BASE | fork_id` against the cooperative
    /// branch.
    #[test]
    fn should_recover_partial_signature_from_with_burn_preimage_under_sighash_all() {
        use crate::utxo::swap_proto_v2_scripts::taker_payment_script;

        let coin = doc_coin();
        let net_cfg = net_config_or_panic(6133);
        let taker_payment = synthetic_taker_payment_tx();
        let maker_addr = maker_address_for(&coin);
        let taker_kp = *coin.as_ref().priv_key_policy.key_pair_or_err().unwrap();
        let taker_pub = *taker_kp.public();
        let maker_pub = taker_pub;
        let dex_fee = DexFee::WithBurn {
            fee_amount: MmNumber::from("0.0075"),
            burn_amount: MmNumber::from("0.0025"),
            burn_destination: DexFeeBurnDestination::PreBurnAccount {
                burn_pubkey: net_cfg.burn_addr_raw_pubkey().to_vec(),
            },
        };
        let args = GenTakerPaymentSpendArgs {
            taker_tx: &taker_payment,
            time_lock: TAKER_PAYMENT_TIME_LOCK as u64,
            maker_secret_hash: &MAKER_SECRET_HASH,
            maker_pub: &maker_pub,
            maker_address: &maker_addr,
            taker_pub: &taker_pub,
            dex_fee: &dex_fee,
            premium_amount: 0u64.into(),
            trading_amount: 1u64.into(),
            dex_fee_addr_raw_pubkey: net_cfg.dex_fee_addr_raw_pubkey(),
        };
        let preimage = block_on(utxo_common::gen_taker_payment_spend_preimage(
            &coin,
            &args,
            SWAP_UNIQUE_DATA,
        ))
        .expect("preimage");
        let signer = preimage.preimage.0;
        let redeem = taker_payment_script(TAKER_PAYMENT_TIME_LOCK, &MAKER_SECRET_HASH, &taker_pub, &maker_pub);
        // SIGHASH_ALL_BASE = 1, fork_id from fixture is 0.
        let sighash = 1u32 | coin.as_ref().conf.fork_id;
        let digest = signer.signature_hash(
            0,
            signer.inputs[0].amount,
            &redeem,
            coin.as_ref().conf.signature_version,
            sighash,
        );
        assert!(
            taker_pub.verify(&digest, &preimage.signature).expect("verify"),
            "WithBurn partial sig must verify under SIGHASH_ALL"
        );
    }

    /// CRD ch.16 R17 — mutating any single output value by one base unit
    /// (here, the burn output at index 1) MUST be rejected: the maker
    /// rebuilds the expected preimage and requires an exact match, with no
    /// tolerance. Replaces the withdrawn pre-correction test of the same
    /// name, which mutated the (then maker-first) output 2 and asserted a
    /// 10%-tolerance-specific error message.
    #[test]
    fn should_reject_with_burn_preimage_with_wrong_burn_value() {
        let coin = doc_coin();
        let net_cfg = net_config_or_panic(6133);
        let taker_payment = synthetic_taker_payment_tx();
        let maker_addr = maker_address_for(&coin);
        let taker_pub = *coin.as_ref().priv_key_policy.key_pair_or_err().unwrap().public();
        let maker_pub = taker_pub;
        let dex_fee = DexFee::WithBurn {
            fee_amount: MmNumber::from("0.0075"),
            burn_amount: MmNumber::from("0.0025"),
            burn_destination: DexFeeBurnDestination::PreBurnAccount {
                burn_pubkey: net_cfg.burn_addr_raw_pubkey().to_vec(),
            },
        };
        let args = GenTakerPaymentSpendArgs {
            taker_tx: &taker_payment,
            time_lock: TAKER_PAYMENT_TIME_LOCK as u64,
            maker_secret_hash: &MAKER_SECRET_HASH,
            maker_pub: &maker_pub,
            maker_address: &maker_addr,
            taker_pub: &taker_pub,
            dex_fee: &dex_fee,
            premium_amount: 0u64.into(),
            trading_amount: 1u64.into(),
            dex_fee_addr_raw_pubkey: net_cfg.dex_fee_addr_raw_pubkey(),
        };
        let mut preimage = block_on(utxo_common::gen_taker_payment_spend_preimage(
            &coin,
            &args,
            SWAP_UNIQUE_DATA,
        ))
        .expect("preimage");
        // Output 1 is the burn leg under the new fee/burn/maker order (R14).
        // Mutate it by a single base unit -- no tolerance applies (R17).
        preimage.preimage.0.outputs[1].value += 1;

        let res = block_on(utxo_common::validate_taker_payment_spend_preimage(
            &coin, &args, &preimage,
        ));
        let err = res.expect_err("validation must fail when the burn output value is off by one");
        let msg = err.to_string();
        match err.into_inner() {
            ValidateTakerPaymentSpendPreimageError::InvalidPreimage(_) => (),
            other => panic!("expected InvalidPreimage, got {:?} ({})", other, msg),
        }
        assert!(
            msg.contains("do not exactly match"),
            "error message must state the exact-match mismatch, got: {}",
            msg
        );
    }

    /// CRD ch.16 T6 -- version-two exact vector, netid 8762, KMD taker
    /// (`NoFee` by R12B). Trade 1 KMD: taker-payment value P = 99,999,504
    /// (funding 100,000,000 minus the funding-spend fee S=496, chapter 15
    /// R21). This test starts directly from P -- the funding-spend
    /// conversion step itself is chapter 15's own R21/R22 concern, already
    /// covered by the funding-spend tests with the updated fee constant.
    /// The taker-payment-spend preimage MUST be `[maker 99,999,008]`
    /// (P - S), with both signature flag bytes `0x01` (`ALL`, fork id 0).
    #[test]
    fn t6_v2_nofee_exact_vector_netid_8762_kmd_taker() {
        const P: u64 = 99_999_504;

        let coin = kmd_coin();
        let mut taker_payment = UtxoTx::default();
        taker_payment.version = 4;
        taker_payment.outputs.push(TransactionOutput {
            value: P,
            script_pubkey: vec![0xaa; 23].into(),
        });
        let maker_addr = maker_address_for(&coin);
        let taker_pub = *coin.as_ref().priv_key_policy.key_pair_or_err().unwrap().public();
        let maker_pub = taker_pub;
        let dex_fee = DexFee::NoFee;
        let args = GenTakerPaymentSpendArgs {
            taker_tx: &taker_payment,
            time_lock: TAKER_PAYMENT_TIME_LOCK as u64,
            maker_secret_hash: &MAKER_SECRET_HASH,
            maker_pub: &maker_pub,
            maker_address: &maker_addr,
            taker_pub: &taker_pub,
            dex_fee: &dex_fee,
            premium_amount: 0u64.into(),
            trading_amount: 1u64.into(),
            dex_fee_addr_raw_pubkey: &[],
        };
        let preimage = block_on(utxo_common::gen_taker_payment_spend_preimage(
            &coin,
            &args,
            SWAP_UNIQUE_DATA,
        ))
        .expect("preimage");
        let signer = preimage.preimage.0;
        assert_eq!(signer.outputs.len(), 1, "NoFee preimage has exactly one output");
        assert_eq!(signer.outputs[0].value, 99_999_008, "P - S");
        let expected_maker = output_script(&maker_addr, ScriptType::P2PKH).to_bytes();
        assert_eq!(signer.outputs[0].script_pubkey, expected_maker);

        // Both flags ALL (0x01); KMD has fork identifier zero.
        let flag_byte = (1u32 | coin.as_ref().conf.fork_id) as u8;
        assert_eq!(flag_byte, 0x01);
    }

    /// CRD ch.16 T7 -- version-two exact vector, `Standard` layout, netid
    /// 6133, KMD taker vs a non-GLEEC maker (base rate 2/100). Trade 0.01
    /// KMD: fee 20,000 sat, taker-payment value P = 1,019,504 (funding
    /// 1,020,000 minus the funding-spend fee S=496 -- see T6's note on
    /// scope). The taker preimage MUST be `[fee P2PKH 20,000]` alone (flag
    /// `0x03`, `SINGLE`); the maker MUST append `[maker 999,008]` as output
    /// 1 (flag `0x01`, `ALL`), giving a final two-output transaction with
    /// the fee output first.
    #[test]
    fn t7_v2_standard_exact_vector_netid_6133_kmd_taker() {
        const P: u64 = 1_019_504;
        const FEE_SAT: u64 = 20_000;
        const MAKER_VALUE: u64 = 999_008;
        const MAKER_SECRET: [u8; 32] = [0x42; 32];

        let coin = kmd_coin();
        let net_cfg = net_config_or_panic(6133);
        let mut taker_payment = UtxoTx::default();
        taker_payment.version = 4;
        taker_payment.outputs.push(TransactionOutput {
            value: P,
            script_pubkey: vec![0xaa; 23].into(),
        });
        let maker_addr = maker_address_for(&coin);
        let taker_pub = *coin.as_ref().priv_key_policy.key_pair_or_err().unwrap().public();
        let maker_pub = taker_pub;
        let dex_fee = DexFee::Standard(MmNumber::from((FEE_SAT, 100_000_000u64)));
        let args = GenTakerPaymentSpendArgs {
            taker_tx: &taker_payment,
            time_lock: TAKER_PAYMENT_TIME_LOCK as u64,
            maker_secret_hash: &MAKER_SECRET_HASH,
            maker_pub: &maker_pub,
            maker_address: &maker_addr,
            taker_pub: &taker_pub,
            dex_fee: &dex_fee,
            premium_amount: 0u64.into(),
            trading_amount: MmNumber::from((1u64, 100u64)).to_decimal(),
            dex_fee_addr_raw_pubkey: net_cfg.dex_fee_addr_raw_pubkey(),
        };
        let preimage = block_on(utxo_common::gen_taker_payment_spend_preimage(
            &coin,
            &args,
            SWAP_UNIQUE_DATA,
        ))
        .expect("preimage");
        {
            let signer = &preimage.preimage.0;
            assert_eq!(signer.outputs.len(), 1, "Standard preimage carries only the fee output");
            assert_eq!(signer.outputs[0].value, FEE_SAT);
        }
        // Taker signs SINGLE (0x03 with fork id 0).
        assert_eq!((3u32 | coin.as_ref().conf.fork_id) as u8, 0x03);

        NativeClient::send_transaction
            .mock_safe(|_, _| MockResult::Return(Box::new(futures01::future::ok(H256Json::default()))));
        let final_tx = block_on(utxo_common::sign_and_broadcast_taker_payment_spend(
            &coin,
            Some(&preimage),
            &args,
            &MAKER_SECRET,
            SWAP_UNIQUE_DATA,
        ))
        .expect("sign and broadcast");
        assert_eq!(
            final_tx.outputs.len(),
            2,
            "final tx has the fee output then the maker payout"
        );
        assert_eq!(final_tx.outputs[0].value, FEE_SAT);
        assert_eq!(final_tx.outputs[1].value, MAKER_VALUE);
        let expected_maker_script = output_script(&maker_addr, ScriptType::P2PKH).to_bytes();
        assert_eq!(final_tx.outputs[1].script_pubkey, expected_maker_script);

        // CRD ch.16 R15/R18 regression: the two signatures in the
        // cooperative script_sig MUST carry their own, different sighash
        // bytes for `Standard` -- maker ALL (0x01), taker SINGLE (0x03).
        // Before this fix both signatures shared one byte (the taker's, or
        // the maker's, whichever the code happened to reuse), which is
        // wire-incompatible with a reference peer.
        let script_sig: Script = final_tx.inputs[0].script_sig.clone().into();
        let instrs: Vec<_> = script_sig
            .iter()
            .collect::<Result<Vec<_>, _>>()
            .expect("script_sig parses");
        assert_eq!(
            *instrs[0].data.unwrap().last().unwrap(),
            0x01,
            "maker signature must carry the ALL byte"
        );
        assert_eq!(
            *instrs[1].data.unwrap().last().unwrap(),
            0x03,
            "taker signature must carry the SINGLE byte"
        );
    }

    /// CRD ch.16 T8 -- a burn output carrying a data push (the withdrawn
    /// pre-correction `OP_RETURN` encoding) MUST be rejected by R17: the
    /// bound R20 form is a bare opcode with no data push, and the maker's
    /// exact rebuild-and-compare has no tolerance for a different script.
    #[test]
    fn t8_v2_withburn_rejects_burn_output_with_data_push() {
        let coin = kmd_coin();
        let net_cfg = net_config_or_panic(8762);
        let taker_payment = synthetic_taker_payment_tx();
        let maker_addr = maker_address_for(&coin);
        let taker_pub = *coin.as_ref().priv_key_policy.key_pair_or_err().unwrap().public();
        let maker_pub = taker_pub;
        let dex_fee = DexFee::WithBurn {
            fee_amount: MmNumber::from("0.0075"),
            burn_amount: MmNumber::from("0.0025"),
            burn_destination: DexFeeBurnDestination::KmdOpReturn,
        };
        let args = GenTakerPaymentSpendArgs {
            taker_tx: &taker_payment,
            time_lock: TAKER_PAYMENT_TIME_LOCK as u64,
            maker_secret_hash: &MAKER_SECRET_HASH,
            maker_pub: &maker_pub,
            maker_address: &maker_addr,
            taker_pub: &taker_pub,
            dex_fee: &dex_fee,
            premium_amount: 0u64.into(),
            trading_amount: 1u64.into(),
            dex_fee_addr_raw_pubkey: net_cfg.dex_fee_addr_raw_pubkey(),
        };
        let mut preimage = block_on(utxo_common::gen_taker_payment_spend_preimage(
            &coin,
            &args,
            SWAP_UNIQUE_DATA,
        ))
        .expect("preimage");
        // Replace the bare OP_RETURN (R20) with the withdrawn, payload-carrying form.
        preimage.preimage.0.outputs[1].script_pubkey = Builder::default()
            .push_opcode(Opcode::OP_RETURN)
            .push_bytes(&250_000u64.to_le_bytes())
            .into_bytes();

        let res = block_on(utxo_common::validate_taker_payment_spend_preimage(
            &coin, &args, &preimage,
        ));
        let err = res.expect_err("a data-push OP_RETURN burn output must be rejected");
        match err.into_inner() {
            ValidateTakerPaymentSpendPreimageError::InvalidPreimage(_) => (),
            other => panic!("expected InvalidPreimage, got {:?}", other),
        }
    }

    /// CRD ch.16 T9 -- spend-fee estimate (R14A). A fixed rate of 1,000 base
    /// units per kB gives S = 496, not 1,000 (the legacy `get_htlc_spend_fee`
    /// rounds a 496-byte spend up to a whole kilobyte) and not the legacy
    /// 305-byte reference size's figure. A dynamic rate gives the same
    /// proportional formula, and the whole-kilobyte fixed policy
    /// (chapter 38 R38.6.6a) gives its rate `r`.
    #[test]
    fn t9_v2_spend_fee_estimate_is_proportional_not_rounded_up() {
        // Fixed-rate: the standard test fixture is FixedPerKb(1000).
        let fixed_coin = kmd_coin();
        let s = block_on(utxo_common::get_v2_swap_spend_fee(
            &fixed_coin,
            utxo_common::V2_SWAP_SPEND_TX_SIZE,
        ))
        .unwrap();
        assert_eq!(s, 496);
        assert_ne!(s, 1_000, "must not round a 496-byte spend up to a whole kilobyte");

        // Dynamic rate: same proportional formula.
        UtxoStandardCoin::get_tx_fee
            .mock_safe(|_| MockResult::Return(Box::pin(futures::future::ok(ActualTxFee::Dynamic(2_000)))));
        let dynamic_coin = doc_coin();
        let s_dynamic = block_on(utxo_common::get_v2_swap_spend_fee(
            &dynamic_coin,
            utxo_common::V2_SWAP_SPEND_TX_SIZE,
        ))
        .unwrap();
        assert_eq!(s_dynamic, (2_000 * 496) / 1000);

        // Whole-kilobyte fixed: the rate itself.
        UtxoStandardCoin::get_tx_fee
            .mock_safe(|_| MockResult::Return(Box::pin(futures::future::ok(ActualTxFee::FixedPerStartedKb(7_000)))));
        let whole_kb_coin = doc_coin();
        let s_whole_kb = block_on(utxo_common::get_v2_swap_spend_fee(
            &whole_kb_coin,
            utxo_common::V2_SWAP_SPEND_TX_SIZE,
        ))
        .unwrap();
        assert_eq!(s_whole_kb, 7_000);
    }
}

/// CRD ch.15 R16/R17 regression (issue #11 batch 2): the version-two funding
/// amount MUST be trading + premium + the dex-fee *total spend amount* (fee
/// + burn for `WithBurn`, zero for `NoFee`), not `fee_amount()` alone, which
/// silently omitted the burn leg. `send_taker_funding` (R16) and
/// `validate_taker_funding` (R17) share the formula.
///
/// This is a source-shape regression pin rather than an end-to-end test:
/// `send_taker_funding` broadcasts and `validate_taker_funding`'s
/// native-mode path issues a best-effort `import_address` RPC call, neither
/// of which this test harness mocks (same limitation the `t18`/`t16_4a`/
/// `t16_4b` tests in `dex_fee.rs` document for the V2 swap machines).
#[test]
fn ch15_r16_r17_funding_amount_uses_total_spend_amount_not_fee_amount() {
    let src = include_str!("utxo_common/utxo_common_swap.rs");
    assert_eq!(
        src.matches("&args.dex_fee.total_spend_amount().to_decimal()").count(),
        2,
        "send_taker_funding (R16) and validate_taker_funding (R17) must both use total_spend_amount()"
    );
    assert!(
        !src.contains("&args.dex_fee.fee_amount().to_decimal()"),
        "the funding-amount formula must not use fee_amount() alone (it silently omits the burn leg)"
    );
}

// ---- protocol.header_layout (CRD ch.37 R37.5.9) -------------------------------------

fn conf_with_header_layout(protocol: Json) -> UtxoConfResult<UtxoCoinConf> {
    let params = UtxoActivationParams::from_legacy_req(&json!({ "method": "enable" })).unwrap();
    let conf = json!({ "coin": "RIC", "protocol": protocol });
    UtxoConfBuilder::new(&conf, &params, "RIC").build()
}

#[test]
fn header_layout_absent_has_no_effect() {
    let conf = conf_with_header_layout(json!({ "type": "UTXO", "chain_variant": "BTC" })).unwrap();
    assert_eq!(conf.header_layout, None);
    // No header_layout key: the family's reader governs, here "BTC" (the standard reader).
    assert_eq!(HeaderReadPolicy::for_conf(&conf).layout(), CoinVariant::Standard);
}

#[test]
fn header_layout_values_are_read_beside_type_and_win_over_the_default() {
    for (value, expected) in [
        ("riecoin", CoinVariant::Riecoin),
        ("kawpow_auxpow_mixed", CoinVariant::KawpowAuxpowMixed),
    ] {
        let conf =
            conf_with_header_layout(json!({ "type": "UTXO", "chain_variant": "BTC", "header_layout": value })).unwrap();
        assert_eq!(conf.header_layout, Some(expected));
        assert_eq!(HeaderReadPolicy::for_conf(&conf).layout(), expected);
    }
}

#[test]
fn header_layout_unknown_value_fails_the_coin_build_naming_the_ticker() {
    for bad in [
        json!("foo"),
        json!("Riecoin"),
        json!("RIECOIN"),
        json!(""),
        json!(112),
        json!(null),
    ] {
        let err = conf_with_header_layout(json!({ "type": "UTXO", "header_layout": bad }))
            .expect_err("an unrecognized value must not fall back to any layout");
        let text = err.to_string();
        assert!(text.contains("RIC"), "{text}");
        assert!(text.contains("header_layout"), "{text}");
        assert!(matches!(err.into_inner(), UtxoConfError::InvalidHeaderLayout { .. }));
    }
}

#[test]
fn header_layout_does_not_change_how_the_protocol_object_parses() {
    let with = json!({ "type": "UTXO", "header_layout": "riecoin" });
    let without = json!({ "type": "UTXO" });
    let parse = |v: Json| json::to_value(json::from_value::<crate::CoinProtocol>(v).unwrap()).unwrap();
    assert_eq!(parse(with), parse(without));
}

/// The RIC tip list served by a public Electrum server, taken through the configured key to the
/// median: the layout of the coin entry plus the key, the list read, the median of parsed times.
#[test]
fn header_layout_ric_entry_yields_the_median_of_the_served_list() {
    let conf =
        conf_with_header_layout(json!({ "type": "UTXO", "chain_variant": "BTC", "header_layout": "riecoin" })).unwrap();
    let hex = include_str!("../../kdf_chain/src/test_data/ric_2658945_x11_v20000000.hex");
    let served = hex::decode(hex.trim()).unwrap();
    let variant = HeaderReadPolicy::for_conf(&conf).layout();
    let headers = chain::BlockHeader::list_from_served_bytes(&served, 11, variant).unwrap();
    let mut times: Vec<u32> = headers.into_iter().map(|h| h.time).collect();
    assert_eq!(common::median(times.as_mut_slice()), Some(1_792_892_599));
    // Without the key the same list is rejected: the pre-fix behavior.
    assert!(chain::BlockHeader::list_from_served_bytes(&served, 11, CoinVariant::Standard).is_err());
}

// ---- node switch header_layout_fallback_families (CRD ch.37 R37.5.9e) -------------------------

mod header_layout_fallback_tests {
    use super::*;
    use crate::utxo::header_layout_fallback::HeaderReadPolicy;
    use crate::utxo::header_layout_fallback::{read_with_fallback, FallbackReadError, HeaderLayoutFallback};
    use crate::utxo::utxo_builder::header_layout_fallback_families;
    use mm2_core::mm_ctx::MmCtxBuilder;
    use std::sync::atomic::Ordering;

    const RIC_LIST: &str = include_str!("../../kdf_chain/src/test_data/ric_2658945_x11_v20000000.hex");
    const RIC_LONE: &str = include_str!("../../kdf_chain/src/test_data/ric_2658955_lone_v20000000.hex");
    const MEWC_MIXED: &str = include_str!("../../kdf_chain/src/test_data/mewc_2124756_x11_9native_2auxpow.hex");
    const MEWC_TAIL: &str = include_str!("../../kdf_chain/src/test_data/mewc_2124727_x3_80_80_120.hex");
    const MEWC_ALL_NATIVE: &str = include_str!("../../kdf_chain/src/test_data/mewc_2124580_x11_all_native.hex");
    const MEWC_ALL_AUXPOW: &str = include_str!("../../kdf_chain/src/test_data/mewc_2124789_x6_all_auxpow.hex");
    const MEWC_LONE_NATIVE: &str = include_str!("../../kdf_chain/src/test_data/mewc_2124580_lone_native.hex");

    fn served(hex: &str) -> Vec<u8> { hex::decode(hex.trim()).unwrap() }

    fn ctx_with(conf: Json) -> mm2_core::mm_ctx::MmArc { MmCtxBuilder::new().with_conf(conf).into_mm_arc() }

    /// A conf built the way the activation builders build it: the node switch from `MM2.json`.
    fn conf(mm2_json: Json, protocol: Json) -> UtxoConfResult<UtxoCoinConf> {
        let families = header_layout_fallback_families(&ctx_with(mm2_json))?;
        let params = UtxoActivationParams::from_legacy_req(&json!({ "method": "enable" })).unwrap();
        let coin_conf = json!({ "coin": "RIC", "protocol": protocol });
        UtxoConfBuilder::new(&coin_conf, &params, "RIC")
            .with_header_layout_fallback_families(families)
            .build()
    }

    fn list_times(
        conf: &UtxoCoinConf,
        bytes: &[u8],
        count: usize,
    ) -> Result<u32, FallbackReadError<serialization::Error>> {
        let headers = HeaderReadPolicy::for_conf(conf).read_list(bytes, count)?;
        let mut times: Vec<u32> = headers.into_iter().map(|h| h.time).collect();
        Ok(common::median(times.as_mut_slice()).unwrap())
    }

    fn keyed_median(layout: CoinVariant, bytes: &[u8], count: usize) -> u32 {
        let mut times: Vec<u32> = chain::BlockHeader::list_from_served_bytes(bytes, count, layout)
            .unwrap()
            .into_iter()
            .map(|h| h.time)
            .collect();
        common::median(times.as_mut_slice()).unwrap()
    }

    #[test]
    fn switch_defaults_to_btc_and_accepts_only_distinct_chain_variant_names() {
        assert_eq!(header_layout_fallback_families(&ctx_with(json!({}))).unwrap(), vec![
            "BTC"
        ]);
        for (value, expected) in [
            (json!([]), vec![]),
            (json!(["BTC"]), vec!["BTC"]),
            (json!(["BTC", "PPC"]), vec!["BTC", "PPC"]),
        ] {
            let got = header_layout_fallback_families(&ctx_with(json!({ "header_layout_fallback_families": value })))
                .unwrap();
            assert_eq!(got, expected);
        }
    }

    #[test]
    fn switch_of_the_wrong_shape_is_a_configuration_error() {
        for bad in [
            json!("BTC"),
            json!(true),
            json!(1),
            json!(null),
            json!({}),
            json!(["BTC", 1]),
            json!(["btc"]),
            json!(["FOO"]),
            json!(["BTC", "BTC"]),
        ] {
            let ctx = ctx_with(json!({ "header_layout_fallback_families": bad.clone() }));
            let err = header_layout_fallback_families(&ctx).expect_err("a malformed switch must not default");
            let text = err.to_string();
            assert!(text.contains("header_layout_fallback_families"), "{text}");
            assert!(text.contains(&bad.to_string()), "{text}");
            assert!(
                matches!(err.into_inner(), UtxoConfError::InvalidHeaderLayoutFallbackFamilies(_)),
                "{bad}"
            );
        }
    }

    #[test]
    fn ric_and_mewc_lists_parse_without_the_key_under_the_default_switch() {
        let conf = conf(json!({}), json!({ "type": "UTXO", "chain_variant": "BTC" })).unwrap();
        assert_eq!(conf.header_layout, None);
        let ric = served(RIC_LIST);
        assert_eq!(list_times(&conf, &ric, 11).unwrap(), 1_792_892_599);
        assert_eq!(
            list_times(&conf, &ric, 11).unwrap(),
            keyed_median(CoinVariant::Riecoin, &ric, 11)
        );
        for (hex, count) in [(MEWC_MIXED, 11), (MEWC_TAIL, 3), (MEWC_ALL_NATIVE, 11)] {
            let bytes = served(hex);
            assert_eq!(
                list_times(&conf, &bytes, count).unwrap(),
                keyed_median(CoinVariant::KawpowAuxpowMixed, &bytes, count),
                "{count}"
            );
        }
        assert!(conf.header_layout_fallback_logged.load(Ordering::Relaxed));
    }

    #[test]
    fn lone_headers_use_the_same_cascade() {
        let conf = conf(json!({}), json!({ "type": "UTXO" })).unwrap();
        let fallback = HeaderLayoutFallback::for_conf(&conf);
        for hex in [RIC_LONE, MEWC_LONE_NATIVE] {
            let bytes = served(hex);
            assert!(chain::BlockHeader::from_served_bytes(&bytes, CoinVariant::Standard).is_err());
            let header = read_with_fallback(CoinVariant::Standard, fallback.as_ref(), |layout| {
                chain::BlockHeader::from_served_bytes(&bytes, layout)
            })
            .unwrap();
            assert_eq!(header.time, u32::from_le_bytes(bytes[68..72].try_into().unwrap()));
        }
    }

    #[test]
    fn empty_switch_rejects_the_same_lists() {
        let conf = conf(
            json!({ "header_layout_fallback_families": [] }),
            json!({ "type": "UTXO" }),
        )
        .unwrap();
        assert!(HeaderLayoutFallback::for_conf(&conf).is_none());
        assert!(list_times(&conf, &served(RIC_LIST), 11).is_err());
        assert!(list_times(&conf, &served(MEWC_MIXED), 11).is_err());
        assert!(list_times(&conf, &served(MEWC_ALL_NATIVE), 11).is_err());
        assert!(!conf.header_layout_fallback_logged.load(Ordering::Relaxed));
    }

    #[test]
    fn a_family_outside_the_set_is_not_probed() {
        let conf = conf(
            json!({ "header_layout_fallback_families": ["RVN"] }),
            json!({ "type": "UTXO" }),
        )
        .unwrap();
        assert!(HeaderLayoutFallback::for_conf(&conf).is_none());
        assert!(list_times(&conf, &served(RIC_LIST), 11).is_err());
    }

    #[test]
    fn a_genuine_80_byte_list_is_never_fallback_parsed() {
        let conf = conf(json!({}), json!({ "type": "UTXO" })).unwrap();
        let bytes = served(MEWC_ALL_AUXPOW);
        assert_eq!(bytes.len(), 6 * 80);
        // Count how often the cascade runs a non-standard layout.
        let probes = std::cell::Cell::new(0);
        let fallback = HeaderLayoutFallback::for_conf(&conf);
        let headers = read_with_fallback(CoinVariant::Standard, fallback.as_ref(), |layout| {
            if layout != CoinVariant::Standard {
                probes.set(probes.get() + 1);
            }
            chain::BlockHeader::list_from_served_bytes(&bytes, 6, layout)
        })
        .unwrap();
        assert_eq!(headers.len(), 6);
        assert_eq!(
            probes.get(),
            0,
            "the standard read succeeded, so no other layout may be tried"
        );
        assert!(
            !conf.header_layout_fallback_logged.load(Ordering::Relaxed),
            "no info line"
        );
    }

    #[test]
    fn a_pinned_layout_never_consults_the_fallback() {
        let conf = conf(
            json!({}),
            json!({ "type": "UTXO", "chain_variant": "BTC", "header_layout": "riecoin" }),
        )
        .unwrap();
        assert!(HeaderLayoutFallback::for_conf(&conf).is_none());
        // The pinned layout governs: the RIC list parses, a MEWC list does not, and nothing is logged.
        assert!(list_times(&conf, &served(RIC_LIST), 11).is_ok());
        assert!(list_times(&conf, &served(MEWC_MIXED), 11).is_err());
        assert!(!conf.header_layout_fallback_logged.load(Ordering::Relaxed));
    }

    #[test]
    fn a_list_no_layout_fits_propagates_the_original_rejection() {
        let conf = conf(json!({}), json!({ "type": "UTXO" })).unwrap();
        let mut bytes = served(RIC_LIST);
        bytes.push(0);
        let original = chain::BlockHeader::list_from_served_bytes(&bytes, 11, CoinVariant::Standard).unwrap_err();
        let err = list_times(&conf, &bytes, 11).unwrap_err();
        assert!(matches!(&err, FallbackReadError::Rejected(e) if format!("{e:?}") == format!("{original:?}")));
        assert!(!conf.header_layout_fallback_logged.load(Ordering::Relaxed));
    }

    /// Five headers, 560 bytes: read as 5 x 112 (Riecoin) and as 4 x 120 + 1 x 80 (mixed). The bytes
    /// are built so both layouts consume exactly: zero upper time halves at every 112-byte boundary,
    /// AuxPoW flag clear in the first four versions and set in the fifth.
    #[test]
    fn a_five_header_list_that_two_layouts_consume_is_ambiguous() {
        let mut bytes = vec![0u8; 560];
        bytes[481] = 0x01; // version of the 5th mixed header at 480: bit 8 set
        let conf = conf(json!({}), json!({ "type": "UTXO" })).unwrap();
        assert!(chain::BlockHeader::list_from_served_bytes(&bytes, 5, CoinVariant::Riecoin).is_ok());
        assert!(chain::BlockHeader::list_from_served_bytes(&bytes, 5, CoinVariant::KawpowAuxpowMixed).is_ok());
        assert!(chain::BlockHeader::list_from_served_bytes(&bytes, 5, CoinVariant::Standard).is_err());
        match list_times(&conf, &bytes, 5) {
            Err(FallbackReadError::Ambiguous(reason)) => {
                assert!(
                    reason.contains("riecoin") && reason.contains("kawpow_auxpow_mixed"),
                    "{reason}"
                );
            },
            other => panic!("expected an ambiguity, got {other:?}"),
        }
        assert!(!conf.header_layout_fallback_logged.load(Ordering::Relaxed));
        // Pinned, the same bytes parse.
        let pinned = self::conf(
            json!({}),
            json!({ "type": "UTXO", "header_layout": "kawpow_auxpow_mixed" }),
        )
        .unwrap();
        assert!(list_times(&pinned, &bytes, 5).is_ok());
    }

    #[test]
    fn length_collision_arithmetic_for_eleven_headers() {
        const COUNT: usize = 11;
        let standard = COUNT * 80;
        let riecoin = COUNT * 112;
        assert_eq!((standard, riecoin), (880, 1232));
        for natives in 0..=COUNT {
            // A mixed list of `natives` 120-byte headers and the rest 80-byte ones.
            let mixed = standard + 40 * natives;
            assert_ne!(mixed, riecoin, "k = {natives}");
        }
        // 1232 would need 8.8 native headers, so the Riecoin probe can never swallow a mixed list.
        assert_eq!((riecoin - standard) % 40, 32);
    }
}

/// CRD ch.60 T60.5, R60.6.4 and finding F3: every UTXO HTLC spend and refund builder refuses, without building
/// or broadcasting, when the spend fee is not smaller than the HTLC value.
#[cfg(not(target_arch = "wasm32"))]
mod spend_refusal_tests {
    use super::*;
    use crate::utxo::utxo_common::{build_and_broadcast_maker_v2_spend, build_funding_spend_preimage_tx,
                                   DEFAULT_SWAP_TX_SPEND_SIZE};
    use crate::{SwapOps, TransactionErr};
    use serialization::serialize;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    const SPEND_FEE: u64 = 496;
    const REFUSAL: &str = "is not smaller than the HTLC value";

    fn payment_tx_bytes(value: u64) -> Vec<u8> {
        let mut tx = UtxoTx::default();
        tx.outputs.push(TransactionOutput {
            value,
            script_pubkey: vec![0xa9; 23].into(),
        });
        serialize(&tx).take()
    }

    /// A KMD-like coin whose fixed policy makes the R38.6.6c spend fee 496, with `p2sh_spending_tx` mocked to
    /// record that a spend was built and to fail, so that no test touches the network.
    fn coin_with_spend_probe() -> (UtxoStandardCoin, Arc<AtomicBool>) {
        UtxoStandardCoin::get_tx_fee
            .mock_safe(|_| MockResult::Return(Box::pin(futures::future::ok(ActualTxFee::FixedPerKb(1000)))));
        let built = Arc::new(AtomicBool::new(false));
        let built_in_mock = built.clone();
        UtxoStandardCoin::p2sh_spending_tx.mock_safe(move |_, _, _, _, _, _, _, _| {
            built_in_mock.store(true, Ordering::SeqCst);
            MockResult::Return(Box::pin(futures::future::err(
                "mocked: the spend would be built".to_owned(),
            )))
        });
        (floor_test_coin("KMD", TxFee::FixedPerKb(1000), 1000, 8), built)
    }

    fn assert_refused(result: Result<TransactionEnum, TransactionErr>, built: &AtomicBool, what: &str) {
        match result {
            Err(TransactionErr::Plain(msg)) => assert!(msg.contains(REFUSAL), "{what}: unexpected refusal: {msg}"),
            other => panic!("{what}: expected an orderly refusal, got {other:?}"),
        }
        assert!(
            !built.load(Ordering::SeqCst),
            "{what}: a refused spend must not be built"
        );
    }

    fn assert_reaches_builder(result: Result<TransactionEnum, TransactionErr>, built: &AtomicBool, what: &str) {
        match result {
            Err(TransactionErr::Plain(msg)) => assert!(!msg.contains(REFUSAL), "{what}: refused: {msg}"),
            other => panic!("{what}: expected the mocked builder failure, got {other:?}"),
        }
        assert!(
            built.load(Ordering::SeqCst),
            "{what}: a spend above the fee must be built"
        );
    }

    #[test]
    fn t60_5_legacy_spends_and_refunds_refuse_a_value_not_above_the_fee() {
        assert_eq!(DEFAULT_SWAP_TX_SPEND_SIZE, 496);
        let other_pub = key_pair_from_seed("t60.5 counterparty").unwrap().public().to_vec();
        let privkey = key_pair_from_seed("t60.5 mine").unwrap().private().secret.to_vec();
        let secret = [7u8; 32];
        let secret_hash = [8u8; 20];

        type Spend = fn(&UtxoStandardCoin, &[u8], &[u8], &[u8], &[u8], &[u8]) -> crate::TransactionFut;
        let cases: [(&str, Spend); 4] = [
            ("taker spends maker payment", |c, tx, pubkey, secret, _hash, key| {
                c.send_taker_spends_maker_payment(tx, 100, pubkey, secret, key, &None)
            }),
            ("maker spends taker payment", |c, tx, pubkey, secret, _hash, key| {
                c.send_maker_spends_taker_payment(tx, 100, pubkey, secret, key, &None)
            }),
            ("taker refunds payment", |c, tx, pubkey, _secret, hash, key| {
                c.send_taker_refunds_payment(tx, 100, pubkey, hash, key, &None)
            }),
            ("maker refunds payment", |c, tx, pubkey, _secret, hash, key| {
                c.send_maker_refunds_payment(tx, 100, pubkey, hash, key, &None)
            }),
        ];
        for (what, spend) in cases {
            for value in [SPEND_FEE - 1, SPEND_FEE] {
                let (coin, built) = coin_with_spend_probe();
                let result = spend(
                    &coin,
                    &payment_tx_bytes(value),
                    &other_pub,
                    &secret,
                    &secret_hash,
                    &privkey,
                )
                .wait();
                assert_refused(result, &built, &format!("{what} at {value}"));
            }
            let (coin, built) = coin_with_spend_probe();
            let result = spend(
                &coin,
                &payment_tx_bytes(SPEND_FEE + 1),
                &other_pub,
                &secret,
                &secret_hash,
                &privkey,
            )
            .wait();
            assert_reaches_builder(result, &built, &format!("{what} at {}", SPEND_FEE + 1));
        }
    }

    /// T60.6 (a), (c) and R60.6.7 item 6: the UTXO safe amount is the R38.6.6c spend-fee estimate at the moment of
    /// the call plus the dust; an unavailable estimate is a typed error, not a placeholder.
    #[test]
    fn t60_6_utxo_safe_amount_is_the_spend_fee_plus_dust() {
        use crate::{MmCoin, SpendSafeAmountError};
        let cases = [
            // (a) dynamic rate 20,000, dust 1,000: 9,920 + 1,000.
            (ActualTxFee::Dynamic(20_000), 1_000, "0.0001092"),
            // (c) KMD, plain fixed 1,000: 496 + 1,000.
            (ActualTxFee::FixedPerKb(1_000), 1_000, "0.00001496"),
            (ActualTxFee::FixedPerStartedKb(1_000), 1_000, "0.00002"),
        ];
        for (policy, dust, expected) in cases {
            UtxoStandardCoin::get_tx_fee.mock_safe(move |_| MockResult::Return(Box::pin(futures::future::ok(policy))));
            let coin = floor_test_coin("KMD", TxFee::FixedPerKb(1000), dust, 8);
            let safe = block_on(coin.spend_safe_amount()).unwrap();
            assert_eq!(safe, Some(MmNumber::from(expected)), "{policy:?}");
        }

        UtxoStandardCoin::get_tx_fee.mock_safe(|_| {
            MockResult::Return(Box::pin(futures::future::err(
                UtxoRpcError::Internal("no estimate".to_owned()).into(),
            )))
        });
        let coin = floor_test_coin("BTC", TxFee::Dynamic(EstimateFeeMethod::Standard), 1000, 8);
        assert!(matches!(
            block_on(coin.spend_safe_amount()).map_err(|e| e.into_inner()),
            Err(SpendSafeAmountError::EstimateUnavailable(_))
        ));
    }

    /// The version-two maker-payment spend and refund builders share one helper.
    #[test]
    fn t60_5_v2_maker_payment_spend_refuses_a_value_not_above_the_fee() {
        let kp = key_pair_from_seed("t60.5 v2").unwrap();
        for value in [SPEND_FEE - 1, SPEND_FEE] {
            let (coin, built) = coin_with_spend_probe();
            let result = block_on(build_and_broadcast_maker_v2_spend(
                &coin,
                &payment_tx_bytes(value),
                Script::new(Bytes::new()),
                Script::new(Bytes::new()),
                0,
                0,
                &kp,
            ))
            .map(TransactionEnum::from);
            assert_refused(result, &built, &format!("v2 maker payment spend at {value}"));
        }
        let (coin, built) = coin_with_spend_probe();
        let result = block_on(build_and_broadcast_maker_v2_spend(
            &coin,
            &payment_tx_bytes(SPEND_FEE + 1),
            Script::new(Bytes::new()),
            Script::new(Bytes::new()),
            0,
            0,
            &kp,
        ))
        .map(TransactionEnum::from);
        assert_reaches_builder(result, &built, "v2 maker payment spend above the fee");
    }

    /// CRD ch.15 R21 step 2: the funding value shall be at least the spend fee plus the coin's dust.
    #[test]
    fn t60_5_v2_funding_spend_requires_fee_plus_dust() {
        let fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(native_client_for_test()), None, false);
        let dust = fields.dust_amount;
        assert_eq!(dust, 1000);
        let redeem = Script::new(Bytes::new());
        let funding_with = |value: u64| {
            let mut tx = UtxoTx::default();
            tx.outputs.push(TransactionOutput {
                value,
                script_pubkey: vec![0xaa; 23].into(),
            });
            tx
        };
        let at_minimum = SPEND_FEE + dust;
        assert!(matches!(
            build_funding_spend_preimage_tx(&fields, &funding_with(at_minimum - 1), &redeem, SPEND_FEE),
            Err(crate::TxGenError::PrevOutputTooLow)
        ));
        let signer = build_funding_spend_preimage_tx(&fields, &funding_with(at_minimum), &redeem, SPEND_FEE)
            .expect("fee plus dust is enough");
        assert_eq!(signer.outputs[0].value, dust);
    }
}

// ---- the layout on every header read path (CRD ch.37 R37.5.1a, R37.5.9a, R37.5.9e) ---------------

/// Plumbing coverage: each production read path is driven with a Riecoin and a mixed Meowcoin
/// header (list), from a pinned coin, from an unpinned coin under the default switch, from an
/// unpinned coin with the switch off, from a pinned coin whose layout does not fit and with a plain
/// standard-80 header. The Electrum fetches are mocked, so nothing touches the network.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod header_read_path_tests {
    use super::*;
    use crate::utxo::header_layout_fallback::HeaderReadPolicy;
    use crate::utxo::rpc_clients::ElectrumBlockHeadersRes;
    use crate::utxo::utxo_block_header_storage::{BlockHeaderStorage, BlockHeaderStorageOps};
    use crate::utxo::utxo_common::{block_header_from_storage_or_rpc, valid_block_header_from_storage,
                                   verify_anchor_header};
    use crate::utxo::utxo_sql_block_header_storage::SqliteBlockHeadersStorage;
    use crate::utxo::{SPVBlockHeader, SPVConf};
    use rpc::v1::types::Bytes as BytesJson;
    use std::sync::atomic::Ordering;

    const RIC_LIST: &str = include_str!("../../kdf_chain/src/test_data/ric_2658945_x11_v20000000.hex");
    const RIC_LONE: &str = include_str!("../../kdf_chain/src/test_data/ric_2658955_lone_v20000000.hex");
    const MEWC_MIXED: &str = include_str!("../../kdf_chain/src/test_data/mewc_2124756_x11_9native_2auxpow.hex");
    const MEWC_LONE_NATIVE: &str = include_str!("../../kdf_chain/src/test_data/mewc_2124580_lone_native.hex");
    const MEWC_ALL_AUXPOW: &str = include_str!("../../kdf_chain/src/test_data/mewc_2124789_x6_all_auxpow.hex");
    const MEWC_ALL_NATIVE: &str = include_str!("../../kdf_chain/src/test_data/mewc_2124580_x11_all_native.hex");

    const TICKER: &str = "PLUMB";

    fn served(hex: &str) -> Vec<u8> { hex::decode(hex.trim()).unwrap() }

    pub(crate) const PIVX_FILES: [(&str, &str); 9] = [
        (
            "v3",
            include_str!("../../kdf_chain/src/test_data/pivx_100000_x11_v3.hex"),
        ),
        (
            "v3_to_v4",
            include_str!("../../kdf_chain/src/test_data/pivx_863780_x11_v3_to_v4.hex"),
        ),
        (
            "v4",
            include_str!("../../kdf_chain/src/test_data/pivx_1500000_x11_v4.hex"),
        ),
        (
            "v5",
            include_str!("../../kdf_chain/src/test_data/pivx_1900000_x11_v5.hex"),
        ),
        (
            "v6_to_v7",
            include_str!("../../kdf_chain/src/test_data/pivx_2153195_x11_v6_to_v7.hex"),
        ),
        (
            "v7",
            include_str!("../../kdf_chain/src/test_data/pivx_2400000_x11_v7.hex"),
        ),
        (
            "v7_to_v8",
            include_str!("../../kdf_chain/src/test_data/pivx_2700495_x11_v7_to_v8.hex"),
        ),
        (
            "v10_v11",
            include_str!("../../kdf_chain/src/test_data/pivx_3000000_x11_v10_v11.hex"),
        ),
        (
            "tip",
            include_str!("../../kdf_chain/src/test_data/pivx_5642858_x11_v11_tip.hex"),
        ),
    ];
    pub(crate) const LBC_FILES: [(&str, &str); 4] = [
        ("lbc_1", include_str!("../../kdf_chain/src/test_data/lbc_1_x11.hex")),
        (
            "lbc_500000",
            include_str!("../../kdf_chain/src/test_data/lbc_500000_x11.hex"),
        ),
        (
            "lbc_2000000",
            include_str!("../../kdf_chain/src/test_data/lbc_2000000_x11.hex"),
        ),
        (
            "lbc_tip",
            include_str!("../../kdf_chain/src/test_data/lbc_2209626_x11_tip.hex"),
        ),
    ];
    const RUNES_TIP: &str = include_str!("../../kdf_chain/src/test_data/runes_2953197_x11_qtum_tip.hex");
    const RUNES_500K: &str = include_str!("../../kdf_chain/src/test_data/runes_500000_x11_qtum.hex");

    fn time_at(bytes: &[u8], start: usize) -> u32 { time_at_offset(bytes, start, 68) }

    fn time_at_offset(bytes: &[u8], start: usize, offset: usize) -> u32 {
        u32::from_le_bytes(bytes[start + offset..start + offset + 4].try_into().unwrap())
    }

    /// The size of the PIVX header at the start of `bytes`, stated from its version field by the
    /// chapter's rule (4-6 and 8 and above carry the 32-byte field), independent of the reader.
    pub(crate) fn pivx_size(bytes: &[u8]) -> usize {
        match u32::from_le_bytes(bytes[..4].try_into().unwrap()) {
            4..=6 | 8.. => 112,
            _ => 80,
        }
    }

    /// Size of a Qtum header with a one-byte compact-size signature.
    fn qtum_size(bytes: &[u8]) -> usize { 181 + usize::from(bytes[180]) }

    pub(crate) fn split_by(bytes: &[u8], size: fn(&[u8]) -> usize) -> Vec<&[u8]> {
        let (mut out, mut rest) = (Vec::new(), bytes);
        while !rest.is_empty() {
            let (head, tail) = rest.split_at(size(rest));
            out.push(head);
            rest = tail;
        }
        out
    }

    /// One served chain section: a lone header and a list, both in the layout `layout`.
    pub(crate) struct Fixture {
        pub(crate) layout: CoinVariant,
        pub(crate) lone: Vec<u8>,
        pub(crate) list: Vec<u8>,
        pub(crate) count: usize,
        /// Where the time field sits in a header of this layout.
        pub(crate) time_offset: usize,
    }

    impl Fixture {
        pub(crate) fn riecoin() -> Self {
            Fixture {
                layout: CoinVariant::Riecoin,
                lone: served(RIC_LONE),
                list: served(RIC_LIST),
                count: 11,
                time_offset: 68,
            }
        }

        pub(crate) fn mewc() -> Self {
            Fixture {
                layout: CoinVariant::KawpowAuxpowMixed,
                lone: served(MEWC_LONE_NATIVE),
                list: served(MEWC_MIXED),
                count: 11,
                time_offset: 68,
            }
        }

        /// Eleven Qtum proof-of-stake headers (246 bytes each): the standard 80 bytes, the state and UTXO
        /// roots, the stake outpoint and a 65-byte compact-size-prefixed block signature. Built from the
        /// layout of `kdf_chain`'s Qtum reader (no real Qtum list is in the tree); the version value is the
        /// one Qtum shares with plain BIP9-signalling chains, so only the Qtum reader consumes the bytes.
        pub(crate) fn qtum() -> Self {
            let headers: Vec<Vec<u8>> = (0..11u32)
                .map(|i| {
                    let mut h = Vec::with_capacity(246);
                    h.extend_from_slice(&0x2000_0000u32.to_le_bytes());
                    h.extend_from_slice(&[i as u8 + 1; 32]); // previous hash
                    h.extend_from_slice(&[0x5a; 32]); // merkle root
                    h.extend_from_slice(&(1_700_000_000 + 60 * i).to_le_bytes());
                    h.extend_from_slice(&0x1b00_ffffu32.to_le_bytes());
                    h.extend_from_slice(&i.to_le_bytes()); // nonce
                    h.extend_from_slice(&[0x66; 32]); // hash_state_root
                    h.extend_from_slice(&[0x77; 32]); // hash_utxo_root
                    h.extend_from_slice(&[0x88; 36]); // prevout_stake
                    h.push(65); // compact-size length of the block signature
                    h.extend_from_slice(&[0x99; 65]);
                    h
                })
                .collect();
            Fixture {
                layout: CoinVariant::Qtum,
                lone: headers[0].clone(),
                list: headers.concat(),
                count: 11,
                time_offset: 68,
            }
        }

        /// Eleven 120-byte KawPoW headers: the real native Meowcoin headers of `MEWC_ALL_NATIVE`
        /// (height 2124580 and the ten before it) with the version field rewritten to the KawPoW
        /// signalling value 0x30000000 that the version-signalled reader and `"RVN"` key on. The bodies
        /// (previous hash, Merkle root, time, bits, height, 64-bit nonce, mix hash) are the chain's own.
        pub(crate) fn kawpow() -> Self {
            let mut list = served(MEWC_ALL_NATIVE);
            assert_eq!(list.len(), 11 * 120);
            for header in list.chunks_mut(120) {
                header[..4].copy_from_slice(&0x3000_0000u32.to_le_bytes());
            }
            Fixture {
                layout: CoinVariant::Standard,
                lone: list[..120].to_vec(),
                count: 11,
                list,
                time_offset: 68,
            }
        }

        pub(crate) fn standard() -> Self {
            let list = served(MEWC_ALL_AUXPOW);
            Fixture {
                layout: CoinVariant::Standard,
                lone: list[..80].to_vec(),
                count: 6,
                list,
                time_offset: 68,
            }
        }

        /// A real PIVX window; the lone header is its first header.
        pub(crate) fn pivx(hex: &str) -> Self {
            let list = served(hex);
            let lone = split_by(&list, pivx_size)[0].to_vec();
            Fixture {
                layout: CoinVariant::Pivx,
                lone,
                count: 11,
                list,
                time_offset: 68,
            }
        }

        /// A real LBC window (eleven 112-byte headers); the lone header is its first header.
        pub(crate) fn lbc(hex: &str) -> Self {
            let list = served(hex);
            Fixture {
                layout: CoinVariant::Lbc,
                lone: list[..112].to_vec(),
                count: 11,
                list,
                time_offset: 100,
            }
        }

        /// A real Runebase window (Qtum-shaped headers); the lone header is its first header.
        pub(crate) fn runes(hex: &str) -> Self {
            let list = served(hex);
            let lone = split_by(&list, qtum_size)[0].to_vec();
            Fixture {
                layout: CoinVariant::Qtum,
                lone,
                count: 11,
                list,
                time_offset: 68,
            }
        }

        /// The median of the eleven time fields, read straight from the fixture bytes.
        pub(crate) fn independent_median(&self, size: fn(&[u8]) -> usize) -> u64 {
            let mut times: Vec<u32> = split_by(&self.list, size)
                .iter()
                .map(|piece| time_at_offset(piece, 0, self.time_offset))
                .collect();
            times.sort_unstable();
            times[times.len() / 2] as u64
        }

        pub(crate) fn lone_time(&self) -> u64 { time_at_offset(&self.lone, 0, self.time_offset) as u64 }

        pub(crate) fn list_time(&self) -> u64 { time_at_offset(&self.list, 0, self.time_offset) as u64 }

        /// The median the median-time-past path derives from the whole list, stated from the parse
        /// under the fixture's true layout.
        pub(crate) fn median(&self) -> u64 {
            let mut times: Vec<u32> = chain::BlockHeader::list_from_served_bytes(&self.list, self.count, self.layout)
                .unwrap()
                .into_iter()
                .map(|h| h.time)
                .collect();
            common::median(times.as_mut_slice()).unwrap() as u64
        }
    }

    #[derive(Clone, Copy)]
    pub(crate) enum Setup {
        /// `protocol.header_layout` set.
        Pinned(CoinVariant),
        /// No key, the default node switch.
        Default,
        /// No key, `header_layout_fallback_families: []`.
        Off,
    }

    /// A coin on a mocked Electrum client with a SQLite header store, both holding the policy of the
    /// setup, exactly as the activation builders derive it from the conf.
    pub(crate) fn coin(setup: Setup) -> UtxoStandardCoin {
        coin_with(|fields| match setup {
            Setup::Pinned(layout) => fields.conf.header_layout = Some(layout),
            Setup::Default => (),
            Setup::Off => fields.conf.header_layout_fallback_families = vec![],
        })
    }

    /// As [`coin`], with the conf built by `UtxoConfBuilder` from a coin JSON whose `protocol` object is
    /// `protocol`, under the node switch `families` (`None` is the default switch). The test proves the
    /// builder reads the keys; nothing pokes a conf field.
    pub(crate) fn coin_from_protocol(protocol: Json, families: Option<&[&str]>) -> UtxoStandardCoin {
        let conf = conf_from_protocol(protocol, families).unwrap();
        coin_with(|fields| fields.conf = conf)
    }

    pub(crate) fn conf_from_protocol(protocol: Json, families: Option<&[&str]>) -> UtxoConfResult<UtxoCoinConf> {
        let params = UtxoActivationParams::from_legacy_req(&json!({ "method": "enable" })).unwrap();
        let coin_conf = json!({ "coin": TICKER, "protocol": protocol });
        let mut builder = UtxoConfBuilder::new(&coin_conf, &params, TICKER);
        if let Some(families) = families {
            builder = builder.with_header_layout_fallback_families(families.iter().map(|f| (*f).to_owned()).collect());
        }
        builder.build()
    }

    fn coin_with(configure: impl FnOnce(&mut UtxoCoinFields)) -> UtxoStandardCoin {
        let client = ElectrumClient(Arc::new(ElectrumClientImpl::new(TICKER.into(), Default::default())));
        let mut fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Electrum(client), None, false);
        fields.conf.ticker = TICKER.into();
        configure(&mut fields);
        let policy = HeaderReadPolicy::for_conf(&fields.conf);
        let storage = BlockHeaderStorage {
            inner: Box::new(SqliteBlockHeadersStorage::in_memory_with_policy(policy)),
            conf: SPVConf {
                starting_block_header: SPVBlockHeader {
                    height: 0,
                    hash: "00".repeat(32),
                    time: 0,
                    bits: 0,
                },
                max_stored_block_headers: None,
                validation_params: None,
            },
        };
        block_on(storage.init(TICKER)).unwrap();
        fields.block_headers_storage = Some(storage);
        utxo_coin_from_fields(fields)
    }

    pub(crate) fn policy_of(coin: &UtxoStandardCoin) -> HeaderReadPolicy {
        HeaderReadPolicy::for_conf(&coin.as_ref().conf)
    }

    pub(crate) fn logged(coin: &UtxoStandardCoin) -> bool {
        coin.as_ref().conf.header_layout_fallback_logged.load(Ordering::Relaxed)
    }

    pub(crate) fn electrum(coin: &UtxoStandardCoin) -> ElectrumClient {
        match &coin.as_ref().rpc_client {
            UtxoRpcClientEnum::Electrum(client) => client.clone(),
            _ => unreachable!("the test coin is Electrum-served"),
        }
    }

    /// Makes `blockchain.block.header` serve `bytes` and `blockchain.block.headers` serve `list`.
    pub(crate) fn serve(lone: &[u8], list: &[u8], count: u64) {
        let lone = lone.to_vec();
        ElectrumClient::blockchain_block_header
            .mock_safe(move |_, _| MockResult::Return(Box::new(futures01::future::ok(BytesJson::from(lone.clone())))));
        let list = list.to_vec();
        ElectrumClient::blockchain_block_headers.mock_safe(move |_, _, _| {
            let res: ElectrumBlockHeadersRes =
                json::from_value(json!({ "count": count, "hex": hex::encode(&list), "max": 2016 })).unwrap();
            MockResult::Return(Box::new(futures01::future::ok(res)))
        });
    }

    /// Drives one read path through the five situations. `read` returns a value the path derives from
    /// the served bytes; `expected` states it independently of the parser.
    pub(crate) fn run_matrix(
        read: impl Fn(&UtxoStandardCoin, &Fixture) -> Result<u64, String>,
        expected: impl Fn(&Fixture) -> u64,
    ) {
        run_matrix_with(false, read, expected)
    }

    /// As [`run_matrix`]; `by_size` is set for the median path, which reads a list its layout rejects
    /// by the size of its headers (CRD ch.37 R37.5.10) instead of rejecting it.
    pub(crate) fn run_matrix_with(
        by_size: bool,
        read: impl Fn(&UtxoStandardCoin, &Fixture) -> Result<u64, String>,
        expected: impl Fn(&Fixture) -> u64,
    ) {
        let (ric, mewc, std80) = (Fixture::riecoin(), Fixture::mewc(), Fixture::standard());

        // (a) a pinned coin reads its layout and never logs a fallback.
        for fx in [&ric, &mewc] {
            let coin = coin(Setup::Pinned(fx.layout));
            assert_eq!(read(&coin, fx), Ok(expected(fx)), "pinned {:?}", fx.layout);
            assert!(!logged(&coin), "a pinned coin must not be fallback-parsed");
        }
        // (b) no key, default switch: the fallback reads both, and logs once per coin.
        for fx in [&ric, &mewc] {
            let coin = coin(Setup::Default);
            assert_eq!(read(&coin, fx), Ok(expected(fx)), "fallback {:?}", fx.layout);
            assert!(logged(&coin));
        }
        // (c) no key, switch off: rejected as before and nothing is logged. The median path is the
        // exception (CRD ch.37 R37.5.10f): with the switch off a list the standard layout rejects is read
        // by the size of its headers, so `ric` yields the true median by the even split.
        for fx in [&ric, &mewc] {
            let coin = coin(Setup::Off);
            if by_size {
                if fx.layout == CoinVariant::Riecoin {
                    assert_eq!(read(&coin, fx), Ok(expected(fx)), "switch off {:?}", fx.layout);
                }
            } else {
                assert!(read(&coin, fx).is_err(), "switch off {:?}", fx.layout);
            }
            assert!(!logged(&coin));
        }
        // (d) a pinned coin whose layout does not fit is rejected without the fallback (the median
        // path again reads the times by size).
        for (pinned, fx) in [(CoinVariant::Riecoin, &mewc), (CoinVariant::KawpowAuxpowMixed, &ric)] {
            let coin = coin(Setup::Pinned(pinned));
            if by_size {
                if fx.layout == CoinVariant::Riecoin {
                    assert_eq!(read(&coin, fx), Ok(expected(fx)), "pinned {pinned:?} {:?}", fx.layout);
                }
            } else {
                assert!(
                    read(&coin, fx).is_err(),
                    "pinned {pinned:?} must reject {:?}",
                    fx.layout
                );
            }
            assert!(!logged(&coin), "a pinned coin must not consult the fallback");
        }
        // (e) a plain standard-80 header is read by the standard layout and never fallback-parsed.
        for setup in [Setup::Default, Setup::Off] {
            let coin = coin(setup);
            assert_eq!(read(&coin, &std80), Ok(expected(&std80)));
            assert!(!logged(&coin));
        }
    }

    fn describe<E: std::fmt::Debug>(e: E) -> String { format!("{e:?}") }

    pub(crate) type PathFn = fn(&UtxoStandardCoin, &Fixture) -> Result<u64, String>;

    fn path_retrieve_last_headers(coin: &UtxoStandardCoin, fx: &Fixture) -> Result<u64, String> {
        serve(&fx.lone, &fx.list, fx.count as u64);
        let limit = NonZeroU64::new(fx.count as u64).unwrap();
        let (registry, headers) = electrum(coin)
            .retrieve_last_headers(limit, 5000, policy_of(coin))
            .wait()
            .map_err(describe)?;
        let from = 5000 - fx.count as u64;
        assert_eq!(headers.len(), fx.count);
        assert_eq!(registry.len(), fx.count);
        assert_eq!(registry[&from].raw().len(), headers[0].raw().len());
        let reencoded: Vec<u8> = headers.iter().flat_map(|h| h.raw().to_vec()).collect();
        assert_eq!(reencoded, fx.list, "the registry keeps the served bytes");
        Ok(registry[&from].time as u64)
    }

    fn path_get_block_timestamp(coin: &UtxoStandardCoin, fx: &Fixture) -> Result<u64, String> {
        serve(&fx.lone, &fx.list, fx.count as u64);
        block_on(
            coin.as_ref()
                .rpc_client
                .get_block_timestamp_with_policy(77, &policy_of(coin)),
        )
        .map_err(describe)
    }

    fn path_valid_block_header_from_storage(coin: &UtxoStandardCoin, fx: &Fixture) -> Result<u64, String> {
        serve(&fx.lone, &fx.list, fx.count as u64);
        let storage = coin.as_ref().block_headers_storage.as_ref().unwrap();
        let client = electrum(coin);
        let header = block_on(valid_block_header_from_storage(coin, 5000, storage, &client)).map_err(describe)?;
        assert_eq!(header.raw().len(), fx.lone.len());
        Ok(header.time as u64)
    }

    fn path_direct_fetch(coin: &UtxoStandardCoin, fx: &Fixture) -> Result<u64, String> {
        serve(&fx.lone, &fx.list, fx.count as u64);
        let client = electrum(coin);
        block_on(block_header_from_storage_or_rpc(coin, 5000, &None, &client))
            .map(|header| header.time as u64)
            .map_err(describe)
    }

    fn path_verify_anchor_header(coin: &UtxoStandardCoin, fx: &Fixture) -> Result<u64, String> {
        serve(&fx.lone, &fx.list, fx.count as u64);
        // The anchor states what the true layout parses the served header to.
        let truth = chain::BlockHeader::from_served_bytes(&fx.lone, fx.layout).unwrap();
        let bits = match truth.bits {
            chain::BlockHeaderBits::Compact(compact) => u32::from(compact),
            chain::BlockHeaderBits::U32(bits) => bits,
        };
        let anchor = SPVBlockHeader {
            height: 77,
            hash: truth.hash().reversed().to_string(),
            time: truth.time,
            bits,
        };
        let client = electrum(coin);
        match block_on(verify_anchor_header(&client, &anchor, &policy_of(coin))) {
            Ok(true) => Ok(fx.lone_time()),
            other => Err(format!("{other:?}")),
        }
    }

    /// The median-time-past path, through the coin's own `get_current_mtp`.
    fn path_median(coin: &UtxoStandardCoin, fx: &Fixture) -> Result<u64, String> {
        serve(&fx.lone, &fx.list, fx.count as u64);
        ElectrumClient::get_block_count.mock_safe(|_| MockResult::Return(Box::new(futures01::future::ok(5000u64))));
        block_on(coin.get_current_mtp()).map(u64::from).map_err(describe)
    }

    /// Stores the fixture's headers in the coin's store (which holds the coin's policy) and reads
    /// them back through the store's two decode paths.
    fn path_store(coin: &UtxoStandardCoin, fx: &Fixture) -> Result<u64, String> {
        let storage = coin.as_ref().block_headers_storage.as_ref().unwrap();
        // The headers are stored the way the sync loop stores them: `header.raw()` of the parse
        // under the true layout of the chain.
        let headers = chain::BlockHeader::list_from_served_bytes(&fx.list, fx.count, fx.layout).unwrap();
        let registry: HashMap<u64, chain::BlockHeader> =
            headers.iter().cloned().zip(100u64..).map(|(h, n)| (n, h)).collect();
        block_on(storage.add_block_headers_to_storage(TICKER, registry)).map_err(describe)?;
        for (height, expected) in (100u64..).zip(&headers) {
            let read = block_on(storage.get_block_header(TICKER, height))
                .map_err(describe)?
                .expect("stored");
            assert_eq!(
                read.raw(),
                expected.raw(),
                "the store re-reads the served bytes at {height}"
            );
        }
        // The scan-based lookup decodes every stored row.
        let found = block_on(storage.get_block_height_by_hash(TICKER, headers[0].hash())).map_err(describe)?;
        assert_eq!(found, Some(100));
        Ok(headers[0].time as u64)
    }

    /// Every threaded read path of the coin layer that a test can drive here, with the value each
    /// derives from the served bytes (the WalletConnect genesis read is driven from its own module).
    pub(crate) fn paths() -> Vec<(&'static str, PathFn, fn(&Fixture) -> u64)> {
        vec![
            ("retrieve_last_headers", path_retrieve_last_headers, |fx| fx.list_time()),
            ("get_block_timestamp", path_get_block_timestamp, |fx| fx.lone_time()),
            (
                "valid_block_header_from_storage",
                path_valid_block_header_from_storage,
                |fx| fx.lone_time(),
            ),
            ("block_header_from_storage_or_rpc", path_direct_fetch, |fx| {
                fx.lone_time()
            }),
            ("verify_anchor_header", path_verify_anchor_header, |fx| fx.lone_time()),
            ("median", path_median, |fx| fx.median()),
            ("sqlite_store", path_store, |fx| fx.list_time()),
        ]
    }

    #[test]
    fn retrieve_last_headers_reads_the_list_under_the_policy() {
        run_matrix(path_retrieve_last_headers, |fx| fx.list_time());
    }

    #[test]
    fn retrieve_last_headers_rejects_a_count_mismatch() {
        for setup in [Setup::Pinned(CoinVariant::Riecoin), Setup::Default] {
            let coin = coin(setup);
            let fx = Fixture::riecoin();
            let limit = NonZeroU64::new(11).unwrap();
            for wrong in [10u64, 12] {
                serve(&fx.lone, &fx.list, wrong);
                assert!(electrum(&coin)
                    .retrieve_last_headers(limit, 5000, policy_of(&coin))
                    .wait()
                    .is_err());
            }
            // The correct count is read.
            serve(&fx.lone, &fx.list, 11);
            assert!(electrum(&coin)
                .retrieve_last_headers(limit, 5000, policy_of(&coin))
                .wait()
                .is_ok());
        }
    }

    #[test]
    fn get_block_timestamp_reads_the_header_under_the_policy() {
        run_matrix(path_get_block_timestamp, |fx| fx.lone_time());
    }

    #[test]
    fn the_standard_get_block_timestamp_is_unchanged() {
        let coin = coin(Setup::Pinned(CoinVariant::Riecoin));
        let (ric, std80) = (Fixture::riecoin(), Fixture::standard());
        serve(&ric.lone, &ric.list, 11);
        assert!(block_on(coin.as_ref().rpc_client.get_block_timestamp(77)).is_err());
        serve(&std80.lone, &std80.list, 6);
        assert_eq!(
            block_on(coin.as_ref().rpc_client.get_block_timestamp(77)).unwrap(),
            std80.lone_time()
        );
    }

    #[test]
    fn valid_block_header_from_storage_reads_the_single_header_under_the_policy() {
        run_matrix(path_valid_block_header_from_storage, |fx| fx.lone_time());
    }

    #[test]
    fn block_header_from_storage_or_rpc_reads_the_direct_fetch_under_the_policy() {
        run_matrix(path_direct_fetch, |fx| fx.lone_time());
    }

    #[test]
    fn verify_anchor_header_reads_the_anchor_under_the_policy() {
        run_matrix(path_verify_anchor_header, |fx| fx.lone_time());
    }

    #[test]
    fn the_median_path_reads_the_list_under_the_policy() { run_matrix_with(true, path_median, |fx| fx.median()); }

    #[test]
    fn the_sqlite_store_rereads_stored_headers_under_the_policy() { run_matrix(path_store, |fx| fx.list_time()); }

    #[test]
    fn the_store_keeps_112_120_and_80_byte_headers() {
        let coin = coin(Setup::Default);
        let storage = coin.as_ref().block_headers_storage.as_ref().unwrap();
        let ric = chain::BlockHeader::from_served_bytes(&Fixture::riecoin().lone, CoinVariant::Riecoin).unwrap();
        let mixed =
            chain::BlockHeader::list_from_served_bytes(&Fixture::mewc().list, 11, CoinVariant::KawpowAuxpowMixed)
                .unwrap();
        let native = mixed.iter().find(|h| h.raw().len() == 120).unwrap().clone();
        let merged = mixed.iter().find(|h| h.raw().len() == 80).unwrap().clone();
        assert_eq!(ric.raw().len(), 112);
        let registry: HashMap<u64, chain::BlockHeader> = [(1, ric.clone()), (2, native.clone()), (3, merged.clone())]
            .into_iter()
            .collect();
        block_on(storage.add_block_headers_to_storage(TICKER, registry)).unwrap();
        for (height, header) in [(1, ric), (2, native), (3, merged)] {
            let read = block_on(storage.get_block_header(TICKER, height)).unwrap().unwrap();
            assert_eq!(read.raw(), header.raw());
        }
        assert!(logged(&coin), "the 112- and 120-byte rows were read by the fallback");
    }

    // ---- protocol.chain_variant read at activation (CRD ch.37 R37.5.1, R37.5.9a, R37.5.9e) ----------

    fn utxo(chain_variant: Json) -> Json { json!({ "type": "UTXO", "chain_variant": chain_variant }) }

    fn read_ok(name: &str, coin: &UtxoStandardCoin, fx: &Fixture, read: PathFn, expected: fn(&Fixture) -> u64) {
        assert_eq!(read(coin, fx), Ok(expected(fx)), "{name}: {:?}", fx.layout);
    }

    /// The median path no longer rejects a list that the coin's layout cannot read: steps 2 to 4 of
    /// CRD ch.37 R37.5.10a read the times by the size of the headers. What it still guarantees, and
    /// what this asserts for it, is that an accepted read passed the range check.
    fn median_path_reads_by_size(name: &str) -> bool { name.starts_with("median") }

    fn assert_plausible_time(name: &str, time: u64) {
        assert!(
            time >= crate::utxo::header_time_only::EARLIEST_PLAUSIBLE_TIME,
            "{name}: an accepted median read passed the range check"
        );
    }

    fn read_err(name: &str, coin: &UtxoStandardCoin, fx: &Fixture, read: PathFn) -> String {
        if median_path_reads_by_size(name) {
            return match read(coin, fx) {
                Ok(time) => {
                    assert_plausible_time(name, time);
                    String::new()
                },
                Err(e) => e,
            };
        }
        read(coin, fx).expect_err(&format!("{name}: {:?} must be rejected", fx.layout))
    }

    /// The hazard: the registry pins `RIC` and `MEWC` to `"BTC"`, with no `header_layout` key. Once the
    /// value is really read, the standard read of their headers fails and only the R37.5.9e fallback
    /// rescues them, on every path, because `"BTC"` is in the default switch.
    #[test]
    fn btc_chain_variant_rescues_ric_and_mewc_on_every_path_under_the_default_switch() {
        for (name, read, expected) in paths() {
            for fx in [Fixture::riecoin(), Fixture::mewc()] {
                let coin = coin_from_protocol(utxo(json!("BTC")), None);
                read_ok(name, &coin, &fx, read, expected);
                assert!(logged(&coin), "{name}: the fallback logs once for the coin");
            }
        }
    }

    #[test]
    fn btc_chain_variant_is_rejected_with_the_switch_off_or_without_btc() {
        for families in [&[][..], &["PPC"][..]] {
            for (name, read, _) in paths() {
                for fx in [Fixture::riecoin(), Fixture::mewc()] {
                    let coin = coin_from_protocol(utxo(json!("BTC")), Some(families));
                    read_err(name, &coin, &fx, read);
                    assert!(!logged(&coin), "{name}: nothing is fallback-parsed under {families:?}");
                }
            }
        }
    }

    #[test]
    fn the_switch_membership_uses_the_coins_real_family() {
        // (chain_variant, switch, the fallback exists)
        let cases: [(Option<&str>, Option<&[&str]>, bool); 12] = [
            (None, None, true),
            (None, Some(&[]), false),
            (None, Some(&["PPC"]), false), // an absent key counts as "BTC"
            (Some("BTC"), None, true),
            (Some("BTC"), Some(&["PPC"]), false),
            (Some("BTC"), Some(&["BTC", "PPC"]), true),
            (Some("PPC"), None, false),
            (Some("PPC"), Some(&["PPC"]), true),
            (Some("PPC"), Some(&["BTC", "PPC"]), true),
            (Some("QTUM"), None, false),
            (Some("QTUM"), Some(&["QTUM"]), true),
            (Some("RVN"), None, false),
        ];
        for (variant, families, expected) in cases {
            let protocol = match variant {
                Some(v) => utxo(json!(v)),
                None => json!({ "type": "UTXO" }),
            };
            let conf = conf_from_protocol(protocol, families).unwrap();
            assert_eq!(
                crate::utxo::header_layout_fallback::HeaderLayoutFallback::for_conf(&conf).is_some(),
                expected,
                "{variant:?} under {families:?}"
            );
        }
        // The Qtum protocol-type reader is a reader choice, not a family: an unkeyed QTUM-type coin is "BTC".
        let conf = conf_from_protocol(json!({ "type": "QTUM" }), None).unwrap();
        assert!(crate::utxo::header_layout_fallback::HeaderLayoutFallback::for_conf(&conf).is_some());
        let conf = conf_from_protocol(json!({ "type": "QTUM" }), Some(&["PPC"])).unwrap();
        assert!(crate::utxo::header_layout_fallback::HeaderLayoutFallback::for_conf(&conf).is_none());
    }

    #[test]
    fn a_non_btc_family_in_the_switch_falls_back_and_logs_once_per_coin() {
        let fx = Fixture::riecoin();
        for (name, read, expected) in paths() {
            let coin = coin_from_protocol(utxo(json!("PPC")), Some(&["PPC"]));
            assert!(!logged(&coin));
            read_ok(name, &coin, &fx, read, expected);
            assert!(logged(&coin), "{name}: the log fired for the fallback");
        }
        // A second activation is a new coin and logs again; the PPC coin outside the default switch does not.
        let other = coin_from_protocol(utxo(json!("PPC")), None);
        read_err("retrieve_last_headers", &other, &fx, path_retrieve_last_headers);
        assert!(!logged(&other));
    }

    #[test]
    fn qtum_chain_variant_reads_qtum_headers_on_every_path_and_never_falls_back() {
        let qtum = Fixture::qtum();
        for (name, read, expected) in paths() {
            let coin = coin_from_protocol(utxo(json!("QTUM")), None);
            read_ok(name, &coin, &qtum, read, expected);
            assert!(!logged(&coin));
            // outside the default switch, so a Riecoin list is rejected, never rescued.
            for fx in [Fixture::riecoin(), Fixture::mewc()] {
                read_err(name, &coin, &fx, read);
                assert!(!logged(&coin), "{name}: a QTUM-pinned coin never falls back");
            }
            // a standard coin does not read the Qtum layout
            let standard = coin_from_protocol(utxo(json!("BTC")), None);
            read_err(name, &standard, &qtum, read);
        }
    }

    #[test]
    fn rvn_chain_variant_reads_the_120_byte_kawpow_form_on_every_path() {
        let kawpow = Fixture::kawpow();
        for (name, read, expected) in paths() {
            let coin = coin_from_protocol(utxo(json!("RVN")), None);
            read_ok(name, &coin, &kawpow, read, expected);
            assert!(!logged(&coin), "{name}: the version-signalled read, not the fallback");
        }
    }

    #[test]
    fn an_unrecognized_chain_variant_fails_the_coin_build_naming_the_ticker_and_value() {
        for bad in [
            json!("FOO"),
            json!("btc"),
            json!("Btc"),
            json!(""),
            json!(7),
            json!(null),
            json!(["BTC"]),
        ] {
            let err = conf_from_protocol(utxo(bad.clone()), None).expect_err("no fallback to the default family");
            let text = err.to_string();
            assert!(text.contains(TICKER), "{text}");
            assert!(text.contains("chain_variant"), "{text}");
            assert!(text.contains(&bad.to_string()), "{text}");
        }
        // Read and validated even when header_layout pins the layout (R37.5.9a).
        let err = conf_from_protocol(
            json!({ "type": "UTXO", "header_layout": "riecoin", "chain_variant": "FOO" }),
            None,
        )
        .expect_err("the key does not excuse an invalid chain_variant");
        assert!(err.to_string().contains("chain_variant"));
        // The eight spellings are accepted verbatim.
        for ok in ["BTC", "QTUM", "LBC", "PPC", "RICK", "MORTY", "RVN", "PIVX"] {
            conf_from_protocol(utxo(json!(ok)), None).unwrap();
        }
    }

    #[test]
    fn chain_variant_does_not_change_how_the_protocol_object_parses() {
        let with = utxo(json!("QTUM"));
        let without = json!({ "type": "UTXO" });
        let parse = |v: Json| json::to_value(json::from_value::<crate::CoinProtocol>(v).unwrap()).unwrap();
        assert_eq!(parse(with), parse(without));
    }

    #[test]
    fn an_absent_key_behaves_as_before_on_every_path() {
        let (std80, qtum) = (Fixture::standard(), Fixture::qtum());
        for (name, read, expected) in paths() {
            // Standard coin: standard headers read, Qtum headers rejected, RIC rescued by the fallback.
            let coin = coin_from_protocol(json!({ "type": "UTXO" }), None);
            read_ok(name, &coin, &std80, read, expected);
            read_err(name, &coin, &qtum, read);
            let rescued = coin_from_protocol(json!({ "type": "UTXO" }), None);
            read_ok(name, &rescued, &Fixture::riecoin(), read, expected);
            // QTUM and QRC20 protocol types keep reading Qtum headers (documented divergence of the
            // maintainer) on every path, the median path included.
            for protocol in [
                json!({ "type": "QTUM" }),
                json!({ "type": "QRC20", "protocol_data": { "platform": "QTUM", "contract_address": "0x00" } }),
            ] {
                let coin = coin_from_protocol(protocol, None);
                read_ok(name, &coin, &qtum, read, expected);
                assert!(!logged(&coin));
            }
        }
    }

    #[test]
    fn header_layout_wins_over_a_conflicting_chain_variant_and_the_fallback_is_never_consulted() {
        for conflicting in ["QTUM", "BTC", "PPC", "LBC", "PIVX"] {
            for (name, read, expected) in paths() {
                let protocol = json!({ "type": "UTXO", "header_layout": "riecoin", "chain_variant": conflicting });
                let coin = coin_from_protocol(protocol.clone(), Some(&["BTC", "QTUM", "PPC", "LBC", "PIVX"]));
                read_ok(name, &coin, &Fixture::riecoin(), read, expected);
                // the key's layout does not fit: rejected whole, with no fallback even in the switch.
                read_err(name, &coin, &Fixture::mewc(), read);
                read_err(name, &coin, &Fixture::qtum(), read);
                assert!(!logged(&coin), "{name}/{conflicting}: a pinned coin never falls back");
            }
        }
    }

    fn all_pivx() -> Vec<(&'static str, Fixture)> { PIVX_FILES.iter().map(|(n, h)| (*n, Fixture::pivx(h))).collect() }

    fn all_lbc() -> Vec<(&'static str, Fixture)> { LBC_FILES.iter().map(|(n, h)| (*n, Fixture::lbc(h))).collect() }

    #[test]
    fn pivx_chain_variant_reads_every_era_window_on_every_path() {
        for (name, read, expected) in paths() {
            for (window, fx) in all_pivx() {
                let coin = coin_from_protocol(utxo(json!("PIVX")), None);
                read_ok(&format!("{name}/{window}"), &coin, &fx, read, expected);
                assert!(!logged(&coin), "{name}/{window}: a PIVX coin never falls back");
            }
        }
    }

    #[test]
    fn lbc_chain_variant_reads_every_window_on_every_path() {
        for (name, read, expected) in paths() {
            for (window, fx) in all_lbc() {
                let coin = coin_from_protocol(utxo(json!("LBC")), None);
                read_ok(&format!("{name}/{window}"), &coin, &fx, read, expected);
                assert!(!logged(&coin), "{name}/{window}: an LBC coin never falls back");
            }
        }
    }

    /// The swap-critical case: the median time past of the latest eleven headers is the true
    /// median of their time fields, stated here from the fixture bytes.
    #[test]
    fn the_tip_window_median_time_past_is_the_true_median_of_the_eleven_times() {
        let cases: [(&str, Fixture, fn(&[u8]) -> usize); 2] = [
            ("PIVX", Fixture::pivx(PIVX_FILES[8].1), pivx_size),
            ("LBC", Fixture::lbc(LBC_FILES[3].1), |_| 112),
        ];
        for (variant, fx, size) in cases {
            let expected = fx.independent_median(size);
            let coin = coin_from_protocol(utxo(json!(variant)), None);
            assert_eq!(path_median(&coin, &fx), Ok(expected), "{variant}: get_current_mtp");
            let via_client = electrum(&coin)
                .get_median_time_past_with_policy(5000, NonZeroU64::new(11).unwrap(), policy_of(&coin))
                .wait()
                .unwrap();
            assert_eq!(
                via_client as u64, expected,
                "{variant}: get_median_time_past_with_policy"
            );
        }
        // Every PIVX era window, including the boundaries, gives its true median too.
        for (window, fx) in all_pivx() {
            let coin = coin_from_protocol(utxo(json!("PIVX")), None);
            assert_eq!(
                path_median(&coin, &fx),
                Ok(fx.independent_median(pivx_size)),
                "{window}"
            );
        }
    }

    #[test]
    fn the_store_keeps_112_and_80_byte_pivx_and_112_byte_lbc_headers() {
        for (variant, fx, size) in [
            ("PIVX", Fixture::pivx(PIVX_FILES[1].1), pivx_size as fn(&[u8]) -> usize),
            ("LBC", Fixture::lbc(LBC_FILES[3].1), |_| 112),
        ] {
            let coin = coin_from_protocol(utxo(json!(variant)), None);
            let storage = coin.as_ref().block_headers_storage.as_ref().unwrap();
            let pieces = split_by(&fx.list, size);
            let headers = chain::BlockHeader::list_from_served_bytes(&fx.list, 11, fx.layout).unwrap();
            let registry: HashMap<u64, chain::BlockHeader> = (100u64..).zip(headers.iter().cloned()).collect();
            block_on(storage.add_block_headers_to_storage(TICKER, registry)).unwrap();
            for (height, piece) in (100u64..).zip(&pieces) {
                let read = block_on(storage.get_block_header(TICKER, height)).unwrap().unwrap();
                assert_eq!(
                    &read.raw()[..],
                    *piece,
                    "{variant}: stored bytes equal served bytes at {height}"
                );
            }
            let lens: std::collections::BTreeSet<usize> = pieces.iter().map(|p| p.len()).collect();
            let wanted: Vec<usize> = if variant == "PIVX" { vec![80, 112] } else { vec![112] };
            assert_eq!(lens.into_iter().collect::<Vec<_>>(), wanted);
            assert!(!logged(&coin));
        }
    }

    #[test]
    fn pivx_and_lbc_reject_malformed_input_on_every_path_without_panicking() {
        for (variant, good) in [
            ("PIVX", Fixture::pivx(PIVX_FILES[7].1)),
            ("LBC", Fixture::lbc(LBC_FILES[0].1)),
        ] {
            let mut cases: Vec<(&str, Vec<u8>, Vec<u8>, u64)> = Vec::new();
            let mut trailing = good.list.clone();
            trailing.push(0);
            cases.push(("trailing byte", good.lone.clone(), trailing, 11));
            cases.push((
                "truncated",
                good.lone.clone(),
                good.list[..good.list.len() - 1].to_vec(),
                11,
            ));
            cases.push((
                "one header short",
                good.lone.clone(),
                good.list[..10 * 112].to_vec(),
                11,
            ));
            cases.push(("count mismatch (10)", good.lone.clone(), good.list.clone(), 10));
            cases.push(("count mismatch (12)", good.lone.clone(), good.list.clone(), 12));
            for (what, lone, list, count) in cases {
                let coin = coin_from_protocol(utxo(json!(variant)), None);
                let fx = Fixture {
                    layout: good.layout,
                    lone,
                    list,
                    count: count as usize,
                    time_offset: good.time_offset,
                };
                // the list-based paths reject; the lone-header paths are not affected by a bad list
                for (name, read) in [
                    ("retrieve_last_headers", path_retrieve_last_headers as PathFn),
                    ("median", path_median as PathFn),
                ] {
                    let outcome = read(&coin, &fx);
                    if median_path_reads_by_size(name) {
                        // R37.5.10: the median path reads such a list by the size of its headers.
                        if let Ok(time) = outcome {
                            assert_plausible_time(name, time);
                        }
                        continue;
                    }
                    assert!(outcome.is_err(), "{variant}/{what}/{name}: {outcome:?}");
                }
            }
        }
        // LBC is fixed-layout: a list of 80-byte headers, and a lone 80-byte header, are malformed.
        let lbc = Fixture::lbc(LBC_FILES[0].1);
        let eighty: Vec<u8> = lbc.list.chunks(112).flat_map(|c| c[..80].to_vec()).collect();
        let coin = coin_from_protocol(utxo(json!("LBC")), None);
        serve(&lbc.list[..80], &eighty, 11);
        assert!(electrum(&coin)
            .retrieve_last_headers(NonZeroU64::new(11).unwrap(), 5000, policy_of(&coin))
            .wait()
            .is_err());
        assert!(block_on(
            coin.as_ref()
                .rpc_client
                .get_block_timestamp_with_policy(77, &policy_of(&coin))
        )
        .is_err());
        // PIVX: a version-7 header served with 112 bytes is rejected (not 80 bytes, not its layout).
        let v7 = Fixture::pivx(PIVX_FILES[5].1);
        let mut padded = v7.lone.clone();
        padded.extend_from_slice(&[0u8; 32]);
        let coin = coin_from_protocol(utxo(json!("PIVX")), None);
        serve(&padded, &v7.list, 11);
        assert!(block_on(
            coin.as_ref()
                .rpc_client
                .get_block_timestamp_with_policy(77, &policy_of(&coin))
        )
        .is_err());
    }

    #[test]
    fn pivx_structural_guard_accepts_an_exactly_80_byte_version_4_header_as_std_80() {
        // R37.5.6 covers the PIVX trailing field, which the version chooses: a header served as
        // exactly 80 bytes is read as std-80 (here a stripped version-4 header).
        let v4 = Fixture::pivx(PIVX_FILES[2].1);
        let coin = coin_from_protocol(utxo(json!("PIVX")), None);
        serve(&v4.lone[..80], &v4.list, 11);
        let time = block_on(
            coin.as_ref()
                .rpc_client
                .get_block_timestamp_with_policy(77, &policy_of(&coin)),
        )
        .unwrap();
        assert_eq!(time, v4.lone_time());
        // A 79-byte or 81-byte header gets no re-read.
        for len in [79, 81] {
            serve(&v4.lone[..len], &v4.list, 11);
            assert!(block_on(
                coin.as_ref()
                    .rpc_client
                    .get_block_timestamp_with_policy(77, &policy_of(&coin))
            )
            .is_err());
        }
    }

    #[test]
    fn pivx_and_lbc_do_not_read_the_other_layouts() {
        // The Qtum-shaped list (246-byte headers) fits neither fixed length family.
        let qtum = Fixture::qtum();
        for variant in ["LBC", "PIVX"] {
            for (name, read, _) in paths() {
                let coin = coin_from_protocol(utxo(json!(variant)), None);
                read_err(name, &coin, &qtum, read);
                assert!(!logged(&coin));
            }
        }
        // A plain 80-byte header chain is not an LBC chain.
        let coin = coin_from_protocol(utxo(json!("LBC")), None);
        read_err(
            "retrieve_last_headers",
            &coin,
            &Fixture::standard(),
            path_retrieve_last_headers,
        );
    }

    #[test]
    fn pivx_and_lbc_are_outside_the_default_switch_so_they_never_fall_back() {
        for variant in ["PIVX", "LBC"] {
            let conf = conf_from_protocol(utxo(json!(variant)), None).unwrap();
            assert!(crate::utxo::header_layout_fallback::HeaderLayoutFallback::for_conf(&conf).is_none());
            assert_eq!(
                crate::utxo::header_layout_fallback::coin_family(&conf),
                variant,
                "the family is the chain_variant value"
            );
            let expected = if variant == "PIVX" {
                CoinVariant::Pivx
            } else {
                CoinVariant::Lbc
            };
            assert_eq!(HeaderReadPolicy::for_conf(&conf).layout(), expected);
        }
    }

    /// Regression pin: a coin with no `chain_variant` reads PIVX-shaped bytes exactly as before this
    /// change: the 80-byte eras are standard headers, the 112-byte eras are rejected (no fallback
    /// rescues them), on every path.
    #[test]
    fn a_coin_without_chain_variant_reads_pivx_bytes_as_before() {
        for (name, read, expected) in paths() {
            for (window, fx) in all_pivx() {
                let coin = coin_from_protocol(json!({ "type": "UTXO" }), None);
                let eighty_only = matches!(window, "v3" | "v7");
                if eighty_only {
                    // The standard reader reads them; the fixture states the parse under the PIVX
                    // layout, which for 80-byte headers is the same read.
                    read_ok(&format!("{name}/{window}"), &coin, &fx, read, expected);
                } else if window == "v4" || window == "v5" || window == "tip" {
                    read_err(&format!("{name}/{window}"), &coin, &fx, read);
                }
            }
        }
    }

    #[test]
    fn runebase_qtum_lists_read_on_the_median_and_sync_paths() {
        for (hex, label) in [(RUNES_TIP, "tip"), (RUNES_500K, "500000")] {
            let fx = Fixture::runes(hex);
            assert_eq!(fx.list.len(), 11 * fx.lone.len(), "{label}: eleven equal-size headers");
            let expected_median = fx.independent_median(qtum_size);
            for protocol in [utxo(json!("QTUM")), json!({ "type": "QTUM" })] {
                let coin = coin_from_protocol(protocol, None);
                // median path
                assert_eq!(path_median(&coin, &fx), Ok(expected_median), "{label}: median");
                // sync path: retrieve_last_headers consumes the bytes exactly and re-encodes them
                assert_eq!(
                    path_retrieve_last_headers(&coin, &fx),
                    Ok(fx.list_time()),
                    "{label}: retrieve_last_headers"
                );
                // the lone-header paths and the store
                assert_eq!(path_get_block_timestamp(&coin, &fx), Ok(fx.lone_time()), "{label}");
                assert_eq!(
                    path_valid_block_header_from_storage(&coin, &fx),
                    Ok(fx.lone_time()),
                    "{label}"
                );
                assert!(!logged(&coin));
                // a fresh coin: the paths above already stored these headers at other heights
                let fresh = coin_from_protocol(utxo(json!("QTUM")), None);
                assert_eq!(path_store(&fresh, &fx), Ok(fx.list_time()), "{label}: store");
            }
        }
    }

    #[test]
    fn ppc_rick_and_morty_read_standard_80_exactly_like_an_absent_key() {
        // Plan step H4 (exclusivity, the thresholded rules, the Equihash reading of RICK/MORTY and
        // PPC's version-4 suppression) is not implemented: these three resolve to the standard reader.
        let std80 = Fixture::standard();
        for variant in ["PPC", "RICK", "MORTY"] {
            for (name, read, expected) in paths() {
                let coin = coin_from_protocol(utxo(json!(variant)), None);
                read_ok(name, &coin, &std80, read, expected);
                let absent = coin_from_protocol(json!({ "type": "UTXO" }), None);
                read_ok(name, &absent, &std80, read, expected);
                read_err(name, &coin, &Fixture::qtum(), read);
            }
        }
    }

    #[test]
    fn a_keyless_btc_coin_rereads_stored_112_and_120_byte_headers_through_the_conf() {
        let coin = coin_from_protocol(utxo(json!("BTC")), None);
        let storage = coin.as_ref().block_headers_storage.as_ref().unwrap();
        let ric = chain::BlockHeader::from_served_bytes(&Fixture::riecoin().lone, CoinVariant::Riecoin).unwrap();
        let native =
            chain::BlockHeader::from_served_bytes(&Fixture::mewc().lone, CoinVariant::KawpowAuxpowMixed).unwrap();
        assert_eq!((ric.raw().len(), native.raw().len()), (112, 120));
        let registry: HashMap<u64, chain::BlockHeader> = [(1, ric.clone()), (2, native.clone())].into_iter().collect();
        block_on(storage.add_block_headers_to_storage(TICKER, registry)).unwrap();
        for (height, header) in [(1, ric), (2, native)] {
            let read = block_on(storage.get_block_header(TICKER, height)).unwrap().unwrap();
            assert_eq!(read.raw(), header.raw());
        }
        assert!(logged(&coin));
        // With the switch off the same rows are unreadable.
        let off = coin_from_protocol(utxo(json!("BTC")), Some(&[]));
        let storage = off.as_ref().block_headers_storage.as_ref().unwrap();
        let ric = chain::BlockHeader::from_served_bytes(&Fixture::riecoin().lone, CoinVariant::Riecoin).unwrap();
        block_on(storage.add_block_headers_to_storage(TICKER, [(1u64, ric)].into_iter().collect())).unwrap();
        assert!(block_on(storage.get_block_header(TICKER, 1)).is_err());
    }
}

/// The legacy `electrum` method honors `min_connected` and `max_connected` (CRD ch. 55 R55.4.3, ch. 38 R38.6.4).
#[cfg(not(target_arch = "wasm32"))]
mod legacy_electrum_connection_settings {
    use super::*;
    use crate::lp_coininit;
    use crate::qrc20::Qrc20ActivationParams;
    use crate::utxo::bch::BchActivationRequest;
    use crypto::CryptoCtx;
    use std::net::TcpListener;

    const SERVERS: [&str; 2] = ["127.0.0.1:1", "127.0.0.1:2"];

    /// The probe returns on the first connection and the rest connect in the background (CRD ch.38
    /// R38.6.4k-R), so the maintained set is waited for.
    fn wait_for_maintained(client: &ElectrumClient, count: usize) {
        let started = std::time::Instant::now();
        while block_on(client.count_connected()) != count {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(10),
                "no {count} maintained connections"
            );
            block_on(Timer::sleep(0.02));
        }
    }

    fn request(extra: Json) -> Json {
        let mut req = json!({
            "method": "electrum",
            "coin": "RCK",
            "servers": SERVERS.iter().map(|url| json!({ "url": url })).collect::<Vec<_>>(),
        });
        req.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        req
    }

    fn settings(params: &UtxoActivationParams) -> (usize, Option<usize>, Option<usize>) {
        match &params.mode {
            UtxoRpcMode::Electrum {
                servers,
                min_connected,
                max_connected,
            } => (servers.len(), *min_connected, *max_connected),
            UtxoRpcMode::Native => panic!("Electrum mode expected"),
        }
    }

    #[test]
    fn max_connected_reaches_the_params() {
        let params = UtxoActivationParams::from_legacy_req(&request(json!({ "max_connected": 1 }))).unwrap();
        assert_eq!(settings(&params), (2, None, Some(1)));

        let params =
            UtxoActivationParams::from_legacy_req(&request(json!({ "min_connected": 2, "max_connected": 2 }))).unwrap();
        assert_eq!(settings(&params), (2, Some(2), Some(2)));
    }

    #[test]
    fn absent_or_null_values_keep_the_defaults() {
        for extra in [json!({}), json!({ "min_connected": null, "max_connected": null })] {
            let params = UtxoActivationParams::from_legacy_req(&request(extra)).unwrap();
            assert_eq!(settings(&params), (2, None, None));
        }
    }

    #[test]
    fn unusable_values_are_refused() {
        let cases = [
            (json!({ "min_connected": 0 }), "'min_connected' must be at least 1"),
            (json!({ "max_connected": 0 }), "'max_connected' must be at least 1"),
            (
                json!({ "min_connected": 2, "max_connected": 1 }),
                "'min_connected' must not exceed 'max_connected'",
            ),
        ];
        for (extra, reason) in cases {
            let error = UtxoActivationParams::from_legacy_req(&request(extra)).unwrap_err();
            assert_eq!(
                error.get_inner().to_string(),
                format!("Unusable Electrum connection settings: {}", reason)
            );
        }

        let mut empty = request(json!({}));
        empty["servers"] = json!([]);
        let error = UtxoActivationParams::from_legacy_req(&empty).unwrap_err();
        assert_eq!(
            error.get_inner().to_string(),
            "Unusable Electrum connection settings: no server is configured"
        );

        for (key, value) in [
            ("min_connected", json!("1")),
            ("max_connected", json!("1")),
            ("max_connected", json!(-1)),
            ("max_connected", json!(1.5)),
            ("min_connected", json!(true)),
        ] {
            let error = UtxoActivationParams::from_legacy_req(&request(json!({ key: value }))).unwrap_err();
            assert!(
                matches!(
                    error.get_inner(),
                    UtxoFromLegacyReqErr::InvalidMinConnected(_) | UtxoFromLegacyReqErr::InvalidMaxConnected(_)
                ),
                "{key}: {error}"
            );
        }
    }

    /// The legacy `enable` method (native daemon) has no Electrum settings and ignores both keys.
    #[test]
    fn enable_ignores_the_keys() {
        let params = UtxoActivationParams::from_legacy_req(
            &json!({ "method": "enable", "coin": "RCK", "min_connected": 0, "max_connected": "x" }),
        )
        .unwrap();
        assert!(matches!(params.mode, UtxoRpcMode::Native));
    }

    #[test]
    fn bch_and_qrc20_inherit_the_values() {
        let bch = BchActivationRequest::from_legacy_req(&request(json!({
            "max_connected": 1,
            "bchd_urls": [],
            "allow_slp_unsafe_conf": true,
        })))
        .unwrap();
        assert_eq!(settings(&bch.utxo_params), (2, None, Some(1)));

        let error = BchActivationRequest::from_legacy_req(&request(json!({
            "max_connected": 0,
            "bchd_urls": [],
        })))
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("Unusable Electrum connection settings: 'max_connected' must be at least 1"));

        let qrc20 = Qrc20ActivationParams::from_legacy_req(&request(json!({
            "max_connected": 1,
            "swap_contract_address": "0x2f754733acd6d753731c00fee32cb484551cc15d",
        })))
        .unwrap();
        assert_eq!(settings(&qrc20.utxo_params), (2, None, Some(1)));
    }

    /// The error the legacy `electrum` method answers (HTTP 500 with `{"error": string}`, R55.1.10) is the
    /// string `lp_coininit` returns; no connection is attempted before the settings are refused.
    #[test]
    fn lp_coininit_refuses_unusable_values_before_connecting() {
        let ctx = MmCtxBuilder::new()
            .with_conf(json!({ "coins": [{ "coin": "RCK", "mm2": 1, "protocol": { "type": "UTXO" } }] }))
            .into_mm_arc();
        CryptoCtx::init_with_iguana_passphrase(ctx.clone(), "legacy electrum settings test").unwrap();
        let error = block_on(lp_coininit(
            &ctx,
            "RCK",
            &request(json!({ "min_connected": 2, "max_connected": 1 })),
        ))
        .err()
        .expect("the request must be refused");
        assert!(
            error.contains("Unusable Electrum connection settings: 'min_connected' must not exceed 'max_connected'"),
            "{error}"
        );
        let error = block_on(lp_coininit(&ctx, "RCK", &request(json!({ "max_connected": "one" }))))
            .err()
            .expect("the request must be refused");
        assert!(
            error.contains("Invalid 'max_connected': invalid type: string \"one\", expected usize"),
            "{error}"
        );
    }

    /// Two loopback servers, `max_connected: 1`: the client builder opens a single connection in the background.
    /// With the default maximum it opens one per server.
    #[test]
    fn the_client_builder_honors_max_connected() {
        let listeners: Vec<_> = (0..2).map(|_| TcpListener::bind("127.0.0.1:0").unwrap()).collect();
        let servers: Vec<_> = listeners
            .iter()
            .map(|listener| json!({ "url": listener.local_addr().unwrap().to_string() }))
            .collect();

        let ctx = MmCtxBuilder::default().into_mm_arc();
        let build = |extra: Json| {
            let mut req = request(extra);
            req["servers"] = json!(servers);
            let params = UtxoActivationParams::from_legacy_req(&req).unwrap();
            let builder = UtxoArcBuilder::new(
                &ctx,
                TEST_COIN_NAME,
                &Json::Null,
                &params,
                PrivKeyBuildPolicy::IguanaPrivKey(keys::Secret::default()),
                UtxoStandardCoin::from,
            );
            let args = ElectrumBuilderArgs {
                spawn_ping: false,
                negotiate_version: false,
                collect_metrics: false,
            };
            let (servers, min_connected, max_connected) = match params.mode.clone() {
                UtxoRpcMode::Electrum {
                    servers,
                    min_connected,
                    max_connected,
                } => (servers, min_connected, max_connected),
                UtxoRpcMode::Native => unreachable!(),
            };
            block_on(builder.electrum_client(args, servers, min_connected, max_connected)).unwrap()
        };

        // The pool is made of both servers, but the maintained set stays at the ceiling.
        let limited = build(json!({ "max_connected": 1 }));
        assert_eq!(block_on(limited.count_connections()), 2);
        wait_for_maintained(&limited, 1);
        block_on(Timer::sleep(0.3));
        assert_eq!(block_on(limited.count_connected()), 1);

        let unlimited = build(json!({}));
        assert_eq!(block_on(unlimited.count_connections()), 2);
        wait_for_maintained(&unlimited, 2);
    }
}

/// The client builder against scripted Electrum servers (CRD ch.38 R38.6.4k-R, T38.6.4.12-R). The builder
/// uses the production timings, so these tests keep every server fast or refusing.
#[cfg(not(target_arch = "wasm32"))]
mod electrum_pool_activation {
    use super::*;
    use crate::utxo::rpc_clients::electrum_stub_server::{StubBehavior, StubServer};
    use std::time::{Duration, Instant};

    fn build_client(servers: &[&StubServer]) -> crate::utxo::utxo_builder::UtxoCoinBuildResult<ElectrumClient> {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        let servers: Vec<_> = servers.iter().map(|server| json!({ "url": server.addr() })).collect();
        let req = json!({ "method": "electrum", "servers": servers });
        let params = UtxoActivationParams::from_legacy_req(&req).unwrap();
        let builder = UtxoArcBuilder::new(
            &ctx,
            TEST_COIN_NAME,
            &Json::Null,
            &params,
            PrivKeyBuildPolicy::IguanaPrivKey(keys::Secret::default()),
            UtxoStandardCoin::from,
        );
        let args = ElectrumBuilderArgs {
            spawn_ping: false,
            negotiate_version: true,
            collect_metrics: false,
        };
        let servers = match params.mode.clone() {
            UtxoRpcMode::Electrum { servers, .. } => servers,
            UtxoRpcMode::Native => unreachable!(),
        };
        block_on(builder.electrum_client(args, servers, None, None))
    }

    /// T38.6.4.12-R: the build waits for the first connection and no longer, and the block-height query that
    /// follows is answered over it.
    #[test]
    fn the_build_waits_for_the_first_connection_and_the_height_is_then_answered() {
        let servers = block_on(async {
            let servers = [
                StubServer::start(StubBehavior::Answer).await,
                StubServer::start(StubBehavior::Answer).await,
            ];
            for server in &servers {
                server.set_accept_delay(Duration::from_millis(300));
                server.set_result("blockchain.headers.subscribe", json!({"height": 777, "hex": "00"}));
            }
            servers
        });

        let started = Instant::now();
        let client = build_client(&[&servers[0], &servers[1]]).unwrap();
        let built_after = started.elapsed();
        assert!(built_after >= Duration::from_millis(250), "{built_after:?}");
        assert!(built_after < Duration::from_secs(5), "{built_after:?}");
        assert!(block_on(client.count_connected()) >= 1);

        assert_eq!(block_on(client.get_block_count().compat()).unwrap(), 777);
    }

    /// T38.6.4.12-R: with no server reachable the build fails at once, naming the coin and every server; the
    /// activation reports it as `Transport` (CRD ch.55 R55.5.14, tested in `coins_activation`).
    #[test]
    fn an_unreachable_server_set_fails_the_build_naming_each_server() {
        let servers = block_on(async {
            [
                StubServer::start(StubBehavior::Down).await,
                StubServer::start(StubBehavior::Down).await,
            ]
        });
        let started = Instant::now();
        let error = build_client(&[&servers[0], &servers[1]]).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());

        let crate::utxo::utxo_builder::UtxoCoinBuildError::FailedToConnectToElectrums(failure) = error.into_inner()
        else {
            panic!("an unreachable server set is a connection failure");
        };
        for server in &servers {
            assert!(failure.contains(&server.addr()), "{failure}");
        }
        assert!(failure.contains(TEST_COIN_NAME), "{failure}");
    }

    /// The wallet maintainer's case: the first listed servers are down or silent, a later one is up. The coin is
    /// enabled in the time of the live server's connection, not after the silent server's 20 s, and its
    /// block-height query is answered.
    #[test]
    fn dead_servers_first_and_a_live_one_last_do_not_stall_the_activation() {
        let servers = block_on(async {
            let servers = [
                StubServer::start(StubBehavior::Down).await,
                StubServer::start(StubBehavior::Silent).await,
                StubServer::start(StubBehavior::Answer).await,
            ];
            servers[2].set_result("blockchain.headers.subscribe", json!({"height": 4242, "hex": "00"}));
            servers
        });
        let started = Instant::now();
        let client = build_client(&[&servers[0], &servers[1], &servers[2]]).unwrap();
        // The build waits up to the activation window of 5 s after its first connection for the heights of the
        // other servers (CRD ch.38 R38.6.4o item 12, D38.6.4o.10), and a silent server never answers: that window,
        // not the silent server's 20 s, is what it costs.
        assert!(started.elapsed() < Duration::from_secs(7), "{:?}", started.elapsed());
        assert_eq!(block_on(client.count_connected()), 1);
        let height = block_on(client.get_block_count().compat()).unwrap();
        assert_eq!(height, 4242);
        assert!(started.elapsed() < Duration::from_secs(7), "{:?}", started.elapsed());
    }
}

// ---- Order-posting latency: one Electrum request message per legacy-address read (CRD ch.38 R38.6.3a) and one
// fee reading per coin and order-posting request (R38.6.6j item 1) ----

#[cfg(not(target_arch = "wasm32"))]
mod order_posting_latency {
    use super::*;
    use crate::request_fee_scope::with_request_fee_scope;
    use crate::utxo::rpc_clients::electrum_rpc_client::electrum_client_over_stub;
    use crate::utxo::rpc_clients::electrum_stub_server::StubServer;
    use crate::utxo::utxo_common::{address_balance, get_all_unspent_ordered_list};
    use crate::{FeeApproxStage, TradePreimageValue};
    use serde_json::Value as Json;

    /// What the scripted Electrum server holds for the coin of the tests below: the balance and the unspent list of
    /// the coin's P2PKH script hash, the unspent list of its P2PK script hash, and the fee readings.
    fn script_electrum(server: &StubServer, p2pkh_hash: &str, p2pk_hash: &str) {
        server.set_scripthash_result(
            "blockchain.scripthash.get_balance",
            p2pkh_hash,
            json!({"confirmed": 1_000_000, "unconfirmed": 5}),
        );
        server.set_scripthash_result(
            "blockchain.scripthash.listunspent",
            p2pkh_hash,
            json!([
                {"tx_hash": "01".repeat(32), "tx_pos": 0, "height": 10, "value": 900_000},
                {"tx_hash": "01".repeat(32), "tx_pos": 0, "height": 10, "value": 900_000},
            ]),
        );
        server.set_scripthash_result(
            "blockchain.scripthash.listunspent",
            p2pk_hash,
            json!([
                {"tx_hash": "02".repeat(32), "tx_pos": 3, "height": 11, "value": 100_000}
            ]),
        );
        server.set_result("blockchain.estimatefee", json!(0.0001));
        server.set_result("blockchain.relayfee", json!(0.00002));
    }

    /// The P2PKH and P2PK script hashes of the test coin's address. The scripted server needs them, and they depend
    /// on the seed's key; the coin is built once here to learn its address and key, and again with the client that
    /// answers for them.
    pub(super) fn script_hashes_of_test_coin(segwit: bool) -> (String, String) {
        let probe = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(native_client_for_test()), None, segwit);
        let address = match &probe.derivation_method {
            DerivationMethod::Iguana(address) => address.clone(),
            _ => unreachable!("the test coin has an Iguana address"),
        };
        let p2pkh = output_script(&address, ScriptType::P2PKH);
        let p2pk = Builder::build_p2pk(probe.priv_key_policy.key_pair().unwrap().public());
        (
            hex::encode(electrum_script_hash(&p2pkh)),
            hex::encode(electrum_script_hash(&p2pk)),
        )
    }

    /// A coin on a scripted Electrum server. Returns the coin and the stub server, which has recorded the request messages.
    fn coin_on_scripted_electrum(
        segwit: bool,
        fee: TxFee,
        force_min_relay_fee: bool,
    ) -> (UtxoStandardCoin, StubServer) {
        let (p2pkh_hash, p2pk_hash) = script_hashes_of_test_coin(segwit);
        let (client, server) = electrum_client_over_stub(|server| script_electrum(server, &p2pkh_hash, &p2pk_hash));
        let mut fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Electrum(client), None, segwit);
        fields.tx_fee = fee;
        fields.conf.force_min_relay_fee = force_min_relay_fee;
        (utxo_coin_from_fields(fields), server)
    }

    pub(super) fn my_address(coin: &UtxoStandardCoin) -> Address {
        match &coin.as_ref().derivation_method {
            DerivationMethod::Iguana(address) => address.clone(),
            _ => unreachable!("the test coin has an Iguana address"),
        }
    }

    /// The `(method, scripthash)` lookups of every request message, one entry per message.
    pub(super) fn lookups(server: &StubServer) -> Vec<Vec<(String, String)>> {
        server
            .request_messages()
            .iter()
            .map(|message| {
                let requests = match message.as_array() {
                    Some(batch) => batch.clone(),
                    None => vec![message.clone()],
                };
                requests
                    .iter()
                    .map(|r| {
                        (
                            r["method"].as_str().unwrap().to_owned(),
                            r["params"][0].as_str().unwrap_or_default().to_owned(),
                        )
                    })
                    .collect()
            })
            .collect()
    }

    pub(super) fn count_requests(server: &StubServer, method: &str) -> usize {
        lookups(server).iter().flatten().filter(|(m, _)| m == method).count()
    }

    /// R38.6.3a: a legacy-address balance read is ONE request message carrying the P2PKH balance lookup and the
    /// P2PK lookup, and the balance is the same as before: the P2PKH balance plus the P2PK unspents.
    #[test]
    fn legacy_address_balance_read_is_one_request_message() {
        let (coin, server) = coin_on_scripted_electrum(false, TxFee::FixedPerKb(1000), false);
        let balance = block_on(address_balance(&coin, &my_address(&coin))).unwrap();

        // (1_000_000 + 5) base units of the P2PKH script, plus the 100_000 of the P2PK output; 8 decimals.
        assert_eq!(balance.spendable, "0.01100005".parse::<BigDecimal>().unwrap());
        assert_eq!(balance.unspendable, BigDecimal::from(0));
        let sent = lookups(&server);
        assert_eq!(sent.len(), 1, "one request message: {:?}", sent);
        assert_eq!(sent[0].len(), 2);
        assert_eq!(sent[0][0].0, "blockchain.scripthash.get_balance");
        assert_eq!(sent[0][1].0, "blockchain.scripthash.listunspent");
    }

    /// R38.6.3a: a legacy-address unspent-list read is ONE request message, and the unspents are those of
    /// before: the P2PKH ones (de-duplicated) and the P2PK one.
    #[test]
    fn legacy_address_unspent_list_read_is_one_request_message() {
        let (coin, server) = coin_on_scripted_electrum(false, TxFee::FixedPerKb(1000), false);
        let (unspents, _guard) = block_on(get_all_unspent_ordered_list(&coin, &my_address(&coin))).unwrap();

        let mut values: Vec<_> = unspents.iter().map(|u| u.value).collect();
        values.sort();
        assert_eq!(values, vec![100_000, 900_000]);
        let sent = lookups(&server);
        assert_eq!(sent.len(), 1, "one request message: {:?}", sent);
        assert_eq!(sent[0].len(), 2);
        assert!(sent[0].iter().all(|(m, _)| m == "blockchain.scripthash.listunspent"));
        assert_ne!(sent[0][0].1, sent[0][1].1, "the P2PKH and the P2PK script hashes");
    }

    /// R38.6.3a: a segwit address sends the P2PKH lookup alone, as before.
    #[test]
    fn segwit_address_reads_send_the_p2pkh_lookup_alone() {
        let (coin, server) = coin_on_scripted_electrum(true, TxFee::FixedPerKb(1000), false);
        let balance = block_on(address_balance(&coin, &my_address(&coin))).unwrap();
        assert_eq!(balance.spendable, "0.01000005".parse::<BigDecimal>().unwrap());
        let sent = lookups(&server);
        assert_eq!(sent.len(), 1, "{:?}", sent);
        assert_eq!(sent[0].len(), 1);
        assert_eq!(sent[0][0].0, "blockchain.scripthash.get_balance");

        let (unspents, _guard) = block_on(get_all_unspent_ordered_list(&coin, &my_address(&coin))).unwrap();
        assert_eq!(unspents.iter().map(|u| u.value).collect::<Vec<_>>(), vec![900_000]);
        let sent = lookups(&server);
        assert_eq!(sent.len(), 2, "{:?}", sent);
        assert_eq!(sent[1].len(), 1);
        assert_eq!(sent[1][0].0, "blockchain.scripthash.listunspent");
    }

    /// R38.6.6j item 1: inside one order-posting request the spend check, the sender trade fee, the receiver
    /// trade fee and a second spend check read a dynamic-fee coin's estimate exactly once.
    #[test]
    fn dynamic_fee_estimate_is_read_once_per_order_posting_request() {
        let (coin, server) = coin_on_scripted_electrum(false, TxFee::Dynamic(EstimateFeeMethod::Standard), false);
        let (spend_safe, receiver_fee) = block_on(with_request_fee_scope(
            &mm2_core::mm_ctx::MmCtxBuilder::default().into_mm_arc(),
            async {
                let first = coin.spend_safe_amount().await.unwrap();
                let sender = coin
                    .get_sender_trade_fee(
                        TradePreimageValue::Exact("0.001".parse().unwrap()),
                        FeeApproxStage::OrderIssue,
                    )
                    .await
                    .unwrap();
                assert!(sender.amount.to_decimal() > BigDecimal::from(0));
                let receiver = coin
                    .get_receiver_trade_fee(FeeApproxStage::OrderIssue)
                    .compat()
                    .await
                    .unwrap();
                let second = coin.spend_safe_amount().await.unwrap();
                assert_eq!(first, second);
                (first, receiver)
            },
        ));
        assert!(spend_safe.is_some());
        // 0.0001 coin/kB over the 496 byte HTLC spend.
        assert_eq!(
            receiver_fee.amount.to_decimal(),
            "0.0000496".parse::<BigDecimal>().unwrap()
        );
        assert_eq!(count_requests(&server, "blockchain.estimatefee"), 1);
    }

    /// R38.6.6j item 3: outside an order-posting request every consumer reads a fresh estimate, as swap
    /// execution does.
    #[test]
    fn dynamic_fee_estimate_is_read_fresh_outside_a_request() {
        let (coin, server) = coin_on_scripted_electrum(false, TxFee::Dynamic(EstimateFeeMethod::Standard), false);
        block_on(async {
            coin.spend_safe_amount().await.unwrap();
            coin.spend_safe_amount().await.unwrap();
            coin.get_receiver_trade_fee(FeeApproxStage::OrderIssue)
                .compat()
                .await
                .unwrap();
        });
        assert_eq!(count_requests(&server, "blockchain.estimatefee"), 3);
    }

    /// R38.6.6j item 1: the forced minimum relay fee is read once per request as well, and afresh outside one.
    #[test]
    fn forced_min_relay_fee_is_read_once_per_order_posting_request() {
        let (coin, server) = coin_on_scripted_electrum(false, TxFee::Dynamic(EstimateFeeMethod::Standard), true);
        block_on(with_request_fee_scope(
            &mm2_core::mm_ctx::MmCtxBuilder::default().into_mm_arc(),
            async {
                coin.spend_safe_amount().await.unwrap();
                coin.get_receiver_trade_fee(FeeApproxStage::OrderIssue)
                    .compat()
                    .await
                    .unwrap();
                coin.spend_safe_amount().await.unwrap();
            },
        ));
        assert_eq!(count_requests(&server, "blockchain.relayfee"), 1);
        assert_eq!(count_requests(&server, "blockchain.estimatefee"), 1);

        block_on(async {
            coin.spend_safe_amount().await.unwrap();
            coin.spend_safe_amount().await.unwrap();
        });
        assert_eq!(count_requests(&server, "blockchain.relayfee"), 3);
        assert_eq!(count_requests(&server, "blockchain.estimatefee"), 3);
    }

    /// CRD ch.38 R38.6.4a item 4: the recently-spent-output cache shields a read that follows this node's own
    /// broadcast from a session that has not seen it yet, whichever session answers. The broadcast goes to the
    /// first server, the next unspent-list read is answered by the second, which still lists the spent output, and
    /// so is the one after it by the third; the spent output stays out and its replacement is in.
    #[test]
    fn a_spent_output_stays_excluded_when_another_session_answers_the_read() {
        use crate::utxo::rpc_clients::electrum_rpc_client::electrum_client_over_stubs;
        use crate::utxo::rpc_clients::UnspentInfo;
        use chain::TransactionOutput;

        let (p2pkh_hash, p2pk_hash) = script_hashes_of_test_coin(false);
        let servers = block_on(async {
            [
                StubServer::start(crate::utxo::rpc_clients::electrum_stub_server::StubBehavior::Answer).await,
                StubServer::start(crate::utxo::rpc_clients::electrum_stub_server::StubBehavior::Answer).await,
                StubServer::start(crate::utxo::rpc_clients::electrum_stub_server::StubBehavior::Answer).await,
            ]
        });
        for server in &servers {
            // None of them has seen the broadcast: every one still lists the output the broadcast spends.
            script_electrum(server, &p2pkh_hash, &p2pk_hash);
            server.set_result("blockchain.transaction.broadcast", json!("ab".repeat(32)));
        }
        let client = block_on(electrum_client_over_stubs(&[&servers[0], &servers[1], &servers[2]]));
        let coin = utxo_coin_from_fields(utxo_coin_fields_for_test(
            UtxoRpcClientEnum::Electrum(client),
            None,
            false,
        ));
        let address = my_address(&coin);

        // The first read (the first server's turn) shows the output the node is about to spend.
        let (unspents, guard) = block_on(get_all_unspent_ordered_list(&coin, &address)).unwrap();
        drop(guard);
        let spent: UnspentInfo = unspents.iter().find(|u| u.value == 900_000).cloned().unwrap();

        // The node broadcasts a spend of it (priority routing: the first server), and records it as sent.
        block_on(
            coin.as_ref()
                .rpc_client
                .send_raw_transaction(vec![1, 2, 3].into())
                .compat(),
        )
        .unwrap();
        let change_script = output_script(&address, ScriptType::P2PKH).to_bytes();
        block_on(async {
            coin.as_ref().recently_spent_outpoints.lock().await.add_spent(
                vec![spent.clone()],
                H256::from([9; 32]),
                vec![TransactionOutput {
                    value: 800_000,
                    script_pubkey: change_script,
                }],
            );
        });

        // The next two reads are answered by the second and the third server, which know nothing of the spend.
        for _ in 0..2 {
            let (unspents, _guard) = block_on(get_all_unspent_ordered_list(&coin, &address)).unwrap();
            let mut values: Vec<_> = unspents.iter().map(|u| u.value).collect();
            values.sort();
            assert_eq!(values, vec![100_000, 800_000], "the spent 900_000 is excluded");
            assert!(unspents.iter().all(|u| u.outpoint != spent.outpoint));
        }
        let listed = |server: &StubServer| count_requests(server, "blockchain.scripthash.listunspent");
        assert_eq!(count_requests(&servers[0], "blockchain.transaction.broadcast"), 1);
        assert_eq!(count_requests(&servers[1], "blockchain.transaction.broadcast"), 0);
        // Two lookups (P2PKH and P2PK) per read: the first server answered one read, the others one each.
        assert_eq!([listed(&servers[0]), listed(&servers[1]), listed(&servers[2])], [
            2, 2, 2
        ]);
    }

    /// A fixed-fee coin has nothing to read, in or out of a request.
    #[test]
    fn fixed_fee_coin_reads_nothing() {
        let (coin, server) = coin_on_scripted_electrum(false, TxFee::FixedPerKb(1000), false);
        block_on(with_request_fee_scope(
            &mm2_core::mm_ctx::MmCtxBuilder::default().into_mm_arc(),
            async { coin.spend_safe_amount().await.unwrap() },
        ));
        assert!(lookups(&server).is_empty());
    }
}

// ---- A withdraw's spend is recorded when its broadcast is accepted (CRD ch.38 R38.5B.9, tests T38.5B.1-T38.5B.10) ----

#[cfg(not(target_arch = "wasm32"))]
mod withdraw_recent_spent {
    use super::order_posting_latency::{count_requests, my_address, script_hashes_of_test_coin};
    use super::*;
    use crate::rpc_command::init_withdraw::{init_withdraw, withdraw_status, WithdrawCompatRpcStatus,
                                            WithdrawStatusRequest};
    use crate::utxo::rpc_clients::electrum_rpc_client::electrum_client_over_stub;
    use crate::utxo::rpc_clients::electrum_stub_server::StubServer;
    use crate::{CoinsContext, MmCoinEnum, TransactionDetails};
    use futures::compat::Future01CompatExt;
    use std::collections::HashSet;
    use std::time::{Duration, Instant};

    /// The one output the scripted backend lists, whatever happens to it.
    const LISTED_VALUE: u64 = 900_000;
    const BROADCAST: &str = "blockchain.transaction.broadcast";
    const TTL_MS: u64 = PENDING_WITHDRAW_TTL_MS;

    fn listed_outpoint() -> OutPoint {
        OutPoint {
            hash: H256::from([1; 32]),
            index: 0,
        }
    }

    /// A coin on a scripted Electrum server that keeps listing one unspent output of the coin's address
    /// (`LISTED_VALUE`, as a backend that has not indexed any broadcast would) and accepts every broadcast.
    fn coin_on_backend() -> (UtxoStandardCoin, StubServer) {
        let (p2pkh_hash, p2pk_hash) = script_hashes_of_test_coin(false);
        let (client, server) = electrum_client_over_stub(|server| {
            server.set_scripthash_result(
                "blockchain.scripthash.listunspent",
                &p2pkh_hash,
                json!([{"tx_hash": "01".repeat(32), "tx_pos": 0, "height": 10, "value": LISTED_VALUE}]),
            );
            server.set_scripthash_result("blockchain.scripthash.listunspent", &p2pk_hash, json!([]));
            server.set_result(BROADCAST, json!("ab".repeat(32)));
        });
        let fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Electrum(client), None, false);
        (utxo_coin_from_fields(fields), server)
    }

    fn withdraw_request() -> WithdrawRequest {
        WithdrawRequest {
            amount: "0.001".parse().unwrap(),
            from: None,
            to: "RQq6fWoy8aGGMLjvRfMY5mBNVm2RQxJyLa".to_string(),
            coin: TEST_COIN_NAME.into(),
            max: false,
            fee: None,
        }
    }

    fn legacy_withdraw(coin: &UtxoStandardCoin) -> TransactionDetails {
        block_on(coin.withdraw(withdraw_request()).compat()).unwrap()
    }

    /// Drives `task::withdraw::init` to its terminal status through the real task path.
    fn task_withdraw(ctx: &MmArc) -> TransactionDetails {
        let init = block_on(init_withdraw(ctx.clone(), withdraw_request())).unwrap();
        let started = Instant::now();
        loop {
            let request = WithdrawStatusRequest {
                task_id: init.task_id,
                forget_if_finished: false,
            };
            let status = block_on(withdraw_status(ctx.clone(), request)).unwrap_or_else(|e| panic!("{}", e));
            match status {
                WithdrawCompatRpcStatus::Ok(details) => return details,
                WithdrawCompatRpcStatus::Error(e) => panic!("the task withdraw failed: {:?}", e),
                _ => (),
            }
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "the task withdraw did not finish"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn tx_of(details: &TransactionDetails) -> UtxoTx { deserialize(details.tx_hex.as_slice()).unwrap() }

    fn inputs_of(tx: &UtxoTx) -> Vec<OutPoint> { tx.inputs.iter().map(|i| i.previous_output.clone()).collect() }

    /// The unspent list a spend is built from, as the withdraw path reads it.
    fn listing(coin: &UtxoStandardCoin) -> Vec<UnspentInfo> {
        let (unspents, _guard) = block_on(coin.get_unspent_ordered_list(&my_address(coin))).unwrap();
        unspents
    }

    /// The change output of a withdraw built by the tests, as the listing offers it once the spend is recorded.
    fn change_of(coin: &UtxoStandardCoin, tx: &UtxoTx) -> UnspentInfo {
        let script = Builder::build_p2pkh(&my_address(coin).hash).to_bytes();
        let (index, output) = tx
            .outputs
            .iter()
            .enumerate()
            .find(|(_, output)| output.script_pubkey == script)
            .expect("the withdraw has a change output");
        UnspentInfo {
            outpoint: OutPoint {
                hash: tx.hash(),
                index: index as u32,
            },
            value: output.value,
            height: None,
        }
    }

    fn broadcast_hex(coin: &UtxoStandardCoin, details: &TransactionDetails) -> Result<String, String> {
        block_on(coin.send_raw_tx(&hex::encode(&details.tx_hex.0)).compat())
    }

    fn record_is_empty(coin: &UtxoStandardCoin) -> bool {
        let record = block_on(coin.as_ref().recently_spent_outpoints.lock());
        record.input_to_output_map.is_empty() && record.output_to_input_map.is_empty()
    }

    fn pending_count(coin: &UtxoStandardCoin) -> usize {
        block_on(coin.as_ref().recently_spent_outpoints.lock())
            .pending_withdraws
            .len()
    }

    fn only_pending_created_ms(coin: &UtxoStandardCoin) -> u64 {
        let record = block_on(coin.as_ref().recently_spent_outpoints.lock());
        assert_eq!(record.pending_withdraws.len(), 1);
        record.pending_withdraws.values().next().unwrap().created_ms
    }

    /// T38.5B.1 (R38.5B.1): a swap-side send enters the record only once the backend has accepted the broadcast.
    #[test]
    fn swap_side_send_is_recorded_only_after_acceptance() {
        let (coin, server) = coin_on_backend();
        let to_other = TransactionOutput {
            value: 100_000,
            script_pubkey: Builder::build_p2pkh(&Address::from("RQq6fWoy8aGGMLjvRfMY5mBNVm2RQxJyLa").hash).to_bytes(),
        };
        let before = listing(&coin);
        assert_eq!(before.iter().map(|u| u.outpoint.clone()).collect::<Vec<_>>(), vec![
            listed_outpoint()
        ]);

        // The backend refuses: the record and the next listing are unchanged.
        server.set_error(BROADCAST, "txn-mempool-conflict");
        block_on(send_outputs_from_my_address_impl(coin.clone(), vec![to_other.clone()])).unwrap_err();
        assert!(record_is_empty(&coin));
        assert_eq!(listing(&coin), before);

        // The backend accepts, and keeps listing the spent input: the listing omits it and offers the change.
        server.clear_error(BROADCAST);
        let sent = block_on(send_outputs_from_my_address_impl(coin.clone(), vec![to_other])).unwrap();
        let after = listing(&coin);
        assert_eq!(after, vec![change_of(&coin, &sent)]);
    }

    /// T38.5B.2 (R38.5B.2, R38.5B.9 item 3): withdraws alone write nothing to the record.
    #[test]
    fn withdraws_alone_write_nothing_to_the_record() {
        let (coin, _server) = coin_on_backend();
        let before = listing(&coin);
        let first = tx_of(&legacy_withdraw(&coin));
        let second = tx_of(&legacy_withdraw(&coin));

        assert_eq!(inputs_of(&first), vec![listed_outpoint()]);
        assert_eq!(inputs_of(&second), vec![listed_outpoint()]);
        assert!(record_is_empty(&coin));
        assert_eq!(listing(&coin), before);
    }

    /// T38.5B.3 (R38.5B.9 items 2 and 6): an accepted broadcast of a withdraw's bytes records the spend.
    #[test]
    fn accepted_broadcast_of_a_withdraw_records_the_spend() {
        let (coin, server) = coin_on_backend();
        let first_details = legacy_withdraw(&coin);
        let first = tx_of(&first_details);
        assert_eq!(inputs_of(&first), vec![listed_outpoint()]);

        broadcast_hex(&coin, &first_details).unwrap();
        assert_eq!(count_requests(&server, BROADCAST), 1);
        assert_eq!(pending_count(&coin), 0, "the pending entry was promoted");
        // The backend still lists the input; the listing omits it and offers the change in its place.
        let change = change_of(&coin, &first);
        assert_eq!(listing(&coin), vec![change.clone()]);

        // A second withdraw excludes the first one's input and spends its change.
        let second = tx_of(&legacy_withdraw(&coin));
        assert_eq!(inputs_of(&second), vec![change.outpoint]);
    }

    /// T38.5B.3, task path: the same assertions hold for `task::withdraw::*`.
    #[test]
    fn accepted_broadcast_of_a_task_withdraw_records_the_spend() {
        let (coin, _server) = coin_on_backend();
        let ctx = MmCtxBuilder::default().into_mm_arc();
        let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();
        block_on(coins_ctx.add_coin(MmCoinEnum::UtxoCoin(coin.clone()))).unwrap();

        let first_details = task_withdraw(&ctx);
        let first = tx_of(&first_details);
        assert_eq!(inputs_of(&first), vec![listed_outpoint()]);
        assert!(record_is_empty(&coin), "building and signing writes nothing");
        assert_eq!(pending_count(&coin), 1);

        broadcast_hex(&coin, &first_details).unwrap();
        let change = change_of(&coin, &first);
        assert_eq!(listing(&coin), vec![change.clone()]);

        let second = tx_of(&task_withdraw(&ctx));
        assert_eq!(inputs_of(&second), vec![change.outpoint]);
    }

    /// The byte-carrier form of the helper behaves like the hex form.
    #[test]
    fn send_raw_tx_bytes_promotes_like_send_raw_tx() {
        let (coin, _server) = coin_on_backend();
        let details = legacy_withdraw(&coin);
        let first = tx_of(&details);

        block_on(coin.send_raw_tx_bytes(&details.tx_hex.0).compat()).unwrap();
        assert_eq!(pending_count(&coin), 0);
        assert_eq!(listing(&coin), vec![change_of(&coin, &first)]);
    }

    /// T38.5B.6 (R38.5B.9 item 2, R38.5B.6): a refused broadcast records nothing, keeps the legacy refusal shape and
    /// leaves the pending entry to age out.
    #[test]
    fn refused_broadcast_records_nothing() {
        let (coin, server) = coin_on_backend();
        let before = listing(&coin);
        let details = legacy_withdraw(&coin);

        server.set_error(BROADCAST, "txn-mempool-conflict");
        let error = broadcast_hex(&coin, &details).unwrap_err();
        assert!(error.contains("txn-mempool-conflict"), "{}", error);
        assert!(record_is_empty(&coin));
        assert_eq!(listing(&coin), before);
        assert_eq!(pending_count(&coin), 1, "the entry is still pending");
        let second = tx_of(&legacy_withdraw(&coin));
        assert_eq!(inputs_of(&second), vec![listed_outpoint()]);

        // The entry ages out.
        // The second withdraw can carry a later lock_time (the current second) and so be a second entry.
        let created = {
            let record = block_on(coin.as_ref().recently_spent_outpoints.lock());
            record.pending_withdraws.values().map(|p| p.created_ms).max().unwrap()
        };
        let mut record = block_on(coin.as_ref().recently_spent_outpoints.lock());
        assert!(!record.promote_pending_withdraw_at(created + TTL_MS, &[7, 7, 7]));
        assert!(record.pending_withdraws.is_empty());
    }

    /// T38.5B.7 (R38.5B.9 item 3): a withdraw that is never broadcast hides nothing, at once or later.
    #[test]
    fn unbroadcast_withdraw_hides_nothing_and_ages_out() {
        let (coin, _server) = coin_on_backend();
        let before = listing(&coin);
        let details = legacy_withdraw(&coin);

        assert_eq!(listing(&coin), before);
        let created = only_pending_created_ms(&coin);

        // Just short of the lifetime the entry is still there; after it, an entry made later drops it.
        {
            let mut record = block_on(coin.as_ref().recently_spent_outpoints.lock());
            record.note_pending_withdraw_at(created + TTL_MS - 1, &[1], vec![], H256::from([3; 32]), vec![]);
            assert_eq!(record.pending_withdraws.len(), 2);
            record.note_pending_withdraw_at(created + TTL_MS, &[2], vec![], H256::from([4; 32]), vec![]);
            assert_eq!(
                record.pending_withdraws.len(),
                2,
                "the withdraw's entry is gone, the two newer remain"
            );
            // An expired entry is not promoted by a broadcast that arrives late.
            assert!(!record.promote_pending_withdraw_at(created + TTL_MS, &details.tx_hex.0));
        }
        assert!(record_is_empty(&coin));
        assert_eq!(listing(&coin), before, "no phantom change output is offered");
    }

    /// T38.5B.8 (R38.5B.9 item 2): bytes the node did not build promote nothing and change nothing.
    #[test]
    fn bytes_the_node_did_not_build_promote_nothing() {
        let (coin, _server) = coin_on_backend();
        let before = listing(&coin);
        let details = legacy_withdraw(&coin);
        let mut reserialized = tx_of(&details);
        reserialized.lock_time += 1;

        // A transaction no withdraw built, and one a caller re-serialized after the withdraw.
        block_on(coin.send_raw_tx_bytes(&[1, 2, 3]).compat()).unwrap();
        block_on(coin.send_raw_tx_bytes(&serialize(&reserialized)).compat()).unwrap();

        assert!(record_is_empty(&coin));
        assert_eq!(listing(&coin), before);
        assert_eq!(pending_count(&coin), 1, "the withdraw's own entry is untouched");
    }

    /// T38.5B.9 (R38.5B.9 item 3): at most 64 pending entries are kept, the oldest dropped first, and an entry
    /// older than ten minutes is dropped when the next one is made.
    #[test]
    fn pending_entries_are_bounded_and_expire() {
        let script = Bytes::from(vec![5u8; 25]);
        let mut record = RecentlySpentOutPoints::new(script);
        let digest = |i: u8| sha256(&[i]);

        for i in 0..70u8 {
            record.note_pending_withdraw_at(1_000 + i as u64, &[i], vec![], H256::from([i; 32]), vec![]);
        }
        assert_eq!(record.pending_withdraws.len(), PENDING_WITHDRAW_CAP);
        let kept: HashSet<_> = record.pending_withdraws.keys().cloned().collect();
        let expected: HashSet<_> = (6..70u8).map(digest).collect();
        assert_eq!(kept, expected, "the six oldest were dropped");

        // Entries made within one millisecond are dropped in the order they were made.
        let mut same_ms = RecentlySpentOutPoints::new(Bytes::from(vec![5u8; 25]));
        for i in 0..65u8 {
            same_ms.note_pending_withdraw_at(500, &[i], vec![], H256::from([i; 32]), vec![]);
        }
        assert!(!same_ms.pending_withdraws.contains_key(&digest(0)));
        assert!(same_ms.pending_withdraws.contains_key(&digest(64)));

        // Ten minutes: an entry exactly that old is dropped by the next entry; a younger one stays.
        let mut aging = RecentlySpentOutPoints::new(Bytes::from(vec![5u8; 25]));
        aging.note_pending_withdraw_at(0, &[1], vec![], H256::from([1; 32]), vec![]);
        aging.note_pending_withdraw_at(1, &[2], vec![], H256::from([2; 32]), vec![]);
        aging.note_pending_withdraw_at(TTL_MS, &[3], vec![], H256::from([3; 32]), vec![]);
        let kept: HashSet<_> = aging.pending_withdraws.keys().cloned().collect();
        assert_eq!(kept, [digest(2), digest(3)].into_iter().collect::<HashSet<_>>());
    }

    /// T38.5B.10 (R38.5B.9 item 3, D38.5B.2): an entry promoted into the record does not expire.
    #[test]
    fn promoted_entry_does_not_expire() {
        let own_script = Bytes::from(vec![5u8; 25]);
        let mut record = RecentlySpentOutPoints::new(own_script.clone());
        let spent = UnspentInfo {
            outpoint: listed_outpoint(),
            value: LISTED_VALUE,
            height: Some(10),
        };
        let tx_hash = H256::from([9; 32]);
        let outputs = vec![
            TransactionOutput {
                value: 100_000,
                script_pubkey: Bytes::from(vec![6u8; 25]),
            },
            TransactionOutput {
                value: 790_000,
                script_pubkey: own_script,
            },
        ];
        record.note_pending_withdraw_at(1_000, &[1, 2, 3], vec![spent.clone()], tx_hash, outputs);
        assert!(record.promote_pending_withdraw_at(1_001, &[1, 2, 3]));

        // Far beyond the pending lifetime, other entries come and go, and the record still holds the spend.
        let much_later = 1_000 + 1_000 * TTL_MS;
        record.note_pending_withdraw_at(much_later, &[4], vec![], H256::from([4; 32]), vec![]);
        assert!(!record.promote_pending_withdraw_at(much_later, &[1, 2, 3]));
        let change = UnspentInfo {
            outpoint: OutPoint {
                hash: tx_hash,
                index: 1,
            },
            value: 790_000,
            height: None,
        };
        let replaced = record.replace_spent_outputs_with_cache([spent].into_iter().collect());
        assert_eq!(replaced, [change].into_iter().collect::<HashSet<_>>());
    }
}

// ---- time-only read of the median-time-past window (CRD ch.37 R37.5.10) -----------------------

/// The steps 2 to 4 of the median-time-past list read (R37.5.10a): the even split, the individual
/// read and the range check, on the pure function with an injectable clock and a fake header
/// fetcher, and through the Electrum client with mocked requests.
#[cfg(not(target_arch = "wasm32"))]
mod header_time_only_tests {
    use super::header_read_path_tests::{coin_from_protocol, conf_from_protocol, electrum, policy_of};
    use super::*;
    use crate::utxo::header_time_only::{check_times, even_split, read_window_times, CLOCK_SLACK_SECS,
                                        EARLIEST_PLAUSIBLE_TIME};
    use crate::utxo::rpc_clients::{ElectrumBlockHeadersRes, UtxoRpcError};
    use rpc::v1::types::Bytes as BytesJson;
    use std::sync::Mutex;

    const RIC_LIST: &str = include_str!("../../kdf_chain/src/test_data/ric_2658945_x11_v20000000.hex");
    const MEWC_MIXED: &str = include_str!("../../kdf_chain/src/test_data/mewc_2124756_x11_9native_2auxpow.hex");
    const MEWC_ALL_NATIVE: &str = include_str!("../../kdf_chain/src/test_data/mewc_2124580_x11_all_native.hex");
    const MEWC_ALL_AUXPOW: &str = include_str!("../../kdf_chain/src/test_data/mewc_2124789_x6_all_auxpow.hex");

    /// The injected clock: after every time of the fixtures below.
    const NOW: u64 = 1_800_000_000;
    const TICKER: &str = "PLUMB";
    const FROM: u64 = 4_990;

    fn served(hex: &str) -> Vec<u8> { hex::decode(hex.trim()).unwrap() }

    /// A header of `size` bytes: the standard 80 bytes (version 1, the time at byte 68), then a
    /// trailer of filler bytes up to `size`.
    fn header(time: u32, size: usize) -> Vec<u8> {
        let mut bytes = vec![0xA5u8; size];
        bytes[..4].copy_from_slice(&1u32.to_le_bytes());
        for (i, b) in bytes[4..68].iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(3).wrapping_add(1);
        }
        bytes[68..72].copy_from_slice(&time.to_le_bytes());
        bytes[72..76].copy_from_slice(&0x1d00ffffu32.to_le_bytes());
        bytes[76..80].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        bytes
    }

    /// `n` distinct plausible times in scrambled order: the median is none of the first, the last,
    /// the smallest and the largest (for `n` > 3).
    fn scrambled_times(n: usize) -> Vec<u32> {
        let m = [7usize, 11, 13, 17, 19, 23, 29]
            .into_iter()
            .find(|m| n % m != 0)
            .unwrap();
        (0..n)
            .map(|i| 1_600_000_000 + (((i * m + 3) % n) as u32) * 1_000 + 17 * i as u32)
            .collect()
    }

    fn true_median(times: &[u32]) -> u32 {
        let mut sorted = times.to_vec();
        sorted.sort_unstable();
        sorted[sorted.len() / 2]
    }

    /// The served list and the individual headers for headers of the given sizes.
    fn window(times: &[u32], sizes: &[usize]) -> (Vec<u8>, Vec<Vec<u8>>) {
        assert_eq!(times.len(), sizes.len());
        let singles: Vec<Vec<u8>> = times.iter().zip(sizes).map(|(t, s)| header(*t, *s)).collect();
        (singles.concat(), singles)
    }

    /// What the fake fetcher does for one height.
    #[derive(Clone)]
    enum Serve {
        Header(Vec<u8>),
        Fail,
    }

    struct Run {
        result: Result<Vec<u32>, MmError<UtxoRpcError>>,
        /// The heights `blockchain.block.header` was asked for.
        heights: Vec<u64>,
    }

    impl Run {
        fn ok(&self) -> Vec<u32> { self.result.as_ref().expect("the window is read").clone() }
    }

    /// Runs `read_window_times` with a fetcher that serves `singles[height - FROM]`.
    fn run_serving(policy: &HeaderReadPolicy, bytes: &[u8], count: usize, now: u64, singles: Vec<Serve>) -> Run {
        let heights = Mutex::new(Vec::new());
        let result = block_on(read_window_times(policy, TICKER, bytes, count, FROM, now, |height| {
            heights.lock().unwrap().push(height);
            let served = singles.get((height - FROM) as usize).cloned().unwrap_or(Serve::Fail);
            std::future::ready(match served {
                Serve::Header(bytes) => Ok(bytes),
                Serve::Fail => MmError::err(UtxoRpcError::Internal("no such header".to_owned())),
            })
        }));
        Run {
            result,
            heights: heights.into_inner().unwrap(),
        }
    }

    fn run(policy: &HeaderReadPolicy, bytes: &[u8], count: usize, singles: &[Vec<u8>]) -> Run {
        run_serving(
            policy,
            bytes,
            count,
            NOW,
            singles.iter().cloned().map(Serve::Header).collect(),
        )
    }

    fn conf_default() -> UtxoCoinConf { conf_from_protocol(json!({ "type": "UTXO" }), None).unwrap() }

    fn conf_switch_off() -> UtxoCoinConf { conf_from_protocol(json!({ "type": "UTXO" }), Some(&[][..])).unwrap() }

    fn logged(conf: &UtxoCoinConf) -> bool { conf.header_time_only_logged.load(Ordering::Relaxed) }

    fn expect_heights(run: &Run, count: usize) {
        let mut heights = run.heights.clone();
        heights.sort_unstable();
        assert_eq!(heights, (FROM..FROM + count as u64).collect::<Vec<_>>());
    }

    fn error_text(result: Result<Vec<u32>, MmError<UtxoRpcError>>) -> String {
        match result.expect_err("the read must be refused").into_inner() {
            UtxoRpcError::InvalidResponse(text) => text,
            other => panic!("expected a typed invalid-response error, got {other:?}"),
        }
    }

    // ---- even split (R37.5.10a step 2) ----

    #[test]
    fn eleven_84_byte_headers_are_read_by_the_even_split_and_logged_once() {
        let times = scrambled_times(11);
        let median = true_median(&times);
        assert!(median != times[0] && median != times[10]);
        assert!(median != *times.iter().min().unwrap() && median != *times.iter().max().unwrap());
        let (bytes, singles) = window(&times, &[84; 11]);
        assert_eq!(bytes.len(), 924);
        let conf = conf_default();
        let policy = HeaderReadPolicy::for_conf(&conf);
        assert!(!logged(&conf));
        let first = run(&policy, &bytes, 11, &singles);
        assert_eq!(first.ok(), times);
        assert!(first.heights.is_empty(), "the split makes no header request");
        assert!(logged(&conf), "the info line is recorded after the first split read");
        // A second read of the same coin and activation is read the same way.
        let second = run(&policy, &bytes, 11, &singles);
        assert_eq!(second.ok(), times);
        assert!(logged(&conf));
    }

    #[test]
    fn the_split_function_reads_byte_68_at_strides_80_84_112_and_120() {
        for stride in [80usize, 84, 112, 120] {
            let times = scrambled_times(11);
            let (bytes, _) = window(&times, &vec![stride; 11]);
            assert_eq!(even_split(&bytes, 11), Some((stride, times.clone())), "{stride}");
            assert_eq!(true_median(&times), true_median(&even_split(&bytes, 11).unwrap().1));
        }
    }

    #[test]
    fn strides_below_80_and_above_4096_go_to_step_3() {
        for stride in [79usize, 4097] {
            let times = scrambled_times(11);
            let singles: Vec<Vec<u8>> = times.iter().map(|t| header(*t, 80)).collect();
            let bytes = vec![0x11u8; 11 * stride];
            assert_eq!(even_split(&bytes, 11), None, "{stride}");
            let conf = conf_default();
            let read = run(&HeaderReadPolicy::for_conf(&conf), &bytes, 11, &singles);
            assert_eq!(read.ok(), times, "{stride}");
            expect_heights(&read, 11);
            assert!(logged(&conf));
        }
        // The edges of the allowed range are split.
        for stride in [80usize, 4096] {
            assert_eq!(even_split(&vec![0u8; 3 * stride], 3).map(|(s, _)| s), Some(stride));
        }
    }

    #[test]
    fn lists_that_step_1_reads_never_reach_the_split() {
        let conf = conf_default();
        let policy = HeaderReadPolicy::for_conf(&conf);
        // Eleven 80-byte headers: the standard layout. Eleven 112-byte Riecoin headers and eleven
        // 120-byte KawPoW headers: the structural fallback of R37.5.9e.
        let eighty = window(&scrambled_times(11), &[80; 11]).0;
        let ric = served(RIC_LIST);
        let kawpow = served(MEWC_ALL_NATIVE);
        for (name, bytes) in [("80", eighty), ("112", ric), ("120", kawpow)] {
            let read = run(&policy, &bytes, 11, &[]);
            assert!(read.result.is_ok(), "{name}");
            assert!(read.heights.is_empty(), "{name}");
            assert!(!logged(&conf), "{name}: step 1 read it, so there is no step line");
        }
        // Six merged-mined 80-byte headers: the standard layout again.
        let auxpow = served(MEWC_ALL_AUXPOW);
        assert!(run(&policy, &auxpow, 6, &[]).result.is_ok());
        assert!(!logged(&conf));
    }

    #[test]
    fn a_zcash_family_list_is_consumed_by_step_1() {
        // Eleven version-4 (Equihash) headers: 140 header bytes, the solution length and 1344
        // solution bytes, 1487 bytes each. The time sits at byte 100, not at 68.
        let times = scrambled_times(11);
        let mut bytes = Vec::new();
        for time in &times {
            let mut h = vec![0x42u8; 140];
            h[..4].copy_from_slice(&4u32.to_le_bytes());
            h[100..104].copy_from_slice(&time.to_le_bytes());
            h.extend_from_slice(&[0xfd, 0x40, 0x05]);
            h.extend(std::iter::repeat(0x7e).take(1344));
            assert_eq!(h.len(), 1487);
            bytes.extend(h);
        }
        let conf = conf_default();
        let read = run(&HeaderReadPolicy::for_conf(&conf), &bytes, 11, &[]);
        assert_eq!(read.ok(), times);
        assert!(read.heights.is_empty());
        assert!(!logged(&conf));
    }

    // ---- individual read (R37.5.10a step 3) ----

    #[test]
    fn a_window_that_straddles_the_size_change_is_read_header_by_header() {
        let times = scrambled_times(11);
        let sizes = [80, 80, 80, 80, 80, 80, 80, 80, 84, 84, 84];
        let (bytes, singles) = window(&times, &sizes);
        assert_eq!(bytes.len(), 892);
        assert_eq!(even_split(&bytes, 11), None);
        for conf in [conf_default(), conf_switch_off()] {
            let read = run(&HeaderReadPolicy::for_conf(&conf), &bytes, 11, &singles);
            let got = read.ok();
            assert_eq!(got, times);
            assert_eq!(true_median(&got), true_median(&times));
            expect_heights(&read, 11);
            assert!(logged(&conf));
        }
    }

    #[test]
    fn no_odd_count_with_mixed_80_and_84_byte_headers_is_ever_split() {
        for n in (1..=25usize).step_by(2) {
            for b in 1..n {
                let times = scrambled_times(n);
                let sizes: Vec<usize> = (0..n).map(|i| if i < n - b { 80 } else { 84 }).collect();
                let (bytes, singles) = window(&times, &sizes);
                assert_eq!(
                    even_split(&bytes, n),
                    None,
                    "n={n} b={b}: divisibility sends it to step 3"
                );
                let conf = conf_default();
                let read = run(&HeaderReadPolicy::for_conf(&conf), &bytes, n, &singles);
                assert_eq!(read.ok(), times, "n={n} b={b}");
                expect_heights(&read, n);
            }
        }
        // The default window of eleven is among them, and a single header is read directly.
        let times = scrambled_times(1);
        let (bytes, singles) = window(&times, &[84]);
        assert_eq!(even_split(&bytes, 1).map(|(s, _)| s), Some(84));
        assert!(run(&HeaderReadPolicy::standard(), &bytes, 1, &singles).result.is_ok());
    }

    #[test]
    fn even_counts_are_read_individually_even_when_uniform() {
        for n in (2..=24usize).step_by(2) {
            let times = scrambled_times(n);
            let (bytes, singles) = window(&times, &vec![84; n]);
            assert_eq!(even_split(&bytes, n), None, "n={n}");
            let read = run(&HeaderReadPolicy::standard(), &bytes, n, &singles);
            assert_eq!(read.ok(), times, "n={n}");
            expect_heights(&read, n);
        }
        // n = 4 with one 84-byte header and three 80-byte ones: 324 bytes, stride 81, where a split
        // would read the wrong bytes.
        let times = scrambled_times(4);
        let (bytes, singles) = window(&times, &[80, 80, 80, 84]);
        assert_eq!(bytes.len(), 324);
        assert_eq!(even_split(&bytes, 4), None);
        let read = run(&HeaderReadPolicy::standard(), &bytes, 4, &singles);
        assert_eq!(read.ok(), times);
        expect_heights(&read, 4);
    }

    #[test]
    fn the_ambiguity_of_the_structural_fallback_is_read_individually_and_not_split() {
        // Five headers, 560 bytes: four 120-byte native headers and one 80-byte merged-mined header,
        // which is also 5 x 112 bytes. Zero bytes everywhere, so a split would read zero times.
        let mut bytes = vec![0u8; 560];
        bytes[481] = 0x01;
        let conf = conf_default();
        let policy = HeaderReadPolicy::for_conf(&conf);
        assert!(matches!(
            policy.read_list(&bytes, 5),
            Err(crate::utxo::header_layout_fallback::FallbackReadError::Ambiguous(_))
        ));
        assert_eq!(
            even_split(&bytes, 5).map(|(s, _)| s),
            Some(112),
            "the split would apply"
        );
        let times = scrambled_times(5);
        let singles: Vec<Vec<u8>> = times.iter().map(|t| header(*t, 80)).collect();
        let read = run(&policy, &bytes, 5, &singles);
        assert_eq!(read.ok(), times);
        expect_heights(&read, 5);
    }

    #[test]
    fn a_failed_request_or_a_short_header_fails_step_3_with_a_typed_error_and_no_median() {
        let times = scrambled_times(11);
        let (bytes, singles) = window(&times, &[80, 80, 80, 80, 80, 80, 80, 80, 84, 84, 84]);
        let policy = HeaderReadPolicy::standard();
        let mut failing: Vec<Serve> = singles.iter().cloned().map(Serve::Header).collect();
        failing[4] = Serve::Fail;
        let text = error_text(run_serving(&policy, &bytes, 11, NOW, failing).result);
        assert!(
            text.contains(TICKER) && text.contains("892") && text.contains("11"),
            "{text}"
        );
        assert!(
            text.contains("request") && text.contains(&(FROM + 4).to_string()),
            "{text}"
        );

        let mut short: Vec<Serve> = singles.iter().cloned().map(Serve::Header).collect();
        short[7] = Serve::Header(singles[7][..79].to_vec());
        let text = error_text(run_serving(&policy, &bytes, 11, NOW, short).result);
        assert!(
            text.contains(TICKER) && text.contains("892") && text.contains("shorter than 80"),
            "{text}"
        );
        assert!(text.contains("79"), "{text}");
    }

    // ---- range check (R37.5.10d, step 4) ----

    #[test]
    fn bytes_that_are_not_a_time_at_68_are_refused() {
        let policy = HeaderReadPolicy::standard();
        for stride in [100usize, 1487] {
            // A field of zeros.
            let zeros = vec![0u8; 11 * stride];
            let text = error_text(run(&policy, &zeros, 11, &[]).result);
            assert!(text.contains(TICKER) && text.contains("below"), "{stride}: {text}");
            assert!(text.contains(&(11 * stride).to_string()), "{stride}: {text}");
            // A hash at byte 68: one that reads as a time in the far future, one as a time of
            // 1970.
            for (hash_start, check) in [
                ([0xDEu8, 0xAD, 0xBE, 0xEF], "above"),
                ([0x00, 0x01, 0x00, 0x00], "below"),
            ] {
                let mut list = vec![0u8; 11 * stride];
                for header in list.chunks_mut(stride) {
                    header[68..72].copy_from_slice(&hash_start);
                    header[72..100].fill(0x5c);
                }
                let text = error_text(run(&policy, &list, 11, &[]).result);
                assert!(text.contains(check), "{stride}/{check}: {text}");
            }
        }
    }

    fn list_with_times(times: &[u32], stride: usize) -> Vec<u8> { window(times, &vec![stride; times.len()]).0 }

    #[test]
    fn the_range_check_has_exact_edges() {
        let policy = HeaderReadPolicy::standard();
        let low = EARLIEST_PLAUSIBLE_TIME as u32;
        // all equal to 1230768000: accepted; one of 1230767999: rejected.
        assert!(run(&policy, &list_with_times(&[low; 11], 100), 11, &[]).result.is_ok());
        let mut one_low = vec![low; 11];
        one_low[6] = low - 1;
        let text = error_text(run(&policy, &list_with_times(&one_low, 100), 11, &[]).result);
        assert!(
            text.contains(&(low - 1).to_string()) && text.contains("below"),
            "{text}"
        );
        // all equal to clock + 86400: accepted; one of clock + 86401: rejected, naming the check, the
        // offending time and the clock.
        let high = (NOW + CLOCK_SLACK_SECS) as u32;
        assert!(run(&policy, &list_with_times(&[high; 11], 100), 11, &[]).result.is_ok());
        let mut one_high = vec![high; 11];
        one_high[0] = high + 1;
        let text = error_text(run(&policy, &list_with_times(&one_high, 100), 11, &[]).result);
        assert!(text.contains("above the node clock"), "{text}");
        assert!(
            text.contains(&(high + 1).to_string()) && text.contains(&NOW.to_string()),
            "{text}"
        );
        // The same on the function itself.
        assert!(check_times(TICKER, 1, 1, &[low], NOW).is_ok());
        assert!(check_times(TICKER, 1, 1, &[low - 1], NOW).is_err());
        assert!(check_times(TICKER, 1, 1, &[high], NOW).is_ok());
        assert!(check_times(TICKER, 1, 1, &[high + 1], NOW).is_err());
    }

    #[test]
    fn a_drop_of_more_than_a_day_between_neighbours_is_accepted() {
        let policy = HeaderReadPolicy::standard();
        let mut times = scrambled_times(11);
        times[5] = times[4] - 3 * 86_400;
        times[8] = times[7] - 2 * 86_400;
        let got = run(&policy, &list_with_times(&times, 100), 11, &[]).ok();
        assert_eq!(got, times);
    }

    // ---- switch and key (R37.5.10f) ----

    #[test]
    fn with_the_switch_off_a_riecoin_list_is_read_by_the_split_and_by_step_1_otherwise() {
        let ric = served(RIC_LIST);
        let expected: Vec<u32> = chain::BlockHeader::list_from_served_bytes(&ric, 11, CoinVariant::Riecoin)
            .unwrap()
            .into_iter()
            .map(|h| h.time)
            .collect();
        // Switch `[]`: step 1 rejects the 112-byte headers; the split reads the low 32 bits at 68.
        let off = conf_switch_off();
        let read = run(&HeaderReadPolicy::for_conf(&off), &ric, 11, &[]);
        assert_eq!(read.ok(), expected);
        assert!(read.heights.is_empty());
        assert!(logged(&off));
        // Default switch: step 1 reads it and there is no step line.
        let default = conf_default();
        let read = run(&HeaderReadPolicy::for_conf(&default), &ric, 11, &[]);
        assert_eq!(read.ok(), expected);
        assert!(!logged(&default));
    }

    #[test]
    fn with_the_switch_off_an_interleaved_meowcoin_window_is_read_header_by_header() {
        let mixed = served(MEWC_MIXED);
        let headers = chain::BlockHeader::list_from_served_bytes(&mixed, 11, CoinVariant::KawpowAuxpowMixed).unwrap();
        let expected: Vec<u32> = headers.iter().map(|h| h.time).collect();
        let singles: Vec<Vec<u8>> = headers.iter().map(|h| h.raw().to_vec()).collect();
        assert_eq!(singles.concat(), mixed);
        assert_ne!(mixed.len() % 11, 0);
        let off = conf_switch_off();
        let read = run(&HeaderReadPolicy::for_conf(&off), &mixed, 11, &singles);
        assert_eq!(read.ok(), expected);
        expect_heights(&read, 11);
        assert!(logged(&off));
        // Default switch: the mixed layout of step 1 reads it.
        let default = conf_default();
        let read = run(&HeaderReadPolicy::for_conf(&default), &mixed, 11, &singles);
        assert_eq!(read.ok(), expected);
        assert!(read.heights.is_empty() && !logged(&default));
    }

    #[test]
    fn a_pinned_layout_that_rejects_the_list_goes_on_to_steps_2_to_4() {
        let conf = conf_from_protocol(json!({ "type": "UTXO", "header_layout": "riecoin" }), None).unwrap();
        assert!(conf.header_layout.is_some());
        let times = scrambled_times(11);
        let (bytes, singles) = window(&times, &[84; 11]);
        let read = run(&HeaderReadPolicy::for_conf(&conf), &bytes, 11, &singles);
        assert_eq!(read.ok(), times);
        assert!(logged(&conf));
        // A list the pinned layout reads is read by step 1.
        let ric = served(RIC_LIST);
        let conf = conf_from_protocol(json!({ "type": "UTXO", "header_layout": "riecoin" }), None).unwrap();
        assert!(run(&HeaderReadPolicy::for_conf(&conf), &ric, 11, &[]).result.is_ok());
        assert!(!logged(&conf));
    }

    // ---- through the Electrum client ----

    fn mock_header_list(list: &[u8], count: u64) {
        let list = list.to_vec();
        ElectrumClient::blockchain_block_headers.mock_safe(move |_, _, _| {
            let res: ElectrumBlockHeadersRes =
                json::from_value(json!({ "count": count, "hex": hex::encode(&list), "max": 2016 })).unwrap();
            MockResult::Return(Box::new(futures01::future::ok(res)))
        });
    }

    /// `blockchain.block.header` serves `singles[height - 4990]` (the window of `get_current_mtp` at
    /// tip 5000), and the heights asked for are recorded.
    fn mock_headers(singles: Vec<Option<Vec<u8>>>) -> Arc<Mutex<Vec<u64>>> {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let recorded = asked.clone();
        ElectrumClient::blockchain_block_header.mock_safe(move |_, height| {
            recorded.lock().unwrap().push(height);
            let served = singles.get((height - FROM) as usize).cloned().flatten();
            let result = match served {
                Some(bytes) => Ok(BytesJson::from(bytes)),
                None => Err(common::jsonrpc_client::JsonRpcError {
                    client_info: "test".to_owned(),
                    request: common::jsonrpc_client::JsonRpcRequestEnum::Single(
                        common::jsonrpc_client::JsonRpcRequest {
                            jsonrpc: "2.0".to_owned(),
                            id: "0".to_owned(),
                            method: "blockchain.block.header".to_owned(),
                            params: Vec::new(),
                        },
                    ),
                    error: common::jsonrpc_client::JsonRpcErrorType::Transport("no such header".to_owned()),
                }),
            };
            MockResult::Return(Box::new(futures01::future::result(result)))
        });
        asked
    }

    fn mtp(coin: &UtxoStandardCoin) -> Result<u32, String> {
        ElectrumClient::get_block_count.mock_safe(|_| MockResult::Return(Box::new(futures01::future::ok(5000u64))));
        block_on(coin.get_current_mtp()).map_err(|e| format!("{e:?}"))
    }

    #[test]
    fn the_client_reads_a_bitcoin_atom_shaped_list_by_the_split() {
        let coin = coin_from_protocol(json!({ "type": "UTXO", "chain_variant": "BTC" }), None);
        let times = scrambled_times(11);
        let (bytes, _) = window(&times, &[84; 11]);
        mock_header_list(&bytes, 11);
        let asked = mock_headers(vec![]);
        assert_eq!(mtp(&coin), Ok(true_median(&times)));
        assert!(asked.lock().unwrap().is_empty());
        assert!(coin.as_ref().conf.header_time_only_logged.load(Ordering::Relaxed));
        // The window the client asked for is the same one it would split.
        let _ = (electrum(&coin), policy_of(&coin));
    }

    #[test]
    fn the_client_reads_a_fork_crossing_window_with_eleven_header_requests() {
        let coin = coin_from_protocol(json!({ "type": "UTXO", "chain_variant": "BTC" }), None);
        let times = scrambled_times(11);
        let sizes = [80, 80, 80, 80, 80, 80, 80, 80, 84, 84, 84];
        let (bytes, singles) = window(&times, &sizes);
        mock_header_list(&bytes, 11);
        let asked = mock_headers(singles.into_iter().map(Some).collect());
        assert_eq!(mtp(&coin), Ok(true_median(&times)));
        let mut heights = asked.lock().unwrap().clone();
        heights.sort_unstable();
        assert_eq!(heights, (4990..=5000).collect::<Vec<_>>());
        assert!(coin.as_ref().conf.header_time_only_logged.load(Ordering::Relaxed));
    }

    #[test]
    fn the_client_fails_the_query_when_a_header_request_fails_or_a_header_is_short() {
        let coin = coin_from_protocol(json!({ "type": "UTXO", "chain_variant": "BTC" }), None);
        let times = scrambled_times(11);
        let sizes = [80, 80, 80, 80, 80, 80, 80, 80, 84, 84, 84];
        let (bytes, singles) = window(&times, &sizes);
        mock_header_list(&bytes, 11);
        let mut served: Vec<Option<Vec<u8>>> = singles.iter().cloned().map(Some).collect();
        served[3] = None;
        mock_headers(served);
        let err = mtp(&coin).unwrap_err();
        assert!(
            err.contains("InvalidResponse") && err.contains(TICKER) && err.contains("request"),
            "{err}"
        );
        let mut served: Vec<Option<Vec<u8>>> = singles.iter().cloned().map(Some).collect();
        served[9] = Some(singles[9][..40].to_vec());
        mock_headers(served);
        let err = mtp(&coin).unwrap_err();
        assert!(
            err.contains("InvalidResponse") && err.contains("shorter than 80"),
            "{err}"
        );
        assert!(!coin.as_ref().conf.header_time_only_logged.load(Ordering::Relaxed));
    }
}

/// CRD ch.28 R16a, T4a (f): the coin's own check of a protocol-information value reads it under the bounds.
#[cfg(not(target_arch = "wasm32"))]
mod t4a_bounded_reading_tests {
    use super::*;
    use common::msgpack_bounds::{check_msgpack_bounds, dropped_peer_values, max_nesting_depth, MAX_PEER_NESTING};

    fn coin() -> UtxoStandardCoin {
        utxo_coin_for_test(UtxoRpcClientEnum::Native(native_client_for_test()), None, false)
    }

    fn supported(coin: &UtxoStandardCoin, info: &[u8]) -> bool {
        utxo_common::is_coin_protocol_supported(coin, &Some(info.to_vec()))
    }

    /// `bytes`, a map, with one more key whose value is `levels` one-element arrays around nil.
    fn map_with_extra_key(bytes: &[u8], levels: usize) -> Vec<u8> {
        let mut out = bytes.to_vec();
        assert_eq!(out[0] & 0xf0, 0x80, "a fixmap");
        out[0] += 1;
        out.extend_from_slice(&[0xa5, b'e', b'x', b't', b'r', b'a']);
        out.extend(vec![0x91; levels]);
        out.push(0xc0);
        out
    }

    #[test]
    fn t4a_f_protocol_info_of_the_real_formats_is_unchanged() {
        let coin = coin();
        for format in [
            UtxoAddressFormat::Standard,
            UtxoAddressFormat::Segwit,
            UtxoAddressFormat::CashAddress {
                network: "bitcoincash".into(),
                pub_addr_prefix: 0,
                p2sh_addr_prefix: 5,
            },
        ] {
            let info = rmp_serde::to_vec(&format).unwrap();
            assert_eq!(check_msgpack_bounds(&info), Ok(info.len()));
            assert!(max_nesting_depth(&info) <= MAX_PEER_NESTING / 2);
            assert!(supported(&coin, &info), "{format:?}");

            // Arbitrary bytes after the first value are ignored.
            for tail in [&[0xc1][..], &[0xdd, 0xff, 0xff, 0xff, 0xff], &[0x91; 200], &[0x00]] {
                let mut with_tail = info.clone();
                with_tail.extend_from_slice(tail);
                assert!(
                    supported(&coin, &with_tail),
                    "{format:?} with a tail of {} bytes",
                    tail.len()
                );
            }
        }

        // What does not decode is still not supported.
        assert!(!supported(&coin, &[]));
        assert!(!supported(&coin, &[0xc0]));
        assert!(!supported(&coin, b"nonsense"));
        // `None` keeps its branch: a standard-format coin supports it.
        assert!(utxo_common::is_coin_protocol_supported(&coin, &None));
    }

    #[test]
    fn t4a_f_protocol_info_beyond_the_bound_is_not_supported() {
        let coin = coin();
        let named = rmp_serde::to_vec_named(&UtxoAddressFormat::Segwit).unwrap();
        assert!(supported(&coin, &map_with_extra_key(&named, MAX_PEER_NESTING - 1)));

        let before = dropped_peer_values();
        assert!(!supported(&coin, &map_with_extra_key(&named, MAX_PEER_NESTING)));
        assert!(!supported(&coin, &map_with_extra_key(&named, 10_000)));
        let mut declared = vec![0xdd];
        declared.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
        assert!(!supported(&coin, &declared));
        assert!(dropped_peer_values() >= before + 3);
    }
}

/// CRD ch.05 R36, T16: whether the key a UTXO coin's legacy swaps announce is the key it pays from.
mod swap_key_agreement_tests {
    use super::*;
    use crate::rpc_command::get_my_address::PathToAddress;
    use crate::{SwapKeyDisagreement, SwapOps};
    use crypto::CryptoCtx;

    const TEST_MNEMONIC: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    /// A coin of the test fields whose policy is the given one.
    fn coin_with(
        policy: PrivKeyPolicy<KeyPair>,
        derivation_method: Option<DerivationMethod<Address, UtxoHDWallet>>,
    ) -> UtxoStandardCoin {
        let client = native_client_for_test();
        let mut fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(client), None, false);
        fields.priv_key_policy = policy;
        if let Some(derivation_method) = derivation_method {
            fields.derivation_method = derivation_method;
        }
        utxo_coin_from_fields(fields)
    }

    fn hd_wallet() -> UtxoHDWallet {
        UtxoHDWallet {
            hd_wallet_storage: HDWalletCoinStorage::default(),
            address_format: UtxoAddressFormat::Standard,
            derivation_path: HDPathToCoin::from_str("m/44'/141'").unwrap(),
            accounts: HDAccountsMutex::new(HDAccountsMap::new()),
            gap_limit: 3,
        }
    }

    /// The HD policy of a coin activated at `path_to_address`.
    fn hd_policy(path_to_address: PathToAddress) -> PrivKeyPolicy<KeyPair> {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        CryptoCtx::init_with_global_hd_account(ctx.clone(), TEST_MNEMONIC).unwrap();
        let global_hd = match PrivKeyBuildPolicy::detect_priv_key_policy(&ctx).unwrap() {
            PrivKeyBuildPolicy::GlobalHDAccount(global_hd) => global_hd,
            _ => panic!("expected a global-HD login"),
        };
        PrivKeyPolicy::HDWallet {
            activated_key: key_pair_from_seed("activated key of the test").unwrap(),
            bip39_secp_priv_key: global_hd.root_priv_key().clone(),
            path_to_address,
        }
    }

    fn node_pubkey() -> keys::Public { *key_pair_from_seed("the passphrase of the test").unwrap().public() }

    /// T16 after §5.12C (R41 item 2, T24): a coin enabled with a single key that is not the node's persistent
    /// key (legacy `enable` or `electrum`, or `IguanaPrivKey`, under a global-HD login) announces its own key
    /// and pays from it, so it agrees.
    #[test]
    fn a_single_key_other_than_the_node_key_agrees() {
        let coin = coin_with(
            PrivKeyPolicy::KeyPair(key_pair_from_seed("the internal key of the test").unwrap()),
            None,
        );
        assert_eq!(coin.swap_key_disagreement(&node_pubkey()), None);
    }

    /// T18: a single key that is the node's persistent key (BCH activated together with its tokens) agrees.
    #[test]
    fn a_single_key_that_is_the_node_key_agrees() {
        let coin = coin_with(
            PrivKeyPolicy::KeyPair(key_pair_from_seed("the passphrase of the test").unwrap()),
            None,
        );
        assert_eq!(coin.swap_key_disagreement(&node_pubkey()), None);
    }

    /// T16: RIN activated at its first address through the HD route agrees.
    #[test]
    fn an_hd_coin_at_the_first_address_agrees() {
        let coin = coin_with(
            hd_policy(PathToAddress::default()),
            Some(DerivationMethod::HDWallet(hd_wallet())),
        );
        assert_eq!(coin.swap_key_disagreement(&node_pubkey()), None);
    }

    /// T16 after §5.12C (R42, T24): RIN activated at account 0, External, address 1, or in another account and
    /// chain, funds from the address of the key at `path_to_address`, so it agrees.
    #[test]
    fn an_hd_coin_at_another_address_agrees() {
        let at_address_1 = PathToAddress {
            account_id: 0,
            chain: Bip44Chain::External,
            address_id: 1,
        };
        let coin = coin_with(hd_policy(at_address_1), Some(DerivationMethod::HDWallet(hd_wallet())));
        assert_eq!(coin.swap_key_disagreement(&node_pubkey()), None);

        let in_account_2_internal_5 = PathToAddress {
            account_id: 2,
            chain: Bip44Chain::Internal,
            address_id: 5,
        };
        let coin = coin_with(
            hd_policy(in_account_2_internal_5),
            Some(DerivationMethod::HDWallet(hd_wallet())),
        );
        assert_eq!(coin.swap_key_disagreement(&node_pubkey()), None);
    }

    /// T24 (R44): the guard still refuses a disagreement it is given. A coin whose single key is not the key of
    /// the first address of the HD account it funds from announces the one and pays from the other.
    #[test]
    fn the_guard_still_refuses_a_constructed_disagreement() {
        let hd_account = UtxoHDAccount {
            account_id: 0,
            extended_pubkey: Secp256k1ExtendedPublicKey::from_str(
                "xpub6DEHSksajpRPM59RPw7Eg6PKdU7E2ehxJWtYdrfQ6JFmMGBsrR6jA78ANCLgzKYm4s5UqQ4ydLEYPbh3TRVvn5oAZVtWfi4qJLMntpZ8uGJ",
            )
            .unwrap(),
            account_derivation_path: HDPathToAccount::from_str("m/44'/141'/0'").unwrap(),
            external_addresses_number: 1,
            internal_addresses_number: 0,
        };
        let wallet = UtxoHDWallet {
            hd_wallet_storage: HDWalletCoinStorage::default(),
            address_format: UtxoAddressFormat::Standard,
            derivation_path: HDPathToCoin::from_str("m/44'/141'").unwrap(),
            accounts: HDAccountsMutex::new(HDAccountsMap::from([(0, hd_account)])),
            gap_limit: 3,
        };
        let coin = coin_with(
            PrivKeyPolicy::KeyPair(key_pair_from_seed("the internal key of the test").unwrap()),
            Some(DerivationMethod::HDWallet(wallet)),
        );
        assert_eq!(
            coin.swap_key_disagreement(&node_pubkey()),
            Some(SwapKeyDisagreement::SingleKey)
        );
    }

    /// R44, fail closed: when the funding address cannot be established, the guard answers a disagreement and
    /// does not let the swap start. Here the single key of the coin has an HD wallet with no account `0`, and
    /// then one whose accounts are locked.
    #[test]
    fn the_guard_fails_closed_when_the_funding_address_cannot_be_established() {
        let coin = coin_with(
            PrivKeyPolicy::KeyPair(key_pair_from_seed("the internal key of the test").unwrap()),
            Some(DerivationMethod::HDWallet(hd_wallet())),
        );
        assert_eq!(
            coin.swap_key_disagreement(&node_pubkey()),
            Some(SwapKeyDisagreement::SingleKey)
        );

        let fields: &UtxoCoinFields = coin.as_ref();
        let wallet = match &fields.derivation_method {
            DerivationMethod::HDWallet(wallet) => wallet,
            _ => panic!("an HD wallet"),
        };
        let _held = block_on(wallet.accounts.lock());
        assert_eq!(
            coin.swap_key_disagreement(&node_pubkey()),
            Some(SwapKeyDisagreement::SingleKey)
        );
    }

    /// T18: a coin activated with the Trezor policy is out of the rule's scope.
    #[test]
    fn a_trezor_coin_is_exempt() {
        let coin = coin_with(PrivKeyPolicy::Trezor, None);
        assert_eq!(coin.swap_key_disagreement(&node_pubkey()), None);
    }

    /// An HD key held with a single derived address funds from that address itself.
    #[test]
    fn an_hd_key_with_a_single_derived_address_agrees() {
        let at_address_1 = PathToAddress {
            account_id: 0,
            chain: Bip44Chain::External,
            address_id: 1,
        };
        let coin = coin_with(hd_policy(at_address_1), None);
        assert_eq!(coin.swap_key_disagreement(&node_pubkey()), None);
    }
}

/// CRD ch.05 §5.12C (R41 item 1 and 2, R42; T20, T21, T22, T24): one address and one key per UTXO swap leg.
mod hd_coin_swap_key_tests {
    use super::*;
    use crate::rpc_command::get_my_address::PathToAddress;
    use crate::utxo::utxo_builder::derive_activated_hd_secret;
    use crate::utxo::utxo_common::get_htlc_pubkey_v2;
    use crate::SwapOps;
    use crypto::privkey::key_pair_from_secret;
    use crypto::{CryptoCtx, DerivationPath, GlobalHDAccountArc};

    const TEST_MNEMONIC: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    fn global_hd() -> GlobalHDAccountArc {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        CryptoCtx::init_with_global_hd_account(ctx.clone(), TEST_MNEMONIC).unwrap();
        match PrivKeyBuildPolicy::detect_priv_key_policy(&ctx).unwrap() {
            PrivKeyBuildPolicy::GlobalHDAccount(global_hd) => global_hd,
            _ => panic!("expected a global-HD login"),
        }
    }

    fn at_address(address_id: u32) -> PathToAddress {
        PathToAddress {
            account_id: 0,
            chain: Bip44Chain::External,
            address_id,
        }
    }

    /// The key at `m/44'/141'/0'/0/<address_id>` of the test wallet, derived by path.
    fn key_at(address_id: u32) -> KeyPair {
        let path = DerivationPath::from_str(&format!("m/44'/141'/0'/0/{}", address_id)).unwrap();
        let secret = global_hd().derive_secp256k1_secret(&path).unwrap();
        key_pair_from_secret(secret.as_slice()).unwrap()
    }

    fn hd_wallet() -> UtxoHDWallet {
        UtxoHDWallet {
            hd_wallet_storage: HDWalletCoinStorage::default(),
            address_format: UtxoAddressFormat::Standard,
            derivation_path: HDPathToCoin::from_str("m/44'/141'").unwrap(),
            accounts: HDAccountsMutex::new(HDAccountsMap::new()),
            gap_limit: 3,
        }
    }

    /// The fields of a coin activated through the HD route at `path_to_address`, built the way the builder
    /// builds them: the activated key is the key at the path, derived from the same wallet.
    fn hd_fields_at(path_to_address: PathToAddress) -> UtxoCoinFields {
        let client = native_client_for_test();
        let mut fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(client), None, false);
        let global_hd = global_hd();
        let coin_path = HDPathToCoin::from_str("m/44'/141'").unwrap();
        let secret = derive_activated_hd_secret(&global_hd, &coin_path, &path_to_address).unwrap();
        fields.priv_key_policy = PrivKeyPolicy::HDWallet {
            activated_key: key_pair_from_secret(secret.as_slice()).unwrap(),
            bip39_secp_priv_key: global_hd.root_priv_key().clone(),
            path_to_address,
        };
        fields.derivation_method = DerivationMethod::HDWallet(hd_wallet());
        fields
    }

    fn address_of(fields: &UtxoCoinFields, key: &KeyPair) -> Address {
        address_from_pubkey(
            key.public(),
            fields.conf.pub_addr_prefix,
            fields.conf.pub_t_addr_prefix,
            fields.conf.checksum_type,
            fields.conf.bech32_hrp.clone(),
            UtxoAddressFormat::Standard,
        )
    }

    /// T20 (R41 item 1, R42): RIN activated at account 0, External, address 1 funds from the address of that
    /// key and signs with its full derivation path, not from the first address.
    #[test]
    fn t20_the_funding_address_is_the_one_at_path_to_address() {
        let fields = hd_fields_at(at_address(1));
        let expected = address_of(&fields, &key_at(1));
        assert_ne!(expected, address_of(&fields, &key_at(0)));

        let sender = block_on(active_utxo_sender_address(&fields, "test")).unwrap();
        assert_eq!(sender.address, expected);
        assert_eq!(sender.pubkey, Some(*key_at(1).public()));
        assert_eq!(
            sender.derivation_path.unwrap().to_string(),
            "m/44'/141'/0'/0/1".to_owned()
        );
        // The key that signs is the key at the path.
        let sender = block_on(active_utxo_sender_address(&fields, "test")).unwrap();
        assert_eq!(
            active_sender_key_pair(&fields, &sender).unwrap().public(),
            key_at(1).public()
        );
    }

    /// T20 (R42): the fee estimate of `trade_preimage` is taken from the outputs of that address.
    #[test]
    fn t20_trade_preimage_estimates_from_the_address_at_path_to_address() {
        let fields = hd_fields_at(at_address(1));
        let estimated_from = block_on(trade_preimage_sender_address(&fields)).unwrap();
        assert_eq!(estimated_from, address_of(&fields, &key_at(1)));
    }

    /// T20 (R42): the change of a funding transaction returns to the funding address, and a spend or refund
    /// output pays to it (the helper both read).
    #[test]
    fn t20_change_and_spend_outputs_use_the_address_at_path_to_address() {
        let fields = hd_fields_at(at_address(1));
        let sender = block_on(active_utxo_sender_address(&fields, "test")).unwrap();
        let spend_to = block_on(utxo_standard_swap_v2::enabled_hd_address_info(
            &fields,
            hd_wallet_of(&fields),
            "test",
        ))
        .unwrap()
        .address;
        assert_eq!(spend_to, sender.address);

        let coin = utxo_coin_from_fields(fields);
        let funding = vec![TransactionOutput {
            script_pubkey: Builder::build_p2pkh(&Address::from("RQq6fWoy8aGGMLjvRfMY5mBNVm2RQxJyLa").hash).to_bytes(),
            value: 100_000,
        }];
        let unspents = vec![UnspentInfo {
            value: 100_000_000,
            outpoint: OutPoint::default(),
            height: Default::default(),
        }];
        let (unsigned, _) = block_on(
            UtxoTxBuilder::new(&coin)
                .with_from_address(sender.address.clone())
                .add_available_inputs(unspents)
                .add_outputs(funding)
                .build(),
        )
        .unwrap();
        let change_script = Builder::build_p2pkh(&sender.address.hash).to_bytes();
        assert!(
            unsigned.outputs.iter().any(|o| o.script_pubkey == change_script),
            "the change returns to the funding address"
        );
    }

    fn hd_wallet_of(fields: &UtxoCoinFields) -> &UtxoHDWallet {
        match &fields.derivation_method {
            DerivationMethod::HDWallet(wallet) => wallet,
            _ => panic!("an HD coin"),
        }
    }

    /// T20 (R42): the V2 HTLC public key and key pair are those of the same address.
    #[test]
    fn t20_the_v2_htlc_key_is_the_key_at_path_to_address() {
        let coin = utxo_coin_from_fields(hd_fields_at(at_address(1)));
        let expected = *key_at(1).public();
        assert_eq!(get_htlc_pubkey_v2(&coin, &[]).unwrap(), expected);
        assert_eq!(*get_htlc_key_pair_v2(&coin, &[]).unwrap().public(), expected);
        assert_eq!(coin.try_derive_htlc_pubkey_v2(&[]).unwrap(), expected);
    }

    /// T20 (R42): at the default `path_to_address` the funding address is the first address, as before: the
    /// address the first external key of account 0 has.
    #[test]
    fn t20_at_the_default_path_the_first_address_is_used() {
        let fields = hd_fields_at(PathToAddress::default());
        let sender = block_on(active_utxo_sender_address(&fields, "test")).unwrap();
        assert_eq!(sender.address, address_of(&fields, &key_at(0)));
        assert_eq!(
            sender.derivation_path.unwrap().to_string(),
            "m/44'/141'/0'/0/0".to_owned()
        );
    }

    /// T20 (R42): a coin without an HD key policy keeps the first address of account 0 of its wallet.
    #[test]
    fn a_coin_without_an_hd_key_policy_keeps_the_first_address_of_its_account() {
        let client = native_client_for_test();
        let mut fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(client), None, false);
        let hd_account = UtxoHDAccount {
            account_id: 0,
            extended_pubkey: Secp256k1ExtendedPublicKey::from_str(
                "xpub6DEHSksajpRPM59RPw7Eg6PKdU7E2ehxJWtYdrfQ6JFmMGBsrR6jA78ANCLgzKYm4s5UqQ4ydLEYPbh3TRVvn5oAZVtWfi4qJLMntpZ8uGJ",
            )
            .unwrap(),
            account_derivation_path: HDPathToAccount::from_str("m/44'/141'/0'").unwrap(),
            external_addresses_number: 1,
            internal_addresses_number: 0,
        };
        let derived = hd_account
            .extended_pubkey
            .derive_child(Bip44Chain::External.to_child_number())
            .unwrap()
            .derive_child(ChildNumber::from(0))
            .unwrap();
        let first = Public::Compressed(H264::from(derived.public_key().serialize()));
        let mut wallet = hd_wallet();
        wallet.accounts = HDAccountsMutex::new(HDAccountsMap::from([(0, hd_account)]));
        fields.priv_key_policy = PrivKeyPolicy::Trezor;
        fields.derivation_method = DerivationMethod::HDWallet(wallet);
        let info = block_on(utxo_standard_swap_v2::enabled_hd_address_info(
            &fields,
            hd_wallet_of(&fields),
            "test",
        ))
        .unwrap();
        assert_eq!(info.pubkey, first);
        assert_eq!(info.derivation_path.to_string(), "m/44'/141'/0'/0/0".to_owned());
    }

    /// T21 (R41 item 2): RIN enabled through legacy `electrum` under a global-HD login holds the internal key as
    /// its single key. It announces that key as its own signing key and funds from that key's address.
    #[test]
    fn t21_a_single_key_coin_announces_and_funds_from_its_own_key() {
        let client = native_client_for_test();
        let mut fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(client), None, false);
        let internal_key = key_at(0);
        fields.priv_key_policy = PrivKeyPolicy::KeyPair(internal_key);
        let funding_address = address_of(&fields, &internal_key);
        fields.derivation_method = DerivationMethod::Iguana(funding_address.clone());
        let coin = utxo_coin_from_fields(fields);

        // The swap start takes this pair (and saves it when it is not the persistent key).
        assert_eq!(coin.own_signing_key_pair().unwrap().public(), internal_key.public());
        assert!(coin.get_htlc_key_pair().is_none());
        let sender = block_on(active_utxo_sender_address(coin.as_ref(), "test")).unwrap();
        assert_eq!(sender.address, funding_address);
        assert_eq!(sender.pubkey, Some(*internal_key.public()));
    }

    /// T22 (R41): the HD coin supplies its key through `get_htlc_key_pair` as before and has no single key of
    /// its own; a single-key coin's own key is the iguana key, which the swap start recognizes as the persistent
    /// key (the `lp_swap` tests).
    #[test]
    fn t22_the_key_sources_are_unchanged_for_the_other_policies() {
        let hd = utxo_coin_from_fields(hd_fields_at(at_address(1)));
        assert_eq!(hd.get_htlc_key_pair().unwrap().public(), key_at(1).public());
        assert!(hd.own_signing_key_pair().is_none());

        let client = native_client_for_test();
        let fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(client), None, false);
        let iguana_key = match &fields.priv_key_policy {
            PrivKeyPolicy::KeyPair(key_pair) => *key_pair,
            _ => panic!("the iguana policy"),
        };
        let iguana = utxo_coin_from_fields(fields);
        assert!(iguana.get_htlc_key_pair().is_none());
        assert_eq!(iguana.own_signing_key_pair().unwrap().public(), iguana_key.public());

        let mut trezor_fields =
            utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(native_client_for_test()), None, false);
        trezor_fields.priv_key_policy = PrivKeyPolicy::Trezor;
        let trezor = utxo_coin_from_fields(trezor_fields);
        assert!(trezor.own_signing_key_pair().is_none());
    }

    /// T24 (R44): under a global-HD login none of the T15 and T16 coins disagrees any more: an HD coin at any
    /// address, a single-key coin that is not the node's key.
    #[test]
    fn t24_no_coin_of_t16_disagrees() {
        let node_pubkey = *key_pair_from_seed("the passphrase of the test").unwrap().public();
        for address_id in [0, 1, 7] {
            let coin = utxo_coin_from_fields(hd_fields_at(at_address(address_id)));
            assert_eq!(coin.swap_key_disagreement(&node_pubkey), None);
        }
    }

    /// CRD ch.49 R49.4, R49.27a (T49.3 to T49.6, T49.27) and ch.05 §5.9B R35: the direct `withdraw` method serves a
    /// UTXO coin under a software global-HD login, with `from` and without it, like `task::withdraw::init`.
    #[cfg(not(target_arch = "wasm32"))]
    mod direct_withdraw {
        use super::*;
        use crate::utxo::utxo_withdraw::resolve_withdraw_sender;
        use crate::{GetWithdrawSenderAddress, HDAddressId, TransactionDetails, WithdrawError, WithdrawFrom};
        use common::HttpStatusCode;
        use mocktopus::mocking::{MockResult, Mockable};

        const RECEIVER: &str = "RQq6fWoy8aGGMLjvRfMY5mBNVm2RQxJyLa";

        /// The sender addresses the mocked unspent listing was asked for.
        type Seen = Arc<Mutex<Vec<Address>>>;

        /// A wallet holding account 0 of the test mnemonic, with `external_addresses_number` known external addresses.
        fn hd_wallet_with_account(external_addresses_number: u32) -> UtxoHDWallet {
            let account_path = "m/44'/141'/0'";
            let extended_pubkey = global_hd()
                .derive_account_extended_pubkey(&DerivationPath::from_str(account_path).unwrap())
                .unwrap();
            let account = UtxoHDAccount {
                account_id: 0,
                extended_pubkey,
                account_derivation_path: HDPathToAccount::from_str(account_path).unwrap(),
                external_addresses_number,
                internal_addresses_number: 0,
            };
            let mut wallet = hd_wallet();
            wallet.accounts = HDAccountsMutex::new(HDAccountsMap::from([(0, account)]));
            wallet
        }

        /// An HD coin activated at `path_to_address`, whose wallet holds account 0 with `known` external addresses.
        fn fields_at(path_to_address: PathToAddress, known: u32) -> UtxoCoinFields {
            let mut fields = hd_fields_at(path_to_address);
            fields.derivation_method = DerivationMethod::HDWallet(hd_wallet_with_account(known));
            fields
        }

        fn request(from: Option<WithdrawFrom>) -> WithdrawRequest {
            WithdrawRequest {
                amount: 1u64.into(),
                from,
                to: RECEIVER.to_string(),
                coin: TEST_COIN_NAME.into(),
                max: false,
                fee: None,
            }
        }

        fn address_id(account_id: u32, address_id: u32) -> WithdrawFrom {
            WithdrawFrom::AddressId(HDAddressId {
                account_id,
                chain: Bip44Chain::External,
                address_id,
            })
        }

        fn derivation_path(path: &str) -> WithdrawFrom {
            WithdrawFrom::DerivationPath {
                derivation_path: path.to_owned(),
            }
        }

        fn listed_unspents() -> Vec<UnspentInfo> {
            vec![UnspentInfo {
                outpoint: OutPoint {
                    hash: 1.into(),
                    index: 0,
                },
                value: 1_000_000_000,
                height: Default::default(),
            }]
        }

        /// Scripts the unspent listing of `UtxoStandardCoin` and records the address it is asked for.
        fn mock_standard_unspents() -> Seen {
            let seen = Seen::default();
            let recorded = seen.clone();
            UtxoStandardCoin::get_unspent_ordered_list.mock_safe(move |coin, address| {
                recorded.lock().unwrap().push(address.clone());
                let cache = block_on(coin.as_ref().recently_spent_outpoints.lock());
                MockResult::Return(Box::pin(futures::future::ok((listed_unspents(), cache))))
            });
            seen
        }

        /// The direct `withdraw` method, as `coins::withdraw` calls it.
        fn direct(
            coin: &UtxoStandardCoin,
            from: Option<WithdrawFrom>,
        ) -> Result<TransactionDetails, MmError<WithdrawError>> {
            coin.withdraw(request(from)).wait()
        }

        fn tx_of(details: &TransactionDetails) -> UtxoTx { deserialize(details.tx_hex.as_slice()).unwrap() }

        fn signed_by(tx: &UtxoTx, key: &KeyPair) -> bool {
            let script_sig: &[u8] = &tx.inputs[0].script_sig;
            let pubkey: &[u8] = key.public();
            script_sig.windows(pubkey.len()).any(|window| window == pubkey)
        }

        /// The withdrawal spends from `sender`: it is the listed address and the displayed sender, the key at its
        /// path signs (and not the key `other`), and the change returns to it.
        fn assert_spent_from(details: &TransactionDetails, sender: &Address, key: &KeyPair, other: &KeyPair) {
            assert_eq!(details.from, vec![sender.display_address().unwrap()]);
            let tx = tx_of(details);
            let change_script = Builder::build_p2pkh(&sender.hash).to_bytes();
            assert!(
                tx.outputs.iter().any(|output| output.script_pubkey == change_script),
                "the change returns to the sender"
            );
            assert!(signed_by(&tx, key), "the key at the sender's path signs");
            assert!(!signed_by(&tx, other), "no other key signs");
        }

        /// T49.3, direct route: an activated external address named by an address-id selector is the sender, the
        /// key at its path signs, and the change returns to it.
        #[test]
        fn t49_3_direct_address_id_selector_spends_from_that_activated_address() {
            let fields = fields_at(PathToAddress::default(), 2);
            let sender = address_of(&fields, &key_at(1));
            assert_ne!(sender, address_of(&fields, &key_at(0)));
            let seen = mock_standard_unspents();
            let coin = utxo_coin_from_fields(fields);

            let details = direct(&coin, Some(address_id(0, 1))).unwrap();

            assert_eq!(*seen.lock().unwrap(), vec![sender.clone()]);
            assert_spent_from(&details, &sender, &key_at(1), &key_at(0));
        }

        /// T49.4, direct route: a derivation-path selector that resolves to the same activated address gives the
        /// same sender as the address-id selector of T49.3.
        #[test]
        fn t49_4_direct_derivation_path_selector_resolves_to_the_same_sender() {
            let fields = fields_at(PathToAddress::default(), 2);
            let sender = address_of(&fields, &key_at(1));
            let seen = mock_standard_unspents();
            let coin = utxo_coin_from_fields(fields);

            let by_path = direct(&coin, Some(derivation_path("m/44'/141'/0'/0/1"))).unwrap();
            let by_id = direct(&coin, Some(address_id(0, 1))).unwrap();

            assert_eq!(*seen.lock().unwrap(), vec![sender.clone(), sender.clone()]);
            assert_spent_from(&by_path, &sender, &key_at(1), &key_at(0));
            assert_eq!(by_path.from, by_id.from);
        }

        /// T49.5 (decisions BY, BZ and CU), direct route, the default `path_to_address`: without `from` the sender
        /// is account 0, External, address 0.
        #[test]
        fn t49_5_direct_without_from_default_path_spends_from_address_zero() {
            let fields = fields_at(PathToAddress::default(), 2);
            let sender = address_of(&fields, &key_at(0));
            let seen = mock_standard_unspents();
            let coin = utxo_coin_from_fields(fields);

            let details = direct(&coin, None).unwrap();

            assert_eq!(*seen.lock().unwrap(), vec![sender.clone()]);
            assert_spent_from(&details, &sender, &key_at(0), &key_at(1));
        }

        /// T49.5 (decision CU), direct route, a non-default `path_to_address` (account 0, External, address 1): without
        /// `from` the sender is that address and not address 0.
        #[test]
        fn t49_5_direct_without_from_non_default_path_spends_from_that_address() {
            let fields = fields_at(at_address(1), 2);
            let sender = address_of(&fields, &key_at(1));
            assert_ne!(sender, address_of(&fields, &key_at(0)));
            let seen = mock_standard_unspents();
            let coin = utxo_coin_from_fields(fields);

            let details = direct(&coin, None).unwrap();

            assert_eq!(*seen.lock().unwrap(), vec![sender.clone()]);
            assert_spent_from(&details, &sender, &key_at(1), &key_at(0));
        }

        /// T49.5 (decisions BY, BZ and CU), task route: `InitUtxoWithdraw::new` takes its sender from
        /// `resolve_withdraw_sender`, the step the direct route takes its sender from too. Without `from` it is the
        /// coin's activated address with its full derivation path: address 0 for the default `path_to_address`,
        /// the address at `path_to_address` for a non-default one.
        #[test]
        fn t49_5_the_shared_sender_step_without_from_is_the_activated_address() {
            for (path_to_address, known, address_id) in [(PathToAddress::default(), 1, 0), (at_address(1), 2, 1)] {
                let fields = fields_at(path_to_address, known);
                let expected = address_of(&fields, &key_at(address_id));
                let coin = utxo_coin_from_fields(fields);

                let sender = block_on(resolve_withdraw_sender(&coin, &request(None))).unwrap();

                assert_eq!(sender.address, expected);
                assert_eq!(sender.pubkey, *key_at(address_id).public());
                assert_eq!(
                    sender.derivation_path.to_string(),
                    format!("m/44'/141'/0'/0/{}", address_id)
                );
                // The step is the trait method of the coin; the task route reaches it through the same function.
                let through_trait = block_on(coin.get_withdraw_sender_address(&request(None))).unwrap();
                assert_eq!(through_trait.address, sender.address);
            }
        }

        /// T49.5, Trezor policy: the default stays account 0, External, address 0 (the Trezor policy does not honor
        /// `path_to_address`), and the direct route does not sign with a device: it answers the refusal it gave
        /// before this change, and does not list the unspents.
        #[test]
        fn t49_5_trezor_policy_default_is_unchanged_and_the_direct_route_does_not_sign_with_a_device() {
            let mut fields = fields_at(at_address(1), 2);
            fields.priv_key_policy = PrivKeyPolicy::Trezor;
            let expected = address_of(&fields, &key_at(0));
            let seen = mock_standard_unspents();
            let coin = utxo_coin_from_fields(fields);

            let sender = block_on(resolve_withdraw_sender(&coin, &request(None))).unwrap();
            assert_eq!(sender.address, expected);
            assert_eq!(sender.derivation_path.to_string(), "m/44'/141'/0'/0/0");

            let err = direct(&coin, None).unwrap_err().into_inner();
            assert!(matches!(err, WithdrawError::InternalError(_)), "{:?}", err);
            assert!(seen.lock().unwrap().is_empty(), "no signing was reached");
        }

        /// Runs a direct withdraw that is refused before any unspent is listed and returns the refusal, after
        /// checking its HTTP status (R49.27a: 400).
        fn refusal(from: Option<WithdrawFrom>, path_to_address: PathToAddress, known: u32) -> WithdrawError {
            let seen = mock_standard_unspents();
            let coin = utxo_coin_from_fields(fields_at(path_to_address, known));

            let err = direct(&coin, from).unwrap_err().into_inner();

            assert!(seen.lock().unwrap().is_empty(), "refused before listing the unspents");
            assert_eq!(err.status_code(), http::StatusCode::BAD_REQUEST);
            // A withdraw never adds an address to the wallet (T49.27b: `account_balance` still lists them all).
            let accounts = block_on(hd_wallet_of(coin.as_ref()).accounts.lock());
            assert_eq!(accounts.get(&0).unwrap().external_addresses_number, known);
            err
        }

        /// T49.6 and T49.27b (HD), direct route: with three known external addresses in account 0, `account_id: 1`
        /// answers `UnknownAccount`, `error_data` an object; the wallet still holds three addresses.
        #[test]
        fn t49_6_direct_unknown_account_answers_unknown_account() {
            let err = refusal(Some(address_id(1, 0)), PathToAddress::default(), 3);
            assert!(matches!(err, WithdrawError::UnknownAccount { .. }), "{:?}", err);
            let json = serde_json::to_value(&err).unwrap();
            assert_eq!(json["error_type"], "UnknownAccount");
            assert!(json["error_data"].is_object());
        }

        /// T49.6 and T49.27b (HD), direct route: with three known external addresses in account 0, `address_id: 3`
        /// (not known) answers `UnexpectedFromAddress`, `error_data` a string; the wallet still holds three.
        #[test]
        fn t49_6_direct_address_not_activated_answers_unexpected_from_address() {
            let err = refusal(Some(address_id(0, 3)), PathToAddress::default(), 3);
            assert!(matches!(err, WithdrawError::UnexpectedFromAddress(_)), "{:?}", err);
            let json = serde_json::to_value(&err).unwrap();
            assert_eq!(json["error_type"], "UnexpectedFromAddress");
            assert!(json["error_data"].is_string());
        }

        /// T49.6, direct route: a derivation path of another coin type answers `UnexpectedFromAddress`,
        /// `error_data` a string.
        #[test]
        fn t49_6_direct_mismatched_coin_type_answers_unexpected_from_address() {
            let err = refusal(Some(derivation_path("m/44'/0'/0'/0/1")), PathToAddress::default(), 3);
            assert!(matches!(err, WithdrawError::UnexpectedFromAddress(_)), "{:?}", err);
            let json = serde_json::to_value(&err).unwrap();
            assert_eq!(json["error_type"], "UnexpectedFromAddress");
            assert!(json["error_data"].is_string());
        }

        /// R49.4: without `from`, if the account of the activated address is not held, the direct route answers
        /// `FromAddressNotFound` (400), as the task route does.
        #[test]
        fn t49_5_direct_without_from_and_a_missing_activated_account_answers_from_address_not_found() {
            let activated_in_account_1 = PathToAddress {
                account_id: 1,
                chain: Bip44Chain::External,
                address_id: 0,
            };
            let err = refusal(None, activated_in_account_1, 1);
            assert!(matches!(err, WithdrawError::FromAddressNotFound), "{:?}", err);
            let json = serde_json::to_value(&err).unwrap();
            assert_eq!(json["error_type"], "FromAddressNotFound");
            assert!(json["error_data"].is_null());
        }

        /// QTUM: the direct `withdraw` under a software HD login spends from the selected address.
        #[test]
        fn qtum_direct_withdraw_with_from_spends_from_the_selected_address() {
            let fields = fields_at(PathToAddress::default(), 2);
            let sender = address_of(&fields, &key_at(1));
            let seen = Seen::default();
            let recorded = seen.clone();
            QtumCoin::get_unspent_ordered_list.mock_safe(move |coin, address| {
                recorded.lock().unwrap().push(address.clone());
                let cache = block_on(coin.as_ref().recently_spent_outpoints.lock());
                MockResult::Return(Box::pin(futures::future::ok((listed_unspents(), cache))))
            });
            let arc: UtxoArc = fields.into();
            let coin = QtumCoin::from(arc);

            let details = coin.withdraw(request(Some(address_id(0, 1)))).wait().unwrap();

            assert_eq!(*seen.lock().unwrap(), vec![sender.clone()]);
            assert_spent_from(&details, &sender, &key_at(1), &key_at(0));
        }

        /// Scripts the unspent listing of the coin type `$coin` and records the address it is asked for.
        macro_rules! mock_listing {
            ($coin:ident) => {{
                let seen = Seen::default();
                let recorded = seen.clone();
                $coin::get_unspent_ordered_list.mock_safe(move |coin, address| {
                    recorded.lock().unwrap().push(address.clone());
                    let cache = block_on(coin.as_ref().recently_spent_outpoints.lock());
                    MockResult::Return(Box::pin(futures::future::ok((listed_unspents(), cache))))
                });
                seen
            }};
        }

        /// The selectors that name the login's own address under an iguana login, in both forms.
        fn own_address_selectors() -> [WithdrawFrom; 2] { [address_id(0, 0), derivation_path("m/44'/141'/0'/0/0")] }

        /// R49.27b item 1: `UnexpectedFromAddress`, HTTP 400, `error_data` a string.
        fn assert_single_key_refusal(err: WithdrawError) {
            assert!(matches!(err, WithdrawError::UnexpectedFromAddress(_)), "{:?}", err);
            assert_eq!(err.status_code(), http::StatusCode::BAD_REQUEST);
            let json = serde_json::to_value(&err).unwrap();
            assert_eq!(json["error_type"], "UnexpectedFromAddress");
            assert!(json["error_data"].is_string());
        }

        /// T49.27b (single-key), direct route, UTXO standard coin: any `from`, the login's own address in both
        /// selector forms included, is refused before the unspents are listed; without `from` the withdraw is
        /// unchanged.
        #[test]
        fn t49_27b_single_key_direct_withdraw_refuses_from_utxo_standard() {
            let seen = mock_listing!(UtxoStandardCoin);
            let coin = utxo_coin_for_test(UtxoRpcClientEnum::Native(native_client_for_test()), None, false);

            for from in own_address_selectors() {
                assert_single_key_refusal(direct(&coin, Some(from)).unwrap_err().into_inner());
            }
            assert!(seen.lock().unwrap().is_empty(), "refused before listing the unspents");

            direct(&coin, None).unwrap();
            assert_eq!(seen.lock().unwrap().len(), 1);
        }

        /// T49.27b (single-key), direct route, QTUM.
        #[test]
        fn t49_27b_single_key_direct_withdraw_refuses_from_qtum() {
            let seen = mock_listing!(QtumCoin);
            let fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(native_client_for_test()), None, false);
            let arc: UtxoArc = fields.into();
            let coin = QtumCoin::from(arc);

            for from in own_address_selectors() {
                let err = coin.withdraw(request(Some(from))).wait().unwrap_err().into_inner();
                assert_single_key_refusal(err);
            }
            assert!(seen.lock().unwrap().is_empty(), "refused before listing the unspents");

            coin.withdraw(request(None)).wait().unwrap();
            assert_eq!(seen.lock().unwrap().len(), 1);
        }

        /// T49.27b (single-key), direct route, BCH, which has no HD wallet and passes the same single-key
        /// constructor.
        #[test]
        fn t49_27b_single_key_direct_withdraw_refuses_from_bch() {
            use crate::utxo::bch::{bch_coin_without_network_for_test, BchCoin};

            let seen = mock_listing!(BchCoin);
            let coin = bch_coin_without_network_for_test();

            for from in own_address_selectors() {
                let err = coin.withdraw(request(Some(from))).wait().unwrap_err().into_inner();
                assert_single_key_refusal(err);
            }
            assert!(seen.lock().unwrap().is_empty(), "refused before listing the unspents");
        }

        /// T49.27b (single-key), task route: the shared sender step refuses the same two selectors with
        /// `UnexpectedFromAddress`, and still resolves the login's own address without `from`.
        #[test]
        fn t49_27b_single_key_shared_sender_step_refuses_from() {
            let coin = utxo_coin_for_test(UtxoRpcClientEnum::Native(native_client_for_test()), None, false);

            for from in own_address_selectors() {
                let err = block_on(resolve_withdraw_sender(&coin, &request(Some(from))))
                    .err()
                    .expect("an explicit `from` is refused under an iguana login")
                    .into_inner();
                assert_single_key_refusal(err);
            }

            let sender = block_on(resolve_withdraw_sender(&coin, &request(None))).unwrap();
            assert_eq!(&sender.address, coin.as_ref().derivation_method.unwrap_iguana());
        }
    }
}

// ---- The configurable sighash suffix (CRD ch.61, tests T6 and T8) ----

#[cfg(not(target_arch = "wasm32"))]
mod sighash_suffix_sites {
    use super::order_posting_latency::{my_address, script_hashes_of_test_coin};
    use super::*;
    use crate::utxo::rpc_clients::electrum_rpc_client::electrum_client_over_stub;
    use crate::utxo::rpc_clients::electrum_stub_server::StubServer;
    use crate::utxo::swap_proto_v2_scripts::taker_payment_script;
    use crate::utxo::utxo_common::{build_taker_payment_spend_preimage_tx, generate_swap_payment_outputs,
                                   generate_taker_fee_tx_outputs, payment_script, send_maker_payment, send_taker_fee,
                                   sign_taker_payment_spend_input};
    use crate::{DexFeeBurnDestination, PrevTxns, SignRawTransactionEnum, SignRawTransactionRequest,
                SignUtxoTransactionParams};
    use futures::compat::Future01CompatExt;
    use keys::{Public, Signature};
    use mm2_net_config::net_config_or_panic;
    use serialization::serialize;

    /// The Bitcoin II suffix: "BC2" and the suffix version 1.
    const BC2_SUFFIX: u32 = 0x0132_4342;
    const LISTED_VALUE: u64 = 900_000;
    const TIME_LOCK: u32 = 1_700_000_000;
    const SECRET_HASH: [u8; 20] = [0x11; 20];

    /// The test coin turned into a Bitcoin-II-like entry: plain (not overwintered) Bitcoin transactions, the
    /// ordinary signature version and fork id, and the given suffix.
    fn fields_with_suffix(rpc_client: UtxoRpcClientEnum, sighash_suffix: u32) -> UtxoCoinFields {
        let mut fields = utxo_coin_fields_for_test(rpc_client, None, false);
        fields.conf.overwintered = false;
        fields.conf.zcash = false;
        fields.conf.tx_version = 2;
        fields.conf.version_group_id = 0;
        fields.conf.consensus_branch_id = 0;
        fields.conf.signature_version = SignatureVersion::Base;
        fields.conf.fork_id = 0;
        fields.conf.sighash_suffix = sighash_suffix;
        fields
    }

    /// A coin on a scripted Electrum server that lists one unspent output of the coin's address and accepts every
    /// broadcast.
    fn coin_on_backend(sighash_suffix: u32) -> (UtxoStandardCoin, StubServer) {
        let (p2pkh_hash, p2pk_hash) = script_hashes_of_test_coin(false);
        let (client, server) = electrum_client_over_stub(|server| {
            server.set_scripthash_result(
                "blockchain.scripthash.listunspent",
                &p2pkh_hash,
                json!([{"tx_hash": "01".repeat(32), "tx_pos": 0, "height": 10, "value": LISTED_VALUE}]),
            );
            server.set_scripthash_result("blockchain.scripthash.listunspent", &p2pk_hash, json!([]));
            server.set_result("blockchain.transaction.broadcast", json!("ab".repeat(32)));
        });
        let fields = fields_with_suffix(UtxoRpcClientEnum::Electrum(client), sighash_suffix);
        (utxo_coin_from_fields(fields), server)
    }

    fn utxo_tx_of(sent: TransactionEnum) -> UtxoTx {
        match sent {
            TransactionEnum::UtxoTx(tx) => tx,
            _ => panic!("expected a UTXO transaction"),
        }
    }

    /// The signature, without its sighash-type byte, that a scriptSig pushes first (`[signature || type]...`). The
    /// signed sighash-type byte is the ordinary `0x01` (R61.2.4).
    fn first_signature_of(script_sig: &Bytes) -> Signature {
        let script = Script::from(script_sig.to_vec());
        let sig_with_type = script
            .get_instruction(0)
            .expect("the scriptSig has a first instruction")
            .expect("the first instruction decodes")
            .data
            .expect("the first instruction is a push")
            .to_vec();
        assert_eq!(
            sig_with_type.last().copied(),
            Some(0x01),
            "R61.2.4: the type byte is unchanged"
        );
        Signature::from(sig_with_type[..sig_with_type.len() - 1].to_vec())
    }

    /// Asserts that `signature` signs input `index` of `tx` over the digest with `suffix` and not over the ordinary one,
    /// recomputing both through the substrate's public routine.
    fn assert_suffix_digest_only(
        tx: &UtxoTx,
        index: usize,
        amount: u64,
        script_code: &Script,
        signature: &Signature,
        public: &Public,
        suffix: u32,
    ) {
        let digest = |suffix: u32| {
            TransactionInputSigner::from_transaction(tx.clone(), suffix).signature_hash(
                index,
                amount,
                script_code,
                SignatureVersion::Base,
                1,
            )
        };
        let suffixed = digest(suffix);
        let ordinary = digest(0);
        assert_ne!(suffixed, ordinary);
        assert!(
            public.verify(&suffixed, signature).unwrap(),
            "input {index}: suffix digest"
        );
        assert!(
            !public.verify(&ordinary, signature).unwrap(),
            "input {index}: ordinary digest"
        );
    }

    /// Every input of `tx` spends a P2PKH output of the coin's own address, of the given amounts.
    fn assert_p2pkh_inputs_carry_the_suffix(coin: &UtxoStandardCoin, tx: &UtxoTx, amounts: &[u64]) {
        assert_eq!(tx.inputs.len(), amounts.len());
        let prev_script = Builder::build_p2pkh(&my_address(coin).hash);
        for (index, input) in tx.inputs.iter().enumerate() {
            let script = Script::from(input.script_sig.to_vec());
            let pubkey_bytes = script
                .get_instruction(1)
                .expect("a P2PKH scriptSig has a second instruction")
                .expect("the second instruction decodes")
                .data
                .expect("the second instruction is a push")
                .to_vec();
            let public = Public::from_slice(&pubkey_bytes).unwrap();
            let signature = first_signature_of(&input.script_sig);
            assert_suffix_digest_only(tx, index, amounts[index], &prev_script, &signature, &public, BC2_SUFFIX);
        }
    }

    /// The Bitcoin II output limits from the activation height (CRD ch.61 R61.6): at most one `OP_RETURN` output of
    /// at most 83 bytes, no `OP_13` inside it, and no bare multisig output.
    fn assert_bc2_output_limits(outputs: &[TransactionOutput]) {
        let op_returns: Vec<_> = outputs
            .iter()
            .filter(|output| output.script_pubkey.first() == Some(&0x6a))
            .collect();
        assert!(op_returns.len() <= 1, "more than one OP_RETURN output: {outputs:?}");
        for output in op_returns {
            assert!(
                output.script_pubkey.len() <= 83,
                "OP_RETURN script of {} bytes",
                output.script_pubkey.len()
            );
            let script = Script::from(output.script_pubkey.to_vec());
            assert!(
                !script.opcodes().filter_map(Result::ok).any(|op| op == Opcode::OP_13),
                "OP_13 inside an OP_RETURN output"
            );
        }
        for output in outputs {
            let script = Script::from(output.script_pubkey.to_vec());
            assert!(!script.is_multisig_script(), "a bare multisig output");
        }
    }

    fn withdraw_request() -> WithdrawRequest {
        WithdrawRequest {
            amount: "0.001".parse().unwrap(),
            from: None,
            to: "RQq6fWoy8aGGMLjvRfMY5mBNVm2RQxJyLa".to_string(),
            coin: TEST_COIN_NAME.into(),
            max: false,
            fee: None,
        }
    }

    /// T8 and T6: the withdraw signs under the suffix digest and builds no restricted output.
    #[test]
    fn t8_withdraw_signs_under_the_suffix_digest() {
        let (coin, _server) = coin_on_backend(BC2_SUFFIX);
        let details = block_on(coin.withdraw(withdraw_request()).compat()).unwrap();
        let tx: UtxoTx = deserialize(details.tx_hex.as_slice()).unwrap();
        assert_p2pkh_inputs_carry_the_suffix(&coin, &tx, &[LISTED_VALUE]);
        assert_bc2_output_limits(&tx.outputs);
    }

    /// T8 and T6: the DEX-fee transaction (the plain, the pre-burn and the bare `OP_RETURN` burn forms of its outputs
    /// for T6, the plain form signed and sent for T8).
    #[test]
    fn t8_dex_fee_transaction_signs_under_the_suffix_digest() {
        let (coin, _server) = coin_on_backend(BC2_SUFFIX);
        let dex_fee = DexFee::Standard(MmNumber::from((10_000u64, 100_000_000u64)));
        let sent = block_on(send_taker_fee(coin.clone(), &dex_fee, test_dex_fee_addr_raw_pubkey()).compat()).unwrap();
        let tx = utxo_tx_of(sent);
        assert_p2pkh_inputs_carry_the_suffix(&coin, &tx, &[LISTED_VALUE]);
        assert_bc2_output_limits(&tx.outputs);

        let net_cfg = net_config_or_panic(6133);
        let fee_address = my_address(&coin);
        for burn_destination in [
            DexFeeBurnDestination::KmdOpReturn,
            DexFeeBurnDestination::PreBurnAccount {
                burn_pubkey: net_cfg.burn_addr_raw_pubkey().to_vec(),
            },
        ] {
            let with_burn = DexFee::WithBurn {
                fee_amount: MmNumber::from("0.0075"),
                burn_amount: MmNumber::from("0.0025"),
                burn_destination,
            };
            let outputs = generate_taker_fee_tx_outputs(&coin, &with_burn, &fee_address).unwrap();
            assert_bc2_output_limits(&outputs);
        }
    }

    /// T8 and T6: the HTLC payment signs under the suffix digest; its one `OP_RETURN` output records the secret hash
    /// and stays inside the Bitcoin II limits.
    #[test]
    fn t8_htlc_payment_signs_under_the_suffix_digest() {
        let (coin, _server) = coin_on_backend(BC2_SUFFIX);
        let my_pub = coin.my_public_key().unwrap().to_vec();
        let other_pub = key_pair_from_seed("ch61 htlc counterparty").unwrap().public().to_vec();

        let outputs = generate_swap_payment_outputs(
            coin.clone(),
            TIME_LOCK,
            &my_pub,
            &other_pub,
            &SECRET_HASH,
            "0.001".parse().unwrap(),
        )
        .unwrap()
        .outputs;
        assert_bc2_output_limits(&outputs);

        let sent = block_on(
            send_maker_payment(
                coin.clone(),
                TIME_LOCK,
                &my_pub,
                &other_pub,
                &SECRET_HASH,
                "0.001".parse().unwrap(),
            )
            .compat(),
        )
        .unwrap();
        let tx = utxo_tx_of(sent);
        assert_p2pkh_inputs_carry_the_suffix(&coin, &tx, &[LISTED_VALUE]);
        assert_bc2_output_limits(&tx.outputs);
    }

    /// T8 and T6: the HTLC spend and the HTLC refund sign the P2SH input over the redeem script under the suffix
    /// digest, and their single output is a plain P2PKH.
    #[test]
    fn t8_htlc_spend_and_refund_sign_under_the_suffix_digest() {
        UtxoStandardCoin::get_current_mtp
            .mock_safe(|_fields| MockResult::Return(Box::pin(futures::future::ok(TIME_LOCK - 1_000))));
        let (coin, _server) = coin_on_backend(BC2_SUFFIX);
        let key_pair = key_pair_from_seed("ch61 htlc spender").unwrap();
        let other = key_pair_from_seed("ch61 htlc counterparty").unwrap();
        let redeem = payment_script(TIME_LOCK, &SECRET_HASH, key_pair.public(), other.public());

        let mut prev = UtxoTx::default();
        prev.version = 2;
        prev.outputs.push(TransactionOutput {
            value: 1_000_000,
            script_pubkey: Builder::build_p2sh(&dhash160(&redeem).into()).to_bytes(),
        });
        let outputs = vec![TransactionOutput {
            value: 990_000,
            script_pubkey: Builder::build_p2pkh(&my_address(&coin).hash).to_bytes(),
        }];

        let spend_data = Builder::default()
            .push_data(&[0x22; 32])
            .push_opcode(Opcode::OP_0)
            .into_script();
        let refund_data = Builder::default().push_opcode(Opcode::OP_1).into_script();
        for (script_data, sequence) in [(spend_data, 0xffff_ffffu32), (refund_data, 0xffff_fffe)] {
            let tx = block_on(coin.p2sh_spending_tx(
                prev.clone(),
                redeem.to_bytes(),
                outputs.clone(),
                script_data,
                sequence,
                TIME_LOCK,
                &key_pair,
            ))
            .unwrap();
            let signature = first_signature_of(&tx.inputs[0].script_sig);
            assert_suffix_digest_only(
                &tx,
                0,
                prev.outputs[0].value,
                &redeem,
                &signature,
                key_pair.public(),
                BC2_SUFFIX,
            );
            assert_bc2_output_limits(&tx.outputs);
        }
    }

    /// T8 and R61.2.3: the taker's single-output pre-signature (SIGHASH_SINGLE, signed type byte `0x03`) verifies under
    /// the suffix digest and not under the ordinary one; the preimage signer carries the coin's suffix. The maker
    /// side re-derives the digest through the same routine to validate it (R61.4.6).
    #[test]
    fn t8_taker_single_output_presignature_signs_under_the_suffix_digest() {
        let fields = fields_with_suffix(UtxoRpcClientEnum::Native(native_client_for_test()), BC2_SUFFIX);
        let taker = key_pair_from_seed("ch61 taker").unwrap();
        let maker = key_pair_from_seed("ch61 maker").unwrap();
        let redeem = taker_payment_script(TIME_LOCK, &[0xbb; 32], taker.public(), maker.public());

        let mut taker_payment = UtxoTx::default();
        taker_payment.version = 2;
        taker_payment.outputs.push(TransactionOutput {
            value: 1_000_000,
            script_pubkey: vec![0xaa; 23].into(),
        });
        let outputs = vec![TransactionOutput {
            value: 10_000,
            script_pubkey: Builder::build_p2pkh(&maker.public().address_hash().into()).to_bytes(),
        }];
        let signer = build_taker_payment_spend_preimage_tx(&fields, &taker_payment, outputs).unwrap();
        assert_eq!(signer.sighash_suffix, BC2_SUFFIX);

        let sighash_type = 3u32;
        let signature = sign_taker_payment_spend_input(&signer, &redeem, &taker, &fields, sighash_type).unwrap();
        let mut ordinary_signer = signer.clone();
        ordinary_signer.sighash_suffix = 0;
        let digest = |signer: &TransactionInputSigner| {
            signer.signature_hash(
                0,
                signer.inputs[0].amount,
                &redeem,
                SignatureVersion::Base,
                sighash_type,
            )
        };
        assert_ne!(digest(&signer), digest(&ordinary_signer));
        assert!(taker.public().verify(&digest(&signer), &signature).unwrap());
        assert!(!taker.public().verify(&digest(&ordinary_signer), &signature).unwrap());
    }

    /// T8 and T6: the raw-transaction signing RPC signs every input under the suffix digest. The unsigned transaction
    /// comes in as hex, the input amounts through `prev_txns`.
    #[test]
    fn t8_sign_raw_transaction_signs_under_the_suffix_digest() {
        let (coin, _server) = coin_on_backend(BC2_SUFFIX);
        let prev_hash = H256::from([7; 32]);
        let mut unsigned = UtxoTx::default();
        unsigned.version = 2;
        unsigned.inputs.push(TransactionInput {
            previous_output: OutPoint {
                hash: prev_hash,
                index: 0,
            },
            script_sig: Default::default(),
            sequence: 0xffff_fffe,
            script_witness: vec![],
        });
        unsigned.outputs.push(TransactionOutput {
            value: 90_000,
            script_pubkey: Builder::build_p2pkh(&my_address(&coin).hash).to_bytes(),
        });
        let request = SignRawTransactionRequest {
            coin: TEST_COIN_NAME.into(),
            tx: SignRawTransactionEnum::UTXO(SignUtxoTransactionParams {
                tx_hex: hex::encode(serialize(&unsigned).to_vec()),
                prev_txns: Some(vec![PrevTxns {
                    tx_hash: hex::encode(prev_hash.to_vec()),
                    index: 0,
                    script_pub_key: hex::encode(Builder::build_p2pkh(&my_address(&coin).hash).to_bytes().to_vec()),
                    amount: "0.001".parse().unwrap(),
                }]),
            }),
        };
        let signed = block_on(crate::utxo::utxo_common::sign_raw_tx(coin.clone(), request)).unwrap();
        let tx: UtxoTx = deserialize(signed.tx_hex.as_slice()).unwrap();
        assert_p2pkh_inputs_carry_the_suffix(&coin, &tx, &[100_000]);
    }

    /// The suffix of a coin travels with the preimage a peer sends: parsing a preimage takes the receiving coin's own
    /// suffix, never a value from the peer's bytes.
    #[test]
    fn a_parsed_swap_preimage_takes_the_coins_own_suffix() {
        let (coin, _server) = coin_on_backend(BC2_SUFFIX);
        let mut tx = UtxoTx::default();
        tx.version = 2;
        tx.inputs.push(TransactionInput {
            previous_output: OutPoint {
                hash: H256::from([7; 32]),
                index: 0,
            },
            script_sig: Default::default(),
            sequence: 0xffff_fffe,
            script_witness: vec![],
        });
        tx.outputs.push(TransactionOutput {
            value: 1_000,
            script_pubkey: vec![0x51].into(),
        });
        let preimage = coin.parse_preimage(&serialize(&tx)).unwrap();
        assert_eq!(preimage.0.sighash_suffix, BC2_SUFFIX);
        let (plain_coin, _plain_server) = coin_on_backend(0);
        let preimage = plain_coin.parse_preimage(&serialize(&tx)).unwrap();
        assert_eq!(preimage.0.sighash_suffix, 0);
    }
}

// ---- BC2 swaps require a counterparty that signs with the suffix (CRD ch.61 §61.5A, tests T10-T13) ----

#[cfg(not(target_arch = "wasm32"))]
mod sighash_suffix_gate_tests {
    use super::*;
    use crate::coin_protocol_capability::{append_capability_word, declared_capabilities, DeclineLog,
                                          CAPABILITY_SIGHASH_SUFFIX, CAPABILITY_TX_V6, DECLINE_WARNING_INTERVAL_SECS};
    use crate::utxo::utxo_common::{suffix_counterparty_decline, suffix_decline_lines, SuffixDecline};
    use common::msgpack_bounds::{check_msgpack_bounds, check_peer_value};
    use serde_json::Value as Json;

    /// The Bitcoin II suffix: "BC2" and the suffix version 1.
    const BC2_SUFFIX: u32 = 0x0132_4342;

    /// The capability word `{"sighash_suffix": true}` as fixed bytes: map(1), str(14), the key, true.
    fn suffix_word() -> Vec<u8> { [&[0x81, 0xae][..], b"sighash_suffix", &[0xc3]].concat() }

    /// The word with the key set to `false`.
    fn false_word() -> Vec<u8> { [&[0x81, 0xae][..], b"sighash_suffix", &[0xc2]].concat() }

    /// The word `{"tx_v6": true}`: well-formed, without the key.
    fn other_key_word() -> Vec<u8> { append_capability_word(Vec::new(), &[CAPABILITY_TX_V6]) }

    fn address_formats() -> Vec<UtxoAddressFormat> {
        vec![
            UtxoAddressFormat::Standard,
            UtxoAddressFormat::Segwit,
            UtxoAddressFormat::CashAddress {
                network: "bitcoincash".to_owned(),
                pub_addr_prefix: 0,
                p2sh_addr_prefix: 5,
            },
        ]
    }

    fn encoded(format: &UtxoAddressFormat) -> Vec<u8> { rmp_serde::to_vec(format).unwrap() }

    /// A coin whose own address format is `format`, with the given resolved suffix and switch.
    fn coin_with(format: &UtxoAddressFormat, suffix: u32, require_switch: bool) -> UtxoStandardCoin {
        let client = UtxoRpcClientEnum::Native(native_client_for_test());
        let mut fields = utxo_coin_fields_for_test(client, None, false);
        fields.conf.sighash_suffix = suffix;
        fields.conf.require_suffix_counterparty = require_switch;
        if let DerivationMethod::Iguana(address) = &mut fields.derivation_method {
            address.addr_format = format.clone();
        }
        utxo_coin_from_fields(fields)
    }

    fn standard_coin(suffix: u32, require_switch: bool) -> UtxoStandardCoin {
        coin_with(&UtxoAddressFormat::Standard, suffix, require_switch)
    }

    fn supported(coin: &UtxoStandardCoin, info: &Option<Vec<u8>>) -> bool {
        utxo_common::is_coin_protocol_supported(coin, info)
    }

    /// The judgment of the coin's ordinary first-value test alone, as it was before the gate existed.
    fn first_value_only(coin: &UtxoStandardCoin, info: &Option<Vec<u8>>) -> bool {
        match info {
            Some(bytes) => match check_peer_value(bytes, "is_coin_protocol_supported") {
                Ok(end) => rmp_serde::from_read_ref::<_, UtxoAddressFormat>(&bytes[..end]).is_ok(),
                Err(_) => false,
            },
            None => !coin.addr_format().is_segwit(),
        }
    }

    /// Counterparty information that carries the word, for a standard-format counterparty.
    fn with_word() -> Option<Vec<u8>> { Some([encoded(&UtxoAddressFormat::Standard), suffix_word()].concat()) }

    /// Information that a coin requiring the word declines, with the cause of the decline. The first value of each
    /// entry decodes, except where the case is the first value itself.
    fn lacking_the_word() -> Vec<(&'static str, Option<Vec<u8>>, SuffixDecline)> {
        let first = encoded(&UtxoAddressFormat::Standard);
        let word = suffix_word();
        vec![
            ("absent", None, SuffixDecline::NoWord),
            ("empty", Some(Vec::new()), SuffixDecline::NoWord),
            ("first value alone", Some(first.clone()), SuffixDecline::NoWord),
            ("malformed first value", Some(vec![0xc1]), SuffixDecline::MalformedWord),
            (
                "malformed word",
                Some([first.clone(), word[..word.len() - 1].to_vec()].concat()),
                SuffixDecline::MalformedWord,
            ),
            (
                "bytes that are not a word",
                Some([first.clone(), vec![0x01]].concat()),
                SuffixDecline::MalformedWord,
            ),
            (
                "word without the key",
                Some([first.clone(), other_key_word()].concat()),
                SuffixDecline::WithoutSuffix,
            ),
            (
                "key set to false",
                Some([first, false_word()].concat()),
                SuffixDecline::WithoutSuffix,
            ),
        ]
    }

    // --- T10: the word as written ---

    #[test]
    fn t10_a_coin_with_a_suffix_sends_the_first_value_then_the_word_as_fixed_bytes() {
        for format in address_formats() {
            let coin = coin_with(&format, BC2_SUFFIX, true);
            let info = utxo_common::coin_protocol_info(&coin);
            let first = encoded(&format);
            assert_eq!(info, [first.clone(), suffix_word()].concat(), "{format:?}");
            // The word itself, byte for byte: map(1), str(14) "sighash_suffix", true.
            assert_eq!(
                &info[first.len()..],
                &[
                    0x81, 0xae, b's', b'i', b'g', b'h', b'a', b's', b'h', b'_', b's', b'u', b'f', b'f', b'i', b'x',
                    0xc3
                ][..],
                "{format:?}"
            );
            // The first value still decodes with the ordinary reader and is the same address format.
            assert_eq!(check_msgpack_bounds(&info), Ok(first.len()), "{format:?}");
            assert_eq!(
                rmp_serde::from_read_ref::<_, UtxoAddressFormat>(&info[..first.len()]).unwrap(),
                format
            );
            assert!(declared_capabilities(Some(&info)).declares(CAPABILITY_SIGHASH_SUFFIX));
            assert!(!declared_capabilities(Some(&info)).declares(CAPABILITY_TX_V6));
        }
    }

    #[test]
    fn t10_a_zero_suffix_sends_the_bytes_of_the_address_format_alone() {
        for format in address_formats() {
            for require_switch in [true, false] {
                let coin = coin_with(&format, 0, require_switch);
                let info = utxo_common::coin_protocol_info(&coin);
                assert_eq!(info, encoded(&format), "{format:?}");
                assert!(declared_capabilities(Some(&info)).is_empty());
            }
        }
    }

    #[test]
    fn t10_the_word_is_sent_whatever_the_switch_says() {
        for format in address_formats() {
            let on = utxo_common::coin_protocol_info(&coin_with(&format, BC2_SUFFIX, true));
            let off = utxo_common::coin_protocol_info(&coin_with(&format, BC2_SUFFIX, false));
            assert_eq!(on, off, "{format:?}");
            assert!(declared_capabilities(Some(&off)).declares(CAPABILITY_SIGHASH_SUFFIX));
        }
    }

    #[test]
    fn t10_the_bc2_and_bc2_segwit_entries_both_send_the_word() {
        let params = UtxoActivationParams::from_legacy_req(&json!({ "method": "enable" })).unwrap();
        for (ticker, is_segwit, format) in [
            ("BC2", false, UtxoAddressFormat::Standard),
            ("BC2-segwit", true, UtxoAddressFormat::Segwit),
        ] {
            let coin_conf = json!({ "coin": ticker, "sighash_suffix": "0x01324342" });
            let conf = UtxoConfBuilder::new(&coin_conf, &params, ticker).build().unwrap();
            assert_eq!(conf.sighash_suffix, BC2_SUFFIX);
            let client = UtxoRpcClientEnum::Native(native_client_for_test());
            let mut fields = utxo_coin_fields_for_test(client, None, is_segwit);
            fields.conf = conf;
            let coin = utxo_coin_from_fields(fields);
            assert_eq!(
                utxo_common::coin_protocol_info(&coin),
                [encoded(&format), suffix_word()].concat(),
                "{ticker}"
            );
        }
    }

    // --- T11: the requirement ---

    #[test]
    fn t11_with_the_switch_on_the_word_is_accepted() {
        for format in address_formats() {
            let coin = coin_with(&format, BC2_SUFFIX, true);
            let info = Some([encoded(&format), suffix_word()].concat());
            assert!(supported(&coin, &info), "{format:?}");
        }
        assert!(supported(&standard_coin(BC2_SUFFIX, true), &with_word()));
    }

    #[test]
    fn t11_with_the_switch_on_everything_without_the_word_is_declined() {
        let coin = standard_coin(BC2_SUFFIX, true);
        for (name, info, _cause) in lacking_the_word() {
            assert!(!supported(&coin, &info), "{name}");
        }
    }

    #[test]
    fn t11_the_decline_names_the_cause() {
        for (name, info, cause) in lacking_the_word() {
            assert_eq!(
                suffix_counterparty_decline(BC2_SUFFIX, true, &info),
                Some(cause),
                "{name}"
            );
        }
        assert_eq!(suffix_counterparty_decline(BC2_SUFFIX, true, &with_word()), None);
    }

    #[test]
    fn t11_with_the_switch_off_the_coin_judges_as_before() {
        let off = standard_coin(BC2_SUFFIX, false);
        for (name, info, _cause) in lacking_the_word() {
            assert_eq!(supported(&off, &info), first_value_only(&off, &info), "{name}");
            assert_eq!(suffix_counterparty_decline(BC2_SUFFIX, false, &info), None, "{name}");
        }
        // Those whose first value decodes are accepted: the switch admits a counterparty without the word.
        for name in [
            "absent",
            "first value alone",
            "malformed word",
            "word without the key",
            "key set to false",
        ] {
            let (_, info, _) = lacking_the_word().into_iter().find(|(n, ..)| *n == name).unwrap();
            assert!(supported(&off, &info), "{name}");
        }
        assert!(supported(&off, &with_word()));
    }

    #[test]
    fn t11_with_a_zero_suffix_every_input_is_judged_as_before() {
        for require_switch in [true, false] {
            let coin = standard_coin(0, require_switch);
            let mut inputs: Vec<Option<Vec<u8>>> = lacking_the_word().into_iter().map(|(_, info, _)| info).collect();
            inputs.push(with_word());
            inputs.push(Some([vec![0xc1], suffix_word()].concat()));
            for format in address_formats() {
                inputs.push(Some(encoded(&format)));
            }
            for info in inputs {
                assert_eq!(supported(&coin, &info), first_value_only(&coin, &info), "{info:?}");
            }
            assert!(supported(&coin, &Some(encoded(&UtxoAddressFormat::Standard))));
        }
    }

    #[test]
    fn t11_the_first_value_is_tested_before_the_word() {
        // A word cannot make an undecodable first value acceptable, with the switch on or off.
        let with_garbage_first = Some([vec![0xc1], suffix_word()].concat());
        for require_switch in [true, false] {
            assert!(!supported(
                &standard_coin(BC2_SUFFIX, require_switch),
                &with_garbage_first
            ));
        }
        // A segwit coin without information is judged by its own format first.
        let segwit = coin_with(&UtxoAddressFormat::Segwit, BC2_SUFFIX, true);
        assert!(!supported(&segwit, &None));
    }

    // --- T13: the switch and the logs ---

    fn build_conf(coin_conf: Json) -> UtxoConfResult<UtxoCoinConf> {
        let params = UtxoActivationParams::from_legacy_req(&json!({ "method": "enable" })).unwrap();
        UtxoConfBuilder::new(&coin_conf, &params, "XYZ").build()
    }

    #[test]
    fn t13_the_switch_is_true_when_absent_and_accepts_both_booleans() {
        assert!(
            build_conf(json!({ "coin": "XYZ" }))
                .unwrap()
                .require_suffix_counterparty
        );
        assert!(
            build_conf(json!({ "coin": "XYZ", "require_suffix_counterparty": null }))
                .unwrap()
                .require_suffix_counterparty
        );
        assert!(
            build_conf(json!({ "coin": "XYZ", "require_suffix_counterparty": true }))
                .unwrap()
                .require_suffix_counterparty
        );
        assert!(
            !build_conf(json!({ "coin": "XYZ", "sighash_suffix": "0x01324342", "require_suffix_counterparty": false }))
                .unwrap()
                .require_suffix_counterparty
        );
    }

    #[test]
    fn t13_a_switch_that_is_not_a_boolean_fails_activation_naming_the_key() {
        for bad in [
            json!("true"),
            json!("false"),
            json!(1),
            json!(0),
            json!(""),
            json!([true]),
            json!({"a": 1}),
        ] {
            let err = build_conf(json!({ "coin": "XYZ", "require_suffix_counterparty": bad }))
                .err()
                .unwrap_or_else(|| panic!("require_suffix_counterparty {bad} must fail"))
                .into_inner();
            assert!(
                matches!(&err, UtxoConfError::InvalidRequireSuffixCounterparty { found } if *found == bad.to_string()),
                "{bad}: {err:?}"
            );
            assert!(
                err.to_string().contains("'require_suffix_counterparty'"),
                "{bad}: {err}"
            );
        }
    }

    #[test]
    fn t13_each_decline_writes_a_debug_line_that_names_the_coin_and_the_cause() {
        let mut log = DeclineLog::default();
        for (cause, text) in [
            (SuffixDecline::NoWord, "no capability word"),
            (SuffixDecline::MalformedWord, "a malformed capability word"),
            (SuffixDecline::WithoutSuffix, "a capability word without sighash_suffix"),
        ] {
            let lines = suffix_decline_lines(&mut log, "BC2", cause, 1_000);
            assert!(lines.debug.contains("BC2"), "{}", lines.debug);
            assert!(lines.debug.contains(text), "{}", lines.debug);
            assert!(
                lines
                    .debug
                    .contains("a BC2 swap requires a counterparty that signs with the suffix"),
                "{}",
                lines.debug
            );
        }
        // The text names the coin that declines, whatever its ticker.
        let other = suffix_decline_lines(&mut DeclineLog::default(), "XYZ", SuffixDecline::NoWord, 1_000);
        assert!(other.debug.contains("a XYZ swap requires"), "{}", other.debug);
        assert!(!other.debug.contains("BC2"), "{}", other.debug);
    }

    #[test]
    fn t13_the_warning_comes_at_most_once_an_hour_with_the_count_since_the_previous_one() {
        let mut log = DeclineLog::default();
        let start = 10_000;
        let first = suffix_decline_lines(&mut log, "BC2", SuffixDecline::NoWord, start);
        let warning = first.warning.expect("the first decline warns");
        assert!(warning.contains("BC2"), "{warning}");
        assert!(warning.contains("declined 1 swap match(es)"), "{warning}");
        assert!(warning.contains("require_suffix_counterparty"), "{warning}");

        // Declines inside the hour write their debug line and no warning.
        for at in [start + 1, start + 2, start + DECLINE_WARNING_INTERVAL_SECS - 1] {
            let lines = suffix_decline_lines(&mut log, "BC2", SuffixDecline::NoWord, at);
            assert!(!lines.debug.is_empty());
            assert_eq!(lines.warning, None, "at {at}");
        }
        // At the hour the warning comes back with the four declines since the previous one.
        let due = suffix_decline_lines(
            &mut log,
            "BC2",
            SuffixDecline::MalformedWord,
            start + DECLINE_WARNING_INTERVAL_SECS,
        );
        let warning = due.warning.expect("a warning is due after an hour");
        assert!(warning.contains("declined 4 swap match(es)"), "{warning}");
        // The count starts again after a warning, and the hour restarts from it.
        let next = suffix_decline_lines(
            &mut log,
            "BC2",
            SuffixDecline::NoWord,
            start + DECLINE_WARNING_INTERVAL_SECS + 1,
        );
        assert_eq!(next.warning, None);
        let later = suffix_decline_lines(
            &mut log,
            "BC2",
            SuffixDecline::NoWord,
            start + 2 * DECLINE_WARNING_INTERVAL_SECS,
        );
        assert!(later
            .warning
            .expect("a warning is due again")
            .contains("declined 2 swap match(es)"));
    }

    #[test]
    fn t13_the_warning_is_kept_per_coin_and_a_clock_that_goes_back_does_not_panic() {
        let mut bc2 = DeclineLog::default();
        let mut bc2_segwit = DeclineLog::default();
        assert!(suffix_decline_lines(&mut bc2, "BC2", SuffixDecline::NoWord, 500)
            .warning
            .is_some());
        assert!(
            suffix_decline_lines(&mut bc2_segwit, "BC2-segwit", SuffixDecline::NoWord, 501)
                .warning
                .is_some()
        );
        // An earlier time than the last warning gives no warning and no underflow.
        assert_eq!(
            suffix_decline_lines(&mut bc2, "BC2", SuffixDecline::NoWord, 100).warning,
            None
        );
    }
}

/// CRD ch.05 R35a, R35c, T14a and T14c: HD accounts at a given index. Activation lists the address at
/// `path_to_address`, and `task::create_new_account` works under a software HD login, for the UTXO standard coin
/// and for QTUM. The wallet storage is mocked and records what is stored, so that a "restart" is a wallet rebuilt
/// from the recorded items.
pub(crate) mod hd_accounts_at_index_tests {
    use super::*;
    use crate::coin_balance::{EnableCoinBalance, EnableCoinBalanceError, EnableCoinBalanceOps, EnableCoinScanPolicy,
                              HDAccountBalance, HDWalletBalance};
    use crate::hd_pubkey::{HDExtractPubkeyError, HDXPubExtractor};
    use crate::hd_wallet::{HDAddressId, HDWalletRpcError, NewAccountCreatingError};
    use crate::hd_wallet_storage::HDAccountStorageItem;
    use crate::rpc_command::get_my_address::PathToAddress;
    use crate::rpc_command::init_create_account::{CreateNewAccountParams, CreatedAccount, InitCreateHDAccountRpcOps};
    use crate::utxo::utxo_builder::derive_activated_hd_secret;
    use crate::BalanceError;
    use async_trait::async_trait;
    use common::HttpStatusCode;
    use crypto::privkey::key_pair_from_secret;
    use crypto::trezor::utxo::TrezorUtxoCoin;
    use crypto::{CryptoCtx, GlobalHDAccountArc, XPub};
    use std::collections::BTreeMap;

    const TEST_MNEMONIC: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    const COIN_PATH: &str = "m/44'/141'";

    /// What the mocked wallet storage holds, by account index.
    pub(crate) type StoredAccounts = Arc<std::sync::Mutex<BTreeMap<u32, HDAccountStorageItem>>>;

    /// A hardware device that counts the requests it gets and answers none of them. It reads from the hardware
    /// context it was given, if any (CRD ch.05 R58a).
    pub(crate) struct CountingDevice {
        pub(crate) calls: AtomicUsize,
        hw_ctx: Option<crypto::HardwareWalletArc>,
    }

    impl CountingDevice {
        pub(crate) fn new() -> CountingDevice {
            CountingDevice {
                calls: AtomicUsize::new(0),
                hw_ctx: None,
            }
        }

        /// A device that reads from `hw_ctx`, as the extractor of a running task does.
        pub(crate) fn with_context(hw_ctx: crypto::HardwareWalletArc) -> CountingDevice {
            CountingDevice {
                calls: AtomicUsize::new(0),
                hw_ctx: Some(hw_ctx),
            }
        }
    }

    #[async_trait]
    impl HDXPubExtractor for CountingDevice {
        async fn extract_utxo_xpub(
            &self,
            _trezor_utxo_coin: TrezorUtxoCoin,
            _derivation_path: DerivationPath,
        ) -> MmResult<XPub, HDExtractPubkeyError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            MmError::err(HDExtractPubkeyError::HwContextNotInitialized)
        }

        fn hardware_context(&self) -> Option<crypto::HardwareWalletArc> { self.hw_ctx.clone() }
    }

    /// Mocks the wallet storage: what is uploaded, updated and removed is recorded in the returned map.
    pub(crate) fn mock_storage() -> StoredAccounts {
        let items: StoredAccounts = Arc::default();

        let uploads = items.clone();
        HDWalletMockStorage::upload_new_account.mock_safe(move |_, _wallet_id, account: HDAccountStorageItem| {
            uploads.lock().unwrap().insert(account.account_id, account);
            MockResult::Return(Box::pin(futures::future::ok(())))
        });
        let external = items.clone();
        HDWalletMockStorage::update_external_addresses_number.mock_safe(move |_, _wallet_id, account_id, number| {
            external
                .lock()
                .unwrap()
                .get_mut(&account_id)
                .expect("the account is stored")
                .external_addresses_number = number;
            MockResult::Return(Box::pin(futures::future::ok(())))
        });
        let internal = items.clone();
        HDWalletMockStorage::update_internal_addresses_number.mock_safe(move |_, _wallet_id, account_id, number| {
            internal
                .lock()
                .unwrap()
                .get_mut(&account_id)
                .expect("the account is stored")
                .internal_addresses_number = number;
            MockResult::Return(Box::pin(futures::future::ok(())))
        });
        let removals = items.clone();
        HDWalletMockStorage::remove_account.mock_safe(move |_, _wallet_id, account_id| {
            removals.lock().unwrap().remove(&account_id);
            MockResult::Return(Box::pin(futures::future::ok(())))
        });
        items
    }

    /// Mocks the node: every address has a zero balance and no history.
    pub(crate) fn mock_chain() {
        NativeClient::display_balances.mock_safe(|_, addresses: Vec<Address>, _| {
            let balances = addresses
                .into_iter()
                .map(|address| (address, BigDecimal::from(0)))
                .collect();
            MockResult::Return(Box::new(futures01::future::ok(balances)))
        });
        NativeClient::list_all_transactions
            .mock_safe(|_, _| MockResult::Return(Box::new(futures01::future::ok(Vec::new()))));
    }

    fn global_hd() -> GlobalHDAccountArc {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        CryptoCtx::init_with_global_hd_account(ctx.clone(), TEST_MNEMONIC).unwrap();
        match PrivKeyBuildPolicy::detect_priv_key_policy(&ctx).unwrap() {
            PrivKeyBuildPolicy::GlobalHDAccount(global_hd) => global_hd,
            _ => panic!("expected a global-HD login"),
        }
    }

    fn path_of(account_id: u32, chain: Bip44Chain, address_id: u32) -> HDAddressId {
        HDAddressId {
            account_id,
            chain,
            address_id,
        }
    }

    /// Account `0` as a previous activation would have stored it.
    fn account_zero() -> UtxoHDAccount {
        UtxoHDAccount {
            account_id: 0,
            extended_pubkey: Secp256k1ExtendedPublicKey::from_str(
                "xpub6DEHSksajpRPM59RPw7Eg6PKdU7E2ehxJWtYdrfQ6JFmMGBsrR6jA78ANCLgzKYm4s5UqQ4ydLEYPbh3TRVvn5oAZVtWfi4qJLMntpZ8uGJ",
            )
            .unwrap(),
            account_derivation_path: HDPathToAccount::from_str("m/44'/141'/0'").unwrap(),
            external_addresses_number: 0,
            internal_addresses_number: 0,
        }
    }

    /// The fields of a coin activated through the software HD route, built the way the builder builds them, over
    /// a wallet that holds `accounts`.
    pub(crate) fn hd_fields(accounts: HDAccountsMap<UtxoHDAccount>) -> UtxoCoinFields {
        let client = native_client_for_test();
        let mut fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(client), None, false);
        let global_hd = global_hd();
        let coin_path = HDPathToCoin::from_str(COIN_PATH).unwrap();
        let path_to_address = PathToAddress::default();
        let secret = derive_activated_hd_secret(&global_hd, &coin_path, &path_to_address).unwrap();
        fields.priv_key_policy = PrivKeyPolicy::HDWallet {
            activated_key: key_pair_from_secret(secret.as_slice()).unwrap(),
            bip39_secp_priv_key: global_hd.root_priv_key().clone(),
            path_to_address,
        };
        fields.derivation_method = DerivationMethod::HDWallet(UtxoHDWallet {
            hd_wallet_storage: HDWalletCoinStorage::default(),
            address_format: UtxoAddressFormat::Standard,
            derivation_path: coin_path,
            accounts: HDAccountsMutex::new(accounts),
            gap_limit: 3,
        });
        fields
    }

    pub(crate) fn hd_coin(accounts: HDAccountsMap<UtxoHDAccount>) -> UtxoStandardCoin {
        utxo_coin_from_fields(hd_fields(accounts))
    }

    /// A coin over a wallet that holds account `0` only, which is what activation leaves.
    pub(crate) fn hd_coin_with_account_zero() -> UtxoStandardCoin {
        hd_coin(HDAccountsMap::from([(0, account_zero())]))
    }

    /// The coin after a restart: its wallet is rebuilt from what the storage holds.
    fn reload(items: &StoredAccounts) -> UtxoStandardCoin {
        let coin_path = HDPathToCoin::from_str(COIN_PATH).unwrap();
        let accounts: HDAccountsMap<UtxoHDAccount> = items
            .lock()
            .unwrap()
            .values()
            .map(|item| {
                let account = UtxoHDAccount::try_from_storage_item(&coin_path, item).unwrap();
                (account.account_id, account)
            })
            .collect();
        hd_coin(accounts)
    }

    fn account_ids(coin: &UtxoStandardCoin) -> Vec<u32> {
        match coin.as_ref().derivation_method {
            DerivationMethod::HDWallet(UtxoHDWallet { ref accounts, .. }) => {
                block_on(accounts.lock()).keys().cloned().collect()
            },
            _ => unreachable!(),
        }
    }

    fn enable(
        coin: &UtxoStandardCoin,
        scan_policy: EnableCoinScanPolicy,
        min_addresses_number: u32,
        path_to_address: Option<HDAddressId>,
    ) -> Result<HDWalletBalance, EnableCoinBalanceError> {
        match block_on(coin.enable_coin_balance(
            None::<&CountingDevice>,
            scan_policy,
            min_addresses_number,
            path_to_address,
        )) {
            Ok(EnableCoinBalance::HD(balance)) => Ok(balance),
            Ok(other) => panic!("an HD balance is expected, got {:?}", other),
            Err(e) => Err(e.into_inner()),
        }
    }

    fn paths(account: &HDAccountBalance) -> Vec<String> {
        account
            .addresses
            .iter()
            .map(|address| address.derivation_path.0.to_string())
            .collect()
    }

    fn stored(items: &StoredAccounts, account_id: u32) -> HDAccountStorageItem {
        items
            .lock()
            .unwrap()
            .get(&account_id)
            .cloned()
            .unwrap_or_else(|| panic!("account {} is not stored", account_id))
    }

    fn external_balance_page(coin: &UtxoStandardCoin, account_index: u32) -> HDAccountBalanceResponse {
        let params = AccountBalanceParams {
            account_index,
            chain: Bip44Chain::External,
            limit: 10,
            paging_options: PagingOptionsEnum::PageNumber(NonZeroUsize::new(1).unwrap()),
        };
        block_on(coin.account_balance_rpc(params)).expect("!account_balance_rpc")
    }

    // -----------------------------------------------------------------------
    // T14a (R35a)
    // -----------------------------------------------------------------------

    /// T14a, R35a items 1 and 2: a fresh HD wallet activated at the default `path_to_address` lists address `0` of
    /// account `0` in the activation result and in `account_balance`, with no `get_new_address` and no
    /// `min_addresses_number`.
    #[test]
    fn t14a_the_default_path_lists_address_zero_of_account_zero() {
        let items = mock_storage();
        mock_chain();
        let coin = hd_coin(HDAccountsMap::new());

        let balance = enable(
            &coin,
            EnableCoinScanPolicy::DoNotScan,
            0,
            Some(HDAddressId::from(&PathToAddress::default())),
        )
        .unwrap_or_else(|e| panic!("the activation failed: {}", e));

        assert_eq!(balance.accounts.len(), 1);
        assert_eq!(balance.accounts[0].account_index, 0);
        assert_eq!(paths(&balance.accounts[0]), ["m/44'/141'/0'/0/0"]);
        assert_eq!(stored(&items, 0).external_addresses_number, 1);
        assert_eq!(stored(&items, 0).internal_addresses_number, 0);

        let page = external_balance_page(&coin, 0);
        assert_eq!(page.total, 1);
        assert_eq!(page.addresses.len(), 1);
    }

    /// T14a, R35a items 1 and 2: with `account_id: 1, chain: External, address_id: 3` and no
    /// `min_addresses_number`, account `1` is created and reported alone -- although the wallet holds account `0`
    /// -- with its external addresses `0` to `3`.
    #[test]
    fn t14a_a_path_in_account_one_creates_it_and_reports_it_alone() {
        let items = mock_storage();
        mock_chain();
        let coin = hd_coin(HDAccountsMap::new());
        enable(
            &coin,
            EnableCoinScanPolicy::DoNotScan,
            0,
            Some(path_of(0, Bip44Chain::External, 0)),
        )
        .unwrap_or_else(|e| panic!("{}", e));
        assert_eq!(account_ids(&coin), [0]);

        let balance = enable(
            &coin,
            EnableCoinScanPolicy::DoNotScan,
            0,
            Some(path_of(1, Bip44Chain::External, 3)),
        )
        .unwrap_or_else(|e| panic!("the activation failed: {}", e));

        assert_eq!(balance.accounts.len(), 1, "the result reports the path's account only");
        assert_eq!(balance.accounts[0].account_index, 1);
        assert_eq!(balance.accounts[0].derivation_path.0.to_string(), "m/44'/141'/1'");
        assert_eq!(paths(&balance.accounts[0]), [
            "m/44'/141'/1'/0/0",
            "m/44'/141'/1'/0/1",
            "m/44'/141'/1'/0/2",
            "m/44'/141'/1'/0/3"
        ]);
        assert_eq!(account_ids(&coin), [0, 1]);
        assert_eq!(stored(&items, 1).external_addresses_number, 4);
        assert_eq!(
            stored(&items, 0).external_addresses_number,
            1,
            "account 0 is left alone"
        );
        assert_eq!(external_balance_page(&coin, 1).total, 4);
    }

    /// T14a, R35a item 2: after a restart -- the wallet rebuilt from the storage -- the activated addresses stay
    /// listed, in the activation result and in `account_balance`, whatever `path_to_address` the next activation
    /// names and although `get_new_address` never handed them out.
    #[test]
    fn t14a_a_restart_keeps_the_addresses_listed() {
        let items = mock_storage();
        mock_chain();
        let coin = hd_coin(HDAccountsMap::new());
        enable(
            &coin,
            EnableCoinScanPolicy::DoNotScan,
            0,
            Some(path_of(0, Bip44Chain::External, 0)),
        )
        .unwrap_or_else(|e| panic!("{}", e));
        enable(
            &coin,
            EnableCoinScanPolicy::DoNotScan,
            0,
            Some(path_of(1, Bip44Chain::External, 3)),
        )
        .unwrap_or_else(|e| panic!("{}", e));

        let restarted = reload(&items);
        assert_eq!(account_ids(&restarted), [0, 1]);
        assert_eq!(external_balance_page(&restarted, 1).total, 4);

        let balance = enable(
            &restarted,
            EnableCoinScanPolicy::DoNotScan,
            0,
            Some(path_of(0, Bip44Chain::External, 0)),
        )
        .unwrap_or_else(|e| panic!("the activation failed: {}", e));
        // The path's account exists: every stored account is reported.
        assert_eq!(balance.accounts.len(), 2);
        assert_eq!(paths(&balance.accounts[0]), ["m/44'/141'/0'/0/0"]);
        assert_eq!(balance.accounts[1].account_index, 1);
        assert_eq!(balance.accounts[1].addresses.len(), 4);
        assert_eq!(stored(&items, 1).external_addresses_number, 4);
    }

    /// T14a, R35a item 2: a path on the internal chain raises the internal chain, not the external one.
    #[test]
    fn t14a_the_internal_chain_is_raised_not_the_external() {
        let items = mock_storage();
        mock_chain();
        let coin = hd_coin(HDAccountsMap::new());

        let balance = enable(
            &coin,
            EnableCoinScanPolicy::DoNotScan,
            0,
            Some(path_of(0, Bip44Chain::Internal, 2)),
        )
        .unwrap_or_else(|e| panic!("the activation failed: {}", e));

        assert_eq!(balance.accounts.len(), 1);
        assert_eq!(paths(&balance.accounts[0]), [
            "m/44'/141'/0'/1/0",
            "m/44'/141'/0'/1/1",
            "m/44'/141'/0'/1/2"
        ]);
        assert!(balance.accounts[0]
            .addresses
            .iter()
            .all(|address| address.chain == Bip44Chain::Internal));
        assert_eq!(stored(&items, 0).internal_addresses_number, 3);
        assert_eq!(stored(&items, 0).external_addresses_number, 0);
    }

    /// T14a, R35a item 3: an `address_id` of 2^31 fails activation with the address-limit failure of the balance
    /// error set, and no address is derived or read.
    #[test]
    fn t14a_an_address_at_the_hardened_limit_fails_activation() {
        let items = mock_storage();
        NativeClient::list_all_transactions
            .mock_safe(|_, _| MockResult::Return(Box::new(futures01::future::ok(Vec::new()))));
        NativeClient::display_balances.mock_safe(|_, addresses: Vec<Address>, _| {
            // The known addresses of the new account are none: only an empty request may reach the node.
            assert!(
                addresses.is_empty(),
                "no balance is read for an address beyond the limit"
            );
            MockResult::Return(Box::new(futures01::future::ok(Vec::new())))
        });
        let coin = hd_coin(HDAccountsMap::new());

        let error = enable(
            &coin,
            EnableCoinScanPolicy::DoNotScan,
            0,
            Some(path_of(0, Bip44Chain::External, crypto::ChildNumber::HARDENED_FLAG)),
        )
        .err()
        .expect("the activation must fail");

        assert!(
            matches!(error, EnableCoinBalanceError::BalanceError(BalanceError::Internal(_))),
            "the address-limit failure of the balance error set: {}",
            error
        );
        assert_eq!(stored(&items, 0).external_addresses_number, 0);
    }

    /// T14a, R35a item 3, the corner below the limit: an `address_id` of 2^31 - 1 is a valid child index, yet the
    /// activation must hold addresses `0` to `2^31 - 1`, a count of 2^31, which is refused. The activation fails
    /// with the same address-limit failure as the limit case and no address is derived or read. Only the empty
    /// account row for the path is stored, with no address count.
    #[test]
    fn t14a_the_last_valid_address_id_needs_a_minimum_beyond_the_limit_and_fails_activation() {
        let items = mock_storage();
        NativeClient::list_all_transactions
            .mock_safe(|_, _| MockResult::Return(Box::new(futures01::future::ok(Vec::new()))));
        NativeClient::display_balances.mock_safe(|_, addresses: Vec<Address>, _| {
            // The known addresses of the new account are none: only an empty request may reach the node.
            assert!(
                addresses.is_empty(),
                "no balance is read for an address of a refused activation"
            );
            MockResult::Return(Box::new(futures01::future::ok(Vec::new())))
        });
        let coin = hd_coin(HDAccountsMap::new());

        let error = enable(
            &coin,
            EnableCoinScanPolicy::DoNotScan,
            0,
            Some(path_of(0, Bip44Chain::External, crypto::ChildNumber::HARDENED_FLAG - 1)),
        )
        .err()
        .expect("the activation must fail");

        assert!(
            matches!(error, EnableCoinBalanceError::BalanceError(BalanceError::Internal(_))),
            "the address-limit failure of the balance error set: {}",
            error
        );
        assert_eq!(stored(&items, 0).external_addresses_number, 0);
        assert_eq!(stored(&items, 0).internal_addresses_number, 0);
    }

    /// T14a, R35a item 2: the account the path names alone is raised to the activated address; every other account
    /// gets `min_addresses_number` alone.
    #[test]
    fn t14a_other_accounts_get_the_minimum_alone() {
        let items = mock_storage();
        mock_chain();
        let coin = hd_coin(HDAccountsMap::new());
        enable(
            &coin,
            EnableCoinScanPolicy::DoNotScan,
            0,
            Some(path_of(0, Bip44Chain::External, 0)),
        )
        .unwrap_or_else(|e| panic!("{}", e));
        enable(
            &coin,
            EnableCoinScanPolicy::DoNotScan,
            0,
            Some(path_of(1, Bip44Chain::External, 0)),
        )
        .unwrap_or_else(|e| panic!("{}", e));

        let balance = enable(
            &coin,
            EnableCoinScanPolicy::DoNotScan,
            2,
            Some(path_of(0, Bip44Chain::External, 4)),
        )
        .unwrap_or_else(|e| panic!("the activation failed: {}", e));

        assert_eq!(
            balance.accounts.len(),
            2,
            "the path's account exists: every account is reported"
        );
        assert_eq!(balance.accounts[0].addresses.len(), 5);
        assert_eq!(balance.accounts[1].addresses.len(), 2);
        assert_eq!(stored(&items, 0).external_addresses_number, 5);
        assert_eq!(stored(&items, 1).external_addresses_number, 2);
    }

    /// T14a: an iguana login is unchanged -- a single address, and no account or address is made known.
    #[test]
    fn t14a_an_iguana_login_is_unchanged() {
        let items = mock_storage();
        NativeClient::display_balance
            .mock_safe(|_, _: Address, _| MockResult::Return(Box::new(futures01::future::ok(BigDecimal::from(0)))));
        let coin = utxo_coin_for_test(UtxoRpcClientEnum::Native(native_client_for_test()), None, false);

        let balance = block_on(coin.enable_coin_balance(
            None::<&CountingDevice>,
            EnableCoinScanPolicy::default(),
            0,
            Some(path_of(2, Bip44Chain::External, 7)),
        ))
        .unwrap_or_else(|e| panic!("the activation failed: {}", e));

        assert!(matches!(balance, EnableCoinBalance::Iguana(_)));
        assert!(items.lock().unwrap().is_empty());
    }

    // -----------------------------------------------------------------------
    // T14c (R35c)
    // -----------------------------------------------------------------------

    /// The T14c sequence over a wallet that holds account `0`, for one coin type.
    fn t14c_sequence<Coin>(coin: &Coin, items: &StoredAccounts)
    where
        Coin: InitCreateHDAccountRpcOps + Sync,
    {
        let created = CreatedAccount::default();
        let create = |request: Json| -> MmResult<HDAccountBalance, HDWalletRpcError> {
            let params: CreateNewAccountParams = json::from_value(request).unwrap();
            block_on(coin.init_create_account_rpc(params, None::<&CountingDevice>, &created))
        };

        // The first call creates account 1 (account 0 comes from activation), without a hardware device.
        let first = create(json!({ "scan": false })).unwrap();
        assert_eq!(first.account_index, 1);
        assert_eq!(first.derivation_path.0.to_string(), "m/44'/141'/1'");
        assert!(first.addresses.is_empty(), "`scan: false` returns no addresses");
        assert_eq!(created.get(), Some(1));

        // A requested index creates that account.
        let three = create(json!({ "scan": false, "account_id": 3 })).unwrap();
        assert_eq!(three.account_index, 3);
        assert_eq!(created.get(), Some(3));

        // The next call without an index creates account 2.
        let two = create(json!({ "scan": false })).unwrap();
        assert_eq!(two.account_index, 2);

        // An index at 2^31 answers `AccountLimitReached`, HTTP 400.
        let error = create(json!({ "scan": false, "account_id": 2_147_483_648u64 }))
            .unwrap_err()
            .into_inner();
        assert!(
            matches!(error, HDWalletRpcError::AccountLimitReached {
                max_accounts_number: 2_147_483_648
            }),
            "{:?}",
            error
        );
        assert_eq!(error.status_code(), common::StatusCode::BAD_REQUEST);

        // A repeated `account_id` answers `AccountAlreadyExists`, HTTP 400.
        let error = create(json!({ "scan": false, "account_id": 3 }))
            .unwrap_err()
            .into_inner();
        assert!(
            matches!(error, HDWalletRpcError::AccountAlreadyExists { account_id: 3 }),
            "{:?}",
            error
        );
        assert_eq!(error.status_code(), common::StatusCode::BAD_REQUEST);

        // ... before any derivation: the device is never asked.
        let device = CountingDevice::new();
        let params: CreateNewAccountParams = json::from_value(json!({ "scan": false, "account_id": 3 })).unwrap();
        let error = block_on(coin.init_create_account_rpc(params, Some(&device), &created))
            .unwrap_err()
            .into_inner();
        assert!(
            matches!(error, HDWalletRpcError::AccountAlreadyExists { account_id: 3 }),
            "{:?}",
            error
        );
        assert_eq!(device.calls.load(std::sync::atomic::Ordering::SeqCst), 0);

        // The accounts are persisted.
        let stored_ids: Vec<u32> = items.lock().unwrap().keys().cloned().collect();
        assert_eq!(stored_ids, [1, 2, 3]);
    }

    /// T14c, R35c: the UTXO standard coin.
    #[test]
    fn t14c_utxo_standard_coin_creates_accounts_without_a_device() {
        let items = mock_storage();
        mock_chain();
        t14c_sequence(&hd_coin_with_account_zero(), &items);
    }

    /// T14c, R35c: QTUM shares the account creation of the UTXO coins.
    #[test]
    fn t14c_qtum_coin_creates_accounts_without_a_device() {
        let items = mock_storage();
        mock_chain();
        let fields = hd_fields(HDAccountsMap::from([(0, account_zero())]));
        let arc: UtxoArc = fields.into();
        t14c_sequence(&QtumCoin::from(arc), &items);
    }

    /// R35c: the extended public key of the new account is derived in software, from the wallet's own key.
    #[test]
    fn t14c_the_account_key_is_derived_in_software() {
        let items = mock_storage();
        mock_chain();
        let coin = hd_coin_with_account_zero();
        let params: CreateNewAccountParams = json::from_value(json!({ "scan": false, "account_id": 5 })).unwrap();
        block_on(coin.init_create_account_rpc(params, None::<&CountingDevice>, &CreatedAccount::default())).unwrap();

        let expected = crypto::derive_secp256k1_extended_pubkey(
            global_hd().root_priv_key().clone(),
            &DerivationPath::from_str("m/44'/141'/5'").unwrap(),
        )
        .unwrap();
        assert_eq!(stored(&items, 5).account_xpub, expected.to_string(bip32::Prefix::XPUB));
    }

    /// R35c item 2: `scan` defaults to `true`, `gap_limit` and `account_id` to the wallet's and the lowest unused
    /// index.
    #[test]
    fn r35c_item2_the_request_defaults() {
        let params: CreateNewAccountParams = json::from_value(json!({})).unwrap();
        assert!(params.scan);
        assert_eq!(params.gap_limit, None);
        assert_eq!(params.account_id, None);
        let params: CreateNewAccountParams =
            json::from_value(json!({ "scan": false, "gap_limit": 5, "account_id": 7 })).unwrap();
        assert!(!params.scan);
        assert_eq!(params.gap_limit, Some(5));
        assert_eq!(params.account_id, Some(7));
    }

    /// R35c item 4: an account a cancelled task created is removed again -- from the wallet and from the
    /// storage -- when it is the wallet's last account; any other account is kept.
    #[test]
    fn r35c_item4_cancelling_removes_the_last_account_only() {
        let items = mock_storage();
        mock_chain();
        let coin = hd_coin_with_account_zero();
        let created = CreatedAccount::default();
        for account_id in [1, 3] {
            let params: CreateNewAccountParams =
                json::from_value(json!({ "scan": false, "account_id": account_id })).unwrap();
            block_on(coin.init_create_account_rpc(params, None::<&CountingDevice>, &created)).unwrap();
        }
        assert_eq!(account_ids(&coin), [0, 1, 3]);

        // Account 1 is not the last one: it is kept.
        let removed = block_on(crate::hd_wallet::common_impl::remove_created_account(&coin, 1)).unwrap();
        assert!(!removed);
        assert_eq!(account_ids(&coin), [0, 1, 3]);

        // Account 3 is the last one.
        let removed = block_on(crate::hd_wallet::common_impl::remove_created_account(&coin, 3)).unwrap();
        assert!(removed);
        assert_eq!(account_ids(&coin), [0, 1]);
        let stored_ids: Vec<u32> = items.lock().unwrap().keys().cloned().collect();
        assert_eq!(stored_ids, [1]);

        // A restart does not bring it back.
        assert_eq!(account_ids(&reload(&items)), [1]);
    }

    /// R35c: no hardware context exists under a software login, and none is needed.
    #[test]
    fn t14c_no_hardware_context_is_needed_under_a_software_login() {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        CryptoCtx::init_with_global_hd_account(ctx.clone(), TEST_MNEMONIC).unwrap();
        assert!(!crate::rpc_command::init_create_account::uses_hardware_wallet(&ctx).unwrap());
    }

    /// The error a refused index becomes at the activation: a coin-creation failure, never a panic.
    #[test]
    fn r35a_item1_the_account_limit_applies_to_the_path_account() {
        let items = mock_storage();
        mock_chain();
        let coin = hd_coin(HDAccountsMap::new());

        let error = enable(
            &coin,
            EnableCoinScanPolicy::DoNotScan,
            0,
            Some(path_of(crypto::ChildNumber::HARDENED_FLAG, Bip44Chain::External, 0)),
        )
        .err()
        .expect("the activation must fail");

        assert!(
            matches!(
                error,
                EnableCoinBalanceError::NewAccountCreatingError(NewAccountCreatingError::AccountLimitReached { .. })
            ),
            "{}",
            error
        );
        assert!(items.lock().unwrap().is_empty());
    }

    // -----------------------------------------------------------------------
    // R35c / R35a: the key source follows the coin's key policy, not the presence of a hardware context
    // -----------------------------------------------------------------------

    /// The extended public key the software global-HD login derives at `path`.
    fn software_xpub(path: &str) -> String {
        crypto::derive_secp256k1_extended_pubkey(
            global_hd().root_priv_key().clone(),
            &DerivationPath::from_str(path).unwrap(),
        )
        .unwrap()
        .to_string(bip32::Prefix::XPUB)
    }

    /// A hardware device is offered (a hardware context exists beside the software login) to a software-keyed
    /// coin: the account is created from the software key, and the device is asked 0 times.
    fn r35c_software_coin_ignores_the_device_offered<Coin>(coin: &Coin, items: &StoredAccounts)
    where
        Coin: InitCreateHDAccountRpcOps + Sync,
    {
        let device = CountingDevice::new();
        let params: CreateNewAccountParams = json::from_value(json!({ "scan": false, "account_id": 5 })).unwrap();
        let account = block_on(coin.init_create_account_rpc(params, Some(&device), &CreatedAccount::default()))
            .unwrap_or_else(|e| panic!("the account creation failed: {:?}", e));

        assert_eq!(account.account_index, 5);
        assert_eq!(device.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(stored(items, 5).account_xpub, software_xpub("m/44'/141'/5'"));
    }

    /// R35c: `task::create_new_account` for a software UTXO coin derives the account key in software although a
    /// device is offered; the device is called 0 times and the account xpub equals the software one.
    #[test]
    fn r35c_a_software_utxo_coin_derives_its_account_key_in_software_with_a_device_offered() {
        let items = mock_storage();
        mock_chain();
        r35c_software_coin_ignores_the_device_offered(&hd_coin_with_account_zero(), &items);
    }

    /// R35c: QTUM shares the account creation of the UTXO coins, and the rule with it.
    #[test]
    fn r35c_a_software_qtum_coin_derives_its_account_key_in_software_with_a_device_offered() {
        let items = mock_storage();
        mock_chain();
        let arc: UtxoArc = hd_fields(HDAccountsMap::from([(0, account_zero())])).into();
        r35c_software_coin_ignores_the_device_offered(&QtumCoin::from(arc), &items);
    }

    /// R35a: the activation of a software UTXO coin creates its first account from the software key although a
    /// device is offered; the device is called 0 times and the account xpub equals the software one.
    #[test]
    fn r35a_activation_of_a_software_utxo_coin_derives_the_account_in_software_with_a_device_offered() {
        let items = mock_storage();
        mock_chain();
        let coin = hd_coin(HDAccountsMap::new());
        let device = CountingDevice::new();

        let balance = match block_on(coin.enable_coin_balance(
            Some(&device),
            EnableCoinScanPolicy::DoNotScan,
            0,
            Some(path_of(0, Bip44Chain::External, 0)),
        )) {
            Ok(EnableCoinBalance::HD(balance)) => balance,
            Ok(other) => panic!("an HD balance is expected, got {:?}", other),
            Err(e) => panic!("the activation failed: {}", e.into_inner()),
        };

        assert_eq!(paths(&balance.accounts[0]), ["m/44'/141'/0'/0/0"]);
        assert_eq!(device.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(stored(&items, 0).account_xpub, software_xpub("m/44'/141'/0'"));
    }

    /// R35c: a coin of the Trezor policy still takes the account key from the device; without the device
    /// offered it reports that the hardware context is missing, and asks nobody.
    #[test]
    fn r35c_a_trezor_policy_coin_still_asks_the_device() {
        let items = mock_storage();
        mock_chain();
        let mut fields = hd_fields(HDAccountsMap::from([(0, account_zero())]));
        fields.priv_key_policy = PrivKeyPolicy::Trezor;
        fields.conf.trezor_coin = Some(TrezorUtxoCoin::Komodo);
        let coin = utxo_coin_from_fields(fields);

        // The device is offered: the request reaches it.
        let device = CountingDevice::new();
        let params: CreateNewAccountParams = json::from_value(json!({ "scan": false, "account_id": 5 })).unwrap();
        let error = block_on(coin.init_create_account_rpc(params, Some(&device), &CreatedAccount::default()))
            .unwrap_err()
            .into_inner();
        assert!(
            matches!(error, HDWalletRpcError::HwContextNotInitialized),
            "{:?}",
            error
        );
        assert_eq!(device.calls.load(std::sync::atomic::Ordering::SeqCst), 1);

        // The device is gone: the coin does not fall back to the software key.
        let params: CreateNewAccountParams = json::from_value(json!({ "scan": false, "account_id": 5 })).unwrap();
        let error = block_on(coin.init_create_account_rpc(params, None::<&CountingDevice>, &CreatedAccount::default()))
            .unwrap_err()
            .into_inner();
        assert!(
            matches!(error, HDWalletRpcError::HwContextNotInitialized),
            "{:?}",
            error
        );
        assert!(!items.lock().unwrap().contains_key(&5));
    }

    // -----------------------------------------------------------------------
    // R54 / T35: the hardware context is looked up at call time, for a coin of the Trezor policy only
    // -----------------------------------------------------------------------

    pub(crate) const DEVICE_PUBKEY: &str = "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";

    /// A node under the software global-HD login, with `device` deciding whether the hardware slot is filled,
    /// and with `coins` registered as activated. The wallet storage holds account `0` of each coin.
    fn node_with_hd_coins(
        device: bool,
        coins: Vec<(&str, UtxoStandardCoin)>,
        items: &StoredAccounts,
    ) -> mm2_core::mm_ctx::MmArc {
        use crate::{CoinsContext, MmCoinEnum};
        use primitives::hash::H264;

        let ctx = MmCtxBuilder::default().into_mm_arc();
        let crypto_ctx = CryptoCtx::init_with_global_hd_account(ctx.clone(), TEST_MNEMONIC).unwrap();
        if device {
            crypto_ctx.init_trezor_ctx_for_tests(H264::from_str(DEVICE_PUBKEY).unwrap(), None);
        }
        let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();
        block_on(async {
            let mut activated = coins_ctx.coins.lock().await;
            for (ticker, coin) in coins {
                activated.insert(ticker.to_owned(), MmCoinEnum::UtxoCoin(coin));
            }
        });
        items.lock().unwrap().insert(0, HDAccountStorageItem {
            account_id: 0,
            account_xpub: account_zero().extended_pubkey.to_string(bip32::Prefix::XPUB),
            external_addresses_number: 0,
            internal_addresses_number: 0,
        });
        ctx
    }

    /// Mocks the node for a single address: its balance is zero.
    fn mock_address_balance() {
        NativeClient::display_balance
            .mock_safe(|_, _, _| MockResult::Return(Box::new(futures01::future::ok(BigDecimal::from(0)))));
    }

    /// A device with the public key `pubkey`: the hardware context an activation would have used, minted on a node
    /// of its own. Two contexts of the same public key are the same device (CRD ch.05 R51).
    pub(crate) fn device_context(pubkey: &str) -> crypto::HardwareWalletArc {
        use primitives::hash::H264;

        let ctx = MmCtxBuilder::default().into_mm_arc();
        let crypto_ctx = CryptoCtx::init_with_global_hd_account(ctx, TEST_MNEMONIC).unwrap();
        crypto_ctx.init_trezor_ctx_for_tests(H264::from_str(pubkey).unwrap(), None)
    }

    /// A coin of the Trezor policy activated with `device` (CRD ch.05 R58a).
    fn trezor_policy_hd_coin_activated_with(device: Option<crypto::HardwareWalletArc>) -> UtxoStandardCoin {
        let mut fields = hd_fields(HDAccountsMap::from([(0, account_zero())]));
        fields.priv_key_policy = PrivKeyPolicy::Trezor;
        fields.conf.trezor_coin = Some(TrezorUtxoCoin::Komodo);
        fields.hw_ctx = device;
        utxo_coin_from_fields(fields)
    }

    /// A coin of the Trezor policy activated with device A (`DEVICE_PUBKEY`).
    fn trezor_policy_hd_coin() -> UtxoStandardCoin {
        trezor_policy_hd_coin_activated_with(Some(device_context(DEVICE_PUBKEY)))
    }

    fn new_address_request(ticker: &str) -> crate::hd_wallet::GetNewHDAddressRequest {
        json::from_value(json!({ "coin": ticker, "account_id": 0, "chain": "External" })).unwrap()
    }

    /// R54, T35: a coin of the Trezor policy whose hardware slot is empty answers `HwContextNotInitialized`
    /// (HTTP 400) to `get_new_address`, and the software coin of the same daemon succeeds.
    #[test]
    fn r54_t35_a_trezor_policy_coin_with_an_empty_slot_answers_hw_context_not_initialized() {
        let items = mock_storage();
        mock_chain();
        mock_address_balance();
        let ctx = node_with_hd_coins(
            false,
            vec![("TRZ", trezor_policy_hd_coin()), ("DOC", hd_coin_with_account_zero())],
            &items,
        );

        let error = block_on(crate::hd_wallet::get_new_address(
            ctx.clone(),
            new_address_request("TRZ"),
        ))
        .err()
        .expect("the Trezor-policy coin needs the device")
        .into_inner();
        assert!(
            matches!(error, HDWalletRpcError::HwContextNotInitialized),
            "{:?}",
            error
        );
        assert_eq!(error.status_code(), common::StatusCode::BAD_REQUEST);

        block_on(crate::hd_wallet::get_new_address(ctx, new_address_request("DOC")))
            .unwrap_or_else(|e| panic!("the software coin must get its address: {:?}", e));
    }

    /// R54: with the slot filled, the Trezor-policy coin passes the check; a software coin does not consult the
    /// slot, filled or not (T32).
    #[test]
    fn r54_presence_of_the_context_is_looked_up_for_the_trezor_policy_coin_only() {
        let items = mock_storage();
        mock_chain();
        mock_address_balance();
        let ctx = node_with_hd_coins(
            true,
            vec![("TRZ", trezor_policy_hd_coin()), ("DOC", hd_coin_with_account_zero())],
            &items,
        );

        for ticker in ["TRZ", "DOC"] {
            block_on(crate::hd_wallet::get_new_address(
                ctx.clone(),
                new_address_request(ticker),
            ))
            .unwrap_or_else(|e| panic!("{}: the new address must be given: {:?}", ticker, e));
        }
    }

    // -----------------------------------------------------------------------
    // R58a / T36: a coin of the Trezor policy keeps the identity of the device it was activated with
    // -----------------------------------------------------------------------

    /// A second device, with another seed than `DEVICE_PUBKEY` (CRD ch.05 R51).
    pub(crate) const DEVICE_PUBKEY_B: &str = "03774ae7f858a9411e5ef4246b70c65aac5649980be5c17891bbec17895da008cb";

    /// Replaces the device of the hardware slot of `ctx` with device B, as a new `task::init_trezor::init` does.
    fn replace_the_device_with_b(ctx: &mm2_core::mm_ctx::MmArc) {
        use primitives::hash::H264;

        CryptoCtx::from_ctx(ctx)
            .unwrap()
            .init_trezor_ctx_for_tests(H264::from_str(DEVICE_PUBKEY_B).unwrap(), None);
    }

    fn account_request(account_id: u32) -> CreateNewAccountParams {
        json::from_value(json!({ "scan": false, "account_id": account_id })).unwrap()
    }

    /// R58a, T36: after the device was replaced, `get_new_address` of the Trezor-policy coin answers
    /// `FoundUnexpectedDevice` and changes nothing in the storage. With device A current it works, and the
    /// software coin of the same node is not affected by the change.
    #[test]
    fn r58a_t36_get_new_address_refuses_a_replaced_device() {
        let items = mock_storage();
        mock_chain();
        mock_address_balance();
        let ctx = node_with_hd_coins(
            true,
            vec![("TRZ", trezor_policy_hd_coin()), ("DOC", hd_coin_with_account_zero())],
            &items,
        );

        // Device A is still current: the address is given.
        block_on(crate::hd_wallet::get_new_address(
            ctx.clone(),
            new_address_request("TRZ"),
        ))
        .unwrap_or_else(|e| panic!("the device of the activation is current: {:?}", e));
        let stored_with_device_a = items.lock().unwrap().clone();

        replace_the_device_with_b(&ctx);

        let error = block_on(crate::hd_wallet::get_new_address(
            ctx.clone(),
            new_address_request("TRZ"),
        ))
        .err()
        .expect("the replaced device must be refused")
        .into_inner();
        assert!(
            matches!(error, HDWalletRpcError::FoundUnexpectedDevice(_)),
            "{:?}",
            error
        );
        assert_eq!(error.status_code(), common::StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(*items.lock().unwrap(), stored_with_device_a, "the storage is unchanged");

        block_on(crate::hd_wallet::get_new_address(ctx, new_address_request("DOC")))
            .unwrap_or_else(|e| panic!("a software coin is not affected by a device change: {:?}", e));
    }

    /// R58a, T36: after the device was replaced, `task::create_new_account` of the Trezor-policy coin answers
    /// `FoundUnexpectedDevice`; the device is asked 0 times and nothing is stored. The same holds for the read of
    /// the account's extended public key itself.
    #[test]
    fn r58a_t36_account_creation_and_xpub_read_refuse_a_replaced_device() {
        use crate::hd_pubkey::ExtractExtendedPubkey;

        let items = mock_storage();
        mock_chain();
        let coin = trezor_policy_hd_coin();
        // The extractor of the running task reads from the current context, which holds device B.
        let device_b = CountingDevice::with_context(device_context(DEVICE_PUBKEY_B));

        let error =
            block_on(coin.init_create_account_rpc(account_request(5), Some(&device_b), &CreatedAccount::default()))
                .unwrap_err()
                .into_inner();
        assert!(
            matches!(error, HDWalletRpcError::FoundUnexpectedDevice(_)),
            "{:?}",
            error
        );
        assert_eq!(device_b.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(items.lock().unwrap().is_empty(), "nothing is stored");

        let xpub_error =
            block_on(coin.extract_extended_pubkey(Some(&device_b), DerivationPath::from_str("m/44'/141'/5'").unwrap()))
                .err()
                .expect("the replaced device must be refused")
                .into_inner();
        assert!(matches!(
            xpub_error,
            HDExtractPubkeyError::HardwareWalletError(crypto::HwError::FoundUnexpectedDevice { .. })
        ));
        assert_eq!(device_b.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    /// R58a, T36: the account an activation creates is read from the device too, so a replaced device is refused
    /// there as well.
    #[test]
    fn r58a_t36_the_account_an_activation_creates_refuses_a_replaced_device() {
        let items = mock_storage();
        mock_chain();
        let mut fields = hd_fields(HDAccountsMap::new());
        fields.priv_key_policy = PrivKeyPolicy::Trezor;
        fields.conf.trezor_coin = Some(TrezorUtxoCoin::Komodo);
        fields.hw_ctx = Some(device_context(DEVICE_PUBKEY));
        let coin = utxo_coin_from_fields(fields);
        let device_b = CountingDevice::with_context(device_context(DEVICE_PUBKEY_B));

        match block_on(coin.enable_coin_balance(
            Some(&device_b),
            EnableCoinScanPolicy::DoNotScan,
            0,
            Some(path_of(0, Bip44Chain::External, 0)),
        )) {
            Err(e) => match e.into_inner() {
                EnableCoinBalanceError::NewAccountCreatingError(NewAccountCreatingError::HardwareWalletError(
                    crypto::HwError::FoundUnexpectedDevice { .. },
                )) => (),
                other => panic!("FoundUnexpectedDevice is expected, got {}", other),
            },
            Ok(other) => panic!("the replaced device must be refused, got {:?}", other),
        }
        assert_eq!(device_b.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(items.lock().unwrap().is_empty(), "nothing is stored");
    }

    /// R58a: with the device of the activation current, the calls pass the check: the account creation reaches the
    /// device (which answers nothing in this test).
    #[test]
    fn r58a_the_device_of_the_activation_is_accepted() {
        let items = mock_storage();
        mock_chain();
        let coin = trezor_policy_hd_coin();
        let device_a = CountingDevice::with_context(device_context(DEVICE_PUBKEY));

        let error =
            block_on(coin.init_create_account_rpc(account_request(5), Some(&device_a), &CreatedAccount::default()))
                .unwrap_err()
                .into_inner();
        assert!(
            matches!(error, HDWalletRpcError::HwContextNotInitialized),
            "the device's own answer is expected: {:?}",
            error
        );
        assert_eq!(device_a.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(!items.lock().unwrap().contains_key(&5));
    }

    /// R58a, R54: an empty slot still answers `HwContextNotInitialized`, whatever device the coin was activated with.
    #[test]
    fn r58a_an_empty_slot_still_answers_hw_context_not_initialized() {
        let items = mock_storage();
        mock_chain();
        mock_address_balance();
        let ctx = node_with_hd_coins(false, vec![("TRZ", trezor_policy_hd_coin())], &items);

        let error = block_on(crate::hd_wallet::get_new_address(ctx, new_address_request("TRZ")))
            .err()
            .expect("the Trezor-policy coin needs the device")
            .into_inner();
        assert!(
            matches!(error, HDWalletRpcError::HwContextNotInitialized),
            "{:?}",
            error
        );

        let error = block_on(trezor_policy_hd_coin().init_create_account_rpc(
            account_request(5),
            None::<&CountingDevice>,
            &CreatedAccount::default(),
        ))
        .unwrap_err()
        .into_inner();
        assert!(
            matches!(error, HDWalletRpcError::HwContextNotInitialized),
            "{:?}",
            error
        );
    }

    /// R58a: a software coin is not affected by a device change: it takes its account key from the seed, the
    /// device is asked 0 times, and it never compares a device.
    #[test]
    fn r58a_a_software_coin_is_not_affected_by_a_device_change() {
        let items = mock_storage();
        mock_chain();
        let coin = hd_coin_with_account_zero();
        let device_b = CountingDevice::with_context(device_context(DEVICE_PUBKEY_B));

        let account =
            block_on(coin.init_create_account_rpc(account_request(5), Some(&device_b), &CreatedAccount::default()))
                .unwrap_or_else(|e| panic!("the account creation failed: {:?}", e));

        assert_eq!(account.account_index, 5);
        assert_eq!(device_b.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(stored(&items, 5).account_xpub, software_xpub("m/44'/141'/5'"));
    }

    /// R58a: a coin of the Trezor policy that keeps no record of its device (no activated coin is in that state)
    /// is refused rather than trusted, by the account read and by `get_new_address`; the device is not asked.
    #[test]
    fn r58a_a_coin_without_a_record_of_its_device_is_refused() {
        let items = mock_storage();
        mock_chain();
        mock_address_balance();
        let device_a = CountingDevice::with_context(device_context(DEVICE_PUBKEY));

        let error = block_on(trezor_policy_hd_coin_activated_with(None).init_create_account_rpc(
            account_request(5),
            Some(&device_a),
            &CreatedAccount::default(),
        ))
        .unwrap_err()
        .into_inner();
        assert!(
            matches!(error, HDWalletRpcError::HardwareWalletInternal(_)),
            "{:?}",
            error
        );
        assert_eq!(device_a.calls.load(std::sync::atomic::Ordering::SeqCst), 0);

        let ctx = node_with_hd_coins(true, vec![("TRZ", trezor_policy_hd_coin_activated_with(None))], &items);
        let error = block_on(crate::hd_wallet::get_new_address(ctx, new_address_request("TRZ")))
            .err()
            .expect("a coin without a record of its device is refused")
            .into_inner();
        assert!(
            matches!(error, HDWalletRpcError::HardwareWalletInternal(_)),
            "{:?}",
            error
        );
    }

    /// R58a, T36: signing takes its hardware context through one check. The device of the activation passes, an
    /// empty slot answers `NoTrezorDeviceAvailable` as before, and another device answers `FoundUnexpectedDevice`
    /// (HTTP 410 of the withdraw error class), all before any device is contacted. Both withdraw routes, the UTXO
    /// one (`InitUtxoWithdraw::trezor_client`) and the EVM one (`eth_trezor_withdraw::trezor_client`), call this
    /// check; they need a task handle to run, so only the check itself is run here.
    #[test]
    fn r58a_t36_signing_refuses_a_replaced_device_before_the_device_is_contacted() {
        use crate::hd_pubkey::hardware_context_for_signing;
        use crate::WithdrawError;

        let items = mock_storage();
        let ctx = node_with_hd_coins(true, vec![], &items);
        let activated_with = device_context(DEVICE_PUBKEY);

        assert!(hardware_context_for_signing(&ctx, Some(&activated_with)).is_ok());

        replace_the_device_with_b(&ctx);
        let error = hardware_context_for_signing(&ctx, Some(&activated_with))
            .err()
            .expect("the replaced device must be refused")
            .into_inner();
        assert!(matches!(error, WithdrawError::FoundUnexpectedDevice(_)), "{:?}", error);
        assert_eq!(error.status_code(), common::StatusCode::GONE);

        let error = hardware_context_for_signing(&ctx, None)
            .err()
            .expect("a coin without a record of its device is refused")
            .into_inner();
        assert!(matches!(error, WithdrawError::HardwareWalletInternal(_)), "{:?}", error);

        CryptoCtx::from_ctx(&ctx).unwrap().reset_hw_ctx();
        let error = hardware_context_for_signing(&ctx, Some(&activated_with))
            .err()
            .expect("an empty slot has no device")
            .into_inner();
        assert!(matches!(error, WithdrawError::NoTrezorDeviceAvailable), "{:?}", error);
    }
}
