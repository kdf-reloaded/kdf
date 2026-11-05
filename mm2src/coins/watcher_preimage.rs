// SPDX-License-Identifier: GPL-2.0-or-later
//! Witness assembly shared by delegated legacy HTLC transactions.
//!
//! A claim template deliberately omits the secret push. Its signature commits
//! to the redeem script and transaction effects, so the watcher can add the
//! secret later without acquiring a signing key or changing the destination.

use crate::WatcherSpendKind;
use kdf_crypto::{dhash160, sha256};
use script::{Builder, Opcode, Script};

pub(crate) fn template_parts(script: &Script, kind: WatcherSpendKind) -> Result<(Vec<u8>, Script), String> {
    let instructions = script
        .iter()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    if instructions.len() != 3 {
        return Err("a watcher template must contain a signature, branch selector and redeem script".into());
    }
    let selector = match kind {
        WatcherSpendKind::Claim => Opcode::OP_0,
        WatcherSpendKind::Refund => Opcode::OP_1,
    };
    if instructions[1].opcode != selector {
        return Err("the watcher template selects the wrong HTLC branch".into());
    }
    let signature = instructions[0].data.ok_or("the watcher template has no signature")?;
    if !(9..=73).contains(&signature.len()) {
        return Err("the watcher template signature has an invalid length".into());
    }
    let redeem = instructions[2]
        .data
        .ok_or("the watcher template has no redeem script")?;
    Ok((signature.to_vec(), Script::from(redeem.to_vec())))
}

pub(crate) fn complete_claim_script(script: &Script, secret: &[u8], secret_hash: &[u8]) -> Result<Script, String> {
    if secret.len() != 32 {
        return Err("a swap secret must contain 32 bytes".into());
    }
    let matches = match secret_hash.len() {
        20 => dhash160(secret).as_slice() == secret_hash,
        32 => sha256(secret).as_slice() == secret_hash,
        _ => false,
    };
    if !matches {
        return Err("the revealed secret does not match the watcher hash lock".into());
    }
    let (signature, redeem) = template_parts(script, WatcherSpendKind::Claim)?;
    Ok(Builder::default()
        .push_data(&signature)
        .push_data(secret)
        .push_opcode(Opcode::OP_0)
        .push_data(&redeem)
        .into_script())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inserts_only_a_matching_secret_and_preserves_the_signature_and_redeem_script() {
        let signature = vec![0x30; 72];
        let redeem = vec![0x51; 80];
        let template = Builder::default()
            .push_data(&signature)
            .push_opcode(Opcode::OP_0)
            .push_data(&redeem)
            .into_script();
        let secret = [17; 32];
        let hash = dhash160(&secret).take();
        let claim = complete_claim_script(&template, &secret, &hash).unwrap();
        let parts = claim.iter().collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0].data, Some(signature.as_slice()));
        assert_eq!(parts[1].data, Some(secret.as_slice()));
        assert_eq!(parts[2].opcode, Opcode::OP_0);
        assert_eq!(parts[3].data, Some(redeem.as_slice()));
        assert!(complete_claim_script(&template, &[18; 32], &hash).is_err());
        assert!(complete_claim_script(&template, &[17; 31], &hash).is_err());
        assert!(complete_claim_script(&claim, &secret, &hash).is_err());
        assert!(template_parts(&template, WatcherSpendKind::Refund).is_err());
    }
}
