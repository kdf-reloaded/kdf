// SPDX-License-Identifier: GPL-2.0-or-later
//! Delegated ARRR HTLC transactions, read and signed with the Zcash libraries.

use super::z_htlc::build_z_p2sh_spend;
use super::{z_tx_read, ZCoin};
use crate::utxo::utxo_common::payment_script;
use crate::watcher_preimage::{complete_claim_script, template_parts};
use crate::{FoundSwapTxSpend, MarketCoinOps, Transaction, TransactionFut, ValidatePaymentInput, WatcherOps,
            WatcherPaymentValidation, WatcherSpendKind};
use async_trait::async_trait;
use chain::constants::SEQUENCE_FINAL;
use crypto::privkey::key_pair_from_secret;
use futures::compat::Future01CompatExt;
use futures::{FutureExt, TryFutureExt};
use keys::Public;
use script::{Builder, Opcode, Script};
use zcash_primitives::transaction::{self, sighash, txid::TxIdDigester, Authorized, Transaction as ZTransaction};
use zcash_protocol::value::Zatoshis;
use zcash_transparent::{address::Script as ZScript, bundle, sighash::TransparentAuthorizingContext};

#[async_trait]
impl WatcherOps for ZCoin {
    fn is_supported_by_watchers(&self) -> bool { self.supports_watcher_v2() }

    fn supports_watcher_v2(&self) -> bool { self.ticker() == "ARRR" }

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
        let payment = z_tx_read::read_tx(input.payment_tx)?;
        let preimage = z_tx_read::read_tx(input.preimage)?;
        let refund = Public::from_slice(input.refund_pub).map_err(|e| e.to_string())?;
        let claim = Public::from_slice(input.claim_pub).map_err(|e| e.to_string())?;
        let redeem = payment_script(input.time_lock, input.secret_hash, &refund, &claim);
        validate_preimage(
            &payment,
            &preimage,
            &redeem,
            input.kind,
            input.time_lock,
            &refund,
            &claim,
        )?;
        z_tx_read::validate_payment(
            self,
            ValidatePaymentInput {
                payment_tx: input.payment_tx.to_vec(),
                time_lock: input.time_lock,
                taker_pub: input.refund_pub.to_vec(),
                maker_pub: input.claim_pub.to_vec(),
                secret_hash: input.secret_hash.to_vec(),
                amount: input.amount,
                swap_contract_address: None,
                try_spv_proof_until: common::now_ms() / 1000 + 60,
                confirmations: input.confirmations.max(1),
            },
            input.refund_pub.to_vec(),
            input.claim_pub.to_vec(),
        )
        .compat()
        .await
    }

    fn complete_watcher_claim(&self, preimage: &[u8], secret: &[u8], secret_hash: &[u8]) -> Result<Vec<u8>, String> {
        complete_claim(preimage, secret, secret_hash)
    }

    async fn watcher_search_for_swap_tx_spend(
        &self,
        time_lock: u32,
        other_pub: &[u8],
        taker_pub: &[u8],
        secret_hash: &[u8],
        tx: &[u8],
        from_block: u64,
    ) -> Result<Option<FoundSwapTxSpend>, String> {
        z_tx_read::search_for_swap_tx_spend(
            self,
            time_lock,
            &Public::from_slice(taker_pub).map_err(|e| e.to_string())?,
            &Public::from_slice(other_pub).map_err(|e| e.to_string())?,
            secret_hash,
            tx,
            from_block,
        )
        .await
    }
}

#[allow(clippy::too_many_arguments)]
fn build_preimage(
    coin: ZCoin,
    payment: &[u8],
    time_lock: u32,
    other_pub: &[u8],
    secret_hash: &[u8],
    htlc_privkey: &[u8],
    kind: WatcherSpendKind,
) -> TransactionFut {
    let key = try_tx_fus!(key_pair_from_secret(htlc_privkey));
    let other = try_tx_fus!(Public::from_slice(other_pub));
    let payment = try_tx_fus!(z_tx_read::read_tx(payment));
    let (refund, claim, selector, sequence) = match kind {
        WatcherSpendKind::Claim => (&other, key.public(), Opcode::OP_0, SEQUENCE_FINAL),
        WatcherSpendKind::Refund => (key.public(), &other, Opcode::OP_1, SEQUENCE_FINAL - 1),
    };
    let redeem = payment_script(time_lock, secret_hash, refund, claim);
    let data = Builder::default().push_opcode(selector).into_script();
    Box::new(
        async move {
            // A refund can be hours away. Expiry zero keeps its transaction valid
            // through the timelock within this consensus epoch, without a fresh signature.
            let tx = build_z_p2sh_spend(
                &coin,
                payment,
                time_lock,
                sequence,
                redeem,
                data,
                key.private().secret.as_slice(),
                Some(0u32.into()),
            )
            .await
            .map_err(|e| crate::TransactionErr::Plain(e.to_string()))?;
            Ok(tx.into())
        }
        .boxed()
        .compat(),
    )
}

