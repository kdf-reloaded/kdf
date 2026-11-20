//! Unit tests of the TRON HD wallet at the coin level (CRD ch.21 R-A2, R21.10B.4, R21.10B.8, T21.10B.8): the key and
//! address the wallet activates, the used-address rule, the scan, and the withdraw from an HD `from`.
//!
//! The coin is built the way the mmrpc 2.0 activation builds it (`tron_coin_from_params_with_global_hd`, below the
//! availability gate `TRON_ACTIVATION_AVAILABLE`, which stays `false`) against a scripted stub node on the loopback
//! interface. The login is a global-HD one over a fixed public BIP-39 test mnemonic.

use super::nodes::TronNode;
use super::proto;
use super::sign::hash_transaction_raw;
use super::test_node::{Reply, StubNode};
use super::{parse_trc20_contract, tron_coin_from_params, tron_coin_from_params_with_global_hd, Network, TronAddress,
            TronCoinParams};
use crate::coin_balance::{EnableCoinScanPolicy, HDAddressBalanceScanner, HDWalletBalanceOps};
use crate::eth::{withdraw_impl, Erc20TokenInfo, EthCoin, EthCoinBuildError};
use crate::hd_pubkey::{HDExtractPubkeyError, HDXPubExtractor};
use crate::hd_wallet::{GetNewHDAddressParams, HDAccountOps, HDAddressId, HDWalletOps, HDWalletRpcOps};
use crate::rpc_command::account_balance::{AccountBalanceParams, AccountBalanceRpcOps};
use crate::rpc_command::get_my_address::PathToAddress;
use crate::rpc_command::init_scan_for_new_addresses::{InitScanAddressesRpcOps, ScanAddressesParams};
use crate::{lp_register_coin, CoinProtocol, DerivationMethod, MarketCoinOps, MmCoinEnum, RegisterCoinParams,
            TransactionDetails, WithdrawError, WithdrawRequest};
use async_trait::async_trait;
use common::block_on;
use crypto::trezor::utxo::TrezorUtxoCoin;
use crypto::{Bip44Chain, CryptoCtx, DerivationPath, GlobalHDAccountArc, KeyPairPolicy, XPub};
use mm2_core::mm_ctx::{MmArc, MmCtxBuilder};
use mm2_err_handle::prelude::*;
use mm2_eth::keys::{verify_address, Signature};
use prost::Message as _;
use serde_json::{json, Value as Json};
use std::str::FromStr;
use std::sync::{Arc, Mutex};

const TEST_MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
const TRC20_CONTRACT: &str = "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t";
const RECEIVER: &str = "TNPeeaaFB7K9cmo4uQpcU32zGK8G1NYqeL";

// The addresses and the secret below are the values at `m/44'/195'/0'/0/N` of `TEST_MNEMONIC`. They were computed
// with a stand-alone BIP-39 / BIP-32 / keccak-256 / base58check implementation written for this test (pure Python,
// not taken from any web source), and every test that uses them also recomputes the address from the seed with the
// crypto crate (`independent_address`), so a wrong literal fails there.
const ADDRESS_0: &str = "TUEZSdKsoDHQMeZwihtdoBiN46zxhGWYdH";
const ADDRESS_1: &str = "TSeJkUh4Qv67VNFwY8LaAxERygNdy6NQZK";
const ADDRESS_2: &str = "TYJPRrdB5APNeRs4R7fYZSwW3TcrTKw2gx";
/// The secret key at `m/44'/195'/0'/0/0`, as `show_priv_key` answers it.
const SECRET_0: &str = "0xb5a4cea271ff424d7c31dc12a3e43e401df7a40d7412a15750f3f0b6b5449a28";

fn hd_ctx() -> MmArc {
    let conf = json!({
        "enable_hd": true,
        "coins": [
            {
                "coin": "TRX",
                "name": "tron",
                "protocol": { "type": "TRX", "protocol_data": { "network": "Nile" } },
                "derivation_path": "m/44'/195'",
                "wallet_only": true,
                "mm2": 1
            },
            {
                "coin": "USDT-TRC20",
                "protocol": { "type": "TRC20", "protocol_data": { "platform": "TRX", "contract_address": TRC20_CONTRACT } },
                "decimals": 6,
                "mm2": 1
            }
        ]
    });
    let ctx = MmCtxBuilder::new().with_conf(conf).into_mm_arc();
    CryptoCtx::init_with_global_hd_account(ctx.clone(), TEST_MNEMONIC).expect("initialize the crypto context");
    let connection = db_common::sqlite::rusqlite::Connection::open_in_memory().expect("in-memory database");
    let _ = ctx.sqlite_connection.pin(Arc::new(Mutex::new(connection)));
    ctx
}

