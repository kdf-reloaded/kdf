//! The per-coin gas-limit overrides of CRD ch. 57 §57.5 and §57.6 at the places that consume them (T5, T6, T9).
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
        swap_v2_contracts: None,
        gas_limit,
        gas_limit_v2,
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
    EthCoin::approve.mock_safe(move |_, _, _| MockResult::Return(Box::new(futures01::future::ok(approved.clone()))));
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

/// CRD ch.38 R38.6.6k item 1: a per-coin `fee_estimate_cache_secs` outside 0 to 600 fails the coin's activation,
/// naming the key, before any node is contacted; a token reads its own entry, else its platform coin's.
#[test]
fn t10_an_unusable_fee_estimate_cache_secs_fails_the_activation_naming_the_key() {
    let params = EthCoinActivationParams {
        urls: vec!["http://dummy.dummy".to_owned()],
        swap_contract_address: Some(Address::from_str("7Bc1bBDD6A0a722fC9bffC49c921B685ECB84b94").unwrap()),
        ..Default::default()
    };
    let activate = |ctx: &MmArc, ticker: &str, conf: &Json, protocol: CoinProtocol| {
        block_on(eth_coin_from_params_with_priv_key(
            ctx, ticker, conf, &params, &[1u8; 32], protocol,
        ))
    };
    let platform_conf =
        |secs: Json| json!({"coin": "ETH", "protocol": {"type": "ETH"}, "fee_estimate_cache_secs": secs});
    let token_conf = json!({"coin": "JST", "mm2": 1, "decimals": 18, "protocol": {"type": "ERC20",
        "protocol_data": {"platform": "ETH", "contract_address": format!("0x{TOKEN_ADDR}")}}});
    let erc20 = || CoinProtocol::ERC20 {
        platform: "ETH".to_owned(),
        contract_address: format!("0x{TOKEN_ADDR}"),
    };
    for bad in [json!(601), json!(-1), json!("5"), json!(1.5)] {
        let conf = platform_conf(bad.clone());
        let ctx = MmCtxBuilder::new()
            .with_conf(
                json!({"netid": mm2_net_config::SUPPORTED_NETIDS[0], "coins": [conf.clone(), token_conf.clone()]}),
            )
            .into_mm_arc();
        // The platform coin's own entry.
        let err = activate(&ctx, "ETH", &conf, CoinProtocol::ETH { chain_id: None }).unwrap_err();
        match err.get_inner() {
            EthCoinBuildError::InvalidConfig(message) => {
                assert!(message.contains("fee_estimate_cache_secs"), "{bad}: {message}")
            },
            other => panic!("expected InvalidConfig for {bad}, got {other:?}"),
        }
        // A token with no value of its own reads the platform's.
        let err = activate(&ctx, "JST", &token_conf, erc20()).unwrap_err();
        match err.get_inner() {
            EthCoinBuildError::InvalidConfig(message) => {
                assert!(message.contains("fee_estimate_cache_secs"), "{bad}: {message}")
            },
            other => panic!("expected InvalidConfig for the token over {bad}, got {other:?}"),
        }
    }
    // A usable value does not fail on the key (the activation then goes on to its nodes, which are unreachable).
    let conf = platform_conf(json!(600));
    let ctx = MmCtxBuilder::new()
        .with_conf(json!({"netid": mm2_net_config::SUPPORTED_NETIDS[0], "coins": [conf.clone()]}))
        .into_mm_arc();
    if let Err(err) = activate(&ctx, "ETH", &conf, CoinProtocol::ETH { chain_id: None }) {
        assert!(!err.to_string().contains("fee_estimate_cache_secs"), "{err}");
    }
}
