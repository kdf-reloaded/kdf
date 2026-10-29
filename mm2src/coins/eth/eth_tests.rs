use super::*;
use crate::hd_wallet::HDAccountsMutex;
use crate::hd_wallet_storage::HDWalletCoinStorage;
use common::block_on;
use crypto::{Bip32DerPathOps, Bip44Chain, Bip44PathToAccount, Bip44PathToCoin, ChildNumber, CryptoCtx, DerivationPath,
             HDPathToCoin, KeyPairPolicy};
use mm2_core::mm_ctx::{MmArc, MmCtxBuilder};
use mocktopus::mocking::*;
use std::collections::BTreeMap;

/// Test-only DEX-fee destination pubkey, resolved through `mm2_net_config`
/// for the community netid. Replaces direct use of
/// `common::DEX_FEE_ADDR_RAW_PUBKEY` so test fixtures go through the same
/// per-netid registry as production code (LP-3F.A1).
fn test_dex_fee_addr_raw_pubkey() -> &'static [u8] {
    mm2_net_config::net_config_or_panic(8762).dex_fee_addr_raw_pubkey()
}

/// The gas price for the tests
const GAS_PRICE: u64 = 50_000_000_000;
// `GAS_PRICE` increased by 3%
const GAS_PRICE_APPROXIMATION_ON_START_SWAP: u64 = 51_500_000_000;
// `GAS_PRICE` increased by 5%
const GAS_PRICE_APPROXIMATION_ON_ORDER_ISSUE: u64 = 52_500_000_000;
// `GAS_PRICE` increased by 7%
const GAS_PRICE_APPROXIMATION_ON_TRADE_PREIMAGE: u64 = 53_500_000_000;

fn check_sum(addr: &str, expected: &str) {
    let actual = checksum_address(addr);
    assert_eq!(expected, actual);
}

pub(super) fn eth_coin_for_test(
    coin_type: EthCoinType,
    urls: Vec<String>,
    fallback_swap_contract: Option<Address>,
) -> (MmArc, EthCoin) {
    // Reloaded has no default operating network: a node only runs on a compiled
    // netid (see docs/GLEEC_COMPATIBILITY.md). Tests therefore pick a supported
    // netid explicitly instead of relying on the inherited default of 0.
    eth_coin_for_test_with_netid(
        coin_type,
        urls,
        fallback_swap_contract,
        mm2_net_config::SUPPORTED_NETIDS[0],
    )
}

fn eth_coin_for_test_with_netid(
    coin_type: EthCoinType,
    urls: Vec<String>,
    fallback_swap_contract: Option<Address>,
    netid: u16,
) -> (MmArc, EthCoin) {
    eth_coin_for_test_with_netid_and_decimals(coin_type, urls, fallback_swap_contract, netid, 18)
}

fn eth_coin_for_test_with_netid_and_decimals(
    coin_type: EthCoinType,
    urls: Vec<String>,
    fallback_swap_contract: Option<Address>,
    netid: u16,
    decimals: u8,
) -> (MmArc, EthCoin) {
    let web3 = crate::eth::alloy_compat::build_provider(crate::node_url::listed_nodes("TEST", urls), vec![]).unwrap();
    let web3_instances = vec![Web3Instance {
        web3: web3.clone(),
        is_parity: true,
    }];
    eth_coin_for_test_with_providers(coin_type, web3, web3_instances, fallback_swap_contract, netid, decimals)
}

/// The coin of the tests above over ready-made providers: `web3` is the failover provider every request goes
/// through, `web3_instances` are the single-node providers of nonce gathering and the signed broadcast.
pub(super) fn eth_coin_for_test_with_providers(
    coin_type: EthCoinType,
    web3: crate::eth::alloy_compat::KdfProvider,
    web3_instances: Vec<Web3Instance>,
    fallback_swap_contract: Option<Address>,
    netid: u16,
    decimals: u8,
) -> (MmArc, EthCoin) {
    let key_pair = KeyPair::from_secret_slice(
        &hex::decode("809465b17d0a4ddb3e4c69e8f23c2cabad868f51f8bed5c765ad1d6516c3306f").unwrap(),
    )
    .unwrap();
    let conf = json!({
        "netid": netid,
        "coins":[
           {"coin":"ETH","name":"ethereum","protocol":{"type":"ETH"},"rpcport":80,"mm2":1},
           {"coin":"JST","name":"jst","rpcport":80,"mm2":1,"protocol":{"type":"ERC20","protocol_data":{"platform":"ETH","contract_address":"0x2b294F029Fde858b2c62184e8390591755521d8E"}}}
        ]
    });
    let ctx = MmCtxBuilder::new().with_conf(conf.clone()).into_mm_arc();
    let ticker = match coin_type {
        EthCoinType::Eth => "ETH".to_string(),
        EthCoinType::Erc20 { .. } => "JST".to_string(),
        EthCoinType::Tron | EthCoinType::Trc20 { .. } => panic!("TRON not used in this test helper"),
    };

    let my_addr = key_pair.address();
    let eth_coin = EthCoin(Arc::new(EthCoinImpl {
        coin_type,
        decimals,
        gas_station_url: None,
        gas_station_decimals: ETH_GAS_STATION_DECIMALS,
        history_sync_state: Mutex::new(HistorySyncState::NotEnabled),
        gas_station_policy: GasStationPricePolicy::MeanAverageFast,
        my_address: my_addr,
        sign_message_prefix: Some(String::from("Ethereum Signed Message:\n")),
        signer: EthSigner::Local(key_pair),
        swap_contract_address: Address::from_slice(&hex::decode("7Bc1bBDD6A0a722fC9bffC49c921B685ECB84b94").unwrap()),
        fallback_swap_contract,
        ticker,
        web3_instances,
        web3,
        ctx: ctx.weak(),
        required_confirmations: 1.into(),
        tron_api: None,
        nft_swap_v2_contract: None,
        swap_gas_fee_policy: Mutex::new(SwapGasFeePolicy::default()),
        erc20_tokens_infos: Default::default(),
        custom_token: false,
        chain_id: None,
        logs_block_range: DEFAULT_LOGS_BLOCK_RANGE,
        derivation_method: DerivationMethod::Iguana(my_addr),
        swap_v2_contracts: None,
        gas_limit: EthGasLimit::default(),
        gas_limit_v2: EthGasLimitV2::default(),
        estimate_gas_mult: None,
    }));
    (ctx, eth_coin)
}

fn eth_hd_coin_for_test(coin_type: EthCoinType) -> (MmArc, EthCoin, Address) {
    const TEST_MNEMONIC: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    let conf = json!({
        "enable_hd": true,
        "netid": mm2_net_config::SUPPORTED_NETIDS[0],
        "coins":[
           {"coin":"ETH","name":"ethereum","protocol":{"type":"ETH"},"rpcport":80,"mm2":1},
           {"coin":"JST","name":"jst","rpcport":80,"mm2":1,"protocol":{"type":"ERC20","protocol_data":{"platform":"ETH","contract_address":"0x2b294F029Fde858b2c62184e8390591755521d8E"}}}
        ]
    });
    let ctx = MmCtxBuilder::new().with_conf(conf).into_mm_arc();
    let crypto_ctx = CryptoCtx::init_with_global_hd_account(ctx.clone(), TEST_MNEMONIC).unwrap();
    let global_hd = match crypto_ctx.key_pair_policy() {
        KeyPairPolicy::GlobalHDAccount(global_hd) => global_hd.clone(),
        KeyPairPolicy::Iguana => panic!("expected global-HD test context"),
    };

    let derivation_path = Bip44PathToCoin::from_str("m/44'/60'").unwrap();
    let account_derivation_path: Bip44PathToAccount =
        derivation_path.derive(ChildNumber::new(0, true).unwrap()).unwrap();
    let extended_pubkey = global_hd
        .derive_account_extended_pubkey(&account_derivation_path.to_derivation_path())
        .unwrap();
    let account = eth_hd_wallet::EthHDAccount {
        account_id: 0,
        extended_pubkey,
        account_derivation_path,
        external_addresses_number: 2,
    };
    let mut accounts = BTreeMap::new();
    accounts.insert(0, account);

    let mut activated_path = DerivationPath::from_str("m/44'/60'/0'").unwrap();
    activated_path.push(Bip44Chain::External.to_child_number());
    activated_path.push(ChildNumber::from(0));
    let activated_secret = global_hd.derive_secp256k1_secret(&activated_path).unwrap();
    let activated_key = KeyPair::from_secret_slice(activated_secret.as_slice()).unwrap();
    let my_addr = activated_key.address();

    let mut selected_path = DerivationPath::from_str("m/44'/60'/0'").unwrap();
    selected_path.push(Bip44Chain::External.to_child_number());
    selected_path.push(ChildNumber::from(1));
    let selected_secret = global_hd.derive_secp256k1_secret(&selected_path).unwrap();
    let selected_addr = KeyPair::from_secret_slice(selected_secret.as_slice())
        .unwrap()
        .address();

    let web3 = crate::eth::alloy_compat::build_provider(
        crate::node_url::listed_nodes("TEST", vec!["http://dummy.dummy".into()]),
        vec![],
    )
    .unwrap();
    let ticker = match coin_type {
        EthCoinType::Eth => "ETH".to_string(),
        EthCoinType::Erc20 { .. } => "JST".to_string(),
        EthCoinType::Tron | EthCoinType::Trc20 { .. } => panic!("TRON not used in this test helper"),
    };
    let eth_coin = EthCoin(Arc::new(EthCoinImpl {
        coin_type,
        decimals: 18,
        gas_station_url: None,
        gas_station_decimals: ETH_GAS_STATION_DECIMALS,
        history_sync_state: Mutex::new(HistorySyncState::NotEnabled),
        gas_station_policy: GasStationPricePolicy::MeanAverageFast,
        my_address: my_addr,
        sign_message_prefix: Some(String::from("Ethereum Signed Message:\n")),
        signer: EthSigner::Local(activated_key),
        swap_contract_address: Address::from_slice(&hex::decode("7Bc1bBDD6A0a722fC9bffC49c921B685ECB84b94").unwrap()),
        fallback_swap_contract: None,
        ticker,
        web3_instances: vec![Web3Instance {
            web3: web3.clone(),
            is_parity: true,
        }],
        web3,
        ctx: ctx.weak(),
        required_confirmations: 1.into(),
        tron_api: None,
        nft_swap_v2_contract: None,
        swap_gas_fee_policy: Mutex::new(SwapGasFeePolicy::default()),
        erc20_tokens_infos: Default::default(),
        custom_token: false,
        chain_id: None,
        logs_block_range: DEFAULT_LOGS_BLOCK_RANGE,
        derivation_method: DerivationMethod::HDWallet(EthHDWallet {
            hd_wallet_storage: HDWalletCoinStorage::default(),
            derivation_path,
            accounts: HDAccountsMutex::new(accounts),
            gap_limit: 20,
        }),
        swap_v2_contracts: None,
        gas_limit: EthGasLimit::default(),
        gas_limit_v2: EthGasLimitV2::default(),
        estimate_gas_mult: None,
    }));
    (ctx, eth_coin, selected_addr)
}

#[test]
/// https://github.com/ethereum/EIPs/blob/master/EIPS/eip-55.md#test-cases
fn test_check_sum_address() {
    check_sum(
        "0xfb6916095ca1df60bb79ce92ce3ea74c37c5d359",
        "0xfB6916095ca1df60bB79Ce92cE3Ea74c37c5d359",
    );
    check_sum(
        "0x52908400098527886e0f7030069857d2e4169ee7",
        "0x52908400098527886E0F7030069857D2E4169EE7",
    );
    check_sum(
        "0x8617e340b3d01fa5f11f306f4090fd50e238070d",
        "0x8617E340B3D01FA5F11F306F4090FD50E238070D",
    );
    check_sum(
        "0xde709f2102306220921060314715629080e2fb77",
        "0xde709f2102306220921060314715629080e2fb77",
    );
    check_sum(
        "0x27b1fdb04752bbc536007a920d24acb045561c26",
        "0x27b1fdb04752bbc536007a920d24acb045561c26",
    );
    check_sum(
        "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed",
        "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed",
    );
    check_sum(
        "0xfB6916095ca1df60bB79Ce92cE3Ea74c37c5d359",
        "0xfB6916095ca1df60bB79Ce92cE3Ea74c37c5d359",
    );
    check_sum(
        "0xdbF03B407c01E7cD3CBea99509d93f8DDDC8C6FB",
        "0xdbF03B407c01E7cD3CBea99509d93f8DDDC8C6FB",
    );
    check_sum(
        "0xD1220A0cf47c7B9Be7A2E6BA89F429762e7b9aDb",
        "0xD1220A0cf47c7B9Be7A2E6BA89F429762e7b9aDb",
    );
}

#[test]
fn test_is_valid_checksum_addr() {
    assert!(is_valid_checksum_addr("0xfB6916095ca1df60bB79Ce92cE3Ea74c37c5d359"));
    assert!(is_valid_checksum_addr("0x52908400098527886E0F7030069857D2E4169EE7"));
    assert!(!is_valid_checksum_addr("0x8617e340B3D01FA5F11F306F4090FD50E238070D"));
    assert!(!is_valid_checksum_addr("0xd1220A0cf47c7B9Be7A2E6BA89F429762e7b9aDb"));
}

#[test]
fn display_u256_with_point() {
    let number = U256::from_dec_str("1000000000000000000").unwrap();
    let string = display_u256_with_decimal_point(number, 18);
    assert_eq!("1.", string);

    let number = U256::from_dec_str("10000000000000000000000000000000000000000000000000000000000").unwrap();
    let string = display_u256_with_decimal_point(number, 18);
    assert_eq!("10000000000000000000000000000000000000000.", string);

    let number = U256::from_dec_str("1234567890000000000").unwrap();
    let string = display_u256_with_decimal_point(number, 18);
    assert_eq!("1.23456789", string);

    let number = U256::from_dec_str("1234567890000000000").unwrap();
    let string = display_u256_with_decimal_point(number, 16);
    assert_eq!("123.456789", string);

    let number = U256::from_dec_str("1234567890000000000").unwrap();
    let string = display_u256_with_decimal_point(number, 0);
    assert_eq!("1234567890000000000.", string);

    let number = U256::from_dec_str("1000000000000000").unwrap();
    let string = display_u256_with_decimal_point(number, 18);
    assert_eq!("0.001", string);

    let number = U256::from_dec_str("0").unwrap();
    let string = display_u256_with_decimal_point(number, 18);
    assert_eq!("0.", string);

    let number = U256::from_dec_str("0").unwrap();
    let string = display_u256_with_decimal_point(number, 0);
    assert_eq!("0.", string);
}

#[test]
fn test_wei_from_big_decimal() {
    let amount = "0.000001".parse().unwrap();
    let wei = wei_from_big_decimal(&amount, 18).unwrap();
    let expected_wei: U256 = 1000000000000u64.into();
    assert_eq!(expected_wei, wei);

    let amount = "1.000001".parse().unwrap();
    let wei = wei_from_big_decimal(&amount, 18).unwrap();
    let expected_wei: U256 = 1000001000000000000u64.into();
    assert_eq!(expected_wei, wei);

    let amount = 1.into();
    let wei = wei_from_big_decimal(&amount, 18).unwrap();
    let expected_wei: U256 = 1000000000000000000u64.into();
    assert_eq!(expected_wei, wei);

    let amount = "0.000000000000000001".parse().unwrap();
    let wei = wei_from_big_decimal(&amount, 18).unwrap();
    let expected_wei: U256 = 1u64.into();
    assert_eq!(expected_wei, wei);

    let amount = 1234.into();
    let wei = wei_from_big_decimal(&amount, 9).unwrap();
    let expected_wei: U256 = 1234000000000u64.into();
    assert_eq!(expected_wei, wei);

    let amount = 1234.into();
    let wei = wei_from_big_decimal(&amount, 0).unwrap();
    let expected_wei: U256 = 1234u64.into();
    assert_eq!(expected_wei, wei);

    let amount = 1234.into();
    let wei = wei_from_big_decimal(&amount, 1).unwrap();
    let expected_wei: U256 = 12340u64.into();
    assert_eq!(expected_wei, wei);

    let amount = "1234.12345".parse().unwrap();
    let wei = wei_from_big_decimal(&amount, 1).unwrap();
    let expected_wei: U256 = 12341u64.into();
    assert_eq!(expected_wei, wei);
}