fn complete_claim(preimage: &[u8], secret: &[u8], secret_hash: &[u8]) -> Result<Vec<u8>, String> {
    let tx = z_tx_read::read_tx(preimage)?;
    let data = tx.into_data().try_map_bundles::<Authorized, String>(
        |bundle| {
            let mut bundle = bundle.ok_or("the watcher claim has no transparent bundle")?;
            if bundle.vin.len() != 1 {
                return Err("a watcher claim must spend exactly one HTLC output".into());
            }
            let input = &bundle.vin[0];
            let script = Script::from(input.script_sig().0 .0.clone());
            let completed = complete_claim_script(&script, secret, secret_hash)?;
            bundle.vin[0] = bundle::TxIn::from_parts(
                input.prevout().clone(),
                ZScript(zcash_script::script::Code(completed.to_vec())),
                input.sequence(),
            );
            Ok(Some(bundle))
        },
        Ok,
        Ok,
    )?;
    let completed = data.freeze().map_err(|e| e.to_string())?;
    Ok(completed.tx_hex())
}

// The parsed transaction has signatures but does not carry its input amount.
// Supply the verified funding output to the library's signature-hash calculator.
#[derive(Debug)]
struct WatcherTransparentAuth {
    spent: bundle::TxOut,
}

impl bundle::Authorization for WatcherTransparentAuth {
    type ScriptSig = ZScript;
}

impl TransparentAuthorizingContext for WatcherTransparentAuth {
    fn input_amounts(&self) -> Vec<Zatoshis> { vec![self.spent.value()] }
    fn input_scriptpubkeys(&self) -> Vec<ZScript> { vec![self.spent.script_pubkey().clone()] }
}

struct WatcherAuth;

impl transaction::Authorization for WatcherAuth {
    type TransparentAuth = WatcherTransparentAuth;
    type SaplingAuth = sapling::bundle::Authorized;
    type OrchardAuth = orchard::bundle::Authorized;
}

#[allow(clippy::too_many_arguments)]
fn validate_preimage(
    payment: &ZTransaction,
    preimage: &ZTransaction,
    redeem: &Script,
    kind: WatcherSpendKind,
    time_lock: u32,
    refund_pub: &Public,
    claim_pub: &Public,
) -> Result<(), String> {
    let funding = payment
        .transparent_bundle()
        .and_then(|bundle| bundle.vout.first())
        .ok_or("the watcher HTLC has no transparent funding output")?;
    let bundle = preimage
        .transparent_bundle()
        .ok_or("the watcher preimage has no transparent input")?;
    if bundle.vin.len() != 1 || u32::from(preimage.expiry_height()) != 0 {
        return Err("a watcher preimage requires one input and no expiry height".into());
    }
    let input = &bundle.vin[0];
    if input.prevout().hash() != payment.txid().as_ref() || input.prevout().n() != 0 {
        return Err("the watcher preimage spends a different HTLC payment".into());
    }
    let (signature, actual_redeem) = template_parts(&Script::from(input.script_sig().0 .0.clone()), kind)?;
    if actual_redeem != *redeem || signature.last() != Some(&1) {
        return Err("the watcher preimage has a different redeem script or signature hash type".into());
    }
    let signer = match kind {
        WatcherSpendKind::Claim if input.sequence() == SEQUENCE_FINAL => claim_pub,
        WatcherSpendKind::Refund if input.sequence() == SEQUENCE_FINAL - 1 && preimage.lock_time() >= time_lock => {
            refund_pub
        },
        _ => return Err("the watcher preimage has incorrect HTLC timelock semantics".into()),
    };
    let data = signing_data(preimage, funding.clone());
    let transparent = data
        .transparent_bundle()
        .ok_or("the watcher preimage has no input bundle")?;
    let script_code = ZScript(zcash_script::script::Code(redeem.to_vec()));
    let signable = zcash_transparent::sighash::SignableInput::from_parts(
        transparent,
        zcash_transparent::sighash::SighashType::ALL,
        0,
        &script_code,
        funding.script_pubkey(),
        funding.value(),
    )
    .map_err(|e| e.to_string())?;
    let digest = sighash::signature_hash(
        &data,
        &sighash::SignableInput::Transparent(signable),
        &data.digest(TxIdDigester),
    );
    let public = secp256k1::PublicKey::from_slice(signer).map_err(|e| e.to_string())?;
    let signature =
        secp256k1::ecdsa::Signature::from_der(&signature[..signature.len() - 1]).map_err(|e| e.to_string())?;
    let message = secp256k1::Message::from_digest(*digest.as_ref());
    secp256k1::Secp256k1::verification_only()
        .verify_ecdsa(&message, &signature, &public)
        .map_err(|e| e.to_string())
}

