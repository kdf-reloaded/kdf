//! Tests of the GasFree settings of an activation and of the custody address a report shows (CRD ch.63 T9, T10,
//! R9, R18, R29).
//!
//! The node and the provider are loopback stubs. The coins are built the way the mmrpc 2.0 activation builds them
//! (`tron_coin_from_params`). TRON activation is on (`TRON_ACTIVATION_AVAILABLE`), so what these tests reach is
//! reachable in a build.
//!
//! No assertion message of this module carries a credential, a rendering or an error text: only labels.

use super::super::super::nodes::TronNode;
use super::super::super::test_node::{Reply, StubNode};
use super::super::super::{parse_trc20_contract, tron_coin_from_params, TronCoinParams};
use super::super::address_word;
use super::super::custody::mismatch_warning;
use super::super::test_vectors::{address, USERS};
use super::*;
use crate::eth::alloy_compat::transport::test_support::{captured_log_lines, install_log_capture};
use crate::eth::eth_tests::eth_coin_for_test;
use crate::eth::{Erc20BuildError, EthCoinBuildError, EthCoinType};
use crate::{lp_register_coin, CoinProtocol, MmCoinEnum, RegisterCoinParams};
use common::block_on;
use mm2_core::mm_ctx::{MmArc, MmCtxBuilder};
use mm2_err_handle::prelude::*;
use serde_json::json;
use std::str::FromStr;

const API_KEY: &str = "key-0f3a9c";
const API_SECRET: &str = "secret-7b1d4e";
const TRC20_CONTRACT: &str = "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t";
const GET_GASFREE_ADDRESS: &str = "getGasFreeAddress(address)";

fn assert_no_credential(text: &str) {
    assert!(!text.contains(API_KEY), "the API key is rendered");
    assert!(!text.contains(API_SECRET), "the API secret is rendered");
}

/// The `tron_gasless_provider` object of a request for the provider at `base_url`, with the `extra` members
/// replacing or adding to the usual ones.
fn provider_json(base_url: &str, extra: Json) -> Json {
    let mut object = json!({
        "base_url": base_url,
        "api_key": API_KEY,
        "api_secret": API_SECRET,
        "service_provider": USERS[4].address,
    });
    for (key, value) in extra.as_object().expect("an object").iter() {
        object[key] = value.clone();
    }
    object
}

fn provider_request(base_url: &str, extra: Json) -> TronGaslessProviderRequest {
    serde_json::from_value(provider_json(base_url, extra)).expect("a provider object")
}

fn binding(network: &Network, base_url: &str) -> GasFreeBinding {
    GasFreeBinding::from_request(network, &provider_request(base_url, json!({}))).expect("a valid provider object")
}

fn gasless_request(value: Json) -> TronTokenGaslessRequest { serde_json::from_value(value).expect("a gasless block") }

/// What the stub node's controller contract answers to `getGasFreeAddress`.
#[derive(Clone, Copy)]
enum Controller {
    /// The address this build derives for the asked user on `Network::Nile`.
    Agrees,
    /// Another address than the derived one: the one the mainnet constants give.
    Disagrees,
    /// HTTP 500.
    Down,
}

/// A 32-byte ABI word holding `value`.
fn word(value: u64) -> String { format!("{value:064x}") }

