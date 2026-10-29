//! The per-swap HTLC key of a shielded coin, derived from the coin's transparent key (CRD ch.39
//! R39.5.6-R39.5.11). Compiled on every target: it is pure key arithmetic.

use crypto::privkey::key_pair_from_secret;
use kdf_crypto::dhash256;
use keys::KeyPair;

use super::ZCoin;

/// Derives the per-swap HTLC key pair of a shielded coin (CRD ch.39 R39.5.8).
///
/// `base_key` is the coin's activated transparent secp256k1 key (R39.5.9) and `swap_unique_input`
/// the role's input of R39.5.7: the maker's secret-hash bytes, or the taker's 16 raw uuid bytes.
///
/// 1. `m` = SHA-256(SHA-256(input)), used as the 32-byte ECDSA message as it is.
/// 2. `s` = the DER encoding of the ECDSA signature of `m` under the base key (RFC 6979 nonce, no
///    extra entropy, no low-R grinding, low-S normalization, no sighash byte).
/// 3. `h` = SHA-256(SHA-256(`s`)).
/// 4. The private key is `h` with byte 0 `&= 0xF8`, byte 31 `&= 0x7F` and `|= 0x40`, read as a
///    big-endian scalar.
///
/// # Errors
///
/// Fails when the base key is not a valid secp256k1 secret.
pub fn derive_swap_htlc_key_pair(base_key: &KeyPair, swap_unique_input: &[u8]) -> Result<KeyPair, String> {
    let message = secp256k1::Message::from_digest(dhash256(swap_unique_input).take());
    let secret = secp256k1::SecretKey::from_slice(base_key.private().secret.as_slice())
        .map_err(|e| ERRL!("The coin's transparent key is not a valid secp256k1 secret: {}", e))?;
    let signature = secp256k1::Secp256k1::signing_only().sign_ecdsa(&message, &secret);
    let mut scalar = dhash256(&signature.serialize_der()).take();
    scalar[0] &= 0xF8;
    scalar[31] &= 0x7F;
    scalar[31] |= 0x40;
    key_pair_from_secret(&scalar).map_err(|e| ERRL!("The derived HTLC key is not a valid secp256k1 secret: {}", e))
}