fn global_hd(ctx: &MmArc) -> GlobalHDAccountArc {
    match CryptoCtx::from_ctx(ctx).expect("a crypto context").key_pair_policy() {
        KeyPairPolicy::GlobalHDAccount(global_hd) => global_hd.clone(),
        KeyPairPolicy::Iguana => panic!("an HD context is expected"),
    }
}

/// The base58check address at `path`, computed from the seed with the crypto crate alone: the public key at the
/// path, and the TRON address of that key. It does not go through the coin.
fn independent_address(ctx: &MmArc, path: &str) -> String {
    let pubkey = global_hd(ctx)
        .derive_account_extended_pubkey(&DerivationPath::from_str(path).expect("a derivation path"))
        .expect("the path derives");
    let uncompressed = pubkey.public_key().serialize_uncompressed();
    super::addr_from_pubkey_str(&hex::encode(uncompressed)).expect("an address of the key")
}

/// The 20-byte payload of a base58check address, as 40 lowercase hex digits.
fn payload_hex(base58: &str) -> String {
    format!(
        "{:x}",
        TronAddress::from_base58(base58)
            .expect("a valid address")
            .to_evm_address()
    )
}

/// A scripted node: the accounts of `accounts` (base58check) exist on chain with 2.5 TRX, every other account is the
/// documented empty answer; the token holders of `holders` (base58check) hold 1.234567 of a 6-decimal token, every
/// other address holds none. For a withdraw it serves the inputs of the fee estimate (CRD ch.21 R21.10B.7): every
/// address has the free bandwidth of a fresh account and staked energy above the 14 000 a token transfer's dry run
/// uses, and the chain prices include both account-creation fees.
fn scripted_node(accounts: &[&str], holders: &[&str]) -> StubNode {
    let accounts: Vec<String> = accounts.iter().map(|address| (*address).to_owned()).collect();
    let holders: Vec<String> = holders.iter().map(|address| payload_hex(address)).collect();
    StubNode::start(move |path, body| {
        let request: Json = serde_json::from_str(body).unwrap_or(Json::Null);
        match path {
            "/wallet/getnowblock" => Reply::ok(
                &json!({
                    "blockID": "ab".repeat(32),
                    "block_header": { "raw_data": { "number": 4242, "timestamp": 1_700_000_000_000i64 } }
                })
                .to_string(),
            ),
            "/wallet/getaccount" => {
                let address = request["address"].as_str().unwrap_or_default();
                if accounts.iter().any(|known| known == address) {
                    Reply::ok(r#"{"balance":2500000,"create_time":1700000000000}"#)
                } else {
                    Reply::ok("{}")
                }
            },
            "/wallet/triggerconstantcontract" => match request["function_selector"].as_str() {
                Some("decimals()") => Reply::ok(&json!({ "constant_result": [format!("{:064x}", 6)] }).to_string()),
                Some("balanceOf(address)") => {
                    // The parameter is one ABI word: 12 zero bytes and the 20-byte owner.
                    let parameter = request["parameter"].as_str().unwrap_or_default();
                    let owner = parameter.get(24..).unwrap_or_default();
                    let units: u64 = if holders.iter().any(|holder| holder == owner) {
                        1_234_567
                    } else {
                        0
                    };
                    Reply::ok(&json!({ "constant_result": [format!("{units:064x}")] }).to_string())
                },
                Some("transfer(address,uint256)") => {
                    Reply::ok(&json!({ "result": { "result": true }, "energy_used": 14_000 }).to_string())
                },
                _ => Reply::status(404, "{}"),
            },
            "/wallet/getaccountresource" => Reply::ok(r#"{"freeNetLimit":600,"EnergyLimit":100000}"#),
            "/wallet/getchainparameters" => Reply::ok(
                &json!({ "chainParameter": [
                    { "key": "getTransactionFee", "value": 1000 },
                    { "key": "getEnergyFee", "value": 420 },
                    { "key": "getCreateNewAccountFeeInSystemContract", "value": 1_000_000 },
                    { "key": "getCreateAccountFee", "value": 100_000 }
                ] })
                .to_string(),
            ),
            other => Reply::status(404, &format!("unexpected endpoint {other}")),
        }
    })
}

fn params(node_url: &str) -> TronCoinParams {
    TronCoinParams {
        nodes: vec![TronNode::plain(node_url.to_owned())],
        required_confirmations: 1,
        tx_history: false,
    }
}

/// The TRX platform coin of the HD wallet at `path_to_address`, built as the activation builds it.
async fn hd_coin_at(ctx: &MmArc, node_url: &str, path_to_address: &PathToAddress) -> EthCoin {
    let conf = crate::coin_conf(ctx, "TRX");
    tron_coin_from_params_with_global_hd(
        ctx,
        "TRX",
        &conf,
        params(node_url),
        &global_hd(ctx),
        path_to_address,
        20,
        CoinProtocol::TRX { network: Network::Nile },
    )
    .await
    .unwrap_or_else(|e| panic!("the HD coin must build: {}", e.get_inner()))
}

async fn hd_coin(ctx: &MmArc, node_url: &str) -> EthCoin { hd_coin_at(ctx, node_url, &PathToAddress::default()).await }

/// A software login has no device: asking one is a failure of the test.
struct NoDevice;

#[async_trait]
impl HDXPubExtractor for NoDevice {
    async fn extract_utxo_xpub(
        &self,
        _trezor_utxo_coin: TrezorUtxoCoin,
        _derivation_path: DerivationPath,
    ) -> MmResult<XPub, HDExtractPubkeyError> {
        panic!("a software login must not ask a hardware device")
    }

    fn hardware_context(&self) -> Option<crypto::HardwareWalletArc> { None }
}

/// Enables the wallet of `coin`: account 0 is created, the enabled address listed, and the first `min` addresses
/// of the account known. No scan.
async fn enable_wallet(coin: &EthCoin, min: u32) {
    let DerivationMethod::HDWallet(hd_wallet) = &coin.derivation_method else {
        panic!("an HD wallet is expected");
    };
    let path = HDAddressId {
        account_id: 0,
        chain: Bip44Chain::External,
        address_id: 0,
    };
    coin.enable_hd_wallet(
        hd_wallet,
        None::<&NoDevice>,
        EnableCoinScanPolicy::DoNotScan,
        min,
        Some(path),
    )
    .await
    .unwrap_or_else(|e| panic!("the wallet must enable: {}", e.get_inner()));
}

async fn known_addresses_number(coin: &EthCoin) -> u32 {
    let DerivationMethod::HDWallet(hd_wallet) = &coin.derivation_method else {
        panic!("an HD wallet is expected");
    };
    let account = hd_wallet.get_account(0).await.expect("account 0 exists");
    account
        .known_addresses_number(Bip44Chain::External)
        .expect("the external chain")
}

// ---------------------------------------------------------------------------------------------------------------
// T21.10B.8, R-A2: the key and the address
// ---------------------------------------------------------------------------------------------------------------

/// T21.10B.8: the activated address is the base58check address at `m/44'/195'/0'/0/0`, computed here from the seed
/// with the crypto crate; the pinned literal agrees with it, the second address likewise, and `show_priv_key`
/// answers the derived key (CRD ch.21 R21.10B.8).
#[test]
fn the_activated_address_is_the_derived_address_and_the_vector_is_pinned() {
    let ctx = hd_ctx();
    let node = scripted_node(&[], &[]);
    block_on(async {
        assert_eq!(independent_address(&ctx, "m/44'/195'/0'/0/0"), ADDRESS_0);
        assert_eq!(independent_address(&ctx, "m/44'/195'/0'/0/1"), ADDRESS_1);

        let coin = hd_coin(&ctx, &node.url()).await;
        assert_eq!(coin.my_address().expect("an address"), ADDRESS_0);
        assert!(matches!(coin.derivation_method, DerivationMethod::HDWallet(_)));
        assert_eq!(coin.decimals, super::TRX_DECIMALS);
        assert_eq!(
            MarketCoinOps::display_priv_key(&coin).expect("a local key is shown"),
            SECRET_0
        );

        // `path_to_address` selects the enabled address below the coin's derivation path.
        let second = PathToAddress {
            account_id: 0,
            chain: Bip44Chain::External,
            address_id: 1,
        };
        let coin = hd_coin_at(&ctx, &node.url(), &second).await;
        assert_eq!(coin.my_address().expect("an address"), ADDRESS_1);
    });
    assert!(node.requests().is_empty(), "building the wallet asks no node");
}

/// A coin config without `derivation_path` is `InvalidPayload` (CRD ch.21 R21.10B.4), before any node request; the
/// HD builder refuses a protocol that is not TRX.
#[test]
fn a_config_without_a_derivation_path_is_invalid_payload_and_a_token_protocol_is_refused() {
    let ctx = hd_ctx();
    let node = scripted_node(&[], &[]);
    block_on(async {
        let result = tron_coin_from_params_with_global_hd(
            &ctx,
            "TRX",
            &json!({}),
            params(&node.url()),
            &global_hd(&ctx),
            &PathToAddress::default(),
            20,
            CoinProtocol::TRX { network: Network::Nile },
        )
        .await;
        assert!(matches!(
            result.map_err(MmError::into_inner),
            Err(EthCoinBuildError::InvalidPayload(_))
        ));

        let conf = crate::coin_conf(&ctx, "USDT-TRC20");
        let result = tron_coin_from_params_with_global_hd(
            &ctx,
            "USDT-TRC20",
            &conf,
            params(&node.url()),
            &global_hd(&ctx),
            &PathToAddress::default(),
            20,
            CoinProtocol::TRC20 {
                platform: "TRX".to_owned(),
                contract_address: TRC20_CONTRACT.to_owned(),
            },
        )
        .await;
        assert!(matches!(
            result.map_err(MmError::into_inner),
            Err(EthCoinBuildError::Internal(_))
        ));
    });
    assert!(node.requests().is_empty());
}

// ---------------------------------------------------------------------------------------------------------------
// R21.10B.4: the used-address rule and the scan
// ---------------------------------------------------------------------------------------------------------------

fn evm(base58: &str) -> ethereum_types::Address { TronAddress::from_base58(base58).expect("valid").to_evm_address() }

/// An address is used when the chain has created its account, or when an enabled TRC20 token has a non-zero balance
/// at it; a token that is not enabled does not count, and a failed node read is an error, never "not used".
#[test]
fn an_address_is_used_when_the_chain_has_its_account_or_an_enabled_token_balance_is_not_zero() {
    let ctx = hd_ctx();
    // ADDRESS_1 has its account on chain, ADDRESS_2 holds the token only, RECEIVER has neither.
    let node = scripted_node(&[ADDRESS_1], &[ADDRESS_2]);
    block_on(async {
        let coin = hd_coin(&ctx, &node.url()).await;

        // No token enabled yet: the token balance at ADDRESS_2 does not count.
        let scanner = coin.produce_hd_address_scanner().await.expect("a scanner");
        assert!(scanner.is_address_used(&evm(ADDRESS_1)).await.expect("read"));
        assert!(!scanner.is_address_used(&evm(ADDRESS_2)).await.expect("read"));
        assert!(!scanner.is_address_used(&evm(RECEIVER)).await.expect("read"));

        // With the token enabled, the token balance alone marks the address used.
        coin.add_erc20_token_info("USDT-TRC20".to_owned(), Erc20TokenInfo {
            token_addr: parse_trc20_contract(TRC20_CONTRACT).expect("a contract"),
            decimals: 6,
        });
        let scanner = coin.produce_hd_address_scanner().await.expect("a scanner");
        assert!(scanner.is_address_used(&evm(ADDRESS_1)).await.expect("read"));
        assert!(scanner.is_address_used(&evm(ADDRESS_2)).await.expect("read"));
        assert!(!scanner.is_address_used(&evm(RECEIVER)).await.expect("read"));
    });

    // A node that fails every request: the read is an error.
    let failing = StubNode::start(|_, _| Reply::status(500, "{}"));
    block_on(async {
        let coin = hd_coin(&ctx, &failing.url()).await;
        let scanner = coin.produce_hd_address_scanner().await.expect("a scanner");
        assert!(scanner.is_address_used(&evm(ADDRESS_1)).await.is_err());
    });
}

/// `task::scan_for_new_addresses` on a TRON platform: the scan starts after the known addresses, stops after
/// `gap_limit` unused addresses, reports the addresses up to the last used one in base58check with their
/// derivation paths, and records the new count.
#[test]
fn the_scan_stops_after_the_gap_and_reports_base58_addresses() {
    let ctx = hd_ctx();
    // The account of ADDRESS_2 exists; ADDRESS_0 and ADDRESS_1 do not.
    let node = scripted_node(&[ADDRESS_2], &[]);
    block_on(async {
        let coin = hd_coin(&ctx, &node.url()).await;
        enable_wallet(&coin, 0).await;
        assert_eq!(known_addresses_number(&coin).await, 1, "the enabled address is known");

        let params: ScanAddressesParams =
            serde_json::from_value(json!({ "account_index": 0, "gap_limit": 3 })).expect("scan parameters");
        let response = coin
            .init_scan_for_new_addresses_rpc(params)
            .await
            .unwrap_or_else(|e| panic!("the scan must succeed: {}", e.get_inner()));
        let found: Vec<(String, String)> = response
            .new_addresses
            .iter()
            .map(|address| (address.address.clone(), address.derivation_path.0.to_string()))
            .collect();
        assert_eq!(found, [
            (ADDRESS_1.to_owned(), "m/44'/195'/0'/0/1".to_owned()),
            (ADDRESS_2.to_owned(), "m/44'/195'/0'/0/2".to_owned()),
        ]);
        assert_eq!(response.derivation_path.0.to_string(), "m/44'/195'/0'");
        assert_eq!(known_addresses_number(&coin).await, 3);
    });
}

/// The tickers of a balance map, sorted.
fn tickers_of(balance: &crate::coin_balance::CoinBalanceMap) -> Vec<&str> {
    let mut tickers: Vec<&str> = balance.keys().map(String::as_str).collect();
    tickers.sort_unstable();
    tickers
}

/// The HD methods asked for a TRC20 ticker (the status note of 2026-11-19 in CRD ch.21 and the Reloaded note after
/// it): the platform's wallet answers, every balance map holds that token's ticker only, and the scan finds an
/// address that holds the token although the chain has not created its account. The TRX ticker keeps the map over
/// the platform and its enabled tokens.
#[test]
fn a_trc20_ticker_answers_its_own_ticker_only_and_its_scan_finds_a_token_only_address() {
    let ctx = hd_ctx();
    // The account of ADDRESS_0 exists; ADDRESS_2 holds the token only and has no account.
    let node = scripted_node(&[ADDRESS_0], &[ADDRESS_2]);
    block_on(async {
        let platform = hd_coin(&ctx, &node.url()).await;
        enable_wallet(&platform, 0).await;
        let registered = lp_register_coin(&ctx, MmCoinEnum::EthCoin(platform.clone()), RegisterCoinParams {
            ticker: "TRX".to_owned(),
            tx_history: false,
        })
        .await;
        assert!(registered.is_ok(), "the platform registers");
        let contract = parse_trc20_contract(TRC20_CONTRACT).expect("a contract");
        platform.add_erc20_token_info("USDT-TRC20".to_owned(), Erc20TokenInfo {
            token_addr: contract,
            decimals: 6,
        });
        let token = platform
            .trc20_token_from_conf_or_contract("USDT-TRC20".to_owned(), contract, 1)
            .await
            .unwrap_or_else(|e| panic!("the token builds: {}", e.get_inner()));

        // The scan asked for the token: ADDRESS_2 is found through its token balance alone.
        let params: ScanAddressesParams =
            serde_json::from_value(json!({ "account_index": 0, "gap_limit": 2 })).expect("scan parameters");
        let response = token
            .init_scan_for_new_addresses_rpc(params)
            .await
            .unwrap_or_else(|e| panic!("the scan must succeed: {}", e.get_inner()));
        let found: Vec<&str> = response
            .new_addresses
            .iter()
            .map(|address| address.address.as_str())
            .collect();
        assert_eq!(found, [ADDRESS_1, ADDRESS_2]);
        for address in &response.new_addresses {
            assert_eq!(tickers_of(&address.balance), ["USDT-TRC20"]);
        }
        assert_eq!(
            response.new_addresses[1].balance["USDT-TRC20"].spendable,
            "1.234567".parse().expect("a decimal")
        );
        assert_eq!(
            known_addresses_number(&platform).await,
            3,
            "the platform's wallet took the scan"
        );

        // `get_new_address`: the token answers its ticker only, the platform the full map.
        let next = |coin: EthCoin| async move {
            coin.get_new_address_rpc(GetNewHDAddressParams {
                account_id: 0,
                chain: Bip44Chain::External,
            })
            .await
            .unwrap_or_else(|e| panic!("get_new_address must succeed: {}", e.get_inner()))
            .new_address
        };
        let of_token = next(token.clone()).await;
        assert_eq!(of_token.address, independent_address(&ctx, "m/44'/195'/0'/0/3"));
        assert_eq!(tickers_of(&of_token.balance), ["USDT-TRC20"]);
        let of_platform = next(platform.clone()).await;
        assert_eq!(of_platform.address, independent_address(&ctx, "m/44'/195'/0'/0/4"));
        assert_eq!(tickers_of(&of_platform.balance), ["TRX", "USDT-TRC20"]);

        // `account_balance`: the same five addresses for both tickers, the maps as above.
        let balances = |coin: EthCoin| async move {
            let params: AccountBalanceParams =
                serde_json::from_value(json!({ "account_index": 0, "chain": "External" })).expect("parameters");
            coin.account_balance_rpc(params)
                .await
                .unwrap_or_else(|e| panic!("account_balance must succeed: {}", e.get_inner()))
        };
        let of_token = balances(token.clone()).await;
        assert_eq!(of_token.addresses.len(), 5);
        for address in &of_token.addresses {
            assert_eq!(tickers_of(&address.balance), ["USDT-TRC20"]);
        }
        assert_eq!(of_token.addresses[2].address, ADDRESS_2);
        assert_eq!(of_token.page_balance.spendable, "1.234567".parse().expect("a decimal"));
        let of_platform = balances(platform.clone()).await;
        assert_eq!(of_platform.addresses.len(), 5);
        for address in &of_platform.addresses {
            assert_eq!(tickers_of(&address.balance), ["TRX", "USDT-TRC20"]);
        }
        assert_eq!(of_platform.page_balance.spendable, "2.5".parse().expect("a decimal"));
    });
}

// ---------------------------------------------------------------------------------------------------------------
// CRD ch.47 `from`, ch.21 R21.10B.4: withdraw with the derived key
// ---------------------------------------------------------------------------------------------------------------

fn withdraw_request(coin: &str, from: Json) -> WithdrawRequest {
    serde_json::from_value(json!({ "coin": coin, "from": from, "to": RECEIVER, "amount": "1" }))
        .unwrap_or_else(|e| panic!("a withdraw request: {e}"))
}

/// Whether the signature of the transaction of `details` recovers to the base58check address `address`.
fn signed_by(details: &TransactionDetails, address: &str) -> bool {
    let tx = proto::Transaction::decode(details.tx_hex.0.as_slice()).expect("a protobuf transaction");
    let raw = tx.raw_data.as_ref().expect("raw data");
    let hash = hash_transaction_raw(raw);
    assert_eq!(tx.signature.len(), 1);
    let signature = Signature::from_str(&hex::encode(&tx.signature[0])).expect("a 65-byte signature");
    verify_address(&evm(address), &signature, &hash).expect("a verifiable signature")
}

async fn withdrawn(ctx: &MmArc, coin: &EthCoin, request: WithdrawRequest) -> TransactionDetails {
    match withdraw_impl(ctx.clone(), coin.clone(), request).await {
        Ok(details) => details,
        Err(e) => panic!("the withdraw must succeed: {}", e.get_inner()),
    }
}

async fn withdraw_error(ctx: &MmArc, coin: &EthCoin, request: WithdrawRequest) -> WithdrawError {
    match withdraw_impl(ctx.clone(), coin.clone(), request).await {
        Ok(_) => panic!("the withdraw must be refused"),
        Err(e) => e.into_inner(),
    }
}

/// A TRX withdraw without `from` is sent from the activated address, and with an HD `from` (the address-id form and
/// the derivation-path form) it is signed with the key of that path: the signature recovers to the `from` address.
#[test]
fn a_trx_withdraw_from_an_hd_address_signs_with_the_derived_key() {
    let ctx = hd_ctx();
    let node = scripted_node(&[ADDRESS_0, ADDRESS_1, ADDRESS_2], &[]);
    block_on(async {
        let coin = hd_coin(&ctx, &node.url()).await;
        enable_wallet(&coin, 3).await;

        // The default sender is the activated address.
        let details = withdrawn(&ctx, &coin, withdraw_request("TRX", Json::Null)).await;
        assert_eq!(details.from, [ADDRESS_0]);
        assert!(signed_by(&details, ADDRESS_0) && !signed_by(&details, ADDRESS_1));

        // An address-id `from` and a derivation-path `from`.
        for from in [
            json!({ "account_id": 0, "chain": "External", "address_id": 1 }),
            json!({ "derivation_path": "m/44'/195'/0'/0/1" }),
        ] {
            let details = withdrawn(&ctx, &coin, withdraw_request("TRX", from)).await;
            assert_eq!(details.from, [ADDRESS_1]);
            assert!(signed_by(&details, ADDRESS_1), "the derived key signs");
            assert!(!signed_by(&details, ADDRESS_0), "not the activated key");
        }
    });
}

/// The `from` rules of an EVM HD withdraw carry over: an address that is not known, an account that does not
/// exist, and a path of another coin type are refused, before any transaction is built.
#[test]
fn a_from_that_names_no_known_hd_address_is_refused() {
    let ctx = hd_ctx();
    let node = scripted_node(&[ADDRESS_0, ADDRESS_1], &[]);
    block_on(async {
        let coin = hd_coin(&ctx, &node.url()).await;
        enable_wallet(&coin, 2).await;

        let unknown_address = json!({ "account_id": 0, "chain": "External", "address_id": 5 });
        assert!(matches!(
            withdraw_error(&ctx, &coin, withdraw_request("TRX", unknown_address)).await,
            WithdrawError::UnexpectedFromAddress(_)
        ));
        let unknown_account = json!({ "account_id": 7, "chain": "External", "address_id": 0 });
        assert!(matches!(
            withdraw_error(&ctx, &coin, withdraw_request("TRX", unknown_account)).await,
            WithdrawError::UnknownAccount { account_id: 7 }
        ));
        let other_coin_type = json!({ "derivation_path": "m/44'/60'/0'/0/0" });
        assert!(matches!(
            withdraw_error(&ctx, &coin, withdraw_request("TRX", other_coin_type)).await,
            WithdrawError::UnexpectedFromAddress(_)
        ));
    });
    assert!(
        node.requests().iter().all(|(path, _)| path != "/wallet/getnowblock"),
        "no transaction was built"
    );
}

/// A TRC20 token uses the HD wallet of its platform: the withdraw from an HD `from` is signed with that key.
#[test]
fn a_trc20_withdraw_from_an_hd_address_signs_with_the_derived_key() {
    let ctx = hd_ctx();
    let node = scripted_node(&[ADDRESS_0, ADDRESS_1], &[ADDRESS_0, ADDRESS_1]);
    block_on(async {
        let platform = hd_coin(&ctx, &node.url()).await;
        enable_wallet(&platform, 2).await;
        let registered = lp_register_coin(&ctx, MmCoinEnum::EthCoin(platform.clone()), RegisterCoinParams {
            ticker: "TRX".to_owned(),
            tx_history: false,
        })
        .await;
        assert!(registered.is_ok(), "the platform registers");
        let token = platform
            .trc20_token_from_conf_or_contract(
                "USDT-TRC20".to_owned(),
                parse_trc20_contract(TRC20_CONTRACT).expect("a contract"),
                1,
            )
            .await
            .unwrap_or_else(|e| panic!("the token builds: {}", e.get_inner()));

        let default = withdrawn(&ctx, &token, withdraw_request("USDT-TRC20", Json::Null)).await;
        assert_eq!(default.from, [ADDRESS_0]);
        assert!(signed_by(&default, ADDRESS_0));

        let from = json!({ "account_id": 0, "chain": "External", "address_id": 1 });
        let details = withdrawn(&ctx, &token, withdraw_request("USDT-TRC20", from)).await;
        assert_eq!(details.coin, "USDT-TRC20");
        assert_eq!(details.from, [ADDRESS_1]);
        assert!(signed_by(&details, ADDRESS_1) && !signed_by(&details, ADDRESS_0));
    });
}

/// The base58check addresses the node was asked about at `path`, in arrival order.
fn asked_addresses(node: &StubNode, path: &str) -> Vec<String> {
    node.bodies_for(path)
        .iter()
        .map(|body| body["address"].as_str().unwrap_or_default().to_owned())
        .collect()
}

/// CRD ch.21 R21.10B.7: the fee estimate of a withdraw with an HD `from` reads the resources of that address, not
/// of the activated one, for TRX and for a token; the token's dry run and its TRX balance for the fee are read for
/// that address too.
#[test]
fn a_withdraw_from_an_hd_address_reads_the_resources_of_that_address() {
    let from = || json!({ "account_id": 0, "chain": "External", "address_id": 1 });

    let ctx = hd_ctx();
    let node = scripted_node(&[ADDRESS_0, ADDRESS_1], &[]);
    block_on(async {
        let coin = hd_coin(&ctx, &node.url()).await;
        enable_wallet(&coin, 2).await;
        let details = withdrawn(&ctx, &coin, withdraw_request("TRX", from())).await;
        assert_eq!(details.from, [ADDRESS_1]);
    });
    assert_eq!(asked_addresses(&node, "/wallet/getaccountresource"), [ADDRESS_1]);

    let ctx = hd_ctx();
    let node = scripted_node(&[ADDRESS_0, ADDRESS_1], &[ADDRESS_0, ADDRESS_1]);
    block_on(async {
        let platform = hd_coin(&ctx, &node.url()).await;
        enable_wallet(&platform, 2).await;
        let registered = lp_register_coin(&ctx, MmCoinEnum::EthCoin(platform.clone()), RegisterCoinParams {
            ticker: "TRX".to_owned(),
            tx_history: false,
        })
        .await;
        assert!(registered.is_ok(), "the platform registers");
        let token = platform
            .trc20_token_from_conf_or_contract(
                "USDT-TRC20".to_owned(),
                parse_trc20_contract(TRC20_CONTRACT).expect("a contract"),
                1,
            )
            .await
            .unwrap_or_else(|e| panic!("the token builds: {}", e.get_inner()));
        let accounts_before = asked_addresses(&node, "/wallet/getaccount").len();

        let details = withdrawn(&ctx, &token, withdraw_request("USDT-TRC20", from())).await;
        assert_eq!(details.from, [ADDRESS_1]);

        assert_eq!(asked_addresses(&node, "/wallet/getaccountresource"), [ADDRESS_1]);
        // The TRX balance that must cover the fee is the sender's.
        let accounts = asked_addresses(&node, "/wallet/getaccount");
        assert_eq!(accounts[accounts_before..], [ADDRESS_1]);
        let dry_runs: Vec<Json> = node
            .bodies_for("/wallet/triggerconstantcontract")
            .into_iter()
            .filter(|body| body["function_selector"] == "transfer(address,uint256)")
            .collect();
        assert_eq!(dry_runs.len(), 1);
        assert_eq!(dry_runs[0]["owner_address"], ADDRESS_1);
    });
}

/// A single-key TRON coin refuses `from`, as an EVM coin with a single key does (CRD ch.49 R49.27b item 1).
#[test]
fn a_single_key_tron_coin_refuses_from() {
    let ctx = MmCtxBuilder::new().into_mm_arc();
    let node = scripted_node(&[ADDRESS_0], &[]);
    block_on(async {
        let mut key = [0u8; 32];
        key[31] = 1;
        let coin = tron_coin_from_params(&ctx, "TRX", &json!({}), params(&node.url()), &key, CoinProtocol::TRX {
            network: Network::Nile,
        })
        .await
        .unwrap_or_else(|e| panic!("the coin builds: {}", e.get_inner()));
        let from = json!({ "account_id": 0, "chain": "External", "address_id": 0 });
        assert!(matches!(
            withdraw_error(&ctx, &coin, withdraw_request("TRX", from)).await,
            WithdrawError::UnexpectedFromAddress(_)
        ));
    });
}
