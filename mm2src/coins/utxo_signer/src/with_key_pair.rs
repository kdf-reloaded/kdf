use crate::sign_common::{complete_tx, p2pk_spend_with_signature, p2pkh_spend_with_signature,
                         p2sh_spend_with_signature, p2wpkh_spend_with_signature};
use crate::Signature;
use chain::{OutPoint, Transaction as UtxoTx, TransactionInput};
use derive_more::Display;
use keys::bytes::Bytes;
use keys::KeyPair;
use mm2_err_handle::prelude::*;
use primitives::hash::H256;
use script::{Builder, Script, SignatureVersion, TransactionInputSigner, UnsignedTransactionInput};
use std::collections::HashSet;

pub type UtxoSignWithKeyPairResult<T> = Result<T, MmError<UtxoSignWithKeyPairError>>;

#[derive(Debug, Display)]
pub enum UtxoSignWithKeyPairError {
    #[display(
        fmt = "{} script '{}' built from input key pair doesn't match expected prev script '{}'",
        script_type,
        script,
        prev_script
    )]
    MismatchScript {
        script_type: String,
        script: Script,
        prev_script: Script,
    },
    #[display(fmt = "Input index '{}' is out of bound. Total length = {}", index, len)]
    InputIndexOutOfBound { len: usize, index: usize },
    #[display(fmt = "Error signing using a private key")]
    ErrorSigning(keys::Error),
}

impl From<keys::Error> for UtxoSignWithKeyPairError {
    fn from(sign: keys::Error) -> Self { UtxoSignWithKeyPairError::ErrorSigning(sign) }
}

pub fn sign_tx(
    unsigned: TransactionInputSigner,
    key_pair: &KeyPair,
    prev_script: Script,
    signature_version: SignatureVersion,
    fork_id: u32,
) -> UtxoSignWithKeyPairResult<UtxoTx> {
    sign_tx_with_p2pk(
        unsigned,
        key_pair,
        prev_script,
        signature_version,
        fork_id,
        &HashSet::new(),
    )
}

/// Like [`sign_tx`], but the inputs whose previous outpoint is listed in `p2pk_outpoints`
/// are treated as pay-to-pubkey (P2PK) outputs and signed with a signature-only scriptSig.
///
/// This is the entry point used by the withdraw/selection path: P2PK unspents discovered for
/// the wallet's legacy address are spendable alongside ordinary P2PKH/P2WPKH inputs. When the
/// `p2pk_outpoints` set is empty this behaves exactly like [`sign_tx`], keeping the existing
/// callers unaffected.
pub fn sign_tx_with_p2pk(
    unsigned: TransactionInputSigner,
    key_pair: &KeyPair,
    prev_script: Script,
    signature_version: SignatureVersion,
    fork_id: u32,
    p2pk_outpoints: &HashSet<OutPoint>,
) -> UtxoSignWithKeyPairResult<UtxoTx> {
    let mut signed_inputs = vec![];
    for (i, input) in unsigned.inputs.iter().enumerate() {
        if p2pk_outpoints.contains(&input.previous_output) {
            signed_inputs.push(p2pk_spend(&unsigned, i, key_pair, signature_version, fork_id)?);
            continue;
        }
        match signature_version {
            SignatureVersion::WitnessV0 => signed_inputs.push(p2wpkh_spend(
                &unsigned,
                i,
                key_pair,
                prev_script.clone(),
                signature_version,
                fork_id,
            )?),
            _ => signed_inputs.push(p2pkh_spend(
                &unsigned,
                i,
                key_pair,
                prev_script.clone(),
                signature_version,
                fork_id,
            )?),
        }
    }
    Ok(complete_tx(unsigned, signed_inputs))
}

