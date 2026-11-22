//! Unit tests of the EVM-only RPC methods a TRX or TRC20 coin refuses (CRD ch.21 D7): `get_raw_transaction`,
//! `sign_raw_transaction`, the legacy `get_trade_fee`, the receiver side of `trade_preimage`,
//! `get_eth_estimated_fee_per_gas` and `get_token_info`. (`stream::fee_estimator::enable` is tested next to its
//! handler in `mm2_main`.)
//!
//! A TRON coin is built the way the legacy activation does, against a stub node on the loopback interface. The
//! coin's placeholder alloy provider is over that node's URL, so a request sent through it would be recorded by
//! the stub: every test asserts that the node received nothing. The EVM tests at the end show that the same
//! methods still ask the node of an EVM coin.

use super::test_node::{Reply, StubNode};
use super::wallet_path_tests::{trc20_coin, trx_coin, TRC20_CONTRACT};
use super::{tron_coin_from_conf_and_request, Network};
use crate::eth::alloy_compat::transport::test_support::{error_body, result_body, stub_provider, Call,
                                                        Reply as EvmReply, StubSender};
use crate::eth::eth_tests::eth_coin_for_test_with_providers;
use crate::eth::fee_estimation::rpc::{get_eth_estimated_fee_per_gas, GetFeeEstimationRequest,
                                      GetFeeEstimationRequestError};
use crate::eth::{EthCoin, EthCoinType, Web3Instance};
use crate::rpc_command::get_token_info::{get_token_info, GetTokenInfoError, GetTokenInfoRequest};
use crate::{CoinProtocol, CoinsContext, FeeApproxStage, MarketCoinOps, MmCoin, RawTransactionError,
            RawTransactionRequest, SignRawTransactionRequest, TradePreimageError};
use common::{block_on, HttpStatusCode};
use futures::compat::Future01CompatExt;
use http::StatusCode;
use mm2_core::mm_ctx::{MmArc, MmCtxBuilder};
use serde_json::json;

const TX_HASH: &str = "0x1111111111111111111111111111111111111111111111111111111111111111";
const ERC20_CONTRACT: &str = "0xdAC17F958D2ee523a2206206994597C13D831ec7";
const RECEIVER_FEE_REFUSAL: &str = "TRON V1 receiver trade fee is not supported";

/// A node that answers every request the way a TRON node answers EVM JSON-RPC: HTTP 405.
fn tron_node() -> StubNode { StubNode::start(|_, _| Reply::status(405, "")) }

/// A TRX coin and a TRC20 coin, each activated against `node` in a context of its own.
async fn tron_coins(node: &StubNode) -> [(MmArc, EthCoin); 2] {
    [trx_coin(&node.url()).await, trc20_coin(&node.url()).await]
}

/// Registers `coin` as an activated coin of `ctx`.
async fn register(ctx: &MmArc, coin: &EthCoin) {
    let coins_ctx = CoinsContext::from_ctx(ctx).expect("coins context");
    coins_ctx
        .add_coin(coin.clone().into())
        .await
        .expect("the coin is registered once");
}

fn fee_estimation_request(coin: &str, estimator_type: &str) -> GetFeeEstimationRequest {
    serde_json::from_value(json!({ "coin": coin, "estimator_type": estimator_type })).expect("a valid request")
}

fn sign_raw_request(coin: &str) -> SignRawTransactionRequest {
    serde_json::from_value(json!({
        "coin": coin,
        "type": "ETH",
        "tx": {
            "to": "0x7Bc1bBDD6A0a722fC9bffC49c921B685ECB84b94",
            "value": "1",
            "gas_limit": 21000,
            "gas_price": "1"
        }
    }))
    .expect("a valid request")
}

fn erc20_on(platform: &str) -> GetTokenInfoRequest {
    GetTokenInfoRequest {
        protocol: CoinProtocol::ERC20 {
            platform: platform.to_owned(),
            contract_address: ERC20_CONTRACT.to_owned(),
        },
    }
}

#[test]
fn get_raw_transaction_is_not_implemented_for_a_tron_coin() {
    let node = tron_node();
    block_on(async {
        for (_ctx, coin) in tron_coins(&node).await {
            let ticker = coin.ticker().to_owned();
            let req = RawTransactionRequest {
                coin: ticker.clone(),
                tx_hash: TX_HASH.to_owned(),
            };
            let err = coin
                .get_raw_transaction(req)
                .compat()
                .await
                .expect_err("a TRON coin is refused")
                .into_inner();
            assert_eq!(err.status_code(), StatusCode::NOT_IMPLEMENTED, "{ticker}");
            assert!(
                matches!(&err, RawTransactionError::NotImplemented { coin } if *coin == ticker),
                "{ticker}: {err}"
            );
        }
    });
    assert!(node.requests().is_empty(), "the refusal comes before any node request");
}

