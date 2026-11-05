//! Pirate1 (Ironwood) withdrawal destinations that need no coin (CRD ch.39 T39.6.5b, T39.6.5d (the
//! mainnet fixture's anchor and flags), and the
//! fee rule of R39.6.5f / the fee derivation of R39.6.5g on bytes).
//!
//! The tests that drive a real Light-mode coin over a wallet database (the built transaction, the
//! broadcast record, the built anchor and flags) are in `z_note_lock_tests/pirate1_withdraw.rs`,
//! next to the harness they use.

use super::z_coin_errors::GenTxError;
use super::z_coin_ops::{broadcast_fee_sat, ironwood_payment_gate};
use super::z_ironwood_read_tests::{DEPOSIT, WITHDRAW};
use super::z_pirate1::*;
use super::z_tx_read::read_tx;
use super::*;
use bech32::primitives::iter::{ByteIterExt, Fe32IterExt};
use bech32::{Bech32, Bech32m, Hrp};
use orchard::keys::{FullViewingKey, Scope, SpendingKey};

/// The raw 43 bytes of the address the vectors below encode: key `[7; 32]`, diversifier index 0.
pub(crate) const RAW_ADDRESS: &str =
    "4559029c0b5dbf941c5ad181a5fe8f45b34630f29d0c8dd8dc1cc3573386f416cb324133156d723df5e62d";
pub(crate) const MAIN_ADDRESS: &str =
    "pirate1g4vs98qttkleg8z66xq6tl50gke5vv8jn5xgmkxurnp4wvux7stvkvjpxv2k6u3a7hnz68an749";
pub(crate) const TEST_ADDRESS: &str =
    "pirate-test1g4vs98qttkleg8z66xq6tl50gke5vv8jn5xgmkxurnp4wvux7stvkvjpxv2k6u3a7hnz6cszju7";
pub(crate) const REGTEST_ADDRESS: &str =
    "pirate-regtest1g4vs98qttkleg8z66xq6tl50gke5vv8jn5xgmkxurnp4wvux7stvkvjpxv2k6u3a7hnz6mj3et4";

/// The root of the empty Ironwood tree, which is also the Ironwood anchor of the mainnet withdrawal
/// fixture (`WITHDRAW`): a wallet of the dictated chain that pays into Ironwood from Sapling anchors its
/// output-only bundle at it.
const EMPTY_TREE_ROOT: &str = "ae2935f1dfd8a24aed7c70df7de3a668eb7a49b1319880dde2bbd9031ae5d82f";

#[derive(Clone, Copy, Debug)]
enum Net {
    Main,
    Test,
    Regtest,
}

impl Net {
    const ALL: [Net; 3] = [Net::Main, Net::Test, Net::Regtest];

    fn sapling_hrp(self) -> &'static str {
        use zcash_protocol::constants::{mainnet, regtest, testnet};
        match self {
            Net::Main => mainnet::HRP_SAPLING_PAYMENT_ADDRESS,
            Net::Test => testnet::HRP_SAPLING_PAYMENT_ADDRESS,
            Net::Regtest => regtest::HRP_SAPLING_PAYMENT_ADDRESS,
        }
    }

    fn spending_key_hrp(self) -> &'static str {
        use zcash_protocol::constants::{mainnet, regtest, testnet};
        match self {
            Net::Main => mainnet::HRP_SAPLING_EXTENDED_SPENDING_KEY,
            Net::Test => testnet::HRP_SAPLING_EXTENDED_SPENDING_KEY,
            Net::Regtest => regtest::HRP_SAPLING_EXTENDED_SPENDING_KEY,
        }
    }

    fn pirate1_hrp(self) -> &'static str {
        match self {
            Net::Main => PIRATE1_HRP_MAIN,
            Net::Test => PIRATE1_HRP_TEST,
            Net::Regtest => PIRATE1_HRP_REGTEST,
        }
    }

    fn address(self) -> &'static str {
        match self {
            Net::Main => MAIN_ADDRESS,
            Net::Test => TEST_ADDRESS,
            Net::Regtest => REGTEST_ADDRESS,
        }
    }
}

