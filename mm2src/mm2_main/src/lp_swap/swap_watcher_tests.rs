// SPDX-License-Identifier: GPL-2.0-or-later
use super::*;
use coins::{CanRefundHtlc, FoundSwapTxSpend, MarketCoinOps, MmCoin, SwapOps, TestCoin, TransactionEnum, WatcherOps};
use common::block_on;
use mm2_core::mm_ctx::MmCtxBuilder;
use mocktopus::mocking::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn observed_transaction(marker: u8) -> TransactionEnum {
    chain::Transaction {
        version: 2,
        inputs: vec![chain::TransactionInput {
            previous_output: chain::OutPoint {
                hash: [marker; 32].into(),
                index: 0,
            },
            script_sig: vec![1, marker].into(),
            sequence: u32::MAX,
            script_witness: vec![],
        }],
        outputs: vec![chain::TransactionOutput {
            value: 100_000,
            script_pubkey: vec![marker].into(),
        }],
        ..chain::Transaction::default()
    }
    .into()
}

fn context(netid: u16, taker: &str, maker: &str) -> WatcherStateMachineCtx {
    let mut terms = swap_watcher_v2::fixture(netid);
    terms.data.taker_coin = taker.into();
    terms.data.maker_coin = maker.into();
    terms.data.swap_started_at = now_ms() / 1000 - 2 * terms.data.lock_duration;
    terms.maker_payment_locktime = (terms.data.swap_started_at + 2 * terms.data.lock_duration) as u32;
    terms.validate_shape(netid).unwrap();
    WatcherStateMachineCtx {
        ctx: MmCtxBuilder::default()
            .with_conf(serde_json::json!({"netid": netid}))
            .into_mm_arc(),
        data: terms.data.clone(),
        conf: WatcherConf::default(),
        taker_coin: TestCoin::new(taker).into(),
        maker_coin: TestCoin::new(maker).into(),
        verified_pub: terms.taker_refund_pub.clone(),
        taker_payment: Some(terms.taker_payment.clone()),
        v2: Some(terms),
    }
}

fn mock_secret_and_claim(ctx: &WatcherStateMachineCtx) {
    let hash = ctx.data.secret_hash.clone();
    TestCoin::extract_secret.mock_safe(move |_, secret_hash, _| {
        assert_eq!(secret_hash, hash);
        MockResult::Return(Ok(vec![33; 32]))
    });
    let template = ctx.data.maker_payment_spend_preimage.clone();
    TestCoin::complete_watcher_claim.mock_safe(move |_, preimage, secret, _| {
        assert_eq!(preimage, template);
        assert_eq!(secret, [33; 32]);
        MockResult::Return(Ok(vec![44; 150]))
    });
}

#[test]
fn watcher_follows_a_claim_even_after_the_refund_deadline_in_both_pair_directions() {
    for netid in [8762, 6133] {
        for (taker, maker) in [("ARRR", "LTC"), ("LTC", "ARRR"), ("ARRR", "vARRR"), ("vARRR", "ARRR")] {
            let ctx = context(netid, taker, maker);
            mock_secret_and_claim(&ctx);
            let terms = ctx.v2.clone().unwrap();
            TestCoin::watcher_search_for_swap_tx_spend.mock_safe(move |coin, lock, claim, refund, hash, raw, _| {
                assert_eq!(hash, terms.data.secret_hash);
                if coin.ticker() == terms.data.taker_coin {
                    assert_eq!(raw, terms.taker_payment);
                    assert_eq!(claim, terms.taker_claim_pub);
                    assert_eq!(refund, terms.taker_refund_pub);
                    assert_eq!(lock, terms.taker_locktime().unwrap());
                } else {
                    assert_eq!(raw, terms.maker_payment);
                    assert_eq!(claim, terms.maker_claim_pub);
                    assert_eq!(refund, terms.maker_refund_pub);
                    assert_eq!(lock, terms.maker_locktime().unwrap());
                }
                MockResult::Return(Box::pin(async {
                    Ok(Some(FoundSwapTxSpend::Spent(observed_transaction(1))))
                }))
            });
            TestCoin::wait_for_confirmations.mock_safe(|_, _, confirmations, _, _, _| {
                assert!(confirmations >= 1);
                MockResult::Return(Box::new(futures01::future::ok(())))
            });
            TestCoin::send_raw_tx_bytes
                .mock_safe(|_, _| panic!("an already confirmed spend must not be broadcast again"));
            let result = block_on(StateMachine::from_ctx(ctx).run(WaitForTakerPaymentSpend));
            assert!(matches!(result, WatcherResult::MakerPaymentSpent));
        }
    }
}