fn signing_data(tx: &ZTransaction, spent: bundle::TxOut) -> transaction::TransactionData<WatcherAuth> {
    tx.clone().into_data().map_bundles::<WatcherAuth>(
        |bundle| {
            bundle.map(|bundle| bundle::Bundle {
                vin: bundle
                    .vin
                    .into_iter()
                    .map(|input| {
                        bundle::TxIn::from_parts(input.prevout().clone(), input.script_sig().clone(), input.sequence())
                    })
                    .collect(),
                vout: bundle.vout,
                authorization: WatcherTransparentAuth { spent },
            })
        },
        |bundle| bundle,
        |bundle| bundle,
    )
}

#[cfg(test)]
mod tests {
    use super::super::z_v6_construction_tests::{params, standard_config, NoProver};
    use super::*;
    use kdf_crypto::dhash160;
    use zcash_primitives::transaction::{builder::Builder as ZBuilder, fees::fixed::FeeRule};
    use zcash_transparent::{address::TransparentAddress,
                            builder::TransparentSigningSet,
                            bundle::{OutPoint, TxOut}};

    #[test]
    fn watcher_v6_claim_keeps_its_signature_and_transaction_id_when_the_secret_is_inserted() {
        exercise_v6_templates(None);
    }

    #[test]
    #[ignore = "requires Sapling proving parameters in ARRR_SAPLING_PARAMS"]
    fn watcher_v6_claim_preserves_real_sapling_proofs_and_binding_signature() {
        let dir = std::path::PathBuf::from(std::env::var_os("ARRR_SAPLING_PARAMS").expect("set ARRR_SAPLING_PARAMS"));
        let prover = zcash_proofs::prover::LocalTxProver::new(
            &dir.join("sapling-spend.params"),
            &dir.join("sapling-output.params"),
        );
        exercise_v6_templates(Some(&prover));
    }

