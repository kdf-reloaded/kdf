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
            // The swap was started by a node whose key is `persistent_pair` (CRD ch.05 R47).
            data.my_persistent_pub = H264::from(&**persistent_pair().public()).into();
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

    let (maker_pub, taker_pub) = derive_htlc_pubkeys(persistent.public(), &coin, &pair, &coin, &pair).unwrap();
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

    let (maker_pub, taker_pub) = derive_htlc_pubkeys(persistent.public(), &coin, &pair, &coin, &pair).unwrap();
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

// ---------------------------------------------------------------------------------------------------------
// CRD ch.05 §5.12D (R45, R47; T27, T28): the swap's own node key under a global-HD login.
// ---------------------------------------------------------------------------------------------------------

/// The passphrase-hash key of an HD login: the node key of earlier builds, now only the legacy key.
fn legacy_pair() -> KeyPair { key_pair_from_seed("the hash of the passphrase text of an HD login").unwrap() }

/// A global-HD login: the internal key (`persistent_pair`) is the node key, and `legacy_pair` the legacy key.
fn hd_ctx() -> MmArc {
    MmCtxBuilder::default()
        .with_conf(json!({"netid": 8762}))
        .with_secp256k1_key_pair(persistent_pair())
        .with_legacy_passphrase_key_pair(legacy_pair())
        .into_mm_arc()
}

/// The saved swap of `saved_swap(None, None)`, whose Started event recorded `recorded` as the persistent key.
fn saved_recording(recorded: &[u8]) -> MakerSavedSwap {
    let mut saved = saved_swap(None, None);
    match saved.events[0].event {
        MakerSwapEvent::Started(ref mut data) => data.my_persistent_pub = H264::from(recorded).into(),
        _ => unreachable!(),
    }
    saved
}

/// T27 (R47): a swap saved by the previous build under a global-HD login, with the legacy key recorded and no
/// saved pair, resumes with the legacy key: it is the swap's node key (which signs the swap messages), the HTLC
/// key of a coin that saved no pair, and the key the refund is signed with.
#[test]
fn t27_a_swap_that_recorded_the_legacy_key_resumes_and_refunds_with_it() {
    let legacy = legacy_pair();
    let coin = mock_coin_signing_with(Some(coin_pair()));
    let (swap, _) = MakerSwap::load_from_saved(hd_ctx(), coin.clone(), coin, saved_recording(legacy.public())).unwrap();
    assert_eq!(swap.node_key_pair.public(), legacy.public());
    assert_eq!(swap.my_persistent_pub.to_vec(), legacy.public().to_vec());
    assert_eq!(swap.r().my_maker_coin_htlc_keypair.public(), legacy.public());
    assert_eq!(swap.r().my_maker_coin_htlc_pub.to_vec(), legacy.public().to_vec());

    mock_payment_of(legacy.public().to_vec());
    let recovered = block_on(swap.recover_funds()).unwrap();
    assert_eq!(recovered.action, RecoveredSwapAction::RefundedMyPayment);
    assert_eq!(searched_with(), legacy.public().to_vec());
    assert_eq!(refund_privkey(), legacy.private().secret.to_vec());
}

/// T27 (R47): a swap that recorded the node key (the internal key) resumes with it, and a swap started now runs
/// with it: its persistent key and its default HTLC key are the node key.
#[test]
fn t27_a_swap_that_recorded_the_internal_key_and_a_new_swap_use_it() {
    let internal = persistent_pair();
    let coin = mock_coin_signing_with(None);
    let (swap, _) =
        MakerSwap::load_from_saved(hd_ctx(), coin.clone(), coin, saved_recording(internal.public())).unwrap();
    assert_eq!(swap.node_key_pair.public(), internal.public());
    assert_eq!(swap.r().my_maker_coin_htlc_keypair.public(), internal.public());

    let ctx = hd_ctx();
    let new_swap = MakerSwap::new(
        ctx.clone(),
        bits256::from([1u8; 32]),
        1.into(),
        1.into(),
        *ctx.secp256k1_key_pair(),
        Uuid::new_v4(),
        None,
        SwapConfirmationsSettings {
            maker_coin_confs: 1,
            maker_coin_nota: false,
            taker_coin_confs: 1,
            taker_coin_nota: false,
        },
        mock_coin_signing_with(None),
        mock_coin_signing_with(None),
        7800,
        None,
    );
    assert_eq!(new_swap.my_persistent_pub.to_vec(), internal.public().to_vec());
    assert_eq!(new_swap.r().my_taker_coin_htlc_keypair.public(), internal.public());
    assert_ne!(new_swap.my_persistent_pub.to_vec(), legacy_pair().public().to_vec());
}

