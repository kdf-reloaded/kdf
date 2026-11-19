//! The GasFree custody address of a user (CRD ch.63 §63.5, R2, R29) and its on-chain token balance (§63.7).
//!
//! The address is derived locally with TRON's `CREATE2` rule from the per-network constants of
//! [`super::networks`]. Before the gasless rail first uses a derived address, [`check_against_controller`]
//! compares it with the controller contract's own `getGasFreeAddress(address)` (R29).
//!
//! # Invariants
//!
//! - The derivation is a pure function of the network and the user address.
//! - Only a match is remembered by [`ControllerChecks`], and for 10 minutes only; a mismatch or a failed read
//!   drops what was remembered and is asked again next time.

use super::super::api::{TronApiClient, TronApiError};
use super::super::balance::trc20_balance_units;
use super::super::{Network, TronAddress};
use super::failure::GasFreeError;
use super::networks::{gasfree_network, INITIALIZE_SELECTOR};
use super::{address_word, keccak};
use common::log::warn;
use common::now_ms;
use ethereum_types::U256;
use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

/// The prefix byte of TRON's `CREATE2` preimage, where Ethereum uses `0xff` (§63.5 step 5).
const TRON_CREATE2_PREFIX: u8 = 0x41;

/// The read-only controller function that returns the custody address of a user.
const GET_GASFREE_ADDRESS: &str = "getGasFreeAddress(address)";

/// A derived custody address with the intermediate values of its derivation.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CustodyDerivation {
    /// §63.5 step 1: the user's 20-byte address, left-padded to 32 bytes.
    pub salt: [u8; 32],
    /// §63.5 step 4: keccak-256 of the init code.
    pub init_code_hash: [u8; 32],
    /// §63.5 step 6: the custody address.
    pub address: TronAddress,
}

/// Derives the GasFree custody address of `user` on `network` (§63.5 steps 1 to 6).
pub(crate) fn derive_custody(network: &Network, user: &TronAddress) -> CustodyDerivation {
    let constants = gasfree_network(network);

    // Step 1.
    let salt = address_word(user);

    // Step 2: `initialize(address)` with the user as its argument.
    let mut init_calldata = Vec::with_capacity(INITIALIZE_SELECTOR.len() + salt.len());
    init_calldata.extend_from_slice(&INITIALIZE_SELECTOR);
    init_calldata.extend_from_slice(&salt);

    // Step 3: the creation bytecode and the ABI encoding of `(address beacon, bytes init_calldata)`: the address
    // word, the offset of the bytes (two head words), their length, and the bytes right-padded to whole words.
    let padded_len = init_calldata.len().div_ceil(32) * 32;
    let mut init_code = Vec::with_capacity(constants.creation_bytecode.len() + 3 * 32 + padded_len);
    init_code.extend_from_slice(constants.creation_bytecode);
    init_code.extend_from_slice(&address_word(&constants.beacon()));
    init_code.extend_from_slice(&usize_word(2 * 32));
    init_code.extend_from_slice(&usize_word(init_calldata.len()));
    init_code.extend_from_slice(&init_calldata);
    init_code.resize(init_code.len() + padded_len - init_calldata.len(), 0);

    // Step 4.
    let init_code_hash = keccak(&init_code);

    // Step 5.
    let mut preimage = Vec::with_capacity(1 + 20 + 32 + 32);
    preimage.push(TRON_CREATE2_PREFIX);
    preimage.extend_from_slice(constants.controller().to_evm_address().as_ref());
    preimage.extend_from_slice(&salt);
    preimage.extend_from_slice(&init_code_hash);

    // Step 6.
    let digest = keccak(&preimage);
    let mut payload = [0u8; 20];
    payload.copy_from_slice(&digest[12..]);

    CustodyDerivation {
        salt,
        init_code_hash,
        address: TronAddress::from_evm_address(payload.into()),
    }
}

