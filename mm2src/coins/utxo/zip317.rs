//! The ZIP 317 conventional fee for a transparent transaction (Reloaded's own rule; the specification is
//! <https://zips.z.cash/zip-0317>).
//!
//! The fee is `marginal_fee * max(grace_actions, logical_actions)`. For a transaction that has transparent
//! inputs and outputs only, the logical actions are
//! `max(ceil(input_bytes / 150), ceil(output_bytes / 34))`, where `input_bytes` is the total serialized size
//! of the transparent inputs and `output_bytes` the total serialized size of the transparent outputs.
//!
//! A coin follows this rule exactly when its entry is an overwintered, version 5 entry whose
//! `consensus_branch_id` is one of [`ZIP317_BRANCH_IDS`] (see [`conf_selects_zip317`]). The entry's `txfee` key
//! is then ignored for building.

use super::{parse_hex_encoded_u32, COMPRESSED_PUBKEY_LEN, MAX_DER_SIGNATURE_LEN, P2PKH_OUTPUT_LEN};
use serde_json::Value as Json;

/// ZIP 317 `marginal_fee`, in base units per logical action.
pub const MARGINAL_FEE: u64 = 5000;
/// ZIP 317 `grace_actions`: a transaction pays for at least this many logical actions.
pub const GRACE_ACTIONS: u64 = 2;
/// ZIP 317 `P2PKH_standard_input_size`: the bytes of transparent input that make one logical action.
pub const STANDARD_INPUT_SIZE: u64 = 150;
/// ZIP 317 `P2PKH_standard_output_size`: the bytes of transparent output that make one logical action.
pub const STANDARD_OUTPUT_SIZE: u64 = 34;

/// The Zcash network branch ids from NU7 on that select the ZIP 317 rule. The branch id alone is not
/// unique (a coin that carries Zcash's Sapling id `0x76b809bb` is not a ZIP 317 coin), so the rule also
/// requires `overwintered` and `txversion` 5.
pub const ZIP317_BRANCH_IDS: &[u32] = &[0x7719_0AD9];
/// The only transaction version the rule applies to.
pub const ZIP317_TX_VERSION: i32 = 5;

/// The serialized size of an unsigned transparent input: outpoint 36, an empty scriptSig length byte 1 and
/// sequence 4.
const UNSIGNED_INPUT_LEN: u64 = 41;
/// The bytes a signature push and a compressed public key push add to a P2PKH scriptSig. It is the allowance
/// of `tx_size_in_v_bytes`: 2 push bytes, a maximal DER signature and a compressed public key.
const P2PKH_SCRIPT_SIG_ALLOWANCE: u64 = 2 + MAX_DER_SIGNATURE_LEN as u64 + COMPRESSED_PUBKEY_LEN as u64;
/// One signed P2PKH input as the builder estimates it: 148 bytes, one logical action.
pub const P2PKH_INPUT_LEN: u64 = UNSIGNED_INPUT_LEN + P2PKH_SCRIPT_SIG_ALLOWANCE;
/// A signature push as `tx_size_in_v_bytes` allows for it: the length byte and a maximal DER signature
/// (the sighash byte included).
pub const SIGNATURE_PUSH_LEN: u64 = 1 + MAX_DER_SIGNATURE_LEN as u64;
/// The length of a DER signature without its sighash byte, at its maximum. A scriptSig builder that appends the
/// sighash byte itself yields a [`SIGNATURE_PUSH_LEN`] push when it is given this many bytes.
pub const MAX_DER_SIGNATURE_LEN_NO_SIGHASH: usize = MAX_DER_SIGNATURE_LEN - 1;

/// The transparent bytes ZIP 317 counts for a transaction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Zip317Bytes {
    /// The total serialized size of the transparent inputs, as they will be when signed.
    pub input: u64,
    /// The total serialized size of the transparent outputs.
    pub output: u64,
}

impl Zip317Bytes {
    /// The same transaction with one more P2PKH output (a change output).
    pub fn with_p2pkh_change(self) -> Self {
        Zip317Bytes {
            input: self.input,
            output: self.output.saturating_add(P2PKH_OUTPUT_LEN),
        }
    }

    /// The conventional fee of these bytes, see [`conventional_fee`].
    pub fn fee(self) -> u64 { conventional_fee(self.input, self.output) }
}

/// The ZIP 317 conventional fee: `5000 * max(2, ceil(input_bytes / 150), ceil(output_bytes / 34))`.
pub fn conventional_fee(input_bytes: u64, output_bytes: u64) -> u64 {
    let actions = input_bytes
        .div_ceil(STANDARD_INPUT_SIZE)
        .max(output_bytes.div_ceil(STANDARD_OUTPUT_SIZE))
        .max(GRACE_ACTIONS);
    MARGINAL_FEE.saturating_mul(actions)
}