/// Parameters of a network's shape. Sapling activates at height 2, so no built-in Ironwood time applies
/// (R39.6.4d withholds it); `tweak` adds the Ironwood members a case needs.
fn params(net: Net, tweak: impl FnOnce(&mut serde_json::Value)) -> ZcoinConsensusParams {
    let mut cfg = json!({
        "overwinter_activation_height": 1,
        "sapling_activation_height": 2,
        "blossom_activation_height": null,
        "heartwood_activation_height": null,
        "canopy_activation_height": 2,
        "coin_type": 133,
        "hrp_sapling_extended_spending_key": net.spending_key_hrp(),
        "hrp_sapling_extended_full_viewing_key": "zxviews",
        "hrp_sapling_payment_address": net.sapling_hrp(),
        "b58_pubkey_address_prefix": [0x1c, 0xb8],
        "b58_script_address_prefix": [0x1c, 0xbd]
    });
    tweak(&mut cfg);
    serde_json::from_value(cfg).unwrap()
}

/// A coin of `net` with an Ironwood pool (a configured activation height).
fn pool_params(net: Net) -> ZcoinConsensusParams { params(net, |cfg| cfg["ironwood_activation_height"] = json!(10)) }

fn raw_address() -> [u8; 43] { <[u8; 43]>::try_from(hex::decode(RAW_ADDRESS).unwrap().as_slice()).unwrap() }

fn encode_with<Ck: bech32::Checksum>(hrp: &str, data: &[u8]) -> String {
    bech32::encode::<Ck>(Hrp::parse(hrp).unwrap(), data).unwrap()
}

fn orchard_address() -> orchard::Address {
    Option::from(orchard::Address::from_raw_address_bytes(&raw_address())).unwrap()
}

/// The vectors are what the encoder produces for a real Orchard-type address, so a changed encoder or a
/// changed address shows here first.
#[test]
fn t39_6_5b_the_vectors_are_the_bech32_encoding_of_a_real_address() {
    let sk = SpendingKey::from_bytes([7u8; 32]).unwrap();
    let address = FullViewingKey::from(&sk).address_at(0u32, Scope::External);
    assert_eq!(address.to_raw_address_bytes(), raw_address());
    for net in Net::ALL {
        assert_eq!(encode_with::<Bech32>(net.pirate1_hrp(), &raw_address()), net.address());
    }
}

/// T39.6.5b: a valid address is accepted on a coin of its own network, and the bytes it decodes to are the
/// address's.
#[test]
fn t39_6_5b_a_valid_address_is_accepted_on_a_coin_of_its_own_network() {
    for net in Net::ALL {
        let params = pool_params(net);
        match classify(&params, "ARRR", net.address()) {
            Pirate1Verdict::Address(address) => assert_eq!(address.to_raw_address_bytes(), raw_address()),
            other => panic!("{:?}: {:?}", net, other),
        }
        match decode_destination(&params, "ARRR", net.address()) {
            Ok(WithdrawDestination::Ironwood(address)) => assert_eq!(address, orchard_address()),
            other => panic!("{:?}: {:?}", net, other),
        }
    }
    // An all-uppercase string is the same Bech32 string (BIP 173).
    let params = pool_params(Net::Main);
    assert!(matches!(
        classify(&params, "ARRR", &MAIN_ADDRESS.to_ascii_uppercase()),
        Pirate1Verdict::Address(_)
    ));
}

