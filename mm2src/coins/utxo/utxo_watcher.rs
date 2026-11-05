// SPDX-License-Identifier: GPL-2.0-or-later
//! Delegated legacy HTLC claims and refunds for LTC and vARRR.

use super::utxo_standard::UtxoStandardCoin;
use super::*;
use crate::watcher_preimage::{complete_claim_script, template_parts};
use crate::{TransactionErr, TransactionFut, WatcherOps, WatcherPaymentValidation, WatcherSpendKind};
use async_trait::async_trait;
use chain::constants::SEQUENCE_FINAL;
use crypto::privkey::key_pair_from_secret;
use futures::{FutureExt, TryFutureExt};
use serialization::deserialize;

#[async_trait]
impl WatcherOps for UtxoStandardCoin {
    fn is_supported_by_watchers(&self) -> bool { self.supports_watcher_v2() }

    // Expand this set only with the corresponding chain's transaction tests.
    fn supports_watcher_v2(&self) -> bool {
        matches!(self.ticker(), "LTC" | "vARRR")
            && !self.as_ref().conf.is_pos
            && self.as_ref().priv_key_policy.key_pair().is_some()
    }

    fn create_maker_payment_spend_preimage(
        &self,
        payment: &[u8],
        time_lock: u32,
        other_pub: &[u8],
        secret_hash: &[u8],
        _swap_unique_data: &[u8],
        htlc_privkey: &[u8],
    ) -> TransactionFut {
        build_preimage(
            self.clone(),
            payment,
            time_lock,
            other_pub,
            secret_hash,
            htlc_privkey,
            WatcherSpendKind::Claim,
        )
    }

    fn create_taker_payment_refund_preimage(
        &self,
        payment: &[u8],
        time_lock: u32,
        other_pub: &[u8],
        secret_hash: &[u8],
        _swap_unique_data: &[u8],
        htlc_privkey: &[u8],
    ) -> TransactionFut {
        build_preimage(
            self.clone(),
            payment,
            time_lock,
            other_pub,
            secret_hash,
            htlc_privkey,
            WatcherSpendKind::Refund,
        )
    }

    async fn validate_watcher_payment(&self, input: WatcherPaymentValidation<'_>) -> Result<(), String> {
        let mut payment: UtxoTx = deserialize(input.payment_tx).map_err(|e| e.to_string())?;
        payment.tx_hash_algo = self.as_ref().tx_hash_algo;
        let refund = Public::from_slice(input.refund_pub).map_err(|e| e.to_string())?;
        let claim = Public::from_slice(input.claim_pub).map_err(|e| e.to_string())?;
        let redeem = utxo_common::payment_script(input.time_lock, input.secret_hash, &refund, &claim);
        let preimage: UtxoTx = deserialize(input.preimage).map_err(|e| e.to_string())?;
        validate_preimage(self, &payment, &preimage, &redeem, input.kind, &refund, &claim)?;
        utxo_common::validate_payment(
            self.clone(),
            payment,
            0,
            &refund,
            &claim,
            input.secret_hash,
            input.amount,
            input.time_lock,
            common::now_ms() / 1000 + 60,
            input.confirmations.max(1),
        )
        .compat()
        .await
    }

    fn complete_watcher_claim(&self, preimage: &[u8], secret: &[u8], secret_hash: &[u8]) -> Result<Vec<u8>, String> {
        let mut tx: UtxoTx = deserialize(preimage).map_err(|e| e.to_string())?;
        if tx.inputs.len() != 1 {
            return Err("a watcher claim must spend exactly one HTLC output".into());
        }
        tx.inputs[0].script_sig =
            complete_claim_script(&tx.inputs[0].script_sig.clone().into(), secret, secret_hash)?.into();
        Ok(serialize(&tx).take())
    }

    async fn watcher_search_for_swap_tx_spend(
        &self,
        time_lock: u32,
        other_pub: &[u8],
        taker_pub: &[u8],
        secret_hash: &[u8],
        tx: &[u8],
        search_from_block: u64,
    ) -> Result<Option<FoundSwapTxSpend>, String> {
        let coin: UtxoArc = self.clone().into();
        utxo_common::search_for_swap_tx_spend_my(
            &coin,
            time_lock,
            taker_pub,
            other_pub,
            secret_hash,
            tx,
            0,
            search_from_block,
        )
        .await
    }
}