/// The conventional fee of a standard transaction (inside the grace actions): what a one-input transaction
/// with a payment and a change output pays.
pub fn standard_fee() -> u64 { MARGINAL_FEE.saturating_mul(GRACE_ACTIONS) }

/// The length of a CompactSize (varint) that encodes `n`.
pub fn compact_size_len(n: u64) -> u64 {
    match n {
        0..=252 => 1,
        253..=0xffff => 3,
        0x1_0000..=0xffff_ffff => 5,
        _ => 9,
    }
}

/// The serialized size of a transaction output with a scriptPubKey of `script_len` bytes: value 8, the
/// CompactSize of the script length and the script.
pub fn output_len(script_len: usize) -> u64 {
    let script_len = script_len as u64;
    8u64.saturating_add(compact_size_len(script_len))
        .saturating_add(script_len)
}

/// The serialized size of a transparent input whose scriptSig is `script_sig_len` bytes: outpoint 36, the
/// CompactSize of the scriptSig length, the scriptSig and sequence 4.
pub fn input_len_with_script_sig(script_sig_len: usize) -> u64 {
    let script_sig_len = script_sig_len as u64;
    36u64
        .saturating_add(compact_size_len(script_sig_len))
        .saturating_add(script_sig_len)
        .saturating_add(4)
}

/// Whether a coins-configuration entry selects the ZIP 317 rule. It does exactly when `overwintered` is `1`,
/// `txversion` is 5 and `consensus_branch_id` is a hexadecimal string (with or without `0x`) that is one of
/// [`ZIP317_BRANCH_IDS`]. An entry without a parsable `consensus_branch_id` never selects it. The keys are read
/// the way the coin configuration reads them.
pub fn conf_selects_zip317(conf: &Json) -> bool {
    selects_zip317(
        conf["overwintered"].as_u64().unwrap_or(0) == 1,
        conf["txversion"].as_i64().unwrap_or(1),
        conf["consensus_branch_id"]
            .as_str()
            .and_then(|s| parse_hex_encoded_u32(s).ok()),
    )
}