#[test]
fn test_wait_for_payment_spend_timeout() {
    EthCoinImpl::spend_events.mock_safe(|_, _, _, _| MockResult::Return(Box::new(futures01::future::ok(vec![]))));
    EthCoin::current_block.mock_safe(|_| MockResult::Return(Box::new(futures01::future::ok(900))));

    let key_pair = KeyPair::from_secret_slice(
        &hex::decode("809465b17d0a4ddb3e4c69e8f23c2cabad868f51f8bed5c765ad1d6516c3306f").unwrap(),
    )
    .unwrap();
    let web3 = crate::eth::alloy_compat::build_provider(
        crate::node_url::listed_nodes("TEST", vec!["http://195.201.0.6:8555".into()]),
        vec![],
    )
    .unwrap();
    let ctx = MmCtxBuilder::new().into_mm_arc();

    let my_addr = key_pair.address();
    let coin = EthCoinImpl {
        coin_type: EthCoinType::Eth,
        decimals: 18,
        gas_station_url: None,
        gas_station_decimals: ETH_GAS_STATION_DECIMALS,
        gas_station_policy: GasStationPricePolicy::MeanAverageFast,
        history_sync_state: Mutex::new(HistorySyncState::NotEnabled),
        my_address: my_addr,
        sign_message_prefix: Some(String::from("Ethereum Signed Message:\n")),
        signer: EthSigner::Local(key_pair),
        swap_contract_address: Address::from_slice(&hex::decode("7Bc1bBDD6A0a722fC9bffC49c921B685ECB84b94").unwrap()),
        fallback_swap_contract: None,
        ticker: "ETH".into(),
        web3_instances: vec![Web3Instance {
            web3: web3.clone(),
            is_parity: true,
        }],
        web3,
        ctx: ctx.weak(),
        required_confirmations: 1.into(),
        tron_api: None,
        nft_swap_v2_contract: None,
        swap_gas_fee_policy: Mutex::new(SwapGasFeePolicy::default()),
        erc20_tokens_infos: Default::default(),
        custom_token: false,
        chain_id: None,
        logs_block_range: DEFAULT_LOGS_BLOCK_RANGE,
        derivation_method: DerivationMethod::Iguana(my_addr),
        swap_v2_contracts: None,
        gas_limit: EthGasLimit::default(),
        gas_limit_v2: EthGasLimitV2::default(),
        estimate_gas_mult: None,
    };

    let coin = EthCoin(Arc::new(coin));
    let wait_until = (now_ms() / 1000) - 1;
    let from_block = 1;
    // raw transaction bytes of https://etherscan.io/tx/0x0869be3e5d4456a29d488a533ad6c118620fef450f36778aecf31d356ff8b41f
    let tx_bytes = [
        248, 240, 3, 133, 1, 42, 5, 242, 0, 131, 2, 73, 240, 148, 133, 0, 175, 192, 188, 82, 20, 114, 128, 130, 22, 51,
        38, 194, 255, 12, 115, 244, 168, 113, 135, 110, 205, 245, 24, 127, 34, 254, 184, 132, 21, 44, 243, 175, 73, 33,
        143, 82, 117, 16, 110, 27, 133, 82, 200, 114, 233, 42, 140, 198, 35, 21, 201, 249, 187, 180, 20, 46, 148, 40,
        9, 228, 193, 130, 71, 199, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 152, 41, 132, 9, 201, 73, 19, 94, 237, 137, 35,
        61, 4, 194, 207, 239, 152, 75, 175, 245, 157, 174, 10, 214, 161, 207, 67, 70, 87, 246, 231, 212, 47, 216, 119,
        68, 237, 197, 125, 141, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 93, 72, 125, 102, 28, 159, 180, 237, 198, 97, 87, 80, 82, 200, 104, 40, 245,
        221, 7, 28, 122, 104, 91, 99, 1, 159, 140, 25, 131, 101, 74, 87, 50, 168, 146, 187, 90, 160, 51, 1, 123, 247,
        6, 108, 165, 181, 188, 40, 56, 47, 211, 229, 221, 73, 5, 15, 89, 81, 117, 225, 216, 108, 98, 226, 119, 232, 94,
        184, 42, 106,
    ];

    assert!(coin
        .wait_for_tx_spend(&tx_bytes, wait_until, from_block, &coin.swap_contract_address())
        .wait()
        .is_err());
}

#[test]
fn test_gas_station() {
    make_gas_station_request.mock_safe(|_| {
        let data = GasStationData {
            average: 500.into(),
            fast: 1000.into(),
        };
        MockResult::Return(Box::pin(async move { Ok(data) }))
    });
    let res_eth = GasStationData::get_gas_price(
        "https://ethgasstation.info/api/ethgasAPI.json",
        8,
        GasStationPricePolicy::MeanAverageFast,
    )
    .wait()
    .unwrap();
    let one_gwei = U256::from(10u64.pow(9));

    let expected_eth_wei = U256::from(75) * one_gwei;
    assert_eq!(expected_eth_wei, res_eth);

    let res_polygon = GasStationData::get_gas_price(
        "https://gasstation-mainnet.matic.network/",
        9,
        GasStationPricePolicy::Average,
    )
    .wait()
    .unwrap();

    let expected_eth_polygon = U256::from(500) * one_gwei;
    assert_eq!(expected_eth_polygon, res_polygon);
}

/// `search_for_swap_tx_spend` decides, after a restart, whether the counterparty took the payment or
/// the payer must refund it, so both outcomes are pinned on an EVM coin.
///
/// The two tests that used to live here (`test_search_for_swap_tx_spend_was_spent` and
/// `..._was_refunded`) queried a public Ropsten endpoint for frozen historical transactions. That
/// chain no longer exists and the endpoint is gone, so they could never pass again, and a keyless
/// public endpoint does not serve historical `eth_getLogs` ranges either. They now run the same
/// real function, over the same recorded raw transactions, against a scripted local node. Do not
/// point them back at a live network.
///
/// The function reads exactly three node methods, all scripted below: `eth_blockNumber`,
/// `eth_getLogs` (once per window, first for `ReceiverSpent`, then for `SenderRefunded`) and
/// `eth_getTransactionByHash`.
mod search_for_swap_tx_spend_scripted {
    use super::scripted_evm_node::spawn;
    use super::*;
    use serde_json::{json, Value as Json};

    const SWAP_CONTRACT_ADDR: &str = "7Bc1bBDD6A0a722fC9bffC49c921B685ECB84b94";
    const CURRENT_BLOCK: u64 = 6_000_000;

    // Recorded Ropsten transactions (the chain is gone; only the bytes are kept).
    const SPENT_PAYMENT_TX: &[u8] = &[
        248, 240, 52, 132, 119, 53, 148, 0, 131, 2, 73, 240, 148, 123, 193, 187, 221, 106, 10, 114, 47, 201, 191, 252,
        73, 201, 33, 182, 133, 236, 184, 75, 148, 135, 71, 13, 228, 223, 130, 0, 0, 184, 132, 21, 44, 243, 175, 188,
        96, 248, 252, 165, 132, 81, 30, 243, 34, 85, 165, 46, 224, 176, 90, 137, 30, 19, 123, 224, 67, 83, 53, 74, 57,
        148, 140, 95, 45, 70, 147, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 117, 244, 28, 175, 51, 95, 91, 184, 141, 201,
        45, 116, 26, 102, 210, 119, 151, 124, 143, 52, 215, 128, 89, 116, 30, 25, 35, 128, 122, 186, 177, 228, 149,
        250, 55, 53, 62, 196, 51, 72, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 93, 56, 62, 80, 28, 160, 65, 22, 195, 212, 184, 202, 226, 151, 224, 111,
        174, 31, 160, 219, 39, 69, 137, 37, 8, 127, 177, 4, 104, 248, 27, 41, 245, 176, 131, 188, 215, 136, 160, 91,
        134, 199, 67, 1, 58, 57, 103, 23, 215, 176, 64, 124, 1, 44, 88, 161, 200, 160, 64, 110, 13, 145, 127, 180, 27,
        171, 131, 253, 90, 48, 147,
    ];
    const SPEND_TX: &[u8] = &[
        249, 1, 9, 37, 132, 119, 53, 148, 0, 131, 2, 73, 240, 148, 123, 193, 187, 221, 106, 10, 114, 47, 201, 191, 252,
        73, 201, 33, 182, 133, 236, 184, 75, 148, 128, 184, 164, 2, 237, 41, 43, 188, 96, 248, 252, 165, 132, 81, 30,
        243, 34, 85, 165, 46, 224, 176, 90, 137, 30, 19, 123, 224, 67, 83, 53, 74, 57, 148, 140, 95, 45, 70, 147, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 71, 13, 228, 223, 130, 0, 0, 168, 151, 11,
        232, 224, 253, 63, 180, 26, 114, 23, 184, 27, 10, 161, 80, 178, 251, 73, 204, 80, 174, 97, 118, 149, 204, 186,
        187, 243, 185, 19, 128, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 157, 73, 251, 238, 138, 245, 142, 240, 85, 44, 209, 63, 194, 242,
        109, 242, 246, 6, 76, 176, 27, 160, 29, 157, 226, 23, 81, 174, 34, 82, 93, 182, 41, 248, 119, 42, 221, 214, 38,
        243, 128, 2, 235, 208, 193, 192, 74, 208, 242, 26, 221, 83, 54, 74, 160, 111, 29, 92, 8, 75, 61, 97, 103, 199,
        100, 189, 72, 74, 221, 144, 66, 170, 68, 121, 29, 105, 19, 194, 35, 245, 196, 131, 236, 29, 105, 101, 30,
    ];
    const REFUNDED_PAYMENT_TX: &[u8] = &[
        249, 1, 43, 130, 10, 96, 132, 149, 2, 249, 0, 131, 2, 73, 240, 148, 123, 193, 187, 221, 106, 10, 114, 47, 201,
        191, 252, 73, 201, 33, 182, 133, 236, 184, 75, 148, 128, 184, 196, 155, 65, 91, 42, 192, 158, 192, 175, 210,
        198, 159, 244, 116, 46, 255, 28, 236, 147, 240, 68, 91, 16, 19, 6, 59, 187, 149, 138, 179, 151, 121, 47, 14,
        80, 251, 147, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3, 141, 126, 164, 198,
        128, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 192, 235, 122, 237, 116, 14, 23, 150, 153, 42, 8, 150, 44, 21, 102,
        27, 222, 181, 128, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 216, 153, 121, 65, 221, 19, 70, 233, 35, 17, 24, 213,
        104, 93, 134, 98, 148, 245, 158, 91, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 93,
        23, 98, 207, 27, 160, 4, 198, 61, 242, 141, 248, 157, 72, 229, 2, 162, 163, 250, 159, 26, 66, 37, 42, 159, 35,
        58, 94, 57, 121, 252, 166, 34, 25, 206, 193, 113, 198, 160, 68, 125, 142, 153, 210, 177, 60, 173, 67, 127, 138,
        52, 112, 9, 49, 108, 109, 44, 177, 142, 9, 124, 10, 200, 37, 100, 52, 137, 196, 74, 67, 192,
    ];
    const REFUND_TX: &[u8] = &[
        249, 1, 11, 130, 10, 97, 132, 149, 2, 249, 0, 131, 2, 73, 240, 148, 123, 193, 187, 221, 106, 10, 114, 47, 201,
        191, 252, 73, 201, 33, 182, 133, 236, 184, 75, 148, 128, 184, 164, 70, 252, 2, 148, 192, 158, 192, 175, 210,
        198, 159, 244, 116, 46, 255, 28, 236, 147, 240, 68, 91, 16, 19, 6, 59, 187, 149, 138, 179, 151, 121, 47, 14,
        80, 251, 147, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3, 141, 126, 164, 198,
        128, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 192, 235, 122, 237, 116, 14, 23, 150, 153, 42, 8, 150, 44, 21, 102, 27, 222, 181,
        128, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 216, 153, 121, 65, 221, 19, 70, 233, 35, 17, 24, 213, 104, 93, 134,
        98, 148, 245, 158, 91, 28, 160, 127, 220, 190, 77, 221, 188, 140, 162, 198, 6, 127, 102, 222, 66, 38, 96, 10,
        19, 27, 208, 119, 219, 60, 231, 2, 118, 91, 169, 99, 78, 209, 135, 160, 51, 115, 90, 189, 124, 172, 205, 134,
        203, 159, 238, 40, 39, 99, 88, 48, 160, 189, 37, 60, 20, 117, 65, 238, 36, 98, 226, 48, 22, 235, 86, 183,
    ];

    fn coin(url: &str, coin_type: EthCoinType) -> (MmArc, EthCoin) {
        let key_pair = KeyPair::from_secret_slice(
            &hex::decode("809465b17d0a4ddb3e4c69e8f23c2cabad868f51f8bed5c765ad1d6516c3306f").unwrap(),
        )
        .unwrap();
        let web3 = crate::eth::alloy_compat::build_provider(
            crate::node_url::listed_nodes("TEST", vec![url.to_owned()]),
            vec![],
        )
        .unwrap();
        let ctx = MmCtxBuilder::new().into_mm_arc();
        let swap_contract_address = Address::from_str(SWAP_CONTRACT_ADDR).unwrap();
        let my_addr = key_pair.address();
        let coin = EthCoin(Arc::new(EthCoinImpl {
            coin_type,
            decimals: 18,
            gas_station_url: None,
            gas_station_decimals: ETH_GAS_STATION_DECIMALS,
            gas_station_policy: GasStationPricePolicy::MeanAverageFast,
            history_sync_state: Mutex::new(HistorySyncState::NotEnabled),
            my_address: my_addr,
            sign_message_prefix: Some(String::from("Ethereum Signed Message:\n")),
            signer: EthSigner::Local(key_pair),
            swap_contract_address,
            fallback_swap_contract: None,
            ticker: "ETH".into(),
            web3_instances: vec![Web3Instance {
                web3: web3.clone(),
                is_parity: true,
            }],
            web3,
            ctx: ctx.weak(),
            required_confirmations: 1.into(),
            tron_api: None,
            nft_swap_v2_contract: None,
            swap_gas_fee_policy: Mutex::new(SwapGasFeePolicy::default()),
            erc20_tokens_infos: Default::default(),
            custom_token: false,
            chain_id: None,
            logs_block_range: DEFAULT_LOGS_BLOCK_RANGE,
            derivation_method: DerivationMethod::Iguana(my_addr),
            swap_v2_contracts: None,
            gas_limit: EthGasLimit::default(),
            gas_limit_v2: EthGasLimitV2::default(),
            estimate_gas_mult: None,
        }));
        (ctx, coin)
    }

    /// The `eth_getTransactionByHash` result a node reports for a legacy transaction.
    fn rpc_tx(tx: &SignedEthTx) -> Json {
        let to = match &tx.action {
            Action::Call(addr) => json!(format!("{:#x}", addr)),
            Action::Create => Json::Null,
        };
        let gas_price = tx.gas_price().expect("a legacy transaction");
        json!({
            "type": "0x0",
            "hash": format!("{:#x}", tx.tx_hash()),
            "nonce": format!("{:#x}", tx.nonce),
            "blockHash": format!("0x{}", "22".repeat(32)),
            "blockNumber": "0x5b8d80",
            "transactionIndex": "0x0",
            "from": format!("{:#x}", tx.sender()),
            "to": to,
            "value": format!("{:#x}", tx.value),
            "gas": format!("{:#x}", tx.gas),
            "gasPrice": format!("{:#x}", gas_price),
            "input": format!("0x{}", hex::encode(&tx.data)),
            "v": format!("{:#x}", tx.v),
            "r": format!("{:#x}", tx.r),
            "s": format!("{:#x}", tx.s),
        })
    }

    /// Runs the search with the node reporting `event` ("ReceiverSpent" or "SenderRefunded") for the
    /// payment's id in `found_tx`, and nothing for the other event.
    fn search(
        coin_type: EthCoinType,
        payment: &[u8],
        found_event: &'static str,
        found_tx: &[u8],
    ) -> Result<Option<FoundSwapTxSpend>, String> {
        let payment_tx = signed_eth_tx_from_bytes(payment).unwrap();
        let func = match coin_type {
            EthCoinType::Erc20 { .. } => "erc20Payment",
            _ => "ethPayment",
        };
        let decoded = SWAP_CONTRACT
            .function(func)
            .unwrap()
            .decode_input(&payment_tx.data[4..])
            .unwrap();
        let Token::FixedBytes(id) = decoded[0].clone() else {
            panic!("the payment id is bytes32")
        };
        let found = signed_eth_tx_from_bytes(found_tx).unwrap();
        let found_hash = found.tx_hash();
        let found_json = rpc_tx(&found);
        let event_topic = |name: &str| format!("{:#x}", SWAP_CONTRACT.event(name).unwrap().signature());
        let wanted_topic = event_topic(found_event);

        let node = spawn(move |method, params| match method {
            "eth_blockNumber" => json!(format!("{:#x}", CURRENT_BLOCK)),
            "eth_getLogs" => {
                // Only the event the test scripts has a log; the other query finds nothing.
                let asked = params[0]["topics"][0].to_string();
                if asked.contains(&wanted_topic) {
                    json!([{
                        "address": format!("0x{}", SWAP_CONTRACT_ADDR.to_lowercase()),
                        "topics": [wanted_topic],
                        "data": format!("0x{}", hex::encode(&id)),
                        "blockHash": format!("0x{}", "22".repeat(32)),
                        "blockNumber": "0x5b8d80",
                        "transactionHash": format!("{:#x}", found_hash),
                        "transactionIndex": "0x0",
                        "logIndex": "0x0",
                        "removed": false,
                    }])
                } else {
                    json!([])
                }
            },
            "eth_getTransactionByHash" => {
                assert_eq!(params[0].as_str().unwrap(), format!("{:#x}", found_hash));
                found_json.clone()
            },
            other => panic!("the scripted node does not serve `{}`", other),
        });
        let (_ctx, coin) = coin(&node.url, coin_type);
        block_on(coin.search_for_swap_tx_spend(
            payment,
            Address::from_str(SWAP_CONTRACT_ADDR).unwrap(),
            CURRENT_BLOCK - 100,
        ))
    }

