//! CRD ch.05 §5.12C (R41, R43; T19, T21, T22, T23), swap-machine side: which HTLC key a coin announces and
//! saves at swap start, and which key a resumed or recovered swap uses.
//!
//! The coin is the mocked `TestCoin`. Its `get_htlc_key_pair` answers nothing and its
//! `own_signing_key_pair` answers the key the coin signs with, as a single-key UTXO coin or an EVM coin with a
//! software signer does. Which key that is for each real coin is tested in the `coins` crate.

use super::maker_swap_tests::eth_tx_for_test;
use super::*;
use coins::{HtlcPaymentCheck, MarketCoinOps, MmCoin, SwapOps, TestCoin};
use common::block_on;
use crypto::privkey::key_pair_from_seed;
use mm2_core::mm_ctx::MmCtxBuilder;
use mocktopus::mocking::*;
use serde_json as json;
use std::cell::RefCell;

/// A finished maker swap (the saved events of a failed-payment swap); its HTLC fields are set below.
const SAVED: &str = r#"{"error_events":[],"events":[{"event":{"data":{"lock_duration":7800,"maker_amount":"3.54932734","maker_coin":"KMD","maker_coin_start_block":1452970,"maker_payment_confirmations":1,"maker_payment_lock":1563759539,"my_persistent_pub":"031bb83b58ec130e28e0a6d5d2acf2eb01b0d3f1670e021d47d31db8a858219da8","secret":"0000000000000000000000000000000000000000000000000000000000000000","started_at":1563743939,"taker":"101ace6b08605b9424b0582b5cce044b70a3c8d8d10cb2965e039b0967ae92b9","taker_amount":"0.02004833998671660000000000","taker_coin":"RIN","taker_coin_start_block":8196380,"taker_payment_confirmations":1,"uuid":"3447b727-fe93-4357-8e5a-8cf2699b7e86"},"type":"Started"},"timestamp":1563743939211},{"event":{"data":{"taker_payment_locktime":1563751737,"taker_pubkey":"03101ace6b08605b9424b0582b5cce044b70a3c8d8d10cb2965e039b0967ae92b9"},"type":"Negotiated"},"timestamp":1563743979835},{"event":{"type":"Finished"},"timestamp":1563763243350}],"success_events":[],"uuid":"3447b727-fe93-4357-8e5a-8cf2699b7e86"}"#;