/// `value` as one 32-byte big-endian ABI word.
fn usize_word(value: usize) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[24..].copy_from_slice(&(value as u64).to_be_bytes());
    word
}

/// How long a confirmation by the controller stays valid, in milliseconds (R29): 10 minutes.
pub(crate) const CONFIRMATION_VALIDITY_MS: u64 = 600_000;

/// The (network, user) pairs whose derived custody address the controller has confirmed, each with the time of
/// its last confirmation (R29).
///
/// The gasless rail uses the one [`ControllerChecks::process_wide`] instance, which lives as long as the process:
/// the check runs again after a restart, and whenever the last confirmation is older than
/// [`CONFIRMATION_VALIDITY_MS`].
pub(crate) struct ControllerChecks {
    /// (EIP-712 chain id, 20-byte user address) to the time of the last confirmation, in milliseconds. Never
    /// held across an `.await`.
    confirmed: Mutex<BTreeMap<(u64, [u8; 20]), u64>>,
}

static PROCESS_WIDE_CHECKS: ControllerChecks = ControllerChecks::new();

impl ControllerChecks {
    pub(crate) const fn new() -> Self {
        ControllerChecks {
            confirmed: Mutex::new(BTreeMap::new()),
        }
    }

    /// The instance of the running process.
    pub(crate) fn process_wide() -> &'static ControllerChecks { &PROCESS_WIDE_CHECKS }

    fn key(network: &Network, user: &TronAddress) -> (u64, [u8; 20]) {
        (gasfree_network(network).chain_id, user.to_evm_address().0)
    }

    /// Whether a confirmation of the pair exists that is at most [`CONFIRMATION_VALIDITY_MS`] old at `now_ms`.
    /// A confirmation stamped later than `now_ms` (the clock was set back) does not count.
    fn is_confirmed(&self, network: &Network, user: &TronAddress, now_ms: u64) -> bool {
        let confirmed = self.confirmed.lock().unwrap_or_else(PoisonError::into_inner);
        confirmed
            .get(&Self::key(network, user))
            .and_then(|confirmed_ms| now_ms.checked_sub(*confirmed_ms))
            .is_some_and(|age_ms| age_ms <= CONFIRMATION_VALIDITY_MS)
    }

    fn confirm(&self, network: &Network, user: &TronAddress, now_ms: u64) {
        self.confirmed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(Self::key(network, user), now_ms);
    }

    fn forget(&self, network: &Network, user: &TronAddress) {
        self.confirmed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&Self::key(network, user));
    }
}

/// The warning line of a failed controller check (R29). It names the network and the three addresses, all of
/// them public values.
fn mismatch_warning(network: &str, user: &TronAddress, derived: &TronAddress, reported: &TronAddress) -> String {
    format!(
        "GasFree is refused on {network}: for the user {user} this build derives the custody address {derived}, \
         but the GasFree controller reports {reported}"
    )
}

/// Reads one 32-byte word from the read-only function `function_selector` of the controller of `network`, with
/// the controller as the calling address.
///
/// # Errors
///
/// The node client's error, or its invalid-response error when the answer is not exactly one 32-byte word.
pub(crate) async fn read_controller_word(
    api: &TronApiClient,
    network: &Network,
    function_selector: &str,
    parameter_hex: &str,
) -> Result<[u8; 32], TronApiError> {
    let controller = gasfree_network(network).controller();
    let answer = api
        .trigger_constant_read(&controller, &controller, function_selector, parameter_hex)
        .await?;
    let first = answer
        .constant_result
        .and_then(|results| results.into_iter().next())
        .ok_or_else(|| TronApiError::InvalidResponse(format!("{function_selector} returned no result")))?;
    let bytes = hex::decode(first)
        .map_err(|e| TronApiError::InvalidResponse(format!("{function_selector} result is not hex: {e}")))?;
    <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| {
        TronApiError::InvalidResponse(format!(
            "{function_selector} result is {} bytes, expected 32",
            bytes.len()
        ))
    })
}