    #[test]
    fn search_finds_the_spend_of_an_eth_payment() {
        let expected = FoundSwapTxSpend::Spent(signed_eth_tx_from_bytes(SPEND_TX).unwrap().into());
        let found = search(EthCoinType::Eth, SPENT_PAYMENT_TX, "ReceiverSpent", SPEND_TX)
            .unwrap()
            .unwrap();
        assert_eq!(expected, found);
    }

    #[test]
    fn search_finds_the_refund_of_an_erc20_payment() {
        let coin_type = EthCoinType::Erc20 {
            platform: "ETH".to_string(),
            token_addr: Address::from_slice(&hex::decode("c0eb7aed740e1796992a08962c15661bdeb58003").unwrap()),
        };
        let expected = FoundSwapTxSpend::Refunded(signed_eth_tx_from_bytes(REFUND_TX).unwrap().into());
        let found = search(coin_type, REFUNDED_PAYMENT_TX, "SenderRefunded", REFUND_TX)
            .unwrap()
            .unwrap();
        assert_eq!(expected, found);
    }
}

#[test]
fn test_withdraw_impl_manual_fee() {
    let (ctx, coin) = eth_coin_for_test(EthCoinType::Eth, vec!["http://dummy.dummy".into()], None);

    evm_withdraw_balance.mock_safe(|_, _| {
        let balance = wei_from_big_decimal(&1000000000.into(), 18).unwrap();
        MockResult::Return(Box::new(futures01::future::ok(balance)))
    });
    get_addr_nonce.mock_safe(|_, _| MockResult::Return(Box::new(futures01::future::ok(0.into()))));

    let withdraw_req = WithdrawRequest {
        amount: 1.into(),
        from: None,
        to: "0x7Bc1bBDD6A0a722fC9bffC49c921B685ECB84b94".to_string(),
        coin: "ETH".to_string(),
        max: false,
        fee: Some(WithdrawFee::EthGas {
            gas: 150000,
            gas_price: 1.into(),
        }),
    };
    let tx_details = block_on(withdraw_impl(ctx, coin.clone(), withdraw_req)).unwrap();
    let expected = Some(
        EthTxFeeDetails {
            coin: "ETH".into(),
            gas_price: "0.000000001".parse().unwrap(),
            gas: 150000,
            total_fee: "0.00015".parse().unwrap(),
        }
        .into(),
    );
    assert_eq!(expected, tx_details.fee_details);
}

#[test]
fn test_withdraw_impl_fee_details() {
    let (ctx, coin) = eth_coin_for_test(
        EthCoinType::Erc20 {
            platform: "ETH".to_string(),
            token_addr: Address::from_slice(&hex::decode("2b294F029Fde858b2c62184e8390591755521d8E").unwrap()),
        },
        vec!["http://dummy.dummy".into()],
        None,
    );

    evm_withdraw_balance.mock_safe(|_, _| {
        let balance = wei_from_big_decimal(&1000000000.into(), 18).unwrap();
        MockResult::Return(Box::new(futures01::future::ok(balance)))
    });
    get_addr_nonce.mock_safe(|_, _| MockResult::Return(Box::new(futures01::future::ok(0.into()))));

    let withdraw_req = WithdrawRequest {
        amount: 1.into(),
        from: None,
        to: "0x7Bc1bBDD6A0a722fC9bffC49c921B685ECB84b94".to_string(),
        coin: "ETH".to_string(),
        max: false,
        fee: Some(WithdrawFee::EthGas {
            gas: 150000,
            gas_price: 1.into(),
        }),
    };
    let tx_details = block_on(withdraw_impl(ctx, coin.clone(), withdraw_req)).unwrap();
    let expected = Some(
        EthTxFeeDetails {
            coin: "ETH".into(),
            gas_price: "0.000000001".parse().unwrap(),
            gas: 150000,
            total_fee: "0.00015".parse().unwrap(),
        }
        .into(),
    );
    assert_eq!(expected, tx_details.fee_details);
}

#[test]
fn task_withdraw_evm_native_accepts_omitted_from_sender() {
    let (_ctx, coin) = eth_coin_for_test(EthCoinType::Eth, vec!["http://dummy.dummy".into()], None);
    let withdraw_req = WithdrawRequest {
        amount: 1.into(),
        from: None,
        to: "0x7Bc1bBDD6A0a722fC9bffC49c921B685ECB84b94".to_string(),
        coin: "JST".to_string(),
        max: false,
        fee: Some(WithdrawFee::EthGas {
            gas: 150000,
            gas_price: 1.into(),
        }),
    };

    validate_evm_withdraw_request(&coin, &withdraw_req).unwrap();
}

#[test]
fn task_withdraw_evm_token_accepts_omitted_from_sender() {
    let (_ctx, coin) = eth_coin_for_test(
        EthCoinType::Erc20 {
            platform: "ETH".to_string(),
            token_addr: Address::from_slice(&hex::decode("2b294F029Fde858b2c62184e8390591755521d8E").unwrap()),
        },
        vec!["http://dummy.dummy".into()],
        None,
    );
    let withdraw_req = WithdrawRequest {
        amount: 1.into(),
        from: None,
        to: "0x7Bc1bBDD6A0a722fC9bffC49c921B685ECB84b94".to_string(),
        coin: "JST".to_string(),
        max: false,
        fee: Some(WithdrawFee::EthGas {
            gas: 150000,
            gas_price: 1.into(),
        }),
    };

    validate_evm_withdraw_request(&coin, &withdraw_req).unwrap();
}

#[test]
fn task_withdraw_evm_token_explicit_hd_from_resolves_selected_sender() {
    let (ctx, coin, selected_addr) = eth_hd_coin_for_test(EthCoinType::Erc20 {
        platform: "ETH".to_string(),
        token_addr: Address::from_slice(&hex::decode("2b294F029Fde858b2c62184e8390591755521d8E").unwrap()),
    });
    let withdraw_req = WithdrawRequest {
        amount: 1.into(),
        from: Some(crate::WithdrawFrom::AddressId(crate::HDAddressId {
            account_id: 0,
            chain: Bip44Chain::External,
            address_id: 1,
        })),
        to: "0x7Bc1bBDD6A0a722fC9bffC49c921B685ECB84b94".to_string(),
        coin: "ETH".to_string(),
        max: false,
        fee: Some(WithdrawFee::EthGas {
            gas: 150000,
            gas_price: 1.into(),
        }),
    };

    validate_evm_withdraw_request(&coin, &withdraw_req).unwrap();
    let sender = block_on(resolve_evm_withdraw_sender(&ctx, &coin, &withdraw_req)).unwrap();

    assert_eq!(sender.address(), selected_addr);
}

#[test]
fn direct_withdraw_evm_explicit_hd_from_signs_with_selected_sender() {
    let (ctx, coin, selected_addr) = eth_hd_coin_for_test(EthCoinType::Eth);
    let withdraw_req = WithdrawRequest {
        amount: 1.into(),
        from: Some(crate::WithdrawFrom::AddressId(crate::HDAddressId {
            account_id: 0,
            chain: Bip44Chain::External,
            address_id: 1,
        })),
        to: "0x7Bc1bBDD6A0a722fC9bffC49c921B685ECB84b94".to_string(),
        coin: "ETH".to_string(),
        max: false,
        fee: Some(WithdrawFee::EthGas {
            gas: 150000,
            gas_price: 1.into(),
        }),
    };

    evm_withdraw_balance.mock_safe(move |_, sender| {
        assert_eq!(sender, selected_addr);
        let balance = wei_from_big_decimal(&1000000000.into(), 18).unwrap();
        MockResult::Return(Box::new(futures01::future::ok(balance)))
    });
    get_addr_nonce.mock_safe(move |addr, _| {
        assert_eq!(addr, selected_addr);
        MockResult::Return(Box::new(futures01::future::ok(0.into())))
    });

    let tx_details = block_on(withdraw_impl(ctx, coin, withdraw_req)).unwrap();
    let signed = signed_eth_tx_from_bytes(&tx_details.tx_hex.0).unwrap();

    assert_eq!(tx_details.from, vec![checksum_address(&format!(
        "{:#02x}",
        selected_addr
    ))]);
    assert_eq!(signed.sender(), selected_addr);
}

#[test]
fn direct_withdraw_evm_single_key_explicit_from_is_structured_sender_error() {
    let (ctx, coin) = eth_coin_for_test(EthCoinType::Eth, vec!["http://dummy.dummy".into()], None);
    let withdraw_req = WithdrawRequest {
        amount: 1.into(),
        from: Some(crate::WithdrawFrom::AddressId(crate::HDAddressId {
            account_id: 0,
            chain: Bip44Chain::External,
            address_id: 1,
        })),
        to: "0x7Bc1bBDD6A0a722fC9bffC49c921B685ECB84b94".to_string(),
        coin: "ETH".to_string(),
        max: false,
        fee: Some(WithdrawFee::EthGas {
            gas: 150000,
            gas_price: 1.into(),
        }),
    };

    let err = block_on(withdraw_impl(ctx, coin, withdraw_req))
        .unwrap_err()
        .into_inner();

    assert!(matches!(err, WithdrawError::UnexpectedFromAddress(_)));
}

#[test]
fn test_increase_by_percent_no_gwei_floor() {
    // Sub-gigawei base: purely proportional, no +1 gwei jump (CRD ch.57 R57.3.2).
    let num = wei_from_big_decimal(&"0.033".parse().unwrap(), 9).unwrap();
    let expected = wei_from_big_decimal(&"0.03729".parse().unwrap(), 9).unwrap();
    assert_eq!(expected, increase_by_percent(num, 13));

    let num = wei_from_big_decimal(&"0.005".parse().unwrap(), 9).unwrap();
    let expected = wei_from_big_decimal(&"0.00565".parse().unwrap(), 9).unwrap();
    assert_eq!(expected, increase_by_percent(num, 13));

    // At and above the old floor threshold the result is unchanged.
    let num = wei_from_big_decimal(&"100".parse().unwrap(), 9).unwrap();
    let expected = wei_from_big_decimal(&"113".parse().unwrap(), 9).unwrap();
    assert_eq!(expected, increase_by_percent(num, 13));

    let num = wei_from_big_decimal(&"30.1".parse().unwrap(), 9).unwrap();
    let expected = wei_from_big_decimal(&"34.013".parse().unwrap(), 9).unwrap();
    assert_eq!(expected, increase_by_percent(num, 13));

    // Zero and sub-100-wei bases get a zero increment.
    assert_eq!(U256::zero(), increase_by_percent(U256::zero(), 13));
    assert_eq!(U256::from(99), increase_by_percent(U256::from(99), 13));
    assert_eq!(U256::from(113), increase_by_percent(U256::from(100), 13));
}

#[test]
fn test_trade_preimage_stage_is_single_proportional_increase() {
    let num = wei_from_big_decimal(&"0.033".parse().unwrap(), 9).unwrap();
    // 33_000_000 wei at 7%: 330_000 * 7 = 2_310_000, no gigawei added.
    assert_eq!(
        num + U256::from(2_310_000u64),
        increase_gas_price_by_stage(num, &FeeApproxStage::TradePreimage)
    );
    assert_eq!(num, increase_gas_price_by_stage(num, &FeeApproxStage::WithoutApprox));
}

#[test]
fn get_sender_trade_preimage() {
    /// Trade fee for the ETH coin is `(eth_payment + eth_sender_refund) * gas_price` always: 65_000 + 100_000
    /// (CRD R57.5.2; the baseline constant was 300_000).
    fn expected_fee(gas_price: u64) -> TradeFee {
        let amount = u256_to_big_decimal(((65_000 + 100_000) * gas_price).into(), 18).expect("!u256_to_big_decimal");
        TradeFee {
            coin: "ETH".to_owned(),
            amount: amount.into(),
            paid_from_trading_vol: false,
        }
    }

    EthCoin::get_gas_price.mock_safe(|_| MockResult::Return(Box::new(futures01::future::ok(GAS_PRICE.into()))));

    let (_ctx, coin) = eth_coin_for_test(EthCoinType::Eth, vec!["http://dummy.dummy".into()], None);

    let actual = block_on(coin.get_sender_trade_fee(
        TradePreimageValue::UpperBound(150.into()),
        FeeApproxStage::WithoutApprox,
    ))
    .expect("!get_sender_trade_fee");
    let expected = expected_fee(GAS_PRICE);
    assert_eq!(actual, expected);

    let value = u256_to_big_decimal(100.into(), 18).expect("!u256_to_big_decimal");
    let actual = block_on(coin.get_sender_trade_fee(TradePreimageValue::Exact(value), FeeApproxStage::OrderIssue))
        .expect("!get_sender_trade_fee");
    let expected = expected_fee(GAS_PRICE_APPROXIMATION_ON_ORDER_ISSUE);
    assert_eq!(actual, expected);

    let value = u256_to_big_decimal(1.into(), 18).expect("!u256_to_big_decimal");
    let actual = block_on(coin.get_sender_trade_fee(TradePreimageValue::Exact(value), FeeApproxStage::StartSwap))
        .expect("!get_sender_trade_fee");
    let expected = expected_fee(GAS_PRICE_APPROXIMATION_ON_START_SWAP);
    assert_eq!(actual, expected);

    let value = u256_to_big_decimal(10000000000u64.into(), 18).expect("!u256_to_big_decimal");
    let actual = block_on(coin.get_sender_trade_fee(TradePreimageValue::Exact(value), FeeApproxStage::TradePreimage))
        .expect("!get_sender_trade_fee");
    let expected = expected_fee(GAS_PRICE_APPROXIMATION_ON_TRADE_PREIMAGE);
    assert_eq!(actual, expected);
}

#[test]
fn get_erc20_sender_trade_preimage() {
    const APPROVE_GAS_LIMIT: u64 = 60_000;
    static mut ALLOWANCE: u64 = 0;
    static mut ESTIMATE_GAS_CALLED: bool = false;

    EthCoin::allowance
        .mock_safe(|_, _| MockResult::Return(Box::new(futures01::future::ok(unsafe { ALLOWANCE.into() }))));

    EthCoin::get_gas_price.mock_safe(|_| MockResult::Return(Box::new(futures01::future::ok(GAS_PRICE.into()))));
    EthCoinImpl::estimate_gas.mock_safe(|_, _| {
        unsafe { ESTIMATE_GAS_CALLED = true };
        MockResult::Return(Box::new(futures01::future::ok(APPROVE_GAS_LIMIT.into())))
    });

    fn expected_trade_fee(gas_limit: u64, gas_price: u64) -> TradeFee {
        let amount = u256_to_big_decimal((gas_limit * gas_price).into(), 18).expect("!u256_to_big_decimal");
        TradeFee {
            coin: "ETH".to_owned(),
            amount: amount.into(),
            paid_from_trading_vol: false,
        }
    }

    let (_ctx, coin) = eth_coin_for_test(
        EthCoinType::Erc20 {
            platform: "ETH".to_string(),
            token_addr: Address::default(),
        },
        vec!["http://dummy.dummy".into()],
        None,
    );

    // value is allowed
    unsafe { ALLOWANCE = 1000 };
    let value = u256_to_big_decimal(1000.into(), 18).expect("u256_to_big_decimal");
    let actual =
        block_on(coin.get_sender_trade_fee(TradePreimageValue::UpperBound(value), FeeApproxStage::WithoutApprox))
            .expect("!get_sender_trade_fee");
    log!([actual.amount.to_decimal()]);
    unsafe { assert!(!ESTIMATE_GAS_CALLED) }
    assert_eq!(actual, expected_trade_fee(300_000, GAS_PRICE));

    // value is greater than allowance
    unsafe { ALLOWANCE = 999 };
    let value = u256_to_big_decimal(1000.into(), 18).expect("u256_to_big_decimal");
    let actual = block_on(coin.get_sender_trade_fee(TradePreimageValue::UpperBound(value), FeeApproxStage::StartSwap))
        .expect("!get_sender_trade_fee");
    unsafe {
        assert!(ESTIMATE_GAS_CALLED);
        ESTIMATE_GAS_CALLED = false;
    }
    assert_eq!(
        actual,
        expected_trade_fee(360_000, GAS_PRICE_APPROXIMATION_ON_START_SWAP)
    );

    // value is allowed
    unsafe { ALLOWANCE = 1000 };
    let value = u256_to_big_decimal(999.into(), 18).expect("u256_to_big_decimal");
    let actual = block_on(coin.get_sender_trade_fee(TradePreimageValue::Exact(value), FeeApproxStage::OrderIssue))
        .expect("!get_sender_trade_fee");
    unsafe { assert!(!ESTIMATE_GAS_CALLED) }
    assert_eq!(
        actual,
        expected_trade_fee(300_000, GAS_PRICE_APPROXIMATION_ON_ORDER_ISSUE)
    );

    // value is greater than allowance
    unsafe { ALLOWANCE = 1000 };
    let value = u256_to_big_decimal(1500.into(), 18).expect("u256_to_big_decimal");
    let actual = block_on(coin.get_sender_trade_fee(TradePreimageValue::Exact(value), FeeApproxStage::TradePreimage))
        .expect("!get_sender_trade_fee");
    unsafe {
        assert!(ESTIMATE_GAS_CALLED);
        ESTIMATE_GAS_CALLED = false;
    }
    assert_eq!(
        actual,
        expected_trade_fee(360_000, GAS_PRICE_APPROXIMATION_ON_TRADE_PREIMAGE)
    );
}

