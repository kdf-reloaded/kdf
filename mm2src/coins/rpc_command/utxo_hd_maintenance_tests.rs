//! CRD ch.05 §5.9B R35b, T14b: the UTXO maintenance commands under a global-HD login.
//!
//! `fetch_utxos` lists every known address of every account; `consolidate_utxos` and the automatic merge loop
//! work on the enabled address only. Each test names the rule it asserts. The responses and errors are compared
//! as the JSON they put on the wire, never by message text.

use super::consolidate_utxos::{consolidate_utxos_rpc, ConsolidateUtxoError, ConsolidateUtxoRequest};
use super::fetch_utxos::{fetch_utxos_rpc, FetchUtxosError, FetchUtxosRequest};
use super::get_my_address::PathToAddress;
use crate::hd_wallet::{HDAccountsMap, HDAccountsMutex};
use crate::hd_wallet_storage::HDWalletCoinStorage;
use crate::utxo::qtum::QtumCoin;
use crate::utxo::rpc_clients::{NativeClient, NativeClientImpl, UnspentInfo, UtxoRpcClientEnum};
use crate::utxo::utxo_common::{address_from_pubkey, big_decimal_from_sat_unsigned, merge_utxo_loop};
use crate::utxo::utxo_standard::UtxoStandardCoin;
use crate::utxo::utxo_tests::{utxo_coin_fields_for_test, utxo_coin_from_fields};
use crate::utxo::{output_script, GetUtxoListOps, UtxoArc, UtxoCoinFields, UtxoHDAccount, UtxoHDWallet,
                  UtxoTxBroadcastOps};
use crate::{CoinsContext, DerivationMethod, MmCoinEnum, PrivKeyBuildPolicy, PrivKeyPolicy};
use chain::OutPoint;
use common::executor::Timer;
use common::{block_on, HttpStatusCode};
use crypto::privkey::{key_pair_from_secret, key_pair_from_seed};
use crypto::{derive_secp256k1_extended_pubkey, derive_secp256k1_secret, Bip32DerPathOps, Bip44Chain, ChildNumber,
             CryptoCtx, HDPathToAccount, HDPathToCoin};
use futures::future::{select, Either};
use http::StatusCode;
use keys::{Address, AddressFormat, KeyPair, Type as ScriptType};
use mm2_core::mm_ctx::{MmArc, MmCtxBuilder};
use mocktopus::mocking::*;
use rpc::v1::types::H256 as H256Json;
use serde_json::{self as json, json, Value as Json};
use serialization::deserialize;
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::{Arc, Mutex};

const TEST_MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
const TICKER: &str = "DOC";
const DECIMALS: u8 = 8;
const VALUE: u64 = 1_000_000;

fn path_to_address(account_id: u32, chain: Bip44Chain, address_id: u32) -> PathToAddress {
    PathToAddress {
        account_id,
        chain,
        address_id,
    }
}

fn default_path() -> PathToAddress { path_to_address(0, Bip44Chain::External, 0) }

/// The key at `m/44'/141'/<account>'/<chain>/<address_id>` of the wallet of `fields`, derived from the wallet's
/// own root key, independently of the code under test.
fn key_at(fields: &UtxoCoinFields, account_id: u32, chain: Bip44Chain, address_id: u32) -> KeyPair {
    let root = match &fields.priv_key_policy {
        PrivKeyPolicy::HDWallet {
            bip39_secp_priv_key, ..
        } => bip39_secp_priv_key.clone(),
        _ => panic!("a coin with the software HD key policy is expected"),
    };
    let hd_wallet = match &fields.derivation_method {
        DerivationMethod::HDWallet(hd_wallet) => hd_wallet,
        _ => panic!("a coin with the HD derivation method is expected"),
    };
    let mut path = hd_wallet.derivation_path.to_derivation_path();
    path.push(ChildNumber::new(account_id, true).unwrap());
    path.push(chain.to_child_number());
    path.push(ChildNumber::from(address_id));
    let secret = derive_secp256k1_secret(root, &path).unwrap();
    key_pair_from_secret(secret.as_slice()).unwrap()
}

fn address_of(fields: &UtxoCoinFields, key_pair: &KeyPair) -> Address {
    address_from_pubkey(
        key_pair.public(),
        fields.conf.pub_addr_prefix,
        fields.conf.pub_t_addr_prefix,
        fields.conf.checksum_type,
        None,
        AddressFormat::Standard,
    )
}