#[test]
fn watcher_switches_from_refund_to_claim_when_the_maker_spends_during_the_refund_wait() {
    let ctx = context(8762, "ARRR", "LTC");
    mock_secret_and_claim(&ctx);
    let reads = Arc::new(AtomicUsize::new(0));
    let count = reads.clone();
    TestCoin::watcher_search_for_swap_tx_spend.mock_safe(move |coin, _, _, _, _, _, _| {
        let found = if coin.ticker() == "ARRR" && count.fetch_add(1, Ordering::SeqCst) == 0 {
            None
        } else {
            Some(FoundSwapTxSpend::Spent(observed_transaction(2)))
        };
        MockResult::Return(Box::pin(async move { Ok(found) }))
    });
    TestCoin::can_refund_htlc
        .mock_safe(|_, _| MockResult::Return(Box::new(futures01::future::ok(CanRefundHtlc::HaveToWait(10)))));
    TestCoin::send_raw_tx_bytes.mock_safe(|_, _| panic!("the refund gate is closed; the later spend must be followed"));
    TestCoin::wait_for_confirmations
        .mock_safe(|_, _, _, _, _, _| MockResult::Return(Box::new(futures01::future::ok(()))));
    let result = block_on(StateMachine::from_ctx(ctx).run(RefundTakerPayment));
    assert!(matches!(result, WatcherResult::MakerPaymentSpent));
    assert_eq!(reads.load(Ordering::SeqCst), 2);
}

#[test]
fn watcher_retries_the_same_signed_claim_after_a_lost_acknowledgement_and_a_reorg() {
    let ctx = context(6133, "LTC", "ARRR");
    mock_secret_and_claim(&ctx);
    let reads = Arc::new(AtomicUsize::new(0));
    let count = reads.clone();
    TestCoin::watcher_search_for_swap_tx_spend.mock_safe(move |_, _, _, _, _, _, _| {
        let read = count.fetch_add(1, Ordering::SeqCst);
        assert!(read < 5, "the watcher failed to finish after the confirmed spend");
        let found = if read.is_multiple_of(2) {
            None
        } else {
            Some(FoundSwapTxSpend::Spent(observed_transaction(3)))
        };
        MockResult::Return(Box::pin(async move { Ok(found) }))
    });
    let sends = Arc::new(AtomicUsize::new(0));
    let count = sends.clone();
    TestCoin::send_raw_tx_bytes.mock_safe(move |_, bytes| {
        assert_eq!(bytes, [44; 150]);
        count.fetch_add(1, Ordering::SeqCst);
        MockResult::Return(Box::new(futures01::future::err("broadcast response was lost".into())))
    });
    let confirmations = Arc::new(AtomicUsize::new(0));
    let count = confirmations.clone();
    TestCoin::wait_for_confirmations.mock_safe(move |_, _, _, _, _, _| {
        let result = if count.fetch_add(1, Ordering::SeqCst) == 0 {
            Err("transaction left the active chain".into())
        } else {
            Ok(())
        };
        MockResult::Return(Box::new(futures01::future::result(result)))
    });
    let result = block_on(StateMachine::from_ctx(ctx).run(SpendMakerPayment { secret: vec![33; 32] }));
    assert!(matches!(result, WatcherResult::MakerPaymentSpent));
    assert_eq!(sends.load(Ordering::SeqCst), 2);
    assert_eq!(confirmations.load(Ordering::SeqCst), 2);
}