#[test]
fn get_receiver_trade_preimage() {
    EthCoin::get_gas_price.mock_safe(|_| MockResult::Return(Box::new(futures01::future::ok(GAS_PRICE.into()))));

    let (_ctx, coin) = eth_coin_for_test(EthCoinType::Eth, vec!["http://dummy.dummy".into()], None);
    // `eth_receiver_spend` default (CRD R57.5.2); the baseline constant was 150_000.
    let amount = u256_to_big_decimal((65_000 * GAS_PRICE).into(), 18).expect("!u256_to_big_decimal");
    let expected_fee = TradeFee {
        coin: "ETH".to_owned(),
        amount: amount.into(),
        paid_from_trading_vol: false,
    };

    let actual = coin
        .get_receiver_trade_fee(FeeApproxStage::WithoutApprox)
        .wait()
        .expect("!get_sender_trade_fee");
    assert_eq!(actual, expected_fee);
}

#[test]
fn test_get_fee_to_send_taker_fee() {
    const DEX_FEE_AMOUNT: u64 = 100_000;
    const TRANSFER_GAS_LIMIT: u64 = 40_000;

    EthCoin::get_gas_price.mock_safe(|_| MockResult::Return(Box::new(futures01::future::ok(GAS_PRICE.into()))));
    EthCoinImpl::estimate_gas
        .mock_safe(|_, _| MockResult::Return(Box::new(futures01::future::ok(TRANSFER_GAS_LIMIT.into()))));

    // fee to send taker fee is `TRANSFER_GAS_LIMIT * gas_price` always.
    let amount = u256_to_big_decimal((TRANSFER_GAS_LIMIT * GAS_PRICE).into(), 18).expect("!u256_to_big_decimal");
    let expected_fee = TradeFee {
        coin: "ETH".to_owned(),
        amount: amount.into(),
        paid_from_trading_vol: false,
    };

    let dex_fee_amount = u256_to_big_decimal(DEX_FEE_AMOUNT.into(), 18).expect("!u256_to_big_decimal");

    // Reloaded operates only on its compiled netids (no default network; see
    // docs/GLEEC_COMPATIBILITY.md). Exercise every supported netid so each
    // network's DEX-fee address stays valid; the fee itself is netid-independent.
    for &netid in mm2_net_config::SUPPORTED_NETIDS {
        let (_ctx, coin) =
            eth_coin_for_test_with_netid(EthCoinType::Eth, vec!["http://dummy.dummy".into()], None, netid);
        let actual = block_on(coin.get_fee_to_send_taker_fee(dex_fee_amount.clone(), FeeApproxStage::WithoutApprox))
            .expect("!get_fee_to_send_taker_fee");
        assert_eq!(actual, expected_fee, "netid {netid}");

        let (_ctx, coin) = eth_coin_for_test_with_netid(
            EthCoinType::Erc20 {
                platform: "ETH".to_string(),
                token_addr: Address::from_slice(&hex::decode("aD22f63404f7305e4713CcBd4F296f34770513f4").unwrap()),
            },
            vec!["http://dummy.dummy".into()],
            None,
            netid,
        );
        let actual = block_on(coin.get_fee_to_send_taker_fee(dex_fee_amount.clone(), FeeApproxStage::WithoutApprox))
            .expect("!get_fee_to_send_taker_fee");
        assert_eq!(actual, expected_fee, "netid {netid}");
    }
}

/// Some ERC20 tokens return the `error: -32016, message: \"The execution failed due to an exception.\"` error
/// if the balance is insufficient.
/// So [`EthCoin::get_fee_to_send_taker_fee`] must return [`TradePreimageError::NotSufficientBalance`].
///
/// `estimate_gas` and `my_balance` are mocked so the check runs offline and deterministically:
/// `estimate_gas` reverts the way an insufficient-balance transfer does, and the mocked balance
/// cannot cover the dex fee, so the code must classify it as `NotSufficientBalance`.
#[test]
fn test_get_fee_to_send_taker_fee_insufficient_balance() {
    const DEX_FEE_AMOUNT: u64 = 100_000_000_000;
    const GAS_PRICE: u64 = 40;

    EthCoin::get_gas_price.mock_safe(|_| MockResult::Return(Box::new(futures01::future::ok(GAS_PRICE.into()))));
    // Reproduce the revert some ERC20 tokens raise when the sender can't afford the transfer.
    EthCoinImpl::estimate_gas.mock_safe(|_, _| {
        MockResult::Return(Box::new(futures01::future::err(MmError::new(Web3RpcError::Transport(
            "error code -32016: The execution failed due to an exception.".to_string(),
        )))))
    });
    // The wallet holds fewer tokens than the dex fee it is asked to send.
    EthCoin::my_balance.mock_safe(|_| MockResult::Return(Box::new(futures01::future::ok(U256::from(1u64)))));

    let (_ctx, coin) = eth_coin_for_test(
        EthCoinType::Erc20 {
            platform: "ETH".to_string(),
            token_addr: Address::from_slice(&hex::decode("aD22f63404f7305e4713CcBd4F296f34770513f4").unwrap()),
        },
        vec!["http://dummy.dummy".into()],
        None,
    );
    let dex_fee_amount = u256_to_big_decimal(DEX_FEE_AMOUNT.into(), 18).expect("!u256_to_big_decimal");

    let error =
        block_on(coin.get_fee_to_send_taker_fee(dex_fee_amount.clone(), FeeApproxStage::WithoutApprox)).unwrap_err();
    log!((error));
    assert!(
        matches!(error.get_inner(), TradePreimageError::NotSufficientBalance { .. }),
        "Expected TradePreimageError::NotSufficientBalance"
    );
}

/// A wallet can hold plenty of the token being sent yet none of the platform coin the
/// gas is paid in — BSC reports this as `-32000 "gas required exceeds allowance (0)"`.
/// The shortfall must be reported against the *platform* ticker, because that is what
/// makes callers classify it as `NotSufficientBaseCoinBalance` ("not enough ETH/BNB
/// for fees") instead of surfacing the raw provider error.
#[test]
fn test_get_fee_to_send_taker_fee_insufficient_gas_balance() {
    const DEX_FEE_AMOUNT: u64 = 100_000_000_000;
    const GAS_PRICE: u64 = 40;

    EthCoin::get_gas_price.mock_safe(|_| MockResult::Return(Box::new(futures01::future::ok(GAS_PRICE.into()))));
    EthCoinImpl::estimate_gas.mock_safe(|_, _| {
        MockResult::Return(Box::new(futures01::future::err(MmError::new(Web3RpcError::Transport(
            "error code -32000: gas required exceeds allowance (0)".to_string(),
        )))))
    });
    // Plenty of the token being sent, so the sent-coin check must not fire...
    EthCoin::my_balance.mock_safe(|_| MockResult::Return(Box::new(futures01::future::ok(U256::from(u64::MAX)))));
    // ...but no platform coin at all to pay the gas with.
    EthCoin::base_coin_balance.mock_safe(|_| MockResult::Return(Box::new(futures01::future::ok(BigDecimal::from(0)))));

    let (_ctx, coin) = eth_coin_for_test(
        EthCoinType::Erc20 {
            platform: "ETH".to_string(),
            token_addr: Address::from_slice(&hex::decode("aD22f63404f7305e4713CcBd4F296f34770513f4").unwrap()),
        },
        vec!["http://dummy.dummy".into()],
        None,
    );
    let dex_fee_amount = u256_to_big_decimal(DEX_FEE_AMOUNT.into(), 18).expect("!u256_to_big_decimal");

    let error = block_on(coin.get_fee_to_send_taker_fee(dex_fee_amount, FeeApproxStage::WithoutApprox)).unwrap_err();
    log!((error));
    match error.get_inner() {
        // Reported against the platform coin, not the token: that ticker mismatch is
        // what `CheckBalanceError::from_trade_preimage_error` keys on.
        TradePreimageError::NotSufficientBalance { coin, .. } => assert_eq!(coin, "ETH"),
        other => panic!("Expected TradePreimageError::NotSufficientBalance for the platform coin, got {other:?}"),
    }
}

fn sender_compressed_pub(tx: &SignedEthTx) -> [u8; 33] {
    let tx_pubkey = tx.public.unwrap();
    let mut raw_pubkey = [0; 65];
    raw_pubkey[0] = 0x04;
    raw_pubkey[1..].copy_from_slice(tx_pubkey.as_bytes());
    let secp_public = PublicKey::from_slice(&raw_pubkey).unwrap();
    secp_public.serialize()
}

#[test]
fn test_negotiate_swap_contract_addr_no_fallback() {
    let (_, coin) = eth_coin_for_test(EthCoinType::Eth, vec!["http://eth1.cipig.net:8555".into()], None);

    let input = None;
    let error = coin.negotiate_swap_contract_addr(input).unwrap_err().into_inner();
    assert_eq!(NegotiateSwapContractAddrErr::NoOtherAddrAndNoFallback, error);

    let slice: &[u8] = &[1; 1];
    let error = coin.negotiate_swap_contract_addr(Some(slice)).unwrap_err().into_inner();
    assert_eq!(
        NegotiateSwapContractAddrErr::InvalidOtherAddrLen(slice.to_vec().into()),
        error
    );

    let slice: &[u8] = &[1; 20];
    let error = coin.negotiate_swap_contract_addr(Some(slice)).unwrap_err().into_inner();
    assert_eq!(
        NegotiateSwapContractAddrErr::UnexpectedOtherAddr(slice.to_vec().into()),
        error
    );

    let slice: &[u8] = coin.swap_contract_address.as_ref();
    let result = coin.negotiate_swap_contract_addr(Some(slice)).unwrap();
    assert_eq!(Some(slice.to_vec().into()), result);
}

#[test]
fn test_negotiate_swap_contract_addr_refuses_a_coin_without_a_swap_contract() {
    // A wallet-only coin is activated without a swap contract; it must never take part in a swap, not even
    // against a peer that names the zero address.
    let (_, coin) = eth_coin_for_test(EthCoinType::Eth, vec!["http://eth1.cipig.net:8555".into()], None);
    let mut inner = Arc::try_unwrap(coin.0).unwrap_or_else(|_| panic!("a fresh coin has one owner"));
    inner.swap_contract_address = Address::default();
    let coin = EthCoin(Arc::new(inner));

    let zero: &[u8] = &[0; 20];
    for other_side in [None, Some(zero)] {
        let error = coin.negotiate_swap_contract_addr(other_side).unwrap_err().into_inner();
        assert_eq!(NegotiateSwapContractAddrErr::NoSwapContract, error);
    }
}

#[test]
fn test_negotiate_swap_contract_addr_has_fallback() {
    let fallback = Address::from_slice(&hex::decode("8500AFc0bc5214728082163326C2FF0C73f4a871").unwrap());

    let (_, coin) = eth_coin_for_test(
        EthCoinType::Eth,
        vec!["http://eth1.cipig.net:8555".into()],
        Some(fallback),
    );

    let input = None;
    let result = coin.negotiate_swap_contract_addr(input).unwrap();
    assert_eq!(Some(fallback.as_bytes().to_vec().into()), result);

    let slice: &[u8] = &[1; 1];
    let error = coin.negotiate_swap_contract_addr(Some(slice)).unwrap_err().into_inner();
    assert_eq!(
        NegotiateSwapContractAddrErr::InvalidOtherAddrLen(slice.to_vec().into()),
        error
    );

    let slice: &[u8] = &[1; 20];
    let error = coin.negotiate_swap_contract_addr(Some(slice)).unwrap_err().into_inner();
    assert_eq!(
        NegotiateSwapContractAddrErr::UnexpectedOtherAddr(slice.to_vec().into()),
        error
    );

    let slice: &[u8] = coin.swap_contract_address.as_ref();
    let result = coin.negotiate_swap_contract_addr(Some(slice)).unwrap();
    assert_eq!(Some(slice.to_vec().into()), result);

    let slice: &[u8] = fallback.as_ref();
    let result = coin.negotiate_swap_contract_addr(Some(slice)).unwrap();
    assert_eq!(Some(fallback.as_bytes().to_vec().into()), result);
}

#[test]
fn polygon_check_if_my_payment_sent() {
    // Network-gated: set POLYGON_RPC_URL to run it (Alchemy or Ankr free tier is
    // fine). Skipped otherwise so the offline suite stays green; a network CI job
    // runs it by setting the secret.
    let url = match std::env::var("POLYGON_RPC_URL") {
        Ok(url) => url,
        Err(_) => {
            log!("skipping polygon_check_if_my_payment_sent: set POLYGON_RPC_URL to run it");
            return;
        },
    };

    let ctx = MmCtxBuilder::new().into_mm_arc();
    let conf = json!({
      "coin": "MATIC",
      "name": "matic",
      "fname": "Polygon",
      "rpcport": 80,
      "mm2": 1,
      "chain_id": 137,
      "avg_blocktime": 0.03,
      "required_confirmations": 3,
      // Small log-scan chunk so the very first eth_getLogs call is tiny and stays
      // within free-tier getLogs range limits (Alchemy/Ankr free tiers reject wide
      // ranges).
      "logs_block_range": 5,
      "protocol": {
        "type": "ETH"
      }
    });

    let request = json!({
        "method": "enable",
        "coin": "MATIC",
        "urls": [url],
        "swap_contract_address": "0x9130b257d37a52e52f21054c4da3450c72f595ce",
    });

    let priv_key = [1; 32];
    let coin = block_on(eth_coin_from_conf_and_request(
        &ctx,
        "MATIC",
        &conf,
        &request,
        &priv_key,
        CoinProtocol::ETH { chain_id: None },
    ))
    .unwrap();

    // A known, immutable historical maker payment on Polygon mainnet (Dec 2021),
    // emitted in block 22_185_152. We start the scan two blocks before it with a
    // small `logs_block_range` (see conf) so `check_if_my_payment_sent` finds the
    // PaymentSent event in its very first getLogs chunk and returns immediately —
    // no crawl toward chain head, and the query stays within free-tier limits.
    //
    // `from_block` is a fixed historical constant, deliberately NOT derived from
    // the coin's own `current_block()` (the method the tested code uses), so the
    // fixture stays independent of the code under test. Because the event is found
    // in that first bounded chunk, the outcome does not depend on `current_block()`.
    // This verifies the real found-path (locating and reconstructing the payment
    // tx) and never goes stale, since the history is immutable.
    const PAYMENT_BLOCK: u64 = 22_185_152;
    let from_block = PAYMENT_BLOCK - 2;

    let secret_hash = hex::decode("fc33114b389f0ee1212abf2867e99e89126f4860").unwrap();
    let swap_contract_address: BytesJson = "9130b257d37a52e52f21054c4da3450c72f595ce".into();
    let my_payment = coin
        .check_if_my_payment_sent(
            1638764369,
            &[],
            &[],
            &secret_hash,
            from_block,
            &Some(swap_contract_address.clone()),
        )
        .wait()
        .unwrap()
        .unwrap();
    let expected_hash = BytesJson::from("69a20008cea0c15ee483b5bbdff942752634aa072dfd2ff715fe87eec302de11");
    assert_eq!(expected_hash, my_payment.tx_hash());

    // Negative case: crawl only the last several blocks for the same payment id.
    // Its PaymentSent event is in 2021, so scanning a recent window must NOT find
    // it — this exercises the "scanned the whole window, nothing found -> None"
    // branch. The head is read via the alloy provider directly, NOT the coin's
    // `current_block()` (the method the tested code uses), so the fixture stays
    // independent of the code under test. The window is small (a handful of
    // `logs_block_range` chunks), so it stays within free-tier getLogs limits.
    let recent_head = {
        use alloy::providers::Provider as _;
        block_on(coin.alloy_provider().get_block_number()).unwrap()
    };
    let recent_from_block = recent_head.saturating_sub(20);
    let not_found = coin
        .check_if_my_payment_sent(
            1638764369,
            &[],
            &[],
            &secret_hash,
            recent_from_block,
            &Some(swap_contract_address),
        )
        .wait()
        .unwrap();
    assert!(
        not_found.is_none(),
        "payment must not be found in the recent {}..{} window",
        recent_from_block,
        recent_head
    );
}