/// T39.6.5b: each address is refused, with the reason, on a coin of another network.
#[test]
fn t39_6_5b_each_address_is_refused_on_a_coin_of_another_network() {
    for address_net in Net::ALL {
        for coin_net in Net::ALL {
            if matches!(
                (address_net, coin_net),
                (Net::Main, Net::Main) | (Net::Test, Net::Test) | (Net::Regtest, Net::Regtest)
            ) {
                continue;
            }
            let verdict = classify(&pool_params(coin_net), "ARRR", address_net.address());
            let Pirate1Verdict::Refused(Pirate1AddressError::OtherNetwork { found, expected, .. }) = verdict else {
                panic!("{:?} on {:?}: {:?}", address_net, coin_net, verdict);
            };
            assert_eq!(
                (found.as_str(), expected.as_str()),
                (address_net.pirate1_hrp(), coin_net.pirate1_hrp())
            );
        }
    }
    // The refusal reaches `validateaddress` and a withdrawal with the cause named.
    let refusal = decode_destination(&pool_params(Net::Test), "ARRR", MAIN_ADDRESS).unwrap_err();
    assert!(
        refusal.validate_reason().contains("belongs to another network"),
        "{}",
        refusal.validate_reason()
    );
    assert!(
        refusal.validate_reason().contains("pirate-test"),
        "{}",
        refusal.validate_reason()
    );
}

fn refusal_of(params: &ZcoinConsensusParams, address: &str) -> Pirate1AddressError {
    match classify(params, "ARRR", address) {
        Pirate1Verdict::Refused(cause) => cause,
        other => panic!("{:?} was not refused: {:?}", address, other),
    }
}

/// T39.6.5b: each defect is refused on its own, with its own reason.
#[test]
fn t39_6_5b_each_defect_is_refused_with_its_reason() {
    let params = pool_params(Net::Main);

    // A bad checksum: the last character changed.
    let mut bad = MAIN_ADDRESS.to_owned();
    let last = if bad.ends_with('q') { 'p' } else { 'q' };
    bad.pop();
    bad.push(last);
    assert_eq!(refusal_of(&params, &bad), Pirate1AddressError::BadChecksum);

    // A Bech32m checksum over the very same bytes.
    let bech32m = encode_with::<Bech32m>(PIRATE1_HRP_MAIN, &raw_address());
    assert_eq!(refusal_of(&params, &bech32m), Pirate1AddressError::Bech32m);

    // A 42-byte and a 44-byte data part, each with a valid classic checksum.
    for len in [42usize, 44] {
        let mut data = raw_address().to_vec();
        data.resize(len, 0x11);
        let short_or_long = encode_with::<Bech32>(PIRATE1_HRP_MAIN, &data);
        assert_eq!(
            refusal_of(&params, &short_or_long),
            Pirate1AddressError::DataLength(len)
        );
    }

    // 43 bytes that are not a valid Orchard-type address: the transmission key is not a point.
    let mut not_a_point = raw_address();
    not_a_point[11..].fill(0xff);
    assert_eq!(
        refusal_of(&params, &encode_with::<Bech32>(PIRATE1_HRP_MAIN, &not_a_point)),
        Pirate1AddressError::NotAnOrchardAddress
    );

    // The padding bit of the last data character set, with a checksum that is valid for that string.
    let hrp = Hrp::parse(PIRATE1_HRP_MAIN).unwrap();
    let mut fes: Vec<_> = raw_address().iter().copied().bytes_to_fes().collect();
    let last = fes.len() - 1;
    fes[last] = fes[last] + bech32::Fe32::try_from(1u8).unwrap();
    let padded: String = fes.iter().copied().with_checksum::<Bech32>(&hrp).chars().collect();
    assert_ne!(padded, MAIN_ADDRESS);
    assert_eq!(refusal_of(&params, &padded), Pirate1AddressError::NonCanonical);

    // Mixed case is not a Bech32 string.
    let mixed = format!("P{}", &MAIN_ADDRESS[1..]);
    assert!(matches!(refusal_of(&params, &mixed), Pirate1AddressError::Malformed(_)));

    // Every refusal carries a reason a person can read, on `validateaddress` and on a withdrawal.
    for bad_address in [&bad, &bech32m] {
        let refusal = decode_destination(&params, "ARRR", bad_address).unwrap_err();
        assert!(
            refusal.validate_reason().contains("Pirate1"),
            "{}",
            refusal.validate_reason()
        );
        let reason = refusal.withdraw_reason();
        assert!(
            reason.contains("Sapling address of ARRR") && reason.contains("Pirate1 (Ironwood)"),
            "{reason}"
        );
    }
    assert!(refusal_of(&params, &bech32m).to_string().contains("Bech32m"));
    assert!(Pirate1AddressError::DataLength(42).to_string().contains("exactly 43"));
}