fn address_at(fields: &UtxoCoinFields, account_id: u32, chain: Bip44Chain, address_id: u32) -> Address {
    address_of(fields, &key_at(fields, account_id, chain, address_id))
}

/// A coin activated through the HD route under a software global-HD login at `path_to_address`. `accounts` is
/// `(account_id, known external addresses, known internal addresses)` for each account the wallet holds.
fn hd_fields(path_to_address: PathToAddress, accounts: &[(u32, u32, u32)]) -> UtxoCoinFields {
    let ctx = MmCtxBuilder::default().into_mm_arc();
    CryptoCtx::init_with_global_hd_account(ctx.clone(), TEST_MNEMONIC).unwrap();
    let global_hd = match PrivKeyBuildPolicy::detect_priv_key_policy(&ctx).unwrap() {
        PrivKeyBuildPolicy::GlobalHDAccount(global_hd) => global_hd,
        _ => panic!("expected a global-HD login"),
    };
    let root = global_hd.root_priv_key().clone();

    let mut account_map = HDAccountsMap::new();
    for &(account_id, external_addresses_number, internal_addresses_number) in accounts {
        let account_path = HDPathToAccount::from_str(&format!("m/44'/141'/{account_id}'")).unwrap();
        let extended_pubkey =
            derive_secp256k1_extended_pubkey(root.clone(), &account_path.to_derivation_path()).unwrap();
        account_map.insert(account_id, UtxoHDAccount {
            account_id,
            extended_pubkey,
            account_derivation_path: account_path,
            external_addresses_number,
            internal_addresses_number,
        });
    }
    let hd_wallet = UtxoHDWallet {
        hd_wallet_storage: HDWalletCoinStorage::default(),
        address_format: AddressFormat::Standard,
        derivation_path: HDPathToCoin::from_str("m/44'/141'").unwrap(),
        accounts: HDAccountsMutex::new(account_map),
        gap_limit: 3,
    };

    let client = NativeClient(Arc::new(NativeClientImpl::default()));
    let mut fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(client), None, false);
    fields.priv_key_policy = PrivKeyPolicy::HDWallet {
        // Replaced below by the key at `path_to_address`, which needs the wallet of `fields`.
        activated_key: key_pair_from_seed("the placeholder of the test").unwrap(),
        bip39_secp_priv_key: root,
        path_to_address: path_to_address.clone(),
    };
    fields.derivation_method = DerivationMethod::HDWallet(hd_wallet);
    let activated = key_at(
        &fields,
        path_to_address.account_id,
        path_to_address.chain,
        path_to_address.address_id,
    );
    if let PrivKeyPolicy::HDWallet { activated_key, .. } = &mut fields.priv_key_policy {
        *activated_key = activated;
    }
    fields
}

/// The same coin under the Trezor policy. No test below sets up a device: the code under test must not ask for one.
fn trezor_fields(path_to_address: PathToAddress, accounts: &[(u32, u32, u32)]) -> UtxoCoinFields {
    let mut fields = hd_fields(path_to_address, accounts);
    fields.priv_key_policy = PrivKeyPolicy::Trezor;
    fields
}

fn unspent(hash: u8, index: u32) -> UnspentInfo {
    UnspentInfo {
        outpoint: OutPoint {
            hash: hash.into(),
            index,
        },
        value: VALUE,
        height: Default::default(),
    }
}

/// Mocks the unspent-output listing: each address answers its own list, any other address answers none. Returns
/// the addresses the code under test asked for, in order.
fn mock_unspents(by_address: Vec<(Address, Vec<UnspentInfo>)>) -> Arc<Mutex<Vec<String>>> {
    let requested = Arc::new(Mutex::new(Vec::new()));
    let seen = requested.clone();
    let table: HashMap<String, Vec<UnspentInfo>> = by_address
        .into_iter()
        .map(|(address, unspents)| (format!("{address}"), unspents))
        .collect();
    UtxoStandardCoin::get_unspent_ordered_list.mock_safe(move |coin, address| {
        let key = format!("{address}");
        seen.lock().unwrap().push(key.clone());
        let unspents = table.get(&key).cloned().unwrap_or_default();
        // The mock runs inside the runtime's `block_on`, so the uncontended lock is taken with another executor.
        let cache = futures::executor::block_on(coin.as_ref().recently_spent_outpoints.lock());
        MockResult::Return(Box::pin(futures::future::ok((unspents, cache))))
    });
    requested
}

