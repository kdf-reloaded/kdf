//! The value an EVM legacy swap contract call carries for the shared swap secret hash (CRD ch.51 R72A).
//!
//! The literals below were computed outside the code under test: `SHARED_HASH_32` is SHA-256 of 32 bytes of
//! `0x42`, `DERIVED_20` is RIPEMD-160 of those 32 hash bytes, and `SWAP_ID_32` is SHA-256 of the little-endian
//! `u32` time lock followed by `SHARED_HASH_32`. Payment building runs against a mock that captures the calldata
//! `sign_and_send_transaction` was given, so no node is needed.

use super::eth_gas_limit_call_site_tests::{eth_coin, token_coin};
use super::*;
use common::block_on;
use mocktopus::mocking::*;
use serde_json::json;
use std::cell::RefCell;

/// SHA-256 of 32 bytes of 0x42: the 32-byte shared value of ch.51 R72.
const SHARED_HASH_32: &str = "425ed4e4a36b30ea21b90e21c712c649e8214c29b7eaf68089d1039c6e55384c";
/// RIPEMD-160 of `SHARED_HASH_32`: the value the swap contract's `bytes20` slot must carry.
const DERIVED_20: &str = "8739f40ec4dbf569dcb38134c6e7310908566981";
/// SHA-256 of `1_800_000_000u32` little-endian followed by `SHARED_HASH_32`.
const SWAP_ID_32: &str = "7ab1069172a6ee74f38e0a0e76274430d5b34fa692a12db07ac75fe9003f1104";
const TIME_LOCK: u32 = 1_800_000_000;
const SWAP_CONTRACT_ADDR: &str = "7Bc1bBDD6A0a722fC9bffC49c921B685ECB84b94";

thread_local! {
    /// The calldata `sign_and_send_transaction` was called with, in call order.
    static SENT_DATA: RefCell<Vec<Vec<u8>>> = const { RefCell::new(Vec::new()) };
}

fn shared_hash_32() -> Vec<u8> { hex::decode(SHARED_HASH_32).unwrap() }

fn derived_20() -> Vec<u8> { hex::decode(DERIVED_20).unwrap() }

/// Makes `sign_and_send_transaction` record its calldata and fail, so the payment is observed without a node.
fn capture_sent_data() {
    SENT_DATA.with(|data| data.borrow_mut().clear());
    EthCoin::allowance.mock_safe(|_, _| MockResult::Return(Box::new(futures01::future::ok(U256::max_value()))));
    EthCoin::sign_and_send_transaction.mock_safe(|_, _value, _action, data, _gas| {
        SENT_DATA.with(|sent| sent.borrow_mut().push(data));
        MockResult::Return(Box::new(futures01::future::err::<SignedEthTx, TransactionErr>(
            TransactionErr::Plain("captured".to_owned()),
        )))
    });
}

fn sent_data() -> Vec<Vec<u8>> { SENT_DATA.with(|data| data.borrow().clone()) }

/// Sends a payment for `secret_hash` and returns the result of the call.
fn send_payment(coin: &EthCoin, secret_hash: &[u8]) -> Result<SignedEthTx, TransactionErr> {
    let swap = Address::from_str(SWAP_CONTRACT_ADDR).unwrap();
    block_on(
        coin.send_hash_time_locked_payment(vec![1; 32], U256::one(), TIME_LOCK, secret_hash, swap, swap)
            .compat(),
    )
}

/// The decoded arguments of the one payment call that was captured, for the function of the coin.
fn captured_payment_args(function: &str) -> Vec<Token> {
    let sent = sent_data();
    assert_eq!(sent.len(), 1, "exactly one payment call is expected");
    SWAP_CONTRACT
        .function(function)
        .unwrap()
        .decode_input(&sent[0][4..])
        .unwrap()
}

// ─── The helper ─────────────────────────────────────────────────────────────────────────────────────

#[test]
fn r72a_helper_keeps_a_20_byte_value_unchanged() {
    let narrow = [0x5au8; 20];
    assert_eq!(contract_secret_hash(&narrow).unwrap(), narrow);
}

#[test]
fn r72a_helper_derives_ripemd160_of_a_32_byte_value() {
    let derived = contract_secret_hash(&shared_hash_32()).unwrap();
    assert_eq!(derived.to_vec(), derived_20());
}

#[test]
fn r72a_helper_32_byte_derivation_equals_the_20_byte_default_of_the_same_secret() {
    // The 20-byte default is RIPEMD-160(SHA-256(secret)); the 32-byte alternate is SHA-256(secret), so the
    // derived value of the one equals the other for the same secret.
    let secret = [0x42u8; 32];
    let wide = sha256(&secret);
    let narrow = kdf_crypto::ripemd160(wide.as_slice());
    assert_eq!(
        contract_secret_hash(wide.as_slice()).unwrap().to_vec(),
        narrow.as_slice()
    );
    assert_eq!(wide.as_slice(), shared_hash_32().as_slice());
}