#[test]
fn test_message_hash() {
    let key_pair = KeyPair::from_secret_slice(
        &hex::decode("809465b17d0a4ddb3e4c69e8f23c2cabad868f51f8bed5c765ad1d6516c3306f").unwrap(),
    )
    .unwrap();
    let web3 = crate::eth::alloy_compat::build_provider(
        crate::node_url::listed_nodes("TEST", vec!["http://195.201.0.6:8545".into()]),
        vec![],
    )
    .unwrap();
    let ctx = MmCtxBuilder::new().into_mm_arc();
    let my_addr = key_pair.address();
    let coin = EthCoin(Arc::new(EthCoinImpl {
        ticker: "ETH".into(),
        coin_type: EthCoinType::Eth,
        my_address: my_addr,
        sign_message_prefix: Some(String::from("Ethereum Signed Message:\n")),
        signer: EthSigner::Local(key_pair),
        swap_contract_address: Address::from_slice(&hex::decode("7Bc1bBDD6A0a722fC9bffC49c921B685ECB84b94").unwrap()),
        fallback_swap_contract: None,
        web3_instances: vec![Web3Instance {
            web3: web3.clone(),
            is_parity: true,
        }],
        web3,
        decimals: 18,
        gas_station_url: None,
        gas_station_decimals: ETH_GAS_STATION_DECIMALS,
        gas_station_policy: GasStationPricePolicy::MeanAverageFast,
        history_sync_state: Mutex::new(HistorySyncState::NotStarted),
        ctx: ctx.weak(),
        required_confirmations: 1.into(),
        tron_api: None,
        nft_swap_v2_contract: None,
        swap_gas_fee_policy: Mutex::new(SwapGasFeePolicy::default()),
        erc20_tokens_infos: Default::default(),
        custom_token: false,
        chain_id: None,
        logs_block_range: DEFAULT_LOGS_BLOCK_RANGE,
        derivation_method: DerivationMethod::Iguana(my_addr),
        swap_v2_contracts: None,
        gas_limit: EthGasLimit::default(),
        gas_limit_v2: EthGasLimitV2::default(),
        estimate_gas_mult: None,
    }));

    let message_hash = coin.sign_message_hash("test").unwrap();
    assert_eq!(
        hex::encode(message_hash),
        "4a5c5d454721bbbb25540c3317521e71c373ae36458f960d2ad46ef088110e95"
    );
}

#[test]
fn test_sign_verify_message() {
    let key_pair = KeyPair::from_secret_slice(
        &hex::decode("809465b17d0a4ddb3e4c69e8f23c2cabad868f51f8bed5c765ad1d6516c3306f").unwrap(),
    )
    .unwrap();
    let web3 = crate::eth::alloy_compat::build_provider(
        crate::node_url::listed_nodes("TEST", vec!["http://195.201.0.6:8545".into()]),
        vec![],
    )
    .unwrap();
    let ctx = MmCtxBuilder::new().into_mm_arc();
    let my_addr = key_pair.address();
    let coin = EthCoin(Arc::new(EthCoinImpl {
        ticker: "ETH".into(),
        coin_type: EthCoinType::Eth,
        my_address: my_addr,
        sign_message_prefix: Some(String::from("Ethereum Signed Message:\n")),
        signer: EthSigner::Local(key_pair),
        swap_contract_address: Address::from_slice(&hex::decode("7Bc1bBDD6A0a722fC9bffC49c921B685ECB84b94").unwrap()),
        fallback_swap_contract: None,
        web3_instances: vec![Web3Instance {
            web3: web3.clone(),
            is_parity: true,
        }],
        web3,
        decimals: 18,
        gas_station_url: None,
        gas_station_decimals: ETH_GAS_STATION_DECIMALS,
        gas_station_policy: GasStationPricePolicy::MeanAverageFast,
        history_sync_state: Mutex::new(HistorySyncState::NotStarted),
        ctx: ctx.weak(),
        required_confirmations: 1.into(),
        tron_api: None,
        nft_swap_v2_contract: None,
        swap_gas_fee_policy: Mutex::new(SwapGasFeePolicy::default()),
        erc20_tokens_infos: Default::default(),
        custom_token: false,
        chain_id: None,
        logs_block_range: DEFAULT_LOGS_BLOCK_RANGE,
        derivation_method: DerivationMethod::Iguana(my_addr),
        swap_v2_contracts: None,
        gas_limit: EthGasLimit::default(),
        gas_limit_v2: EthGasLimitV2::default(),
        estimate_gas_mult: None,
    }));

    let message = "test";
    let signature = coin.sign_message(message).unwrap();
    assert_eq!(signature, "0xcdf11a9c4591fb7334daa4b21494a2590d3f7de41c7d2b333a5b61ca59da9b311b492374cc0ba4fbae53933260fa4b1c18f15d95b694629a7b0620eec77a938600");

    let is_valid = coin
        .verify_message(&signature, message, "0xbAB36286672fbdc7B250804bf6D14Be0dF69fa29")
        .unwrap();
    assert!(is_valid);
}

// ---------------------------------------------------------------------------
// addr_from_pubkey_str — pure pubkey-to-address conversion
// ---------------------------------------------------------------------------

#[test]
fn test_addr_from_pubkey_str_valid() {
    // Use secp256k1 to derive a proper compressed pubkey (33 bytes)
    let secret = hex::decode("809465b17d0a4ddb3e4c69e8f23c2cabad868f51f8bed5c765ad1d6516c3306f").unwrap();
    let secp = secp256k1::Secp256k1::new();
    let secret_key = secp256k1::SecretKey::from_slice(&secret).unwrap();
    let pubkey = secp256k1::PublicKey::from_secret_key(&secp, &secret_key);
    let pubkey_hex = hex::encode(pubkey.serialize()); // 33-byte compressed
    let addr = addr_from_pubkey_str(&pubkey_hex).unwrap();
    // Should produce a valid Ethereum address (0x-prefixed, 42 chars)
    assert!(addr.starts_with("0x"));
    assert_eq!(addr.len(), 42);
}

#[test]
fn test_addr_from_pubkey_str_invalid_hex() {
    let result = addr_from_pubkey_str("not_a_hex_string");
    assert!(result.is_err());
}