/// Derives the custody address of `user` on `network` and requires the controller contract to derive the same
/// one (R29). A match is remembered in `checks` for [`CONFIRMATION_VALIDITY_MS`], so the controller is asked at
/// most once per (network, user) in that time.
///
/// # Errors
///
/// - [`GasFreeError::DerivationMismatch`] when the controller reports another address; one warning line is
///   logged and the pair is no longer remembered.
/// - [`GasFreeError::Node`] when the read fails; the pair is no longer remembered.
pub(crate) async fn check_against_controller(
    api: &TronApiClient,
    checks: &ControllerChecks,
    network: &Network,
    user: &TronAddress,
) -> Result<TronAddress, GasFreeError> {
    check_against_controller_at(api, checks, network, user, now_ms()).await
}

/// [`check_against_controller`] at the time `now_ms`, in milliseconds of the clock the confirmations are stamped
/// with.
async fn check_against_controller_at(
    api: &TronApiClient,
    checks: &ControllerChecks,
    network: &Network,
    user: &TronAddress,
    now_ms: u64,
) -> Result<TronAddress, GasFreeError> {
    let derived = derive_custody(network, user);
    if checks.is_confirmed(network, user, now_ms) {
        return Ok(derived.address);
    }
    let word = match read_controller_word(api, network, GET_GASFREE_ADDRESS, &hex::encode(derived.salt)).await {
        Ok(word) => word,
        Err(error) => {
            checks.forget(network, user);
            return Err(GasFreeError::Node(error));
        },
    };
    // The address is the last 20 bytes of the word.
    let mut payload = [0u8; 20];
    payload.copy_from_slice(&word[12..]);
    let reported = TronAddress::from_evm_address(payload.into());
    if reported != derived.address {
        checks.forget(network, user);
        let name = gasfree_network(network).name;
        warn!("{}", mismatch_warning(name, user, &derived.address, &reported));
        return Err(GasFreeError::DerivationMismatch { network: name });
    }
    checks.confirm(network, user, now_ms);
    Ok(derived.address)
}

/// The on-chain balance of the custody address `custody` in the TRC20 token at `token`, in the token's smallest
/// unit (§63.7), read with `balanceOf(address)`.
///
/// # Errors
///
/// [`GasFreeError::CustodyBalance`] when the read fails or its answer is not a usable balance.
pub(crate) async fn custody_token_balance(
    api: &TronApiClient,
    token: &TronAddress,
    custody: &TronAddress,
) -> Result<U256, GasFreeError> {
    trc20_balance_units(api, token, custody)
        .await
        .map_err(GasFreeError::CustodyBalance)
}

#[cfg(test)]
mod tests {
    use super::super::test_vectors::{address, hex32, CUSTODY_VECTORS, SHASTA_STALE_SDK_CUSTODY, USERS};
    use super::*;

    /// ch.63 T1: the mainnet and Nile pairs the public SDK publishes and the Shasta pairs the live controller
    /// returns, with the salt and the init-code hash of every derivation.
    #[test]
    fn the_derivation_reproduces_every_custody_vector() {
        for (network, vectors) in CUSTODY_VECTORS {
            for (user, vector) in USERS.iter().zip(vectors) {
                let derived = derive_custody(&network, &address(user.address));
                assert_eq!(derived.salt, hex32(user.salt));
                assert_eq!(derived.init_code_hash, hex32(vector.init_code_hash));
                assert_eq!(derived.address.to_base58(), vector.custody);
            }
        }
    }

    /// The Shasta vectors of the public SDK v1.1.2 describe the controller before its upgrade of 2026-07-04.
    #[test]
    fn the_shasta_derivation_is_not_the_stale_sdk_value() {
        for (user, stale) in USERS.iter().zip(SHASTA_STALE_SDK_CUSTODY) {
            let derived = derive_custody(&Network::Shasta, &address(user.address));
            assert_ne!(derived.address.to_base58(), stale);
        }
    }

