//! Tests of TRON GasFree on the activation routes and in the reports (CRD ch.63 §63.3.1 -- §63.3.3, R1, R9, R13,
//! R18, R29, T9, T10).
//!
//! The activation routines and the HD methods run end to end against a loopback stub of a TRON node whose
//! controller contract answers `getGasFreeAddress`. TRON activation is on (`TRON_ACTIVATION_AVAILABLE`), so every
//! path these tests reach is reachable in a build.
//!
//! The `my_balance` placement of §63.3.3 is tested where its handler lives (`mm2_main`).
//!
//! No assertion message of this module carries a credential.

use super::*;
use coins::eth::tron::gasfree_custody_address;

const API_KEY: &str = "key-0f3a9c";
const API_SECRET: &str = "secret-7b1d4e";
const PROVIDER_URL: &str = "https://open.gasfree.io";
const GET_GASFREE_ADDRESS: &str = "getGasFreeAddress(address)";
const MEMBER: &str = "gasfree_address";

fn assert_no_credential(text: &str) {
    assert!(!text.contains(API_KEY), "the API key is rendered");
    assert!(!text.contains(API_SECRET), "the API secret is rendered");
}

/// A `tron_gasless_provider` object, with the `extra` members replacing or adding to the usual ones.
fn provider(extra: Json) -> Json {
    let mut object = json!({
        "base_url": PROVIDER_URL,
        "api_key": API_KEY,
        "api_secret": API_SECRET,
        "service_provider": USDT_CONTRACT,
    });
    for (key, value) in extra.as_object().expect("an object").iter() {
        object[key] = value.clone();
    }
    object
}

/// The members of a platform request that carries the usual provider object.
fn with_provider() -> Json { json!({ "tron_gasless_provider": provider(json!({})) }) }

/// `first` with the members of `second` added.
fn merged(mut first: Json, second: Json) -> Json {
    for (key, value) in second.as_object().expect("an object").iter() {
        first[key] = value.clone();
    }
    first
}

/// The custody address this build derives on Nile for the base58check address `user`.
fn custody_of(user: &str) -> String {
    let user = TronAddress::from_base58(user).expect("a valid address");
    gasfree_custody_address(&TronNetwork::Nile, &user).to_base58()
}

/// What the stub node's controller contract answers to `getGasFreeAddress`.
#[derive(Clone, Copy)]
enum Controller {
    /// The address this build derives for the asked user on Nile.
    Agrees,
    /// Another address than the derived one: the one the mainnet constants give.
    Disagrees,
    /// HTTP 500.
    Down,
}

/// The controller's answer to a `triggerconstantcontract` request, or `None` when the request is another read.
fn controller_answer(controller: Controller, body: &str) -> Option<(u16, String)> {
    let request: Json = serde_json::from_str(body).unwrap_or(Json::Null);
    if request["function_selector"] != GET_GASFREE_ADDRESS {
        return None;
    }
    // The parameter is one ABI word: 12 zero bytes and the 20-byte user.
    let payload = request["parameter"].as_str().unwrap_or_default().get(24..)?;
    let user = TronAddress::parse(&format!("41{payload}")).ok()?;
    let network = match controller {
        Controller::Agrees => TronNetwork::Nile,
        Controller::Disagrees => TronNetwork::Mainnet,
        Controller::Down => return Some((500, "{}".to_owned())),
    };
    let custody = gasfree_custody_address(&network, &user);
    let word = format!("{:0>64}", hex::encode(custody.to_evm_address()));
    Some((200, json!({ "constant_result": [word] }).to_string()))
}

/// The healthy node of the parent module with the controller contract of `controller`.
fn gasfree_node(controller: Controller) -> TronStub {
    TronStub::start(move |path, body| {
        if path == "/wallet/triggerconstantcontract" {
            if let Some(answer) = controller_answer(controller, body) {
                return answer;
            }
        }
        healthy(path, body)
    })
}

/// A node for the HD tests with the controller contract of `controller`: the `accounts` (base58check) exist on
/// chain with 2.5 TRX and every other account is the documented empty answer; no address holds the token.
fn gasfree_hd_node(controller: Controller, accounts: Vec<String>) -> TronStub {
    TronStub::start(move |path, body| {
        let request: Json = serde_json::from_str(body).unwrap_or(Json::Null);
        match path {
            "/wallet/getaccount" => {
                let address = request["address"].as_str().unwrap_or_default();
                if accounts.iter().any(|known| known == address) {
                    (200, json!({ "balance": 2_500_000, "create_time": 1 }).to_string())
                } else {
                    (200, "{}".to_owned())
                }
            },
            "/wallet/triggerconstantcontract" if request["function_selector"] == "balanceOf(address)" => {
                (200, json!({ "constant_result": [word(0)] }).to_string())
            },
            "/wallet/triggerconstantcontract" => {
                controller_answer(controller, body).unwrap_or_else(|| healthy(path, body))
            },
            _ => healthy(path, body),
        }
    })
}

