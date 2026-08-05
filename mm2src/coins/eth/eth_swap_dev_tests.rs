//! Local `geth --dev` integration tests for EVM: the v1 ETH/ERC20 HTLC swap
//! payment + refund path (`send_maker_payment` -> `send_maker_refunds_payment`)
//! and plain ETH/ERC20 `withdraw`.
//!
//! They deploy a clean-room `EtomicSwap` HTLC contract and a minimal ERC20 to a
//! throwaway `geth --dev` chain (the shared `mm2_test_helpers::geth_dev` harness),
//! broadcast the real transactions, and assert the on-chain outcome. They skip
//! themselves when `geth` is not on `PATH`, so the default offline suite stays
//! green; a CI job with geth installed runs them.

use super::*;
use common::now_ms;
use mm2_core::mm_ctx::{MmArc, MmCtxBuilder};
use mm2_test_helpers::geth_dev::{GethDev, ETOMIC_SWAP_BYTECODE, TEST_ERC20_BYTECODE};
use serde_json::json;

/// Test-only DEX-fee destination pubkey (community netid), used as the swap receiver.
fn test_dex_fee_addr_raw_pubkey() -> &'static [u8] {
    mm2_net_config::net_config_or_panic(8762).dex_fee_addr_raw_pubkey()
}

/// Well-known throwaway test key (the same one the ignored upstream tests used).
const TEST_PRIV_KEY: &str = "809465b17d0a4ddb3e4c69e8f23c2cabad868f51f8bed5c765ad1d6516c3306f";

/// 100 ETH in wei — plenty of gas + value for the throwaway test key.
const HUNDRED_ETH_WEI: u128 = 100_000_000_000_000_000_000;

fn test_key_pair() -> KeyPair { KeyPair::from_secret_slice(&hex::decode(TEST_PRIV_KEY).unwrap()).unwrap() }

/// `0x`-prefixed lowercase hex of an [`Address`] (H160), as the JSON-RPC harness expects.
fn addr_hex(addr: Address) -> String { format!("0x{:x}", addr) }