/// A Nile node: an account of 2.5 TRX, a 6-decimal token with a balance of 1.234567 at every address, and the
/// controller contract of `controller`.
fn node(controller: Controller) -> StubNode {
    StubNode::start(move |path, body| {
        let request: Json = serde_json::from_str(body).unwrap_or(Json::Null);
        match path {
            "/wallet/getaccount" => Reply::ok(r#"{"balance":2500000,"create_time":1700000000000}"#),
            "/wallet/triggerconstantcontract" => match request["function_selector"].as_str() {
                Some("decimals()") => Reply::ok(&json!({ "constant_result": [word(6)] }).to_string()),
                Some("balanceOf(address)") => Reply::ok(&json!({ "constant_result": [word(1_234_567)] }).to_string()),
                Some(GET_GASFREE_ADDRESS) => {
                    // The parameter is one ABI word: 12 zero bytes and the 20-byte user.
                    let parameter = request["parameter"].as_str().unwrap_or_default();
                    let Some(user) = parameter
                        .get(24..)
                        .and_then(|payload| TronAddress::from_hex(&format!("41{payload}")).ok())
                    else {
                        return Reply::status(400, "{}");
                    };
                    let answer = match controller {
                        Controller::Agrees => gasfree_custody_address(&Network::Nile, &user),
                        Controller::Disagrees => gasfree_custody_address(&Network::Mainnet, &user),
                        Controller::Down => return Reply::status(500, "{}"),
                    };
                    Reply::ok(&json!({ "constant_result": [hex::encode(address_word(&answer))] }).to_string())
                },
                _ => Reply::status(404, "{}"),
            },
            other => Reply::status(404, &format!("unexpected endpoint {other}")),
        }
    })
}

/// The number of `getGasFreeAddress` reads the node received.
fn controller_reads(node: &StubNode) -> usize {
    node.bodies_for("/wallet/triggerconstantcontract")
        .iter()
        .filter(|body| body["function_selector"] == GET_GASFREE_ADDRESS)
        .count()
}

fn api(node: &StubNode) -> TronApiClient {
    TronApiClient::new_with_cutoff_s("TRX", vec![TronNode::plain(node.url())], 5.)
}

fn priv_key() -> [u8; 32] {
    // Deterministic non-zero key (do not use in production).
    let mut key = [0u8; 32];
    key[31] = 1;
    key
}

fn ctx() -> MmArc {
    MmCtxBuilder::new()
        .with_conf(json!({
            "coins": [
                {
                    "coin": "TRX",
                    "protocol": { "type": "TRX", "protocol_data": { "network": "Nile" } },
                    "wallet_only": true,
                    "mm2": 1
                },
                {
                    "coin": "USDT-TRC20",
                    "protocol": {
                        "type": "TRC20",
                        "protocol_data": { "platform": "TRX", "contract_address": TRC20_CONTRACT }
                    },
                    "decimals": 6,
                    "mm2": 1
                }
            ]
        }))
        .into_mm_arc()
}

/// A single-key Nile TRX platform coin over `node`, with the provider binding `gasfree`.
async fn platform(ctx: &MmArc, node: &StubNode, gasfree: Option<GasFreeBinding>) -> EthCoin {
    let params = TronCoinParams {
        nodes: vec![TronNode::plain(node.url())],
        required_confirmations: 1,
        tx_history: false,
        gasfree,
        swap_contracts: None,
        swap_evidence_node: None,
    };
    tron_coin_from_params(
        ctx,
        "TRX",
        &crate::coin_conf(ctx, "TRX"),
        params,
        &priv_key(),
        CoinProtocol::TRX { network: Network::Nile },
    )
    .await
    .unwrap_or_else(|_| panic!("the platform coin must build"))
}

async fn register(ctx: &MmArc, coin: &EthCoin, ticker: &str) {
    let registered = lp_register_coin(ctx, MmCoinEnum::EthCoin(coin.clone()), RegisterCoinParams {
        ticker: ticker.to_owned(),
        tx_history: false,
    })
    .await;
    assert!(registered.is_ok(), "the coin registers");
}

async fn token(
    platform: &EthCoin,
    gasless: Option<&TronTokenGaslessRequest>,
) -> Result<EthCoin, MmError<Erc20BuildError>> {
    let contract = parse_trc20_contract(TRC20_CONTRACT).expect("a contract");
    platform
        .trc20_token_from_conf_or_contract("USDT-TRC20".to_owned(), contract, 1, gasless)
        .await
}

// ---------------------------------------------------------------------------------------------------------------
// ch.63 T9: the provider object
// ---------------------------------------------------------------------------------------------------------------

/// ch.63 T9, R20: a host-only `base_url` is accepted, and an absent timeout or poll interval is the documented
/// default.
#[test]
fn t9_a_host_only_base_url_is_accepted_and_the_defaults_are_those_of_r20() {
    for base_url in [
        "https://open.gasfree.io",
        "https://open-test.gasfree.io/",
        "http://127.0.0.1:8080",
    ] {
        let binding = binding(&Network::Nile, base_url);
        assert_eq!(binding.provider.request_timeout_ms(), 15_000);
        assert_eq!(binding.status_poll_interval_ms, 3_000);
        assert_eq!(binding.service_provider, address(USERS[4].address));
        assert_eq!(binding.network, Network::Nile);
    }
    let request = provider_request(
        "https://open.gasfree.io",
        json!({ "request_timeout_ms": 2_500, "status_poll_interval_ms": 750 }),
    );
    let binding = GasFreeBinding::from_request(&Network::Mainnet, &request).expect("a valid provider object");
    assert_eq!(binding.provider.request_timeout_ms(), 2_500);
    assert_eq!(binding.status_poll_interval_ms, 750);
}

/// ch.63 T9, §63.4.1: the request base is the configured scheme and host followed by the network segment and the
/// `api/v1` prefix. The provider stub sees the path of the supported-tokens call.
#[test]
fn t9_the_request_base_is_the_host_then_the_network_segment() {
    for (network, path) in [
        (Network::Mainnet, "/tron/api/v1/config/token/all"),
        (Network::Nile, "/nile/api/v1/config/token/all"),
        (Network::Shasta, "/shasta/api/v1/config/token/all"),
    ] {
        let provider = StubNode::start(|_, _| Reply::ok(r#"{"code":200,"data":{"tokens":[]}}"#));
        // A trailing slash is the whole path of a host-only URL.
        let binding = binding(&network, &format!("{}/", provider.url()));
        let tokens = block_on(binding.provider.supported_tokens());
        assert!(matches!(&tokens, Ok(tokens) if tokens.is_empty()), "the stub's answer");
        let requests = provider.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].0, path);
    }
}

/// ch.63 T9, R13: a `base_url` that is more or less than a scheme and a host is refused, and the refusal repeats
/// nothing of it.
#[test]
fn t9_a_base_url_with_a_path_is_refused() {
    for base_url in [
        "https://open.gasfree.io/tron",
        "https://open.gasfree.io/tron/",
        "https://open.gasfree.io/tron/api/v1",
        "https://open.gasfree.io/?token=hunter2",
        "https://open.gasfree.io/#hunter2",
        "https://bob:hunter2@open.gasfree.io",
        "ftp://open.gasfree.io",
        "open.gasfree.io",
        "",
    ] {
        let error = GasFreeBinding::from_request(&Network::Mainnet, &provider_request(base_url, json!({})))
            .expect_err("more or less than a scheme and a host");
        assert!(matches!(error, GaslessConfigError::BaseUrl(_)), "the URL refusal");
        let text = format!("{error} {error:?}");
        assert!(
            !text.contains("hunter2") && !text.contains("gasfree.io"),
            "the URL is repeated"
        );
        assert_no_credential(&text);
    }
}

/// ch.63 T9, §63.3.1: `service_provider` must be a well-formed base58check TRON address.
#[test]
fn t9_a_malformed_service_provider_is_refused() {
    let user = address(USERS[4].address);
    for service_provider in [
        String::new(),
        "not-an-address".to_owned(),
        // One character short, and a last character that breaks the checksum.
        USERS[4].address[..33].to_owned(),
        format!("{}M", &USERS[4].address[..33]),
        // The hex forms are not the base58check form the request takes.
        user.to_hex(),
        format!("0x{}", hex::encode(user.to_evm_address())),
    ] {
        let request = provider_request(
            "https://open.gasfree.io",
            json!({ "service_provider": service_provider }),
        );
        let error = GasFreeBinding::from_request(&Network::Nile, &request).expect_err("a malformed address");
        assert_eq!(error, GaslessConfigError::ServiceProvider);
        assert_no_credential(&error.to_string());
    }
}

/// The provider object needs its four members, and a credential of another JSON type is refused without being
/// repeated.
#[test]
fn the_provider_object_needs_its_members_and_never_echoes_a_credential() {
    for member in ["base_url", "api_key", "api_secret", "service_provider"] {
        let mut object = provider_json("https://open.gasfree.io", json!({}));
        object.as_object_mut().expect("an object").remove(member);
        let error = serde_json::from_value::<TronGaslessProviderRequest>(object).expect_err("a missing member");
        assert!(error.to_string().contains(member), "the refusal names the member");
        assert_no_credential(&error.to_string());
    }
    for member in ["base_url", "api_key", "api_secret"] {
        for value in [json!(7_039_112_233u64), json!(["hunter2"]), json!({ "v": "hunter2" })] {
            let object = provider_json("https://open.gasfree.io", json!({ (member): value }));
            let error = serde_json::from_value::<TronGaslessProviderRequest>(object).expect_err("not a string");
            let text = error.to_string();
            assert!(
                !text.contains("7039112233") && !text.contains("hunter2"),
                "the value is repeated"
            );
            assert_no_credential(&text);
        }
    }
}

/// R9: a provider object or a token block of the wrong JSON type, and a member of the wrong type, fail decoding
/// with a text that repeats no part of the value. The marker stands for a misplaced credential.
#[test]
fn a_wrongly_typed_object_or_member_is_refused_without_being_repeated() {
    const MARKER: &str = "misplaced-marker-51c7";
    const MARKER_NUMBER: u64 = 7_039_112_233;
    let quiet = |text: String| {
        assert!(
            !text.contains(MARKER) && !text.contains("7039112233"),
            "the value is repeated"
        );
        assert_no_credential(&text);
    };

    for value in [
        json!(MARKER),
        json!([MARKER, API_SECRET]),
        json!(MARKER_NUMBER),
        json!(true),
    ] {
        let Err(error) = serde_json::from_value::<TronGaslessProviderRequest>(value.clone()) else {
            panic!("a provider object must be an object")
        };
        assert!(error.to_string().contains("must be an object"), "the fixed text");
        quiet(error.to_string());
        let Err(error) = serde_json::from_value::<TronTokenGaslessRequest>(value) else {
            panic!("a gasless block must be an object")
        };
        assert!(error.to_string().contains("must be an object"), "the fixed text");
        quiet(error.to_string());
    }
    for member in ["service_provider", "request_timeout_ms", "status_poll_interval_ms"] {
        for value in [json!([MARKER]), json!({ "v": MARKER }), json!(-1), json!(1.5)] {
            let object = provider_json("https://open.gasfree.io", json!({ (member): value }));
            let Err(error) = serde_json::from_value::<TronGaslessProviderRequest>(object) else {
                panic!("a wrongly typed member must be refused")
            };
            assert!(error.to_string().contains(member), "the refusal names the member");
            quiet(error.to_string());
        }
    }
    let object = provider_json("https://open.gasfree.io", json!({ "request_timeout_ms": MARKER }));
    let Err(error) = serde_json::from_value::<TronGaslessProviderRequest>(object) else {
        panic!("a wrongly typed member must be refused")
    };
    quiet(error.to_string());
    for block in [
        json!({ "enabled": MARKER }),
        json!({ "enabled": MARKER_NUMBER }),
        json!({ "enabled": true, "transfer_max_fee": MARKER }),
        json!({ "enabled": true, "transfer_max_fee": [MARKER] }),
        json!({ "enabled": true, "transfer_max_fee": { "v": MARKER_NUMBER } }),
    ] {
        let Err(error) = serde_json::from_value::<TronTokenGaslessRequest>(block) else {
            panic!("a wrongly typed member must be refused")
        };
        quiet(error.to_string());
    }
    // An explicit `null` is an absent optional member.
    let object = provider_json(
        "https://open.gasfree.io",
        json!({ "request_timeout_ms": null, "status_poll_interval_ms": null }),
    );
    assert!(serde_json::from_value::<TronGaslessProviderRequest>(object).is_ok());
    assert!(!gasless_request(json!({ "transfer_max_fee": null })).is_enabled());
}

// ---------------------------------------------------------------------------------------------------------------
// R9: the credentials
// ---------------------------------------------------------------------------------------------------------------

/// R9, ch.35 R35.1.17: no debug form shows a credential, and the URL is shown by its host.
#[test]
fn debug_shows_no_credential_and_the_host_only() {
    let request = provider_request("https://open.gasfree.io/key/hunter2?token=hunter2", json!({}));
    let rendered = format!("{request:?}");
    assert_no_credential(&rendered);
    assert!(!rendered.contains("hunter2"), "the URL beyond its host is rendered");
    assert!(rendered.contains("https://open.gasfree.io"), "the host is named");
    assert!(rendered.contains("<set>"), "a set secret is said to be set");

    let binding = binding(&Network::Nile, "https://open.gasfree.io");
    let rendered = format!("{binding:?}");
    assert_no_credential(&rendered);
    assert!(rendered.contains("https://open.gasfree.io"), "the host is named");
    assert_no_credential(&format!("{:?}", CoinGasFree::Platform(binding)));
}

/// R9: a platform coin with a provider binding shows no credential in its debug form.
#[test]
fn the_debug_form_of_a_coin_shows_no_credential() {
    let node = node(Controller::Agrees);
    let ctx = ctx();
    let coin = block_on(platform(
        &ctx,
        &node,
        Some(binding(&Network::Nile, "https://open.gasfree.io")),
    ));
    assert!(coin.gasfree_platform_binding().is_some());
    assert_no_credential(&format!("{:?}", &*coin));
}

// ---------------------------------------------------------------------------------------------------------------
// ch.63 T9: the token block
// ---------------------------------------------------------------------------------------------------------------

/// ch.63 T9, §63.3.2: `enabled` is `false` unless given, and the cap is converted to the token's smallest unit.
#[test]
fn t9_the_token_block_defaults_to_disabled_and_converts_the_cap() {
    assert!(!gasless_request(json!({})).is_enabled());
    assert!(!gasless_request(json!({ "transfer_max_fee": "1" })).is_enabled());
    assert!(!gasless_request(json!({ "enabled": false })).is_enabled());
    assert!(gasless_request(json!({ "enabled": true })).is_enabled());

    let units = |fee: Json, decimals: u8| {
        TokenGasless::from_request(
            &gasless_request(json!({ "enabled": true, "transfer_max_fee": fee })),
            decimals,
        )
        .map(|gasless| gasless.transfer_max_fee)
    };
    assert_eq!(units(json!("1.5"), 6), Ok(Some(U256::from(1_500_000u64))));
    assert_eq!(units(json!("0.000001"), 6), Ok(Some(U256::from(1u64))));
    assert_eq!(units(json!("1.500000000"), 6), Ok(Some(U256::from(1_500_000u64))));
    assert_eq!(units(json!("0"), 6), Ok(Some(U256::zero())));
    assert_eq!(units(json!(2), 6), Ok(Some(U256::from(2_000_000u64))));
    assert_eq!(units(json!(0.25), 6), Ok(Some(U256::from(250_000u64))));
    assert_eq!(units(json!("3"), 18), Ok(Some(U256::from(3u64) * U256::exp10(18))));
    assert_eq!(
        TokenGasless::from_request(&gasless_request(json!({ "enabled": true })), 6),
        Ok(TokenGasless { transfer_max_fee: None })
    );
}

/// ch.63 T9, §63.3.2: a negative cap, and a cap finer than the token's decimals, are refused.
#[test]
fn t9_a_negative_or_too_fine_cap_is_refused() {
    let refused = |fee: &str, decimals: u8| {
        let request = gasless_request(json!({ "enabled": true, "transfer_max_fee": fee }));
        TokenGasless::from_request(&request, decimals).expect_err("a refused cap")
    };
    assert_eq!(refused("-1", 6), GaslessConfigError::NegativeTransferMaxFee);
    assert_eq!(refused("-0.000001", 6), GaslessConfigError::NegativeTransferMaxFee);
    assert_eq!(refused("0.0000001", 6), GaslessConfigError::TransferMaxFeeTooFine {
        decimals: 6
    });
    assert_eq!(refused("1.2345678", 6), GaslessConfigError::TransferMaxFeeTooFine {
        decimals: 6
    });
    assert_eq!(refused("0.5", 0), GaslessConfigError::TransferMaxFeeTooFine {
        decimals: 0
    });
    // More than the token's amount type holds.
    assert_eq!(
        refused(&format!("1{}", "0".repeat(80)), 6),
        GaslessConfigError::TransferMaxFeeOutOfRange
    );

    // The checks that need no decimals: a provider on the platform, then the sign of the cap.
    let negative = gasless_request(json!({ "enabled": true, "transfer_max_fee": "-1" }));
    let plain = gasless_request(json!({ "enabled": true }));
    assert_eq!(
        TokenGasless::precheck(&plain, false),
        Err(GaslessConfigError::NoPlatformProvider)
    );
    assert_eq!(
        TokenGasless::precheck(&negative, false),
        Err(GaslessConfigError::NoPlatformProvider)
    );
    assert_eq!(
        TokenGasless::precheck(&negative, true),
        Err(GaslessConfigError::NegativeTransferMaxFee)
    );
    assert_eq!(TokenGasless::precheck(&plain, true), Ok(()));
}

/// ch.63 T9, R1: a token built on a platform keeps its enabled block in base units; `enabled: false` is an absent
/// block; an enabled block without a platform provider is refused before any node is asked.
#[test]
fn t9_a_token_keeps_its_enabled_block_and_needs_a_platform_provider() {
    let ctx = ctx();
    block_on(async {
        let node = node(Controller::Agrees);
        let with_provider = platform(&ctx, &node, Some(binding(&Network::Nile, "https://open.gasfree.io"))).await;

        let capped = gasless_request(json!({ "enabled": true, "transfer_max_fee": "1.5" }));
        let built = token(&with_provider, Some(&capped)).await;
        let Ok(built) = built else {
            panic!("the token must build")
        };
        assert!(matches!(
            &built.tron_gasfree,
            Some(CoinGasFree::Token(gasless)) if gasless.transfer_max_fee == Some(U256::from(1_500_000u64))
        ));
        // The binding stays on the platform coin: a token coin holds none.
        assert!(built.gasfree_platform_binding().is_none());

        let uncapped = gasless_request(json!({ "enabled": true }));
        let Ok(built) = token(&with_provider, Some(&uncapped)).await else {
            panic!("the token must build")
        };
        assert!(matches!(
            &built.tron_gasfree,
            Some(CoinGasFree::Token(gasless)) if gasless.transfer_max_fee.is_none()
        ));

        // `enabled: false` behaves as an absent block, whatever else it holds.
        for block in [
            json!({ "enabled": false }),
            json!({ "enabled": false, "transfer_max_fee": "-1" }),
            json!({ "transfer_max_fee": "0.0000001" }),
        ] {
            let Ok(built) = token(&with_provider, Some(&gasless_request(block))).await else {
                panic!("the token must build")
            };
            assert!(built.tron_gasfree.is_none(), "a disabled block is kept");
        }
        let Ok(built) = token(&with_provider, None).await else {
            panic!("the token must build")
        };
        assert!(built.tron_gasfree.is_none());

        // A cap the token cannot express is an invalid payload.
        for fee in ["-1", "0.0000001"] {
            let block = gasless_request(json!({ "enabled": true, "transfer_max_fee": fee }));
            let Err(error) = token(&with_provider, Some(&block)).await else {
                panic!("the cap must be refused")
            };
            assert!(
                matches!(error.get_inner(), Erc20BuildError::InvalidPayload(_)),
                "the cap refusal"
            );
        }
        assert!(node.requests().is_empty(), "a configured decimals value asks no node");

        // Without a provider on the platform an enabled block is refused, a disabled one is not.
        let bare_node = self::node(Controller::Agrees);
        let without_provider = platform(&ctx, &bare_node, None).await;
        let Err(error) = token(&without_provider, Some(&uncapped)).await else {
            panic!("an enabled block needs a provider")
        };
        assert!(
            matches!(error.get_inner(), Erc20BuildError::InvalidPayload(_)),
            "the provider refusal"
        );
        let Ok(built) = token(&without_provider, Some(&gasless_request(json!({ "enabled": false })))).await else {
            panic!("the token must build")
        };
        assert!(built.tron_gasfree.is_none());
        assert!(bare_node.requests().is_empty());
    });
}

/// The binding lives on the platform coin: the builder refuses one for a token built on its own.
#[test]
fn a_token_built_on_its_own_takes_no_provider_binding() {
    let node = node(Controller::Agrees);
    let ctx = ctx();
    let params = TronCoinParams {
        nodes: vec![TronNode::plain(node.url())],
        required_confirmations: 1,
        tx_history: false,
        gasfree: Some(binding(&Network::Nile, "https://open.gasfree.io")),
        swap_contracts: None,
        swap_evidence_node: None,
    };
    let built = block_on(tron_coin_from_params(
        &ctx,
        "USDT-TRC20",
        &crate::coin_conf(&ctx, "USDT-TRC20"),
        params,
        &priv_key(),
        CoinProtocol::TRC20 {
            platform: "TRX".to_owned(),
            contract_address: TRC20_CONTRACT.to_owned(),
        },
    ));
    let Err(error) = built else {
        panic!("a token built on its own takes no binding")
    };
    assert!(matches!(error.get_inner(), EthCoinBuildError::Internal(_)));
    assert_no_credential(&error.get_inner().to_string());
}

// ---------------------------------------------------------------------------------------------------------------
// R18, R29: the address a report shows
// ---------------------------------------------------------------------------------------------------------------

/// R29 in reports: a match shows the address, and a second report within 10 minutes makes no controller call.
#[test]
fn a_confirmed_address_is_shown_and_not_asked_again() {
    let node = node(Controller::Agrees);
    let (api, binding) = (api(&node), binding(&Network::Nile, "https://open.gasfree.io"));
    let user = address(USERS[0].address);
    let derived = gasfree_custody_address(&Network::Nile, &user);

    assert_eq!(block_on(binding.report_address(&api, &user)), Some(derived));
    assert_eq!(controller_reads(&node), 1);
    for _ in 0..3 {
        assert_eq!(block_on(binding.report_address(&api, &user)), Some(derived));
    }
    assert_eq!(controller_reads(&node), 1, "a confirmation is reused");

    // Another user of the same platform is asked for once.
    let other = address(USERS[1].address);
    assert_eq!(
        block_on(binding.report_address(&api, &other)),
        Some(gasfree_custody_address(&Network::Nile, &other))
    );
    assert_eq!(controller_reads(&node), 2);
}

/// R29 in reports: a controller that reports another address makes the report omit the field, with one warning
/// line per check; nothing is remembered.
#[test]
fn a_contradicted_address_is_omitted_with_one_warning() {
    let log_captured = install_log_capture();
    let node = node(Controller::Disagrees);
    let (api, binding) = (api(&node), binding(&Network::Nile, "https://open.gasfree.io"));
    let user = address(USERS[2].address);

    assert_eq!(block_on(binding.report_address(&api, &user)), None);
    assert_eq!(controller_reads(&node), 1);
    assert_eq!(block_on(binding.report_address(&api, &user)), None);
    assert_eq!(controller_reads(&node), 2, "a mismatch is not remembered");

    if log_captured {
        let expected = mismatch_warning(
            "nile",
            &user,
            &gasfree_custody_address(&Network::Nile, &user),
            &gasfree_custody_address(&Network::Mainnet, &user),
        );
        let lines: Vec<String> = captured_log_lines()
            .into_iter()
            .filter(|line| line.contains(&expected))
            .collect();
        // One line per refused check.
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|line| line.starts_with("WARN")));
    }
}