/// The number of `getGasFreeAddress` reads the node received.
fn controller_reads(node: &TronStub) -> usize {
    node.log
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|(path, body)| path == "/wallet/triggerconstantcontract" && body.contains(GET_GASFREE_ADDRESS))
        .count()
}

/// `enable_erc20` for `ticker` with the `activation_params`.
fn enable_token_with(
    ctx: &MmArc,
    ticker: &str,
    activation_params: Json,
) -> Result<EvmTokenActivationResult, MmError<EnableTokenError>> {
    let request = serde_json::from_value(json!({ "ticker": ticker, "activation_params": activation_params }))
        .expect("a token request");
    block_on(enable_evm_token(ctx.clone(), request))
}

fn enabled_token(ctx: &MmArc, ticker: &str, activation_params: Json) -> Json {
    match enable_token_with(ctx, ticker, activation_params) {
        Ok(result) => serde_json::to_value(&result).expect("a serializable result"),
        Err(e) => panic!("enable_erc20 failed: {e}"),
    }
}

/// `task::enable_erc20` for `ticker` with the `activation_params`, run to its terminal status.
fn token_task(ctx: &MmArc, ticker: &str, activation_params: Json) -> Json {
    let init = serde_json::from_value(json!({ "ticker": ticker, "activation_params": activation_params }))
        .expect("a token task request");
    let response = block_on(init_token::<EthCoin>(ctx.clone(), init)).unwrap_or_else(|e| panic!("init failed: {e}"));
    poll_status(|| {
        let status = block_on(init_token_status::<EthCoin>(
            ctx.clone(),
            status_request(response.task_id),
        ))
        .unwrap_or_else(|e| panic!("status failed: {e}"));
        serde_json::to_value(&status).expect("a serializable status")
    })
}

/// The `erc20_tokens_requests` of a platform request: the one token `ticker` with the `gasless` block.
fn gasless_tokens(ticker: &str, gasless: Json) -> Json { json!([{ "ticker": ticker, "gasless": gasless }]) }

// ---------------------------------------------------------------------------------------------------------------
// ch.63 T9: the provider object on the platform routes
// ---------------------------------------------------------------------------------------------------------------

/// ch.63 T9, §63.3.1: a TRON platform takes the provider object on the one-shot and on the task route, and the
/// answer carries the custody address; no answer carries a credential.
#[test]
fn t9_a_tron_platform_takes_the_provider_object_on_both_routes() {
    let node = gasfree_node(Controller::Agrees);
    let ctx = tron_ctx();
    let result = activated(&ctx, request("TRX", std::slice::from_ref(&node.url), with_provider()));
    let address = registered(&ctx, "TRX").my_address().expect("an address");
    assert_eq!(result["eth_addresses_infos"][&address][MEMBER], custody_of(&address));
    assert_no_credential(&result.to_string());

    let ctx = tron_ctx();
    let status = run_task(&ctx, request("TRX", std::slice::from_ref(&node.url), with_provider()));
    assert_eq!(status["status"], "Ok");
    assert_eq!(
        status["details"]["eth_addresses_infos"][&address][MEMBER],
        custody_of(&address)
    );
    assert_no_credential(&status.to_string());
    assert!(is_registered(&ctx, "TRX"));
}

