//! The per-coin gas-limit overrides of CRD ch. 57 §57.5 and §57.6 at the places that consume them (T5, T6, T9),
//! and the gas-estimate multiplier of R57.5.3b (T13).
//!
//! Every coin here is built the way activation builds it: the gas-limit blocks come from coins-file JSON
//! through [`coin_gas_limits`], and the call sites run against mocks that capture the figure they were
//! given. Mocks are thread-local, so the tests do not disturb one another.

use super::*;
use common::block_on;
use mm2_core::mm_ctx::MmCtxBuilder;
use mm2_eth::keys::Secret;
use mocktopus::mocking::*;
use serde_json::json;
use std::cell::RefCell;

const TOKEN_ADDR: &str = "2b294F029Fde858b2c62184e8390591755521d8E";

thread_local! {
    /// The gas figures `sign_and_send_transaction` was called with, in call order.
    static SENT_GAS: RefCell<Vec<U256>> = const { RefCell::new(Vec::new()) };
}

fn token_address() -> Address { Address::from_str(TOKEN_ADDR).unwrap() }

/// A coin of `coin_type` over a coins file holding an `ETH` platform entry and a `JST` token entry, each merged
/// with the given extra members (`gas_limit`, `gas_limit_v2`, ...). The limits are resolved by the activation
/// helper from the entry of the coin itself, the platform entry being its fallback.
fn coin_over_entries(coin_type: EthCoinType, eth_extra: Json, jst_extra: Json) -> Result<(MmArc, EthCoin), String> {
    let mut eth = json!({"coin": "ETH", "protocol": {"type": "ETH"}, "mm2": 1});
    let mut jst = json!({"coin": "JST", "mm2": 1, "decimals": 18, "protocol": {"type": "ERC20",
        "protocol_data": {"platform": "ETH", "contract_address": format!("0x{TOKEN_ADDR}")}}});
    for (entry, extra) in [(&mut eth, eth_extra), (&mut jst, jst_extra)] {
        for (key, value) in extra.as_object().unwrap() {
            entry[key] = value.clone();
        }
    }
    let ctx = MmCtxBuilder::new()
        .with_conf(json!({"netid": mm2_net_config::SUPPORTED_NETIDS[0], "coins": [eth, jst]}))
        .into_mm_arc();

    let (ticker, protocol) = match &coin_type {
        EthCoinType::Eth => ("ETH", CoinProtocol::ETH { chain_id: None }),
        EthCoinType::Erc20 { platform, .. } => ("JST", CoinProtocol::ERC20 {
            platform: platform.clone(),
            contract_address: format!("0x{TOKEN_ADDR}"),
        }),
        _ => panic!("EVM coin types only"),
    };
    let entry = crate::coin_conf(&ctx, ticker);
    let (gas_limit, gas_limit_v2) = coin_gas_limits(&ctx, &entry, &protocol).map_err(|e| e.to_string())?;
    let estimate_gas_mult = coin_estimate_gas_mult(&ctx, &entry, &protocol).map_err(|e| e.to_string())?;

    let key_pair = KeyPair::from_secret_slice(
        &hex::decode("809465b17d0a4ddb3e4c69e8f23c2cabad868f51f8bed5c765ad1d6516c3306f").unwrap(),
    )
    .unwrap();
    let web3 = crate::eth::alloy_compat::build_provider(vec!["http://dummy.dummy".to_owned()], vec![]).unwrap();
    let my_address = key_pair.address();
    let coin = EthCoin(Arc::new(EthCoinImpl {
        coin_type,
        decimals: 18,
        gas_station_url: None,
        gas_station_decimals: ETH_GAS_STATION_DECIMALS,
        history_sync_state: Mutex::new(HistorySyncState::NotEnabled),
        gas_station_policy: GasStationPricePolicy::MeanAverageFast,
        my_address,
        sign_message_prefix: None,
        signer: EthSigner::Local(key_pair),
        swap_contract_address: Address::from_str("7Bc1bBDD6A0a722fC9bffC49c921B685ECB84b94").unwrap(),
        fallback_swap_contract: None,
        ticker: ticker.to_owned(),
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
        derivation_method: DerivationMethod::Iguana(my_address),
        swap_v2_contracts: Some(SwapV2Contracts {
            maker_swap_v2_contract: Address::from_str("5Bc1bBDD6A0a722fC9bffC49c921B685ECB84b95").unwrap(),
            taker_swap_v2_contract: Address::from_str("6Bc1bBDD6A0a722fC9bffC49c921B685ECB84b96").unwrap(),
        }),
        gas_limit,
        gas_limit_v2,
        estimate_gas_mult,
    }));
    Ok((ctx, coin))
}

fn eth_coin_with_ctx(eth_extra: Json) -> (MmArc, EthCoin) {
    coin_over_entries(EthCoinType::Eth, eth_extra, json!({})).unwrap()
}

fn eth_coin(eth_extra: Json) -> EthCoin { eth_coin_with_ctx(eth_extra).1 }

fn token_coin(eth_extra: Json, jst_extra: Json) -> EthCoin {
    coin_over_entries(
        EthCoinType::Erc20 {
            platform: "ETH".to_owned(),
            token_addr: token_address(),
        },
        eth_extra,
        jst_extra,
    )
    .unwrap()
    .1
}

/// Makes `sign_and_send_transaction` record its gas figure and fail, so the call site is observed without a node.
fn capture_sent_gas() {
    SENT_GAS.with(|gas| gas.borrow_mut().clear());
    EthCoin::sign_and_send_transaction.mock_safe(|_, _value, _action, _data, gas| {
        SENT_GAS.with(|sent| sent.borrow_mut().push(gas));
        MockResult::Return(Box::new(futures01::future::err::<SignedEthTx, TransactionErr>(
            TransactionErr::Plain("captured".to_owned()),
        )))
    });
}

fn sent_gas() -> Vec<u64> { SENT_GAS.with(|gas| gas.borrow().iter().map(|g| g.as_u64()).collect()) }