fn ctx_with(coin: impl Into<MmCoinEnum>) -> MmArc {
    let ctx = MmCtxBuilder::default().into_mm_arc();
    let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();
    block_on(coins_ctx.add_coin(coin.into())).unwrap();
    ctx
}

fn fetch(ctx: MmArc, coin: &str) -> Result<Json, FetchUtxosError> {
    block_on(fetch_utxos_rpc(ctx, FetchUtxosRequest { coin: coin.to_owned() }))
        .map(|response| json::to_value(&response).unwrap())
        .map_err(|e| e.into_inner())
}

fn consolidate(
    ctx: MmArc,
    coin: &str,
    merge_at: usize,
    max_merge_at_once: usize,
    broadcast: bool,
) -> Result<Json, ConsolidateUtxoError> {
    let request: ConsolidateUtxoRequest = json::from_value(json!({
        "coin": coin,
        "merge_conditions": { "merge_at": merge_at, "max_merge_at_once": max_merge_at_once },
        "broadcast": broadcast,
    }))
    .unwrap();
    block_on(consolidate_utxos_rpc(ctx, request))
        .map(|response| json::to_value(&response).unwrap())
        .map_err(|e| e.into_inner())
}

fn value_json(sat: u64) -> Json { json::to_value(big_decimal_from_sat_unsigned(sat, DECIMALS)).unwrap() }

/// The transaction of a `consolidate_utxos` response.
fn built_tx(response: &Json) -> chain::Transaction {
    let tx_hex = response["tx"]["tx_hex"].as_str().expect("tx_hex is a string");
    deserialize(hex::decode(tx_hex).unwrap().as_slice()).unwrap()
}

fn spent_outpoints(tx: &chain::Transaction) -> HashSet<OutPoint> {
    tx.inputs.iter().map(|input| input.previous_output.clone()).collect()
}

/// R35b item 3: the transaction pays one output to `address`, and every input is signed with `key_pair`.
fn assert_paid_to_and_signed_by(tx: &chain::Transaction, address: &Address, key_pair: &KeyPair) {
    assert_eq!(tx.outputs.len(), 1);
    assert_eq!(
        tx.outputs[0].script_pubkey,
        output_script(address, ScriptType::P2PKH).to_bytes()
    );
    let pubkey = key_pair.public().to_vec();
    assert!(!tx.inputs.is_empty());
    for input in tx.inputs.iter() {
        assert!(
            input
                .script_sig
                .windows(pubkey.len())
                .any(|window| window == pubkey.as_slice()),
            "an input is not signed with the key of the enabled address"
        );
    }
}

fn assert_error_type(error: &impl serde::Serialize, expected: &str) {
    assert_eq!(json::to_value(error).unwrap()["error_type"], expected);
}

// ------------------------------------------------------------------------------------------------------------
// fetch_utxos
// ------------------------------------------------------------------------------------------------------------

