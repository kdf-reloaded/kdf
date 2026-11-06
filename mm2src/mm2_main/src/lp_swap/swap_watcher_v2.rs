// SPDX-License-Identifier: GPL-2.0-or-later
//! Versioned watcher delegation with explicit per-chain HTLC keys.
//!
//! The legacy envelope has one maker key and no taker HTLC keys. Those fields
//! cannot describe a shielded coin's per-swap signing keys on both chains.

use super::dex_fee::compute_dex_fee_with_taker_pubkey;
use super::swap_watcher::{TakerSwapWatcherData, WatcherStateMachineCtx};
use coins::{DexFee, ValidateFeeArgs, WatcherPaymentValidation, WatcherSpendKind};
use common::mm_number::BigDecimal;
use futures::compat::Future01CompatExt;
use mm2_net_config::net_config_or_panic;
use serde::{Deserialize, Serialize};

/// Public terms and transactions signed by the taker before delegation.
/// The existing envelope remains unchanged for legacy watcher peers.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TakerSwapWatcherV2 {
    pub data: TakerSwapWatcherData,
    pub netid: u16,
    pub taker_amount: BigDecimal,
    pub maker_amount: BigDecimal,
    pub taker_payment: Vec<u8>,
    pub maker_payment: Vec<u8>,
    pub taker_fee: Vec<u8>,
    pub taker_refund_pub: Vec<u8>,
    pub taker_claim_pub: Vec<u8>,
    pub maker_refund_pub: Vec<u8>,
    pub maker_claim_pub: Vec<u8>,
    pub maker_payment_confirmations: u64,
    pub maker_payment_requires_nota: bool,
    pub maker_payment_locktime: u32,
}

impl TakerSwapWatcherV2 {
    pub(super) fn validate_publisher(&self, netid: u16, publisher: &[u8]) -> Result<(), String> {
        self.validate_shape(netid)?;
        if publisher != self.taker_refund_pub {
            return Err("the watcher delegation was not signed by its taker HTLC key".into());
        }
        Ok(())
    }

    pub(super) fn validate_shape(&self, netid: u16) -> Result<(), String> {
        if self.netid != netid || self.data.taker_coin == self.data.maker_coin || self.data.uuid.is_nil() {
            return Err("the watcher delegation has an incorrect network, pair or swap identifier".into());
        }
        for amount in [&self.taker_amount, &self.maker_amount] {
            // Bound the representation before fee conversion can expand a
            // remotely supplied exponent into an enormous power of ten.
            let (coefficient, scale) = amount.as_bigint_and_exponent();
            if !(-128..=128).contains(&scale) || coefficient.bits() > 512 {
                return Err("the watcher payment amount exceeds its numeric bounds".into());
            }
            if amount <= &BigDecimal::from(0) {
                return Err("watcher payment amounts must be positive".into());
            }
        }
        if self.data.taker_fee_hash.len() != 32
            || self.data.taker_payment_hash.len() != 32
            || self.data.maker_payment_hash.len() != 32
            || !matches!(self.data.secret_hash.len(), 20 | 32)
        {
            return Err("the watcher delegation contains an invalid transaction or secret hash".into());
        }
        for public in [
            &self.taker_refund_pub,
            &self.taker_claim_pub,
            &self.maker_refund_pub,
            &self.maker_claim_pub,
        ] {
            if public.len() != 33 || secp256k1::PublicKey::from_slice(public).is_err() {
                return Err("the watcher delegation contains an invalid HTLC public key".into());
            }
        }
        if self.data.lock_duration == 0 {
            return Err("the watcher delegation has no lock duration".into());
        }
        if self.data.taker_payment_confirmations > u64::from(u32::MAX)
            || self.maker_payment_confirmations > u64::from(u32::MAX)
        {
            return Err("the watcher confirmation count exceeds the chain API's range".into());
        }
        self.maker_locktime()?;
        self.taker_locktime()?;
        Ok(())
    }

    pub(super) fn taker_locktime(&self) -> Result<u32, String> {
        self.data
            .swap_started_at
            .checked_add(self.data.lock_duration)
            .and_then(|height| u32::try_from(height).ok())
            .ok_or_else(|| "the watcher taker timelock overflows".into())
    }

    pub(super) fn maker_locktime(&self) -> Result<u32, String> {
        let expected = self
            .data
            .lock_duration
            .checked_mul(2)
            .and_then(|duration| self.data.swap_started_at.checked_add(duration))
            .and_then(|height| u32::try_from(height).ok())
            .ok_or("the watcher maker timelock overflows")?;
        // The parties' accepted start times can differ by up to sixty seconds.
        // Use the negotiated lock, never a reconstruction from the taker's clock.
        if expected.abs_diff(self.maker_payment_locktime) > 60
            || self.maker_payment_locktime <= self.taker_locktime()?
        {
            return Err("the watcher maker timelock does not match the negotiated interval".into());
        }
        Ok(self.maker_payment_locktime)
    }
}