thread_local! {
    /// The key the mocked payment pays the script of.
    static PAYS: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    /// The public key an already-sent search was made with, and the private key a refund was signed with.
    static SEARCHED_WITH: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static REFUND_PRIVKEY: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

fn persistent_pair() -> KeyPair {
    key_pair_from_seed("spice describe gravity federal blast come thank unfair canal monkey style afraid").unwrap()
}

/// The key of a coin whose key is not the node's persistent key: the internal key of an HD login.
fn coin_pair() -> KeyPair { key_pair_from_seed("the key a coin signs its swap transactions with").unwrap() }

fn ctx_of(persistent: KeyPair) -> MmArc {
    MmCtxBuilder::default()
        .with_conf(json!({"netid": 8762}))
        .with_secp256k1_key_pair(persistent)
        .into_mm_arc()
}

/// A coin that keeps its key (derives none) and signs with `own`.
fn mock_coin_signing_with(own: Option<KeyPair>) -> MmCoinEnum {
    TestCoin::ticker.mock_safe(|_| MockResult::Return("KMD"));
    TestCoin::swap_contract_address.mock_safe(|_| MockResult::Return(None));
    TestCoin::derives_swap_htlc_key.mock_safe(|_| MockResult::Return(false));
    TestCoin::get_htlc_key_pair.mock_safe(|_| MockResult::Return(None));
    TestCoin::own_signing_key_pair.mock_safe(move |_| MockResult::Return(own));
    MmCoinEnum::Test(TestCoin::default())
}

/// The saved maker swap with the given negotiated maker-coin key and optional saved key pair; `None` for both
/// is a swap saved by the previous build, which wrote neither field.
fn saved_swap(pubkey: Option<&[u8]>, pair: Option<KeyPair>) -> MakerSavedSwap {
    let mut saved: MakerSavedSwap = json::from_str(SAVED).unwrap();
    match saved.events[0].event {
        MakerSwapEvent::Started(ref mut data) => {
            data.secret_hash = Some(vec![7u8; 20].into());
            data.maker_coin_htlc_pubkey = pubkey.map(|p| H264::from(p).into());
            data.maker_coin_htlc_privkey = pair.map(SerializableSecp256k1Keypair::from);
        },
        _ => unreachable!(),
    }
    saved
}

fn restore(saved: MakerSavedSwap, coin: MmCoinEnum) -> MakerSwap {
    let (swap, _) = MakerSwap::load_from_saved(ctx_of(persistent_pair()), coin.clone(), coin, saved).unwrap();
    swap
}

/// Mocks the coin so that a payment pays the script of exactly the public key `pays`, and records the key the
/// already-sent search was made with and the private key the refund is signed with.
fn mock_payment_of(pays: Vec<u8>) {
    PAYS.with(|p| *p.borrow_mut() = pays);
    TestCoin::check_payment_pays_htlc.mock_safe(|_, _, first, _, _, _| {
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

fn searched_with() -> Vec<u8> { SEARCHED_WITH.with(|s| s.borrow().clone()) }
fn refund_privkey() -> Vec<u8> { REFUND_PRIVKEY.with(|r| r.borrow().clone()) }

/// T22 (R41, R43): under an iguana login the coin's key is the node's persistent key. The swap start holds no
/// pair and saves none, and the key on the negotiation wire is the persistent key, as before this section.
#[test]
fn t22_an_iguana_login_saves_no_pair_and_announces_the_persistent_key() {
    let persistent = persistent_pair();
    let coin = mock_coin_signing_with(Some(persistent));

    let (pair, saved) = start_htlc_key_pair(&coin, persistent.public(), &[1u8; 16]).unwrap();
    assert!(pair.is_none(), "the persistent key is not held as a pair of the coin");
    assert!(saved.is_none(), "an iguana login saves no HTLC key pair");

    let ctx = ctx_of(persistent);
    let (maker_pub, taker_pub) = derive_htlc_pubkeys(&ctx, &coin, &pair, &coin, &pair).unwrap();
    assert_eq!(maker_pub.to_vec(), persistent.public().to_vec());
    assert_eq!(taker_pub.to_vec(), persistent.public().to_vec());

    // A coin that has no key of its own (a hardware signer, a coin that signs with another curve) is the same.
    let coin = mock_coin_signing_with(None);
    let (pair, saved) = start_htlc_key_pair(&coin, persistent.public(), &[1u8; 16]).unwrap();
    assert!(pair.is_none() && saved.is_none());
}

/// T21 and T19 (R41 items 2 and 3, R43): a coin whose own key is not the persistent key, a single-key UTXO coin
/// under a global-HD login or an EVM coin with a software signer, announces its own key and saves the pair.
#[test]
fn t21_a_coin_key_other_than_the_persistent_key_is_announced_and_saved() {
    let persistent = persistent_pair();
    let own = coin_pair();
    let coin = mock_coin_signing_with(Some(own));

    let (pair, saved) = start_htlc_key_pair(&coin, persistent.public(), &[1u8; 16]).unwrap();
    assert_eq!(pair.unwrap().public(), own.public());
    assert_eq!(saved.unwrap().priv_key(), own.private().secret.take());

    let ctx = ctx_of(persistent);
    let (maker_pub, taker_pub) = derive_htlc_pubkeys(&ctx, &coin, &pair, &coin, &pair).unwrap();
    assert_eq!(maker_pub.to_vec(), own.public().to_vec());
    assert_eq!(taker_pub.to_vec(), own.public().to_vec());
}

/// R41: a coin with an HD key policy (or Tendermint) keeps supplying its key through `get_htlc_key_pair`, and
/// saves it as before.
#[test]
fn an_hd_key_policy_pair_is_still_saved() {
    let persistent = persistent_pair();
    let hd_key = coin_pair();
    mock_coin_signing_with(None);
    TestCoin::get_htlc_key_pair.mock_safe(move |_| MockResult::Return(Some(hd_key)));
    let coin = MmCoinEnum::Test(TestCoin::default());
    let (pair, saved) = start_htlc_key_pair(&coin, persistent.public(), &[1u8; 16]).unwrap();
    assert_eq!(pair.unwrap().public(), hd_key.public());
    assert_eq!(saved.unwrap().priv_key(), hd_key.private().secret.take());
}

/// T23 (R43), saved by the previous build without a pair: the swap resumes with the node's persistent key,
/// which is the key its Started event recorded, and the refund of its own payment is signed with it.
#[test]
fn t23_a_swap_saved_without_a_pair_resumes_with_the_persistent_key() {
    let persistent = persistent_pair();
    let coin = mock_coin_signing_with(Some(coin_pair()));
    // The previous build wrote neither the negotiated key nor a pair; it negotiated the persistent key.
    let swap = restore(saved_swap(None, None), coin);
    assert_eq!(swap.r().my_maker_coin_htlc_keypair.public(), persistent.public());
    assert_eq!(swap.r().my_maker_coin_htlc_pub.to_vec(), persistent.public().to_vec());

    // It also resumes when the Started event recorded the persistent key as the negotiated key.
    let coin = mock_coin_signing_with(Some(coin_pair()));
    let swap = restore(saved_swap(Some(persistent.public()), None), coin);
    assert_eq!(swap.r().my_maker_coin_htlc_keypair.public(), persistent.public());

    mock_payment_of(persistent.public().to_vec());
    let recovered = block_on(swap.recover_funds()).unwrap();
    assert_eq!(recovered.action, RecoveredSwapAction::RefundedMyPayment);
    assert_eq!(searched_with(), persistent.public().to_vec());
    assert_eq!(refund_privkey(), persistent.private().secret.to_vec());
}

/// T23 (R43), saved with a pair: the swap resumes with the saved pair, whatever the coin's key is now, and the
/// refund of its own payment is signed with it. A later change of the coin's activation does not change it.
#[test]
fn t23_a_swap_saved_with_a_pair_resumes_and_recovers_with_that_pair() {
    let saved_pair = coin_pair();
    // The coin is now activated with another key; the swap keeps the pair it was saved with.
    let coin = mock_coin_signing_with(Some(KeyPair::random_compressed()));
    let swap = restore(saved_swap(Some(saved_pair.public()), Some(saved_pair)), coin);
    assert_eq!(swap.r().my_maker_coin_htlc_keypair.public(), saved_pair.public());
    assert_eq!(swap.r().my_maker_coin_htlc_pub.to_vec(), saved_pair.public().to_vec());

    mock_payment_of(saved_pair.public().to_vec());
    let recovered = block_on(swap.recover_funds()).unwrap();
    assert_eq!(recovered.action, RecoveredSwapAction::RefundedMyPayment);
    assert_eq!(searched_with(), saved_pair.public().to_vec());
    assert_eq!(refund_privkey(), saved_pair.private().secret.to_vec());
}