/// T14b, R35b `fetch_utxos`: under an HD login every known address of every account, on both chains, is read and
/// listed with its derivation path; `total_count` sums the listed outputs; an address without outputs is not
/// listed; only known addresses are read.
#[test]
fn t14b_fetch_utxos_lists_every_known_address_of_every_account_on_both_chains() {
    // Account 0: external addresses 0 to 2 are known, internal address 0 is known. Account 1: external 0.
    let fields = hd_fields(default_path(), &[(0, 3, 1), (1, 1, 0)]);
    let a0_ext0 = address_at(&fields, 0, Bip44Chain::External, 0);
    let a0_ext1 = address_at(&fields, 0, Bip44Chain::External, 1);
    let a0_ext2 = address_at(&fields, 0, Bip44Chain::External, 2);
    let a0_int0 = address_at(&fields, 0, Bip44Chain::Internal, 0);
    let a1_ext0 = address_at(&fields, 1, Bip44Chain::External, 0);
    // A known address of account 0 nobody has a listing for, and an unknown one that holds outputs.
    let a0_ext3_unknown = address_at(&fields, 0, Bip44Chain::External, 3);

    let requested = mock_unspents(vec![
        (a0_ext0.clone(), vec![unspent(1, 0), unspent(1, 1)]),
        (a0_ext1.clone(), vec![unspent(2, 0)]),
        (a0_int0.clone(), vec![unspent(3, 7)]),
        (a1_ext0.clone(), vec![unspent(4, 0)]),
        (a0_ext3_unknown.clone(), vec![unspent(5, 0)]),
    ]);
    let ctx = ctx_with(utxo_coin_from_fields(fields));

    let response = fetch(ctx, TICKER).unwrap_or_else(|e| panic!("{}", json::to_string(&e).unwrap()));

    assert_eq!(response["total_count"], 5);
    let addresses = response["addresses"].as_array().unwrap();
    let listed: Vec<(String, u64, String)> = addresses
        .iter()
        .map(|a| {
            (
                a["address"].as_str().unwrap().to_owned(),
                a["count"].as_u64().unwrap(),
                a["derivation_path"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(listed, vec![
        (format!("{a0_ext0}"), 2, "m/44'/141'/0'/0/0".to_owned()),
        (format!("{a0_ext1}"), 1, "m/44'/141'/0'/0/1".to_owned()),
        (format!("{a0_int0}"), 1, "m/44'/141'/0'/1/0".to_owned()),
        (format!("{a1_ext0}"), 1, "m/44'/141'/1'/0/0".to_owned()),
    ]);
    // The outputs of an address: `txid`, `vout` and `value` members.
    let first = &addresses[0]["utxos"];
    assert_eq!(first.as_array().unwrap().len(), 2);
    assert_eq!(first[1]["vout"], 1);
    assert_eq!(first[1]["value"], value_json(VALUE));
    assert!(first[0]["txid"].is_string());
    assert_eq!(addresses[2]["utxos"][0]["vout"], 7);

    // Only the five known addresses were read, none beyond them.
    let expected: Vec<String> = [&a0_ext0, &a0_ext1, &a0_ext2, &a0_int0, &a1_ext0]
        .iter()
        .map(|a| format!("{a}"))
        .collect();
    let mut read = requested.lock().unwrap().clone();
    read.sort();
    let mut expected_sorted = expected;
    expected_sorted.sort();
    assert_eq!(read, expected_sorted);
}

/// R35b `fetch_utxos`: the known set is not changed by reading it (nothing is derived beyond it or persisted).
#[test]
fn r35b_fetch_utxos_does_not_change_the_known_addresses() {
    let fields = hd_fields(default_path(), &[(0, 2, 0)]);
    let _requested = mock_unspents(vec![]);
    let coin = utxo_coin_from_fields(fields);
    let ctx = ctx_with(coin.clone());

    let response = fetch(ctx, TICKER).unwrap_or_else(|e| panic!("{}", json::to_string(&e).unwrap()));

    assert_eq!(response["total_count"], 0);
    assert_eq!(response["addresses"], json!([]));
    match &coin.as_ref().derivation_method {
        DerivationMethod::HDWallet(hd_wallet) => {
            let accounts = block_on(hd_wallet.accounts.lock());
            assert_eq!(accounts[&0].external_addresses_number, 2);
            assert_eq!(accounts[&0].internal_addresses_number, 0);
            assert_eq!(accounts.len(), 1);
        },
        _ => panic!("an HD coin is expected"),
    }
}

/// R35b `fetch_utxos`: under a single-address login the one address is listed, without a `derivation_path`
/// member (unchanged).
#[test]
fn r35b_fetch_utxos_iguana_is_unchanged() {
    let client = NativeClient(Arc::new(NativeClientImpl::default()));
    let fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(client), None, false);
    let my_address = match &fields.derivation_method {
        DerivationMethod::Iguana(address) => address.clone(),
        _ => panic!("an iguana coin is expected"),
    };
    let requested = mock_unspents(vec![(my_address.clone(), vec![unspent(1, 0), unspent(1, 1)])]);
    let ctx = ctx_with(utxo_coin_from_fields(fields));

    let response = fetch(ctx, TICKER).unwrap_or_else(|e| panic!("{}", json::to_string(&e).unwrap()));

    assert_eq!(response["total_count"], 2);
    let addresses = response["addresses"].as_array().unwrap();
    assert_eq!(addresses.len(), 1);
    assert_eq!(addresses[0]["address"], format!("{my_address}"));
    assert_eq!(addresses[0]["count"], 2);
    assert!(addresses[0].get("derivation_path").is_none());
    assert_eq!(*requested.lock().unwrap(), vec![format!("{my_address}")]);
}

/// T14b, R35b: a QTUM coin, and a coin that is not active, answer as the error arms say.
#[test]
fn t14b_fetch_utxos_answers_coin_not_supported_and_no_such_coin() {
    let client = NativeClient(Arc::new(NativeClientImpl::default()));
    let fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(client), None, false);
    let ctx = ctx_with(QtumCoin::from(UtxoArc::new(fields)));

    let error = fetch(ctx.clone(), TICKER).unwrap_err();
    assert!(matches!(error, FetchUtxosError::CoinNotSupported));
    assert_eq!(error.status_code(), StatusCode::BAD_REQUEST);
    assert_error_type(&error, "CoinNotSupported");

    let error = fetch(ctx, "NOSUCHCOIN").unwrap_err();
    assert!(matches!(error, FetchUtxosError::NoSuchCoin));
    assert_eq!(error.status_code(), StatusCode::NOT_FOUND);
}

/// R35b `fetch_utxos` under the Trezor policy: the known addresses are read from the wallet's account public
/// keys, with no device and no signing.
#[test]
fn r35b_fetch_utxos_under_the_trezor_policy_reads_without_a_device() {
    // Under the Trezor policy the coin holds no key to derive from; the addresses are the ones of the account
    // public key, which the software coin of the same wallet derives as well.
    let reference = hd_fields(default_path(), &[(0, 2, 0)]);
    let a0 = address_at(&reference, 0, Bip44Chain::External, 0);
    let fields = trezor_fields(default_path(), &[(0, 2, 0)]);
    let requested = mock_unspents(vec![(a0.clone(), vec![unspent(1, 0)])]);
    let ctx = ctx_with(utxo_coin_from_fields(fields));

    let response = fetch(ctx, TICKER).unwrap_or_else(|e| panic!("{}", json::to_string(&e).unwrap()));

    assert_eq!(response["total_count"], 1);
    assert_eq!(response["addresses"][0]["address"], format!("{a0}"));
    assert_eq!(requested.lock().unwrap().len(), 2);
}

// ------------------------------------------------------------------------------------------------------------
// consolidate_utxos
// ------------------------------------------------------------------------------------------------------------

/// T14b, R35b `consolidate_utxos`: under an HD login with two known addresses that hold outputs, `merge_at: 2`
/// and `broadcast: false` spend only the enabled address's outputs, to that same address, signed with the key at
/// the enabled address's path; the other known address is neither read nor spent; the transaction is returned
/// unsent.
#[test]
fn t14b_consolidate_utxos_merges_the_enabled_address_only() {
    let fields = hd_fields(default_path(), &[(0, 2, 0)]);
    let enabled = address_at(&fields, 0, Bip44Chain::External, 0);
    let other = address_at(&fields, 0, Bip44Chain::External, 1);
    let enabled_key = key_at(&fields, 0, Bip44Chain::External, 0);
    let requested = mock_unspents(vec![
        (enabled.clone(), vec![unspent(1, 0), unspent(1, 1), unspent(1, 2)]),
        (other.clone(), vec![unspent(2, 0), unspent(2, 1)]),
    ]);
    // A broadcast would reach this mock; the transaction must not.
    let sent = Arc::new(Mutex::new(0usize));
    let sent_counter = sent.clone();
    UtxoStandardCoin::broadcast_tx.mock_safe(move |_coin, _tx| {
        *sent_counter.lock().unwrap() += 1;
        MockResult::Return(Box::pin(futures::future::ok(H256Json::default())))
    });
    let ctx = ctx_with(utxo_coin_from_fields(fields));

    let response =
        consolidate(ctx, TICKER, 2, 50, false).unwrap_or_else(|e| panic!("{}", json::to_string(&e).unwrap()));

    assert_eq!(response["tx"]["from"], json!([format!("{enabled}")]));
    assert_eq!(response["tx"]["to"], json!([format!("{enabled}")]));
    let consolidated = response["consolidated_utxos"].as_array().unwrap();
    assert_eq!(consolidated.len(), 3);
    assert!(consolidated.iter().all(|u| u["value"] == value_json(VALUE)));

    let tx = built_tx(&response);
    let expected: HashSet<OutPoint> = (0..3)
        .map(|index| OutPoint {
            hash: 1u8.into(),
            index,
        })
        .collect();
    assert_eq!(spent_outpoints(&tx), expected);
    assert_paid_to_and_signed_by(&tx, &enabled, &enabled_key);

    assert_eq!(*requested.lock().unwrap(), vec![format!("{enabled}")]);
    assert!(!requested.lock().unwrap().contains(&format!("{other}")));
    assert_eq!(
        *sent.lock().unwrap(),
        0,
        "broadcast: false must not send the transaction"
    );
}

/// R35b `consolidate_utxos`, non-default path: the enabled address at `path_to_address` account 0, External, 1
/// is the one consolidated, signed with the key at that path; address 0 is untouched.
#[test]
fn r35b_consolidate_utxos_uses_the_enabled_address_at_a_non_default_path() {
    let fields = hd_fields(path_to_address(0, Bip44Chain::External, 1), &[(0, 2, 0)]);
    let address_0 = address_at(&fields, 0, Bip44Chain::External, 0);
    let enabled = address_at(&fields, 0, Bip44Chain::External, 1);
    let enabled_key = key_at(&fields, 0, Bip44Chain::External, 1);
    let requested = mock_unspents(vec![
        (address_0.clone(), vec![unspent(1, 0), unspent(1, 1)]),
        (enabled.clone(), vec![unspent(2, 0), unspent(2, 1)]),
    ]);
    let ctx = ctx_with(utxo_coin_from_fields(fields));

    let response =
        consolidate(ctx, TICKER, 2, 50, false).unwrap_or_else(|e| panic!("{}", json::to_string(&e).unwrap()));

    assert_eq!(response["tx"]["from"], json!([format!("{enabled}")]));
    assert_eq!(response["tx"]["to"], json!([format!("{enabled}")]));
    let tx = built_tx(&response);
    let expected: HashSet<OutPoint> = (0..2)
        .map(|index| OutPoint {
            hash: 2u8.into(),
            index,
        })
        .collect();
    assert_eq!(spent_outpoints(&tx), expected);
    assert_paid_to_and_signed_by(&tx, &enabled, &enabled_key);
    assert_eq!(*requested.lock().unwrap(), vec![format!("{enabled}")]);
}

/// R35b `consolidate_utxos` with `broadcast: true` under HD: the transaction is sent once, from the enabled
/// address's outputs, signed with the key at its path.
#[test]
fn r35b_consolidate_utxos_broadcast_sends_the_signed_merge_under_hd() {
    let fields = hd_fields(path_to_address(0, Bip44Chain::External, 1), &[(0, 2, 0)]);
    let enabled = address_at(&fields, 0, Bip44Chain::External, 1);
    let enabled_key = key_at(&fields, 0, Bip44Chain::External, 1);
    let _requested = mock_unspents(vec![(enabled.clone(), vec![unspent(2, 0), unspent(2, 1)])]);
    let sent: Arc<Mutex<Vec<chain::Transaction>>> = Arc::new(Mutex::new(Vec::new()));
    let sent_txs = sent.clone();
    UtxoStandardCoin::broadcast_tx.mock_safe(move |_coin, tx| {
        sent_txs.lock().unwrap().push(tx.clone());
        MockResult::Return(Box::pin(futures::future::ok(H256Json::default())))
    });
    let ctx = ctx_with(utxo_coin_from_fields(fields));

    let response = consolidate(ctx, TICKER, 2, 50, true).unwrap_or_else(|e| panic!("{}", json::to_string(&e).unwrap()));

    assert_eq!(response["tx"]["from"], json!([format!("{enabled}")]));
    let sent = sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_paid_to_and_signed_by(&sent[0], &enabled, &enabled_key);
    assert_eq!(spent_outpoints(&sent[0]).len(), 2);
}

/// T14b, R35b `consolidate_utxos`: fewer outputs than `merge_at`, and fewer than two once the
/// `max_merge_at_once` cut is applied, answer `BadMergeConditions` 400.
#[test]
fn t14b_consolidate_utxos_answers_bad_merge_conditions() {
    let fields = hd_fields(default_path(), &[(0, 2, 0)]);
    let enabled = address_at(&fields, 0, Bip44Chain::External, 0);
    let _requested = mock_unspents(vec![(enabled, vec![unspent(1, 0), unspent(1, 1)])]);
    let ctx = ctx_with(utxo_coin_from_fields(fields));

    let error = consolidate(ctx.clone(), TICKER, 3, 50, false).unwrap_err();
    assert!(matches!(error, ConsolidateUtxoError::BadMergeConditions(_)));
    assert_eq!(error.status_code(), StatusCode::BAD_REQUEST);
    assert_error_type(&error, "BadMergeConditions");

    let error = consolidate(ctx, TICKER, 2, 1, false).unwrap_err();
    assert!(matches!(error, ConsolidateUtxoError::BadMergeConditions(_)));
    assert_eq!(error.status_code(), StatusCode::BAD_REQUEST);
}

/// T14b, R35b: a QTUM coin answers `CoinNotSupported` 400; a coin that is not active answers `NoSuchCoin` 404.
#[test]
fn t14b_consolidate_utxos_answers_coin_not_supported_and_no_such_coin() {
    let client = NativeClient(Arc::new(NativeClientImpl::default()));
    let fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(client), None, false);
    let ctx = ctx_with(QtumCoin::from(UtxoArc::new(fields)));

    let error = consolidate(ctx.clone(), TICKER, 2, 50, false).unwrap_err();
    assert!(matches!(error, ConsolidateUtxoError::CoinNotSupported));
    assert_eq!(error.status_code(), StatusCode::BAD_REQUEST);
    assert_error_type(&error, "CoinNotSupported");

    let error = consolidate(ctx, "NOSUCHCOIN", 2, 50, false).unwrap_err();
    assert!(matches!(error, ConsolidateUtxoError::NoSuchCoin));
    assert_eq!(error.status_code(), StatusCode::NOT_FOUND);
}

/// R35b `consolidate_utxos` under the Trezor policy: the command is refused as an enabled address that cannot be
/// used, before any output is read or any signing is attempted. No device is asked.
#[test]
fn r35b_consolidate_utxos_is_refused_under_the_trezor_policy() {
    let fields = trezor_fields(default_path(), &[(0, 2, 0)]);
    let requested = mock_unspents(vec![]);
    let ctx = ctx_with(utxo_coin_from_fields(fields));

    for broadcast in [false, true] {
        let error = consolidate(ctx.clone(), TICKER, 2, 50, broadcast).unwrap_err();
        assert!(matches!(error, ConsolidateUtxoError::InvalidAddress(_)));
        assert_eq!(error.status_code(), StatusCode::BAD_REQUEST);
    }
    assert!(requested.lock().unwrap().is_empty());
}

/// R35b `consolidate_utxos`, single-address login: unchanged. The one address is merged into itself.
#[test]
fn r35b_consolidate_utxos_iguana_is_unchanged() {
    let client = NativeClient(Arc::new(NativeClientImpl::default()));
    let fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(client), None, false);
    let my_address = match &fields.derivation_method {
        DerivationMethod::Iguana(address) => address.clone(),
        _ => panic!("an iguana coin is expected"),
    };
    let my_key_pair = match &fields.priv_key_policy {
        PrivKeyPolicy::KeyPair(key_pair) => *key_pair,
        _ => panic!("a single-key coin is expected"),
    };
    let requested = mock_unspents(vec![(my_address.clone(), vec![unspent(1, 0), unspent(1, 1)])]);
    let ctx = ctx_with(utxo_coin_from_fields(fields));

    let response =
        consolidate(ctx, TICKER, 2, 50, false).unwrap_or_else(|e| panic!("{}", json::to_string(&e).unwrap()));

    assert_eq!(response["tx"]["from"], json!([format!("{my_address}")]));
    assert_eq!(response["tx"]["to"], json!([format!("{my_address}")]));
    assert_eq!(response["consolidated_utxos"].as_array().unwrap().len(), 2);
    assert_paid_to_and_signed_by(&built_tx(&response), &my_address, &my_key_pair);
    assert_eq!(*requested.lock().unwrap(), vec![format!("{my_address}")]);
}