/// ch.63 T9, R13, §63.3.1: a `base_url` with a path and a `service_provider` that is not a TRON address are an
/// invalid payload on both routes, before any node is contacted; the refusal repeats no value of the object.
#[test]
fn t9_a_base_url_with_a_path_and_a_malformed_service_provider_are_invalid_payload() {
    let evm_form = format!(
        "0x{}",
        hex::encode(TronAddress::parse(USDT_CONTRACT).expect("valid").to_evm_address())
    );
    for bad in [
        json!({ "base_url": "https://open.gasfree.io/tron" }),
        json!({ "base_url": "https://open.gasfree.io/tron/api/v1/" }),
        json!({ "base_url": "https://open.gasfree.io/?token=hunter2" }),
        json!({ "base_url": "open.gasfree.io" }),
        json!({ "service_provider": "not-an-address" }),
        json!({ "service_provider": evm_form }),
        json!({ "service_provider": "" }),
    ] {
        let members = json!({ "tron_gasless_provider": provider(bad) });
        let node = untouched_stub();

        let ctx = tron_ctx();
        let wire = failure(&ctx, request("TRX", std::slice::from_ref(&node.url), members.clone()));
        assert_eq!(wire.0["error_type"], "InvalidPayload");
        assert_eq!(wire.1, StatusCode::BAD_REQUEST);
        let text = wire.0.to_string();
        assert_no_credential(&text);
        assert!(
            !text.contains("hunter2") && !text.contains("gasfree.io") && !text.contains("not-an-address"),
            "a value of the object is repeated"
        );
        assert!(!is_registered(&ctx, "TRX"));

        let ctx = tron_ctx();
        let status = run_task(&ctx, request("TRX", std::slice::from_ref(&node.url), members));
        assert_eq!(status["status"], "Error");
        assert_eq!(status["details"]["error_type"], "InvalidPayload");
        assert_no_credential(&status.to_string());
        assert!(!is_registered(&ctx, "TRX"));

        assert_eq!(node.request_count(), 0, "no node is contacted");
    }
}

/// ch.63 T9, §63.3.1, R1: the provider object on an EVM platform is an invalid payload, on both routes, before
/// any node is contacted.
#[test]
fn t9_an_evm_platform_refuses_the_provider_object() {
    let node = untouched_stub();
    let ctx = tron_ctx();
    let wire = failure(&ctx, request("ETH", std::slice::from_ref(&node.url), with_provider()));
    assert_eq!(wire.0["error_type"], "InvalidPayload");
    assert_eq!(wire.1, StatusCode::BAD_REQUEST);
    assert_no_credential(&wire.0.to_string());

    let status = run_task(&ctx, request("ETH", std::slice::from_ref(&node.url), with_provider()));
    assert_eq!(status["status"], "Error");
    assert_eq!(status["details"]["error_type"], "InvalidPayload");
    assert_no_credential(&status.to_string());

    assert_eq!(node.request_count(), 0, "no node is contacted");
    assert!(!is_registered(&ctx, "ETH"));
}

// ---------------------------------------------------------------------------------------------------------------
// ch.63 T9: the token block
// ---------------------------------------------------------------------------------------------------------------

/// ch.63 T9, §63.3.2, R1, in the platform request: an enabled token block needs a provider on the platform;
/// `enabled: false` behaves as an absent block; a negative cap and a cap finer than the token's decimals are
/// refused.
#[test]
fn t9_a_token_block_in_the_platform_request_needs_a_provider_and_a_usable_cap() {
    let node = gasfree_node(Controller::Agrees);
    let activate_with = |members: Json| {
        let ctx = tron_ctx();
        let outcome = activate(&ctx, request("TRX", std::slice::from_ref(&node.url), members));
        (ctx, outcome)
    };
    let refused = |members: Json| {
        let (ctx, outcome) = activate_with(members);
        let Err(error) = outcome else {
            panic!("the activation must be refused")
        };
        assert!(
            matches!(error.get_inner(), EnablePlatformCoinWithTokensError::InvalidPayload(_)),
            "an invalid payload"
        );
        assert_eq!(error.get_inner().status_code(), StatusCode::BAD_REQUEST);
        assert_no_credential(&error.get_inner().to_string());
        assert!(!is_registered(&ctx, "TRX") && !is_registered(&ctx, "USDT-TRC20"));
    };
    let accepted = |members: Json| {
        let (ctx, outcome) = activate_with(members);
        assert!(outcome.is_ok(), "the activation must pass");
        assert!(is_registered(&ctx, "TRX") && is_registered(&ctx, "USDT-TRC20"));
    };
    let tokens_with = |gasless: Json| json!({ "erc20_tokens_requests": gasless_tokens("USDT-TRC20", gasless) });

    // No provider on the platform.
    refused(tokens_with(json!({ "enabled": true })));
    refused(tokens_with(json!({ "enabled": true, "transfer_max_fee": "1" })));
    accepted(tokens_with(json!({ "enabled": false })));
    accepted(tokens_with(json!({ "enabled": false, "transfer_max_fee": "-1" })));
    accepted(tokens_with(json!({})));

    // A provider on the platform.
    accepted(merged(with_provider(), tokens_with(json!({ "enabled": true }))));
    accepted(merged(
        with_provider(),
        tokens_with(json!({ "enabled": true, "transfer_max_fee": "1.5" })),
    ));
    accepted(merged(
        with_provider(),
        tokens_with(json!({ "enabled": true, "transfer_max_fee": 0 })),
    ));
    refused(merged(
        with_provider(),
        tokens_with(json!({ "enabled": true, "transfer_max_fee": "-0.5" })),
    ));
    // The token has 6 decimals.
    refused(merged(
        with_provider(),
        tokens_with(json!({ "enabled": true, "transfer_max_fee": "0.0000001" })),
    ));

    // The task route reports the same refusal and the same success.
    let ctx = tron_ctx();
    let status = run_task(
        &ctx,
        request(
            "TRX",
            std::slice::from_ref(&node.url),
            tokens_with(json!({ "enabled": true })),
        ),
    );
    assert_eq!(status["status"], "Error");
    assert_eq!(status["details"]["error_type"], "InvalidPayload");
    let ctx = tron_ctx();
    let status = run_task(
        &ctx,
        request(
            "TRX",
            std::slice::from_ref(&node.url),
            merged(with_provider(), tokens_with(json!({ "enabled": true }))),
        ),
    );
    assert_eq!(status["status"], "Ok");
}

