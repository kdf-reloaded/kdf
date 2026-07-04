use crate::update_coins_config;

// ── HD Wallet Integration Tests ──────────────────────────────────────

mod hd_wallet_integration {
    use crate::PrivKeyBuildPolicy;
    use crypto::CryptoCtx;
    use mm2_core::mm_ctx::MmCtxBuilder;
    use std::str::FromStr;

    /// Standard BIP39 test mnemonic ("abandon" x11 + "about"). Known test vector.
    const TEST_MNEMONIC: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    #[test]
    fn test_detect_priv_key_policy_returns_global_hd() {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        CryptoCtx::init_with_global_hd_account(ctx.clone(), TEST_MNEMONIC).expect("CryptoCtx HD init should succeed");

        let policy = PrivKeyBuildPolicy::detect_priv_key_policy(&ctx).expect("detect_priv_key_policy should succeed");

        assert!(
            matches!(policy, PrivKeyBuildPolicy::GlobalHDAccount(_)),
            "Expected GlobalHDAccount policy, got Iguana or Trezor"
        );
    }

    #[test]
    fn test_detect_priv_key_policy_returns_iguana() {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        CryptoCtx::init_with_iguana_passphrase(ctx.clone(), TEST_MNEMONIC)
            .expect("CryptoCtx Iguana init should succeed");

        let policy = PrivKeyBuildPolicy::detect_priv_key_policy(&ctx).expect("detect_priv_key_policy should succeed");

        assert!(
            matches!(policy, PrivKeyBuildPolicy::IguanaPrivKey(_)),
            "Expected IguanaPrivKey policy, got GlobalHDAccount or Trezor"
        );
    }

    /// R45.4.8 acceptance test: the `enable_hd` config field is the single source
    /// of truth that selects the startup key-pair policy. This mirrors the
    /// production branch in `lp_native_dex` and asserts both directions.
    #[test]
    fn test_enable_hd_config_selects_key_pair_policy() {
        // Truthy `enable_hd` → global-HD account.
        let ctx = MmCtxBuilder::new()
            .with_conf(serde_json::json!({"enable_hd": true}))
            .into_mm_arc();
        assert!(ctx.enable_hd(), "enable_hd should be true when config sets it");
        if ctx.enable_hd() {
            CryptoCtx::init_with_global_hd_account(ctx.clone(), TEST_MNEMONIC)
                .expect("CryptoCtx HD init should succeed");
        } else {
            CryptoCtx::init_with_iguana_passphrase(ctx.clone(), TEST_MNEMONIC)
                .expect("CryptoCtx Iguana init should succeed");
        }
        let policy = PrivKeyBuildPolicy::detect_priv_key_policy(&ctx).expect("detect_priv_key_policy should succeed");
        assert!(
            matches!(policy, PrivKeyBuildPolicy::GlobalHDAccount(_)),
            "Expected GlobalHDAccount policy when enable_hd is true"
        );

        // Absent `enable_hd` → Iguana single-key identity.
        let ctx = MmCtxBuilder::new().with_conf(serde_json::json!({})).into_mm_arc();
        assert!(!ctx.enable_hd(), "enable_hd should default to false when absent");
        if ctx.enable_hd() {
            CryptoCtx::init_with_global_hd_account(ctx.clone(), TEST_MNEMONIC)
                .expect("CryptoCtx HD init should succeed");
        } else {
            CryptoCtx::init_with_iguana_passphrase(ctx.clone(), TEST_MNEMONIC)
                .expect("CryptoCtx Iguana init should succeed");
        }
        let policy = PrivKeyBuildPolicy::detect_priv_key_policy(&ctx).expect("detect_priv_key_policy should succeed");
        assert!(
            matches!(policy, PrivKeyBuildPolicy::IguanaPrivKey(_)),
            "Expected IguanaPrivKey policy when enable_hd is absent"
        );
    }