#[test]
fn test_addr_from_pubkey_str_wrong_length() {
    // Only 3 bytes instead of 33/65
    let result = addr_from_pubkey_str("aabbcc");
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// wei_from_gwei_decimal — gwei to wei conversion
// ---------------------------------------------------------------------------

#[test]
fn test_wei_from_gwei_decimal_one() {
    use common::mm_number::BigDecimal;
    let one_gwei: BigDecimal = "1".parse().unwrap();
    let wei = wei_from_gwei_decimal(&one_gwei).unwrap();
    assert_eq!(wei, U256::from(1_000_000_000u64));
}

#[test]
fn test_wei_from_gwei_decimal_fractional() {
    use common::mm_number::BigDecimal;
    let half_gwei: BigDecimal = "0.5".parse().unwrap();
    let wei = wei_from_gwei_decimal(&half_gwei).unwrap();
    assert_eq!(wei, U256::from(500_000_000u64));
}

#[test]
fn test_wei_from_gwei_decimal_large() {
    use common::mm_number::BigDecimal;
    let amount: BigDecimal = "100".parse().unwrap();
    let wei = wei_from_gwei_decimal(&amount).unwrap();
    assert_eq!(wei, U256::from(100_000_000_000u64));
}

// ---------------------------------------------------------------------------
// GasStationData — JSON deserialization
// ---------------------------------------------------------------------------

#[test]
fn test_gas_station_data_standard_format() {
    let json = r#"{"average": 30, "fast": 50}"#;
    let data: GasStationData = serde_json::from_str(json).unwrap();
    // GasStationData fields are private — just verify it deserializes
    let _ = format!("{:?}", serde_json::to_value(&data));
}

#[test]
fn test_gas_station_data_matic_format() {
    // Matic gas station uses "standard" instead of "average"
    let json = r#"{"standard": 25, "fast": 40}"#;
    let _data: GasStationData = serde_json::from_str(json).unwrap();
}

#[test]
fn test_gas_station_data_missing_average() {
    let json = r#"{"fast": 50}"#;
    let result = serde_json::from_str::<GasStationData>(json);
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// u256_to_big_decimal — roundtrip with wei_from_big_decimal
// ---------------------------------------------------------------------------

#[test]
fn test_u256_big_decimal_roundtrip() {
    use common::mm_number::BigDecimal;
    let original: BigDecimal = "1.5".parse().unwrap();
    let wei = wei_from_big_decimal(&original, 18).unwrap();
    let reconstructed = u256_to_big_decimal(wei, 18).unwrap();
    assert_eq!(original, reconstructed);
}

#[test]
fn test_u256_to_big_decimal_zero() {
    let result = u256_to_big_decimal(U256::from(0), 18).unwrap();
    assert_eq!(result, 0.into());
}

// ---------------------------------------------------------------------------
// wei_to_gwei_decimal — inverse of wei_from_gwei_decimal
// ---------------------------------------------------------------------------

#[test]
fn test_wei_to_gwei_roundtrip() {
    use common::mm_number::BigDecimal;
    let gwei: BigDecimal = "42.123456789".parse().unwrap();
    let wei = wei_from_gwei_decimal(&gwei).unwrap();
    let back = wei_to_gwei_decimal(wei).unwrap();
    assert_eq!(gwei, back);
}

// ---------------------------------------------------------------------------
// EVM Trezor signing policy (CRD §50). These fixtures build an EthCoin with an
// `EthSigner::Trezor` policy directly (no device, no task handle): the signer
// stores only the address / public key / derivation path (R50.1).
// ---------------------------------------------------------------------------

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
fn trezor_eth_coin_for_test(coin_type: EthCoinType) -> (MmArc, EthCoin) {
    // Fixed key material only to source a stable address + public key for the
    // Trezor signer fixture; no secret is stored on the coin (CRD R50.1).
    let key_pair = KeyPair::from_secret_slice(
        &hex::decode("809465b17d0a4ddb3e4c69e8f23c2cabad868f51f8bed5c765ad1d6516c3306f").unwrap(),
    )
    .unwrap();
    let address = key_pair.address();
    let public = *key_pair.public();
    let derivation_path = DerivationPath::from_str("m/44'/60'/0'/0/0").unwrap();

    let web3 = crate::eth::alloy_compat::build_provider(
        crate::node_url::listed_nodes("TEST", vec!["http://dummy.dummy".into()]),
        vec![],
    )
    .unwrap();
    let conf = json!({ "netid": mm2_net_config::SUPPORTED_NETIDS[0], "coins": [] });
    let ctx = MmCtxBuilder::new().with_conf(conf).into_mm_arc();
    let ticker = match coin_type {
        EthCoinType::Eth => "ETH",
        EthCoinType::Erc20 { .. } => "JST",
        EthCoinType::Tron => "TRX",
        EthCoinType::Trc20 { .. } => "TRC20",
    }
    .to_string();

    let eth_coin = EthCoin(Arc::new(EthCoinImpl {
        coin_type,
        decimals: 18,
        gas_station_url: None,
        gas_station_decimals: ETH_GAS_STATION_DECIMALS,
        history_sync_state: Mutex::new(HistorySyncState::NotEnabled),
        gas_station_policy: GasStationPricePolicy::MeanAverageFast,
        my_address: address,
        sign_message_prefix: Some(String::from("Ethereum Signed Message:\n")),
        signer: EthSigner::Trezor(EthTrezorSigner {
            derivation_path,
            address,
            public,
        }),
        swap_contract_address: Address::from_slice(&hex::decode("7Bc1bBDD6A0a722fC9bffC49c921B685ECB84b94").unwrap()),
        fallback_swap_contract: None,
        ticker,
        web3_instances: vec![Web3Instance {
            web3: web3.clone(),
            is_parity: true,
        }],
        web3,
        ctx: ctx.weak(),
        required_confirmations: 1.into(),
        tron_api: None,
        nft_swap_v2_contract: None,
        swap_gas_fee_policy: Mutex::new(SwapGasFeePolicy::default()),
        erc20_tokens_infos: Default::default(),
        custom_token: false,
        chain_id: Some(1),
        logs_block_range: DEFAULT_LOGS_BLOCK_RANGE,
        derivation_method: DerivationMethod::Iguana(address),
        swap_v2_contracts: None,
        gas_limit: EthGasLimit::default(),
        gas_limit_v2: EthGasLimitV2::default(),
        estimate_gas_mult: None,
    }));
    (ctx, eth_coin)
}

/// A hardware-backed coin is never a software HD wallet, so the one-shot `enable_erc20` keeps refusing it: that
/// call has no `user_action` channel for the device to confirm through (CRD R35.2.5). The coin under test keeps
/// its `HDWallet` derivation and differs from the software one only by its signer, so the hardware clauses of the
/// predicate are the ones being exercised.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
#[test]
fn a_trezor_signed_hd_coin_is_not_a_software_hd_wallet() {
    let (_ctx, software, address) = eth_hd_coin_for_test(EthCoinType::Eth);
    assert!(matches!(software.derivation_method, DerivationMethod::HDWallet(_)));
    assert!(software.uses_software_hd_wallet(), "the control: a software HD coin");

    let key_pair = KeyPair::from_secret_slice(
        &hex::decode("809465b17d0a4ddb3e4c69e8f23c2cabad868f51f8bed5c765ad1d6516c3306f").unwrap(),
    )
    .unwrap();
    let mut inner = Arc::try_unwrap(software.0).unwrap_or_else(|_| panic!("a sole owner of the fixture"));
    inner.signer = EthSigner::Trezor(EthTrezorSigner {
        derivation_path: DerivationPath::from_str("m/44'/60'/0'/0/0").unwrap(),
        address,
        public: *key_pair.public(),
    });
    let hardware = EthCoin(Arc::new(inner));

    assert!(matches!(hardware.derivation_method, DerivationMethod::HDWallet(_)));
    assert!(hardware.uses_hardware_signer());
    assert!(hardware.hardware_derivation_path().is_some());
    assert!(!hardware.uses_software_hd_wallet());
}

/// R50.24 / R49.6: the direct legacy `withdraw` method rejects a Trezor-policy
/// coin, steering clients to the `task::withdraw` API. No device interaction.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
#[test]
fn direct_withdraw_rejected_for_trezor_signer() {
    use futures::compat::Future01CompatExt;
    let (_ctx, coin) = trezor_eth_coin_for_test(EthCoinType::Eth);
    let req = WithdrawRequest {
        amount: 1.into(),
        from: None,
        to: "0x7Bc1bBDD6A0a722fC9bffC49c921B685ECB84b94".to_string(),
        coin: "ETH".to_string(),
        max: false,
        fee: None,
    };
    let err = block_on(coin.withdraw(req).compat()).unwrap_err();
    assert!(matches!(err.into_inner(), WithdrawError::UnsupportedUnderTrezor(_)));
}

/// R50.20: a TRON-family coin under the Trezor policy is rejected as unsupported
/// before any device exchange (no connect, PIN, passphrase, or signing).
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
#[test]
fn trezor_withdraw_rejects_tron_without_device() {
    let (_ctx, coin) = trezor_eth_coin_for_test(EthCoinType::Tron);
    let err = crate::eth::eth_trezor_withdraw::ensure_trezor_withdraw_supported(&coin).unwrap_err();
    assert!(matches!(err.into_inner(), WithdrawError::UnsupportedUnderTrezor(_)));
}

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
#[test]
fn tron_v1_sender_trade_fee_returns_error_without_network() {
    let (_ctx, coin) = trezor_eth_coin_for_test(EthCoinType::Tron);
    let err = block_on(coin.get_sender_trade_fee(TradePreimageValue::Exact(1.into()), FeeApproxStage::WithoutApprox))
        .unwrap_err();
    assert!(
        matches!(err.into_inner(), TradePreimageError::InternalError(msg) if msg.contains("TRON V1 sender trade fee"))
    );
}

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
#[test]
fn tron_v1_wait_for_tx_spend_returns_error_without_decoding() {
    let (_ctx, coin) = trezor_eth_coin_for_test(EthCoinType::Tron);
    let err = coin
        .wait_for_tx_spend(&[], 0, 0, &coin.swap_contract_address())
        .wait()
        .unwrap_err();
    assert!(matches!(err, TransactionErr::Plain(msg) if msg.contains("TRON V1 swap spend watchers")));
}

fn legacy_eip155_vector_bytes() -> Vec<u8> {
    hex::decode(
        "f86c098504a817c800825208943535353535353535353535353535353535353535880de0b6b3a76400008025a028ef61340bd939bc2195fe537567866003e1a15d3c71ff63e1590620aa636276a067cbe9d8997f761aecb703304b3800ccf555c9f3dc64214b297fb1966a3b6d83",
    )
    .unwrap()
}

#[test]
fn test_signed_eth_tx_from_bytes_legacy_unchanged() {
    let bytes = legacy_eip155_vector_bytes();
    let signed = signed_eth_tx_from_bytes(&bytes).unwrap();
    assert_eq!(alloy::rlp::encode(&*signed), bytes);
    assert_eq!(
        format!("{:x}", signed.tx_hash()),
        "33469b22e9f636356c4160a87eb19df52b7412e8eac32a4a55ffe88ea8350788"
    );
}

fn typed_envelope(type_byte: u8) -> Vec<u8> {
    // `type_byte || rlp(list of a few plausible fields)`; need not be a valid typed transaction.
    let mut bytes = vec![type_byte];
    bytes.extend_from_slice(&alloy::rlp::encode(&vec![1u64, 2, 3]));
    bytes
}

#[test]
fn test_signed_eth_tx_from_bytes_typed_eip4844_and_eip7702_rejected() {
    for (type_byte, name) in [(0x03u8, "EIP-4844"), (0x04, "EIP-7702")] {
        let bytes = typed_envelope(type_byte);
        let err = signed_eth_tx_from_bytes(&bytes).unwrap_err();
        assert!(err.contains(&format!("type 0x{type_byte:02x}")), "{err}");
        assert!(err.contains(name), "{err}");
        assert!(err.contains("only legacy, EIP-2930 and EIP-1559"), "{err}");
        assert!(err.contains(&format!("payload length {} bytes", bytes.len())), "{err}");
        assert!(!err.contains('\n'));
    }
}

#[test]
fn test_signed_eth_tx_from_bytes_typed_unknown() {
    let bytes = typed_envelope(0x7f);
    let err = signed_eth_tx_from_bytes(&bytes).unwrap_err();
    assert!(err.contains("type 0x7f"), "{err}");
    assert!(err.contains("unknown type"), "{err}");
    assert!(err.contains("only legacy, EIP-2930 and EIP-1559"), "{err}");
}

#[test]
fn test_signed_eth_tx_from_bytes_malformed_supported_typed_reports_length_and_prefix() {
    for type_byte in [0x01u8, 0x02] {
        let bytes = typed_envelope(type_byte);
        let err = signed_eth_tx_from_bytes(&bytes).unwrap_err();
        assert!(err.contains(&format!("payload length {} bytes", bytes.len())), "{err}");
        assert!(err.contains(&format!("0x{type_byte:02x}")), "{err}");
    }
}

#[test]
fn test_signed_eth_tx_from_bytes_non_transaction_payload() {
    // A bare RLP-encoded 32-byte string (e.g. a hash): 0xa0 || 32 bytes.
    let bytes = alloy::rlp::encode(&[0xabu8; 32][..]);
    assert_eq!(bytes.len(), 33);
    let err = signed_eth_tx_from_bytes(&bytes).unwrap_err();
    assert!(err.contains("payload length 33 bytes"), "{err}");
    assert!(err.contains(&format!("first 10 bytes 0xa0{}", "ab".repeat(9))), "{err}");
    assert!(!err.contains("typed EIP-2718"), "{err}");
    assert!(!err.contains('\n'));

    let err = signed_eth_tx_from_bytes(&[]).unwrap_err();
    assert!(err.contains("payload length 0 bytes"), "{err}");
}

mod swap_gas_fee_policy_tx_form {
    //! CRD R35.6.6 / R35.6A.17: the stored swap gas-fee policy decides the pricing and the
    //! serialized form of a swap transaction. These tests exercise the construction and signing
    //! path, not the store-and-echo behaviour of the policy RPCs.
    use super::*;
    use crate::eth::fee_estimation::eip1559::{EstimationSource, FeePerGasEstimated, FeePerGasLevel};
    use mm2_eth::keys::{public_to_address, Secret};
    use std::str::FromStr;

    const SECRET: &str = "4646464646464646464646464646464646464646464646464646464646464646";
    const CHAIN_ID: u64 = 1;

    fn secret() -> Secret { Secret::from_str(SECRET).unwrap() }

    fn sender() -> Address {
        let pair = mm2_eth::keys::KeyPair::from_secret(secret()).unwrap();
        public_to_address(&pair.public())
    }

    fn level(prio: u64, max_fee: u64) -> FeePerGasLevel {
        FeePerGasLevel {
            max_priority_fee_per_gas: U256::from(prio),
            max_fee_per_gas: U256::from(max_fee),
            min_wait_time: None,
            max_wait_time: None,
        }
    }

    /// Three distinguishable tiers, none equal to the legacy gas price used below.
    fn estimate() -> FeePerGasEstimated {
        FeePerGasEstimated {
            base_fee: U256::from(10_000_000_000u64),
            source: EstimationSource::Simple,
            base_fee_trend: String::new(),
            priority_fee_trend: String::new(),
            low: level(1_000_000_000, 12_000_000_000),
            medium: level(2_000_000_000, 14_000_000_000),
            high: level(3_000_000_000, 17_000_000_000),
        }
    }

    fn swap_tx(fee: SwapFee) -> SwapTx {
        SwapTx::new(
            fee,
            U256::from(9u64),
            U256::from(21000u64),
            Action::Call(Address::from_str("3535353535353535353535353535353535353535").unwrap()),
            U256::from(1_000_000_000_000_000_000u64),
            vec![0xde, 0xad, 0xbe, 0xef],
        )
    }

    fn sign_legacy(tx: SwapTx) -> SignedEthTx {
        match tx {
            SwapTx::Legacy(tx) => tx.sign(&secret(), Some(CHAIN_ID)),
            SwapTx::Eip1559(_) => panic!("expected the legacy form"),
        }
    }

    fn sign_typed(tx: SwapTx) -> SignedEthTx {
        match tx {
            SwapTx::Eip1559(tx) => tx.sign(&secret()).unwrap(),
            SwapTx::Legacy(_) => panic!("expected the EIP-1559 form"),
        }
    }

    #[test]
    fn non_legacy_tiers_produce_type_2_with_the_tiers_fee_fields() {
        let est = estimate();
        for (policy, expect) in [
            (SwapGasFeePolicy::Low, &est.low),
            (SwapGasFeePolicy::Medium, &est.medium),
            (SwapGasFeePolicy::High, &est.high),
        ] {
            let fee = eip1559_swap_fee(policy, CHAIN_ID, &est).unwrap();
            let signed = sign_typed(swap_tx(fee));
            let bytes = alloy::rlp::encode(&signed);
            assert_eq!(bytes[0], 0x02, "{:?} must emit a type-2 envelope", policy);
            match &signed.kind {
                TxKind::Typed(t) => {
                    assert_eq!(t.tx_type, 2);
                    assert_eq!(t.chain_id, CHAIN_ID);
                    assert_eq!(t.max_fee_per_gas, Some(expect.max_fee_per_gas), "{:?}", policy);
                    assert_eq!(
                        t.max_priority_fee_per_gas,
                        Some(expect.max_priority_fee_per_gas),
                        "{:?}",
                        policy
                    );
                    assert_eq!(t.gas_price, None, "a type-2 transaction carries no legacy gas_price");
                },
                TxKind::Legacy { .. } => panic!("{:?} emitted the legacy form", policy),
            }
        }
    }

    #[test]
    fn tiers_are_distinct_and_ordered() {
        let est = estimate();
        let fee = |p| match eip1559_swap_fee(p, CHAIN_ID, &est).unwrap() {
            SwapFee::Eip1559 {
                max_fee_per_gas,
                max_priority_fee_per_gas,
                ..
            } => (max_fee_per_gas, max_priority_fee_per_gas),
            SwapFee::Legacy { .. } => panic!("unexpected legacy fee"),
        };
        let (low, medium, high) = (
            fee(SwapGasFeePolicy::Low),
            fee(SwapGasFeePolicy::Medium),
            fee(SwapGasFeePolicy::High),
        );
        assert!(low < medium && medium < high);
    }

    #[test]
    fn legacy_policy_has_no_eip1559_tier() {
        assert!(eip1559_swap_fee(SwapGasFeePolicy::Legacy, CHAIN_ID, &estimate()).is_err());
    }

    #[test]
    fn inconsistent_estimate_is_rejected_not_clamped() {
        let mut est = estimate();
        est.high = level(5_000_000_000, 1_000_000_000);
        assert!(eip1559_swap_fee(SwapGasFeePolicy::High, CHAIN_ID, &est).is_err());
    }

    #[test]
    fn typed_signed_transaction_round_trips_through_our_decoder() {
        let est = estimate();
        for policy in [SwapGasFeePolicy::Low, SwapGasFeePolicy::Medium, SwapGasFeePolicy::High] {
            let fee = eip1559_swap_fee(policy, CHAIN_ID, &est).unwrap();
            let signed = sign_typed(swap_tx(fee));
            assert_eq!(signed.sender(), sender(), "sender recovered at signing time");

            let bytes = alloy::rlp::encode(&signed);
            // Decode the emitted bytes the way a counterparty's bytes are decoded.
            let decoded = signed_eth_tx_from_bytes(&bytes).unwrap();
            assert_eq!(decoded.sender(), sender());
            assert_eq!(decoded.tx_hash(), signed.tx_hash());
            // The identifier is the hash of the whole envelope.
            assert_eq!(decoded.tx_hash(), H256::from_slice(keccak256(&bytes).as_slice()));
            assert_eq!(alloy::rlp::encode(&decoded), bytes);
            assert_eq!(decoded.nonce, U256::from(9u64));
            assert_eq!(decoded.gas, U256::from(21000u64));
            assert_eq!(decoded.value, U256::from(1_000_000_000_000_000_000u64));
            assert_eq!(decoded.data, vec![0xde, 0xad, 0xbe, 0xef]);
        }
    }

    #[test]
    fn typed_signing_message_is_keccak_of_type_byte_and_unsigned_payload() {
        use alloy::rlp::{Encodable, Header};
        let fee = eip1559_swap_fee(SwapGasFeePolicy::Medium, CHAIN_ID, &estimate()).unwrap();
        let SwapTx::Eip1559(tx) = swap_tx(fee) else {
            panic!("expected the EIP-1559 form")
        };
        // Independent construction of 0x02 || rlp([chain_id, nonce, prio, max_fee, gas, to, value,
        // data, access_list]) straight from the EIP, not through alloy's transaction type.
        let to = Address::from_str("3535353535353535353535353535353535353535").unwrap();
        let mut payload = Vec::new();
        CHAIN_ID.encode(&mut payload);
        9u64.encode(&mut payload);
        2_000_000_000u64.encode(&mut payload);
        14_000_000_000u64.encode(&mut payload);
        21000u64.encode(&mut payload);
        to.0[..].encode(&mut payload);
        1_000_000_000_000_000_000u64.encode(&mut payload);
        [0xdeu8, 0xad, 0xbe, 0xef][..].encode(&mut payload);
        Header {
            list: true,
            payload_length: 0,
        }
        .encode(&mut payload); // empty access list
        let mut msg = vec![0x02];
        Header {
            list: true,
            payload_length: payload.len(),
        }
        .encode(&mut msg);
        msg.extend_from_slice(&payload);
        assert_eq!(tx.signing_hash().unwrap(), H256::from_slice(keccak256(&msg).as_slice()));
    }

    #[test]
    fn legacy_form_is_unchanged_and_pinned() {
        let fee = SwapFee::Legacy {
            gas_price: U256::from(20_000_000_000u64),
        };
        // EIP-155 specification vector (empty data) to pin the bytes of the legacy form.
        let tx = SwapTx::new(
            fee,
            U256::from(9u64),
            U256::from(21000u64),
            Action::Call(Address::from_str("3535353535353535353535353535353535353535").unwrap()),
            U256::from(1_000_000_000_000_000_000u64),
            Vec::new(),
        );
        let signed = sign_legacy(tx);
        let expected = hex::decode(
            "f86c098504a817c800825208943535353535353535353535353535353535353535880de0b6b3a76400008025a028ef61340bd939bc2195fe537567866003e1a15d3c71ff63e1590620aa636276a067cbe9d8997f761aecb703304b3800ccf555c9f3dc64214b297fb1966a3b6d83",
        )
        .unwrap();
        assert_eq!(alloy::rlp::encode(&signed), expected);
        assert_eq!(signed.gas_price(), Some(U256::from(20_000_000_000u64)));
        assert!(matches!(signed.kind, TxKind::Legacy { .. }));
    }

    #[test]
    fn legacy_form_with_data_round_trips_and_is_untyped() {
        let signed = sign_legacy(swap_tx(SwapFee::Legacy {
            gas_price: U256::from(7u64),
        }));
        let bytes = alloy::rlp::encode(&signed);
        assert!(bytes[0] >= 0xc0, "legacy form leads with an RLP list header");
        let decoded = signed_eth_tx_from_bytes(&bytes).unwrap();
        assert_eq!(decoded.sender(), sender());
        assert_eq!(decoded.tx_hash(), signed.tx_hash());
    }

    #[test]
    fn default_policy_is_legacy() {
        assert_eq!(SwapGasFeePolicy::default(), SwapGasFeePolicy::Legacy);
    }
}

/// A scripted EVM JSON-RPC node on a local socket, shared by the tests that must run the real
/// network-facing code without a network: every request is answered by the handler closure
/// (`method`, `params` -> `result`). Its thread stops when the node is dropped.
mod scripted_evm_node {
    use serde_json::{json, Value as Json};
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    pub struct ScriptedNode {
        pub url: String,
        stop: Arc<AtomicBool>,
    }

    impl Drop for ScriptedNode {
        fn drop(&mut self) { self.stop.store(true, Ordering::SeqCst); }
    }

    type Handler = Arc<dyn Fn(&str, &Json) -> Json + Send + Sync>;

    fn serve(mut stream: TcpStream, handler: &Handler) {
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).ok();
        let mut raw = Vec::new();
        let mut buf = [0u8; 4096];
        let body_at = loop {
            let n = match stream.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            raw.extend_from_slice(&buf[..n]);
            if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&raw[..end]).to_lowercase();
                let len = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if raw.len() >= end + 4 + len {
                    break end + 4;
                }
            }
        };
        let request: Json = serde_json::from_slice(&raw[body_at..]).unwrap();
        let reply = |req: &Json| {
            json!({"jsonrpc": "2.0", "id": req["id"],
                   "result": handler(req["method"].as_str().unwrap(), &req["params"])})
        };
        let body = match request.as_array() {
            Some(batch) => Json::Array(batch.iter().map(reply).collect()),
            None => reply(&request),
        }
        .to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).ok();
    }

    /// Starts a node answering every request through `handler`. A method the test did not script
    /// must panic in the handler, so a test never silently depends on an unscripted read.
    pub fn spawn(handler: impl Fn(&str, &Json) -> Json + Send + Sync + 'static) -> ScriptedNode {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = stop.clone();
        let handler: Handler = Arc::new(handler);
        std::thread::spawn(move || {
            while !stop_t.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let handler = handler.clone();
                        std::thread::spawn(move || serve(stream, &handler));
                    },
                    Err(_) => std::thread::sleep(std::time::Duration::from_millis(5)),
                }
            }
        });
        ScriptedNode { url, stop }
    }
}

/// The serialized form of what the swap send path really broadcasts is pinned against the swap
/// gas-fee policy: the whole path (`SwapOps` entry point, `sign_and_send_transaction_impl`, signing,
/// `eth_sendRawTransaction`) runs against a scripted node on a local socket and the assertion reads
/// the first byte of the broadcast bytes -- the EIP-2718 envelope byte a counterparty parses.
mod swap_send_path_wire_form {
    use super::scripted_evm_node::{spawn, ScriptedNode};
    use super::*;
    use mm2_eth::keys::{public_to_address, KeyPair as EthKeyPair, Secret};
    use serde_json::{json, Value as Json};

    const POLYGON_CHAIN_ID: u64 = 137;

    fn answer(method: &str, params: &Json, sent: &Mutex<Vec<Vec<u8>>>) -> Json {
        match method {
            "parity_nextNonce" | "eth_getTransactionCount" => {
                // The send path polls until the node reports the nonce moved past the used one.
                json!(if sent.lock().unwrap().is_empty() { "0x0" } else { "0x1" })
            },
            "eth_gasPrice" => json!("0x3b9aca00"),
            "eth_feeHistory" => {
                let count = match &params[0] {
                    Json::String(s) => u64::from_str_radix(s.trim_start_matches("0x"), 16).unwrap(),
                    other => other.as_u64().unwrap(),
                } as usize;
                let percentiles = params[2].as_array().map(|p| p.len()).unwrap_or(0);
                json!({
                    "oldestBlock": "0x1",
                    "baseFeePerGas": vec!["0x2540be400"; count + 1],
                    "gasUsedRatio": vec![0.5; count],
                    "reward": vec![vec!["0x3b9aca00"; percentiles]; count],
                })
            },
            "eth_sendRawTransaction" => {
                let raw = params[0].as_str().unwrap().trim_start_matches("0x");
                sent.lock().unwrap().push(hex::decode(raw).unwrap());
                json!(format!("0x{}", "11".repeat(32)))
            },
            other => panic!("the scripted node does not serve `{}`", other),
        }
    }

