use super::*;
use crate::lp_coininit;
use crypto::CryptoCtx;
use mm2_core::mm_ctx::MmCtxBuilder;
use wasm_bindgen_test::*;
use web_sys::console;

/// Test-only DEX-fee destination pubkey, resolved through `mm2_net_config`
/// for the community netid. Replaces direct use of
/// `common::DEX_FEE_ADDR_RAW_PUBKEY` so WASM test fixtures go through the
/// same per-netid registry as production code (LP-3F.A1).
fn test_dex_fee_addr_raw_pubkey() -> &'static [u8] {
    mm2_net_config::net_config_or_panic(8762).dex_fee_addr_raw_pubkey()
}

wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
fn pass() {
    let ctx = MmCtxBuilder::default().into_mm_arc();
    let _coins_context = CoinsContext::from_ctx(&ctx).unwrap();
    assert_eq!(1, 1);
}

#[wasm_bindgen_test]
async fn test_send() {
    let key_pair = KeyPair::from_secret_slice(
        &hex::decode("809465b17d0a4ddb3e4c69e8f23c2cabad868f51f8bed5c765ad1d6516c3306f").unwrap(),
    )
    .unwrap();
    let web3 = crate::eth::alloy_compat::build_provider(vec!["http://195.201.0.6:8565".into()], vec![]).unwrap();
    let ctx = MmCtxBuilder::new().into_mm_arc();
    let coin = EthCoin(Arc::new(EthCoinImpl {
        ticker: "ETH".into(),
        coin_type: EthCoinType::Eth,
        my_address: key_pair.address(),
        sign_message_prefix: Some(String::from("Ethereum Signed Message:\n")),
        signer: EthSigner::Local(key_pair.clone()),
        swap_contract_address: Address::from_str("7Bc1bBDD6A0a722fC9bffC49c921B685ECB84b94").unwrap(),
        fallback_swap_contract: None,
        swap_v2_contracts: None,
        gas_limit_v2: EthGasLimitV2::default(),
        derivation_method: DerivationMethod::Iguana(key_pair.address()),
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
        chain_id: None,
        logs_block_range: DEFAULT_LOGS_BLOCK_RANGE,
        tron_api: None,
        nft_swap_v2_contract: None,
        swap_gas_fee_policy: Mutex::new(SwapGasFeePolicy::default()),
        erc20_tokens_infos: Default::default(),
    }));
    let tx = coin
        .send_maker_payment(
            1000,
            &[],
            test_dex_fee_addr_raw_pubkey(),
            &[1; 20],
            "0.001".parse().unwrap(),
            &None,
        )
        .compat()
        .await;
    console::log_1(&format!("{:?}", tx).into());

    let block = coin.current_block().compat().await;
    console::log_1(&format!("{:?}", block).into());
}

#[wasm_bindgen_test]
async fn test_init_eth_coin() {
    let conf = json!({
        "coins": [{
            "coin": "ETH",
            "name": "ethereum",
            "fname": "Ethereum",
            "protocol":{
                "type": "ETH"
            },
            "rpcport": 80,
            "mm2": 1
        }]
    });

    let ctx = MmCtxBuilder::new().with_conf(conf).into_mm_arc();
    CryptoCtx::init_with_iguana_passphrase(
        ctx.clone(),
        "spice describe gravity federal blast come thank unfair canal monkey style afraid",
    )
    .unwrap();

    let req = json!({
        "urls":["http://195.201.0.6:8565"],
        "swap_contract_address":"0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
    });
    let _coin = lp_coininit(&ctx, "ETH", &req).await.unwrap();
}