/// ch.63 T9, §63.3.2, R1, on the token routes: `enable_erc20` answers `InvalidPayload` (HTTP 400) and
/// `task::enable_erc20` its invalid-payload class token `Transport` (CRD ch.35 R35.3A.6) for an enabled block on a
/// platform without a provider and for a cap the token cannot express.
#[test]
fn t9_a_token_block_on_the_token_routes_needs_a_provider_and_a_usable_cap() {
    let node = gasfree_node(Controller::Agrees);

    // A platform without a provider.
    let ctx = tron_ctx();
    activated(&ctx, request("TRX", std::slice::from_ref(&node.url), json!({})));
    let Err(error) = enable_token_with(&ctx, "USDT-TRC20", json!({ "gasless": { "enabled": true } })) else {
        panic!("an enabled block needs a provider")
    };
    assert!(matches!(error.get_inner(), EnableTokenError::InvalidPayload(_)));
    assert_eq!(error.get_inner().status_code(), StatusCode::BAD_REQUEST);
    assert!(!is_registered(&ctx, "USDT-TRC20"));
    let status = token_task(&ctx, "USDT-TRC20", json!({ "gasless": { "enabled": true } }));
    assert_eq!(status["status"], "Error");
    assert_eq!(status["details"]["error_type"], "Transport");
    assert!(!is_registered(&ctx, "USDT-TRC20"));
    // `enabled: false` behaves as an absent block.
    enabled_token(
        &ctx,
        "USDT-TRC20",
        json!({ "gasless": { "enabled": false, "transfer_max_fee": "-1" } }),
    );
    assert!(is_registered(&ctx, "USDT-TRC20"));
    let status = token_task(&ctx, "NODEC-TRC20", json!({ "gasless": {} }));
    assert_eq!(status["status"], "Ok");

    // A platform with a provider.
    let ctx = tron_ctx();
    activated(&ctx, request("TRX", std::slice::from_ref(&node.url), with_provider()));
    for fee in ["-1", "0.0000001"] {
        let block = json!({ "gasless": { "enabled": true, "transfer_max_fee": fee } });
        let Err(error) = enable_token_with(&ctx, "USDT-TRC20", block.clone()) else {
            panic!("the cap must be refused")
        };
        assert!(matches!(error.get_inner(), EnableTokenError::InvalidPayload(_)));
        let status = token_task(&ctx, "USDT-TRC20", block);
        assert_eq!(status["status"], "Error");
        assert_eq!(status["details"]["error_type"], "Transport");
        assert!(!is_registered(&ctx, "USDT-TRC20"));
    }
    enabled_token(
        &ctx,
        "USDT-TRC20",
        json!({ "gasless": { "enabled": true, "transfer_max_fee": "1.5" } }),
    );
    assert!(is_registered(&ctx, "USDT-TRC20"));
    let status = token_task(&ctx, "NODEC-TRC20", json!({ "gasless": { "enabled": true } }));
    assert_eq!(status["status"], "Ok");
}

// ---------------------------------------------------------------------------------------------------------------
// ch.63 T10, R18: the placements
// ---------------------------------------------------------------------------------------------------------------