#[test]
fn get_raw_transaction_refuses_a_tron_coin_before_reading_the_hash() {
    let node = tron_node();
    block_on(async {
        for (_ctx, coin) in tron_coins(&node).await {
            let req = RawTransactionRequest {
                coin: coin.ticker().to_owned(),
                tx_hash: "not a hash".to_owned(),
            };
            let err = coin
                .get_raw_transaction(req)
                .compat()
                .await
                .expect_err("a TRON coin is refused")
                .into_inner();
            assert!(matches!(err, RawTransactionError::NotImplemented { .. }), "{err}");
        }
    });
    assert!(node.requests().is_empty());
}

#[test]
fn sign_raw_transaction_is_not_implemented_for_a_tron_coin() {
    let node = tron_node();
    block_on(async {
        for (_ctx, coin) in tron_coins(&node).await {
            let ticker = coin.ticker().to_owned();
            let err = coin
                .sign_raw_tx(&sign_raw_request(&ticker))
                .compat()
                .await
                .expect_err("a TRON coin is refused")
                .into_inner();
            assert_eq!(err.status_code(), StatusCode::NOT_IMPLEMENTED, "{ticker}");
            assert!(
                matches!(&err, RawTransactionError::NotImplemented { coin } if *coin == ticker),
                "{ticker}: {err}"
            );
        }
    });
    assert!(node.requests().is_empty(), "the refusal comes before any node request");
}

#[test]
fn legacy_get_trade_fee_refuses_a_tron_coin() {
    let node = tron_node();
    block_on(async {
        for (_ctx, coin) in tron_coins(&node).await {
            let err = coin.get_trade_fee().compat().await.expect_err("a TRON coin is refused");
            assert_eq!(err, "TRON trade fee is not supported", "{}", coin.ticker());
        }
    });
    assert!(node.requests().is_empty(), "the refusal comes before any node request");
}

#[test]
fn receiver_trade_fee_refuses_a_tron_coin() {
    let node = tron_node();
    block_on(async {
        for (_ctx, coin) in tron_coins(&node).await {
            let err = coin
                .get_receiver_trade_fee(FeeApproxStage::TradePreimage)
                .compat()
                .await
                .expect_err("a TRON coin is refused")
                .into_inner();
            assert!(
                matches!(&err, TradePreimageError::InternalError(text) if text == RECEIVER_FEE_REFUSAL),
                "{}: {err}",
                coin.ticker()
            );
        }
    });
    assert!(node.requests().is_empty(), "the refusal comes before any node request");
}

#[test]
fn fee_per_gas_estimation_is_not_supported_for_a_tron_coin() {
    let node = tron_node();
    block_on(async {
        for (ctx, coin) in tron_coins(&node).await {
            register(&ctx, &coin).await;
            for estimator_type in ["Simple", "Provider"] {
                let req = fee_estimation_request(coin.ticker(), estimator_type);
                let err = match get_eth_estimated_fee_per_gas(ctx.clone(), req).await {
                    Ok(_) => panic!("{} got a fee estimate", coin.ticker()),
                    Err(e) => e.into_inner(),
                };
                assert!(
                    matches!(err, GetFeeEstimationRequestError::CoinNotSupported),
                    "{}: {err}",
                    coin.ticker()
                );
                assert_eq!(err.status_code(), StatusCode::NOT_IMPLEMENTED);
            }
        }
    });
    assert!(node.requests().is_empty(), "the refusal comes before any node request");
}

#[test]
fn fee_per_gas_estimation_of_a_tron_coin_does_not_ask_the_gas_api() {
    let node = tron_node();
    let gas_api = StubNode::start(|_, _| Reply::status(500, ""));
    block_on(async {
        let conf = json!({ "gas_api": { "provider": "Infura", "url": gas_api.url() } });
        let ctx = MmCtxBuilder::new().with_conf(conf).into_mm_arc();
        let req = json!({ "urls": [node.url()] });
        let key = {
            let mut key = [0u8; 32];
            key[31] = 1;
            key
        };
        let coin = tron_coin_from_conf_and_request(&ctx, "TRX", &json!({}), &req, &key, CoinProtocol::TRX {
            network: Network::Nile,
        })
        .await
        .expect("a TRX coin activates against a stub node");
        register(&ctx, &coin).await;

        let err = match get_eth_estimated_fee_per_gas(ctx.clone(), fee_estimation_request("TRX", "Provider")).await {
            Ok(_) => panic!("TRX got a fee estimate"),
            Err(e) => e.into_inner(),
        };
        assert!(matches!(err, GetFeeEstimationRequestError::CoinNotSupported), "{err}");
    });
    assert!(gas_api.requests().is_empty(), "the gas API was not asked");
    assert!(node.requests().is_empty(), "the node was not asked");
}