fn run(fut: EthTxFut) { let _ = block_on(fut.compat()); }

/// The counterparty's HTLC payment transaction that a spend or a refund refers to.
fn payment_tx(coin: &EthCoin) -> SignedEthTx {
    let (function, inputs) = match coin.coin_type {
        EthCoinType::Eth => ("ethPayment", vec![
            Token::FixedBytes(vec![7u8; 32]),
            Token::Address(coin.my_address),
            Token::FixedBytes(vec![9u8; 20]),
            Token::Uint(U256::from(1_800_000_000u64)),
        ]),
        _ => ("erc20Payment", vec![
            Token::FixedBytes(vec![7u8; 32]),
            Token::Uint(U256::from(1_000_000u64)),
            Token::Address(token_address()),
            Token::Address(coin.my_address),
            Token::FixedBytes(vec![9u8; 20]),
            Token::Uint(U256::from(1_800_000_000u64)),
        ]),
    };
    let data = SWAP_CONTRACT.function(function).unwrap().encode_input(&inputs).unwrap();
    let unsigned = UnSignedEthTx {
        nonce: U256::from(3u64),
        gas_price: U256::from(1_000_000_000u64),
        gas: U256::from(150_000u64),
        action: Action::Call(coin.swap_contract_address),
        value: U256::from(1_000u64),
        data,
    };
    let secret = Secret::from_str("4646464646464646464646464646464646464646464646464646464646464646").unwrap();
    unsigned.sign(&secret, Some(1))
}

fn mock_payment_sent() {
    EthCoin::payment_status
        .mock_safe(|_, _, _| MockResult::Return(Box::new(futures01::future::ok(PAYMENT_STATE_SENT.into()))));
}

fn wei_amount(wei: u64) -> TradeFee {
    TradeFee {
        coin: "ETH".to_owned(),
        amount: u256_to_big_decimal(wei.into(), 18).unwrap().into(),
        paid_from_trading_vol: false,
    }
}

fn mock_gas_price_one() {
    EthCoin::get_gas_price.mock_safe(|_| MockResult::Return(Box::new(futures01::future::ok(U256::one()))));
}

// ─── T5: the nine V1 keys at their call sites ───────────────────────────────────────────────────────

#[test]
fn t5_sends_use_eth_send_coins_and_eth_send_erc20() {
    capture_sent_gas();
    let cfg = json!({"gas_limit": {"eth_send_coins": 31_111, "eth_send_erc20": 422_222}});
    let eth = eth_coin(cfg.clone());
    run(eth.send_to_address(token_address(), U256::one()));
    // The token entry is silent, so it inherits the platform's block.
    let token = token_coin(cfg, json!({}));
    run(token.send_to_address(token_address(), U256::one()));
    assert_eq!(sent_gas(), vec![31_111, 422_222]);
}

#[test]
fn t5_payment_uses_eth_payment_and_erc20_payment() {
    capture_sent_gas();
    EthCoin::allowance.mock_safe(|_, _| MockResult::Return(Box::new(futures01::future::ok(U256::max_value()))));
    let cfg = json!({"gas_limit": {"eth_payment": 12_345, "erc20_payment": 234_567}});
    let swap = Address::from_str("7Bc1bBDD6A0a722fC9bffC49c921B685ECB84b94").unwrap();
    let eth = eth_coin(cfg.clone());
    run(eth.send_hash_time_locked_payment(vec![1; 32], U256::one(), 1_800_000_000, &[2; 20], swap, swap));
    let token = token_coin(cfg, json!({}));
    run(token.send_hash_time_locked_payment(vec![1; 32], U256::one(), 1_800_000_000, &[2; 20], swap, swap));
    assert_eq!(sent_gas(), vec![12_345, 234_567]);
}

#[test]
fn t5_erc20_payment_after_an_approval_still_uses_erc20_payment() {
    capture_sent_gas();
    // Allowance below the value: the payment follows an `approve`, which is mocked out here.
    EthCoin::allowance.mock_safe(|_, _| MockResult::Return(Box::new(futures01::future::ok(U256::zero()))));
    let approved = payment_tx(&token_coin(json!({}), json!({})));
    EthCoin::approve_for_v1_payment
        .mock_safe(move |_, _, _| MockResult::Return(Box::new(futures01::future::ok(approved.clone()))));
    let swap = Address::from_str("7Bc1bBDD6A0a722fC9bffC49c921B685ECB84b94").unwrap();
    let token = token_coin(json!({"gas_limit": {"erc20_payment": 234_567}}), json!({}));
    run(token.send_hash_time_locked_payment(vec![1; 32], U256::one(), 1_800_000_000, &[2; 20], swap, swap));
    assert_eq!(sent_gas(), vec![234_567]);
}

#[test]
fn t5_receiver_spend_uses_eth_receiver_spend_and_erc20_receiver_spend() {
    capture_sent_gas();
    mock_payment_sent();
    let cfg = json!({"gas_limit": {"eth_receiver_spend": 13_579, "erc20_receiver_spend": 246_802}});
    let swap = Address::from_str("7Bc1bBDD6A0a722fC9bffC49c921B685ECB84b94").unwrap();
    let eth = eth_coin(cfg.clone());
    run(eth.spend_hash_time_locked_payment(payment_tx(&eth), swap, &[5; 32]));
    let token = token_coin(cfg, json!({}));
    run(token.spend_hash_time_locked_payment(payment_tx(&token), swap, &[5; 32]));
    assert_eq!(sent_gas(), vec![13_579, 246_802]);
}

#[test]
fn t5_broadcast_refund_uses_the_refund_keys_and_never_a_payment_key() {
    capture_sent_gas();
    mock_payment_sent();
    // Payment keys set far apart from the refund keys: a refund priced by a payment key would show.
    let cfg = json!({"gas_limit": {"eth_sender_refund": 98_765, "erc20_sender_refund": 187_654,
                                   "eth_payment": 1, "erc20_payment": 2}});
    let swap = Address::from_str("7Bc1bBDD6A0a722fC9bffC49c921B685ECB84b94").unwrap();
    let eth = eth_coin(cfg.clone());
    run(eth.refund_hash_time_locked_payment(swap, payment_tx(&eth)));
    let token = token_coin(cfg, json!({}));
    run(token.refund_hash_time_locked_payment(swap, payment_tx(&token)));
    assert_eq!(sent_gas(), vec![98_765, 187_654]);
}