/// Creates signed input spending p2pk output
pub fn p2pk_spend(
    signer: &TransactionInputSigner,
    input_index: usize,
    key_pair: &KeyPair,
    signature_version: SignatureVersion,
    fork_id: u32,
) -> UtxoSignWithKeyPairResult<TransactionInput> {
    let unsigned_input = get_input(signer, input_index)?;

    let script = Builder::build_p2pk(key_pair.public());
    let signature = calc_and_sign_sighash(signer, input_index, script, key_pair, signature_version, fork_id)?;
    Ok(p2pk_spend_with_signature(unsigned_input, fork_id, signature))
}

/// Creates signed input spending p2pkh output
pub fn p2pkh_spend(
    signer: &TransactionInputSigner,
    input_index: usize,
    key_pair: &KeyPair,
    prev_script: Script,
    signature_version: SignatureVersion,
    fork_id: u32,
) -> UtxoSignWithKeyPairResult<TransactionInput> {
    let unsigned_input = get_input(signer, input_index)?;

    let script = Builder::build_p2pkh(&key_pair.public().address_hash().into());
    if script != prev_script {
        return MmError::err(UtxoSignWithKeyPairError::MismatchScript {
            script_type: "P2PKH".to_owned(),
            script,
            prev_script,
        });
    }

    let signature = calc_and_sign_sighash(signer, input_index, script, key_pair, signature_version, fork_id)?;
    Ok(p2pkh_spend_with_signature(
        unsigned_input,
        key_pair.public(),
        fork_id,
        signature,
    ))
}

/// Creates signed input spending hash time locked p2sh output
pub fn p2sh_spend(
    signer: &TransactionInputSigner,
    input_index: usize,
    key_pair: &KeyPair,
    script_data: Script,
    redeem_script: Script,
    signature_version: SignatureVersion,
    fork_id: u32,
) -> UtxoSignWithKeyPairResult<TransactionInput> {
    let unsigned_input = get_input(signer, input_index)?;

    let signature = calc_and_sign_sighash(
        signer,
        input_index,
        redeem_script.clone(),
        key_pair,
        signature_version,
        fork_id,
    )?;
    Ok(p2sh_spend_with_signature(
        unsigned_input,
        redeem_script,
        script_data,
        fork_id,
        signature,
    ))
}

/// Creates signed input spending p2wpkh output
pub fn p2wpkh_spend(
    signer: &TransactionInputSigner,
    input_index: usize,
    key_pair: &KeyPair,
    prev_script: Script,
    signature_version: SignatureVersion,
    fork_id: u32,
) -> UtxoSignWithKeyPairResult<TransactionInput> {
    let unsigned_input = get_input(signer, input_index)?;

    let script = Builder::build_p2pkh(&key_pair.public().address_hash().into());
    if script != prev_script {
        return MmError::err(UtxoSignWithKeyPairError::MismatchScript {
            script_type: "P2PKH".to_owned(),
            script,
            prev_script,
        });
    }

    let signature = calc_and_sign_sighash(signer, input_index, script, key_pair, signature_version, fork_id)?;
    Ok(p2wpkh_spend_with_signature(
        unsigned_input,
        key_pair.public(),
        fork_id,
        signature,
    ))
}

/// Calculates the input script hash and sign it using `key_pair`.
pub(crate) fn calc_and_sign_sighash(
    signer: &TransactionInputSigner,
    input_index: usize,
    output_script: Script,
    key_pair: &KeyPair,
    signature_version: SignatureVersion,
    fork_id: u32,
) -> UtxoSignWithKeyPairResult<Signature> {
    let sighash = signature_hash_to_sign(signer, input_index, output_script, signature_version, fork_id)?;
    sign_message(&sighash, key_pair)
}

fn signature_hash_to_sign(
    signer: &TransactionInputSigner,
    input_index: usize,
    output_script: Script,
    signature_version: SignatureVersion,
    fork_id: u32,
) -> UtxoSignWithKeyPairResult<H256> {
    let input_amount = get_input(signer, input_index)?.amount;

    let sighash_type = 1 | fork_id;
    Ok(signer.signature_hash(
        input_index,
        input_amount,
        &output_script,
        signature_version,
        sighash_type,
    ))
}