/// ch.63 T10, single key: the platform's and the tokens' block of the platform-with-tokens result (also without
/// balances), the one-shot token result and the task-based token result carry `gasfree_address`, equal to the
/// local derivation for the address.
#[test]
fn t10_the_single_address_results_carry_the_custody_address() {
    let node = gasfree_node(Controller::Agrees);
    let ctx = tron_ctx();
    let result = activated(
        &ctx,
        request(
            "TRX",
            std::slice::from_ref(&node.url),
            merged(
                with_provider(),
                json!({ "erc20_tokens_requests": tokens(&["USDT-TRC20"]) }),
            ),
        ),
    );
    let address = registered(&ctx, "TRX").my_address().expect("an address");
    let custody = custody_of(&address);
    assert_ne!(custody, address);
    assert_eq!(result["eth_addresses_infos"][&address][MEMBER], custody.as_str());
    assert_eq!(result["erc20_addresses_infos"][&address][MEMBER], custody.as_str());
    // The entries keep what they had.
    assert_eq!(
        result["eth_addresses_infos"][&address]["balances"],
        json!({ "spendable": "2.5", "unspendable": "0" })
    );

    // The one-shot token result: the entry of the address.
    let token = enabled_token(&ctx, "NODEC-TRC20", json!({}));
    assert_eq!(
        token["balances"][&address],
        json!({ "spendable": "1.234567", "unspendable": "0", (MEMBER): custody.as_str() })
    );

    // The task-based token result: beside `address`.
    let ctx_task = tron_ctx();
    activated(
        &ctx_task,
        request("TRX", std::slice::from_ref(&node.url), with_provider()),
    );
    let status = token_task(&ctx_task, "USDT-TRC20", json!({}));
    assert_eq!(status["status"], "Ok");
    let wallet = &status["details"]["wallet_balance"];
    assert_eq!(wallet["wallet_type"], "Iguana");
    assert_eq!(wallet["address"], address.as_str());
    assert_eq!(wallet[MEMBER], custody.as_str());

    // Without the balances the blocks still describe the address.
    let ctx_bare = tron_ctx();
    let result = activated(
        &ctx_bare,
        request(
            "TRX",
            std::slice::from_ref(&node.url),
            merged(with_provider(), json!({ "get_balances": false })),
        ),
    );
    assert_eq!(result["eth_addresses_infos"][&address][MEMBER], custody.as_str());
    assert_eq!(result["erc20_addresses_infos"][&address][MEMBER], custody.as_str());
}

/// ch.63 T10, global HD: every address entry of the platform-with-tokens result, of `get_new_address`,
/// `account_balance`, `task::account_balance`, `task::scan_for_new_addresses` and of the one-shot and task-based
/// token results carries `gasfree_address`, equal to the local derivation for that address, for the TRX ticker and
/// for a TRC20 ticker.
#[test]
fn t10_the_hd_reports_carry_the_custody_address_of_each_address() {
    use coins::hd_wallet::get_new_address;
    use coins::rpc_command::account_balance::account_balance;

    let ctx = tron_hd_ctx();
    let address = |id: u32| independent_address(&ctx, &format!("m/44'/195'/0'/0/{id}"));
    // The accounts at 0 and 2 exist: a scan reports 1 and 2.
    let node = gasfree_hd_node(Controller::Agrees, vec![address(0), address(2)]);
    let assert_entries = |entries: &Json, expected: usize, what: &str| {
        let entries = entries.as_array().expect("address entries");
        assert_eq!(entries.len(), expected, "{what}");
        for entry in entries {
            let address = entry["address"].as_str().expect("an address");
            assert_eq!(entry[MEMBER], custody_of(address).as_str(), "{what}: {address}");
        }
    };

    // The platform-with-tokens result.
    let result = activated(
        &ctx,
        hd_request(
            &node,
            merged(
                with_provider(),
                json!({ "erc20_tokens_requests": tokens(&["USDT-TRC20"]) }),
            ),
        ),
    );
    assert_eq!(result["wallet_balance"]["wallet_type"], "HD");
    let account = &result["wallet_balance"]["accounts"][0];
    assert_eq!(account["addresses"][0]["address"], address(0).as_str());
    assert_entries(&account["addresses"], 1, "the activation result");
    assert_no_credential(&result.to_string());

    // `task::scan_for_new_addresses`, for the TRX ticker.
    let status = scan_task(&ctx, "TRX", 2);
    assert_eq!(status["status"], "Ok");
    assert_entries(&status["details"]["new_addresses"], 2, "the scan");

    // `get_new_address`, for both tickers.
    for (ticker, id) in [("TRX", 3), ("USDT-TRC20", 4)] {
        let request = serde_json::from_value(json!({ "coin": ticker, "account_id": 0, "chain": "External" }))
            .expect("a get_new_address request");
        let response = block_on(get_new_address(ctx.clone(), request))
            .unwrap_or_else(|e| panic!("get_new_address failed: {}", e.get_inner()));
        let new_address = serde_json::to_value(&response).expect("a serializable answer")["new_address"].clone();
        assert_eq!(new_address["address"], address(id).as_str());
        assert_eq!(new_address[MEMBER], custody_of(&address(id)).as_str(), "{ticker}");
    }

    // `account_balance` and `task::account_balance`, for both tickers.
    for ticker in ["TRX", "USDT-TRC20"] {
        let request = serde_json::from_value(json!({ "coin": ticker, "account_index": 0, "chain": "External" }))
            .expect("an account_balance request");
        let response = block_on(account_balance(ctx.clone(), request))
            .unwrap_or_else(|e| panic!("account_balance failed: {}", e.get_inner()));
        let response = serde_json::to_value(&response).expect("a serializable answer");
        assert_entries(&response["addresses"], 5, ticker);

        let params: InitAccountBalanceParams = serde_json::from_value(json!({ "account_index": 0 })).expect("params");
        let result = block_on(registered(&ctx, ticker).init_account_balance_rpc(params))
            .unwrap_or_else(|e| panic!("init_account_balance failed: {}", e.get_inner()));
        let result = serde_json::to_value(&result).expect("a serializable answer");
        assert_entries(&result["addresses"], 5, ticker);
    }

    // The one-shot token result on an HD platform: one entry per known address.
    let token = enabled_token(&ctx, "NODEC-TRC20", json!({}));
    let balances = token["balances"].as_object().expect("a balances map");
    assert_eq!(balances.len(), 5);
    for (address, entry) in balances {
        assert_eq!(entry[MEMBER], custody_of(address).as_str(), "{address}");
        assert!(entry["spendable"].is_string());
    }

    // The task-based token result on an HD platform.
    let ctx_task = tron_hd_ctx();
    activated(&ctx_task, hd_request(&node, with_provider()));
    let status = token_task(&ctx_task, "USDT-TRC20", json!({}));
    assert_eq!(status["status"], "Ok");
    let wallet = &status["details"]["wallet_balance"];
    assert_eq!(wallet["wallet_type"], "HD");
    assert_entries(&wallet["accounts"][0]["addresses"], 1, "the token task");
}