#[test]
fn t5_upper_bound_sender_preimage_adds_the_refund_keys() {
    mock_gas_price_one();
    EthCoin::allowance.mock_safe(|_, _| MockResult::Return(Box::new(futures01::future::ok(U256::max_value()))));
    let cfg = json!({"gas_limit": {"eth_payment": 1_000, "eth_sender_refund": 2_000,
                                   "erc20_payment": 10_000, "erc20_sender_refund": 20_000}});
    let value = || TradePreimageValue::UpperBound(1.into());

    // Platform coin: `eth_payment + eth_sender_refund` (the baseline's single 300000 constant, consolidated).
    let eth = eth_coin(cfg.clone());
    let fee = block_on(eth.get_sender_trade_fee(value(), FeeApproxStage::WithoutApprox)).unwrap();
    assert_eq!(fee, wei_amount(3_000));
    // Every stage of this build reserves the refund leg (CRD D1: no separate non-upper-bound stage yet).
    for stage in [
        FeeApproxStage::StartSwap,
        FeeApproxStage::OrderIssue,
        FeeApproxStage::TradePreimage,
    ] {
        let fee = block_on(eth.get_sender_trade_fee(value(), stage.clone())).unwrap();
        let expected = increase_gas_price_by_stage(U256::one(), &stage).as_u64() * 3_000;
        assert_eq!(fee, wei_amount(expected), "{stage:?}");
    }

    // Token: `erc20_payment + erc20_sender_refund`, allowance sufficient so no approve estimate is added.
    let token = token_coin(cfg, json!({}));
    let fee = block_on(token.get_sender_trade_fee(value(), FeeApproxStage::WithoutApprox)).unwrap();
    assert_eq!(fee, wei_amount(30_000));
}

#[test]
fn t5_token_sender_preimage_adds_the_approve_estimate_when_the_allowance_is_short() {
    mock_gas_price_one();
    EthCoin::allowance.mock_safe(|_, _| MockResult::Return(Box::new(futures01::future::ok(U256::zero()))));
    // The approve figure arrives through the one `approve_gas_estimate` seam, from the node's estimate.
    EthCoinImpl::estimate_gas
        .mock_safe(|_, _| MockResult::Return(Box::new(futures01::future::ok(U256::from(60_000u64)))));
    let token = token_coin(
        json!({"gas_limit": {"erc20_payment": 10_000, "erc20_sender_refund": 20_000}}),
        json!({}),
    );
    let fee = block_on(token.get_sender_trade_fee(TradePreimageValue::Exact(1.into()), FeeApproxStage::WithoutApprox))
        .unwrap();
    assert_eq!(fee, wei_amount(10_000 + 20_000 + 60_000));
}

#[test]
fn t5_receiver_preimage_uses_the_receiver_spend_keys() {
    mock_gas_price_one();
    let cfg = json!({"gas_limit": {"eth_receiver_spend": 5_000, "erc20_receiver_spend": 50_000}});
    let eth = eth_coin(cfg.clone());
    let fee = eth
        .get_receiver_trade_fee(FeeApproxStage::WithoutApprox)
        .wait()
        .unwrap();
    assert_eq!(fee, wei_amount(5_000));
    let token = token_coin(cfg, json!({}));
    let fee = token
        .get_receiver_trade_fee(FeeApproxStage::WithoutApprox)
        .wait()
        .unwrap();
    assert_eq!(fee, wei_amount(50_000));
}

#[test]
fn t5_flat_trade_fee_uses_eth_max_trade_gas_for_the_platform_coin_and_tokens() {
    mock_gas_price_one();
    let cfg = json!({"gas_limit": {"eth_max_trade_gas": 7_000}});
    let eth = eth_coin(cfg.clone());
    assert_eq!(eth.get_trade_fee().wait().unwrap(), wei_amount(7_000));
    let token = token_coin(cfg, json!({}));
    assert_eq!(token.get_trade_fee().wait().unwrap(), wei_amount(7_000));
}

#[test]
fn t5_max_native_withdrawal_deducts_eth_send_coins() {
    const BALANCE: u64 = 10_000_000;
    const GAS_PRICE: u64 = 3;
    EthCoin::get_gas_price.mock_safe(|_| MockResult::Return(Box::new(futures01::future::ok(U256::from(GAS_PRICE)))));
    evm_withdraw_balance.mock_safe(|_, _| MockResult::Return(Box::new(futures01::future::ok(U256::from(BALANCE)))));
    get_addr_nonce.mock_safe(|_, _| MockResult::Return(Box::new(futures01::future::ok(0.into()))));
    // The estimate is asked about the balance less `eth_send_coins * gas_price`, the standard transfer fee.
    thread_local! { static ESTIMATED_VALUE: RefCell<Option<U256>> = const { RefCell::new(None) }; }
    EthCoinImpl::estimate_gas.mock_safe(|_, req| {
        ESTIMATED_VALUE.with(|v| *v.borrow_mut() = req.value);
        MockResult::Return(Box::new(futures01::future::ok(U256::from(21_000u64))))
    });

    let (ctx, eth) = eth_coin_with_ctx(json!({"gas_limit": {"eth_send_coins": 50_000}}));
    let req = WithdrawRequest {
        amount: 0.into(),
        from: None,
        to: "0x7Bc1bBDD6A0a722fC9bffC49c921B685ECB84b94".to_owned(),
        coin: "ETH".to_owned(),
        max: true,
        fee: None,
    };
    block_on(withdraw_impl(ctx, eth, req)).unwrap();
    assert_eq!(
        ESTIMATED_VALUE.with(|v| *v.borrow()),
        Some(U256::from(BALANCE - 50_000 * GAS_PRICE))
    );
}