/// T27 (R47): a recorded key that matches neither key is an error naming the swap, and the swap is not resumed.
#[test]
fn t27_a_recorded_key_that_matches_neither_key_is_reported() {
    let coin = mock_coin_signing_with(None);
    let other = coin_pair();
    let saved = saved_recording(other.public());
    let uuid = saved.uuid;
    let err = match MakerSwap::load_from_saved(hd_ctx(), coin.clone(), coin, saved) {
        Ok(_) => panic!("a swap that recorded an unknown key must not resume"),
        Err(e) => e,
    };
    assert!(err.contains(&uuid.to_string()), "{err}");
    assert!(err.contains(&hex::encode(other.public().to_vec())), "{err}");
    assert!(err.contains("not resumed"), "{err}");
}

/// T28 (R47): recovery lists the legacy key among its candidates: a payment of a swap that saved no pair, built
/// with the legacy key, is recovered with it even though the swap's saved key and the node key are others.
#[test]
fn t28_recovery_recovers_a_payment_built_with_the_legacy_key() {
    let legacy = legacy_pair();
    let coin = mock_coin_signing_with(None);
    mock_payment_of(legacy.public().to_vec());
    let ctx = hd_ctx();
    let stages = RecoveryStages::default();
    let key = block_on(resolve_recovery_htlc_key(RecoveryKeyRequest {
        ctx: &ctx,
        coin: &coin,
        owner: PaymentOwner::Mine,
        per_swap_pubkey: H264::from(coin_pair().public().to_vec().as_slice()),
        derived_keypair: None,
        saved_keypair: None,
        other_pubkey: &[2u8; 33],
        time_lock: 1,
        secret_hash: &[7u8; 20],
        payment_tx: &[1u8; 4],
        stages: &stages,
    }))
    .unwrap();
    assert_eq!(key.kind, super::super::RecoveryHtlcKeyKind::LegacyPassphrase);
    assert_eq!(key.keypair.public(), legacy.public());

    // The node key is still a candidate, and is tried before the legacy key.
    mock_payment_of(persistent_pair().public().to_vec());
    let key = block_on(resolve_recovery_htlc_key(RecoveryKeyRequest {
        ctx: &ctx,
        coin: &coin,
        owner: PaymentOwner::Mine,
        per_swap_pubkey: H264::from(coin_pair().public().to_vec().as_slice()),
        derived_keypair: None,
        saved_keypair: None,
        other_pubkey: &[2u8; 33],
        time_lock: 1,
        secret_hash: &[7u8; 20],
        payment_tx: &[1u8; 4],
        stages: &stages,
    }))
    .unwrap();
    assert_eq!(key.kind, super::super::RecoveryHtlcKeyKind::Persistent);
    assert_eq!(key.keypair.public(), persistent_pair().public());
}

/// T27 (R47) and T30: the swap messages are signed with the swap's node key when it has no per-swap key, and
/// with the per-swap key otherwise; the key a swap that recorded the legacy key signs with is the legacy key.
#[test]
fn t27_swap_messages_are_signed_with_the_swaps_node_key() {
    use super::super::swap_msg::signing_identity;
    use crate::mm2::lp_network::Libp2pPeerId;
    let legacy = legacy_pair();
    let internal = persistent_pair();
    let (secret, from) = signing_identity(&None, &legacy);
    assert_eq!(secret, legacy.private_bytes());
    assert!(from.is_none());
    let (secret, _) = signing_identity(&None, &internal);
    assert_eq!(secret, internal.private_bytes());
    let per_swap = coin_pair();
    let (secret, from) = signing_identity(&Some(per_swap), &legacy);
    assert_eq!(secret, per_swap.private_bytes());
    assert_eq!(from, Some(per_swap.libp2p_peer_id()));

    let coin = mock_coin_signing_with(None);
    let (swap, _) = MakerSwap::load_from_saved(hd_ctx(), coin.clone(), coin, saved_recording(legacy.public())).unwrap();
    assert_eq!(
        signing_identity(&swap.p2p_privkey, &swap.node_key_pair).0,
        legacy.private_bytes()
    );
}

/// T30: under an iguana login the legacy key is the node key, and a swap resumes exactly as before.
#[test]
fn t30_under_an_iguana_login_the_legacy_key_is_the_node_key() {
    let ctx = ctx_of(persistent_pair());
    assert_eq!(
        ctx.legacy_passphrase_key_pair().public(),
        ctx.secp256k1_key_pair().public()
    );
    let coin = mock_coin_signing_with(None);
    let (swap, _) = MakerSwap::load_from_saved(ctx, coin.clone(), coin, saved_swap(None, None)).unwrap();
    assert_eq!(swap.node_key_pair.public(), persistent_pair().public());
}