/// ch.63 T10, R18: without a provider on the platform no answer carries `gasfree_address`: the member is absent
/// from the JSON text, not `null`. The controller is never read.
#[test]
fn t10_without_a_provider_no_answer_has_the_member() {
    use coins::hd_wallet::get_new_address;
    use coins::rpc_command::account_balance::account_balance;

    let absent = |answer: &Json, what: &str| {
        assert!(!answer.to_string().contains(MEMBER), "{what}");
    };

    // Single key.
    let node = gasfree_node(Controller::Agrees);
    let ctx = tron_ctx();
    let result = activated(
        &ctx,
        request(
            "TRX",
            std::slice::from_ref(&node.url),
            json!({ "erc20_tokens_requests": tokens(&["USDT-TRC20"]) }),
        ),
    );
    absent(&result, "the single-key activation");
    absent(&enabled_token(&ctx, "NODEC-TRC20", json!({})), "enable_erc20");
    let ctx_task = tron_ctx();
    activated(&ctx_task, request("TRX", std::slice::from_ref(&node.url), json!({})));
    let status = token_task(&ctx_task, "USDT-TRC20", json!({}));
    assert_eq!(status["status"], "Ok");
    absent(&status, "task::enable_erc20");
    assert_eq!(controller_reads(&node), 0);

    // Global HD.
    let ctx = tron_hd_ctx();
    let address = |id: u32| independent_address(&ctx, &format!("m/44'/195'/0'/0/{id}"));
    let node = gasfree_hd_node(Controller::Agrees, vec![address(0), address(2)]);
    let result = activated(
        &ctx,
        hd_request(&node, json!({ "erc20_tokens_requests": tokens(&["USDT-TRC20"]) })),
    );
    absent(&result, "the HD activation");
    let status = scan_task(&ctx, "TRX", 2);
    assert_eq!(status["status"], "Ok");
    assert_eq!(
        status["details"]["new_addresses"].as_array().map(|found| found.len()),
        Some(2)
    );
    absent(&status, "the scan");
    for ticker in ["TRX", "USDT-TRC20"] {
        let request = serde_json::from_value(json!({ "coin": ticker, "account_id": 0, "chain": "External" }))
            .expect("a get_new_address request");
        let response = block_on(get_new_address(ctx.clone(), request))
            .unwrap_or_else(|e| panic!("get_new_address failed: {}", e.get_inner()));
        absent(
            &serde_json::to_value(&response).expect("a serializable answer"),
            "get_new_address",
        );

        let request = serde_json::from_value(json!({ "coin": ticker, "account_index": 0, "chain": "External" }))
            .expect("an account_balance request");
        let response = block_on(account_balance(ctx.clone(), request))
            .unwrap_or_else(|e| panic!("account_balance failed: {}", e.get_inner()));
        absent(
            &serde_json::to_value(&response).expect("a serializable answer"),
            "account_balance",
        );

        let params: InitAccountBalanceParams = serde_json::from_value(json!({ "account_index": 0 })).expect("params");
        let result = block_on(registered(&ctx, ticker).init_account_balance_rpc(params))
            .unwrap_or_else(|e| panic!("init_account_balance failed: {}", e.get_inner()));
        absent(
            &serde_json::to_value(&result).expect("a serializable answer"),
            "task::account_balance",
        );
    }
    absent(&enabled_token(&ctx, "NODEC-TRC20", json!({})), "enable_erc20 under HD");
    assert_eq!(controller_reads(&node), 0);
}

