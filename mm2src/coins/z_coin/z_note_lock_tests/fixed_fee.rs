//! A ZCoin pays one fixed fee per transaction, and the figures reported before a trade equal what the
//! transactions then pay (CRD ch.39 R39.6.5f, ch.08). The harness is the one of the parent module; the
//! build tests make real Sapling proofs, so they need the Sapling parameter files and say so where those
//! are absent.

use super::history_fee::paid_fee;
use super::*;
use crate::z_coin::z_coin_ops::ZCOIN_DEFAULT_TX_FEE_SAT;
use crate::z_coin::z_htlc::{z_p2sh_spend, z_send_htlc};
use crate::{FeeApproxStage, MarketCoinOps, TradePreimageValue};
use chain::constants::SEQUENCE_FINAL;
use crypto::privkey::key_pair_from_secret;
use kdf_crypto::dhash160;
use script::{Builder as ScriptBuilder, Opcode};

const TIME_LOCK: u32 = 1_800_000_000;
const SECRET: [u8; 32] = [7; 32];

fn sat(amount: &MmNumber) -> u64 {
    let amount = amount.to_decimal() * BigDecimal::from(100_000_000u64);
    amount
        .with_scale(0)
        .to_string()
        .parse()
        .expect("a whole number of base units")
}

/// The fee paid by a spend or a refund of an HTLC of `htlc_sat`: the HTLC value less what the shielded
/// output takes.
fn spend_fee(tx: &ZTransaction, htlc_sat: u64) -> u64 {
    let balance = i64::from(*tx.sapling_bundle().expect("a Sapling output").value_balance());
    u64::try_from(i64::try_from(htlc_sat).unwrap() + balance).expect("a fee is not negative")
}

/// The sender fee, the taker-fee fee, the receiver fee and the safe amount of a coin whose fee is the
/// shared ZCoin default all report the same 10,000, and the safe amount adds the dust.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_reported_figures_equal_the_one_fixed_fee() {
    let Some(fixture) = LightCoinFixture::with_notes_under(1, 2, test_params_with_zip212(), ZCOIN_DEFAULT_TX_FEE_SAT)
    else {
        return;
    };
    let coin = &fixture.coin;
    let sender = coin
        .get_sender_trade_fee(TradePreimageValue::Exact(1.into()), FeeApproxStage::TradePreimage)
        .await
        .unwrap();
    let taker = coin
        .get_fee_to_send_taker_fee(BigDecimal::from(1), FeeApproxStage::TradePreimage)
        .await
        .unwrap();
    let receiver = coin
        .get_receiver_trade_fee(FeeApproxStage::TradePreimage)
        .compat()
        .await
        .unwrap();
    assert_eq!(sat(&sender.amount), 10_000);
    assert_eq!(sat(&taker.amount), 10_000);
    assert_eq!(sat(&receiver.amount), 10_000);
    assert!(receiver.paid_from_trading_vol);
    let safe = coin.spend_safe_amount().await.unwrap().expect("a safe amount");
    assert_eq!(sat(&safe), 10_000 + coin.as_ref().dust_amount);
}

/// A configured `txfee` wins: every figure follows it, the receiver fee and the safe amount included
/// (not the 496-byte size rule).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_configured_fee_is_the_one_every_figure_follows() {
    let Some(fixture) = LightCoinFixture::with_notes_under(1, 2, test_params_with_zip212(), 25_000) else {
        return;
    };
    let coin = &fixture.coin;
    let receiver = coin
        .get_receiver_trade_fee(FeeApproxStage::TradePreimage)
        .compat()
        .await
        .unwrap();
    assert_eq!(sat(&receiver.amount), 25_000);
    let safe = coin.spend_safe_amount().await.unwrap().expect("a safe amount");
    assert_eq!(sat(&safe), 25_000 + coin.as_ref().dust_amount);
    let sender = coin
        .get_sender_trade_fee(TradePreimageValue::Exact(1.into()), FeeApproxStage::TradePreimage)
        .await
        .unwrap();
    assert_eq!(sat(&sender.amount), 25_000);
}

/// A payment, a spend and a refund each pay the coin's fixed fee: the spend and the refund no longer pay
/// the former constant 1,000.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_payment_the_spend_and_the_refund_pay_the_fixed_fee() {
    for fee in [ZCOIN_DEFAULT_TX_FEE_SAT, 25_000] {
        let Some(fixture) = LightCoinFixture::with_notes_under(1, 2, test_params_with_zip212(), fee) else {
            return;
        };
        let mine = key_pair_from_secret(&[21; 32]).unwrap();
        let other = key_pair_from_secret(&[22; 32]).unwrap();
        let payment = z_send_htlc(
            &fixture.coin,
            TIME_LOCK,
            mine.public(),
            other.public(),
            dhash160(&SECRET).as_slice(),
            "0.001".parse().unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(paid_fee(&payment), fee, "the payment");
        let redeem = crate::utxo::utxo_common::payment_script(
            TIME_LOCK,
            dhash160(&SECRET).as_slice(),
            mine.public(),
            other.public(),
        );
        let spend = z_p2sh_spend(
            &fixture.coin,
            payment.clone(),
            TIME_LOCK,
            SEQUENCE_FINAL,
            redeem.clone(),
            ScriptBuilder::default()
                .push_data(&SECRET)
                .push_opcode(Opcode::OP_0)
                .into_script(),
            other.private().secret.as_slice(),
        )
        .await
        .unwrap();
        let refund = z_p2sh_spend(
            &fixture.coin,
            payment,
            TIME_LOCK,
            SEQUENCE_FINAL - 1,
            redeem,
            ScriptBuilder::default().push_opcode(Opcode::OP_1).into_script(),
            mine.private().secret.as_slice(),
        )
        .await
        .unwrap();
        assert_eq!(spend_fee(&spend, 100_000), fee, "the spend");
        assert_eq!(spend_fee(&refund, 100_000), fee, "the refund");
    }
}

/// The coin's own minimum trading volume and the default minimum volume of a new order built from it are
/// 0.001 ARRR (ten times the fixed fee), and a payment at that floor less the spend fee is positive.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_coin_floor_is_ten_times_the_fixed_fee() {
    for (fee, floor) in [(ZCOIN_DEFAULT_TX_FEE_SAT, 100_000u64), (25_000, 250_000)] {
        let Some(fixture) = LightCoinFixture::with_notes_under(1, 2, test_params_with_zip212(), fee) else {
            return;
        };
        let min = MarketCoinOps::min_trading_vol(&fixture.coin);
        assert_eq!(sat(&min), floor);
        assert!(floor - fee > 0);
    }
}