// ------------------------------------------------------------------------------------------------------------
// The automatic merge loop
// ------------------------------------------------------------------------------------------------------------

/// R35b, the automatic merge loop: under an HD login with a non-default `path_to_address` the loop reads the
/// enabled address, and no other one. Nothing is merged here (the listing answers no outputs), and the loop ends
/// when the coin is dropped.
#[test]
fn r35b_merge_loop_reads_the_enabled_address_under_hd() {
    let fields = hd_fields(path_to_address(0, Bip44Chain::External, 1), &[(0, 3, 0)]);
    let enabled = address_at(&fields, 0, Bip44Chain::External, 1);
    let requested = mock_unspents(vec![]);
    let arc = UtxoArc::new(fields);
    let weak = arc.downgrade();

    let seen = requested.clone();
    let stopper = async move {
        while seen.lock().unwrap().len() < 2 {
            Timer::sleep(0.01).await;
        }
        drop(arc);
    };
    let looped = merge_utxo_loop(weak, 2, 0.01, 10, UtxoStandardCoin::from);
    // Guarded like the Trezor test: a lingering strong reference must fail the test, not hang it.
    let both = futures::future::join(looped, stopper);
    match block_on(select(Box::pin(both), Box::pin(Timer::sleep(30.)))) {
        Either::Left(_) => (),
        Either::Right(_) => panic!("the merge loop did not end after the coin was dropped"),
    }

    let read = requested.lock().unwrap();
    assert!(read.len() >= 2);
    assert!(read.iter().all(|address| *address == format!("{enabled}")));
}