// ---------------------------------------------------------------------------------------------------------------
// R29 in the reports
// ---------------------------------------------------------------------------------------------------------------

/// R29 in the reports: the controller is read once per address, and a second report within 10 minutes reads
/// nothing; the platform and its tokens share the confirmation.
#[test]
fn a_confirmed_custody_address_is_not_asked_for_again() {
    let node = gasfree_node(Controller::Agrees);
    let ctx = tron_ctx();
    let result = activated(&ctx, request("TRX", std::slice::from_ref(&node.url), with_provider()));
    let address = registered(&ctx, "TRX").my_address().expect("an address");
    assert_eq!(result["eth_addresses_infos"][&address][MEMBER], custody_of(&address));
    assert_eq!(controller_reads(&node), 1);

    let token = enabled_token(&ctx, "USDT-TRC20", json!({}));
    assert_eq!(token["balances"][&address][MEMBER], custody_of(&address));
    let status = token_task(&ctx, "NODEC-TRC20", json!({}));
    assert_eq!(status["details"]["wallet_balance"][MEMBER], custody_of(&address));
    assert_eq!(controller_reads(&node), 1, "a confirmation is reused");
}

/// R29 in the reports: a controller that reports another address makes every answer omit the member; the
/// activation itself passes.
#[test]
fn a_contradicted_custody_address_is_omitted_from_the_answers() {
    let node = gasfree_node(Controller::Disagrees);
    let ctx = tron_ctx();
    let result = activated(
        &ctx,
        request(
            "TRX",
            std::slice::from_ref(&node.url),
            merged(
                with_provider(),
                json!({ "erc20_tokens_requests": tokens(&["USDT-TRC20"]) }),
            ),
        ),
    );
    assert!(!result.to_string().contains(MEMBER), "the member is omitted");
    let address = registered(&ctx, "TRX").my_address().expect("an address");
    assert_eq!(
        result["eth_addresses_infos"][&address]["balances"],
        json!({ "spendable": "2.5", "unspendable": "0" })
    );
    assert!(controller_reads(&node) >= 1);

    let token = enabled_token(&ctx, "NODEC-TRC20", json!({}));
    assert_eq!(
        token["balances"][&address],
        json!({ "spendable": "1.234567", "unspendable": "0" })
    );

    // Under HD every entry omits it.
    let ctx = tron_hd_ctx();
    let hd_address = independent_address(&ctx, "m/44'/195'/0'/0/0");
    let node = gasfree_hd_node(Controller::Disagrees, vec![hd_address]);
    let result = activated(&ctx, hd_request(&node, with_provider()));
    assert!(!result.to_string().contains(MEMBER), "the member is omitted");
    assert!(controller_reads(&node) >= 1);
}

/// R29 in the reports: a controller that cannot be read leaves the locally derived address in the answers.
#[test]
fn an_unreadable_controller_leaves_the_derived_custody_address() {
    let node = gasfree_node(Controller::Down);
    let ctx = tron_ctx();
    let result = activated(&ctx, request("TRX", std::slice::from_ref(&node.url), with_provider()));
    let address = registered(&ctx, "TRX").my_address().expect("an address");
    assert_eq!(result["eth_addresses_infos"][&address][MEMBER], custody_of(&address));
    assert_eq!(result["erc20_addresses_infos"][&address][MEMBER], custody_of(&address));
    let reads = controller_reads(&node);
    assert!(reads >= 1);
    // Nothing was confirmed: the next report asks again.
    let token = enabled_token(&ctx, "USDT-TRC20", json!({}));
    assert_eq!(token["balances"][&address][MEMBER], custody_of(&address));
    assert!(controller_reads(&node) > reads);
}

