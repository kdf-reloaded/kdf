//! Checks of the GasFree constants against the live controller contracts (CRD ch.63 T1, T2), through public TRON
//! nodes. They are read-only and need no key. They run only with the `live-network-tests` feature
//! (docs/UNIT_TESTS.md, "Tests that need public servers").
//!
//! One more test, [`a_nile_transfer_is_submitted_and_followed_to_its_settlement`], moves a token on the Nile test
//! network through a GasFree provider (CRD ch.63 T14). It needs provider credentials and a funded key, reads them
//! from the environment (docs/TEST_ENV_VARS.md) and is always ignored: it runs only when it is asked for by name
//! with `--ignored`. No value it reads is printed or put into an assertion text.

use super::super::api::TronApiClient;
use super::super::nodes::TronNode;
use super::super::Network;
use super::super::TronAddress;
use super::address_word;
use super::authorization::{sign_permit, typed_data_hash, PermitTransfer, PERMIT_VERSION};
use super::binding::{gasfree_custody_address, GasFreeBinding, TronGaslessProviderRequest};
use super::custody::{check_against_controller, read_controller_word, ControllerChecks};
use super::test_vectors::{address, hex32, CUSTODY_VECTORS, DOMAIN_VECTORS, MESSAGES, MESSAGE_MAX_FEE,
                          MESSAGE_SERVICE_PROVIDER, USERS};
use super::withdraw::{submit, track, SETTLEMENT_GRACE_MS};
use common::{block_on, now_ms};
use ethereum_types::U256;
use mm2_eth::keys::{KeyPair, Secret};
use std::fmt::Display;
use std::time::Duration;

/// The read-only controller function that returns the full typed-data hash of a `PermitTransfer`; its arguments
/// are the nine members in their order.
const GET_MESSAGE_HASH: &str =
    "getMessageHashForTransfer(address,address,address,address,uint256,uint256,uint256,uint256,uint256)";

/// How often a rate-limited read is tried.
const ATTEMPTS: u32 = 6;

fn public_node(network: &Network) -> TronApiClient {
    let url = match network {
        Network::Mainnet => "https://api.trongrid.io",
        Network::Nile => "https://nile.trongrid.io",
        Network::Shasta => "https://api.shasta.trongrid.io",
    };
    TronApiClient::new("TRX", vec![TronNode::plain(url.to_owned())])
}

