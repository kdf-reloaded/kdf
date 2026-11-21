//! Checks of the GasFree constants against the live controller contracts (CRD ch.63 T1, T2), through public TRON
//! nodes. They are read-only and need no key. They run only with the `live-network-tests` feature
//! (docs/UNIT_TESTS.md, "Tests that need public servers").

use super::super::api::TronApiClient;
use super::super::nodes::TronNode;
use super::super::Network;
use super::address_word;
use super::authorization::{typed_data_hash, PermitTransfer, PERMIT_VERSION};
use super::custody::{check_against_controller, read_controller_word, ControllerChecks};
use super::test_vectors::{address, hex32, CUSTODY_VECTORS, DOMAIN_VECTORS, MESSAGES, MESSAGE_MAX_FEE,
                          MESSAGE_SERVICE_PROVIDER, USERS};
use common::block_on;
use ethereum_types::U256;
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