    #[test]
    fn test_hd_policy_gives_deterministic_context() {
        let ctx1 = MmCtxBuilder::default().into_mm_arc();
        CryptoCtx::init_with_global_hd_account(ctx1.clone(), TEST_MNEMONIC).unwrap();
        let policy1 = PrivKeyBuildPolicy::detect_priv_key_policy(&ctx1).unwrap();

        let ctx2 = MmCtxBuilder::default().into_mm_arc();
        CryptoCtx::init_with_global_hd_account(ctx2.clone(), TEST_MNEMONIC).unwrap();
        let policy2 = PrivKeyBuildPolicy::detect_priv_key_policy(&ctx2).unwrap();

        // Extract the GlobalHDAccountArc from both and verify same root key
        match (policy1, policy2) {
            (PrivKeyBuildPolicy::GlobalHDAccount(hd1), PrivKeyBuildPolicy::GlobalHDAccount(hd2)) => {
                assert_eq!(
                    hd1.root_seed_bytes(),
                    hd2.root_seed_bytes(),
                    "Same mnemonic should produce same root seed"
                );
            },
            _ => panic!("Both should be GlobalHDAccount"),
        }
    }

    #[test]
    fn test_hd_derivation_produces_correct_key_for_known_path() {
        use crypto::DerivationPath;

        let ctx = MmCtxBuilder::default().into_mm_arc();
        CryptoCtx::init_with_global_hd_account(ctx.clone(), TEST_MNEMONIC).unwrap();
        let policy = PrivKeyBuildPolicy::detect_priv_key_policy(&ctx).unwrap();

        let global_hd = match policy {
            PrivKeyBuildPolicy::GlobalHDAccount(hd) => hd,
            _ => panic!("Expected GlobalHDAccount"),
        };

        // Derive at m/44'/141'/0'/0/0 (KMD BIP44 path)
        let kmd_path = DerivationPath::from_str("m/44'/141'/0'/0/0").expect("valid path");
        let secret1 = global_hd
            .derive_secp256k1_secret(&kmd_path)
            .expect("derivation should succeed");

        // Derive at m/44'/0'/0'/0/0 (BTC BIP44 path)
        let btc_path = DerivationPath::from_str("m/44'/0'/0'/0/0").expect("valid path");
        let secret2 = global_hd
            .derive_secp256k1_secret(&btc_path)
            .expect("derivation should succeed");

        // Different coin types must produce different keys
        assert_ne!(
            secret1.as_slice(),
            secret2.as_slice(),
            "Different coin_type in BIP44 path must produce different keys"
        );

        // Same path must produce same key (deterministic)
        let secret1_again = global_hd.derive_secp256k1_secret(&kmd_path).unwrap();
        assert_eq!(secret1.as_slice(), secret1_again.as_slice());
    }

    #[test]
    fn test_hd_derived_key_produces_valid_address() {
        use crypto::DerivationPath;
        use keys::KeyPair;
        use keys::Private;

        let ctx = MmCtxBuilder::default().into_mm_arc();
        CryptoCtx::init_with_global_hd_account(ctx.clone(), TEST_MNEMONIC).unwrap();
        let policy = PrivKeyBuildPolicy::detect_priv_key_policy(&ctx).unwrap();

        let global_hd = match policy {
            PrivKeyBuildPolicy::GlobalHDAccount(hd) => hd,
            _ => panic!("Expected GlobalHDAccount"),
        };

        // Derive KMD key at m/44'/141'/0'/0/0
        let path = DerivationPath::from_str("m/44'/141'/0'/0/0").expect("valid path");
        let secret = global_hd.derive_secp256k1_secret(&path).unwrap();

        // Build a key pair from the derived secret
        let private = Private {
            prefix: 188, // KMD WIF prefix
            secret,
            compressed: true,
            checksum_type: kdf_crypto::ChecksumType::DSHA256,
        };
        let key_pair = KeyPair::from_private(private).expect("valid key pair from HD-derived secret");

        // Public key should be 33 bytes (compressed)
        assert_eq!(key_pair.public().len(), 33);

        // Address hash should be 20 bytes (RIPEMD160(SHA256(pubkey)))
        assert_eq!(key_pair.public().address_hash().len(), 20);
    }

