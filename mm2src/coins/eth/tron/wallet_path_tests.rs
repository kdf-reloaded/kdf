//! Unit tests of the TRON wallet path (CRD ch.21 §21.9): the generic balance (`MarketCoinOps::my_balance`)
//! and the broadcast of a signed transaction (`send_raw_tx` / `send_raw_tx_bytes`) of a TRX or TRC20 coin.
//!
//! Every test builds the coin the way the legacy activation does (`tron_coin_from_conf_and_request`) against a
//! stub node on the loopback interface, so the real HTTP layer and the real coin code run; only the node's answers
//! are canned.

use super::test_node::{Reply, StubNode};
use super::withdraw::withdraw_tron;
use super::{tron_coin_from_conf_and_request, Network, TronAddress, TRX_DECIMALS};
use crate::closed_port::{closed_url, CLOSED_PORT};
use crate::eth::{EthCoin, EthCoinType, EvmWithdrawSender};
use crate::{BalanceError, CoinProtocol, MarketCoinOps, MmCoin, MmCoinEnum, WithdrawError, WithdrawFee, WithdrawRequest};
use bigdecimal::BigDecimal;
use common::block_on;
use ethereum_types::U256;
use futures::compat::Future01CompatExt;
use mm2_core::mm_ctx::{MmArc, MmCtxBuilder};
use serde_json::json;
use std::str::FromStr;

pub(super) const TRC20_CONTRACT: &str = "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t";
const TXID: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";
/// A stand-in for a signed protobuf transaction: the stub node never parses it.
const TX_BYTES: [u8; 4] = [0x0a, 0x02, 0xab, 0xcd];
const TX_HEX: &str = "0a02abcd";

fn priv_key() -> [u8; 32] {
    // Deterministic non-zero key (do not use in production).
    let mut key = [0u8; 32];
    key[31] = 1;
    key
}

/// A TRX coin activated against `node_url`. The context is returned so that it outlives the coin's use.
pub(super) async fn trx_coin(node_url: &str) -> (MmArc, EthCoin) { trx_coin_over(&[node_url]).await }

/// A TRX coin activated against the nodes `node_urls`, in this order.
async fn trx_coin_over(node_urls: &[&str]) -> (MmArc, EthCoin) {
    let ctx = MmCtxBuilder::new().into_mm_arc();
    let req = json!({ "urls": node_urls });
    let coin = tron_coin_from_conf_and_request(&ctx, "TRX", &json!({}), &req, &priv_key(), CoinProtocol::TRX {
        network: Network::Nile,
    })
    .await
    .expect("a TRX coin activates against a stub node");
    (ctx, coin)
}

/// A 6-decimal TRC20 coin activated against `node_url`.
pub(super) async fn trc20_coin(node_url: &str) -> (MmArc, EthCoin) { trc20_coin_of_decimals(node_url, 6).await }

/// A TRC20 coin of `decimals` decimals activated against `node_url`.
pub(super) async fn trc20_coin_of_decimals(node_url: &str, decimals: u8) -> (MmArc, EthCoin) {
    trc20_coin_over(&[node_url], decimals).await
}

/// A TRC20 coin of `decimals` decimals activated against the nodes `node_urls`. The coin puts them into an order
/// of its own.
pub(super) async fn trc20_coin_over(node_urls: &[&str], decimals: u8) -> (MmArc, EthCoin) {
    let ctx = MmCtxBuilder::new().into_mm_arc();
    let req = json!({ "urls": node_urls });
    let conf = json!({ "decimals": decimals });
    let coin = tron_coin_from_conf_and_request(&ctx, "USDT-TRC20", &conf, &req, &priv_key(), CoinProtocol::TRC20 {
        platform: "TRX".to_owned(),
        contract_address: TRC20_CONTRACT.to_owned(),
    })
    .await
    .expect("a TRC20 coin activates against a stub node");
    (ctx, coin)
}

/// The coin's own address in the base58 form the node API is asked with.
fn my_base58(coin: &EthCoin) -> String { TronAddress::from_evm_address(coin.my_address).to_base58() }

/// A node whose `getaccount` answers `reply` and which refuses every other endpoint.
fn getaccount_node(status: u16, body: &'static str) -> StubNode {
    StubNode::start(move |path, _| match path {
        "/wallet/getaccount" => Reply::status(status, body),
        other => Reply::status(404, &format!("unexpected endpoint {other}")),
    })
}

/// The generic wallet balance of a TRX coin against a node that answers `getaccount` with `status` and `body`.
fn trx_wallet_balance(status: u16, body: &'static str) -> Result<BigDecimal, BalanceError> {
    let node = getaccount_node(status, body);
    block_on(async {
        let (_ctx, coin) = trx_coin(&node.url()).await;
        MarketCoinOps::my_balance(&coin)
            .compat()
            .await
            .map(|balance| balance.spendable)
    })
    .map_err(|e| e.into_inner())
}