#[test]
fn t5_insufficient_gas_funds_threshold_is_eth_send_coins() {
    EthCoin::get_gas_price.mock_safe(|_| MockResult::Return(Box::new(futures01::future::ok(U256::from(40u64)))));
    EthCoinImpl::estimate_gas.mock_safe(|_, _| {
        MockResult::Return(Box::new(futures01::future::err(MmError::new(Web3RpcError::Transport(
            "error code -32000: gas required exceeds allowance (0)".to_string(),
        )))))
    });
    EthCoin::my_balance.mock_safe(|_| MockResult::Return(Box::new(futures01::future::ok(U256::from(u64::MAX)))));
    EthCoin::base_coin_balance.mock_safe(|_| MockResult::Return(Box::new(futures01::future::ok(BigDecimal::from(0)))));

    let token = token_coin(json!({"gas_limit": {"eth_send_coins": 123_456}}), json!({}));
    let dex_fee = u256_to_big_decimal(100_000_000_000u64.into(), 18).unwrap();
    let err = block_on(token.get_fee_to_send_taker_fee(dex_fee, FeeApproxStage::WithoutApprox)).unwrap_err();
    match err.get_inner() {
        TradePreimageError::NotSufficientBalance { required, .. } => {
            assert_eq!(
                *required,
                u256_to_big_decimal(U256::from(123_456u64 * 40), ETH_DECIMALS).unwrap()
            );
        },
        other => panic!("expected NotSufficientBalance, got {other:?}"),
    }
}

// ─── defaults, inheritance and the absent block ─────────────────────────────────────────────────────

#[test]
fn defaults_reach_the_call_sites_when_no_block_is_configured() {
    capture_sent_gas();
    mock_payment_sent();
    mock_gas_price_one();
    let swap = Address::from_str("7Bc1bBDD6A0a722fC9bffC49c921B685ECB84b94").unwrap();
    let eth = eth_coin(json!({}));
    run(eth.send_to_address(token_address(), U256::one()));
    run(eth.send_hash_time_locked_payment(vec![1; 32], U256::one(), 1_800_000_000, &[2; 20], swap, swap));
    run(eth.spend_hash_time_locked_payment(payment_tx(&eth), swap, &[5; 32]));
    run(eth.refund_hash_time_locked_payment(swap, payment_tx(&eth)));
    // The platform coin's defaults: send 21000, payment 65000, receiver spend 65000, sender refund 100000.
    assert_eq!(sent_gas(), vec![21_000, 65_000, 65_000, 100_000]);
    assert_eq!(eth.get_trade_fee().wait().unwrap(), wei_amount(150_000));
    let fee = block_on(eth.get_sender_trade_fee(TradePreimageValue::Exact(1.into()), FeeApproxStage::WithoutApprox));
    assert_eq!(fee.unwrap(), wei_amount(165_000));
    assert_eq!(
        eth.get_receiver_trade_fee(FeeApproxStage::WithoutApprox)
            .wait()
            .unwrap(),
        wei_amount(65_000)
    );
}

#[test]
fn a_token_with_its_own_block_wins_and_a_silent_token_inherits_the_platform_block() {
    capture_sent_gas();
    let platform = json!({"gas_limit": {"eth_send_erc20": 111_111}});
    let own = json!({"gas_limit": {"eth_send_erc20": 222_222}});
    run(token_coin(platform.clone(), own).send_to_address(token_address(), U256::one()));
    run(token_coin(platform.clone(), json!({})).send_to_address(token_address(), U256::one()));
    // The platform coin is not changed by what a token configures.
    run(eth_coin(platform).send_to_address(token_address(), U256::one()));
    // Both outcomes above are token sends or a native send; the native one is the default 21000.
    assert_eq!(sent_gas(), vec![222_222, 111_111, 21_000]);
}

// ─── T6: the V2 selection matrix ────────────────────────────────────────────────────────────────────

fn v2_config() -> Json {
    json!({"gas_limit_v2": {
        "maker": {"eth_payment": 101, "erc20_payment": 102, "eth_taker_spend": 103, "erc20_taker_spend": 104,
                  "eth_maker_refund_timelock": 105, "erc20_maker_refund_timelock": 106,
                  "eth_maker_refund_secret": 107, "erc20_maker_refund_secret": 108},
        "taker": {"eth_payment": 201, "erc20_payment": 202, "eth_maker_spend": 203, "erc20_maker_spend": 204,
                  "eth_taker_refund_timelock": 205, "erc20_taker_refund_timelock": 206,
                  "eth_taker_refund_secret": 207, "erc20_taker_refund_secret": 208, "approve_payment": 209},
        "nft_maker": {"erc721_payment": 301, "erc1155_payment": 302, "erc721_taker_spend": 303,
                      "erc1155_taker_spend": 304, "erc721_maker_refund_timelock": 305,
                      "erc1155_maker_refund_timelock": 306, "erc721_maker_refund_secret": 307,
                      "erc1155_maker_refund_secret": 308}
    }})
}

#[test]
fn t6_selection_matrix_resolves_to_the_configured_members() {
    use eth_swap_v2::{EthPaymentType::*, PaymentMethod::*};
    let eth = eth_coin(v2_config());
    let token = token_coin(v2_config(), json!({}));
    let rows = || {
        [
            (MakerPayments, Send, 101, 102),
            (MakerPayments, Spend, 103, 104),
            (MakerPayments, RefundTimelock, 105, 106),
            (MakerPayments, RefundSecret, 107, 108),
            (TakerPayments, Send, 201, 202),
            (TakerPayments, Spend, 203, 204),
            (TakerPayments, RefundTimelock, 205, 206),
            (TakerPayments, RefundSecret, 207, 208),
        ]
    };
    for (side, method, eth_expected, _) in rows() {
        assert_eq!(
            eth.gas_limit_v2.gas_limit(&eth.coin_type, side, method),
            Ok(eth_expected)
        );
    }
    for (side, method, _, erc20_expected) in rows() {
        assert_eq!(
            token.gas_limit_v2.gas_limit(&token.coin_type, side, method),
            Ok(erc20_expected)
        );
    }
    assert_eq!(token.gas_limit_v2.taker.approve_payment, 209);
}