pub(super) async fn validate_fee(ctx: &WatcherStateMachineCtx, terms: &TakerSwapWatcherV2) -> Result<(), String> {
    for (coin, raw, hash) in [
        (&ctx.taker_coin, &terms.taker_payment, &terms.data.taker_payment_hash),
        (&ctx.maker_coin, &terms.maker_payment, &terms.data.maker_payment_hash),
        (&ctx.taker_coin, &terms.taker_fee, &terms.data.taker_fee_hash),
    ] {
        if coin.tx_enum_from_bytes(raw)?.tx_hash().0 != *hash {
            return Err("the watcher transaction does not match its advertised identifier".into());
        }
    }
    let cfg = net_config_or_panic(ctx.ctx.netid());
    let fee = compute_dex_fee_with_taker_pubkey(
        cfg,
        &ctx.taker_coin,
        &terms.data.maker_coin,
        &terms.taker_amount.clone().into(),
        &ctx.verified_pub,
    );
    if matches!(fee, DexFee::NoFee) {
        return Err("fee-exempt swaps do not support watcher delegation".into());
    }
    let transaction = ctx.taker_coin.tx_enum_from_bytes(&terms.taker_fee)?;
    ctx.taker_coin
        .validate_fee(ValidateFeeArgs {
            fee_tx: &transaction,
            expected_sender: &ctx.verified_pub,
            fee_addr: cfg.dex_fee_addr_raw_pubkey(),
            dex_fee: &fee,
            min_block_number: terms.data.taker_coin_start_block,
            uuid: terms.data.uuid.as_bytes(),
        })
        .compat()
        .await
}

pub(super) async fn validate_payments(ctx: &WatcherStateMachineCtx, terms: &TakerSwapWatcherV2) -> Result<(), String> {
    let taker_confirmations = terms
        .data
        .taker_payment_confirmations
        .max(ctx.taker_coin.required_confirmations())
        .max(1);
    let maker_confirmations = terms
        .maker_payment_confirmations
        .max(ctx.maker_coin.required_confirmations())
        .max(1);
    if taker_confirmations > u64::from(u32::MAX) || maker_confirmations > u64::from(u32::MAX) {
        return Err("the operator's watcher confirmation policy exceeds the chain API's range".into());
    }
    ctx.taker_coin
        .validate_watcher_payment(WatcherPaymentValidation {
            payment_tx: &terms.taker_payment,
            preimage: &terms.data.taker_payment_refund_preimage,
            time_lock: terms.taker_locktime()?,
            refund_pub: &terms.taker_refund_pub,
            claim_pub: &terms.taker_claim_pub,
            secret_hash: &terms.data.secret_hash,
            amount: terms.taker_amount.clone(),
            confirmations: taker_confirmations,
            kind: WatcherSpendKind::Refund,
        })
        .await?;
    ctx.maker_coin
        .validate_watcher_payment(WatcherPaymentValidation {
            payment_tx: &terms.maker_payment,
            preimage: &terms.data.maker_payment_spend_preimage,
            time_lock: terms.maker_locktime()?,
            refund_pub: &terms.maker_refund_pub,
            claim_pub: &terms.maker_claim_pub,
            secret_hash: &terms.data.secret_hash,
            amount: terms.maker_amount.clone(),
            confirmations: maker_confirmations,
            kind: WatcherSpendKind::Claim,
        })
        .await?;
    let deadline = common::now_ms() / 1000 + ctx.conf.wait_taker_payment as u64;
    ctx.taker_coin
        .wait_for_confirmations(
            &terms.taker_payment,
            taker_confirmations,
            terms.data.taker_payment_requires_nota.unwrap_or(false) || ctx.taker_coin.requires_notarization(),
            deadline,
            5,
        )
        .compat()
        .await?;
    ctx.maker_coin
        .wait_for_confirmations(
            &terms.maker_payment,
            maker_confirmations,
            terms.maker_payment_requires_nota || ctx.maker_coin.requires_notarization(),
            deadline,
            5,
        )
        .compat()
        .await
}