/// The selection rule over the parsed values; see [`conf_selects_zip317`].
pub fn selects_zip317(overwintered: bool, tx_version: i64, branch_id: Option<u32>) -> bool {
    overwintered
        && tx_version == i64::from(ZIP317_TX_VERSION)
        && branch_id.is_some_and(|id| ZIP317_BRANCH_IDS.contains(&id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_constants_match_the_builder_estimate() {
        assert_eq!(P2PKH_INPUT_LEN, 148);
        assert_eq!(SIGNATURE_PUSH_LEN, 73);
        assert_eq!(P2PKH_OUTPUT_LEN, 34);
        // A P2PKH output: value 8, script length 1, script 25.
        assert_eq!(output_len(25), 34);
        // A P2SH output is 32 bytes, the swap OP_RETURN with a 20-byte secret hash is 31.
        assert_eq!(output_len(23), 32);
        assert_eq!(output_len(22), 31);
    }

    /// The grace floor is 2 actions.
    #[test]
    fn test_conventional_fee_grace_floor() {
        assert_eq!(conventional_fee(0, 0), 10_000);
        assert_eq!(conventional_fee(1, 1), 10_000);
        assert_eq!(conventional_fee(148, 34), 10_000);
        assert_eq!(conventional_fee(300, 68), 10_000);
        assert_eq!(standard_fee(), 10_000);
    }

    /// Inputs dominate, with the boundaries at 150 and 151 bytes (and 300 and 301).
    #[test]
    fn test_conventional_fee_inputs_dominate() {
        assert_eq!(conventional_fee(150, 34), 10_000, "150 input bytes are 1 action");
        assert_eq!(conventional_fee(151, 34), 10_000, "151 input bytes are 2 actions");
        assert_eq!(conventional_fee(300, 34), 10_000);
        assert_eq!(conventional_fee(301, 34), 15_000, "301 input bytes are 3 actions");
        assert_eq!(conventional_fee(1500, 34), 50_000);
        assert_eq!(conventional_fee(1501, 34), 55_000);
        // n P2PKH inputs of 148 bytes pay 5000 * max(2, ceil(148 n / 150)) = 5000 * max(2, n) for n up to 74.
        for n in 1..=10u64 {
            assert_eq!(conventional_fee(n * P2PKH_INPUT_LEN, 68), 5000 * n.max(2), "{n} inputs");
        }
    }

    /// Outputs dominate, with the boundaries at 34 and 35 bytes (and 68 and 69).
    #[test]
    fn test_conventional_fee_outputs_dominate() {
        assert_eq!(conventional_fee(148, 34), 10_000, "34 output bytes are 1 action");
        assert_eq!(conventional_fee(148, 35), 10_000, "35 output bytes are 2 actions");
        assert_eq!(conventional_fee(148, 68), 10_000);
        assert_eq!(conventional_fee(148, 69), 15_000, "69 output bytes are 3 actions");
        assert_eq!(conventional_fee(148, 170), 25_000, "5 P2PKH outputs");
        assert_eq!(conventional_fee(148, 171), 30_000);
    }

    #[test]
    fn test_conventional_fee_saturates() {
        assert_eq!(conventional_fee(u64::MAX, 0), u64::MAX);
        assert_eq!(conventional_fee(0, u64::MAX), u64::MAX);
    }

    #[test]
    fn test_with_p2pkh_change() {
        let bytes = Zip317Bytes { input: 148, output: 68 };
        assert_eq!(bytes.fee(), 10_000);
        assert_eq!(bytes.with_p2pkh_change(), Zip317Bytes {
            input: 148,
            output: 102
        });
        assert_eq!(bytes.with_p2pkh_change().fee(), 15_000);
    }

    #[test]
    fn test_compact_size_and_lengths() {
        assert_eq!([0, 252, 253, 65535, 65536].map(compact_size_len), [1, 1, 3, 3, 5]);
        assert_eq!(input_len_with_script_sig(0), 41);
        assert_eq!(input_len_with_script_sig(107), 148);
        assert_eq!(input_len_with_script_sig(252), 293);
        assert_eq!(input_len_with_script_sig(253), 296);
    }

    /// The selection matrix over the coins-configuration keys.
    #[test]
    fn test_conf_selects_zip317_matrix() {
        let zec = |branch: serde_json::Value| json!({"overwintered": 1, "txversion": 5, "consensus_branch_id": branch, "txfee": 70000});
        // ZEC, version 5, NU7, with and without the prefix.
        assert!(conf_selects_zip317(&zec(json!("0x77190ad9"))));
        assert!(conf_selects_zip317(&zec(json!("77190ad9"))));
        assert!(conf_selects_zip317(&zec(json!("0x77190AD9"))));
        // An unknown branch id falls back to the per-kilobyte rule.
        assert!(!conf_selects_zip317(&zec(json!("0x12345678"))));
        // ZEC version 4 (NU5's id 0xc2d6d0b4 or the older 0x37a5165b) stays per kilobyte.
        let zec_v4 = json!({"overwintered": 1, "txversion": 4, "consensus_branch_id": "0x37a5165b", "txfee": 70000});
        assert!(!conf_selects_zip317(&zec_v4));
        let zec_v4_nu7 = json!({"overwintered": 1, "txversion": 4, "consensus_branch_id": "0x77190ad9"});
        assert!(!conf_selects_zip317(&zec_v4_nu7), "version 4 never selects the rule");
        // FLUX: Zcash's Sapling id with version 4.
        let flux = json!({"overwintered": 1, "txversion": 4, "consensus_branch_id": "0x76b809bb", "txfee": 10000});
        assert!(!conf_selects_zip317(&flux));
        // ZER, BTCZ, KMD and VRSC: version 4 or 3, no branch id or an old one.
        assert!(!conf_selects_zip317(&json!({"overwintered": 1, "txversion": 4})));
        assert!(!conf_selects_zip317(&json!({"overwintered": 1, "txversion": 3})));
        assert!(!conf_selects_zip317(&json!({"txfee": 1000})));
        assert!(!conf_selects_zip317(&json!({})));
        // A non-overwintered entry with txversion 5 is unchanged, whatever its branch id.
        assert!(!conf_selects_zip317(
            &json!({"txversion": 5, "consensus_branch_id": "0x77190ad9"})
        ));
        assert!(!conf_selects_zip317(
            &json!({"overwintered": 0, "txversion": 5, "consensus_branch_id": "0x77190ad9"})
        ));
        // A branch id that is not a string, or does not parse, selects nothing.
        assert!(!conf_selects_zip317(&zec(json!(0x7719_0AD9u32))));
        assert!(!conf_selects_zip317(&zec(json!("not hex"))));
        assert!(!conf_selects_zip317(&zec(json!(null))));
        // The `txfee` value does not take part in the selection.
        let no_txfee = json!({"overwintered": 1, "txversion": 5, "consensus_branch_id": "0x77190ad9"});
        assert!(conf_selects_zip317(&no_txfee));
        let zero_txfee = json!({"overwintered": 1, "txversion": 5, "consensus_branch_id": "0x77190ad9", "txfee": 0});
        assert!(conf_selects_zip317(&zero_txfee));
    }

    #[test]
    fn test_selects_zip317_parsed_values() {
        assert!(selects_zip317(true, 5, Some(0x7719_0AD9)));
        assert!(!selects_zip317(true, 5, Some(0x76b8_09bb)));
        assert!(!selects_zip317(true, 5, None));
        assert!(!selects_zip317(true, 4, Some(0x7719_0AD9)));
        assert!(!selects_zip317(false, 5, Some(0x7719_0AD9)));
    }
}