    #[test]
    fn test_different_mnemonics_produce_different_keys() {
        use crypto::DerivationPath;

        let mnemonic_a = TEST_MNEMONIC;
        let mnemonic_b = "zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo wrong";

        let ctx_a = MmCtxBuilder::default().into_mm_arc();
        CryptoCtx::init_with_global_hd_account(ctx_a.clone(), mnemonic_a).unwrap();
        let policy_a = PrivKeyBuildPolicy::detect_priv_key_policy(&ctx_a).unwrap();

        let ctx_b = MmCtxBuilder::default().into_mm_arc();
        CryptoCtx::init_with_global_hd_account(ctx_b.clone(), mnemonic_b).unwrap();
        let policy_b = PrivKeyBuildPolicy::detect_priv_key_policy(&ctx_b).unwrap();

        let (hd_a, hd_b) = match (policy_a, policy_b) {
            (PrivKeyBuildPolicy::GlobalHDAccount(a), PrivKeyBuildPolicy::GlobalHDAccount(b)) => (a, b),
            _ => panic!("Both should be GlobalHDAccount"),
        };

        let path = DerivationPath::from_str("m/44'/141'/0'/0/0").unwrap();
        let secret_a = hd_a.derive_secp256k1_secret(&path).unwrap();
        let secret_b = hd_b.derive_secp256k1_secret(&path).unwrap();

        assert_ne!(
            secret_a.as_slice(),
            secret_b.as_slice(),
            "Different mnemonics must produce different keys at same path"
        );
    }

    /// CRD 05 R30 / Ch.38 R38.8 — software account extended-public-key derivation and canonical
    /// `xpub` serialisation. Derives the account-level extended pubkey from the in-memory BIP-32
    /// master for BIP-84 and BIP-44 account paths, with no `trezor_coin` config and no device.
    #[test]
    fn test_software_account_xpub_derivation_and_serialization() {
        use crypto::DerivationPath;

        let ctx = MmCtxBuilder::default().into_mm_arc();
        CryptoCtx::init_with_global_hd_account(ctx.clone(), TEST_MNEMONIC).unwrap();
        let global_hd = match PrivKeyBuildPolicy::detect_priv_key_policy(&ctx).unwrap() {
            PrivKeyBuildPolicy::GlobalHDAccount(hd) => hd,
            _ => panic!("Expected GlobalHDAccount"),
        };

        // BIP-84 account path m/84'/141'/0'.
        let segwit_account_path = DerivationPath::from_str("m/84'/141'/0'").expect("valid account path");
        let segwit_xpub = global_hd
            .derive_account_extended_pubkey(&segwit_account_path)
            .expect("software account xpub derivation should succeed");
        let segwit_xpub_str = segwit_xpub.to_string(bip32::Prefix::XPUB);

        // BIP-44 account path m/44'/141'/0'.
        let legacy_account_path = DerivationPath::from_str("m/44'/141'/0'").expect("valid account path");
        let legacy_xpub = global_hd
            .derive_account_extended_pubkey(&legacy_account_path)
            .expect("software account xpub derivation should succeed");
        let legacy_xpub_str = legacy_xpub.to_string(bip32::Prefix::XPUB);

        // Canonical serialisation: the `xpub` version prefix (0x0488B21E base58-encodes to "xpub").
        assert!(
            segwit_xpub_str.starts_with("xpub"),
            "BIP-84 account xpub must use the canonical xpub prefix, got: {}",
            segwit_xpub_str
        );
        assert!(
            legacy_xpub_str.starts_with("xpub"),
            "BIP-44 account xpub must use the canonical xpub prefix, got: {}",
            legacy_xpub_str
        );

        // Distinct purpose paths yield distinct account xpubs.
        assert_ne!(
            segwit_xpub_str, legacy_xpub_str,
            "BIP-84 and BIP-44 account paths must produce different account xpubs"
        );

        // Determinism: re-deriving the same path yields the identical xpub.
        let segwit_xpub_again = global_hd
            .derive_account_extended_pubkey(&segwit_account_path)
            .unwrap()
            .to_string(bip32::Prefix::XPUB);
        assert_eq!(
            segwit_xpub_str, segwit_xpub_again,
            "Account xpub derivation must be deterministic"
        );

        // The standalone software-source helper produces the same bytes as the context method.
        let helper_xpub =
            crypto::derive_secp256k1_extended_pubkey(global_hd.root_priv_key().clone(), &segwit_account_path)
                .unwrap()
                .to_string(bip32::Prefix::XPUB);
        assert_eq!(
            segwit_xpub_str, helper_xpub,
            "The free software-derivation helper must match the context method"
        );
    }