#[cfg(test)]
pub(super) fn fixture(netid: u16) -> TakerSwapWatcherV2 {
    let public = |byte| {
        crypto::privkey::key_pair_from_secret(&[byte; 32])
            .unwrap()
            .public_slice()
            .to_vec()
    };
    TakerSwapWatcherV2 {
        data: TakerSwapWatcherData {
            uuid: uuid::Uuid::from_bytes([7; 16]),
            secret_hash: vec![8; 20],
            maker_payment_spend_preimage: vec![9; 100],
            taker_payment_refund_preimage: vec![10; 100],
            swap_started_at: 1_800_000_000,
            lock_duration: 7500,
            taker_coin: "ARRR".into(),
            maker_coin: "LTC".into(),
            taker_fee_hash: vec![11; 32],
            taker_payment_hash: vec![12; 32],
            maker_payment_hash: vec![13; 32],
            taker_coin_start_block: 100,
            maker_coin_start_block: 200,
            taker_payment_confirmations: 1,
            taker_payment_requires_nota: Some(false),
            maker_pub: public(3),
        },
        netid,
        taker_amount: BigDecimal::from(10),
        maker_amount: BigDecimal::from(20),
        taker_payment: vec![14; 100],
        maker_payment: vec![15; 100],
        taker_fee: vec![16; 100],
        taker_refund_pub: public(1),
        taker_claim_pub: public(2),
        maker_refund_pub: public(3),
        maker_claim_pub: public(4),
        maker_payment_confirmations: 1,
        maker_payment_requires_nota: false,
        maker_payment_locktime: 1_800_015_000,
    }
}

#[cfg(test)]
mod tests {
    use super::super::swap_watcher::SwapWatcherMsg;
    use super::*;

    #[test]
    fn watcher_v2_checks_network_hashes_keys_amounts_and_agreed_timelocks() {
        for netid in [8762, 6133] {
            let good = fixture(netid);
            good.validate_shape(netid).unwrap();
            good.validate_publisher(netid, &good.taker_refund_pub).unwrap();
            assert!(good.validate_publisher(netid, &good.maker_claim_pub).is_err());
            assert!(good.validate_shape(if netid == 8762 { 6133 } else { 8762 }).is_err());
            let mut bad = good.clone();
            bad.data.taker_fee_hash.clear();
            assert!(bad.validate_shape(netid).is_err());
            let mut bad = good.clone();
            bad.maker_claim_pub = vec![0; 33];
            assert!(bad.validate_shape(netid).is_err());
            let mut bad = good.clone();
            bad.taker_amount = BigDecimal::from(0);
            assert!(bad.validate_shape(netid).is_err());
            let mut bad = good.clone();
            bad.taker_amount = "1e1000000000".parse().unwrap();
            assert!(bad.validate_shape(netid).is_err());
            let mut bad = good.clone();
            bad.data.swap_started_at = u64::MAX;
            assert!(bad.validate_shape(netid).is_err());
            let mut skew = good.clone();
            skew.maker_payment_locktime += 60;
            skew.validate_shape(netid).unwrap();
            assert_eq!(skew.maker_locktime().unwrap(), good.maker_payment_locktime + 60);
            skew.maker_payment_locktime += 1;
            assert!(skew.validate_shape(netid).is_err());
            let mut bad = good;
            bad.data.lock_duration = 0;
            assert!(bad.validate_shape(netid).is_err());
            let mut bad = fixture(netid);
            bad.maker_payment_confirmations = u64::from(u32::MAX) + 1;
            assert!(bad.validate_shape(netid).is_err());
        }
    }

    #[test]
    fn watcher_v2_signed_envelope_round_trips_without_altering_legacy_envelopes() {
        use common::msgpack_bounds::{check_msgpack_bounds, max_nesting_depth, MAX_PEER_NESTING};
        for netid in [8762, 6133] {
            let data = fixture(netid);
            let wire = SwapWatcherMsg::TakerSwapWatcherMsgV2(data.clone().into());
            let encoded = mm2_p2p::encode_and_sign(&wire, &[1; 32]).unwrap();
            assert_eq!(check_msgpack_bounds(&encoded), Ok(encoded.len()));
            assert!(max_nesting_depth(&encoded) <= MAX_PEER_NESTING / 2);
            let (read, _, publisher) = mm2_p2p::decode_signed::<SwapWatcherMsg>(&encoded).unwrap();
            let SwapWatcherMsg::TakerSwapWatcherMsgV2(read) = read else {
                panic!("wrong envelope version")
            };
            read.validate_publisher(netid, &publisher.to_bytes()).unwrap();
            assert_eq!(read.netid, netid);
            assert_eq!(read.maker_payment_locktime, data.maker_payment_locktime);
            assert_eq!(read.taker_refund_pub, data.taker_refund_pub);
            assert_eq!(read.maker_claim_pub, data.maker_claim_pub);
            let legacy = SwapWatcherMsg::TakerSwapWatcherMsg(data.data.into());
            let encoded = mm2_p2p::encode_and_sign(&legacy, &[19; 32]).unwrap();
            let (read, _, _) = mm2_p2p::decode_signed::<SwapWatcherMsg>(&encoded).unwrap();
            assert!(matches!(read, SwapWatcherMsg::TakerSwapWatcherMsg(_)));
        }
    }
}