#[test]
fn watcher_preserves_spv_and_the_operators_confirmation_policy() {
    let ctx = context(8762, "LTC", "ARRR");
    TestCoin::required_confirmations.mock_safe(|_| MockResult::Return(6));
    TestCoin::requires_notarization.mock_safe(|_| MockResult::Return(true));
    let checks = Arc::new(AtomicUsize::new(0));
    let count = checks.clone();
    TestCoin::validate_watcher_payment.mock_safe(move |_, input| {
        assert_eq!(
            input.confirmations, 6,
            "SPV must receive the actual nonzero confirmation policy"
        );
        count.fetch_add(1, Ordering::SeqCst);
        MockResult::Return(Box::pin(async { Ok(()) }))
    });
    let waits = Arc::new(AtomicUsize::new(0));
    let count = waits.clone();
    TestCoin::wait_for_confirmations.mock_safe(move |_, _, confirmations, nota, _, _| {
        assert_eq!(confirmations, 6);
        assert!(nota);
        count.fetch_add(1, Ordering::SeqCst);
        MockResult::Return(Box::new(futures01::future::ok(())))
    });
    block_on(swap_watcher_v2::validate_payments(&ctx, ctx.v2.as_ref().unwrap())).unwrap();
    assert_eq!(checks.load(Ordering::SeqCst), 2);
    assert_eq!(waits.load(Ordering::SeqCst), 2);
}

#[test]
fn watcher_keeps_the_job_past_a_short_rpc_wait_while_the_payment_is_being_confirmed() {
    let mut ctx = context(8762, "LTC", "ARRR");
    let terms = ctx.v2.as_mut().unwrap();
    terms.data.swap_started_at = now_ms() / 1000;
    terms.maker_payment_locktime = (terms.data.swap_started_at + 2 * terms.data.lock_duration) as u32;
    ctx.data = terms.data.clone();
    ctx.conf.wait_taker_payment = 0.0;
    mock_secret_and_claim(&ctx);
    let attempts = Arc::new(AtomicUsize::new(0));
    let count = attempts.clone();
    TestCoin::validate_watcher_payment.mock_safe(move |_, _| {
        let result = if count.fetch_add(1, Ordering::SeqCst) == 0 {
            Err("the funding payment is not confirmed yet".into())
        } else {
            Ok(())
        };
        MockResult::Return(Box::pin(async move { result }))
    });
    TestCoin::watcher_search_for_swap_tx_spend.mock_safe(|_, _, _, _, _, _, _| {
        MockResult::Return(Box::pin(async {
            Ok(Some(FoundSwapTxSpend::Spent(observed_transaction(1))))
        }))
    });
    TestCoin::wait_for_confirmations
        .mock_safe(|_, _, _, _, _, _| MockResult::Return(Box::new(futures01::future::ok(()))));
    TestCoin::send_raw_tx_bytes.mock_safe(|_, _| panic!("the observed spend is already confirmed"));
    let result = block_on(StateMachine::from_ctx(ctx).run(ValidateTakerPayment));
    assert!(matches!(result, WatcherResult::MakerPaymentSpent));
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
}

#[test]
fn watcher_message_admission_defaults_to_off_and_checks_the_extension_switch() {
    let terms = swap_watcher_v2::fixture(8762);
    let message = SwapWatcherMsg::TakerSwapWatcherMsgV2(terms.into());
    let encoded = mm2_p2p::encode_and_sign(&message, &[1; 32]).unwrap();
    for config in [
        serde_json::json!({"netid": 8762, "watcher_htlc_v2": true}),
        serde_json::json!({"netid": 8762, "is_watcher": true}),
    ] {
        let ctx = MmCtxBuilder::default().with_conf(config).into_mm_arc();
        block_on(process_watcher_msg(ctx.clone(), &encoded));
        assert!(
            ctx.swaps_ctx.lock().unwrap().is_none(),
            "a disabled watcher must not start a job"
        );
    }
}