#[allow(clippy::too_many_arguments)]
fn build_preimage(
    coin: UtxoStandardCoin,
    payment: &[u8],
    time_lock: u32,
    other_pub: &[u8],
    secret_hash: &[u8],
    htlc_privkey: &[u8],
    kind: WatcherSpendKind,
) -> TransactionFut {
    let key = try_tx_fus!(key_pair_from_secret(htlc_privkey));
    let other = try_tx_fus!(Public::from_slice(other_pub));
    let mut payment: UtxoTx = try_tx_fus!(deserialize(payment).map_err(|e| format!("{e}")));
    payment.tx_hash_algo = coin.as_ref().tx_hash_algo;
    let (refund, claim, selector, sequence) = match kind {
        WatcherSpendKind::Claim => (&other, key.public(), Opcode::OP_0, SEQUENCE_FINAL),
        WatcherSpendKind::Refund => (key.public(), &other, Opcode::OP_1, SEQUENCE_FINAL - 1),
    };
    let redeem = utxo_common::payment_script(time_lock, secret_hash, refund, claim);
    let expected_script = Builder::build_p2sh(&dhash160(&redeem).into()).to_bytes();
    let value = match payment.outputs.first() {
        Some(output) if output.script_pubkey == expected_script => output.value,
        _ => {
            return Box::new(futures01::future::err(TransactionErr::Plain(
                "the payment does not fund the requested watcher HTLC".into(),
            )))
        },
    };
    let destination = utxo_common::address_from_pubkey(
        key.public(),
        coin.as_ref().conf.pub_addr_prefix,
        coin.as_ref().conf.pub_t_addr_prefix,
        coin.as_ref().conf.checksum_type,
        coin.as_ref().conf.bech32_hrp.clone(),
        coin.addr_format().clone(),
    );
    Box::new(
        async move {
            let fee = try_tx_s!(coin.get_htlc_spend_fee(utxo_common::DEFAULT_SWAP_TX_SPEND_SIZE).await);
            let output = chain::TransactionOutput {
                value: utxo_common::htlc_spend_output_value(value, fee)?,
                script_pubkey: utxo_common::output_script(&destination, ScriptType::P2PKH).to_bytes(),
            };
            let script_data = Builder::default().push_opcode(selector).into_script();
            let tx = try_tx_s!(
                coin.p2sh_spending_tx(
                    payment,
                    redeem.into(),
                    vec![output],
                    script_data,
                    sequence,
                    time_lock,
                    &key,
                )
                .await
            );
            Ok(tx.into())
        }
        .boxed()
        .compat(),
    )
}

fn validate_preimage(
    coin: &UtxoStandardCoin,
    payment: &UtxoTx,
    preimage: &UtxoTx,
    redeem: &Script,
    kind: WatcherSpendKind,
    refund_pub: &Public,
    claim_pub: &Public,
) -> Result<(), String> {
    let config = &coin.as_ref().conf;
    if preimage.version != config.tx_version
        || preimage.overwintered != config.overwintered
        || preimage.zcash != config.zcash
        || preimage.version_group_id != config.version_group_id
    {
        return Err("the watcher preimage uses a different chain transaction format".into());
    }
    if preimage.inputs.len() != 1 || preimage.outputs.len() != 1 || preimage.expiry_height != 0 {
        return Err("a watcher preimage must have one input, one output and no expiry height".into());
    }
    let spent = payment.outputs.first().ok_or("the HTLC payment has no output")?;
    let input = &preimage.inputs[0];
    if input.previous_output.hash != payment.hash() || input.previous_output.index != 0 {
        return Err("the watcher preimage spends a different payment".into());
    }
    let (signature, actual_redeem) = template_parts(&input.script_sig.clone().into(), kind)?;
    if actual_redeem != *redeem || preimage.outputs[0].value == 0 || preimage.outputs[0].value >= spent.value {
        return Err("the watcher preimage has different payment terms or an invalid value".into());
    }
    let pubkey = match kind {
        WatcherSpendKind::Claim => {
            if input.sequence != SEQUENCE_FINAL {
                return Err("the watcher claim is not final".into());
            }
            claim_pub
        },
        WatcherSpendKind::Refund => {
            if input.sequence != SEQUENCE_FINAL - 1 || preimage.lock_time < payment_locktime(redeem)? {
                return Err("the watcher refund does not enforce its timelock".into());
            }
            refund_pub
        },
    };
    let hash_type = *signature.last().ok_or("the watcher signature has no hash type")?;
    if u32::from(hash_type) != (1 | coin.as_ref().conf.fork_id) {
        return Err("a watcher signature must commit to all transaction outputs".into());
    }
    let mut signer = TransactionInputSigner::from(preimage.clone());
    signer.consensus_branch_id = coin.as_ref().conf.consensus_branch_id;
    // ZIP-243 reads this value from the input descriptor. It cannot be
    // recovered from the spend itself, so restore the verified funding value.
    signer.inputs[0].amount = spent.value;
    let digest = signer.signature_hash(
        0,
        spent.value,
        redeem,
        coin.as_ref().conf.signature_version,
        u32::from(hash_type),
    );
    let signature = keys::Signature::from(&signature[..signature.len() - 1]);
    if !pubkey.verify(&digest, &signature).map_err(|e| e.to_string())? {
        return Err("the watcher preimage signature does not authorize the HTLC spend".into());
    }
    Ok(())
}