#[test]
fn t6_nft_matrix_resolves_to_the_nft_maker_members() {
    use eth_swap_v2::nft_swap_v2::NftKind::*;
    use eth_swap_v2::PaymentMethod::*;
    let eth = eth_coin(v2_config());
    for (method, erc721, erc1155) in [
        (Send, 301, 302),
        (Spend, 303, 304),
        (RefundTimelock, 305, 306),
        (RefundSecret, 307, 308),
    ] {
        assert_eq!(eth.gas_limit_v2.nft_gas_limit(Erc721, method), erc721);
        assert_eq!(eth.gas_limit_v2.nft_gas_limit(Erc1155, method), erc1155);
    }
}

#[test]
fn t6_an_unconfigured_coin_resolves_to_the_chapter_defaults() {
    use eth_swap_v2::{EthPaymentType::*, PaymentMethod::*};
    let eth = eth_coin(json!({}));
    let token = token_coin(json!({}), json!({}));
    let rows = || {
        [
            (MakerPayments, Send, 65_000, 150_000),
            (MakerPayments, Spend, 100_000, 150_000),
            (MakerPayments, RefundTimelock, 90_000, 100_000),
            (MakerPayments, RefundSecret, 90_000, 100_000),
            (TakerPayments, Send, 65_000, 150_000),
            (TakerPayments, Spend, 100_000, 115_000),
            (TakerPayments, RefundTimelock, 90_000, 100_000),
            (TakerPayments, RefundSecret, 90_000, 100_000),
        ]
    };
    for (side, method, eth_expected, _) in rows() {
        assert_eq!(
            eth.gas_limit_v2.gas_limit(&eth.coin_type, side, method),
            Ok(eth_expected)
        );
    }
    for (side, method, _, erc20_expected) in rows() {
        assert_eq!(
            token.gas_limit_v2.gas_limit(&token.coin_type, side, method),
            Ok(erc20_expected)
        );
    }
    assert_eq!(token.gas_limit_v2.taker.approve_payment, 50_000);
}

#[test]
fn t6_a_token_inherits_and_overrides_the_v2_block() {
    use eth_swap_v2::{EthPaymentType::*, PaymentMethod::*};
    let inherited = token_coin(v2_config(), json!({}));
    assert_eq!(
        inherited
            .gas_limit_v2
            .gas_limit(&inherited.coin_type, MakerPayments, Send),
        Ok(102)
    );
    let own = token_coin(v2_config(), json!({"gas_limit_v2": {"maker": {"erc20_payment": 999}}}));
    assert_eq!(own.gas_limit_v2.gas_limit(&own.coin_type, MakerPayments, Send), Ok(999));
    // The token's block replaces the platform's as a whole: an unmentioned member is the default.
    assert_eq!(
        own.gas_limit_v2.gas_limit(&own.coin_type, TakerPayments, Send),
        Ok(150_000)
    );
}

// ─── T9: no clamp, and the type rejection naming the key ────────────────────────────────────────────

#[test]
fn t9_a_huge_configured_limit_reaches_the_call_site_unchanged() {
    capture_sent_gas();
    // Ten times the default of `eth_max_trade_gas` and then some, with `eth_max_trade_gas` itself far below.
    let cfg = json!({"gas_limit": {"eth_send_erc20": 90_000_000_000u64, "eth_max_trade_gas": 1}});
    let token = token_coin(cfg, json!({}));
    run(token.send_to_address(token_address(), U256::one()));
    assert_eq!(sent_gas(), vec![90_000_000_000]);
    // `eth_max_trade_gas` below another member leaves that member unchanged, and prices only the flat fee.
    assert_eq!(token.gas_limit.eth_send_erc20, 90_000_000_000);
    assert_eq!(token.gas_limit.eth_max_trade_gas, 1);

    // A V2 member far above every default is read back unchanged too.
    let v2 = eth_coin(json!({"gas_limit_v2": {"maker": {"eth_payment": 90_000_000_000u64}}}));
    assert_eq!(v2.gas_limit_v2.maker.eth_payment, 90_000_000_000);
}

#[test]
fn t9_a_negative_a_decimal_and_a_string_fail_activation_naming_the_key() {
    let bad = [
        (json!({"gas_limit": {"eth_payment": -1}}), "gas_limit.eth_payment"),
        (
            json!({"gas_limit": {"eth_max_trade_gas": 1.5}}),
            "gas_limit.eth_max_trade_gas",
        ),
        (
            json!({"gas_limit": {"erc20_receiver_spend": "150000"}}),
            "gas_limit.erc20_receiver_spend",
        ),
        (
            json!({"gas_limit_v2": {"taker": {"approve_payment": -50_000}}}),
            "gas_limit_v2.taker.approve_payment",
        ),
        (
            json!({"gas_limit_v2": {"nft_maker": {"erc721_payment": "1"}}}),
            "gas_limit_v2.nft_maker.erc721_payment",
        ),
    ];
    for (extra, key) in bad {
        let err = coin_over_entries(EthCoinType::Eth, extra.clone(), json!({}))
            .map(|_| ())
            .unwrap_err();
        assert!(err.contains(key), "{err} should name {key}");
        // A token without a block of its own inherits the platform's block, and with it the rejection.
        let err = coin_over_entries(
            EthCoinType::Erc20 {
                platform: "ETH".to_owned(),
                token_addr: token_address(),
            },
            extra,
            json!({}),
        )
        .map(|_| ())
        .unwrap_err();
        assert!(err.contains(key), "{err} should name {key}");
    }
}