/// R29 in reports: a controller that cannot be read leaves the locally derived address in the report.
#[test]
fn an_unreadable_controller_leaves_the_derived_address() {
    let node = node(Controller::Down);
    let (api, binding) = (api(&node), binding(&Network::Nile, "https://open.gasfree.io"));
    let user = address(USERS[3].address);
    let derived = gasfree_custody_address(&Network::Nile, &user);

    assert_eq!(block_on(binding.report_address(&api, &user)), Some(derived));
    let reads = controller_reads(&node);
    assert!(reads >= 1, "the controller was asked");
    // A failed read confirms nothing: the next report asks again.
    assert_eq!(block_on(binding.report_address(&api, &user)), Some(derived));
    assert!(controller_reads(&node) > reads);
}

/// ch.63 T10, R18 at the coin: the platform coin and its token report the custody address of the enabled address,
/// equal to the local derivation; a platform without a provider, its token, and an EVM coin report none.
#[test]
fn t10_the_coin_reports_the_custody_address_only_with_a_provider() {
    block_on(async {
        // With a provider.
        let ctx = ctx();
        let node = node(Controller::Agrees);
        let trx = platform(&ctx, &node, Some(binding(&Network::Nile, "https://open.gasfree.io"))).await;
        let user = TronAddress::from_evm_address(trx.my_address);
        let custody = gasfree_custody_address(&Network::Nile, &user).to_base58();
        assert_eq!(trx.enabled_address_gasfree().await, Some(custody.clone()));
        assert_eq!(trx.gasfree_report_address(&trx.my_address).await, Some(custody.clone()));
        // Any address of the platform: the report derives for the address it is asked for.
        let other = address(USERS[0].address);
        assert_eq!(
            trx.gasfree_report_address(&other.to_evm_address()).await,
            Some(gasfree_custody_address(&Network::Nile, &other).to_base58())
        );

        // A token resolves the binding from its platform coin, whether its own block is enabled or not.
        let Ok(usdt) = token(&trx, None).await else {
            panic!("the token must build")
        };
        assert_eq!(usdt.enabled_address_gasfree().await, None, "the platform is not active");
        register(&ctx, &trx, "TRX").await;
        let reads = controller_reads(&node);
        assert_eq!(usdt.enabled_address_gasfree().await, Some(custody));
        assert_eq!(
            controller_reads(&node),
            reads,
            "the platform and its tokens share the checks"
        );

        // Without a provider.
        let ctx = self::ctx();
        let bare_node = self::node(Controller::Agrees);
        let trx = platform(&ctx, &bare_node, None).await;
        register(&ctx, &trx, "TRX").await;
        let Ok(usdt) = token(&trx, None).await else {
            panic!("the token must build")
        };
        assert_eq!(trx.enabled_address_gasfree().await, None);
        assert_eq!(usdt.enabled_address_gasfree().await, None);
        assert_eq!(controller_reads(&bare_node), 0, "no provider, no controller read");
    });

    // An EVM coin never reports one.
    let (_ctx, eth) = eth_coin_for_test(EthCoinType::Eth, vec!["http://127.0.0.1:1".to_owned()], None);
    assert_eq!(block_on(eth.enabled_address_gasfree()), None);
    let address = eth.my_address;
    assert_eq!(block_on(eth.gasfree_report_address(&address)), None);
}