    fn spawn_send_node() -> (ScriptedNode, Arc<Mutex<Vec<Vec<u8>>>>) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let sent_h = sent.clone();
        (spawn(move |method, params| answer(method, params, &sent_h)), sent)
    }

    /// An ERC-20 token coin on a chain-137 platform with `policy` as its stored swap gas-fee policy. The
    /// platform carries `"max_eth_tx_type": 2`, as every shipped coin with a non-legacy policy does.
    fn polygon_token(url: &str, policy: SwapGasFeePolicy, token_addr: Address) -> (MmArc, EthCoin) {
        polygon_token_with_conf(
            url,
            policy,
            token_addr,
            json!([{"coin": "MATIC", "max_eth_tx_type": 2}]),
            json!({}),
        )
    }

    /// As [`polygon_token`], with `extra_coins` appended to the coins file and `node_conf` merged into the
    /// node configuration (compatibility switches).
    fn polygon_token_with_conf(
        url: &str,
        policy: SwapGasFeePolicy,
        token_addr: Address,
        extra_coins: Json,
        node_conf: Json,
    ) -> (MmArc, EthCoin) {
        let key_pair = EthKeyPair::from_secret_slice(
            &hex::decode("809465b17d0a4ddb3e4c69e8f23c2cabad868f51f8bed5c765ad1d6516c3306f").unwrap(),
        )
        .unwrap();
        let web3 = crate::eth::alloy_compat::build_provider(
            crate::node_url::listed_nodes("TEST", vec![url.to_owned()]),
            vec![],
        )
        .unwrap();
        let mut coins = vec![
            json!({"coin": "USDC-PLG20", "protocol": {"type": "ERC20", "protocol_data":
                {"platform": "MATIC", "contract_address": format!("{:#x}", token_addr)}}}),
        ];
        coins.extend(extra_coins.as_array().unwrap().iter().cloned());
        let mut conf = json!({
            "netid": mm2_net_config::SUPPORTED_NETIDS[0],
            "coins": coins,
        });
        for (k, v) in node_conf.as_object().unwrap() {
            conf[k] = v.clone();
        }
        let ctx = MmCtxBuilder::new().with_conf(conf).into_mm_arc();
        let my_address = public_to_address(&key_pair.public());
        let coin = EthCoin(Arc::new(EthCoinImpl {
            coin_type: EthCoinType::Erc20 {
                platform: "MATIC".to_owned(),
                token_addr,
            },
            decimals: 6,
            gas_station_url: None,
            gas_station_decimals: ETH_GAS_STATION_DECIMALS,
            history_sync_state: Mutex::new(HistorySyncState::NotEnabled),
            gas_station_policy: GasStationPricePolicy::MeanAverageFast,
            my_address,
            sign_message_prefix: None,
            signer: EthSigner::Local(key_pair),
            swap_contract_address: Address::from_str("9130b257d37a52e52f21054c4da3450c72f595ce").unwrap(),
            fallback_swap_contract: None,
            ticker: "USDC-PLG20".to_owned(),
            web3_instances: vec![Web3Instance {
                web3: web3.clone(),
                is_parity: true,
            }],
            web3,
            ctx: ctx.weak(),
            required_confirmations: 1.into(),
            tron_api: None,
            nft_swap_v2_contract: None,
            swap_gas_fee_policy: Mutex::new(policy),
            erc20_tokens_infos: Default::default(),
            custom_token: false,
            chain_id: Some(POLYGON_CHAIN_ID),
            logs_block_range: DEFAULT_LOGS_BLOCK_RANGE,
            derivation_method: DerivationMethod::Iguana(my_address),
            swap_v2_contracts: None,
            gas_limit: EthGasLimit::default(),
            gas_limit_v2: EthGasLimitV2::default(),
            estimate_gas_mult: None,
        }));
        (ctx, coin)
    }

    /// The counterparty's `erc20Payment` transaction (legacy form) that the spend refers to.
    fn counterparty_payment(coin: &EthCoin, token_addr: Address) -> Vec<u8> {
        let function = SWAP_CONTRACT.function("erc20Payment").unwrap();
        let data = function
            .encode_input(&[
                Token::FixedBytes(vec![7u8; 32]),
                Token::Uint(U256::from(1_000_000u64)),
                Token::Address(token_addr),
                Token::Address(coin.my_address),
                Token::FixedBytes(vec![9u8; 20]),
                Token::Uint(U256::from(1_800_000_000u64)),
            ])
            .unwrap();
        let unsigned = UnSignedEthTx {
            nonce: U256::from(3u64),
            gas_price: U256::from(1_000_000_000u64),
            gas: U256::from(150_000u64),
            action: Action::Call(coin.swap_contract_address),
            value: U256::zero(),
            data,
        };
        let counterparty =
            Secret::from_str("4646464646464646464646464646464646464646464646464646464646464646").unwrap();
        rlp::encode(&unsigned.sign(&counterparty, Some(POLYGON_CHAIN_ID))).to_vec()
    }

    /// Drives one swap send and returns the bytes the node received.
    fn broadcast(policy: SwapGasFeePolicy, send: impl Fn(&EthCoin, &[u8]) -> TransactionFut) -> Vec<u8> {
        EthCoin::payment_status
            .mock_safe(|_, _, _| MockResult::Return(Box::new(futures01::future::ok(PAYMENT_STATE_SENT.into()))));
        let (node, sent) = spawn_send_node();
        let token_addr = Address::from_str("3c499c542cef5e3811e1192ce70d8cc03d5c3359").unwrap();
        let (_ctx, coin) = polygon_token(&node.url, policy, token_addr);
        let payment = counterparty_payment(&coin, token_addr);
        block_on(send(&coin, &payment).compat()).expect("the swap send succeeds");
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 1, "exactly one transaction is broadcast");
        sent[0].clone()
    }

    fn spend_maker_payment(coin: &EthCoin, payment: &[u8]) -> TransactionFut {
        coin.send_taker_spends_maker_payment(
            payment,
            0,
            &[],
            &[5u8; 32],
            &[],
            &Some(BytesJson::from(coin.swap_contract_address.as_bytes().to_vec())),
        )
    }

    fn refund_own_payment(coin: &EthCoin, payment: &[u8]) -> TransactionFut {
        coin.send_taker_refunds_payment(
            payment,
            0,
            &[],
            &[],
            &[],
            &Some(BytesJson::from(coin.swap_contract_address.as_bytes().to_vec())),
        )
    }

    fn assert_untyped_legacy(raw: &[u8]) {
        assert!(
            raw[0] >= 0xc0,
            "legacy form leads with an RLP list header, got 0x{:02x}",
            raw[0]
        );
        let decoded = signed_eth_tx_from_bytes(raw).unwrap();
        assert!(matches!(decoded.kind, TxKind::Legacy { .. }));
    }

    fn assert_type_2(raw: &[u8]) {
        assert_eq!(raw[0], 0x02, "a tier must emit the EIP-1559 envelope byte");
        let decoded = signed_eth_tx_from_bytes(raw).unwrap();
        match &decoded.kind {
            TxKind::Typed(t) => {
                assert_eq!(t.tx_type, 2);
                assert_eq!(t.chain_id, POLYGON_CHAIN_ID);
            },
            TxKind::Legacy { .. } => panic!("decoded as legacy"),
        }
    }

    #[test]
    fn legacy_policy_spend_of_the_counterpartys_payment_is_untyped() {
        assert_untyped_legacy(&broadcast(SwapGasFeePolicy::Legacy, spend_maker_payment));
    }

    #[test]
    fn every_tier_spend_of_the_counterpartys_payment_is_type_2() {
        for policy in [SwapGasFeePolicy::Low, SwapGasFeePolicy::Medium, SwapGasFeePolicy::High] {
            assert_type_2(&broadcast(policy, spend_maker_payment));
        }
    }

    #[test]
    fn refund_follows_the_policy_form() {
        assert_untyped_legacy(&broadcast(SwapGasFeePolicy::Legacy, refund_own_payment));
        assert_type_2(&broadcast(SwapGasFeePolicy::Medium, refund_own_payment));
    }

    // ── CRD D35.7: the per-coin cap on the emitted transaction type ─────────────────────────────

    /// Drives one swap send and returns its outcome with every transaction the node received.
    fn try_broadcast(
        policy: SwapGasFeePolicy,
        extra_coins: Json,
        node_conf: Json,
    ) -> (Result<(), String>, Vec<Vec<u8>>) {
        EthCoin::payment_status
            .mock_safe(|_, _, _| MockResult::Return(Box::new(futures01::future::ok(PAYMENT_STATE_SENT.into()))));
        let (node, sent) = spawn_send_node();
        let token_addr = Address::from_str("3c499c542cef5e3811e1192ce70d8cc03d5c3359").unwrap();
        let (_ctx, coin) = polygon_token_with_conf(&node.url, policy, token_addr, extra_coins, node_conf);
        let payment = counterparty_payment(&coin, token_addr);
        let result = block_on(spend_maker_payment(&coin, &payment).compat())
            .map(|_| ())
            .map_err(|e| format!("{:?}", e));
        let sent = sent.lock().unwrap().clone();
        (result, sent)
    }

    fn platform_with_cap(cap: Json) -> Json { json!([{"coin": "MATIC", "max_eth_tx_type": cap}]) }

    fn assert_refused_without_broadcast(outcome: (Result<(), String>, Vec<Vec<u8>>)) -> String {
        let (result, sent) = outcome;
        let err = result.expect_err("a type above the coin's maximum must be refused at signing");
        assert!(err.contains("refusing to sign a type-2 transaction"), "{err}");
        assert!(
            sent.is_empty(),
            "nothing is broadcast, and in particular no lower type is substituted"
        );
        err
    }

    #[test]
    fn a_sufficient_cap_lets_the_typed_send_proceed() {
        for cap in [2, 3] {
            let (result, sent) = try_broadcast(SwapGasFeePolicy::Medium, platform_with_cap(json!(cap)), json!({}));
            result.expect("the send succeeds");
            assert_eq!(sent.len(), 1);
            assert_type_2(&sent[0]);
        }
    }

    #[test]
    fn an_insufficient_cap_refuses_the_typed_send_at_signing() {
        for cap in [0, 1] {
            let err = assert_refused_without_broadcast(try_broadcast(
                SwapGasFeePolicy::Medium,
                platform_with_cap(json!(cap)),
                json!({}),
            ));
            assert!(err.contains(&format!("is {}", cap)), "{err}");
        }
    }

    #[test]
    fn the_cap_does_not_affect_an_untyped_send() {
        let (result, sent) = try_broadcast(SwapGasFeePolicy::Legacy, platform_with_cap(json!(0)), json!({}));
        result.expect("the send succeeds");
        assert_untyped_legacy(&sent[0]);
    }

    #[test]
    fn an_absent_cap_is_refused_by_default() {
        // The token and its platform are both silent: the default matches the reference, where the absent
        // key reads as 0.
        let absent = assert_refused_without_broadcast(try_broadcast(
            SwapGasFeePolicy::Medium,
            json!([{"coin": "MATIC"}]),
            json!({}),
        ));
        // The refusal is the one an explicit cap of 0 gets, down to the stated maximum.
        let explicit_zero = assert_refused_without_broadcast(try_broadcast(
            SwapGasFeePolicy::Medium,
            platform_with_cap(json!(0)),
            json!({}),
        ));
        assert!(
            absent.contains("is 0 ") && explicit_zero.contains("is 0 "),
            "{absent} / {explicit_zero}"
        );
        // An untyped send is unaffected by the default.
        let (result, sent) = try_broadcast(SwapGasFeePolicy::Legacy, json!([{"coin": "MATIC"}]), json!({}));
        result.expect("the send succeeds");
        assert_untyped_legacy(&sent[0]);
    }

    #[test]
    fn the_switch_makes_an_absent_cap_unrestricted() {
        let switch = json!({ "max_eth_tx_type_absent_unrestricted": true });
        let (result, sent) = try_broadcast(SwapGasFeePolicy::Medium, json!([{"coin": "MATIC"}]), switch.clone());
        result.expect("the send succeeds");
        assert_type_2(&sent[0]);
        // An untyped send is unaffected by the switch.
        let (result, sent) = try_broadcast(SwapGasFeePolicy::Legacy, json!([]), switch.clone());
        result.expect("the send succeeds");
        assert_untyped_legacy(&sent[0]);
        // The switch never overrides a key that is present.
        assert_refused_without_broadcast(try_broadcast(
            SwapGasFeePolicy::Medium,
            platform_with_cap(json!(0)),
            switch.clone(),
        ));
        let (result, sent) = try_broadcast(SwapGasFeePolicy::Medium, platform_with_cap(json!(2)), switch);
        result.expect("the send succeeds");
        assert_type_2(&sent[0]);
    }

    #[test]
    fn anything_but_the_boolean_true_leaves_the_switch_off() {
        for off in [json!("true"), json!(1), json!(false), json!(null)] {
            assert_refused_without_broadcast(try_broadcast(
                SwapGasFeePolicy::Medium,
                json!([]),
                json!({ "max_eth_tx_type_absent_unrestricted": off }),
            ));
        }
    }

    /// The token coin (USDC-PLG20) has no `max_eth_tx_type` of its own; its platform (MATIC) does. The
    /// precedence of a token's own entry over the platform's is pinned by `resolve_max_eth_tx_type`.
    #[test]
    fn a_token_inherits_its_platforms_cap() {
        assert_refused_without_broadcast(try_broadcast(
            SwapGasFeePolicy::High,
            platform_with_cap(json!(0)),
            json!({}),
        ));
        let (result, sent) = try_broadcast(SwapGasFeePolicy::High, platform_with_cap(json!(2)), json!({}));
        result.expect("the send succeeds");
        assert_type_2(&sent[0]);
    }
}

/// Tests that used to talk to live EVM nodes and now run the same real code against a scripted local
/// node (see [`scripted_evm_node`]). Do not point them back at a network: the hosts and the Ropsten
/// chain they used are gone, and a keyless public endpoint refuses the archive reads such tests need.
mod scripted_node_ports {
    use super::scripted_evm_node::spawn;
    use super::*;
    use mm2_eth::keys::{KeyPair as EthKeyPair, Secret};
    use serde_json::{json, Value as Json};
    use std::collections::HashMap;

    const TEST_KEY: &str = "809465b17d0a4ddb3e4c69e8f23c2cabad868f51f8bed5c765ad1d6516c3306f";
    const TOKEN_ADDR: &str = "a1d6df714f91debf4e0802a542e13067f31b8262";
    const FEE_BLOCK: u64 = 1_000;
    const ETH_FEE: &str = "0.000526435076465";
    const ERC20_FEE: &str = "5.548262548262548262";

    fn fee_addr() -> Address { addr_from_raw_pubkey(test_dex_fee_addr_raw_pubkey()).unwrap() }

    /// The `eth_getTransactionByHash` result a node reports for a legacy transaction mined in `block`.
    fn rpc_tx(tx: &SignedEthTx, block: u64) -> Json {
        let to = match &tx.action {
            Action::Call(addr) => json!(format!("{:#x}", addr)),
            Action::Create => Json::Null,
        };
        json!({
            "type": "0x0",
            "hash": format!("{:#x}", tx.tx_hash()),
            "nonce": format!("{:#x}", tx.nonce),
            "blockHash": format!("0x{}", "22".repeat(32)),
            "blockNumber": format!("{:#x}", block),
            "transactionIndex": "0x0",
            "from": format!("{:#x}", tx.sender()),
            "to": to,
            "value": format!("{:#x}", tx.value),
            "gas": format!("{:#x}", tx.gas),
            "gasPrice": format!("{:#x}", tx.gas_price().expect("a legacy transaction")),
            "input": format!("0x{}", hex::encode(&tx.data)),
            "v": format!("{:#x}", tx.v),
            "r": format!("{:#x}", tx.r),
            "s": format!("{:#x}", tx.s),
        })
    }

    /// A dex-fee transaction signed by the test key, as the fee payer would have sent it:
    /// a plain transfer for ETH, `transfer(fee_addr, amount)` on the token contract for an ERC20.
    fn fee_tx(erc20: bool) -> SignedEthTx {
        let (to, value, data) = if erc20 {
            let amount = wei_from_big_decimal(&ERC20_FEE.parse().unwrap(), 18).unwrap();
            let data = ERC20_CONTRACT
                .function("transfer")
                .unwrap()
                .encode_input(&[Token::Address(fee_addr()), Token::Uint(amount)])
                .unwrap();
            (Address::from_str(TOKEN_ADDR).unwrap(), U256::zero(), data)
        } else {
            let amount = wei_from_big_decimal(&ETH_FEE.parse().unwrap(), 18).unwrap();
            (fee_addr(), amount, vec![])
        };
        let secret = Secret::from_str(TEST_KEY).unwrap();
        let tx = SwapTx::new(
            SwapFee::Legacy {
                gas_price: U256::from(50_000_000_000u64),
            },
            U256::from(7u64),
            U256::from(100_000u64),
            Action::Call(to),
            value,
            data,
        );
        match tx {
            SwapTx::Legacy(tx) => tx.sign(&secret, Some(1)),
            SwapTx::Eip1559(_) => unreachable!("a legacy fee was given"),
        }
    }

