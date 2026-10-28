//! CRD ch.39 R39.5.6-R39.5.11, maker side: a shielded coin's per-swap HTLC key is derived, never saved, and
//! re-derived on restart and in recovery. The coin is the mocked `TestCoin`, which derives its key with the
//! real derivation of `coins::z_coin::derive_swap_htlc_key_pair` from a base key chosen per test.
//!
//! The keys are the test-only keys of swept throwaway wallets (never fund) of the fixture shared with the
//! derivation vector test of the `coins` crate.

use super::maker_swap_tests::eth_tx_for_test;
use super::*;
use coins::{HtlcPaymentCheck, MarketCoinOps, MmCoin, SwapOps, TestCoin, Transaction};
use common::block_on;
use crypto::privkey::{key_pair_from_secret, key_pair_from_seed};
use mm2_core::mm_ctx::MmCtxBuilder;
use mocktopus::mocking::*;
use serde_json as json;
use std::cell::{Cell, RefCell};
use std::str::FromStr;

const VECTORS: &str = include_str!("../../../coins/z_coin/test-fixtures/arrr-htlc-key-derivation.json");
/// A finished maker swap (the saved events of a failed-payment swap); its HTLC fields are replaced below.
const SAVED: &str = r#"{"error_events":[],"events":[{"event":{"data":{"lock_duration":7800,"maker_amount":"3.54932734","maker_coin":"ARRR","maker_coin_start_block":1452970,"maker_payment_confirmations":1,"maker_payment_lock":1563759539,"my_persistent_pub":"031bb83b58ec130e28e0a6d5d2acf2eb01b0d3f1670e021d47d31db8a858219da8","secret":"0000000000000000000000000000000000000000000000000000000000000000","started_at":1563743939,"taker":"101ace6b08605b9424b0582b5cce044b70a3c8d8d10cb2965e039b0967ae92b9","taker_amount":"0.02004833998671660000000000","taker_coin":"RIN","taker_coin_start_block":8196380,"taker_payment_confirmations":1,"uuid":"3447b727-fe93-4357-8e5a-8cf2699b7e86"},"type":"Started"},"timestamp":1563743939211},{"event":{"data":{"taker_payment_locktime":1563751737,"taker_pubkey":"03101ace6b08605b9424b0582b5cce044b70a3c8d8d10cb2965e039b0967ae92b9"},"type":"Negotiated"},"timestamp":1563743979835},{"event":{"type":"Finished"},"timestamp":1563763243350}],"success_events":[],"uuid":"3447b727-fe93-4357-8e5a-8cf2699b7e86"}"#;