/// The real activation entry point rejects an unusable member, before any node is contacted.
#[test]
fn t9_the_activation_builder_rejects_an_unusable_member_naming_the_key() {
    let ctx = MmCtxBuilder::new()
        .with_conf(json!({"netid": mm2_net_config::SUPPORTED_NETIDS[0], "coins": []}))
        .into_mm_arc();
    let conf = json!({"coin": "ETH", "protocol": {"type": "ETH"}, "gas_limit": {"eth_payment": "65000"}});
    let params = EthCoinActivationParams {
        urls: vec!["http://dummy.dummy".to_owned()],
        swap_contract_address: Some(Address::from_str("7Bc1bBDD6A0a722fC9bffC49c921B685ECB84b94").unwrap()),
        ..Default::default()
    };
    let err = block_on(eth_coin_from_params_with_priv_key(
        &ctx,
        "ETH",
        &conf,
        &params,
        &[1u8; 32],
        CoinProtocol::ETH { chain_id: None },
    ))
    .unwrap_err();
    match err.get_inner() {
        EthCoinBuildError::InvalidConfig(message) => assert!(message.contains("gas_limit.eth_payment"), "{message}"),
        other => panic!("expected InvalidConfig, got {other:?}"),
    }
}

// ─── T13: the gas-estimate multiplier (R57.5.3b) ────────────────────────────────────────────────────

/// The node's `eth_estimateGas` answers `estimate` to everything and the gas price is one.
fn stub_node_estimate(estimate: u64) {
    mock_gas_price_one();
    EthCoinImpl::estimate_gas
        .mock_safe(move |_, _| MockResult::Return(Box::new(futures01::future::ok(U256::from(estimate)))));
}

/// Like [`capture_sent_gas`], but every send succeeds, so a payment that follows an approve is reached.
fn capture_sent_gas_ok(tx: SignedEthTx) {
    SENT_GAS.with(|gas| gas.borrow_mut().clear());
    EthCoin::sign_and_send_transaction.mock_safe(move |_, _value, _action, _data, gas| {
        SENT_GAS.with(|sent| sent.borrow_mut().push(gas));
        MockResult::Return(Box::new(futures01::future::ok(tx.clone())))
    });
}

fn mock_allowance(allowed: U256) {
    EthCoin::allowance.mock_safe(move |_, _| MockResult::Return(Box::new(futures01::future::ok(allowed))));
}

fn swap_contract() -> Address { Address::from_str("7Bc1bBDD6A0a722fC9bffC49c921B685ECB84b94").unwrap() }

/// The gas figures of the legacy ERC-20 payment of `token` over a node estimating `estimate`, allowance short:
/// the approve first, the payment second.
fn v1_token_payment_gas(token: &EthCoin, estimate: u64) -> Vec<u64> {
    stub_node_estimate(estimate);
    mock_allowance(U256::zero());
    capture_sent_gas_ok(payment_tx(token));
    run(token.send_hash_time_locked_payment(
        vec![1; 32],
        U256::one(),
        1_800_000_000,
        &[2; 20],
        swap_contract(),
        swap_contract(),
    ));
    sent_gas()
}

#[test]
fn t13a_a_token_inherits_the_platform_multiplier_and_the_approve_carries_the_truncated_product() {
    let token = token_coin(json!({"estimate_gas_mult": 1.3}), json!({}));
    assert_eq!(token.estimate_gas_mult, Some(1.3));
    // 1.3 x 50000: the double nearest 1.3 is a little above it, so the exact product is just above 65000.
    // The payment that follows carries `erc20_payment`, unchanged.
    assert_eq!(v1_token_payment_gas(&token, 50_000), vec![65_000, 150_000]);
}

#[test]
fn t13b_the_tokens_own_multiplier_wins_over_the_platforms() {
    let token = token_coin(json!({"estimate_gas_mult": 1.3}), json!({"estimate_gas_mult": 2.0}));
    assert_eq!(token.estimate_gas_mult, Some(2.0));
    assert_eq!(v1_token_payment_gas(&token, 50_000), vec![100_000, 150_000]);
    // The platform coin itself is not changed by what a token configures.
    let platform = eth_coin(json!({"estimate_gas_mult": 1.3}));
    assert_eq!(platform.estimate_gas_mult, Some(1.3));
}

#[test]
fn t13b_an_own_multiplier_of_one_is_a_value_not_an_absence_and_overrides_the_platform() {
    let token = token_coin(json!({"estimate_gas_mult": 1.3}), json!({"estimate_gas_mult": 1.0}));
    assert_eq!(v1_token_payment_gas(&token, 50_000), vec![50_000, 150_000]);
}

#[test]
fn t13c_neither_entry_setting_the_member_leaves_the_estimate_exactly() {
    let token = token_coin(json!({}), json!({}));
    assert_eq!(token.estimate_gas_mult, None);
    assert_eq!(v1_token_payment_gas(&token, 50_001), vec![50_001, 150_000]);
    // A platform coin consults only its own entry; a multiplier on a token never reaches the platform.
    let (_ctx, platform) = coin_over_entries(EthCoinType::Eth, json!({}), json!({"estimate_gas_mult": 3.0})).unwrap();
    assert_eq!(platform.estimate_gas_mult, None);
}

#[test]
fn t13d_the_preimage_adds_the_raw_estimate_and_the_fixed_limits_do_not_move() {
    let cfg = json!({"estimate_gas_mult": 1.3,
                     "gas_limit": {"erc20_payment": 10_000, "erc20_sender_refund": 20_000}});
    let token = token_coin(cfg, json!({}));
    stub_node_estimate(50_000);
    mock_allowance(U256::zero());
    let fee = block_on(token.get_sender_trade_fee(TradePreimageValue::Exact(1.into()), FeeApproxStage::WithoutApprox))
        .unwrap();
    // The RAW 50000, not 65000.
    assert_eq!(fee, wei_amount(10_000 + 20_000 + 50_000));
    // The broadcast approve is scaled and the payment after it is the configured `erc20_payment`.
    assert_eq!(v1_token_payment_gas(&token, 50_000), vec![65_000, 10_000]);
}