/// Builds an `EthCoin` bound to the local geth node, using the throwaway test key.
/// Returns the `MmArc` too: the coin only holds a weak ctx ref, so the caller must
/// keep the returned `MmArc` alive for the lifetime of the coin.
fn dev_eth_coin(coin_type: EthCoinType, node: &GethDev, swap_contract: Address) -> (MmArc, EthCoin) {
    let key_pair = test_key_pair();
    let my_addr = key_pair.address();
    let web3 = crate::eth::alloy_compat::build_provider(vec![node.rpc_url.clone()], vec![]).unwrap();
    let ctx = MmCtxBuilder::new().into_mm_arc();
    let ticker = match &coin_type {
        EthCoinType::Erc20 { .. } => "TEST".to_string(),
        _ => "ETH".to_string(),
    };
    let coin = EthCoin(Arc::new(EthCoinImpl {
        ticker,
        coin_type,
        my_address: my_addr,
        sign_message_prefix: Some(String::from("Ethereum Signed Message:\n")),
        signer: EthSigner::Local(key_pair),
        swap_contract_address: swap_contract,
        fallback_swap_contract: None,
        web3_instances: vec![Web3Instance {
            web3: web3.clone(),
            is_parity: false,
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
        chain_id: Some(node.chain_id),
        logs_block_range: DEFAULT_LOGS_BLOCK_RANGE,
        derivation_method: DerivationMethod::Iguana(my_addr),
        swap_v2_contracts: None,
        gas_limit_v2: EthGasLimitV2::default(),
    }));
    (ctx, coin)
}

/// Deploys the ERC20 fixture (dev account gets the whole supply) and moves
/// `amount` (base units) to `to`. Returns the token address.
fn deploy_erc20_and_fund(node: &GethDev, to: Address, amount: U256) -> Address {
    let token_addr: Address = node.deploy(TEST_ERC20_BYTECODE, "").parse().unwrap();
    let transfer_data = ERC20_CONTRACT
        .function("transfer")
        .unwrap()
        .encode_input(&[Token::Address(to), Token::Uint(amount)])
        .unwrap();
    node.send_call(&addr_hex(token_addr), &transfer_data);
    token_addr
}

#[test]
fn send_and_refund_eth_payment() {
    let node = match GethDev::start() {
        Some(node) => node,
        None => {
            log!("geth binary not available; skipping send_and_refund_eth_payment");
            return;
        },
    };

    let test_addr = test_key_pair().address();
    node.fund_eth(&addr_hex(test_addr), HUNDRED_ETH_WEI);

    let swap_addr: Address = node.deploy(ETOMIC_SWAP_BYTECODE, "").parse().unwrap();
    let (_ctx, coin) = dev_eth_coin(EthCoinType::Eth, &node, swap_addr);

    let secret_hash = [1u8; 20];
    // A timelock in the past so the refund is immediately valid.
    let time_lock = (now_ms() / 1000) as u32 - 200;

    let payment = coin
        .send_maker_payment(
            time_lock,
            &[],
            test_dex_fee_addr_raw_pubkey(),
            &secret_hash,
            "0.001".parse().unwrap(),
            &coin.swap_contract_address(),
        )
        .wait()
        .unwrap();

    let payment_hash = format!("0x{}", hex::encode(payment.tx_hash().0));
    let receipt = node.wait_receipt(&payment_hash);
    assert_eq!(receipt["status"].as_str(), Some("0x1"), "payment tx reverted");

    let refund = coin
        .send_maker_refunds_payment(
            &payment.tx_hex(),
            time_lock,
            test_dex_fee_addr_raw_pubkey(),
            &secret_hash,
            &[],
            &coin.swap_contract_address(),
        )
        .wait()
        .unwrap();

    let refund_hash = format!("0x{}", hex::encode(refund.tx_hash().0));
    let refund_receipt = node.wait_receipt(&refund_hash);
    assert_eq!(refund_receipt["status"].as_str(), Some("0x1"), "refund tx reverted");

    // The payment must now be in the SenderRefunded state (3).
    let id = coin.etomic_swap_id(time_lock, &secret_hash);
    let state = coin.payment_status(swap_addr, Token::FixedBytes(id)).wait().unwrap();
    assert_eq!(state, U256::from(3u64), "payment state should be SenderRefunded");
}

#[test]
fn send_and_refund_erc20_payment() {
    let node = match GethDev::start() {
        Some(node) => node,
        None => {
            log!("geth binary not available; skipping send_and_refund_erc20_payment");
            return;
        },
    };

    let test_addr = test_key_pair().address();
    node.fund_eth(&addr_hex(test_addr), HUNDRED_ETH_WEI);

    let swap_addr: Address = node.deploy(ETOMIC_SWAP_BYTECODE, "").parse().unwrap();
    let token_addr = deploy_erc20_and_fund(&node, test_addr, U256::from(1000u64) * U256::exp10(18));

    let (_ctx, coin) = dev_eth_coin(
        EthCoinType::Erc20 {
            platform: "ETH".to_string(),
            token_addr,
        },
        &node,
        swap_addr,
    );

    let secret_hash = [1u8; 20];
    let time_lock = (now_ms() / 1000) as u32 - 200;

    let payment = coin
        .send_maker_payment(
            time_lock,
            &[],
            test_dex_fee_addr_raw_pubkey(),
            &secret_hash,
            "0.001".parse().unwrap(),
            &coin.swap_contract_address(),
        )
        .wait()
        .unwrap();

    let payment_hash = format!("0x{}", hex::encode(payment.tx_hash().0));
    let receipt = node.wait_receipt(&payment_hash);
    assert_eq!(receipt["status"].as_str(), Some("0x1"), "erc20 payment tx reverted");

    let refund = coin
        .send_maker_refunds_payment(
            &payment.tx_hex(),
            time_lock,
            test_dex_fee_addr_raw_pubkey(),
            &secret_hash,
            &[],
            &coin.swap_contract_address(),
        )
        .wait()
        .unwrap();

    let refund_hash = format!("0x{}", hex::encode(refund.tx_hash().0));
    let refund_receipt = node.wait_receipt(&refund_hash);
    assert_eq!(
        refund_receipt["status"].as_str(),
        Some("0x1"),
        "erc20 refund tx reverted"
    );

    let id = coin.etomic_swap_id(time_lock, &secret_hash);
    let state = coin.payment_status(swap_addr, Token::FixedBytes(id)).wait().unwrap();
    assert_eq!(state, U256::from(3u64), "payment state should be SenderRefunded");
}

/// Deterministic, offline coverage of the EVM `withdraw` path for both ETH and an
/// ERC20 token: build the withdrawal, broadcast it, and assert it executed and the
/// recipient actually received the funds on-chain.
#[test]
fn withdraw_eth_and_erc20() {
    let node = match GethDev::start() {
        Some(node) => node,
        None => {
            log!("geth binary not available; skipping withdraw_eth_and_erc20");
            return;
        },
    };

    let test_addr = test_key_pair().address();
    node.fund_eth(&addr_hex(test_addr), HUNDRED_ETH_WEI);

    // The swap contract is irrelevant to `withdraw`; deploy one so the coin is well-formed.
    let swap_addr: Address = node.deploy(ETOMIC_SWAP_BYTECODE, "").parse().unwrap();
    let recipient = "0x657980d55733B41c0C64c06003864e1aAD917Ca7";

    // --- ETH withdraw ---
    let (_eth_ctx, eth_coin) = dev_eth_coin(EthCoinType::Eth, &node, swap_addr);
    let details = eth_coin
        .withdraw(WithdrawRequest {
            coin: "ETH".into(),
            from: None,
            to: recipient.into(),
            amount: "1".parse().unwrap(),
            max: false,
            fee: None,
        })
        .wait()
        .unwrap();
    assert_eq!(details.to, vec![recipient.to_string()]);
    let tx_hash = eth_coin.send_raw_tx_bytes(&details.tx_hex.0).wait().unwrap();
    let receipt = node.wait_receipt(&format!("0x{}", tx_hash));
    assert_eq!(receipt["status"].as_str(), Some("0x1"), "eth withdraw tx reverted");
    // The recipient (starting from zero) now holds exactly 1 ETH.
    let bal = node.rpc("eth_getBalance", json!([recipient, "latest"]));
    assert_eq!(bal.as_str(), Some("0xde0b6b3a7640000"), "recipient eth balance"); // 1e18

    // --- ERC20 withdraw ---
    let token_addr = deploy_erc20_and_fund(&node, test_addr, U256::from(1000u64) * U256::exp10(18));
    let (_erc20_ctx, erc20_coin) = dev_eth_coin(
        EthCoinType::Erc20 {
            platform: "ETH".to_string(),
            token_addr,
        },
        &node,
        swap_addr,
    );
    let details = erc20_coin
        .withdraw(WithdrawRequest {
            coin: "TEST".into(),
            from: None,
            to: recipient.into(),
            amount: "100".parse().unwrap(),
            max: false,
            fee: None,
        })
        .wait()
        .unwrap();
    assert_eq!(details.to, vec![recipient.to_string()]);
    let tx_hash = erc20_coin.send_raw_tx_bytes(&details.tx_hex.0).wait().unwrap();
    let receipt = node.wait_receipt(&format!("0x{}", tx_hash));
    assert_eq!(receipt["status"].as_str(), Some("0x1"), "erc20 withdraw tx reverted");
    // The recipient now holds exactly 100 tokens.
    let balance_of = ERC20_CONTRACT
        .function("balanceOf")
        .unwrap()
        .encode_input(&[Token::Address(recipient.parse().unwrap())])
        .unwrap();
    let call = node.rpc(
        "eth_call",
        json!([{ "to": addr_hex(token_addr), "data": format!("0x{}", hex::encode(balance_of)) }, "latest"]),
    );
    let decoded = ERC20_CONTRACT
        .function("balanceOf")
        .unwrap()
        .decode_output(&hex::decode(call.as_str().unwrap().trim_start_matches("0x")).unwrap())
        .unwrap();
    match &decoded[0] {
        Token::Uint(bal) => assert_eq!(*bal, U256::from(100u64) * U256::exp10(18), "recipient token balance"),
        other => panic!("unexpected balanceOf output: {:?}", other),
    }
}