thread_local! {
    /// The base key the mocked coin derives from (the wallet's transparent key under the policy in force).
    static BASE_SECRET: Cell<[u8; 32]> = const { Cell::new([0; 32]) };
    /// The public key whose script the mocked payment pays.
    static PAYS: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    /// What a recovery tried, searched with and signed with.
    static TRIED: RefCell<Vec<Vec<u8>>> = const { RefCell::new(Vec::new()) };
    static SEARCHED_WITH: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static REFUND_PRIVKEY: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

fn vectors() -> json::Value { json::from_str(VECTORS).unwrap() }

fn vector_base_key(role: &str) -> KeyPair {
    let private = keys::Private::from_str(vectors()[role]["base_wif"].as_str().unwrap()).unwrap();
    KeyPair::from_private(private).unwrap()
}

fn vector_secret_hash() -> Vec<u8> { hex::decode(vectors()["secret_hash"].as_str().unwrap()).unwrap() }

fn vector_maker_arrr_pub() -> Vec<u8> { hex::decode(vectors()["maker"]["arrr_htlc_pub"].as_str().unwrap()).unwrap() }

fn mock_deriving_coin() {
    TestCoin::ticker.mock_safe(|_| MockResult::Return("ARRR"));
    TestCoin::swap_contract_address.mock_safe(|_| MockResult::Return(None));
    TestCoin::derives_swap_htlc_key.mock_safe(|_| MockResult::Return(true));
    TestCoin::derive_swap_htlc_key_pair.mock_safe(|_, input| {
        let base = key_pair_from_secret(&BASE_SECRET.with(Cell::get)).unwrap();
        MockResult::Return(coins::z_coin::derive_swap_htlc_key_pair(&base, input))
    });
}

/// The saved maker swap with the given negotiated key and optional saved private key.
fn saved_swap(pubkey: &[u8], privkey: Option<KeyPair>) -> MakerSavedSwap {
    let mut saved: MakerSavedSwap = json::from_str(SAVED).unwrap();
    match saved.events[0].event {
        MakerSwapEvent::Started(ref mut data) => {
            data.secret_hash = Some(vector_secret_hash().into());
            data.maker_coin_htlc_pubkey = Some(H264::from(pubkey).into());
            data.maker_coin_htlc_privkey = privkey.map(SerializableSecp256k1Keypair::from);
        },
        _ => unreachable!(),
    }
    saved
}

/// Restores the swap under the wallet whose transparent key is `base`.
fn restore(saved: MakerSavedSwap, base: &KeyPair) -> (MakerSwap, KeyPair) {
    BASE_SECRET.with(|b| b.set(base.private().secret.take()));
    mock_deriving_coin();
    let persistent =
        key_pair_from_seed("spice describe gravity federal blast come thank unfair canal monkey style afraid").unwrap();
    let ctx = MmCtxBuilder::default()
        .with_conf(json!({"netid": 8762}))
        .with_secp256k1_key_pair(persistent)
        .into_mm_arc();
    let maker_coin = MmCoinEnum::Test(TestCoin::default());
    let taker_coin = MmCoinEnum::Test(TestCoin::default());
    let (swap, _) = MakerSwap::load_from_saved(ctx, maker_coin, taker_coin, saved).unwrap();
    (swap, persistent)
}

fn saved_payment() -> TransactionIdentifier {
    TransactionIdentifier {
        tx_hex: eth_tx_for_test().tx_hex().into(),
        tx_hash: vec![0u8; 32].into(),
    }
}

/// Mocks the coin so that a payment pays the script of exactly the public key `pays`, and records what the
/// recovery tried and signed with.
fn mock_payment_of(pays: Vec<u8>) {
    PAYS.with(|p| *p.borrow_mut() = pays);
    TestCoin::check_payment_pays_htlc.mock_safe(|_, _, first, _, _, _| {
        TRIED.with(|t| t.borrow_mut().push(first.to_vec()));
        let check = if PAYS.with(|p| p.borrow().as_slice() == first) {
            HtlcPaymentCheck::Pays
        } else {
            HtlcPaymentCheck::PaysAnotherScript {
                payment_id: "abcd".to_owned(),
            }
        };
        MockResult::Return(Box::pin(futures::future::ready(Ok(check))))
    });
    TestCoin::check_if_my_payment_sent.mock_safe(|_, _, my_pub, _, _, _, _| {
        SEARCHED_WITH.with(|s| *s.borrow_mut() = my_pub.to_vec());
        let found = PAYS.with(|p| p.borrow().as_slice() == my_pub);
        MockResult::Return(Box::new(futures01::future::ok(found.then(|| eth_tx_for_test().into()))))
    });
    TestCoin::search_for_swap_tx_spend_my
        .mock_safe(|_, _, _, _, _, _, _, _| MockResult::Return(Box::pin(futures::future::ready(Ok(None)))));
    TestCoin::send_maker_refunds_payment.mock_safe(|_, _, _, _, _, privkey, _| {
        REFUND_PRIVKEY.with(|r| *r.borrow_mut() = privkey.to_vec());
        MockResult::Return(Box::new(futures01::future::ok(eth_tx_for_test().into())))
    });
}

fn reset_captures() {
    TRIED.with(|t| t.borrow_mut().clear());
    SEARCHED_WITH.with(|s| s.borrow_mut().clear());
    REFUND_PRIVKEY.with(|r| r.borrow_mut().clear());
}

fn tried() -> Vec<Vec<u8>> { TRIED.with(|t| t.borrow().clone()) }
fn searched_with() -> Vec<u8> { SEARCHED_WITH.with(|s| s.borrow().clone()) }
fn refund_privkey() -> Vec<u8> { REFUND_PRIVKEY.with(|r| r.borrow().clone()) }

/// T39.5.b: the saved record of a swap that derives its key holds no HTLC private key; after the restart (the
/// replay of the saved events) the key is re-derived from the saved secret hash, is the negotiated key, and
/// signs the refund recovery builds. The unsaved payment is searched with the re-derived key (R39.5.11(c)).
#[test]
fn derived_key_is_not_saved_and_is_rederived_on_restart_and_signs_the_refund() {
    reset_captures();
    let base = vector_base_key("maker");
    let saved = saved_swap(&vector_maker_arrr_pub(), None);

    // The record carries no private key for either leg.
    let serialized = json::to_value(&saved).unwrap();
    assert_eq!(
        serialized["events"][0]["event"]["data"]["maker_coin_htlc_privkey"],
        json::Value::Null
    );
    assert_eq!(
        serialized["events"][0]["event"]["data"]["taker_coin_htlc_privkey"],
        json::Value::Null
    );

    let (swap, _) = restore(saved, &base);
    let held = swap.r().my_maker_coin_htlc_keypair;
    assert_eq!(held.public().to_vec(), vector_maker_arrr_pub());
    assert_eq!(swap.r().my_maker_coin_htlc_pub.to_vec(), vector_maker_arrr_pub());
    swap.check_rederived_htlc_keys()
        .expect("the re-derived keys are the negotiated keys");

    mock_payment_of(vector_maker_arrr_pub());
    let recovered = block_on(swap.recover_funds()).unwrap();
    assert_eq!(recovered.action, RecoveredSwapAction::RefundedMyPayment);
    assert_eq!(searched_with(), vector_maker_arrr_pub());
    // The refund is signed with the re-derived private key, whose public half is the key in the HTLC script.
    let refund_key = key_pair_from_secret(&refund_privkey()).unwrap();
    assert_eq!(refund_key.public().to_vec(), vector_maker_arrr_pub());
    assert_eq!(tried(), vec![vector_maker_arrr_pub()]);
}

/// T39.5.c, and the policy change of D39.5.a: the same saved swap restored under another wallet key (another
/// seed, or the same seed under the other key policy) re-derives a different key. With the payment saved,
/// recovery fails with the script-mismatch class; without it, with the not-found class. There is no saved
/// private key to fall back to, and the persistent key does not pay the script either.
#[test]
fn another_base_key_fails_with_the_script_mismatch_or_the_not_found_class() {
    reset_captures();
    let other_base = vector_base_key("taker");

    // Payment not saved: the already-sent check searches with the re-derived key and finds nothing.
    let (swap, _) = restore(saved_swap(&vector_maker_arrr_pub(), None), &other_base);
    assert_ne!(
        swap.r().my_maker_coin_htlc_keypair.public().to_vec(),
        vector_maker_arrr_pub()
    );
    let err = swap.check_rederived_htlc_keys().unwrap_err();
    assert!(err.contains("not resumed"), "{err}");
    assert!(
        !err.contains(&hex::encode(vector_maker_arrr_pub())),
        "no key in the message: {err}"
    );
    mock_payment_of(vector_maker_arrr_pub());
    let err = block_on(swap.recover_funds()).unwrap_err();
    assert!(err.contains("Maker payment transaction was not found"), "{err}");
    assert_ne!(searched_with(), vector_maker_arrr_pub());

    // Payment saved: nothing pays the re-derived key's script.
    swap.w().maker_payment = Some(saved_payment());
    let err = block_on(swap.recover_funds()).unwrap_err();
    assert!(
        err.contains("does not pay the HTLC script this swap negotiated"),
        "{err}"
    );
    assert!(!err.contains("No private key is held"), "{err}");
    assert!(refund_privkey().is_empty(), "nothing is built");
}

/// R39.5.4 amendment, in-flight compatibility: a swap an earlier build made with a random per-swap key and
/// its saved private key keeps that key, and recovery tries the candidates in order: (1) the re-derived key,
/// (2) the saved pair, (3) the persistent key.
#[test]
fn a_swap_of_an_earlier_build_keeps_its_saved_key_and_candidates_are_tried_in_order() {
    reset_captures();
    let base = vector_base_key("maker");
    let saved_pair = KeyPair::random_compressed();
    let saved_pub = saved_pair.public().to_vec();
    let derived_pub = vector_maker_arrr_pub();

    // Restored: the saved pair is held, and the swap may resume with it.
    let (swap, persistent) = restore(saved_swap(&saved_pub, Some(saved_pair)), &base);
    assert_eq!(swap.r().my_maker_coin_htlc_keypair.public().to_vec(), saved_pub);
    swap.check_rederived_htlc_keys()
        .expect("a saved pair that owns the saved public key resumes");

    // (2) The payment pays the saved key: the derived key is tried first, then the saved pair signs.
    mock_payment_of(saved_pub.clone());
    let recovered = block_on(swap.recover_funds()).unwrap();
    assert_eq!(recovered.action, RecoveredSwapAction::RefundedMyPayment);
    assert_eq!(
        searched_with(),
        saved_pub,
        "the unsaved payment is searched with the saved key"
    );
    assert_eq!(tried(), vec![derived_pub.clone(), saved_pub.clone()]);
    assert_eq!(
        refund_privkey(),
        saved_pair.private().secret.to_vec(),
        "the saved private key signs"
    );

    // (3) The payment pays the persistent key: all three are tried, in order, and the persistent key signs.
    reset_captures();
    mock_payment_of(persistent.public().to_vec());
    swap.w().maker_payment = Some(saved_payment());
    let recovered = block_on(swap.recover_funds()).unwrap();
    assert_eq!(recovered.action, RecoveredSwapAction::RefundedMyPayment);
    assert_eq!(tried(), vec![derived_pub, saved_pub, persistent.public().to_vec()]);
    assert_eq!(refund_privkey(), persistent.private().secret.to_vec());
}

/// D39.5.a, policy in force: a swap made under the HD base key is recovered with the HD key; restored under the
/// seed policy's base key it derives another key and, with a saved private key that does not belong to the saved
/// public key, ends in the named "no private key is held" stop (R39.5.4), before anything is built.
#[test]
fn a_swap_restored_under_the_other_key_policy_ends_in_the_named_stop() {
    reset_captures();
    let hd_base = vector_base_key("maker");
    let seed_base = vector_base_key("taker");
    let derived_pub = vector_maker_arrr_pub();
    let unrelated_pair = KeyPair::random_compressed();

    // Under the policy the swap was made with, the derived key is the first candidate and signs.
    let (swap, _) = restore(saved_swap(&derived_pub, Some(unrelated_pair)), &hd_base);
    mock_payment_of(derived_pub.clone());
    swap.w().maker_payment = Some(saved_payment());
    block_on(swap.recover_funds()).unwrap();
    assert_eq!(tried(), vec![derived_pub.clone()]);
    let signing = key_pair_from_secret(&refund_privkey()).unwrap();
    assert_eq!(signing.public().to_vec(), derived_pub);

    // Under the other policy: another key is derived; the saved private key is not the saved key's.
    reset_captures();
    let (swap, _) = restore(saved_swap(&derived_pub, Some(unrelated_pair)), &seed_base);
    mock_payment_of(derived_pub.clone());
    swap.w().maker_payment = Some(saved_payment());
    let err = block_on(swap.recover_funds()).unwrap_err();
    assert!(err.contains("No private key is held for the HTLC public key"), "{err}");
    assert!(refund_privkey().is_empty(), "nothing is built");
}

/// A coin that does not derive its key is untouched: its saved key is restored and saved as before.
#[test]
fn a_coin_that_does_not_derive_keeps_its_saved_private_key() {
    let saved_pair = KeyPair::random_compressed();
    let mut saved = saved_swap(saved_pair.public(), Some(saved_pair));
    BASE_SECRET.with(|b| b.set([1; 32]));
    TestCoin::ticker.mock_safe(|_| MockResult::Return("KMD"));
    TestCoin::swap_contract_address.mock_safe(|_| MockResult::Return(None));
    TestCoin::derives_swap_htlc_key.mock_safe(|_| MockResult::Return(false));
    let ctx = MmCtxBuilder::default()
        .with_conf(json!({"netid": 8762}))
        .with_secp256k1_key_pair(
            key_pair_from_seed("spice describe gravity federal blast come thank unfair canal monkey style afraid")
                .unwrap(),
        )
        .into_mm_arc();
    if let MakerSwapEvent::Started(ref mut data) = saved.events[0].event {
        data.maker_coin = "KMD".into();
    }
    let (swap, _) = MakerSwap::load_from_saved(
        ctx,
        MmCoinEnum::Test(TestCoin::default()),
        MmCoinEnum::Test(TestCoin::default()),
        saved,
    )
    .unwrap();
    assert_eq!(
        swap.r().my_maker_coin_htlc_keypair.private().secret,
        saved_pair.private().secret
    );
    swap.check_rederived_htlc_keys()
        .expect("a coin that does not derive has nothing to re-derive");
}

/// T39.5.b, start side: what a swap machine takes from a coin at start. A coin that derives its key gives the
/// derived pair (the reference's key for the same secret hash) and nothing to save; any other coin gives its pair
/// and the private key to save, as before.
#[test]
fn start_saves_no_private_key_for_a_derived_key_and_still_saves_any_other() {
    BASE_SECRET.with(|b| b.set(vector_base_key("maker").private().secret.take()));
    mock_deriving_coin();
    let coin = MmCoinEnum::Test(TestCoin::default());
    let (pair, saved) = start_htlc_key_pair(&coin, &vector_secret_hash()).unwrap();
    assert_eq!(pair.unwrap().public().to_vec(), vector_maker_arrr_pub());
    assert!(saved.is_none(), "a derived key is never written to the swap record");

    TestCoin::derives_swap_htlc_key.mock_safe(|_| MockResult::Return(false));
    let kept = KeyPair::random_compressed();
    TestCoin::get_htlc_key_pair.mock_safe(move |_| MockResult::Return(Some(kept)));
    let (pair, saved) = start_htlc_key_pair(&coin, &vector_secret_hash()).unwrap();
    assert_eq!(pair.unwrap().public(), kept.public());
    assert_eq!(saved.unwrap().priv_key(), kept.private().secret.take());
}