#[test]
fn t13d_a_v2_payment_and_its_allowance_step_are_not_scaled() {
    // The V2 allowance step reaches the same estimation, and is not multiplied; the V2 payment carries its
    // R57.6.2 limit.
    let token = token_coin(json!({"estimate_gas_mult": 1.3}), json!({}));
    stub_node_estimate(50_000);
    mock_allowance(U256::zero());
    EthCoin::wait_for_required_allowance
        .mock_safe(|_, _, _, _| MockResult::Return(Box::new(futures01::future::ok(()))));
    capture_sent_gas_ok(payment_tx(&token));
    let maker_pub = [3u8; 64];
    let args = SendTakerFundingArgs {
        funding_time_lock: 1_800_000_000,
        payment_time_lock: 1_800_000_100,
        taker_secret_hash: &[4; 32],
        maker_secret_hash: &[5; 32],
        maker_pub: &maker_pub,
        dex_fee: &DexFee::NoFee,
        premium_amount: 0.into(),
        trading_amount: 1.into(),
        swap_unique_data: &[],
    };
    block_on(token.send_taker_funding_impl(args)).unwrap();
    assert_eq!(sent_gas(), vec![50_000, 150_000]);
}

#[test]
fn t13d_the_v2_payment_follows_a_configured_v2_limit_when_the_member_is_set() {
    let mut cfg = v2_config();
    cfg["estimate_gas_mult"] = json!(9.5);
    let token = token_coin(cfg, json!({}));
    stub_node_estimate(50_000);
    mock_allowance(U256::zero());
    EthCoin::wait_for_required_allowance
        .mock_safe(|_, _, _, _| MockResult::Return(Box::new(futures01::future::ok(()))));
    capture_sent_gas_ok(payment_tx(&token));
    let maker_pub = [3u8; 64];
    let args = SendTakerFundingArgs {
        funding_time_lock: 1_800_000_000,
        payment_time_lock: 1_800_000_100,
        taker_secret_hash: &[4; 32],
        maker_secret_hash: &[5; 32],
        maker_pub: &maker_pub,
        dex_fee: &DexFee::NoFee,
        premium_amount: 0.into(),
        trading_amount: 1.into(),
        swap_unique_data: &[],
    };
    block_on(token.send_taker_funding_impl(args)).unwrap();
    // Raw approve estimate, then the configured taker `erc20_payment`.
    assert_eq!(sent_gas(), vec![50_000, 202]);
}

#[test]
fn t13e_a_value_that_is_not_a_number_fails_activation_naming_the_key() {
    for bad in [json!("1.3"), json!(true), json!(null), json!([1.3]), json!({"x": 1.3})] {
        let extra = json!({ "estimate_gas_mult": bad });
        let err = coin_over_entries(EthCoinType::Eth, extra.clone(), json!({}))
            .map(|_| ())
            .unwrap_err();
        assert!(err.contains("estimate_gas_mult"), "{err}");
        // A token without a member of its own inherits the platform's entry, and with it the rejection.
        let token_type = || EthCoinType::Erc20 {
            platform: "ETH".to_owned(),
            token_addr: token_address(),
        };
        let err = coin_over_entries(token_type(), extra.clone(), json!({}))
            .map(|_| ())
            .unwrap_err();
        assert!(err.contains("estimate_gas_mult"), "{err}");
        // The token's own malformed member is rejected too.
        let err = coin_over_entries(token_type(), json!({}), extra)
            .map(|_| ())
            .unwrap_err();
        assert!(err.contains("estimate_gas_mult"), "{err}");
    }
}

#[test]
fn t13e_a_malformed_platform_member_is_not_consulted_when_the_token_has_its_own() {
    let token_type = EthCoinType::Erc20 {
        platform: "ETH".to_owned(),
        token_addr: token_address(),
    };
    let (_ctx, token) = coin_over_entries(
        token_type,
        json!({"estimate_gas_mult": "bad"}),
        json!({"estimate_gas_mult": 2.0}),
    )
    .unwrap();
    assert_eq!(token.estimate_gas_mult, Some(2.0));
}

/// The real activation entry point rejects a malformed member, before any node is contacted.
#[test]
fn t13e_the_activation_builder_rejects_a_string_member_naming_the_key() {
    let ctx = MmCtxBuilder::new()
        .with_conf(json!({"netid": mm2_net_config::SUPPORTED_NETIDS[0], "coins": []}))
        .into_mm_arc();
    let conf = json!({"coin": "ETH", "protocol": {"type": "ETH"}, "estimate_gas_mult": "1.3"});
    let params = EthCoinActivationParams {
        urls: vec!["http://dummy.dummy".to_owned()],
        swap_contract_address: Some(swap_contract()),
        ..Default::default()
    };
    let err = block_on(eth_coin_from_params_with_priv_key(
        &ctx,
        "ETH",
        &conf,
        &params,
        &[1u8; 32],
        CoinProtocol::ETH { chain_id: None },
    ))
    .unwrap_err();
    match err.get_inner() {
        EthCoinBuildError::InvalidConfig(message) => assert!(message.contains("estimate_gas_mult"), "{message}"),
        other => panic!("expected InvalidConfig, got {other:?}"),
    }
}

/// The token route of a platform coin (the one the legacy and the task token activations share) applies the
/// same inheritance and rejection.
#[test]
fn t13_the_token_route_of_a_platform_coin_inherits_overrides_and_rejects() {
    let build = |platform_extra: Json, jst_extra: Json| {
        let (ctx, platform) = coin_over_entries(EthCoinType::Eth, platform_extra, jst_extra).unwrap();
        let token = block_on(platform.erc20_token_from_conf_or_contract("JST".to_owned(), token_address(), 1, false));
        drop(ctx);
        token
    };
    let inherited = build(json!({"estimate_gas_mult": 1.3}), json!({})).unwrap();
    assert_eq!(inherited.estimate_gas_mult, Some(1.3));
    let own = build(json!({"estimate_gas_mult": 1.3}), json!({"estimate_gas_mult": 2.0})).unwrap();
    assert_eq!(own.estimate_gas_mult, Some(2.0));
    let none = build(json!({}), json!({})).unwrap();
    assert_eq!(none.estimate_gas_mult, None);
    let err = build(json!({}), json!({"estimate_gas_mult": "2.0"})).unwrap_err();
    assert!(err.to_string().contains("estimate_gas_mult"), "{err}");
}