    fn exercise_v6_templates(prover: Option<&zcash_proofs::prover::LocalTxProver>) {
        let refund = key_pair_from_secret(&[13; 32]).unwrap();
        let claim = key_pair_from_secret(&[14; 32]).unwrap();
        let secret = [21; 32];
        let hash = dhash160(&secret).take();
        let time_lock = 1_800_000_000;
        let redeem = payment_script(time_lock, &hash, refund.public(), claim.public());
        let funding_output = TxOut::new(
            Zatoshis::const_from_u64(100_000),
            TransparentAddress::ScriptHash(dhash160(&redeem).take()).script().into(),
        );
        // A version-six payment with a real HTLC output. Its synthetic funding
        // input is irrelevant to verifying the delegated spend's signature.
        let payment = transaction::TransactionData::<Authorized>::from_parts_v6(
            zcash_protocol::consensus::BranchId::Nu6_3,
            0,
            0u32.into(),
            Some(bundle::Bundle {
                vin: vec![],
                vout: vec![funding_output.clone()],
                authorization: bundle::Authorized,
            }),
            None,
            None,
            None,
        )
        .freeze()
        .unwrap();
        for kind in [WatcherSpendKind::Claim, WatcherSpendKind::Refund] {
            let key = if kind == WatcherSpendKind::Claim {
                &claim
            } else {
                &refund
            };
            let (selector, sequence) = if kind == WatcherSpendKind::Claim {
                (Opcode::OP_0, SEQUENCE_FINAL)
            } else {
                (Opcode::OP_1, SEQUENCE_FINAL - 1)
            };
            let mut builder = ZBuilder::new(
                params(Some(297)),
                300u32.into(),
                standard_config(prover.map(|_| sapling::Anchor::empty_tree())),
            )
            .with_expiry_height(0u32.into());
            builder.set_lock_time(time_lock);
            builder
                .add_kdf_p2sh_input(
                    secp256k1::PublicKey::from_slice(key.public()).unwrap(),
                    OutPoint::new(*payment.txid().as_ref(), 0),
                    funding_output.clone(),
                    zcash_script::script::FromChain::parse(&zcash_script::script::Code(redeem.to_vec())).unwrap(),
                    zcash_script::script::Sig::parse(&zcash_script::script::Code(vec![selector as u8])).unwrap(),
                    sequence,
                )
                .unwrap();
            if prover.is_some() {
                let destination = sapling::zip32::ExtendedSpendingKey::master(&[29; 32])
                    .default_address()
                    .1;
                builder
                    .add_sapling_output::<std::convert::Infallible>(
                        None,
                        destination,
                        Zatoshis::const_from_u64(99_000),
                        zcash_protocol::memo::MemoBytes::empty(),
                    )
                    .unwrap();
            } else {
                builder.add_transparent_output_raw(TxOut::new(
                    Zatoshis::const_from_u64(99_000),
                    TransparentAddress::PublicKeyHash([4; 20]).script().into(),
                ));
            }
            let mut signing = TransparentSigningSet::new();
            signing.add_key(secp256k1::SecretKey::from_slice(key.private().secret.as_slice()).unwrap());
            let built = match prover {
                Some(prover) => builder.build(
                    &signing,
                    &[],
                    &[],
                    rand::rngs::OsRng,
                    prover,
                    prover,
                    &FeeRule::non_standard(Zatoshis::const_from_u64(1_000)),
                ),
                None => builder.build(
                    &signing,
                    &[],
                    &[],
                    rand::rngs::OsRng,
                    &NoProver,
                    &NoProver,
                    &FeeRule::non_standard(Zatoshis::const_from_u64(1_000)),
                ),
            };
            let template = built.unwrap().into_transaction();
            validate_preimage(
                &payment,
                &template,
                &redeem,
                kind,
                time_lock,
                refund.public(),
                claim.public(),
            )
            .unwrap();
            assert_eq!(u32::from(template.expiry_height()), 0);
            if kind == WatcherSpendKind::Claim {
                let completed = complete_claim(&template.tx_hex(), &secret, &hash).unwrap();
                let completed = z_tx_read::read_tx(&completed).unwrap();
                assert_eq!(completed.txid(), template.txid());
                if let Some(prover) = prover {
                    let data = signing_data(&completed, funding_output.clone());
                    let digest =
                        sighash::signature_hash(&data, &sighash::SignableInput::Shielded, &data.digest(TxIdDigester));
                    let (spend_key, output_key) = prover.verifying_keys();
                    let mut validator = sapling::BatchValidator::new();
                    assert!(validator.check_bundle(completed.sapling_bundle().unwrap().clone(), *digest.as_ref()));
                    assert!(
                        validator.validate(&spend_key, &output_key, rand::rngs::OsRng),
                        "inserting the secret must leave every Sapling proof and binding signature valid"
                    );
                } else {
                    assert!(completed.sapling_bundle().is_none());
                    assert!(template.sapling_bundle().is_none());
                }
                assert_eq!(z_tx_read::extract_secret(&hash, &completed.tx_hex()).unwrap(), secret);
                assert!(complete_claim(&template.tx_hex(), &[22; 32], &hash).is_err());
            }
            assert!(validate_preimage(
                &payment,
                &template,
                &redeem,
                kind,
                time_lock,
                claim.public(),
                refund.public()
            )
            .is_err());
        }
    }
}