fn payment_locktime(redeem: &Script) -> Result<u32, String> {
    let instruction = redeem
        .iter()
        .nth(1)
        .ok_or("the HTLC has no timelock")?
        .map_err(|e| e.to_string())?;
    let bytes: [u8; 4] = instruction
        .data
        .ok_or("the HTLC timelock is not a data push")?
        .try_into()
        .map_err(|_| "the HTLC timelock must contain four bytes")?;
    Ok(u32::from_le_bytes(bytes))
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::utxo::utxo_tests::{native_client_for_test, utxo_coin_fields_for_test, utxo_coin_from_fields};
    use crate::TransactionEnum;
    use futures::executor::block_on;
    use mocktopus::mocking::*;

    fn coin(ticker: &str) -> UtxoStandardCoin {
        let mut fields = utxo_coin_fields_for_test(
            UtxoRpcClientEnum::Native(native_client_for_test()),
            Some("watcher fixture"),
            false,
        );
        fields.conf.ticker = ticker.to_owned();
        fields.conf.tx_version = if ticker == "vARRR" { 4 } else { 2 };
        fields.conf.zcash = ticker == "vARRR";
        fields.conf.overwintered = ticker == "vARRR";
        if ticker != "vARRR" {
            fields.conf.version_group_id = 0;
            fields.conf.consensus_branch_id = 0;
        }
        utxo_coin_from_fields(fields)
    }

    #[test]
    fn watcher_preimages_sign_the_correct_key_and_preserve_outputs_after_secret_insertion() {
        UtxoStandardCoin::get_current_mtp.mock_safe(|_| MockResult::Return(Box::pin(async { Ok(1_700_000_000) })));
        let refund = key_pair_from_secret(&[11; 32]).unwrap();
        let claim = key_pair_from_secret(&[12; 32]).unwrap();
        let secret = [19; 32];
        let hash = dhash160(&secret).take();
        let time_lock = 1_800_000_000;
        let redeem = utxo_common::payment_script(time_lock, &hash, refund.public(), claim.public());
        for ticker in ["LTC", "vARRR"] {
            let coin = coin(ticker);
            let payment = UtxoTx {
                version: coin.as_ref().conf.tx_version,
                zcash: coin.as_ref().conf.zcash,
                overwintered: coin.as_ref().conf.overwintered,
                version_group_id: coin.as_ref().conf.version_group_id,
                inputs: vec![chain::TransactionInput {
                    previous_output: chain::OutPoint {
                        hash: [42; 32].into(),
                        index: 0,
                    },
                    script_sig: vec![1, 1].into(),
                    sequence: SEQUENCE_FINAL,
                    script_witness: vec![],
                }],
                outputs: vec![chain::TransactionOutput {
                    value: 100_000,
                    script_pubkey: Builder::build_p2sh(&dhash160(&redeem).into()).to_bytes(),
                }],
                ..UtxoTx::default()
            };
            let raw = serialize(&payment).take();
            assert!(coin.supports_watcher_v2());
            for kind in [WatcherSpendKind::Claim, WatcherSpendKind::Refund] {
                let (key, other) = match kind {
                    WatcherSpendKind::Claim => (&claim, refund.public()),
                    WatcherSpendKind::Refund => (&refund, claim.public()),
                };
                let tx = block_on(
                    build_preimage(
                        coin.clone(),
                        &raw,
                        time_lock,
                        other,
                        &hash,
                        key.private().secret.as_slice(),
                        kind,
                    )
                    .compat(),
                )
                .unwrap();
                let TransactionEnum::UtxoTx(template) = tx else {
                    panic!("expected a UTXO watcher template")
                };
                validate_preimage(
                    &coin,
                    &payment,
                    &template,
                    &redeem,
                    kind,
                    refund.public(),
                    claim.public(),
                )
                .unwrap_or_else(|error| panic!("{ticker} {kind:?}: {error}"));
                assert!(validate_preimage(
                    &coin,
                    &payment,
                    &template,
                    &redeem,
                    kind,
                    claim.public(),
                    refund.public()
                )
                .is_err());
                let mut redirected = template.clone();
                redirected.outputs[0].value += 1;
                assert!(validate_preimage(
                    &coin,
                    &payment,
                    &redirected,
                    &redeem,
                    kind,
                    refund.public(),
                    claim.public()
                )
                .is_err());
                if kind == WatcherSpendKind::Claim {
                    let complete = coin
                        .complete_watcher_claim(&serialize(&template).take(), &secret, &hash)
                        .unwrap();
                    let complete: UtxoTx = deserialize(complete.as_slice()).unwrap();
                    assert_eq!(complete.outputs, template.outputs);
                    assert_eq!(complete.inputs[0].previous_output, template.inputs[0].previous_output);
                    let mut restored = complete.clone();
                    restored.inputs[0].script_sig = template.inputs[0].script_sig.clone();
                    assert_eq!(restored, template);
                    assert_eq!(
                        utxo_common::extract_secret(&hash, &serialize(&complete).take()).unwrap(),
                        secret
                    );
                } else {
                    assert_eq!(template.inputs[0].sequence, SEQUENCE_FINAL - 1);
                    assert_eq!(template.lock_time, time_lock);
                }
            }
        }
        assert!(!coin("BTC").supports_watcher_v2());
    }
}