    #[test]
    fn the_warning_names_the_network_and_the_addresses() {
        let user = address(USERS[0].address);
        let derived = derive_custody(&Network::Nile, &user).address;
        let reported = derive_custody(&Network::Mainnet, &user).address;
        let line = mismatch_warning("nile", &user, &derived, &reported);
        for part in ["nile", &user.to_base58(), &derived.to_base58(), &reported.to_base58()] {
            assert!(line.contains(part), "the warning lacks one of its parts");
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod controller_check_tests {
    use super::super::super::nodes::TronNode;
    use super::super::super::test_node::{Reply, StubNode};
    use super::super::failure::{GaslessMember, RpcArm};
    use super::super::test_vectors::{address, USERS};
    use super::*;
    use crate::eth::alloy_compat::transport::test_support::{captured_log_lines, install_log_capture};
    use common::block_on;

    fn client(node: &StubNode) -> TronApiClient {
        TronApiClient::new_with_cutoff_s("TRX", vec![TronNode::plain(node.url())], 5.)
    }

    /// A controller that answers `getGasFreeAddress` with `address`.
    fn controller_reporting(address: TronAddress) -> StubNode {
        let word = hex::encode(address_word(&address));
        StubNode::start(move |_, _| {
            Reply::ok(&format!(
                r#"{{"result":{{"result":true}},"constant_result":["{word}"]}}"#
            ))
        })
    }

    #[test]
    fn a_match_is_ok_and_the_controller_is_asked_once() {
        let user = address(USERS[0].address);
        let derived = derive_custody(&Network::Nile, &user);
        let node = controller_reporting(derived.address);
        let (api, checks) = (client(&node), ControllerChecks::new());

        let first = block_on(check_against_controller(&api, &checks, &Network::Nile, &user)).expect("a match");
        assert_eq!(first, derived.address);
        let second = block_on(check_against_controller(&api, &checks, &Network::Nile, &user)).expect("a match");
        assert_eq!(second, derived.address);

        // One request: the controller as owner and contract, the user's word as the parameter.
        let controller = gasfree_network(&Network::Nile).controller().to_base58();
        let bodies = node.bodies_for("/wallet/triggerconstantcontract");
        assert_eq!(bodies.len(), 1);
        assert_eq!(bodies[0]["owner_address"], controller.as_str());
        assert_eq!(bodies[0]["contract_address"], controller.as_str());
        assert_eq!(bodies[0]["function_selector"], "getGasFreeAddress(address)");
        assert_eq!(bodies[0]["parameter"], hex::encode(derived.salt).as_str());

        // The match holds for this network and user only.
        assert!(!checks.is_confirmed(&Network::Mainnet, &user, now_ms()));
        assert!(!checks.is_confirmed(&Network::Nile, &address(USERS[1].address), now_ms()));
    }

    #[test]
    fn a_mismatch_is_refused_as_unavailable_and_not_remembered() {
        let log_captured = install_log_capture();
        let user = address(USERS[1].address);
        let derived = derive_custody(&Network::Nile, &user).address;
        // The controller reports the address another network's constants give.
        let reported = derive_custody(&Network::Mainnet, &user).address;
        let node = controller_reporting(reported);
        let (api, checks) = (client(&node), ControllerChecks::new());

        let error = block_on(check_against_controller(&api, &checks, &Network::Nile, &user)).expect_err("a mismatch");
        assert!(matches!(error, GasFreeError::DerivationMismatch { network: "nile" }));
        assert_eq!(error.rpc_arm(), RpcArm::Gasless(GaslessMember::Unavailable));
        assert_eq!(GaslessMember::Unavailable.http_status(), 503);
        let text = error.to_string();
        assert!(text.contains("nile") && text.contains("no longer derives the expected address"));
        assert!(!checks.is_confirmed(&Network::Nile, &user, now_ms()));

        // Nothing was remembered: the next call asks the controller again.
        assert!(block_on(check_against_controller(&api, &checks, &Network::Nile, &user)).is_err());
        assert_eq!(node.requests().len(), 2);

        if log_captured {
            let expected = mismatch_warning("nile", &user, &derived, &reported);
            let lines: Vec<String> = captured_log_lines()
                .into_iter()
                .filter(|line| line.contains(&expected))
                .collect();
            // One line per refused call.
            assert_eq!(lines.len(), 2);
            assert!(lines.iter().all(|line| line.starts_with("WARN")));
        }
    }

    #[test]
    fn a_failed_read_is_the_node_error_and_not_remembered() {
        let user = address(USERS[2].address);
        let down = StubNode::start(|_, _| Reply::status(500, "{}"));
        let checks = ControllerChecks::new();
        let error =
            block_on(check_against_controller(&client(&down), &checks, &Network::Nile, &user)).expect_err("no read");
        assert!(matches!(error, GasFreeError::Node(_)));
        assert_eq!(error.rpc_arm(), RpcArm::Transport);
        assert!(!checks.is_confirmed(&Network::Nile, &user, now_ms()));

        // An answer that is not one 32-byte word is a failed read too.
        let short = StubNode::start(|_, _| Reply::ok(r#"{"result":{"result":true},"constant_result":["00ff"]}"#));
        let error = block_on(check_against_controller(
            &client(&short),
            &checks,
            &Network::Nile,
            &user,
        ))
        .expect_err("a short word");
        assert!(matches!(error, GasFreeError::Node(_)));
        assert!(!checks.is_confirmed(&Network::Nile, &user, now_ms()));
    }

    /// A stub controller whose answer a test can change, with the number of requests it got.
    fn switchable_controller(first: TronAddress) -> (StubNode, std::sync::Arc<std::sync::Mutex<TronAddress>>) {
        let reported = std::sync::Arc::new(std::sync::Mutex::new(first));
        let answer = reported.clone();
        let node = StubNode::start(move |_, _| {
            let address = *answer.lock().unwrap_or_else(PoisonError::into_inner);
            let word = hex::encode(address_word(&address));
            Reply::ok(&format!(
                r#"{{"result":{{"result":true}},"constant_result":["{word}"]}}"#
            ))
        });
        (node, reported)
    }

    #[test]
    fn a_confirmation_is_reused_for_ten_minutes_and_then_read_again() {
        let user = address(USERS[3].address);
        let derived = derive_custody(&Network::Nile, &user).address;
        let (node, _reported) = switchable_controller(derived);
        let (api, checks) = (client(&node), ControllerChecks::new());
        let start_ms = 1_000_000;
        let check_at = |now_ms: u64| {
            block_on(check_against_controller_at(
                &api,
                &checks,
                &Network::Nile,
                &user,
                now_ms,
            ))
        };

        assert_eq!(check_at(start_ms).expect("a match"), derived);
        assert_eq!(node.requests().len(), 1);
        // Within the validity, its last millisecond included: no request.
        for now_ms in [start_ms, start_ms + 1, start_ms + CONFIRMATION_VALIDITY_MS] {
            assert_eq!(check_at(now_ms).expect("a remembered match"), derived);
        }
        assert_eq!(node.requests().len(), 1);
        // Older than the validity: the controller is read again, and the match counts from that time.
        let later_ms = start_ms + CONFIRMATION_VALIDITY_MS + 1;
        assert_eq!(check_at(later_ms).expect("a match"), derived);
        assert_eq!(node.requests().len(), 2);
        assert_eq!(
            check_at(later_ms + CONFIRMATION_VALIDITY_MS).expect("a remembered match"),
            derived
        );
        assert_eq!(node.requests().len(), 2);
        // A clock that was set back does not keep a confirmation alive.
        assert_eq!(check_at(start_ms).expect("a match"), derived);
        assert_eq!(node.requests().len(), 3);
    }

    #[test]
    fn an_expired_confirmation_followed_by_a_mismatch_is_refused_and_dropped() {
        let user = address(USERS[4].address);
        let derived = derive_custody(&Network::Nile, &user).address;
        let (node, reported) = switchable_controller(derived);
        let (api, checks) = (client(&node), ControllerChecks::new());
        let start_ms = 1_000_000;
        let check_at = |now_ms: u64| {
            block_on(check_against_controller_at(
                &api,
                &checks,
                &Network::Nile,
                &user,
                now_ms,
            ))
        };

        assert_eq!(check_at(start_ms).expect("a match"), derived);
        // The controller changes its answer; the confirmation still holds until it expires.
        *reported.lock().unwrap_or_else(PoisonError::into_inner) = derive_custody(&Network::Mainnet, &user).address;
        assert_eq!(
            check_at(start_ms + CONFIRMATION_VALIDITY_MS).expect("a remembered match"),
            derived
        );
        assert_eq!(node.requests().len(), 1);

        let expired_ms = start_ms + CONFIRMATION_VALIDITY_MS + 1;
        let error = check_at(expired_ms).expect_err("a mismatch");
        assert!(matches!(error, GasFreeError::DerivationMismatch { network: "nile" }));
        assert_eq!(error.rpc_arm(), RpcArm::Gasless(GaslessMember::Unavailable));
        assert_eq!(GaslessMember::Unavailable.http_status(), 503);
        assert_eq!(node.requests().len(), 2);
        // The entry is gone: no time makes the pair confirmed, and the next call asks again.
        for now_ms in [start_ms, start_ms + 1, expired_ms] {
            assert!(!checks.is_confirmed(&Network::Nile, &user, now_ms));
        }
        assert!(check_at(start_ms + 1).is_err());
        assert_eq!(node.requests().len(), 3);
    }

    #[test]
    fn a_failed_read_drops_an_expired_confirmation() {
        let user = address(USERS[0].address);
        let checks = ControllerChecks::new();
        let start_ms = 1_000_000;
        checks.confirm(&Network::Shasta, &user, start_ms);
        assert!(checks.is_confirmed(&Network::Shasta, &user, start_ms + CONFIRMATION_VALIDITY_MS));
        let down = StubNode::start(|_, _| Reply::status(500, "{}"));
        let error = block_on(check_against_controller_at(
            &client(&down),
            &checks,
            &Network::Shasta,
            &user,
            start_ms + CONFIRMATION_VALIDITY_MS + 1,
        ))
        .expect_err("no read");
        assert!(matches!(error, GasFreeError::Node(_)));
        assert!(!checks.is_confirmed(&Network::Shasta, &user, start_ms));
    }

    #[test]
    fn the_custody_balance_is_read_with_balance_of() {
        let (token, custody) = (address(USERS[3].address), address(USERS[4].address));
        let node = StubNode::start(|_, _| {
            Reply::ok(&format!(
                r#"{{"result":{{"result":true}},"constant_result":["{:064x}"]}}"#,
                1_234_567u64
            ))
        });
        let balance = block_on(custody_token_balance(&client(&node), &token, &custody)).expect("a balance");
        assert_eq!(balance, U256::from(1_234_567u64));
        let bodies = node.bodies_for("/wallet/triggerconstantcontract");
        assert_eq!(bodies.len(), 1);
        assert_eq!(bodies[0]["contract_address"], token.to_base58().as_str());
        assert_eq!(bodies[0]["function_selector"], "balanceOf(address)");
        assert_eq!(bodies[0]["parameter"], hex::encode(address_word(&custody)).as_str());

        let down = StubNode::start(|_, _| Reply::status(500, "{}"));
        let error = block_on(custody_token_balance(&client(&down), &token, &custody)).expect_err("no read");
        assert!(matches!(error, GasFreeError::CustodyBalance(_)));
        assert_eq!(error.rpc_arm(), RpcArm::Transport);
    }
}