#[test]
fn get_token_info_refuses_an_erc20_protocol_on_a_tron_platform() {
    let node = tron_node();
    block_on(async {
        for (ctx, coin) in tron_coins(&node).await {
            register(&ctx, &coin).await;
            let ticker = coin.ticker().to_owned();
            let err = get_token_info(ctx.clone(), erc20_on(&ticker))
                .await
                .expect_err("a TRON platform is refused")
                .into_inner();
            assert_eq!(err.status_code(), StatusCode::BAD_REQUEST, "{ticker}");
            assert!(
                matches!(&err, GetTokenInfoError::UnsupportedTokenProtocol(platform) if *platform == ticker),
                "{ticker}: {err}"
            );
        }
    });
    assert!(node.requests().is_empty(), "the refusal comes before any node request");
}

#[test]
fn get_token_info_keeps_refusing_a_trc20_protocol() {
    let node = tron_node();
    block_on(async {
        let (ctx, coin) = trx_coin(&node.url()).await;
        register(&ctx, &coin).await;
        let req = GetTokenInfoRequest {
            protocol: CoinProtocol::TRC20 {
                platform: "TRX".to_owned(),
                contract_address: TRC20_CONTRACT.to_owned(),
            },
        };
        let err = get_token_info(ctx.clone(), req)
            .await
            .expect_err("a TRC20 protocol is refused")
            .into_inner();
        assert!(matches!(err, GetTokenInfoError::UnsupportedTokenProtocol(_)), "{err}");
    });
    assert!(node.requests().is_empty());
}

/// An EVM platform coin whose failover provider and whose one single-node instance are stubs.
fn evm_coin(
    handler: impl Fn(&Call) -> EvmReply + Send + Sync + Clone + 'static,
) -> (MmArc, EthCoin, StubSender, StubSender) {
    let (web3, failover) = stub_provider(&["http://a.test"], 10., handler.clone());
    let (single, single_sender) = stub_provider(&["http://n1.test"], 30., handler);
    let (ctx, coin) = eth_coin_for_test_with_providers(
        EthCoinType::Eth,
        web3,
        vec![Web3Instance {
            web3: single,
            is_parity: false,
        }],
        None,
        mm2_net_config::SUPPORTED_NETIDS[0],
        18,
    );
    (ctx, coin, failover, single_sender)
}

#[test]
fn get_raw_transaction_of_an_evm_coin_still_asks_its_node() {
    let (_ctx, coin, failover, _single) = evm_coin(|call| EvmReply::Body(result_body(&call.request, "null")));
    let req = RawTransactionRequest {
        coin: "ETH".to_owned(),
        tx_hash: TX_HASH.to_owned(),
    };
    let err = block_on(coin.get_raw_transaction(req).compat())
        .expect_err("the node does not know the transaction")
        .into_inner();
    assert!(matches!(err, RawTransactionError::HashNotExist(_)), "{err}");
    assert_eq!(failover.hosts_for("eth_getTransactionByHash"), ["a.test"]);
}

#[test]
fn sign_raw_transaction_of_an_evm_coin_still_asks_its_node_for_the_nonce() {
    let (_ctx, coin, _failover, single) = evm_coin(|call| EvmReply::Body(result_body(&call.request, r#""0x5""#)));
    let signed = block_on(coin.sign_raw_tx(&sign_raw_request("ETH")).compat()).expect("an EVM coin signs");
    assert!(!signed.tx_hex.0.is_empty());
    assert!(!single.hosts_for("eth_getTransactionCount").is_empty());
}

#[test]
fn fee_per_gas_estimation_of_an_evm_coin_still_asks_its_node() {
    let (ctx, coin, failover, _single) =
        evm_coin(|call| EvmReply::Body(error_body(&call.request, -32000, "no fee history")));
    let err = block_on(async {
        register(&ctx, &coin).await;
        match get_eth_estimated_fee_per_gas(ctx.clone(), fee_estimation_request("ETH", "Simple")).await {
            Ok(_) => panic!("the stub node has no fee history"),
            Err(e) => e.into_inner(),
        }
    });
    assert!(matches!(err, GetFeeEstimationRequestError::Internal(_)), "{err}");
    assert!(!failover.hosts_for("eth_feeHistory").is_empty());
}
