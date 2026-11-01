//! CRD ch.39 R39.5.6-R39.5.11, taker side: the shielded leg's key is derived from the 16 raw uuid bytes, is
//! not saved, and is re-derived on restart and in recovery. Test-only keys of swept throwaway wallets (never
//! fund), the fixture shared with the `coins` crate's derivation vector test.

use super::*;
use coins::{HtlcPaymentCheck, MarketCoinOps, MmCoin, SwapOps, TestCoin};
use common::block_on;
use crypto::privkey::{key_pair_from_secret, key_pair_from_seed};
use mm2_core::mm_ctx::MmCtxBuilder;
use mocktopus::mocking::*;
use serde_json as json;
use std::cell::Cell;
use std::str::FromStr;

const VECTORS: &str = include_str!("../../../coins/z_coin/test-fixtures/arrr-htlc-key-derivation.json");
/// The saved swap of the taker tests, whose HTLC fields and uuid are replaced below.
const SAVED: &str = r#"{"error_events":[],"events":[{"event":{"data":{"lock_duration":7800,"maker":"1bb83b58ec130e28e0a6d5d2acf2eb01b0d3f1670e021d47d31db8a858219da8","maker_amount":"3.54932734","maker_coin":"ARRR","maker_coin_start_block":1452970,"maker_payment_confirmations":1,"maker_payment_wait":1563746537,"my_persistent_pub":"03101ace6b08605b9424b0582b5cce044b70a3c8d8d10cb2965e039b0967ae92b9","started_at":1563743937,"taker_amount":"0.02004833998671660000000000","taker_coin":"RIN","taker_coin_start_block":8196380,"taker_payment_confirmations":1,"taker_payment_lock":1563751737,"uuid":"3447b727-fe93-4357-8e5a-8cf2699b7e86"},"type":"Started"},"timestamp":1563743937741},{"event":{"data":{"maker_payment_locktime":1563759539,"maker_pubkey":"031bb83b58ec130e28e0a6d5d2acf2eb01b0d3f1670e021d47d31db8a858219da8","secret_hash":"70f9c95e9cfdf49e9f9b9afcda92cd2d40cbd49f"},"type":"Negotiated"},"timestamp":1563744003530},{"event":{"type":"Finished"},"timestamp":1563744118580}],"success_events":[],"uuid":"3447b727-fe93-4357-8e5a-8cf2699b7e86"}"#;

thread_local! {
    static BASE_SECRET: Cell<[u8; 32]> = const { Cell::new([0; 32]) };
}

fn vectors() -> json::Value { json::from_str(VECTORS).unwrap() }

fn vector_base_key(role: &str) -> KeyPair {
    let private = keys::Private::from_str(vectors()[role]["base_wif"].as_str().unwrap()).unwrap();
    KeyPair::from_private(private).unwrap()
}

fn vector_uuid() -> Uuid { Uuid::from_str(vectors()["uuid"].as_str().unwrap()).unwrap() }

fn vector_taker_arrr_pub() -> Vec<u8> { hex::decode(vectors()["taker"]["arrr_htlc_pub"].as_str().unwrap()).unwrap() }

fn restore(saved_pubkey: &[u8], privkey: Option<KeyPair>, base: &KeyPair) -> TakerSwap {
    let mut saved: TakerSavedSwap = json::from_str(SAVED).unwrap();
    match saved.events[0].event {
        TakerSwapEvent::Started(ref mut data) => {
            data.uuid = vector_uuid();
            data.maker_coin_htlc_pubkey = Some(H264::from(saved_pubkey).into());
            data.maker_coin_htlc_privkey = privkey.map(SerializableSecp256k1Keypair::from);
        },
        _ => unreachable!(),
    }
    BASE_SECRET.with(|b| b.set(base.private().secret.take()));
    TestCoin::ticker.mock_safe(|_| MockResult::Return("ARRR"));
    TestCoin::swap_contract_address.mock_safe(|_| MockResult::Return(None));
    TestCoin::derives_swap_htlc_key.mock_safe(|_| MockResult::Return(true));
    TestCoin::derive_swap_htlc_key_pair.mock_safe(|_, input| {
        let base = key_pair_from_secret(&BASE_SECRET.with(Cell::get)).unwrap();
        MockResult::Return(coins::z_coin::derive_swap_htlc_key_pair(&base, input))
    });
    let ctx = MmCtxBuilder::default()
        .with_conf(json!({"netid": 8762}))
        .with_secp256k1_key_pair(
            key_pair_from_seed("spice describe gravity federal blast come thank unfair canal monkey style afraid")
                .unwrap(),
        )
        .into_mm_arc();
    let (swap, _) = TakerSwap::load_from_saved(
        ctx,
        MmCoinEnum::Test(TestCoin::default()),
        MmCoinEnum::Test(TestCoin::default()),
        saved,
    )
    .unwrap();
    swap
}