/// Runs `read` until it answers. A public node that answers HTTP 429 is asked again after a growing pause; when
/// it still limits the rate after [`ATTEMPTS`] tries the test fails as undecided, to be run again later. Any other
/// error fails the test too.
fn read_with_back_off<T, E: Display>(what: &str, read: impl Fn() -> Result<T, E>) -> T {
    let mut pause = Duration::from_secs(2);
    for attempt in 1..=ATTEMPTS {
        match read() {
            Ok(answer) => return answer,
            Err(error) if error.to_string().contains("status 429") => {
                if attempt < ATTEMPTS {
                    std::thread::sleep(pause);
                    pause *= 2;
                }
            },
            Err(error) => panic!("{what}: {error}"),
        }
    }
    panic!("{what}: undecided, rerun later: the public node still limits the request rate after {ATTEMPTS} tries");
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public TRON nodes")]
fn the_live_controllers_derive_the_same_custody_address() {
    let user = address(USERS[0].address);
    for (network, vectors) in CUSTODY_VECTORS {
        let api = public_node(&network);
        let what = format!("getGasFreeAddress on {network:?}");
        // A fresh set of checks, so that the controller is really asked.
        let confirmed = read_with_back_off(&what, || {
            block_on(check_against_controller(
                &api,
                &ControllerChecks::new(),
                &network,
                &user,
            ))
        });
        assert_eq!(confirmed.to_base58(), vectors[0].custody);
    }
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public TRON nodes")]
fn the_live_controllers_compute_the_same_message_hash() {
    let message = &MESSAGES[0];
    let permit = PermitTransfer {
        token: address(message.token),
        service_provider: address(MESSAGE_SERVICE_PROVIDER),
        user: address(message.user),
        receiver: address(message.receiver),
        value: U256::from(message.value),
        max_fee: U256::from(MESSAGE_MAX_FEE),
        deadline: message.deadline,
        nonce: message.nonce,
    };
    let mut parameter = Vec::with_capacity(9 * 32);
    for member in [&permit.token, &permit.service_provider, &permit.user, &permit.receiver] {
        parameter.extend_from_slice(&address_word(member));
    }
    for number in [
        message.value,
        MESSAGE_MAX_FEE,
        permit.deadline,
        PERMIT_VERSION,
        permit.nonce,
    ] {
        parameter.extend_from_slice(&[0u8; 24]);
        parameter.extend_from_slice(&number.to_be_bytes());
    }
    let parameter = hex::encode(parameter);

    for (network, _, pinned) in DOMAIN_VECTORS {
        let local = typed_data_hash(&network, &permit).expect("a typed-data hash");
        assert_eq!(local, hex32(pinned));
        let api = public_node(&network);
        let what = format!("getMessageHashForTransfer on {network:?}");
        let live = read_with_back_off(&what, || {
            block_on(read_controller_word(&api, &network, GET_MESSAGE_HASH, &parameter))
        });
        assert_eq!(live, local);
    }
}

/// The value of the environment variable `name`. The panic names the variable and never a value.
fn live_setting(name: &str) -> String {
    match std::env::var(name) {
        Ok(value) if !value.is_empty() => value,
        _ => panic!("the environment variable {name} is not set"),
    }
}

/// CRD ch.63 T14: one transfer on Nile, signed, submitted to the provider and followed to `SUCCEED`.
///
/// Reads `GASFREE_LIVE_BASE_URL`, `GASFREE_LIVE_API_KEY`, `GASFREE_LIVE_API_SECRET`,
/// `GASFREE_LIVE_SERVICE_PROVIDER`, `GASFREE_LIVE_PRIVATE_KEY` (64 hex digits), `GASFREE_LIVE_TOKEN` (the TRC20
/// contract), `GASFREE_LIVE_RECEIVER` and `GASFREE_LIVE_AMOUNT_UNITS` (the amount in the token's smallest unit). The
/// custody address of the key must hold the amount and the provider's fee.
#[test]
#[ignore = "moves a token on Nile; needs GasFree provider credentials and a funded key in the environment"]
fn a_nile_transfer_is_submitted_and_followed_to_its_settlement() {
    let network = Network::Nile;
    let object = serde_json::json!({
        "base_url": live_setting("GASFREE_LIVE_BASE_URL"),
        "api_key": live_setting("GASFREE_LIVE_API_KEY"),
        "api_secret": live_setting("GASFREE_LIVE_API_SECRET"),
        "service_provider": live_setting("GASFREE_LIVE_SERVICE_PROVIDER"),
    });
    let Ok(request) = serde_json::from_value::<TronGaslessProviderRequest>(object) else {
        panic!("the provider settings are not a provider object")
    };
    let Ok(binding) = GasFreeBinding::from_request(&network, &request) else {
        panic!("the provider settings are refused")
    };
    let key = hex::decode(live_setting("GASFREE_LIVE_PRIVATE_KEY")).unwrap_or_default();
    let (Ok(secret), Ok(key_pair)) = (Secret::from_slice(&key), KeyPair::from_secret_slice(&key)) else {
        panic!("GASFREE_LIVE_PRIVATE_KEY is not a private key")
    };
    let user = TronAddress::from_evm_address(key_pair.address());
    let (Ok(token), Ok(receiver), Ok(value)) = (
        TronAddress::from_base58(&live_setting("GASFREE_LIVE_TOKEN")),
        TronAddress::from_base58(&live_setting("GASFREE_LIVE_RECEIVER")),
        U256::from_dec_str(&live_setting("GASFREE_LIVE_AMOUNT_UNITS")),
    ) else {
        panic!("the token, the receiver or the amount is not usable")
    };

    // The provider's record of the account: its custody address is the derived one, and it quotes the fee.
    let Ok(account) = block_on(binding.provider().account_info(&user)) else {
        panic!("the provider does not answer the account call")
    };
    assert!(
        account.gas_free_address == gasfree_custody_address(&network, &user),
        "the provider reports another custody address than the derived one"
    );
    assert!(account.allow_submit, "an earlier transfer of the account is unsettled");
    let Some(asset) = account.assets.iter().find(|asset| asset.token_address == token) else {
        panic!("the provider does not list the token for the account")
    };
    let activation_fee = if account.active {
        U256::zero()
    } else {
        asset.activate_fee
    };

    let now_s = now_ms() / 1000;
    let deadline = now_s + 300;
    let permit = PermitTransfer {
        token,
        service_provider: *binding.service_provider(),
        user,
        receiver,
        value,
        max_fee: asset.transfer_fee + activation_fee,
        deadline,
        nonce: account.nonce,
    };
    let Ok(signed) = sign_permit(&network, permit, &secret, now_s) else {
        panic!("the authorization is not signed")
    };

    let lock = binding.sender_lock(&user);
    let guard = block_on(lock.lock());
    let submitted = match block_on(submit(&binding, &signed, guard, None, "the live token", &0.into())) {
        Ok(submitted) => submitted,
        Err(error) => panic!("the submission failed: {error}"),
    };
    let trace_id = submitted.trace_id.clone();
    let bound_ms = deadline * 1000 + SETTLEMENT_GRACE_MS;
    match block_on(track(&binding, submitted, bound_ms, None)) {
        Ok(hash) => println!("GasFree transfer {trace_id} settled, transaction {}", hash.to_hex()),
        Err(error) => panic!("the transfer did not settle: {error}"),
    }
}