/// ch.63 T10 for the report text: an entry without a custody address has no such member at all, not a `null`.
#[test]
fn an_absent_custody_address_is_not_in_the_json_text() {
    use crate::coin_balance::{coin_balance_map_for_ticker, HDAddressBalance};
    use crate::CoinBalance;
    use crypto::{Bip44Chain, DerivationPath, RpcDerivationPath};

    let entry = |gasfree_address: Option<String>| HDAddressBalance {
        address: USERS[0].address.to_owned(),
        gasfree_address,
        derivation_path: RpcDerivationPath(DerivationPath::from_str("m/44'/195'/0'/0/0").expect("a path")),
        chain: Bip44Chain::External,
        balance: coin_balance_map_for_ticker("TRX", CoinBalance::default()),
    };
    let text = serde_json::to_string(&entry(None)).expect("a serializable entry");
    assert!(
        !text.contains("gasfree_address") && !text.contains("null"),
        "the member is in the text"
    );
    let text = serde_json::to_string(&entry(Some(USERS[1].address.to_owned()))).expect("a serializable entry");
    assert!(
        text.contains(&format!(r#""gasfree_address":"{}""#, USERS[1].address)),
        "the member is not in the text"
    );
}

/// R9: nothing the GasFree settings and the report rule log carries a credential.
#[test]
fn no_log_line_carries_a_credential() {
    let log_captured = install_log_capture();
    let request = provider_request("https://open.gasfree.io", json!({}));
    common::log::info!("the provider object of a request: {:?}", request);
    let bad = provider_request("https://open.gasfree.io/tron", json!({}));
    if let Err(error) = GasFreeBinding::from_request(&Network::Nile, &bad) {
        common::log::warn!("a refused provider object: {}", error);
    }
    let binding = binding(&Network::Nile, "https://open.gasfree.io");
    common::log::info!("the binding of a platform: {:?}", binding);
    for controller in [Controller::Agrees, Controller::Disagrees, Controller::Down] {
        let node = node(controller);
        let user = address(USERS[4].address);
        let _ = block_on(binding.report_address(&api(&node), &user));
    }
    if log_captured {
        let lines = captured_log_lines();
        assert!(
            lines.iter().any(|line| line.contains("the binding of a platform")),
            "the capture works"
        );
        assert!(
            lines
                .iter()
                .all(|line| !line.contains(API_KEY) && !line.contains(API_SECRET)),
            "a log line carries a credential"
        );
    }
}