/// R35b, the automatic merge loop, merge branch: under an HD login with a non-default `path_to_address` and
/// `merge_at` outputs on the enabled address, one round sends exactly one transaction. It spends the enabled
/// address's outputs only (two other known addresses hold outputs too, which stay untouched), pays one output back
/// to the enabled address and is signed with the key at the enabled address's path. The loop is stopped after
/// that first round, by dropping the coin.
#[test]
fn r35b_merge_loop_merges_the_enabled_address_under_hd() {
    let fields = hd_fields(path_to_address(0, Bip44Chain::External, 1), &[(0, 3, 0)]);
    let address_0 = address_at(&fields, 0, Bip44Chain::External, 0);
    let enabled = address_at(&fields, 0, Bip44Chain::External, 1);
    let address_2 = address_at(&fields, 0, Bip44Chain::External, 2);
    let enabled_key = key_at(&fields, 0, Bip44Chain::External, 1);
    let requested = mock_unspents(vec![
        (address_0, vec![unspent(1, 0), unspent(1, 1), unspent(1, 2)]),
        (enabled.clone(), vec![unspent(2, 0), unspent(2, 1), unspent(2, 2)]),
        (address_2, vec![unspent(3, 0), unspent(3, 1), unspent(3, 2)]),
    ]);
    let sent: Arc<Mutex<Vec<chain::Transaction>>> = Arc::new(Mutex::new(Vec::new()));
    let sent_txs = sent.clone();
    UtxoStandardCoin::broadcast_tx.mock_safe(move |_coin, tx| {
        sent_txs.lock().unwrap().push(tx.clone());
        MockResult::Return(Box::pin(futures::future::ok(H256Json::default())))
    });
    let arc = UtxoArc::new(fields);
    let weak = arc.downgrade();

    // The coin is dropped as soon as the first transaction is sent. The pause between the rounds is far longer
    // than the stopper's poll, so the loop wakes to a dropped coin and ends without a second round.
    let seen = sent.clone();
    let stopper = async move {
        while seen.lock().unwrap().is_empty() {
            Timer::sleep(0.01).await;
        }
        drop(arc);
    };
    let looped = merge_utxo_loop(weak, 3, 0.2, 10, UtxoStandardCoin::from);
    // Guarded like the other loop tests: a lingering strong reference must fail the test, not hang it.
    let both = futures::future::join(looped, stopper);
    match block_on(select(Box::pin(both), Box::pin(Timer::sleep(30.)))) {
        Either::Left(_) => (),
        Either::Right(_) => panic!("the merge loop did not end after the coin was dropped"),
    }

    let sent = sent.lock().unwrap();
    assert_eq!(sent.len(), 1, "one round sends exactly one merge transaction");
    let expected: HashSet<OutPoint> = (0..3)
        .map(|index| OutPoint {
            hash: 2u8.into(),
            index,
        })
        .collect();
    assert_eq!(spent_outpoints(&sent[0]), expected);
    assert_paid_to_and_signed_by(&sent[0], &enabled, &enabled_key);
    // Only the enabled address was read.
    let read = requested.lock().unwrap();
    assert!(!read.is_empty());
    assert!(read.iter().all(|address| *address == format!("{enabled}")));
}

/// R35b, the automatic merge loop, under the Trezor policy: the enabled address cannot be used, so the loop ends
/// by itself with a logged error, reads nothing, and no merge or signing runs.
#[test]
fn r35b_merge_loop_ends_under_the_trezor_policy() {
    let fields = trezor_fields(default_path(), &[(0, 2, 0)]);
    let requested = mock_unspents(vec![]);
    let arc = UtxoArc::new(fields);
    let looped = merge_utxo_loop(arc.downgrade(), 2, 0.01, 10, UtxoStandardCoin::from);

    match block_on(select(Box::pin(looped), Box::pin(Timer::sleep(10.)))) {
        Either::Left(_) => (),
        Either::Right(_) => panic!("the merge loop did not end by itself"),
    }
    assert!(requested.lock().unwrap().is_empty());
    drop(arc);
}