/// T39.6.5b: a configured `hrp_ironwood_payment_address` replaces the built-in prefix.
#[test]
fn t39_6_5b_a_configured_prefix_replaces_the_builtin_one() {
    let custom = "pirate-lab";
    let params = params(Net::Main, |cfg| {
        cfg["ironwood_activation_height"] = json!(10);
        cfg["hrp_ironwood_payment_address"] = json!(custom);
    });
    assert_eq!(pirate1_hrp(&params), custom);
    let own = encode_with::<Bech32>(custom, &raw_address());
    assert!(matches!(classify(&params, "ARRR", &own), Pirate1Verdict::Address(_)));
    // The built-in prefix of the same network is now a prefix of another network.
    assert!(matches!(
        refusal_of(&params, MAIN_ADDRESS),
        Pirate1AddressError::OtherNetwork { .. }
    ));
    // Without the override the custom prefix is nothing the coin knows: the Sapling answer stands.
    assert_eq!(
        classify(&pool_params(Net::Main), "ARRR", &own),
        Pirate1Verdict::NotPirate1
    );
    // The members are optional in both directions.
    assert_eq!(pirate1_hrp(&pool_params(Net::Regtest)), PIRATE1_HRP_REGTEST);
}

/// T39.6.5b / R39.6.5a: only a coin with an Ironwood pool accepts a Pirate1 address; what counts as a
/// pool is a known activation time or height, configured, built-in or discovered, and
/// `ironwood_gate_disabled` does not change it.
#[test]
fn t39_6_5b_a_coin_without_an_ironwood_pool_refuses_every_pirate1_address() {
    for net in Net::ALL {
        let params = params(net, |_| {});
        assert!(!has_ironwood_pool(&params, "ARRR"));
        assert_eq!(
            refusal_of(&params, net.address()),
            Pirate1AddressError::NoIronwoodPool {
                coin: "ARRR".to_owned()
            }
        );
    }
    // The refusal names the coin; `validateaddress` and a withdrawal both carry it.
    let refusal = decode_destination(&params(Net::Main, |_| {}), "ARRR", MAIN_ADDRESS).unwrap_err();
    assert!(
        refusal.validate_reason().contains("ARRR has no Ironwood pool"),
        "{}",
        refusal.validate_reason()
    );

    // A pool: a configured height, a configured time, the built-in time of the real chain, a discovered
    // height; and the gate switch changes none of them.
    assert!(has_ironwood_pool(
        &params(Net::Main, |c| c["ironwood_activation_height"] = json!(10)),
        "ARRR"
    ));
    assert!(has_ironwood_pool(
        &params(Net::Main, |c| c["ironwood_activation_time"] = json!(1_791_054_000u32)),
        "ARRR"
    ));
    assert!(has_ironwood_pool(
        &params(Net::Main, |c| {
            c["sapling_activation_height"] = json!(152_855);
        }),
        "ARRR"
    ));
    assert!(!has_ironwood_pool(
        &params(Net::Main, |c| c["sapling_activation_height"] = json!(152_855)),
        "OTHER"
    ));
    let discovered = params(Net::Main, |_| {});
    discovered.record_discovered_ironwood_height(77);
    assert!(has_ironwood_pool(&discovered, "ARRR"));
    let gated = params(Net::Main, |c| {
        c["ironwood_activation_height"] = json!(10);
        c["ironwood_gate_disabled"] = json!(true);
    });
    assert!(has_ironwood_pool(&gated, "ARRR"));
    assert!(matches!(
        classify(&gated, "ARRR", MAIN_ADDRESS),
        Pirate1Verdict::Address(_)
    ));
}