#[test]
fn watcher_admission_returns_promptly_and_shutdown_releases_the_job() {
    let ctx = MmCtxBuilder::default()
        .with_conf(serde_json::json!({"netid": 8762, "is_watcher": true, "watcher_htlc_v2": true}))
        .into_mm_arc();
    let coins = coins::CoinsContext::from_ctx(&ctx).unwrap();
    TestCoin::is_supported_by_watchers.mock_safe(|_| MockResult::Return(true));
    TestCoin::supports_watcher_v2.mock_safe(|_| MockResult::Return(true));
    block_on(coins.add_coin(TestCoin::new("ARRR").into())).unwrap();
    block_on(coins.add_coin(TestCoin::new("LTC").into())).unwrap();
    let message = SwapWatcherMsg::TakerSwapWatcherMsgV2(swap_watcher_v2::fixture(8762).into());
    let encoded = mm2_p2p::encode_and_sign(&message, &[1; 32]).unwrap();
    // These intentionally unparseable transaction fixtures keep the background
    // job in fee validation. No chain endpoint or trader funds are involved.
    block_on(async {
        let admit = process_watcher_msg(ctx.clone(), &encoded).fuse();
        let timeout = Timer::sleep(2.0).fuse();
        futures::pin_mut!(admit, timeout);
        futures::select! { _ = admit => (), _ = timeout => panic!("gossip processing waited for the watcher job") }
    });
    let swaps = SwapsContext::from_ctx(&ctx).unwrap();
    assert_eq!(swaps.taker_swap_watchers.lock().len(), 1);
    block_on(process_watcher_msg(ctx.clone(), &encoded));
    assert_eq!(
        swaps.taker_swap_watchers.lock().len(),
        1,
        "repeated gossip must keep one job"
    );
    ctx.stop().unwrap();
    block_on(async {
        let finished = async {
            while !swaps.taker_swap_watchers.lock().is_empty() {
                Timer::sleep(0.01).await;
            }
        }
        .fuse();
        let timeout = Timer::sleep(2.0).fuse();
        futures::pin_mut!(finished, timeout);
        futures::select! { _ = finished => (), _ = timeout => panic!("shutdown did not cancel the watcher job") }
    });
}

#[test]
fn watcher_validates_the_exact_network_fee_descriptor_and_transaction_ids() {
    use common::mm_number::MmNumber;
    for netid in [8762, 6133] {
        let mut ctx = context(netid, "LTC", "ARRR");
        let terms = ctx.v2.as_mut().unwrap();
        terms.taker_amount = 777.into();
        let payment = observed_transaction(5);
        let maker = observed_transaction(6);
        let fee = observed_transaction(7);
        terms.taker_payment = payment.tx_hex();
        terms.maker_payment = maker.tx_hex();
        terms.taker_fee = fee.tx_hex();
        terms.data.taker_payment_hash = payment.tx_hash().0;
        terms.data.maker_payment_hash = maker.tx_hash().0;
        terms.data.taker_fee_hash = fee.tx_hash().0;
        ctx.data = terms.data.clone();
        TestCoin::tx_enum_from_bytes.mock_safe(|_, bytes| {
            MockResult::Return(
                serialization::deserialize::<_, chain::Transaction>(bytes)
                    .map(TransactionEnum::UtxoTx)
                    .map_err(|error| error.to_string()),
            )
        });
        TestCoin::min_tx_amount.mock_safe(|_| MockResult::Return("0.00001".parse().unwrap()));
        TestCoin::should_burn_dex_fee.mock_safe(|_| MockResult::Return(true));
        let count = Arc::new(AtomicUsize::new(0));
        let calls = count.clone();
        let terms = ctx.v2.as_ref().unwrap().clone();
        let expected = terms.clone();
        TestCoin::validate_fee.mock_safe(move |_, args| {
            assert_eq!(args.expected_sender, expected.taker_refund_pub);
            assert_eq!(
                args.fee_addr,
                mm2_net_config::net_config_or_panic(netid).dex_fee_addr_raw_pubkey()
            );
            assert_eq!(args.uuid, expected.data.uuid.as_bytes());
            assert_eq!(args.min_block_number, expected.data.taker_coin_start_block);
            assert_eq!(
                args.dex_fee.total_spend_amount(),
                MmNumber::from(if netid == 8762 { "1" } else { "15.54" })
            );
            assert_eq!(
                args.dex_fee.burn_amount(),
                MmNumber::from(if netid == 8762 { "0.25" } else { "0" })
            );
            calls.fetch_add(1, Ordering::SeqCst);
            MockResult::Return(Box::new(futures01::future::ok(())))
        });
        block_on(swap_watcher_v2::validate_fee(&ctx, &terms)).unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 1);
        let mut changed = terms;
        changed.data.taker_payment_hash[0] ^= 1;
        assert!(block_on(swap_watcher_v2::validate_fee(&ctx, &changed)).is_err());
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "a wrong transaction id must fail before fee validation"
        );
    }
}