/// T39.5.a/T39.5.b on the taker side: the key is derived from the 16 raw uuid bytes (not the hyphenated text),
/// equals the reference's key for the same swap, is not in the record, and is the key recovery signs with.
#[test]
fn taker_key_is_derived_from_the_uuid_rederived_on_restart_and_used_by_recovery() {
    let swap = restore(&vector_taker_arrr_pub(), None, &vector_base_key("taker"));
    assert_eq!(
        swap.r().my_maker_coin_htlc_keypair.public().to_vec(),
        vector_taker_arrr_pub()
    );
    swap.check_rederived_htlc_keys()
        .expect("the re-derived key is the negotiated key");
    assert!(swap.r().data.maker_coin_htlc_privkey.is_none());
    assert!(swap.r().data.taker_coin_htlc_privkey.is_none());

    static TRIED: std::sync::Mutex<Vec<Vec<u8>>> = std::sync::Mutex::new(Vec::new());
    TestCoin::check_payment_pays_htlc.mock_safe(|_, _, _, second, _, _| {
        // The maker sent this payment: the taker's key is the second (spend) key.
        TRIED.lock().unwrap().push(second.to_vec());
        MockResult::Return(Box::pin(futures::future::ready(Ok(HtlcPaymentCheck::Pays))))
    });
    let key = block_on(swap.recovery_maker_coin_key(&[0u8; 4], &RecoveryStages::default())).unwrap();
    assert_eq!(key.kind, super::super::RecoveryHtlcKeyKind::Derived);
    assert_eq!(key.keypair.public().to_vec(), vector_taker_arrr_pub());
    assert_eq!(*TRIED.lock().unwrap(), vec![vector_taker_arrr_pub()]);
}

/// The 16 raw bytes are the input: the same key is not derived from the uuid's text form, so a derivation that
/// fed the hyphenated string would not match the reference's key.
#[test]
fn the_uuid_text_form_does_not_derive_the_key() {
    let base = vector_base_key("taker");
    let from_text = coins::z_coin::derive_swap_htlc_key_pair(&base, vector_uuid().to_string().as_bytes()).unwrap();
    assert_ne!(from_text.public().to_vec(), vector_taker_arrr_pub());
    let from_bytes = coins::z_coin::derive_swap_htlc_key_pair(&base, vector_uuid().as_bytes()).unwrap();
    assert_eq!(from_bytes.public().to_vec(), vector_taker_arrr_pub());
}

/// Another base key (another seed, or the other key policy): the pre-check refuses to resume the swap, and a
/// swap of an earlier build keeps its saved private key (candidate 2) behind the re-derived key (candidate 1).
#[test]
fn another_base_key_is_not_resumed_and_a_saved_pair_is_kept() {
    let swap = restore(&vector_taker_arrr_pub(), None, &vector_base_key("maker"));
    let err = swap.check_rederived_htlc_keys().unwrap_err();
    assert!(err.contains("not resumed"), "{err}");

    let saved_pair = KeyPair::random_compressed();
    let swap = restore(saved_pair.public(), Some(saved_pair), &vector_base_key("taker"));
    assert_eq!(
        swap.r().my_maker_coin_htlc_keypair.private().secret,
        saved_pair.private().secret
    );
    swap.check_rederived_htlc_keys()
        .expect("a saved pair that owns the saved key resumes");
    static TRIED: std::sync::Mutex<Vec<Vec<u8>>> = std::sync::Mutex::new(Vec::new());
    let saved_pub = saved_pair.public().to_vec();
    static PAYS: std::sync::Mutex<Vec<u8>> = std::sync::Mutex::new(Vec::new());
    *PAYS.lock().unwrap() = saved_pub.clone();
    TestCoin::check_payment_pays_htlc.mock_safe(|_, _, _, second, _, _| {
        TRIED.lock().unwrap().push(second.to_vec());
        let check = if second == PAYS.lock().unwrap().as_slice() {
            HtlcPaymentCheck::Pays
        } else {
            HtlcPaymentCheck::PaysAnotherScript {
                payment_id: "abcd".to_owned(),
            }
        };
        MockResult::Return(Box::pin(futures::future::ready(Ok(check))))
    });
    let key = block_on(swap.recovery_maker_coin_key(&[0u8; 4], &RecoveryStages::default())).unwrap();
    assert_eq!(key.kind, super::super::RecoveryHtlcKeyKind::PerSwap);
    assert_eq!(key.keypair.private().secret, saved_pair.private().secret);
    assert_eq!(*TRIED.lock().unwrap(), vec![vector_taker_arrr_pub(), saved_pub]);
}