#[test]
fn t13e_a_product_with_a_fractional_part_is_truncated_not_rounded() {
    // 0.7 x 10: the double nearest 0.7 lies just below it, so the exact product is just below 7 and truncates
    // to 6, one unit below the decimal arithmetic result.
    let token = token_coin(json!({"estimate_gas_mult": 0.7}), json!({}));
    assert_eq!(v1_token_payment_gas(&token, 10), vec![6, 150_000]);
    // 1.5 x 3 = 4.5 truncates to 4 (a rounding implementation would give 5).
    let token = token_coin(json!({"estimate_gas_mult": 1.5}), json!({}));
    assert_eq!(v1_token_payment_gas(&token, 3), vec![4, 150_000]);
}

#[test]
fn t13_a_negative_multiplier_activates_and_fails_the_approve_at_send_time() {
    let token = token_coin(json!({"estimate_gas_mult": -1.5}), json!({}));
    assert_eq!(token.estimate_gas_mult, Some(-1.5));
    stub_node_estimate(50_000);
    mock_allowance(U256::zero());
    capture_sent_gas_ok(payment_tx(&token));
    let err = block_on(
        token
            .send_hash_time_locked_payment(
                vec![1; 32],
                U256::one(),
                1_800_000_000,
                &[2; 20],
                swap_contract(),
                swap_contract(),
            )
            .compat(),
    )
    .unwrap_err();
    assert!(err.get_plain_text_format().contains("negative"), "{err:?}");
    // Nothing was broadcast: neither the approve nor the payment.
    assert!(sent_gas().is_empty());
}

#[test]
fn t13_a_zero_multiplier_gives_a_zero_approve_limit() {
    let token = token_coin(json!({"estimate_gas_mult": 0.0}), json!({}));
    assert_eq!(v1_token_payment_gas(&token, 50_000), vec![0, 150_000]);
}

/// The multiplier reaches no fixed limit: with a distinctive member set, every R57.5.2 and R57.6.2 limit
/// equals its configured or default value at its call site.
#[test]
fn t13f_no_fixed_limit_changes_when_the_member_is_set() {
    for (platform_extra, defaults) in [
        // Defaults.
        (json!({"estimate_gas_mult": 7.75}), true),
        // Configured values.
        (
            {
                let mut cfg = v2_config();
                cfg["estimate_gas_mult"] = json!(7.75);
                cfg["gas_limit"] = json!({"eth_send_coins": 31_111, "eth_send_erc20": 422_222, "eth_payment": 12_345,
                    "erc20_payment": 234_567, "eth_receiver_spend": 13_579, "erc20_receiver_spend": 246_802,
                    "eth_sender_refund": 98_765, "erc20_sender_refund": 187_654, "eth_max_trade_gas": 7_000});
                cfg
            },
            false,
        ),
    ] {
        let v = |configured: u64, default: u64| if defaults { default } else { configured };
        let eth = eth_coin(platform_extra.clone());
        let token = token_coin(platform_extra, json!({}));
        assert_eq!(eth.estimate_gas_mult, Some(7.75));
        assert_eq!(token.estimate_gas_mult, Some(7.75));

        // Every V1 call site: sends, payments (following an approve for the token), spends, refunds.
        stub_node_estimate(1_000);
        mock_allowance(U256::zero());
        mock_payment_sent();
        capture_sent_gas_ok(payment_tx(&token));
        run(eth.send_to_address(token_address(), U256::one()));
        run(token.send_to_address(token_address(), U256::one()));
        run(eth.send_hash_time_locked_payment(
            vec![1; 32],
            U256::one(),
            1_800_000_000,
            &[2; 20],
            swap_contract(),
            swap_contract(),
        ));
        run(token.send_hash_time_locked_payment(
            vec![1; 32],
            U256::one(),
            1_800_000_000,
            &[2; 20],
            swap_contract(),
            swap_contract(),
        ));
        run(eth.spend_hash_time_locked_payment(payment_tx(&eth), swap_contract(), &[5; 32]));
        run(token.spend_hash_time_locked_payment(payment_tx(&token), swap_contract(), &[5; 32]));
        run(eth.refund_hash_time_locked_payment(swap_contract(), payment_tx(&eth)));
        run(token.refund_hash_time_locked_payment(swap_contract(), payment_tx(&token)));
        assert_eq!(sent_gas(), vec![
            v(31_111, 21_000),
            v(422_222, 210_000),
            v(12_345, 65_000),
            // The token payment: the approve (1000 x 7.75) and then the payment itself.
            7_750,
            v(234_567, 150_000),
            v(13_579, 65_000),
            v(246_802, 150_000),
            v(98_765, 100_000),
            v(187_654, 150_000),
        ]);

        // The fee-quote figures.
        assert_eq!(eth.get_trade_fee().wait().unwrap(), wei_amount(v(7_000, 150_000)));
        assert_eq!(
            eth.get_receiver_trade_fee(FeeApproxStage::WithoutApprox)
                .wait()
                .unwrap(),
            wei_amount(v(13_579, 65_000))
        );
        mock_allowance(U256::max_value());
        let fee = block_on(
            token.get_sender_trade_fee(TradePreimageValue::UpperBound(1.into()), FeeApproxStage::WithoutApprox),
        )
        .unwrap();
        assert_eq!(fee, wei_amount(v(234_567 + 187_654, 300_000)));

        // The V2 limits, as resolved for the selection matrix.
        let (maker_pay, taker_pay, approve) = if defaults {
            (150_000, 150_000, 50_000)
        } else {
            (102, 202, 209)
        };
        assert_eq!(token.gas_limit_v2.maker.erc20_payment, maker_pay);
        assert_eq!(token.gas_limit_v2.taker.erc20_payment, taker_pay);
        assert_eq!(token.gas_limit_v2.taker.approve_payment, approve);
    }
}