    /// CRD 05 R31 — extended-public-key source selection by key-pair policy. Under a software
    /// `GlobalHDAccount` policy the software source yields an account xpub; under `Iguana`
    /// policy there is no global-HD master, so the software source is unavailable (refused).
    #[test]
    fn test_software_xpub_source_selection_by_policy() {
        use crypto::DerivationPath;

        let account_path = DerivationPath::from_str("m/84'/141'/0'").unwrap();

        // GlobalHDAccount → software source available.
        let ctx_hd = MmCtxBuilder::default().into_mm_arc();
        CryptoCtx::init_with_global_hd_account(ctx_hd.clone(), TEST_MNEMONIC).unwrap();
        match PrivKeyBuildPolicy::detect_priv_key_policy(&ctx_hd).unwrap() {
            PrivKeyBuildPolicy::GlobalHDAccount(hd) => {
                hd.derive_account_extended_pubkey(&account_path)
                    .expect("software source must yield an account xpub under GlobalHDAccount policy");
            },
            _ => panic!("Expected GlobalHDAccount policy"),
        }

        // Iguana → no global-HD master, software source is unavailable.
        let ctx_iguana = MmCtxBuilder::default().into_mm_arc();
        CryptoCtx::init_with_iguana_passphrase(ctx_iguana.clone(), TEST_MNEMONIC).unwrap();
        assert!(
            matches!(
                PrivKeyBuildPolicy::detect_priv_key_policy(&ctx_iguana).unwrap(),
                PrivKeyBuildPolicy::IguanaPrivKey(_)
            ),
            "Iguana policy must not expose a software global-HD master"
        );
    }

    /// CRD 05 R29 / Ch.38 R38.8.1 — software-HD wallet identity. The digest is software-derived
    /// (no hardware handle), equals the daemon-wide `mm2_rmd160`, and is stable across
    /// constructions from the same mnemonic. Under Iguana policy no HD identity is available.
    #[test]
    fn test_software_hd_wallet_identity() {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        let crypto_ctx = CryptoCtx::init_with_global_hd_account(ctx.clone(), TEST_MNEMONIC).unwrap();

        let identity = crypto_ctx
            .global_hd_wallet_rmd160()
            .expect("software global-HD identity must be available under GlobalHDAccount policy");

        // (a) software-derived, no hardware handle present.
        assert!(
            crypto_ctx.hw_wallet_rmd160().is_none(),
            "no hardware-wallet identity should be present in software mode"
        );
        // (b) equals the internal key-pair public-key hash (== daemon-wide mm2_rmd160).
        assert_eq!(
            identity,
            crypto_ctx.mm2_internal_key_pair().public().address_hash(),
            "software HD identity must equal the internal pubkey hash"
        );
        assert_eq!(
            identity,
            *ctx.rmd160(),
            "software HD identity must equal the daemon-wide mm2_rmd160"
        );

        // (c) restart stability: a second construction from the same mnemonic yields the same digest.
        let ctx2 = MmCtxBuilder::default().into_mm_arc();
        let crypto_ctx2 = CryptoCtx::init_with_global_hd_account(ctx2.clone(), TEST_MNEMONIC).unwrap();
        assert_eq!(
            identity,
            crypto_ctx2.global_hd_wallet_rmd160().unwrap(),
            "same mnemonic must yield the same software HD identity across constructions"
        );

        // Iguana policy exposes no HD identity.
        let ctx_iguana = MmCtxBuilder::default().into_mm_arc();
        let crypto_ctx_iguana = CryptoCtx::init_with_iguana_passphrase(ctx_iguana.clone(), TEST_MNEMONIC).unwrap();
        assert!(
            crypto_ctx_iguana.global_hd_wallet_rmd160().is_none(),
            "Iguana policy must not expose a software HD identity"
        );
    }
}