#[test]
fn r72a_helper_rejects_other_widths_with_the_shared_text() {
    for len in [0usize, 19, 21, 33] {
        let err = contract_secret_hash(&vec![0u8; len]).unwrap_err();
        assert_eq!(
            err,
            format!("secret_hash must be 20 or 32 bytes (ch.51 R65/R71/R72), got {}", len)
        );
    }
}

// ─── Payment building ───────────────────────────────────────────────────────────────────────────────

#[test]
fn r72a_eth_payment_embeds_the_derived_value_for_a_32_byte_secret_hash() {
    capture_sent_data();
    let coin = eth_coin(json!({}));
    let _ = send_payment(&coin, &shared_hash_32());
    let args = captured_payment_args("ethPayment");
    assert_eq!(args[2], Token::FixedBytes(derived_20()));
}

#[test]
fn r72a_erc20_payment_embeds_the_derived_value_for_a_32_byte_secret_hash() {
    capture_sent_data();
    let coin = token_coin(json!({}), json!({}));
    let _ = send_payment(&coin, &shared_hash_32());
    let args = captured_payment_args("erc20Payment");
    assert_eq!(args[4], Token::FixedBytes(derived_20()));
}

#[test]
fn r72a_eth_payment_keeps_a_20_byte_secret_hash_unchanged() {
    capture_sent_data();
    let coin = eth_coin(json!({}));
    let _ = send_payment(&coin, &derived_20());
    let args = captured_payment_args("ethPayment");
    assert_eq!(args[2], Token::FixedBytes(derived_20()));
}

#[test]
fn r72a_erc20_payment_keeps_a_20_byte_secret_hash_unchanged() {
    capture_sent_data();
    let coin = token_coin(json!({}), json!({}));
    let _ = send_payment(&coin, &[0x5au8; 20]);
    let args = captured_payment_args("erc20Payment");
    assert_eq!(args[4], Token::FixedBytes(vec![0x5au8; 20]));
}

#[test]
fn r72a_payment_with_a_wrongly_sized_secret_hash_is_not_built() {
    capture_sent_data();
    let eth = eth_coin(json!({}));
    let token = token_coin(json!({}), json!({}));
    for coin in [&eth, &token] {
        match send_payment(coin, &[0u8; 33]) {
            Err(TransactionErr::Plain(e)) => assert!(e.contains("got 33"), "unexpected error text: {}", e),
            _ => panic!("a payment with a 33-byte secret hash must be refused before it is built"),
        }
    }
    assert!(sent_data().is_empty(), "no transaction may be sent");
}

#[test]
fn r72a_swap_id_is_computed_over_the_shared_value() {
    let coin = eth_coin(json!({}));
    assert_eq!(
        hex::encode(coin.etomic_swap_id(TIME_LOCK, &shared_hash_32())),
        SWAP_ID_32
    );
}

// ─── Payment validation ─────────────────────────────────────────────────────────────────────────────

#[test]
fn r72a_validation_accepts_the_derived_value_for_a_32_byte_secret_hash() {
    let slot = Token::FixedBytes(derived_20());
    check_payment_secret_hash_arg(&slot, &shared_hash_32()).unwrap();
}

#[test]
fn r72a_validation_refuses_the_raw_leading_bytes_and_names_the_derived_value() {
    // A payment built with the shared 32 bytes carries only their leading 20 bytes in the `bytes20` slot.
    let shared = shared_hash_32();
    let slot = Token::FixedBytes(shared[..20].to_vec());
    let err = check_payment_secret_hash_arg(&slot, &shared).unwrap_err();
    assert!(err.contains("secret_hash arg"), "unexpected error text: {}", err);
    assert!(err.contains("is invalid"), "unexpected error text: {}", err);
    let expected = format!("expected {:?}", Token::FixedBytes(derived_20()));
    assert!(err.contains(&expected), "unexpected error text: {}", err);
}

#[test]
fn r72a_validation_of_a_20_byte_secret_hash_is_unchanged() {
    let narrow = vec![0x5au8; 20];
    check_payment_secret_hash_arg(&Token::FixedBytes(narrow.clone()), &narrow).unwrap();
    let other = Token::FixedBytes(vec![0x5bu8; 20]);
    let err = check_payment_secret_hash_arg(&other, &narrow).unwrap_err();
    assert!(err.contains("is invalid"), "unexpected error text: {}", err);
    assert!(
        err.contains(&format!("expected {:?}", Token::FixedBytes(narrow))),
        "unexpected error text: {}",
        err
    );
}

#[test]
fn r72a_validation_of_a_wrongly_sized_secret_hash_is_an_error() {
    let slot = Token::FixedBytes(vec![0u8; 20]);
    let err = check_payment_secret_hash_arg(&slot, &[0u8; 21]).unwrap_err();
    assert!(err.contains("got 21"), "unexpected error text: {}", err);
}