/// T39.6.5b: a Sapling address of the coin stays what it was, and the strings that are neither form keep
/// the Sapling answer; the withdrawal reason names both accepted forms.
#[test]
fn t39_6_5b_sapling_strings_keep_their_answers() {
    for net in Net::ALL {
        let params = pool_params(net);
        let sapling = encode_payment_address(
            net.sapling_hrp(),
            &ExtendedSpendingKey::master(&[99; 32]).default_address().1,
        );
        assert!(matches!(
            decode_destination(&params, "ARRR", &sapling),
            Ok(WithdrawDestination::Sapling(_))
        ));
        // A corrupted checksum, a Sapling-shaped string with a Pirate1 prefix, and plain garbage.
        let corrupted = format!("{}q", &sapling[..sapling.len() - 1]);
        for bad in [corrupted.as_str(), "not an address", ""] {
            let refusal = decode_destination(&params, "ARRR", bad).unwrap_err();
            assert!(
                refusal.validate_reason().ends_with("on decode_payment_address"),
                "{}",
                refusal.validate_reason()
            );
            let reason = refusal.withdraw_reason();
            assert!(
                reason.contains("Sapling address of ARRR") && reason.contains(net.pirate1_hrp()),
                "{reason}"
            );
        }
    }
    // A coin without a pool names only the form it accepts, as it always did.
    let refusal = decode_destination(&params(Net::Main, |_| {}), "ARRR", "not an address").unwrap_err();
    assert!(
        !refusal.withdraw_reason().contains("Pirate1"),
        "{}",
        refusal.withdraw_reason()
    );
}

/// T39.5A.a (parity record, not asserted): for the strings used above -- `MAIN_ADDRESS`, `TEST_ADDRESS`,
/// `REGTEST_ADDRESS` and the defective ones -- the reference contract of §39.5A answers `is_valid: false`
/// on `validateaddress` (R39.5A.1: only a Bech32 Sapling address under the Zcash main-network prefix `zs`
/// decodes) and `InvalidAddress` on the task route of `withdraw`, before any note selection (R39.5A.2).
/// This project answers `is_valid: true` for a valid Pirate1 address of the coin's network and builds the
/// transaction (R39.6.5b, R39.6.5c); the divergence is additive and local, and is recorded in
/// `docs/GLEEC_COMPATIBILITY.md`. There is no reference binary here, so nothing below asserts the
/// reference side.
#[test]
fn t39_5a_a_the_divergence_from_the_reference_is_documented_where_it_is_tested() {
    // What is asserted is this project's side of the record: the three valid strings are accepted.
    for net in Net::ALL {
        assert!(decode_destination(&pool_params(net), "ARRR", net.address()).is_ok());
    }
}

/// R39.6.5f: a transaction with an Ironwood output pays 10,000 base units whatever the coin's fee says;
/// one without keeps the configured fee.
#[test]
fn t39_6_5f_the_fee_is_fixed_for_an_ironwood_output_and_configured_otherwise() {
    assert_eq!(IRONWOOD_PAYMENT_FEE_SAT, 10_000);
    for configured in [0, 100, 1_000, 10_000, 250_000] {
        assert_eq!(fee_sat(true, configured), 10_000);
        assert_eq!(fee_sat(false, configured), configured);
    }
}

// ---------------------------------------------------------------------------------------------
// T39.6.5d: the Ironwood anchor and flags
// ---------------------------------------------------------------------------------------------

/// The Ironwood anchor and flags of a transaction: (anchor is the empty-tree root, spends enabled,
/// outputs enabled).
pub(crate) fn ironwood_anchor_and_flags(tx: &ZTransaction) -> (bool, bool, bool) {
    let bundle = tx.ironwood_bundle().expect("an Ironwood bundle");
    (
        bundle.anchor().to_bytes() == orchard::Anchor::empty_tree().to_bytes(),
        bundle.flags().spends_enabled(),
        bundle.flags().outputs_enabled(),
    )
}