#[test]
fn trx_balance_is_read_from_getaccount() {
    let node = getaccount_node(200, r#"{"address":"x","balance":2500000,"create_time":1700000000000}"#);
    let (balance, raw, expected_address) = block_on(async {
        let (_ctx, coin) = trx_coin(&node.url()).await;
        let balance = MarketCoinOps::my_balance(&coin).compat().await.expect("balance");
        let raw = EthCoin::my_balance(&coin).compat().await.expect("raw balance");
        (balance, raw, my_base58(&coin))
    });
    assert_eq!(balance.spendable, BigDecimal::from_str("2.5").unwrap());
    assert_eq!(balance.unspendable, BigDecimal::from(0));
    assert_eq!(raw, U256::from(2_500_000u64));

    // The node was asked about the coin's own address, in base58.
    let bodies = node.bodies_for("/wallet/getaccount");
    assert!(!bodies.is_empty());
    for body in bodies {
        assert_eq!(body["address"], json!(expected_address));
        assert_eq!(body["visible"], json!(true));
    }
}

#[test]
fn trx_absent_account_reads_as_zero() {
    // The documented answer for an account that does not exist on chain is an empty object.
    let balance = trx_wallet_balance(200, "{}").expect("an absent account has a balance");
    assert_eq!(balance, BigDecimal::from(0));
}

#[test]
fn trx_balance_of_a_non_2xx_answer_is_an_error_not_zero() {
    // An error body that fits the account shape (`{}`) must not read as "absent account, balance 0".
    for (status, body) in [
        (500, "{}"),
        (429, r#"{"Error":"rate limit"}"#),
        (403, "{}"),
        (502, "<html>bad gateway</html>"),
    ] {
        match trx_wallet_balance(status, body) {
            Err(BalanceError::Transport(_)) => (),
            other => panic!("HTTP {status} must be a transport error, got {other:?}"),
        }
    }
}

#[test]
fn trx_balance_with_an_unreachable_node_is_an_error_not_zero() {
    let url = closed_url();
    let result = block_on(async {
        let (_ctx, coin) = trx_coin(&url).await;
        MarketCoinOps::my_balance(&coin).compat().await
    })
    .map_err(|e| e.into_inner());
    assert!(matches!(result, Err(BalanceError::Transport(_))), "{result:?}");
}

#[test]
fn trx_balance_with_an_undecodable_body_is_an_error_not_zero() {
    for body in ["not json", "[]", r#"{"balance":"many","create_time":1}"#] {
        match trx_wallet_balance(200, body) {
            Err(BalanceError::InvalidResponse(_)) => (),
            other => panic!("body {body:?} must be an invalid response, got {other:?}"),
        }
    }
}

#[test]
fn trx_negative_balance_is_an_error() {
    match trx_wallet_balance(200, r#"{"balance":-5,"create_time":1700000000000}"#) {
        Err(BalanceError::InvalidResponse(text)) => assert!(text.contains("negative"), "{text}"),
        other => panic!("a negative balance must be an error, got {other:?}"),
    }
}

/// A node whose `triggerconstantcontract` answers with `body`.
fn balance_of_node(body: String) -> StubNode {
    StubNode::start(move |path, _| match path {
        "/wallet/triggerconstantcontract" => Reply::ok(&body),
        other => Reply::status(404, &format!("unexpected endpoint {other}")),
    })
}

/// The wallet balance of a TRC20 coin against a node whose `balanceOf` answer is `body`.
fn trc20_wallet_balance(body: String) -> Result<BigDecimal, BalanceError> {
    let node = balance_of_node(body);
    block_on(async {
        let (_ctx, coin) = trc20_coin(&node.url()).await;
        MarketCoinOps::my_balance(&coin)
            .compat()
            .await
            .map(|balance| balance.spendable)
    })
    .map_err(|e| e.into_inner())
}

#[test]
fn trc20_balance_is_decoded_with_the_token_decimals() {
    let node = balance_of_node(format!(
        r#"{{"result":{{"result":true}},"constant_result":["{:064x}"],"energy_used":473}}"#,
        1_234_567u64
    ));
    let (balance, raw, my_address, contract) = block_on(async {
        let (_ctx, coin) = trc20_coin(&node.url()).await;
        let balance = MarketCoinOps::my_balance(&coin).compat().await.expect("balance");
        let raw = EthCoin::my_balance(&coin).compat().await.expect("raw balance");
        let contract = match &coin.coin_type {
            EthCoinType::Trc20 { token_addr, .. } => TronAddress::from_evm_address(*token_addr).to_base58(),
            other => panic!("expected a TRC20 coin, got {other:?}"),
        };
        (balance, raw, my_base58(&coin), contract)
    });
    // 6 decimals: 1_234_567 token units are 1.234567 tokens.
    assert_eq!(balance.spendable, BigDecimal::from_str("1.234567").unwrap());
    assert_eq!(balance.unspendable, BigDecimal::from(0));
    assert_eq!(raw, U256::from(1_234_567u64));
    assert_eq!(contract, TRC20_CONTRACT);

    // `balanceOf(address)` was called on the token contract for the coin's own address (one ABI word).
    let bodies = node.bodies_for("/wallet/triggerconstantcontract");
    assert!(!bodies.is_empty());
    let evm_address = hex::encode(TronAddress::from_base58(&my_address).unwrap().to_evm_address());
    for body in bodies {
        assert_eq!(body["owner_address"], json!(my_address));
        assert_eq!(body["contract_address"], json!(TRC20_CONTRACT));
        assert_eq!(body["function_selector"], json!("balanceOf(address)"));
        assert_eq!(body["parameter"], json!(format!("{}{}", "0".repeat(24), evm_address)));
    }
}

#[test]
fn trc20_zero_balance_is_zero() {
    let balance = trc20_wallet_balance(format!(r#"{{"constant_result":["{:064x}"]}}"#, 0)).expect("balance");
    assert_eq!(balance, BigDecimal::from(0));
}

#[test]
fn trc20_balance_without_a_usable_result_is_an_error_not_zero() {
    let reverted = format!("08c379a0{:064x}{:064x}{}", 32, 5, "ab".repeat(32));
    for body in [
        // No result at all, an empty list, and a node-side failure without a result.
        r#"{}"#.to_owned(),
        r#"{"constant_result":[]}"#.to_owned(),
        r#"{"result":{"result":false,"code":"CONTRACT_VALIDATE_ERROR","message":"6e6f"}}"#.to_owned(),
        // Not hex, shorter than one word, longer than one word (revert data).
        r#"{"constant_result":["zz"]}"#.to_owned(),
        r#"{"constant_result":["00ff"]}"#.to_owned(),
        format!(r#"{{"constant_result":["{reverted}"]}}"#),
    ] {
        match trc20_wallet_balance(body.clone()) {
            Err(BalanceError::InvalidResponse(_)) => (),
            other => panic!("body {body} must be an invalid response, got {other:?}"),
        }
    }
}

#[test]
fn trc20_balance_of_a_non_2xx_answer_is_an_error_not_zero() {
    let node = StubNode::start(|_, _| Reply::status(500, r#"{"constant_result":["00"]}"#));
    let result = block_on(async {
        let (_ctx, coin) = trc20_coin(&node.url()).await;
        MarketCoinOps::my_balance(&coin).compat().await
    })
    .map_err(|e| e.into_inner());
    assert!(matches!(result, Err(BalanceError::Transport(_))), "{result:?}");
}

#[test]
fn the_legacy_builder_builds_coins_against_a_stub_node() {
    let node = StubNode::start(|_, _| Reply::status(404, "{}"));
    block_on(async {
        let (_ctx, trx) = trx_coin(&node.url()).await;
        assert!(matches!(trx.coin_type, EthCoinType::Tron));
        assert_eq!(trx.decimals, TRX_DECIMALS);
        assert!(trx.tron_api.is_some());

        let (_ctx, token) = trc20_coin(&node.url()).await;
        assert!(matches!(token.coin_type, EthCoinType::Trc20 { .. }));
        assert_eq!(token.decimals, 6);
        assert!(token.tron_api.is_some());
    });
    // Building a coin sends nothing to the node.
    assert!(node.requests().is_empty());
}

/// The coins configuration of the wallet-only tests: a TRX and a TRC20 coin that say `"wallet_only": false`, the
/// same two that say `true`, and an EVM coin of each kind.
fn wallet_only_conf() -> serde_json::Value {
    let trc20 = json!({ "type": "TRC20", "protocol_data": { "platform": "TRX", "contract_address": TRC20_CONTRACT } });
    json!({ "coins": [
        { "coin": "TRX", "mm2": 1, "wallet_only": false,
          "protocol": { "type": "TRX", "protocol_data": { "network": "Nile" } } },
        { "coin": "USDT-TRC20", "mm2": 1, "decimals": 6, "wallet_only": false, "protocol": trc20 },
        { "coin": "TRXW", "mm2": 1, "wallet_only": true,
          "protocol": { "type": "TRX", "protocol_data": { "network": "Nile" } } },
        { "coin": "USDTW-TRC20", "mm2": 1, "decimals": 6, "wallet_only": true, "protocol": trc20 },
        { "coin": "ETH", "mm2": 1, "wallet_only": false, "protocol": { "type": "ETH" } },
        { "coin": "JST", "mm2": 1, "wallet_only": true, "protocol": { "type": "ERC20", "protocol_data": {
            "platform": "ETH", "contract_address": "0x2b294F029Fde858b2c62184e8390591755521d8E" } } },
    ] })
}

/// The TRON coin `ticker` of [`wallet_only_conf`], built against a node that is never asked.
async fn conf_coin(ctx: &MmArc, ticker: &str) -> EthCoin {
    let conf = crate::coin_conf(ctx, ticker);
    let protocol = CoinProtocol::from_conf_json(conf["protocol"].clone()).expect("a TRON protocol");
    let req = json!({ "urls": ["http://127.0.0.1:1"] });
    tron_coin_from_conf_and_request(ctx, ticker, &conf, &req, &priv_key(), protocol)
        .await
        .expect("a TRON coin builds without a node request")
}

/// CRD ch.21 R-D1, R21.10B.9, R21.B3: while TRON swaps are switched off a TRX or TRC20 coin is wallet-only whatever
/// its coins-config entry says; with the switch on the entry decides. An EVM coin follows its entry either way.
#[test]
fn a_tron_coin_is_wallet_only_whatever_its_config_says() {
    const { assert!(!crate::TRON_SWAPS_AVAILABLE) };
    let ctx = MmCtxBuilder::new().with_conf(wallet_only_conf()).into_mm_arc();
    block_on(async {
        for (ticker, config_says) in [
            ("TRX", false),
            ("USDT-TRC20", false),
            ("TRXW", true),
            ("USDTW-TRC20", true),
        ] {
            let coin = conf_coin(&ctx, ticker).await;
            assert!(coin.is_tron_family(), "{ticker}");
            assert!(MmCoin::wallet_only(&coin, &ctx), "{ticker}: the trait answer");
            assert!(
                MmCoinEnum::from(coin.clone()).wallet_only(&ctx),
                "{ticker}: the enum answer"
            );
            assert!(coin.wallet_only_with_switch(&ctx, false), "{ticker}: switch off");
            assert_eq!(
                coin.wallet_only_with_switch(&ctx, true),
                config_says,
                "{ticker}: switch on"
            );
        }
    });

    let (_eth_ctx, eth) =
        crate::eth::eth_tests::eth_coin_for_test(EthCoinType::Eth, vec!["http://127.0.0.1:1".to_owned()], None);
    let (_jst_ctx, jst) = crate::eth::eth_tests::eth_coin_for_test(
        EthCoinType::Erc20 {
            platform: "ETH".to_owned(),
            token_addr: Default::default(),
        },
        vec!["http://127.0.0.1:1".to_owned()],
        None,
    );
    for switch in [false, true] {
        assert!(!eth.wallet_only_with_switch(&ctx, switch), "ETH, switch {switch}");
        assert!(jst.wallet_only_with_switch(&ctx, switch), "JST, switch {switch}");
    }
    assert!(!MmCoin::wallet_only(&eth, &ctx));
    assert!(MmCoin::wallet_only(&jst, &ctx));
}

/// CRD ch.21 R-D1, R21.B3: while TRON swaps are switched off a TRX or TRC20 coin declines every new match, with
/// and without counterparty protocol information. An EVM coin accepts every counterparty, as before.
#[test]
fn a_tron_coin_declines_every_match_while_tron_swaps_are_off() {
    let ctx = MmCtxBuilder::new().with_conf(wallet_only_conf()).into_mm_arc();
    let infos = [None, Some(Vec::new()), Some(vec![1u8, 2, 3])];
    block_on(async {
        for ticker in ["TRX", "USDT-TRC20", "TRXW"] {
            let coin = conf_coin(&ctx, ticker).await;
            for info in &infos {
                assert!(!MmCoin::is_coin_protocol_supported(&coin, info), "{ticker} {info:?}");
            }
            assert!(!coin.protocol_supported_with_switch(false), "{ticker}");
            assert!(coin.protocol_supported_with_switch(true), "{ticker}");
        }
    });

    let (_ctx, eth) =
        crate::eth::eth_tests::eth_coin_for_test(EthCoinType::Eth, vec!["http://127.0.0.1:1".to_owned()], None);
    for info in &infos {
        assert!(MmCoin::is_coin_protocol_supported(&eth, info), "ETH {info:?}");
    }
    assert!(eth.protocol_supported_with_switch(false) && eth.protocol_supported_with_switch(true));
}

/// A node whose `broadcasthex` answers `body` (HTTP 200) and which refuses every other endpoint.
fn broadcast_node(status: u16, body: String) -> StubNode {
    StubNode::start(move |path, _| match path {
        "/wallet/broadcasthex" => Reply::status(status, &body),
        other => Reply::status(404, &format!("unexpected endpoint {other}")),
    })
}

/// The three ways to submit the same transaction: hex, `0x`-prefixed hex, raw bytes. The coin is a TRX coin
/// unless `trc20`.
fn broadcast_all_forms(node: &StubNode, trc20: bool) -> Vec<Result<String, String>> {
    block_on(async {
        let (_ctx, coin) = if trc20 {
            trc20_coin(&node.url()).await
        } else {
            trx_coin(&node.url()).await
        };
        vec![
            MarketCoinOps::send_raw_tx(&coin, TX_HEX).compat().await,
            MarketCoinOps::send_raw_tx(&coin, &format!("0x{TX_HEX}")).compat().await,
            MarketCoinOps::send_raw_tx_bytes(&coin, &TX_BYTES).compat().await,
        ]
    })
}

#[test]
fn send_raw_tx_returns_the_txid_of_an_accepted_broadcast() {
    for trc20 in [false, true] {
        // The node may write the id in capitals; the returned form is lowercase hex without a prefix.
        let node = broadcast_node(200, format!(r#"{{"result":true,"txid":"{}"}}"#, TXID.to_uppercase()));
        for result in broadcast_all_forms(&node, trc20) {
            assert_eq!(result.expect("an accepted broadcast returns its txid"), TXID);
        }
        // Every form reached `broadcasthex` as the same hex of the signed transaction, without `0x`.
        let bodies = node.bodies_for("/wallet/broadcasthex");
        assert_eq!(bodies.len(), 3);
        for body in bodies {
            assert_eq!(body["transaction"], json!(TX_HEX));
        }
        // Nothing but the broadcast was asked of the node.
        assert!(node.requests().iter().all(|(path, _)| path == "/wallet/broadcasthex"));
    }
}

#[test]
fn send_raw_tx_rejection_carries_the_node_code_and_message() {
    let node = broadcast_node(
        200,
        r#"{"result":false,"code":"SIGERROR","message":"validate signature error"}"#.to_owned(),
    );
    for result in broadcast_all_forms(&node, false) {
        let err = result.expect_err("a rejected broadcast is an error");
        assert!(err.contains("SIGERROR"), "{err}");
        assert!(err.contains("validate signature error"), "{err}");
    }
}

#[test]
fn send_raw_tx_without_a_usable_txid_is_an_error() {
    for body in [
        // Accepted but no id, an empty id, ids of the wrong shape.
        r#"{"result":true}"#,
        r#"{"result":true,"txid":""}"#,
        r#"{"result":true,"txid":"abcd"}"#,
        r#"{"result":true,"txid":"not hex at all not hex at all not hex at all not hex at all!!"}"#,
        // No success flag: an error body in 200 form must not read as an accepted transaction.
        r#"{"txid":"a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90"}"#,
        r#"{"Error":"something went wrong"}"#,
        "{}",
        "not json",
    ] {
        let node = broadcast_node(200, body.to_owned());
        for result in broadcast_all_forms(&node, false) {
            let err = result.expect_err(body);
            assert!(!err.is_empty(), "{body}");
        }
    }
}

#[test]
fn send_raw_tx_non_2xx_is_an_error_even_with_an_accepting_body() {
    let node = broadcast_node(500, format!(r#"{{"result":true,"txid":"{TXID}"}}"#));
    for result in broadcast_all_forms(&node, false) {
        assert!(
            result.is_err(),
            "an HTTP 500 answer was taken for an accepted broadcast"
        );
    }
}

#[test]
fn send_raw_tx_rejects_input_that_is_not_hex_before_any_request() {
    let node = broadcast_node(200, format!(r#"{{"result":true,"txid":"{TXID}"}}"#));
    let results = block_on(async {
        let (_ctx, coin) = trx_coin(&node.url()).await;
        vec![
            MarketCoinOps::send_raw_tx(&coin, "zz").compat().await,
            MarketCoinOps::send_raw_tx(&coin, "").compat().await,
            MarketCoinOps::send_raw_tx_bytes(&coin, &[]).compat().await,
        ]
    });
    for result in results {
        assert!(result.is_err());
    }
    assert!(node.requests().is_empty());
}

#[test]
fn broadcast_errors_name_no_node_url_or_key() {
    let url = format!("http://user:secret@127.0.0.1:{CLOSED_PORT}/APIKEY");
    let results = block_on(async {
        let (_ctx, coin) = trx_coin(&url).await;
        vec![
            MarketCoinOps::send_raw_tx(&coin, TX_HEX).compat().await,
            MarketCoinOps::send_raw_tx_bytes(&coin, &TX_BYTES).compat().await,
        ]
    });
    for result in results {
        let err = result.expect_err("an unreachable node is an error");
        for secret in ["user", "secret", "APIKEY"] {
            assert!(!err.contains(secret), "a credential part is rendered");
        }
    }
}

/// The address of the test key (secret = 1) in the TRON wire form: `41` and the 20-byte EVM address.
const KEY_ONE_WIRE_HEX: &str = "417e5f4552091a69125d5dfcb7b8c2659029395bdf";

#[test]
fn my_address_of_a_tron_coin_is_the_base58_display_form() {
    let node = StubNode::start(|_, _| Reply::status(404, "{}"));
    block_on(async {
        for (_ctx, coin) in [trx_coin(&node.url()).await, trc20_coin(&node.url()).await] {
            let address = MarketCoinOps::my_address(&coin).expect("address");
            assert_eq!(address.len(), 34, "{address}");
            assert!(address.starts_with('T'), "{address}");
            // Base58Check of the 21-byte wire form of the key's address.
            assert_eq!(
                TronAddress::from_base58(&address).expect("valid").to_hex(),
                KEY_ONE_WIRE_HEX
            );
            assert_eq!(address, my_base58(&coin));
            // The enable, my_balance and get_enabled_coins answers read it through the coin enum.
            let coin_enum = MmCoinEnum::from(coin.clone());
            let enum_address = coin_enum.my_address().expect("address");
            assert_eq!(enum_address, address);
        }
    });
}

#[test]
fn my_address_of_an_evm_coin_is_unchanged() {
    let (_ctx, coin) =
        crate::eth::eth_tests::eth_coin_for_test(EthCoinType::Eth, vec!["http://127.0.0.1:1".to_owned()], None);
    let address = MarketCoinOps::my_address(&coin).expect("address");
    assert_eq!(
        address,
        crate::eth::checksum_address(&format!("{:#02x}", coin.my_address))
    );
    assert!(address.starts_with("0x") && address.len() == 42, "{address}");
}

#[test]
fn validate_address_of_a_tron_coin_takes_the_tron_forms() {
    let node = StubNode::start(|_, _| Reply::status(404, "{}"));
    block_on(async {
        for (_ctx, coin) in [trx_coin(&node.url()).await, trc20_coin(&node.url()).await] {
            let own = MarketCoinOps::my_address(&coin).expect("address");
            for valid in [
                own.clone(),
                KEY_ONE_WIRE_HEX.to_owned(),
                format!("0x{KEY_ONE_WIRE_HEX}"),
            ] {
                let result = MmCoin::validate_address(&coin, &valid);
                assert!(result.is_valid, "{valid}: {:?}", result.reason);
            }
            // The EVM form, a damaged checksum and garbage are not TRON addresses.
            let mut damaged = own.clone();
            damaged.pop();
            damaged.push(if own.ends_with('1') { '2' } else { '1' });
            for invalid in [
                "0x7E5F4552091A69125d5DfCb7b8C2659029395Bdf".to_owned(),
                damaged,
                "not-an-address".to_owned(),
                String::new(),
            ] {
                let result = MmCoin::validate_address(&coin, &invalid);
                assert!(!result.is_valid, "{invalid}");
                assert!(result.reason.is_some());
            }
        }
    });
}

#[test]
fn validate_address_of_an_evm_coin_is_unchanged() {
    let (_ctx, coin) =
        crate::eth::eth_tests::eth_coin_for_test(EthCoinType::Eth, vec!["http://127.0.0.1:1".to_owned()], None);
    let own = MarketCoinOps::my_address(&coin).expect("address");
    assert!(MmCoin::validate_address(&coin, &own).is_valid);
    assert!(!MmCoin::validate_address(&coin, &my_tron_form()).is_valid);
}

/// The EVM `0x` form of the address of the test key (secret = 1), the one a TRON coin must refuse.
const KEY_ONE_EVM: &str = "0x7E5F4552091A69125d5DfCb7b8C2659029395Bdf";

#[test]
fn validate_address_of_a_tron_coin_pins_the_four_forms_of_t21_10b_14() {
    let node = StubNode::start(|_, _| Reply::status(404, "{}"));
    block_on(async {
        for (_ctx, coin) in [trx_coin(&node.url()).await, trc20_coin(&node.url()).await] {
            for valid in [
                my_tron_form(),
                KEY_ONE_WIRE_HEX.to_owned(),
                format!("0x{KEY_ONE_WIRE_HEX}"),
            ] {
                assert!(MmCoin::validate_address(&coin, &valid).is_valid, "{valid}");
            }
            let evm = MmCoin::validate_address(&coin, KEY_ONE_EVM);
            assert!(!evm.is_valid);
            assert!(evm.reason.is_some());
            let lower = MmCoin::validate_address(&coin, &KEY_ONE_EVM.to_lowercase());
            assert!(!lower.is_valid);
            assert!(lower.reason.is_some());
        }
    });
}

#[test]
fn convertaddress_of_a_tron_coin_converts_between_base58_and_hex() {
    let node = StubNode::start(|_, _| Reply::status(404, "{}"));
    let base58 = my_tron_form();
    block_on(async {
        for (_ctx, coin) in [trx_coin(&node.url()).await, trc20_coin(&node.url()).await] {
            let to_hex = json!({ "format": "hex" });
            let to_base58 = json!({ "format": "base58" });
            assert_eq!(
                coin.convert_to_address(&base58, to_hex.clone()),
                Ok(KEY_ONE_WIRE_HEX.to_owned())
            );
            assert_eq!(
                coin.convert_to_address(KEY_ONE_WIRE_HEX, to_base58.clone()),
                Ok(base58.clone())
            );
            // `0x41...` is an input form too; the output never carries `0x`.
            assert_eq!(
                coin.convert_to_address(&format!("0x{KEY_ONE_WIRE_HEX}"), to_hex.clone()),
                Ok(KEY_ONE_WIRE_HEX.to_owned())
            );
            assert_eq!(
                coin.convert_to_address(&format!("0x{KEY_ONE_WIRE_HEX}"), to_base58.clone()),
                Ok(base58.clone())
            );
            // Identity conversions.
            assert_eq!(coin.convert_to_address(&base58, to_base58), Ok(base58.clone()));
            assert_eq!(
                coin.convert_to_address(KEY_ONE_WIRE_HEX, to_hex),
                Ok(KEY_ONE_WIRE_HEX.to_owned())
            );
        }
    });
}

#[test]
fn convertaddress_of_a_tron_coin_refuses_the_evm_form_and_the_evm_format_values() {
    let node = StubNode::start(|_, _| Reply::status(404, "{}"));
    block_on(async {
        let (_ctx, coin) = trx_coin(&node.url()).await;
        // An EVM `0x` 20-byte input is refused in both TRON formats.
        for format in ["base58", "hex"] {
            for evm in [KEY_ONE_EVM.to_owned(), KEY_ONE_EVM.to_lowercase()] {
                assert!(
                    coin.convert_to_address(&evm, json!({ "format": format })).is_err(),
                    "{evm}"
                );
            }
        }
        // A damaged checksum and garbage are refused.
        let mut damaged = my_tron_form();
        damaged.pop();
        damaged.push('1');
        for bad in [damaged.as_str(), "not-an-address", ""] {
            assert!(
                coin.convert_to_address(bad, json!({ "format": "hex" })).is_err(),
                "{bad}"
            );
        }
        // The EVM format values, an unknown value and a missing one are no TRON formats.
        for format in [
            json!({ "format": "mixedcase" }),
            json!({ "format": "singlecase" }),
            json!({ "format": "cashaddress" }),
            json!({}),
            json!("hex"),
        ] {
            let err = coin
                .convert_to_address(&my_tron_form(), format.clone())
                .expect_err("a non-TRON format value is refused");
            assert!(err.contains("base58"), "{format}: {err}");
        }
    });
}

#[test]
fn convertaddress_of_an_evm_coin_is_unchanged_and_takes_no_tron_format() {
    let (_ctx, coin) =
        crate::eth::eth_tests::eth_coin_for_test(EthCoinType::Eth, vec!["http://127.0.0.1:1".to_owned()], None);
    let lower = KEY_ONE_EVM.to_lowercase();
    assert_eq!(
        coin.convert_to_address(&lower, json!({ "format": "mixedcase" })),
        Ok(crate::eth::checksum_address(&lower))
    );
    assert!(coin
        .convert_to_address(&lower, json!({ "format": "singlecase" }))
        .is_err());
    // The TRON format values are not EVM formats.
    assert!(coin.convert_to_address(&lower, json!({ "format": "base58" })).is_err());
    assert!(coin.convert_to_address(&lower, json!({ "format": "hex" })).is_err());
    // A TRON address is not an EVM input.
    assert!(coin
        .convert_to_address(&my_tron_form(), json!({ "format": "mixedcase" }))
        .is_err());
}

#[test]
fn show_priv_key_of_a_tron_coin_is_the_evm_form_of_the_key() {
    let node = StubNode::start(|_, _| Reply::status(404, "{}"));
    // Secret = 1: `0x` and 64 lowercase hex digits.
    let expected = format!("0x{}01", "0".repeat(62));
    block_on(async {
        for (_ctx, coin) in [trx_coin(&node.url()).await, trc20_coin(&node.url()).await] {
            let shown = MarketCoinOps::display_priv_key(&coin).expect("a local key is shown");
            assert_eq!(shown, expected);
            assert_eq!(shown.len(), 66);
        }
    });
}

/// The base58 TRON address of the test key.
fn my_tron_form() -> String {
    TronAddress::from_hex(KEY_ONE_WIRE_HEX)
        .expect("valid wire form")
        .to_base58()
}

fn withdraw_request(coin: &EthCoin, fee: Option<WithdrawFee>) -> WithdrawRequest {
    WithdrawRequest {
        memo: None,
        expiration_seconds: None,
        fee_method: None,
        gasless: None,
        coin: coin.ticker.clone(),
        from: None,
        to: "TNPeeaaFB7K9cmo4uQpcU32zGK8G1NYqeL".to_owned(),
        amount: BigDecimal::from(1),
        max: false,
        fee,
    }
}

/// A node that serves a funded account and a `balanceOf` word of 5 tokens, and refuses the rest.
fn funded_node() -> StubNode {
    StubNode::start(|path, _| match path {
        "/wallet/getaccount" => Reply::ok(r#"{"balance":50000000,"create_time":1700000000000}"#),
        "/wallet/triggerconstantcontract" => Reply::ok(&format!(r#"{{"constant_result":["{:064x}"]}}"#, 5_000_000u64)),
        other => Reply::status(404, &format!("unexpected endpoint {other}")),
    })
}

#[test]
fn withdraw_with_any_fee_descriptor_is_refused_before_a_node_is_asked() {
    let node = funded_node();
    block_on(async {
        for (_ctx, coin) in [trx_coin(&node.url()).await, trc20_coin(&node.url()).await] {
            let fees = [
                WithdrawFee::EthGas {
                    gas_price: BigDecimal::from(1),
                    gas: 21_000,
                },
                WithdrawFee::UtxoFixed {
                    amount: BigDecimal::from_str("0.0001").unwrap(),
                },
                WithdrawFee::UtxoPerKbyte {
                    amount: BigDecimal::from_str("0.0001").unwrap(),
                },
                WithdrawFee::Qrc20Gas {
                    gas_limit: 2_500_000,
                    gas_price: 40,
                },
                WithdrawFee::Tron,
            ];
            for fee in fees {
                let req = withdraw_request(&coin, Some(fee));
                match withdraw_tron(coin.clone(), req, EvmWithdrawSender::activated(&coin)).await {
                    Err(e) => assert!(
                        matches!(e.get_inner(), WithdrawError::InvalidFeePolicy(_)),
                        "a fee descriptor must answer InvalidFeePolicy"
                    ),
                    Ok(_) => panic!("a TRON withdraw with a fee descriptor must be refused"),
                }
            }
        }
    });
    assert!(
        node.requests().is_empty(),
        "the refusal must come before any node request"
    );
}

/// CRD ch.21 R-WP6: a `fee` of the form a TRON result's `fee_details` has (`"type": "Tron"`, CRD ch.21 R-F2), with
/// or without the other members of that object, parses as a fee descriptor, so the request reaches the withdraw
/// and is refused there with `InvalidFeePolicy` instead of failing request parsing.
#[test]
fn withdraw_request_with_a_tron_fee_descriptor_parses_and_is_refused() {
    let node = funded_node();
    block_on(async {
        let (_ctx, coin) = trx_coin(&node.url()).await;
        let fees = [
            json!({"type": "Tron"}),
            json!({
                "type": "Tron",
                "coin": "TRX",
                "bandwidth_used": 267,
                "energy_used": 0,
                "bandwidth_fee": "0.267",
                "energy_fee": "0",
                "total_fee": "0.267"
            }),
        ];
        for fee in fees {
            let req: WithdrawRequest = serde_json::from_value(json!({
                "coin": coin.ticker.clone(),
                "to": "TNPeeaaFB7K9cmo4uQpcU32zGK8G1NYqeL",
                "amount": "1",
                "fee": fee,
            }))
            .expect("a withdraw request with a Tron fee descriptor must parse");
            assert_eq!(req.fee, Some(WithdrawFee::Tron));
            match withdraw_tron(coin.clone(), req, EvmWithdrawSender::activated(&coin)).await {
                Err(e) => assert!(
                    matches!(e.get_inner(), WithdrawError::InvalidFeePolicy(_)),
                    "a Tron fee descriptor must answer InvalidFeePolicy"
                ),
                Ok(_) => panic!("a TRON withdraw with a Tron fee descriptor must be refused"),
            }
        }
    });
    assert!(
        node.requests().is_empty(),
        "the refusal must come before any node request"
    );
}

#[test]
fn withdraw_without_a_fee_descriptor_still_reaches_the_node() {
    for trc20 in [false, true] {
        let node = funded_node();
        let result = block_on(async {
            let (_ctx, coin) = if trc20 {
                trc20_coin(&node.url()).await
            } else {
                trx_coin(&node.url()).await
            };
            withdraw_tron(
                coin.clone(),
                withdraw_request(&coin, None),
                EvmWithdrawSender::activated(&coin),
            )
            .await
        });
        // The stub refuses the endpoints after the balance read, so the withdraw ends in an error, but not the
        // fee-policy one, and only after the node was asked for the balance.
        if let Err(e) = result {
            assert!(!matches!(e.get_inner(), WithdrawError::InvalidFeePolicy(_)));
        }
        let first = node.requests().into_iter().next().expect("the node was asked").0;
        let expected = if trc20 {
            "/wallet/triggerconstantcontract"
        } else {
            "/wallet/getaccount"
        };
        assert_eq!(first, expected);
    }
}

pub(super) const NOW_BLOCK_BODY: &str = r#"{"blockID":"0000000003456789abcdef0123456789abcdef0123456789abcdef0123456789","block_header":{"raw_data":{"number":58251145,"txTrieRoot":"0000000000000000000000000000000000000000000000000000000000000000","witness_address":"41ab","parentHash":"0000000000000000000000000000000000000000000000000000000000000000","version":32,"timestamp":1700000000000},"witness_signature":"ab"},"transactions":[]}"#;

/// A node that serves the withdraw's endpoints with the answer forms of the public node.
fn withdraw_node() -> StubNode {
    StubNode::start(|path, _| match path {
        "/wallet/getaccount" => Reply::ok(r#"{"balance":50000000,"create_time":1700000000000}"#),
        "/wallet/triggerconstantcontract" => Reply::ok(&format!(
            r#"{{"constant_result":["{:064x}"],"energy_used":14000}}"#,
            5_000_000u64
        )),
        "/wallet/getnowblock" => Reply::ok(NOW_BLOCK_BODY),
        "/wallet/getchainparameters" => Reply::ok(
            r#"{"chainParameter":[{"key":"getTransactionFee","value":1000},{"key":"getEnergyFee","value":420}]}"#,
        ),
        "/wallet/getaccountresource" => Reply::ok(r#"{"freeNetLimit":600,"NetLimit":0,"EnergyLimit":0}"#),
        other => Reply::status(404, &format!("unexpected endpoint {other}")),
    })
}

#[test]
fn withdraw_gets_past_tapos_with_the_nodes_getnowblock_form() {
    for trc20 in [false, true] {
        let node = withdraw_node();
        let result = block_on(async {
            let (_ctx, coin) = if trc20 {
                trc20_coin(&node.url()).await
            } else {
                trx_coin(&node.url()).await
            };
            withdraw_tron(
                coin.clone(),
                withdraw_request(&coin, None),
                EvmWithdrawSender::activated(&coin),
            )
            .await
        });
        let details = result.unwrap_or_else(|e| panic!("the withdraw must succeed (trc20={trc20}): {e:?}"));
        assert!(!details.tx_hex.0.is_empty());
        assert!(node.bodies_for("/wallet/getnowblock").len() == 1);
    }
}

const NODE_ERROR_BODY: &str = r#"{"Error":"class org.tron.api.ParseException : invalid address for field"}"#;

#[test]
fn trx_balance_of_a_node_error_answer_is_an_error_not_zero() {
    match trx_wallet_balance(200, NODE_ERROR_BODY) {
        Err(BalanceError::InvalidResponse(text)) => assert!(text.contains("invalid address"), "{text}"),
        other => panic!("a node error answer must be an error, got {other:?}"),
    }
}

#[test]
fn trx_balance_of_an_account_answer_without_balance_or_creation_time_is_an_error() {
    for body in [r#"{"address":"x"}"#, r#"{"latest_opration_time":1}"#] {
        match trx_wallet_balance(200, body) {
            Err(BalanceError::InvalidResponse(_)) => (),
            other => panic!("body {body} must be an invalid response, got {other:?}"),
        }
    }
}

#[test]
fn trx_account_with_a_balance_but_no_creation_time_exists() {
    let balance = trx_wallet_balance(200, r#"{"balance":3000000}"#).expect("an account with a balance exists");
    assert_eq!(balance, BigDecimal::from(3));
}

#[test]
fn trx_balance_falls_over_to_the_next_node_on_a_node_error_answer() {
    let failing = getaccount_node(200, NODE_ERROR_BODY);
    let healthy = getaccount_node(200, r#"{"balance":2500000,"create_time":1700000000000}"#);
    let balance = block_on(async {
        let (_ctx, coin) = trx_coin_over(&[&failing.url(), &healthy.url()]).await;
        MarketCoinOps::my_balance(&coin)
            .compat()
            .await
            .expect("the second node answers")
    });
    assert_eq!(balance.spendable, BigDecimal::from_str("2.5").unwrap());
    // The activation order is random: the failing node is asked first or not at all, the healthy one exactly once.
    assert!(failing.bodies_for("/wallet/getaccount").len() <= 1);
    assert_eq!(healthy.bodies_for("/wallet/getaccount").len(), 1);
}

#[test]
fn trc20_balance_of_a_contract_call_without_constant_result_is_an_error() {
    // The measured answer of a contract call on an address that is not a contract.
    let body = r#"{"result":{"code":"CONTRACT_VALIDATE_ERROR","message":"x"}}"#;
    match trc20_wallet_balance(body.to_owned()) {
        Err(BalanceError::InvalidResponse(_)) => (),
        other => panic!("a contract call without a result must be an error, got {other:?}"),
    }
    // A node error answer on the contract endpoint is an error too.
    assert!(trc20_wallet_balance(NODE_ERROR_BODY.to_owned()).is_err());
}

#[test]
fn send_raw_tx_of_a_node_error_answer_is_an_error_without_a_txid() {
    let node = broadcast_node(200, NODE_ERROR_BODY.to_owned());
    for result in broadcast_all_forms(&node, false) {
        let err = result.expect_err("a node error answer is not an accepted broadcast");
        assert!(err.contains("invalid address"), "{err}");
    }
}

#[test]
fn withdraw_balance_read_of_a_node_error_answer_is_an_error_not_zero() {
    let node = getaccount_node(200, NODE_ERROR_BODY);
    let result = block_on(async {
        let (_ctx, coin) = trx_coin(&node.url()).await;
        withdraw_tron(
            coin.clone(),
            withdraw_request(&coin, None),
            EvmWithdrawSender::activated(&coin),
        )
        .await
    });
    match result {
        Err(e) => assert!(matches!(e.get_inner(), WithdrawError::Transport(_))),
        Ok(_) => panic!("a node error answer must not read as a balance"),
    }
    // Nothing past the balance read was attempted.
    assert!(node.requests().iter().all(|(path, _)| path == "/wallet/getaccount"));
}

/// A node with an account of 2.5 TRX, a token balance and a head block.
fn wallet_node() -> StubNode {
    StubNode::start(|path, _| match path {
        "/wallet/getaccount" => Reply::ok(r#"{"balance":2500000,"create_time":1700000000000}"#),
        "/wallet/triggerconstantcontract" => Reply::ok(&format!(r#"{{"constant_result":["{:064x}"]}}"#, 777u64)),
        "/wallet/getnowblock" => Reply::ok(&format!(
            r#"{{"blockID":"{}","block_header":{{"raw_data":{{"number":4242,"timestamp":1700000000000}}}}}}"#,
            "ab".repeat(32)
        )),
        other => Reply::status(404, &format!("unexpected endpoint {other}")),
    })
}

/// CRD ch.21 R-WP1 and the platform balance of a token: `base_coin_balance` is the TRX balance at 6 decimals, for
/// TRX and for the platform of a TRC20 token, whatever the token's own decimals are.
#[test]
fn base_coin_balance_is_the_trx_balance_at_6_decimals_for_trx_and_trc20() {
    let node = wallet_node();
    block_on(async {
        let (_ctx, trx) = trx_coin(&node.url()).await;
        let balance = MarketCoinOps::base_coin_balance(&trx).compat().await.expect("balance");
        assert_eq!(balance, BigDecimal::from_str("2.5").unwrap());

        // A token of 8 decimals: its own balance is read at 8, the platform balance still at 6.
        let ctx = MmCtxBuilder::new().into_mm_arc();
        let req = json!({ "urls": [node.url()] });
        let token = tron_coin_from_conf_and_request(
            &ctx,
            "USDT-TRC20",
            &json!({ "decimals": 8 }),
            &req,
            &priv_key(),
            CoinProtocol::TRC20 {
                platform: "TRX".to_owned(),
                contract_address: TRC20_CONTRACT.to_owned(),
            },
        )
        .await
        .expect("a TRC20 coin activates against a stub node");
        let base = MarketCoinOps::base_coin_balance(&token)
            .compat()
            .await
            .expect("balance");
        assert_eq!(base, BigDecimal::from_str("2.5").unwrap());
        let own = MarketCoinOps::my_balance(&token).compat().await.expect("balance");
        assert_eq!(own.spendable, BigDecimal::from_str("0.00000777").unwrap());
    });
}

#[test]
fn base_coin_balance_of_a_failed_read_is_an_error() {
    let node = getaccount_node(500, "{}");
    let result = block_on(async {
        let (_ctx, coin) = trx_coin(&node.url()).await;
        MarketCoinOps::base_coin_balance(&coin).compat().await
    })
    .map_err(|e| e.into_inner());
    assert!(matches!(result, Err(BalanceError::Transport(_))), "{result:?}");
}

/// CRD ch.21 R21.10B.5: the head block of a TRON coin is the `getnowblock` number, not an EVM node's.
#[test]
fn the_current_block_of_a_tron_coin_comes_from_getnowblock() {
    let node = wallet_node();
    let block = block_on(async {
        let (_ctx, coin) = trx_coin(&node.url()).await;
        MarketCoinOps::current_block(&coin).compat().await
    });
    assert_eq!(block, Ok(4242));
    assert!(!node.bodies_for("/wallet/getnowblock").is_empty());

    let down = getaccount_node(200, "{}");
    let failed = block_on(async {
        let (_ctx, coin) = trx_coin(&down.url()).await;
        MarketCoinOps::current_block(&coin).compat().await
    });
    assert!(failed.is_err(), "{failed:?}");
}

/// CRD ch.21 R-AD1: a TRON coin parses both address forms where it parses an address; the EVM form is refused.
#[test]
fn a_tron_coin_parses_both_address_forms_and_refuses_the_evm_form() {
    let node = wallet_node();
    block_on(async {
        for (_ctx, coin) in [trx_coin(&node.url()).await, trc20_coin(&node.url()).await] {
            let parsed = TronAddress::from_base58(TRC20_CONTRACT).expect("valid");
            let wire = parsed.to_hex();
            for form in [TRC20_CONTRACT.to_owned(), wire.clone(), format!("0x{wire}")] {
                assert_eq!(coin.address_from_str(&form), Ok(parsed.to_evm_address()), "{form}");
            }
            let evm = format!("0x{}", hex::encode(parsed.to_evm_address()));
            assert!(coin.address_from_str(&evm).is_err(), "the EVM form is refused");
            assert!(coin.address_from_str("").is_err());
        }
    });
}

/// CRD ch.21 R21.10B.7 "Routes", ch.49 R49.9: `task::withdraw` takes TRX and TRC20 for a key-local coin.
#[test]
fn the_withdraw_check_of_the_family_admits_trx_and_trc20() {
    let node = wallet_node();
    let req: WithdrawRequest = serde_json::from_value(json!({ "coin": "TRX", "to": TRC20_CONTRACT, "amount": "1" }))
        .expect("a withdraw request");
    block_on(async {
        for (_ctx, coin) in [trx_coin(&node.url()).await, trc20_coin(&node.url()).await] {
            crate::eth::validate_evm_withdraw_request(&coin, &req).expect("TRON is in the family");
        }
    });
}

/// `verify_message` of a TRX or TRC20 coin is unchanged: the address is read with the EVM parser, so a TRON address
/// (base58check or `41...` hex) is refused with `AddressDecodingError` and never verified with the EVM message
/// scheme; an EVM `0x` address is still parsed, as before.
#[test]
fn verify_message_of_a_tron_coin_reads_the_address_with_the_evm_parser() {
    use crate::VerificationError;
    let node = wallet_node();
    let signature = format!("0x{}", "00".repeat(65));
    block_on(async {
        for coin in [{
            let ctx = MmCtxBuilder::new().into_mm_arc();
            let req = json!({ "urls": [node.url()] });
            let conf = json!({ "sign_message_prefix": "Tron Signed Message:\n" });
            let coin = tron_coin_from_conf_and_request(&ctx, "TRX", &conf, &req, &priv_key(), CoinProtocol::TRX {
                network: Network::Nile,
            })
            .await
            .expect("a TRX coin activates against a stub node");
            (ctx, coin)
        }] {
            let (_ctx, coin) = coin;
            let parsed = TronAddress::from_base58(TRC20_CONTRACT).expect("valid");
            for form in [
                TRC20_CONTRACT.to_owned(),
                parsed.to_hex(),
                format!("0x{}", parsed.to_hex()),
            ] {
                let result =
                    MarketCoinOps::verify_message(&coin, &signature, "message", &form).map_err(|e| e.into_inner());
                assert!(
                    matches!(result, Err(VerificationError::AddressDecodingError(_))),
                    "{form}: {result:?}"
                );
            }
            let evm = crate::eth::checksum_address(&format!("0x{}", hex::encode(parsed.to_evm_address())));
            let result = MarketCoinOps::verify_message(&coin, &signature, "message", &evm).map_err(|e| e.into_inner());
            assert!(
                !matches!(result, Err(VerificationError::AddressDecodingError(_))),
                "the EVM form is still parsed: {result:?}"
            );
        }
    });
}