#[test]
fn watcher_registry_respects_the_extension_opt_in_for_every_registration_path() {
    let ctx = context(8762, "LTC", "ARRR").ctx;
    let coins = coins::CoinsContext::from_ctx(&ctx).unwrap();
    TestCoin::is_supported_by_watchers.mock_safe(|_| MockResult::Return(true));
    TestCoin::supports_watcher_v2.mock_safe(|coin| MockResult::Return(coin.ticker() != "SC"));
    block_on(coins.add_coin(TestCoin::new("ARRR").into())).unwrap();
    block_on(coins::lp_register_coin(
        &ctx,
        TestCoin::new("LTC").into(),
        coins::RegisterCoinParams {
            ticker: "LTC".into(),
            tx_history: false,
        },
    ))
    .unwrap();
    assert!(block_on(coins.watcher_supported_tickers(false)).is_empty());
    let mut enabled = block_on(coins.watcher_supported_tickers(true));
    enabled.sort();
    assert_eq!(enabled, ["ARRR", "LTC"]);
    block_on(coins.add_coin(TestCoin::new("SC").into())).unwrap();
    assert_eq!(block_on(coins.watcher_supported_tickers(false)), ["SC"]);
}

#[test]
fn watcher_refuses_legacy_context_without_inventing_a_payment_amount() {
    let mut ctx = context(8762, "ARRR", "LTC");
    ctx.v2 = None;
    TestCoin::send_raw_tx_bytes.mock_safe(|_, _| panic!("an incomplete delegation cannot authorize a broadcast"));
    let result = block_on(StateMachine::from_ctx(ctx).run(ValidateTakerPayment));
    assert!(matches!(result, WatcherResult::StoppedOnError(message) if message.contains("lacks the amount")));
}

#[test]
fn watcher_bounds_the_number_of_jobs_started_from_untrusted_gossip() {
    let ctx = context(8762, "ARRR", "LTC");
    let swaps = SwapsContext::from_ctx(&ctx.ctx).unwrap();
    let mut leases = Vec::new();
    for index in 0..MAX_CONCURRENT_WATCHERS {
        let mut key = vec![0; 32];
        key[..8].copy_from_slice(&(index as u64).to_le_bytes());
        leases.push(SwapWatcherLock::try_lock(swaps.clone(), key).unwrap());
    }
    assert!(SwapWatcherLock::try_lock(swaps.clone(), vec![255; 32]).is_none());
    drop(leases);
    assert!(swaps.taker_swap_watchers.lock().is_empty());
    assert!(SwapWatcherLock::try_lock(swaps.clone(), vec![255; 32]).is_some());
}

#[test]
fn watcher_lock_cleanup_does_not_remove_a_new_owners_lease() {
    let ctx = context(8762, "ARRR", "LTC");
    let swaps = SwapsContext::from_ctx(&ctx.ctx).unwrap();
    let key = vec![8; 32];
    let mut first = SwapWatcherLock::try_lock(swaps.clone(), key.clone()).unwrap();
    assert!(SwapWatcherLock::try_lock(swaps.clone(), key.clone()).is_none());
    swaps.taker_swap_watchers.lock().insert(key.clone(), first.expires + 1);
    assert!(!first.renew());
    drop(first);
    assert!(swaps.taker_swap_watchers.lock().contains_key(&key));
    swaps.taker_swap_watchers.lock().remove(&key);
    let second = SwapWatcherLock::try_lock(swaps.clone(), key.clone()).unwrap();
    drop(second);
    assert!(!swaps.taker_swap_watchers.lock().contains_key(&key));
}