// ---------------------------------------------------------------------------------------------------------------
// R9: the credentials
// ---------------------------------------------------------------------------------------------------------------

/// R9, ch.35 R35.1.17: the debug form of an activation request shows neither credential, and shows the provider
/// URL by its host.
#[test]
fn the_debug_form_of_a_request_shows_no_credential() {
    let members = json!({
        "tron_gasless_provider": provider(json!({ "base_url": "https://open.gasfree.io/key/hunter2" })),
        "erc20_tokens_requests": gasless_tokens("USDT-TRC20", json!({ "enabled": true, "transfer_max_fee": "1.5" })),
    });
    let req = request("TRX", &["http://127.0.0.1:1".to_owned()], members);
    let rendered = format!("{req:?}");
    assert_no_credential(&rendered);
    assert!(!rendered.contains("hunter2"), "the URL beyond its host is rendered");
    assert!(rendered.contains(PROVIDER_URL), "the host is named");
    assert!(rendered.contains("<set>"), "a set secret is said to be set");
}

/// R9: a credential of the wrong JSON type fails the decoding of the request without being repeated.
#[test]
fn an_undecodable_provider_object_repeats_no_credential() {
    for member in ["api_key", "api_secret", "base_url"] {
        let body = request_json(
            "TRX",
            &["http://127.0.0.1:1".to_owned()],
            json!({ "tron_gasless_provider": provider(json!({ (member): 7_039_112_233u64 })) }),
        );
        let Err(error) = serde_json::from_value::<Req>(body) else {
            panic!("the request must not decode")
        };
        let text = error.to_string();
        assert!(!text.contains("7039112233"), "the value is repeated");
        assert_no_credential(&text);
    }
    // A missing credential is named by its member, and the other one is not repeated.
    let mut object = provider(json!({}));
    object.as_object_mut().expect("an object").remove("api_secret");
    let body = request_json(
        "TRX",
        &["http://127.0.0.1:1".to_owned()],
        json!({ "tron_gasless_provider": object }),
    );
    let Err(error) = serde_json::from_value::<Req>(body) else {
        panic!("the request must not decode")
    };
    assert_no_credential(&error.to_string());
}

/// R9: a `tron_gasless_provider` or a `gasless` member that is not an object fails the decoding of the request, on
/// the platform routes (one request type serves the one-shot and the task form) and on both token routes, with a
/// fixed text that repeats no part of the value. The marker stands for a misplaced credential.
#[test]
fn a_gasfree_member_of_the_wrong_json_type_repeats_nothing_of_its_value() {
    const MARKER: &str = "misplaced-marker-51c7";
    let node_urls = ["http://127.0.0.1:1".to_owned()];
    let quiet = |text: String, fixed: &str| {
        assert!(text.contains(fixed), "the fixed text");
        assert!(
            !text.contains(MARKER) && !text.contains("7039112233"),
            "the value is repeated"
        );
        assert_no_credential(&text);
    };
    for value in [
        json!(MARKER),
        json!([MARKER, API_KEY, API_SECRET]),
        json!(7_039_112_233u64),
    ] {
        // The platform request: the provider object, and the block of a token entry.
        let body = request_json("TRX", &node_urls, json!({ "tron_gasless_provider": value }));
        let Err(error) = serde_json::from_value::<Req>(body) else {
            panic!("the request must not decode")
        };
        quiet(error.to_string(), "tron_gasless_provider must be an object");
        let body = request_json(
            "TRX",
            &node_urls,
            json!({ "erc20_tokens_requests": gasless_tokens("USDT-TRC20", value.clone()) }),
        );
        let Err(error) = serde_json::from_value::<Req>(body) else {
            panic!("the request must not decode")
        };
        quiet(error.to_string(), "gasless must be an object");

        // `enable_erc20` and `task::enable_erc20::init`.
        let body = json!({ "ticker": "USDT-TRC20", "activation_params": { "gasless": value } });
        let Err(error) = serde_json::from_value::<EnableEvmTokenRequest>(body.clone()) else {
            panic!("the request must not decode")
        };
        quiet(error.to_string(), "gasless must be an object");
        let Err(error) = serde_json::from_value::<
            crate::init_token::InitTokenReq<crate::erc20_token_activation::EvmTokenActivationParams>,
        >(body) else {
            panic!("the request must not decode")
        };
        quiet(error.to_string(), "gasless must be an object");
    }
}