#[test]
fn test_update_coin_config_success() {
    let conf = json!([
        {
            "coin": "RICK",
            "asset": "RICK",
            "fname": "RICK (TESTCOIN)",
            "rpcport": 25435,
            "txversion": 4,
            "overwintered": 1,
            "mm2": 1,
        },
        {
            "coin": "MORTY",
            "asset": "MORTY",
            "fname": "MORTY (TESTCOIN)",
            "rpcport": 16348,
            "txversion": 4,
            "overwintered": 1,
            "mm2": 1,
        },
        {
            "coin": "ETH",
            "name": "ethereum",
            "fname": "Ethereum",
            "etomic": "0x0000000000000000000000000000000000000000",
            "rpcport": 80,
            "mm2": 1,
            "required_confirmations": 3,
        },
        {
            "coin": "ARPA",
            "name": "arpa-chain",
            "fname": "ARPA Chain",
            // ARPA coin contains the protocol already. This coin should be skipped.
            "protocol": {
                "type":"ERC20",
                "protocol_data": {
                    "platform": "ETH",
                    "contract_address": "0xBA50933C268F567BDC86E1aC131BE072C6B0b71a"
                }
            },
            "rpcport": 80,
            "mm2": 1,
            "required_confirmations": 3,
        },
        {
            "coin": "JST",
            "name": "JST",
            "fname": "JST (TESTCOIN)",
            "etomic": "0x996a8ae0304680f6a69b8a9d7c6e37d65ab5ab56",
            "rpcport": 80,
            "mm2": 1,
        },
    ]);
    let actual = update_coins_config(conf).unwrap();
    let expected = json!([
        {
            "coin": "RICK",
            "asset": "RICK",
            "fname": "RICK (TESTCOIN)",
            "rpcport": 25435,
            "txversion": 4,
            "overwintered": 1,
            "mm2": 1,
            "protocol": {
                "type": "UTXO"
            },
        },
        {
            "coin": "MORTY",
            "asset": "MORTY",
            "fname": "MORTY (TESTCOIN)",
            "rpcport": 16348,
            "txversion": 4,
            "overwintered": 1,
            "mm2": 1,
            "protocol": {
                "type": "UTXO"
            },
        },
        {
            "coin": "ETH",
            "name": "ethereum",
            "fname": "Ethereum",
            "rpcport": 80,
            "mm2": 1,
            "required_confirmations": 3,
            "protocol": {
                "type": "ETH"
            },
        },
        {
            "coin": "ARPA",
            "name": "arpa-chain",
            "fname": "ARPA Chain",
            "protocol": {
                "type": "ERC20",
                "protocol_data": {
                    "platform": "ETH",
                    "contract_address": "0xBA50933C268F567BDC86E1aC131BE072C6B0b71a"
                }
            },
            "rpcport": 80,
            "mm2": 1,
            "required_confirmations": 3,
        },
        {
            "coin": "JST",
            "name": "JST",
            "fname": "JST (TESTCOIN)",
            "rpcport": 80,
            "mm2": 1,
            "protocol": {
                "type": "ERC20",
                "protocol_data": {
                    "platform": "ETH",
                    "contract_address": "0x996a8ae0304680f6a69b8a9d7c6e37d65ab5ab56"
                }
            },
        },
    ]);
    assert_eq!(actual, expected);
}

#[test]
fn test_update_coin_config_error_not_array() {
    let conf = json!({
        "coin": "RICK",
        "asset": "RICK",
        "fname": "RICK (TESTCOIN)",
        "rpcport": 25435,
        "txversion": 4,
        "overwintered": 1,
        "mm2": 1,
    });
    let error = update_coins_config(conf).err().unwrap();
    assert!(error.contains("Coins config must be an array"));
}

#[test]
fn test_update_coin_config_error_not_object() {
    let conf = json!([["Ford", "BMW", "Fiat"]]);
    let error = update_coins_config(conf).err().unwrap();
    assert!(error.contains("Expected object, found"));
}

#[test]
fn test_update_coin_config_invalid_etomic() {
    let conf = json!([
        {
            "coin": "JST",
            "name": "JST",
            "fname": "JST (TESTCOIN)",
            "etomic": 12345678,
            "rpcport": 80,
            "mm2": 1,
        },
    ]);
    let error = update_coins_config(conf).err().unwrap();
    assert!(error.contains("Expected etomic as string, found"));
}