impl ZCoin {
    /// The per-swap HTLC key pair of this coin for `swap_unique_input`, derived from its activated
    /// transparent key (R39.5.9): the seed's key under the seed policy, the key at the coins-file
    /// `derivation_path` under the HD policy (R39.6.4).
    ///
    /// # Errors
    ///
    /// Fails when the coin holds no software key pair or the key is invalid.
    pub(super) fn derived_htlc_key_pair(&self, swap_unique_input: &[u8]) -> Result<KeyPair, String> {
        let base_key = try_s!(self.utxo_arc.priv_key_policy.key_pair_or_err());
        derive_swap_htlc_key_pair(base_key, swap_unique_input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc_command::get_my_address::PathToAddress;
    use crate::utxo::utxo_builder::derive_activated_hd_secret;
    use crate::z_coin::transparent_secret_for_policy;
    use keys::{Private, Public};
    use mm2_core::mm_ctx::MmCtxBuilder;
    use serde_json::{json, Value as Json};
    use std::str::FromStr;

    /// Test-only keys of swept throwaway wallets, and the keys the reference derived for them.
    const VECTORS: &str = include_str!("test-fixtures/arrr-htlc-key-derivation.json");

    /// Standard BIP39 test mnemonic.
    const TEST_MNEMONIC: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    fn vectors() -> Json { serde_json::from_str(VECTORS).unwrap() }

    fn base_key(node: &Json) -> KeyPair {
        let private = Private::from_str(node["base_wif"].as_str().unwrap()).unwrap();
        KeyPair::from_private(private).unwrap()
    }

    fn pub_hex(key: &KeyPair) -> String { hex::encode(key.public().to_vec()) }

    fn str_of<'a>(v: &'a Json, key: &str) -> &'a str { v[key].as_str().unwrap() }

    /// T39.5.a: the base key is the node's seed key, and the derived keys equal what the reference derived,
    /// bit for bit: the maker's from the 20 secret-hash bytes, the taker's from the 16 raw uuid bytes.
    #[test]
    fn derivation_equals_the_reference_vector() {
        let v = vectors();
        let secret_hash = hex::decode(str_of(&v, "secret_hash")).unwrap();
        let uuid = uuid::Uuid::from_str(str_of(&v, "uuid")).unwrap();

        for (role, input) in [("maker", secret_hash), ("taker", uuid.as_bytes().to_vec())] {
            let node = &v[role];
            let base = base_key(node);
            // The base key is the key behind `my_persistent_pub`, and a UTXO leg's key is that same key.
            assert_eq!(pub_hex(&base), str_of(node, "my_persistent_pub"), "{role} base key");
            assert_eq!(pub_hex(&base), str_of(node, "rin_htlc_pub"), "{role} UTXO leg key");

            let derived = derive_swap_htlc_key_pair(&base, &input).unwrap();
            assert_eq!(pub_hex(&derived), str_of(node, "arrr_htlc_pub"), "{role} ARRR leg key");
            assert_ne!(
                derived.public(),
                base.public(),
                "the shielded leg's key is not the persistent key"
            );
            // Deterministic: a restart derives the same key again.
            assert_eq!(
                derive_swap_htlc_key_pair(&base, &input).unwrap().private().secret,
                derived.private().secret
            );
        }
    }

    /// T39.5.a, interop fact (i): inputs whose raw ECDSA `s` is above n/2, so low-S normalization changes the
    /// signature. These two cases are computed from the CRD rule (R39.5.8), not observed from the reference.
    #[test]
    fn low_s_normalization_is_applied() {
        let v = vectors();
        let cases = &v["low_s_cases"];
        let base = base_key(&v[str_of(cases, "base")]);
        let taker_uuid = uuid::Uuid::from_str(str_of(&cases["taker_uuid"], "input")).unwrap();
        let maker_secret_hash = hex::decode(str_of(&cases["maker_secret_hash"], "input")).unwrap();

        for (name, input) in [
            ("taker_uuid", taker_uuid.as_bytes().to_vec()),
            ("maker_secret_hash", maker_secret_hash),
        ] {
            let derived = pub_hex(&derive_swap_htlc_key_pair(&base, &input).unwrap());
            assert_eq!(derived, str_of(&cases[name], "expected_htlc_pub"), "{name}");
            assert_ne!(derived, str_of(&cases[name], "if_low_s_skipped"), "{name}");
        }
    }

    /// The clamp of R39.5.8 step 4 is visible on the derived scalar, read big-endian without reversal.
    #[test]
    fn derived_scalar_is_clamped() {
        let v = vectors();
        let derived = derive_swap_htlc_key_pair(&base_key(&v["maker"]), &[0x42; 20]).unwrap();
        let scalar = derived.private().secret.take();
        assert_eq!(scalar[0] & 0x07, 0);
        assert_eq!(scalar[31] & 0x80, 0);
        assert_eq!(scalar[31] & 0x40, 0x40);
    }

    fn internal_secret(ctx: &mm2_core::mm_ctx::MmArc) -> Vec<u8> {
        crypto::CryptoCtx::from_ctx(ctx)
            .unwrap()
            .mm2_internal_privkey_slice()
            .to_vec()
    }

    fn ctx_with_policy(hd: bool) -> mm2_core::mm_ctx::MmArc {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        if hd {
            crypto::CryptoCtx::init_with_global_hd_account(ctx.clone(), TEST_MNEMONIC).unwrap();
        } else {
            crypto::CryptoCtx::init_with_iguana_passphrase(ctx.clone(), TEST_MNEMONIC).unwrap();
        }
        ctx
    }

    /// R39.6.4 "Transparent key under HD", D39.5.a: for the same seed the seed policy and the HD policy give
    /// different transparent keys, and the HD one is the key at `m/44'/141'/0'/0/0`.
    #[test]
    fn hd_and_seed_policies_give_different_base_keys_and_the_hd_one_is_at_the_path() {
        let conf = json!({"coin": "ARRR", "derivation_path": "m/44'/141'"});

        let seed_ctx = ctx_with_policy(false);
        let seed_secret = internal_secret(&seed_ctx);
        let seed_key = transparent_secret_for_policy(&seed_ctx, &conf, &seed_secret).unwrap();
        // The seed policy changes nothing: the given secret is the coin's key.
        assert_eq!(seed_key, seed_secret);

        let hd_ctx = ctx_with_policy(true);
        let hd_key = transparent_secret_for_policy(&hd_ctx, &conf, &seed_secret).unwrap();
        assert_ne!(hd_key, seed_key);

        // Expected: the key at m/44'/141'/0'/0/0 of the same mnemonic, independently spelled out.
        let crypto_ctx = crypto::CryptoCtx::from_ctx(&hd_ctx).unwrap();
        let hd = match crypto_ctx.key_pair_policy() {
            crypto::KeyPairPolicy::GlobalHDAccount(hd) => hd.clone(),
            _ => panic!("expected the HD policy"),
        };
        let path = bip32::DerivationPath::from_str("m/44'/141'/0'/0/0").unwrap();
        assert_eq!(hd_key, hd.derive_secp256k1_secret(&path).unwrap().as_slice().to_vec());
        // And it is the derivation the UTXO builder uses for a UTXO coin's default address.
        let coin_path = crypto::HDPathToCoin::from_str("m/44'/141'").unwrap();
        assert_eq!(
            hd_key,
            derive_activated_hd_secret(&hd, &coin_path, &PathToAddress::default())
                .unwrap()
                .as_slice()
                .to_vec()
        );
    }

    /// R39.6.4: under the HD policy a coin entry without `derivation_path` fails the build; the seed policy
    /// does not need one.
    #[test]
    fn hd_policy_without_a_derivation_path_is_a_build_error() {
        let conf = json!({"coin": "ARRR"});
        let hd_ctx = ctx_with_policy(true);
        let err = transparent_secret_for_policy(&hd_ctx, &conf, &[1; 32]).unwrap_err();
        assert!(
            err.to_string().contains("derivation_path is required"),
            "{}",
            err.to_string()
        );

        let seed_ctx = ctx_with_policy(false);
        assert!(transparent_secret_for_policy(&seed_ctx, &conf, &[1; 32]).is_ok());
    }

    /// The base key of the swap key follows the policy: the same swap input derives a different HTLC key
    /// from the HD base key than from the seed base key.
    #[test]
    fn the_htlc_key_follows_the_policys_base_key() {
        let conf = json!({"coin": "ARRR", "derivation_path": "m/44'/141'"});
        let seed_ctx = ctx_with_policy(false);
        let seed_secret = internal_secret(&seed_ctx);
        let hd_ctx = ctx_with_policy(true);
        let hd_secret = transparent_secret_for_policy(&hd_ctx, &conf, &seed_secret).unwrap();
        let input = [9u8; 20];
        let from_seed = derive_swap_htlc_key_pair(&key_pair_from_secret(&seed_secret).unwrap(), &input).unwrap();
        let from_hd = derive_swap_htlc_key_pair(&key_pair_from_secret(&hd_secret).unwrap(), &input).unwrap();
        assert_ne!(from_seed.public(), from_hd.public());
    }

    /// T39.5.b: the key re-derived after a restart signs a refund of the HTLC. The refund spends the P2SH
    /// output of the redeem script rebuilt from the derived key (as `send_maker_refunds_payment` rebuilds it
    /// from the key it is given); the builder signs only with a key whose public half is the one in the
    /// redeem script's refund branch, and refuses a signing set that holds another key.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_rederived_key_signs_a_refund_of_the_htlc() {
        use crate::utxo::utxo_common::payment_script;
        use crate::z_coin::z_v6_construction_tests::{params, standard_config, NoProver};
        use crate::z_coin::{FixedFeeRule, TransparentSigningSet, TxOut, ZTxBuilder};
        use zcash_protocol::value::Zatoshis as Amount;
        use zcash_script::script::{Code, FromChain, Sig};
        use zcash_transparent::address::TransparentAddress;
        use zcash_transparent::bundle::OutPoint;

        let v = vectors();
        let base = base_key(&v["maker"]);
        let secret_hash = hex::decode(str_of(&v, "secret_hash")).unwrap();
        let counterparty = Public::from_slice(&hex::decode(str_of(&v["taker"], "arrr_htlc_pub")).unwrap()).unwrap();
        let time_lock = 1_793_235_835u32;

        // The key as it is after a restart: derived again from the saved input, not read from any record.
        let derived = derive_swap_htlc_key_pair(&base, &secret_hash).unwrap();
        let redeem = payment_script(time_lock, &secret_hash, derived.public(), &counterparty);
        let p2sh = TransparentAddress::ScriptHash(kdf_crypto::dhash160(&redeem).take());
        let payment_out = TxOut::new(Amount::const_from_u64(100_000), p2sh.script().into());

        let refund_with = |signing_key: &KeyPair| {
            let secret = secp256k1::SecretKey::from_slice(signing_key.private().secret.as_slice()).unwrap();
            let mut builder = ZTxBuilder::new(params(None), 100u32.into(), standard_config(None));
            builder.set_lock_time(time_lock);
            builder
                .add_kdf_p2sh_input(
                    secp256k1::PublicKey::from_slice(derived.public()).unwrap(),
                    OutPoint::new([5; 32], 0),
                    payment_out.clone(),
                    FromChain::parse(&Code(redeem.to_vec())).unwrap(),
                    Sig::parse(&Code(vec![0x51])).unwrap(),
                    0xffff_fffe,
                )
                .unwrap();
            builder.add_transparent_output_raw(TxOut::new(
                Amount::const_from_u64(99_000),
                TransparentAddress::PublicKeyHash([4; 20]).script().into(),
            ));
            let mut signing = TransparentSigningSet::new();
            signing.add_key(secret);
            builder.build(
                &signing,
                &[],
                &[],
                rand::rngs::OsRng,
                &NoProver,
                &NoProver,
                &FixedFeeRule::non_standard(Amount::const_from_u64(1_000)),
            )
        };

        let refund = refund_with(&derived)
            .expect("the derived key signs the refund")
            .into_transaction();
        assert_eq!(refund.lock_time(), time_lock);
        assert_eq!(refund.transparent_bundle().unwrap().vin[0].sequence(), 0xffff_fffe);
        assert!(!refund.transparent_bundle().unwrap().vin[0].script_sig().0 .0.is_empty());

        // Any other key (the persistent key, or the key of another wallet) cannot sign it.
        assert!(
            refund_with(&base).is_err(),
            "the persistent key must not sign the refund"
        );
    }
}