    /// Runs `validate_fee` for `fee_tx(erc20)` against a node that reports it mined in `FEE_BLOCK`.
    /// `expected_sender` is `None` for the sender that really signed it.
    fn validate(erc20: bool, wrong_sender: bool, min_block_number: u64) -> Result<(), String> {
        let tx = fee_tx(erc20);
        let reported = rpc_tx(&tx, FEE_BLOCK);
        let hash = format!("{:#x}", tx.tx_hash());
        // `validate_fee` reads the transaction by hash and nothing else.
        let node = spawn(move |method, params| match method {
            "eth_getTransactionByHash" => {
                assert_eq!(params[0].as_str().unwrap(), hash);
                reported.clone()
            },
            other => panic!("the scripted node does not serve `{}`", other),
        });
        let coin_type = if erc20 {
            EthCoinType::Erc20 {
                platform: "ETH".to_string(),
                token_addr: Address::from_str(TOKEN_ADDR).unwrap(),
            }
        } else {
            EthCoinType::Eth
        };
        let (_ctx, coin) = eth_coin_for_test(coin_type, vec![node.url.clone()], None);
        // As the original tests did: the transaction is fetched back through the node's own answer.
        let fetched = {
            use alloy::providers::Provider as _;
            let hash = alloy::primitives::B256::from_slice(tx.tx_hash().as_bytes());
            let alloy_tx = block_on(coin.web3.get_transaction_by_hash(hash)).unwrap().unwrap();
            signed_tx_from_alloy_tx(alloy_tx).unwrap()
        };
        let sender = sender_compressed_pub(&fetched);
        let fetched = fetched.into();
        let amount: BigDecimal = if erc20 { ERC20_FEE } else { ETH_FEE }.parse().unwrap();
        let expected_sender: &[u8] = if wrong_sender {
            test_dex_fee_addr_raw_pubkey()
        } else {
            &sender
        };
        coin.validate_fee(ValidateFeeArgs {
            fee_tx: &fetched,
            expected_sender,
            fee_addr: test_dex_fee_addr_raw_pubkey(),
            dex_fee: &DexFee::Standard(amount.into()),
            min_block_number,
            uuid: &[],
        })
        .wait()
    }

    // The four tests below used to fetch two real mainnet dex-fee transactions through a hosted
    // Infura project and a community node; both hosts are gone or keyless-refused. Each now signs an
    // equivalent fee transaction with the test key and serves it from a scripted node.

    #[test]
    fn validate_dex_fee_invalid_sender_eth() {
        let err = validate(false, true, 0).unwrap_err();
        assert!(err.contains("was sent from wrong address"), "{err}");
    }

    #[test]
    fn validate_dex_fee_invalid_sender_erc() {
        let err = validate(true, true, 0).unwrap_err();
        assert!(err.contains("was sent from wrong address"), "{err}");
    }

    #[test]
    fn validate_dex_fee_eth_confirmed_before_min_block() {
        // Mined in FEE_BLOCK, so a min_block at or above it must be rejected, one below it accepted.
        let err = validate(false, false, FEE_BLOCK + 1).unwrap_err();
        assert!(err.contains("confirmed before min_block"), "{err}");
        let err = validate(false, false, FEE_BLOCK).unwrap_err();
        assert!(err.contains("confirmed before min_block"), "{err}");
        validate(false, false, FEE_BLOCK - 1).unwrap();
    }

    #[test]
    fn validate_dex_fee_erc_confirmed_before_min_block() {
        let err = validate(true, false, FEE_BLOCK + 1).unwrap_err();
        assert!(err.contains("confirmed before min_block"), "{err}");
        let err = validate(true, false, FEE_BLOCK).unwrap_err();
        assert!(err.contains("confirmed before min_block"), "{err}");
        validate(true, false, FEE_BLOCK - 1).unwrap();
    }

    /// Several transactions sent at once must each get a distinct, consecutive nonce.
    ///
    /// The original sent five transactions at once to a public dev node and only checked that every
    /// send succeeded and the nonce lookup was logged, which cannot detect a nonce collision on its
    /// own. The scripted node here behaves like a real one in the one respect that matters: its
    /// next nonce moves only when a transaction is actually broadcast. Without `NONCE_LOCK` the five
    /// concurrent sends would all read nonce 0; the assertion is that the nodes saw 0..5 exactly once.
    #[test]
    fn test_nonce_lock() {
        use futures::future::join_all;
        use mm2_test_helpers::for_tests::wait_for_log;

        let sent: Arc<std::sync::Mutex<Vec<u64>>> = Arc::default();
        let sent_h = sent.clone();
        let node = spawn(move |method, params| match method {
            "parity_nextNonce" | "eth_getTransactionCount" => json!(format!("{:#x}", sent_h.lock().unwrap().len())),
            "eth_gasPrice" => json!("0x3b9aca00"),
            "eth_feeHistory" => json!({
                "oldestBlock": "0x1",
                "baseFeePerGas": ["0x3b9aca00", "0x3b9aca00"],
                "gasUsedRatio": [0.5],
                "reward": [[]],
            }),
            "eth_sendRawTransaction" => {
                let raw = hex::decode(params[0].as_str().unwrap().trim_start_matches("0x")).unwrap();
                let tx = signed_eth_tx_from_bytes(&raw).unwrap();
                sent_h.lock().unwrap().push(tx.nonce.as_u64());
                json!(format!("{:#x}", tx.tx_hash()))
            },
            other => panic!("the scripted node does not serve `{}`", other),
        });
        let (ctx, coin) = eth_coin_for_test(EthCoinType::Eth, vec![node.url.clone()], None);
        let futures: Vec<_> = (0..5)
            .map(|_| {
                sign_and_send_transaction_impl(
                    ctx.clone(),
                    coin.clone(),
                    1000000000000u64.into(),
                    Action::Call(coin.my_address),
                    vec![],
                    21000.into(),
                )
            })
            .collect();
        for result in block_on(join_all(futures)) {
            result.unwrap();
        }
        let mut nonces = sent.lock().unwrap().clone();
        nonces.sort_unstable();
        assert_eq!(nonces, vec![0, 1, 2, 3, 4], "each transaction must use its own nonce");
        block_on(wait_for_log(&ctx, 1.1, |line| line.contains("get_addr_nonce…"))).unwrap();
    }

    /// `get_addr_nonce` asks every configured node and answers with the highest nonce (CRD ch.35 R35.1.12).
    ///
    /// The original (`#[ignore]`d, Ropsten via a hosted project key, a gas station that is gone, and a
    /// real broadcast) only logged the nonce afterwards. What it was after, several URLs of which one
    /// is dead, is checked here: one node lags and one never answers. The lagging node must not hold the
    /// answer: the highest value wins in the first round.
    #[test]
    fn test_nonce_several_urls() {
        let lagging_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let lag = lagging_calls.clone();
        let behind = spawn(move |method, _| match method {
            "eth_getTransactionCount" => {
                lag.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                json!("0x4")
            },
            other => panic!("the scripted node does not serve `{}`", other),
        });
        let ahead = spawn(|method, _| match method {
            "eth_getTransactionCount" => json!("0x5"),
            other => panic!("the scripted node does not serve `{}`", other),
        });
        // A closed loopback port stands in for the node that never answers.
        let dead_url = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            format!("http://127.0.0.1:{}", l.local_addr().unwrap().port())
        };
        let instances: Vec<Web3Instance> = [behind.url.clone(), ahead.url.clone(), dead_url]
            .into_iter()
            .map(|url| Web3Instance {
                web3: crate::eth::alloy_compat::build_provider(
                    crate::node_url::listed_nodes("TEST", vec![url]),
                    vec![],
                )
                .unwrap(),
                is_parity: false,
            })
            .collect();
        let key = EthKeyPair::from_secret_slice(&hex::decode(TEST_KEY).unwrap()).unwrap();
        let nonce = get_addr_nonce(key.address(), instances).wait().unwrap();
        assert_eq!(nonce, U256::from(5u64));
        assert_eq!(
            lagging_calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the lagging node is asked once; its disagreement forces no second round"
        );
    }
}

/// ch.57 T10 (R57.7A.1, R57.7A.3): an EVM coin's minimum trading volume is exactly one smallest unit,
/// 10^-decimals, and its minimum transaction amount is exactly zero.
#[test]
fn t10_evm_min_trading_vol_is_one_smallest_unit() {
    let erc20 = || EthCoinType::Erc20 {
        platform: "ETH".to_string(),
        token_addr: Address::from_slice(&hex::decode("2b294F029Fde858b2c62184e8390591755521d8E").unwrap()),
    };
    // 24 > 19: `10u64.pow(24)` would overflow, so this pins the absence of a machine-integer power.
    for decimals in [18u8, 8, 6, 24] {
        for coin_type in [EthCoinType::Eth, erc20()] {
            let (_ctx, coin) = eth_coin_for_test_with_netid_and_decimals(
                coin_type,
                vec!["http://dummy.dummy".into()],
                None,
                mm2_net_config::SUPPORTED_NETIDS[0],
                decimals,
            );
            let expected = MmNumber::from(mm2_number::BigRational::new(1.into(), num_bigint_pow10(decimals)));
            assert_eq!(coin.min_trading_vol(), expected, "decimals {decimals}");
            assert_eq!(coin.min_tx_amount(), BigDecimal::from(0), "decimals {decimals}");
        }
    }

    let (_ctx, coin8) = eth_coin_for_test_with_netid_and_decimals(
        erc20(),
        vec!["http://dummy.dummy".into()],
        None,
        mm2_net_config::SUPPORTED_NETIDS[0],
        8,
    );
    // The baseline integer-division formula yields 10^-2 for eight decimals; it must not come back.
    assert_ne!(coin8.min_trading_vol(), MmNumber::from((1, 100)));
    assert_eq!(coin8.min_trading_vol(), MmNumber::from((1, 100_000_000)));
}

fn num_bigint_pow10(decimals: u8) -> mm2_number::BigInt {
    let mut v = mm2_number::BigInt::from(1);
    for _ in 0..decimals {
        v *= 10;
    }
    v
}

/// CRD ch.57 R57.2.1 (the node queries of one gas-price resolution are concurrent) and R57.2.6 item 1 (one
/// resolution per coin and order-posting request), against a scripted node whose queries take time.
mod gas_price_resolution {
    use super::scripted_evm_node::spawn;
    use super::*;
    use crate::request_fee_scope::with_request_fee_scope;
    use serde_json::{json, Value as Json};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    const NODE_LATENCY: Duration = Duration::from_millis(400);
    const GAS_PRICE_WEI: u64 = 1_000_000_000;
    const BASE_FEE_WEI: u64 = 10_000_000_000;

    #[derive(Default)]
    struct Calls {
        gas_price: AtomicUsize,
        fee_history: AtomicUsize,
    }

    /// A node answering both gas-price sources after [`NODE_LATENCY`]. A source in `broken` answers a value
    /// that is not a result of that method.
    fn node(calls: Arc<Calls>, broken: &'static [&'static str]) -> super::scripted_evm_node::ScriptedNode {
        spawn(move |method, _params| {
            std::thread::sleep(NODE_LATENCY);
            match method {
                "eth_gasPrice" => {
                    calls.gas_price.fetch_add(1, Ordering::SeqCst);
                    if broken.contains(&method) {
                        return json!("not a price");
                    }
                    json!(format!("{:#x}", GAS_PRICE_WEI))
                },
                "eth_feeHistory" => {
                    calls.fee_history.fetch_add(1, Ordering::SeqCst);
                    if broken.contains(&method) {
                        return json!("not a fee history");
                    }
                    json!({
                        "oldestBlock": "0x1",
                        "baseFeePerGas": [format!("{:#x}", BASE_FEE_WEI), format!("{:#x}", BASE_FEE_WEI)],
                        "gasUsedRatio": [0.5],
                    })
                },
                other => panic!("the scripted node does not serve `{}`", other),
            }
        })
    }

    fn coin_on(node: &super::scripted_evm_node::ScriptedNode) -> EthCoin {
        eth_coin_for_test(EthCoinType::Eth, vec![node.url.clone()], None).1
    }

    fn fee_history_price() -> U256 { increase_by_percent(U256::from(BASE_FEE_WEI), BASE_BLOCK_FEE_DIFF_PCT) }

    /// R57.2.1: two queries of N ms each resolve in about N ms, not 2N, and the maximum wins.
    #[test]
    fn the_node_queries_are_concurrent_and_the_maximum_wins() {
        let calls = Arc::new(Calls::default());
        let node = node(calls.clone(), &[]);
        let coin = coin_on(&node);

        let started = Instant::now();
        let price = block_on(coin.get_gas_price().compat()).unwrap();
        let elapsed = started.elapsed();

        assert_eq!(price, fee_history_price());
        assert!(price > U256::from(GAS_PRICE_WEI));
        assert!(
            elapsed < NODE_LATENCY * 2 - Duration::from_millis(100),
            "the two queries ran one after another: {elapsed:?}"
        );
        assert_eq!(calls.gas_price.load(Ordering::SeqCst), 1);
        assert_eq!(calls.fee_history.load(Ordering::SeqCst), 1);
    }

    /// R57.2.1: a source that errors is dropped; with none left the resolution fails.
    #[test]
    fn a_failing_source_is_dropped_and_no_source_is_an_error() {
        let calls = Arc::new(Calls::default());
        let node_without_history = node(calls.clone(), &["eth_feeHistory"]);
        let price = block_on(coin_on(&node_without_history).get_gas_price().compat()).unwrap();
        assert_eq!(price, U256::from(GAS_PRICE_WEI));

        let node_without_price = node(calls.clone(), &["eth_gasPrice"]);
        let price = block_on(coin_on(&node_without_price).get_gas_price().compat()).unwrap();
        assert_eq!(price, fee_history_price());

        let dead_node = node(calls, &["eth_gasPrice", "eth_feeHistory"]);
        let err = block_on(coin_on(&dead_node).get_gas_price().compat()).unwrap_err();
        assert!(matches!(err.into_inner(), Web3RpcError::Internal(m) if m == "All requests failed"));
    }

    /// R57.2.7: the platform coin and a token of it share one gas-price reading, within a request and across
    /// requests of the node; outside a request each resolves afresh.
    #[test]
    fn a_platform_and_its_token_share_one_gas_price_reading() {
        let calls = Arc::new(Calls::default());
        let node = node(calls.clone(), &[]);
        let platform = coin_on(&node);
        let token = eth_coin_for_test(
            EthCoinType::Erc20 {
                platform: "ETH".to_owned(),
                token_addr: Address::from_slice(&[7; 20]),
            },
            vec![node.url.clone()],
            None,
        )
        .1;
        assert_eq!((platform.ticker.as_str(), token.ticker.as_str()), ("ETH", "JST"));
        // The node whose cache is used: its coins file lists the token under its platform.
        let ctx = mm2_core::mm_ctx::MmCtxBuilder::new()
            .with_conf(json!({"coins": [
                {"coin": "ETH", "protocol": {"type": "ETH"}},
                {"coin": "JST", "protocol": {"type": "ERC20", "protocol_data": {"platform": "ETH"}}},
            ]}))
            .into_mm_arc();

        // Within one request the token asks first, then the platform.
        let (from_token, from_platform) = block_on(with_request_fee_scope(&ctx, async {
            let token_price = token.get_gas_price().compat().await.unwrap();
            let platform_price = platform.get_gas_price().compat().await.unwrap();
            (token_price, platform_price)
        }));
        assert_eq!((from_token, from_platform), (fee_history_price(), fee_history_price()));
        assert_eq!(calls.gas_price.load(Ordering::SeqCst), 1);
        assert_eq!(calls.fee_history.load(Ordering::SeqCst), 1);

        // A later request, platform first this time: the same reading.
        block_on(with_request_fee_scope(&ctx, async {
            platform.get_gas_price().compat().await.unwrap();
            token.get_gas_price().compat().await.unwrap();
        }));
        assert_eq!(calls.gas_price.load(Ordering::SeqCst), 1);

        // Outside a request (swap execution) every coin resolves afresh.
        block_on(async {
            token.get_gas_price().compat().await.unwrap();
            platform.get_gas_price().compat().await.unwrap();
        });
        assert_eq!(calls.gas_price.load(Ordering::SeqCst), 3);
        assert_eq!(calls.fee_history.load(Ordering::SeqCst), 3);
    }

    /// R57.2.6 item 1: inside one order-posting request the sender fee, the receiver fee and the maximum volume
    /// resolve the gas price once; outside a request (swap execution) every consumer resolves it afresh.
    #[test]
    fn the_gas_price_is_resolved_once_per_order_posting_request() {
        let calls = Arc::new(Calls::default());
        let node = node(calls.clone(), &[]);
        let coin = coin_on(&node);

        let prices = block_on(with_request_fee_scope(
            &mm2_core::mm_ctx::MmCtxBuilder::default().into_mm_arc(),
            async {
                let sender = coin.get_gas_price().compat().await.unwrap();
                let receiver = coin.get_gas_price().compat().await.unwrap();
                let max_volume = coin.get_gas_price().compat().await.unwrap();
                (sender, receiver, max_volume)
            },
        ));
        assert_eq!(prices, (fee_history_price(), fee_history_price(), fee_history_price()));
        assert_eq!(calls.gas_price.load(Ordering::SeqCst), 1);
        assert_eq!(calls.fee_history.load(Ordering::SeqCst), 1);

        // The same three consumers outside a request, as in swap execution, read three times.
        block_on(async {
            for _ in 0..3 {
                coin.get_gas_price().compat().await.unwrap();
            }
        });
        assert_eq!(calls.gas_price.load(Ordering::SeqCst), 4);
        assert_eq!(calls.fee_history.load(Ordering::SeqCst), 4);
    }
}