/// T39.6.5d: the mainnet fixture of T39.6.5a that pays from Sapling into Ironwood (height 4198585) is an
/// output-only bundle anchored at the root of the empty tree: the form a Pirate1 withdrawal builds. The
/// mainnet deposit, which spends an Ironwood note, is the other form: spends enabled, a real anchor. The
/// built transaction is compared with the first in `pirate1_withdraw` (T39.6.5c, T39.6.5d).
#[test]
fn t39_6_5d_the_mainnet_payment_into_ironwood_is_output_only_at_the_empty_tree_root() {
    let withdraw = read_tx(&WITHDRAW.bytes()).unwrap();
    assert_eq!(
        hex::encode(withdraw.ironwood_bundle().unwrap().anchor().to_bytes()),
        EMPTY_TREE_ROOT
    );
    assert_eq!(ironwood_anchor_and_flags(&withdraw), (true, false, true));

    let deposit = read_tx(&DEPOSIT.bytes()).unwrap();
    assert_eq!(ironwood_anchor_and_flags(&deposit), (false, true, true));
}

// ---------------------------------------------------------------------------------------------
// R39.6.5d: the gate before any note is selected
// ---------------------------------------------------------------------------------------------

#[test]
fn t39_6_5c_a_target_height_below_the_activation_height_is_refused_before_note_selection() {
    let refused = |light, activation, target| ironwood_payment_gate("ARRR", light, activation, target);
    assert!(
        refused(true, Some(100), 100).is_ok(),
        "the activation height itself builds version 6"
    );
    assert!(refused(true, Some(100), 4_000_000).is_ok());
    let below = refused(true, Some(100), 99).unwrap_err().to_string();
    assert!(
        below.contains("ARRR") && below.contains("below the Ironwood activation height 100"),
        "{below}"
    );
    assert!(
        below.contains("version-4 transaction has no Ironwood bundle"),
        "{below}"
    );
    let unknown = refused(true, None, 99).unwrap_err();
    assert!(matches!(unknown, GenTxError::IronwoodPaymentRefused { .. }));
    assert!(unknown.to_string().contains("not known"), "{unknown}");
    let native = refused(false, Some(100), 4_000_000).unwrap_err().to_string();
    assert!(
        native.contains("ARRR") && native.contains("native mode builds no transactions"),
        "{native}"
    );
}

// ---------------------------------------------------------------------------------------------
// R39.6.5g: the fee read from the bytes
// ---------------------------------------------------------------------------------------------

/// R39.6.5g on the mainnet withdrawal: the fee is the sum of the Sapling, Orchard and Ironwood value
/// balances less the transparent outputs, computed here from the fixture's own bundles.
#[test]
fn t39_6_5g_the_fee_is_read_from_the_value_balances_of_the_bytes() {
    let tx = read_tx(&WITHDRAW.bytes()).unwrap();
    let sapling = i64::from(*tx.sapling_bundle().unwrap().value_balance());
    let ironwood = i64::from(*tx.ironwood_bundle().unwrap().value_balance());
    assert!(tx.orchard_bundle().is_none());
    assert!(tx
        .transparent_bundle()
        .is_none_or(|b| b.vin.is_empty() && b.vout.is_empty()));
    // The fixture's own balances: one Sapling note leaves, part of it enters Ironwood, the rest is
    // change and the fee. The derived fee is their sum.
    let expected = u64::try_from(sapling + ironwood).unwrap();
    assert_eq!(broadcast_fee_sat(&tx), Some(expected));
    assert_eq!(
        (sapling, ironwood, expected),
        (11_204_950_000, -11_204_940_000, 10_000),
        "112.0495 ARRR leave Sapling, 112.0494 enter Ironwood, the fee is 0.0001 ARRR"
    );
}

/// R39.6.5g: a transaction with transparent inputs is the one left unrecorded, because the bytes do not
/// carry the values of its inputs. The recorded HTLC refund of the mainnet chain spends a transparent
/// output.
#[test]
fn t39_6_5g_a_transaction_with_transparent_inputs_has_no_derivable_fee() {
    let refund =
        read_tx(&hex::decode(include_str!("test-fixtures/arrr-htlc-refund-0496f6d3.hex").trim()).unwrap()).unwrap();
    assert!(refund.transparent_bundle().is_some_and(|bundle| !bundle.vin.is_empty()));
    assert_eq!(broadcast_fee_sat(&refund), None);
}