fn sign_message(message: &H256, key_pair: &KeyPair) -> UtxoSignWithKeyPairResult<Bytes> {
    let signature = key_pair.private().sign(message)?;
    Ok(Bytes::from(signature.to_vec()))
}

#[track_caller]
fn get_input(
    unsigned: &TransactionInputSigner,
    input_index: usize,
) -> UtxoSignWithKeyPairResult<&UnsignedTransactionInput> {
    unsigned
        .inputs
        .get(input_index)
        .or_mm_err(|| UtxoSignWithKeyPairError::InputIndexOutOfBound {
            len: unsigned.inputs.len(),
            index: input_index,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chain::TransactionOutput;
    use keys::{AddressHashEnum, KeyPair, Signature};
    use script::{Script, SignerHashAlgo};

    const SECRET_1C: &str = "Kwr371tjA9u2rFSMZjTNun2PXXP3WPZu2afRHTcta6KxEUdm1vEw";

    fn make_unsigned(prevouts: &[OutPoint], amounts: &[u64]) -> TransactionInputSigner {
        let inputs = prevouts
            .iter()
            .zip(amounts.iter())
            .map(|(outpoint, amount)| UnsignedTransactionInput {
                previous_output: *outpoint,
                sequence: 0xffff_fffe,
                amount: *amount,
                witness: vec![],
            })
            .collect();

        TransactionInputSigner {
            version: 1,
            n_time: None,
            overwintered: false,
            version_group_id: 0,
            consensus_branch_id: 0,
            expiry_height: 0,
            value_balance: 0,
            inputs,
            outputs: vec![TransactionOutput {
                value: 1,
                script_pubkey: vec![0x6a].into(),
            }],
            lock_time: 0,
            join_splits: vec![],
            shielded_spends: vec![],
            shielded_outputs: vec![],
            zcash: false,
            str_d_zeel: None,
            hash_algo: SignerHashAlgo::DSHA256,
        }
    }

    fn parse_sig_with_hashtype(script_sig: &Bytes) -> Vec<u8> {
        let sig_script = Script::from(script_sig.to_vec());
        let first = sig_script
            .get_instruction(0)
            .expect("scriptSig must have at least one instruction")
            .expect("scriptSig first instruction must decode");
        first
            .data
            .expect("scriptSig first instruction must be push-data")
            .to_vec()
    }

    #[test]
    fn sign_tx_with_p2pk_builds_signature_only_scriptsig() {
        let key_pair = KeyPair::from_private(SECRET_1C.into()).unwrap();
        let fork_id = 0u32;
        let sighash_type = 1 | fork_id;

        let p2pk_prevout = OutPoint {
            hash: "0101010101010101010101010101010101010101010101010101010101010101"
                .parse()
                .unwrap(),
            index: 0,
        };
        let unsigned = make_unsigned(&[p2pk_prevout], &[42_000]);
        let unsigned_for_checks = unsigned.clone();

        let mut p2pk_outpoints = HashSet::new();
        p2pk_outpoints.insert(p2pk_prevout);

        let prev_script = Builder::build_p2pkh(&AddressHashEnum::AddressHash(key_pair.public().address_hash()));
        let signed = sign_tx_with_p2pk(
            unsigned,
            &key_pair,
            prev_script,
            SignatureVersion::Base,
            fork_id,
            &p2pk_outpoints,
        )
        .unwrap();

        let script_sig = Script::from(signed.inputs[0].script_sig.to_vec());
        assert!(
            script_sig.get_instruction(1).is_none(),
            "P2PK scriptSig must contain signature only"
        );

        let sig_with_type = parse_sig_with_hashtype(&signed.inputs[0].script_sig);
        assert_eq!(sig_with_type.last().copied(), Some(sighash_type as u8));
        let der_sig = Signature::from(sig_with_type[..sig_with_type.len() - 1].to_vec());

        let p2pk_script = Builder::build_p2pk(key_pair.public());
        let sighash = unsigned_for_checks.signature_hash(
            0,
            unsigned_for_checks.inputs[0].amount,
            &p2pk_script,
            SignatureVersion::Base,
            sighash_type,
        );
        assert!(key_pair.public().verify(&sighash, &der_sig).unwrap());
    }

    #[test]
    fn sign_tx_with_p2pk_routes_mixed_inputs_correctly() {
        let key_pair = KeyPair::from_private(SECRET_1C.into()).unwrap();
        let fork_id = 0u32;
        let sighash_type = 1 | fork_id;

        let p2pk_prevout = OutPoint {
            hash: "0202020202020202020202020202020202020202020202020202020202020202"
                .parse()
                .unwrap(),
            index: 1,
        };
        let p2pkh_prevout = OutPoint {
            hash: "0303030303030303030303030303030303030303030303030303030303030303"
                .parse()
                .unwrap(),
            index: 2,
        };

        let unsigned = make_unsigned(&[p2pk_prevout, p2pkh_prevout], &[11_000, 22_000]);
        let unsigned_for_checks = unsigned.clone();

        let mut p2pk_outpoints = HashSet::new();
        p2pk_outpoints.insert(p2pk_prevout);

        let p2pkh_script = Builder::build_p2pkh(&AddressHashEnum::AddressHash(key_pair.public().address_hash()));
        let signed = sign_tx_with_p2pk(
            unsigned,
            &key_pair,
            p2pkh_script.clone(),
            SignatureVersion::Base,
            fork_id,
            &p2pk_outpoints,
        )
        .unwrap();

        // Input 0: P2PK => signature-only scriptSig
        let p2pk_script_sig = Script::from(signed.inputs[0].script_sig.to_vec());
        assert!(
            p2pk_script_sig.get_instruction(1).is_none(),
            "P2PK input must be signature-only"
        );
        let p2pk_sig_with_type = parse_sig_with_hashtype(&signed.inputs[0].script_sig);
        assert_eq!(p2pk_sig_with_type.last().copied(), Some(sighash_type as u8));
        let p2pk_der = Signature::from(p2pk_sig_with_type[..p2pk_sig_with_type.len() - 1].to_vec());
        let p2pk_prev_script = Builder::build_p2pk(key_pair.public());
        let p2pk_sighash = unsigned_for_checks.signature_hash(
            0,
            unsigned_for_checks.inputs[0].amount,
            &p2pk_prev_script,
            SignatureVersion::Base,
            sighash_type,
        );
        assert!(key_pair.public().verify(&p2pk_sighash, &p2pk_der).unwrap());

        // Input 1: P2PKH => signature + pubkey scriptSig
        let p2pkh_script_sig = Script::from(signed.inputs[1].script_sig.to_vec());
        let second_instr = p2pkh_script_sig
            .get_instruction(1)
            .expect("P2PKH scriptSig must contain second instruction")
            .expect("P2PKH scriptSig second instruction must decode");
        assert_eq!(
            second_instr.data.expect("P2PKH second instruction must push pubkey"),
            key_pair.public().to_vec().as_slice()
        );
        let p2pkh_sig_with_type = parse_sig_with_hashtype(&signed.inputs[1].script_sig);
        assert_eq!(p2pkh_sig_with_type.last().copied(), Some(sighash_type as u8));
        let p2pkh_der = Signature::from(p2pkh_sig_with_type[..p2pkh_sig_with_type.len() - 1].to_vec());
        let p2pkh_sighash = unsigned_for_checks.signature_hash(
            1,
            unsigned_for_checks.inputs[1].amount,
            &p2pkh_script,
            SignatureVersion::Base,
            sighash_type,
        );
        assert!(key_pair.public().verify(&p2pkh_sighash, &p2pkh_der).unwrap());
    }
}
