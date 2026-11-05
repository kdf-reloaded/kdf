//! Capability words in coin protocol information (CRD chapter 59, §59.4A).
//!
//! A coin's protocol information is the opaque byte string it publishes in `MakerOrderCreated`,
//! `TakerRequest`, `MakerReserved` and the side channel of a synchronization answer. A coin that
//! needs its counterparty to have a capability appends a **capability word** to its *first value*
//! (the bytes it sends without the word): one MessagePack map with string keys and `true` values.
//!
//! Public exports:
//!
//! - [`CAPABILITY_TX_V6`], the one key in use (R59.4A.4);
//! - [`append_capability_word`], the writer (R59.4A.1);
//! - [`declared_capabilities`], [`DeclaredCapabilities`] and [`declares_capability`], the fail-closed
//!   reader (R59.4A.2).
//!
//! The layout is a wire format that deployed peers read; the bytes of the first value are never
//! changed, and every reader of the first value ignores what follows it (R59.4A.3).
//!
//! The reader skips the first value with the bounds check of CRD ch.28 R16a
//! ([`common::msgpack_bounds`]) and reads the word with the same discipline: every length and count
//! is checked against the bytes that are actually present before it is used, and nothing is
//! allocated from it. Any failure yields "nothing declared"; it is never an error and never a
//! panic.

use common::msgpack_bounds::check_msgpack_bounds;
use std::collections::BTreeSet;

/// Capability key: this node builds and spends the version-6 transaction format of the Ironwood
/// upgrade (CRD ch.59 R59.4A.4; the rule that requires it is bound in ch.39).
pub const CAPABILITY_TX_V6: &str = "tx_v6";

/// MessagePack marker of `true`, the only value that declares a capability.
const MARKER_TRUE: u8 = 0xc3;

/// The capabilities that a protocol-information value declares (R59.4A.2). Empty when nothing is
/// declared, which is also the result of every malformed input.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DeclaredCapabilities {
    keys: BTreeSet<String>,
}

impl DeclaredCapabilities {
    /// Whether `key` is declared (present in the word with the value `true`).
    pub fn declares(&self, key: &str) -> bool { self.keys.contains(key) }

    /// Whether nothing is declared.
    pub fn is_empty(&self) -> bool { self.keys.is_empty() }
}

/// Appends the capability word for `keys` to `first_value` (R59.4A.1).
///
/// The word is one MessagePack map `{key: true, ...}` with the keys in ascending byte order and
/// without duplicates, so the same set always gives the same bytes. An empty `keys` returns
/// `first_value` unchanged: no empty map is written.
pub fn append_capability_word(first_value: Vec<u8>, keys: &[&str]) -> Vec<u8> {
    let sorted: BTreeSet<&str> = keys.iter().copied().collect();
    if sorted.is_empty() {
        return first_value;
    }
    let mut out = first_value;
    push_map_header(&mut out, sorted.len());
    for key in sorted {
        push_str(&mut out, key);
        out.push(MARKER_TRUE);
    }
    out
}

fn push_map_header(out: &mut Vec<u8>, len: usize) {
    if let Ok(len) = u8::try_from(len) {
        if len <= 0x0f {
            out.push(0x80 | len);
            return;
        }
    }
    if let Ok(len) = u16::try_from(len) {
        out.push(0xde);
        out.extend_from_slice(&len.to_be_bytes());
    } else {
        // A map of more than u32::MAX keys is not representable; the key set is a constant list in practice.
        out.push(0xdf);
        out.extend_from_slice(&u32::try_from(len).unwrap_or(u32::MAX).to_be_bytes());
    }
}

fn push_str(out: &mut Vec<u8>, s: &str) {
    let len = s.len();
    if len <= 0x1f {
        out.push(0xa0 | len as u8);
    } else if let Ok(len) = u8::try_from(len) {
        out.push(0xd9);
        out.push(len);
    } else if let Ok(len) = u16::try_from(len) {
        out.push(0xda);
        out.extend_from_slice(&len.to_be_bytes());
    } else {
        out.push(0xdb);
        out.extend_from_slice(&u32::try_from(len).unwrap_or(u32::MAX).to_be_bytes());
    }
    out.extend_from_slice(s.as_bytes());
}

/// Whether `info` declares `key`. Has the shape of `MmCoin::is_coin_protocol_supported`, so a coin
/// that requires a capability can call it from its acceptance check (R59.4A.5). Fail-closed.
pub fn declares_capability(info: &Option<Vec<u8>>, key: &str) -> bool {
    declared_capabilities(info.as_deref()).declares(key)
}

/// Reads the capability word of a protocol-information value (R59.4A.2).
///
/// 1. Skips exactly one complete MessagePack value from the start, whatever its type.
/// 2. Decodes the next value as a map with string keys.
/// 3. A key is declared exactly when its value is the boolean `true`.
///
/// Nothing is declared when `info` is absent or empty, when the first value is truncated or
/// malformed, when nothing follows it, or when what follows is not a map with string keys (or is
/// truncated). A `false` or non-boolean value does not declare; unknown keys and bytes after the
/// word are ignored. The result never fails: malformed input is "nothing declared".
pub fn declared_capabilities(info: Option<&[u8]>) -> DeclaredCapabilities {
    info.and_then(read_word).unwrap_or_default()
}

fn read_word(bytes: &[u8]) -> Option<DeclaredCapabilities> {
    let mut pos = skip_value(bytes, 0)?;
    let entries = read_map_header(bytes, &mut pos)?;
    let mut keys = BTreeSet::new();
    for _ in 0..entries {
        let key = read_str(bytes, &mut pos)?;
        if *bytes.get(pos)? == MARKER_TRUE {
            pos += 1;
            keys.insert(key.to_owned());
        } else {
            pos = skip_value(bytes, pos)?;
        }
    }
    Some(DeclaredCapabilities { keys })
}

/// Reads `n` big-endian bytes at `*pos` and advances `*pos`.
fn read_be(bytes: &[u8], pos: &mut usize, n: usize) -> Option<u64> {
    let end = pos.checked_add(n)?;
    let value = bytes
        .get(*pos..end)?
        .iter()
        .fold(0u64, |acc, byte| (acc << 8) | u64::from(*byte));
    *pos = end;
    Some(value)
}

/// Reads a map header at `*pos` and returns the entry count. The count is accepted only if the
/// remaining bytes can hold that many entries (at least two bytes each).
fn read_map_header(bytes: &[u8], pos: &mut usize) -> Option<u64> {
    let marker = *bytes.get(*pos)?;
    *pos += 1;
    let entries = match marker {
        0x80..=0x8f => u64::from(marker & 0x0f),
        0xde => read_be(bytes, pos, 2)?,
        0xdf => read_be(bytes, pos, 4)?,
        _ => return None,
    };
    fits(bytes, *pos, entries.checked_mul(2)?).then_some(entries)
}

/// Reads a string at `*pos` (fixstr, str8, str16 or str32) and returns it. A key that is not valid
/// UTF-8 is malformed.
fn read_str<'a>(bytes: &'a [u8], pos: &mut usize) -> Option<&'a str> {
    let marker = *bytes.get(*pos)?;
    *pos += 1;
    let len = match marker {
        0xa0..=0xbf => u64::from(marker & 0x1f),
        0xd9 => read_be(bytes, pos, 1)?,
        0xda => read_be(bytes, pos, 2)?,
        0xdb => read_be(bytes, pos, 4)?,
        _ => return None,
    };
    if !fits(bytes, *pos, len) {
        return None;
    }
    // `fits` guarantees `len` is at most the remaining byte count, so it fits in `usize`.
    let end = *pos + len as usize;
    let s = std::str::from_utf8(&bytes[*pos..end]).ok()?;
    *pos = end;
    Some(s)
}

/// Whether `count` more bytes are present from `pos`.
fn fits(bytes: &[u8], pos: usize, count: u64) -> bool { count <= (bytes.len().saturating_sub(pos)) as u64 }

/// Skips exactly one complete MessagePack value that starts at `start` and returns the offset after
/// it, or `None` if the value is truncated, malformed or outside the bounds of CRD ch.28 R16a
/// (R59.4A.2 step 1). The walk is the shared one of [`common::msgpack_bounds`].
pub(crate) fn skip_value(bytes: &[u8], start: usize) -> Option<usize> {
    let rest = bytes.get(start..)?;
    check_msgpack_bounds(rest).ok().map(|len| start + len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::msgpack_bounds::MAX_PEER_NESTING;

    fn with_word(first: &[u8], word: &[u8]) -> Vec<u8> { [first, word].concat() }

    fn declared(info: &[u8]) -> bool { declared_capabilities(Some(info)).declares(CAPABILITY_TX_V6) }

    /// The word `{"tx_v6": true}`.
    fn tx_v6_word() -> Vec<u8> { append_capability_word(Vec::new(), &[CAPABILITY_TX_V6]) }

    /// Real encodings of a UTXO address format, the first value of a UTXO coin.
    fn address_formats() -> Vec<Vec<u8>> {
        use keys::AddressFormat;
        [
            AddressFormat::Standard,
            AddressFormat::Segwit,
            AddressFormat::CashAddress {
                network: "bitcoincash".to_owned(),
                pub_addr_prefix: 0,
                p2sh_addr_prefix: 5,
            },
        ]
        .iter()
        .map(|format| rmp_serde::to_vec(format).unwrap())
        .collect()
    }

    #[test]
    fn t14_a_writer_one_key_is_the_fixed_bytes() {
        // map(1), str(5) "tx_v6", true
        let expected = [0x81, 0xa5, b't', b'x', b'_', b'v', b'6', 0xc3];
        assert_eq!(tx_v6_word(), expected);
    }

    #[test]
    fn t14_a_writer_two_keys_are_in_ascending_byte_order() {
        let expected = [0x82, 0xa1, b'a', 0xc3, 0xa5, b't', b'x', b'_', b'v', b'6', 0xc3];
        assert_eq!(append_capability_word(Vec::new(), &[CAPABILITY_TX_V6, "a"]), expected);
        assert_eq!(append_capability_word(Vec::new(), &["a", CAPABILITY_TX_V6]), expected);
    }

    #[test]
    fn t14_a_writer_drops_duplicates_and_matches_a_serializer() {
        let word = append_capability_word(Vec::new(), &["b", "a", "b"]);
        let expected: std::collections::BTreeMap<&str, bool> = [("a", true), ("b", true)].into_iter().collect();
        assert_eq!(word, rmp_serde::to_vec(&expected).unwrap());
    }

    #[test]
    fn t14_a_writer_keeps_the_first_value_bytes_first_and_unchanged() {
        for first in address_formats() {
            let out = append_capability_word(first.clone(), &[CAPABILITY_TX_V6]);
            assert_eq!(&out[..first.len()], first.as_slice());
            assert_eq!(&out[first.len()..], tx_v6_word().as_slice());
        }
    }

    #[test]
    fn t14_a_writer_without_keys_returns_the_first_value_unchanged() {
        let first = address_formats().remove(0);
        assert_eq!(append_capability_word(first.clone(), &[]), first);
        assert_eq!(append_capability_word(Vec::new(), &[]), Vec::<u8>::new());
    }

    #[test]
    fn t14_a_writer_long_key_set_uses_the_wider_headers() {
        let long_key = "k".repeat(40);
        let word = append_capability_word(Vec::new(), &[&long_key]);
        assert_eq!(&word[..4], &[0x81, 0xd9, 40, b'k']);
        let many: Vec<String> = (0..20).map(|i| format!("key{i:02}")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let word = append_capability_word(Vec::new(), &refs);
        assert_eq!(&word[..3], &[0xde, 0, 20]);
        let read = declared_capabilities(Some(&with_word(&[0x01], &word)));
        assert!(refs.iter().all(|key| read.declares(key)));
        assert!(declared_capabilities(Some(&with_word(
            &[0x01],
            &append_capability_word(Vec::new(), &[&long_key])
        )))
        .declares(&long_key));
    }

    #[test]
    fn t14_b_reader_first_value_alone_declares_nothing() {
        for first in address_formats() {
            assert!(!declared(&first));
        }
    }

    #[test]
    fn t14_b_reader_first_value_followed_by_true_declares_it() {
        for first in address_formats() {
            let info = with_word(&first, &tx_v6_word());
            let read = declared_capabilities(Some(&info));
            assert!(read.declares(CAPABILITY_TX_V6));
            assert!(!read.declares("other"));
            assert!(declares_capability(&Some(info), CAPABILITY_TX_V6));
        }
    }

    #[test]
    fn t14_b_reader_false_declares_nothing() {
        let first = address_formats().remove(0);
        // map(1), "tx_v6", false
        let word = [0x81, 0xa5, b't', b'x', b'_', b'v', b'6', 0xc2];
        assert!(!declared(&with_word(&first, &word)));
    }

    #[test]
    fn t14_b_reader_bytes_that_are_not_a_map_declare_nothing() {
        let first = address_formats().remove(0);
        for not_a_map in [
            vec![0x01],
            vec![0xc3],
            vec![0x91, 0xc3],
            vec![0xa5, b't', b'x', b'_', b'v', b'6'],
            vec![0xc0],
            vec![0x00; 16],
        ] {
            assert!(!declared(&with_word(&first, &not_a_map)), "{not_a_map:?}");
        }
    }

    #[test]
    fn t14_b_reader_absent_and_empty_declare_nothing() {
        assert!(!declared_capabilities(None).declares(CAPABILITY_TX_V6));
        assert!(!declared_capabilities(Some(&[])).declares(CAPABILITY_TX_V6));
        assert!(declared_capabilities(None).is_empty());
        assert!(!declares_capability(&None, CAPABILITY_TX_V6));
        assert!(!declares_capability(&Some(Vec::new()), CAPABILITY_TX_V6));
    }

    #[test]
    fn t14_b_first_value_truncated_in_the_middle() {
        let cash = address_formats().remove(2);
        for cut in 0..cash.len() {
            assert_eq!(skip_value(&cash[..cut], 0), None, "cut {cut}");
            assert!(declared_capabilities(Some(&cash[..cut])).is_empty(), "cut {cut}");
            // The word right behind the cut value is not read as the word of a complete first value.
            assert!(!declared(&with_word(&cash[..cut], &tx_v6_word())), "cut {cut}");
        }
    }

    #[test]
    fn t14_b_word_truncated_in_the_middle() {
        let first = address_formats().remove(0);
        let word = tx_v6_word();
        for cut in 0..word.len() {
            assert!(!declared(&with_word(&first, &word[..cut])), "cut {cut}");
        }
    }

    #[test]
    fn t14_b_unknown_key_and_trailing_bytes_are_ignored() {
        let first = address_formats().remove(1);
        // map(2): "zz" -> 7, "tx_v6" -> true; then trailing bytes.
        let word = [
            0x82, 0xa2, b'z', b'z', 0x07, 0xa5, b't', b'x', b'_', b'v', b'6', 0xc3, 0xde, 0xad, 0xbe, 0xef,
        ];
        let read = declared_capabilities(Some(&with_word(&first, &word)));
        assert!(read.declares(CAPABILITY_TX_V6));
        assert!(!read.declares("zz"));
    }

    #[test]
    fn t14_b_unknown_key_with_a_nested_value_is_skipped() {
        let first = address_formats().remove(0);
        // map(2): "zz" -> [[1, {"a": 2}], "x"], "tx_v6" -> true
        let word = [
            0x82, 0xa2, b'z', b'z', 0x92, 0x92, 0x01, 0x81, 0xa1, b'a', 0x02, 0xa1, b'x', 0xa5, b't', b'x', b'_', b'v',
            b'6', 0xc3,
        ];
        assert!(declared(&with_word(&first, &word)));
    }

    #[test]
    fn t14_b_non_boolean_value_declares_nothing() {
        let first = address_formats().remove(0);
        for value in [vec![0x01], vec![0xc0], vec![0xa1, b't'], vec![0x91, 0xc3], vec![
            0xcc, 0x01,
        ]] {
            let mut word = vec![0x81, 0xa5, b't', b'x', b'_', b'v', b'6'];
            word.extend_from_slice(&value);
            assert!(!declared(&with_word(&first, &word)), "{value:?}");
        }
    }

    #[test]
    fn t14_b_key_twice_is_declared_when_any_occurrence_is_true() {
        let first = address_formats().remove(0);
        // map(2): "tx_v6" -> true, "tx_v6" -> false; and the reverse order.
        let true_then_false = [
            0x82, 0xa5, b't', b'x', b'_', b'v', b'6', 0xc3, 0xa5, b't', b'x', b'_', b'v', b'6', 0xc2,
        ];
        let false_then_true = [
            0x82, 0xa5, b't', b'x', b'_', b'v', b'6', 0xc2, 0xa5, b't', b'x', b'_', b'v', b'6', 0xc3,
        ];
        assert!(declared(&with_word(&first, &true_then_false)));
        assert!(declared(&with_word(&first, &false_then_true)));
    }

    #[test]
    fn t14_b_empty_map_as_the_word_declares_nothing() {
        let first = address_formats().remove(0);
        let read = declared_capabilities(Some(&with_word(&first, &[0x80])));
        assert!(read.is_empty());
        assert!(!read.declares(CAPABILITY_TX_V6));
    }

    #[test]
    fn t14_b_non_string_key_makes_the_word_malformed() {
        let first = address_formats().remove(0);
        // map(2): 1 -> true, "tx_v6" -> true
        let word = [0x82, 0x01, 0xc3, 0xa5, b't', b'x', b'_', b'v', b'6', 0xc3];
        assert!(!declared(&with_word(&first, &word)));
    }

    #[test]
    fn t14_b_key_that_is_not_utf8_makes_the_word_malformed() {
        let first = address_formats().remove(0);
        // map(2): "tx_v6" -> true, then a key of one invalid byte.
        let word = [0x82, 0xa5, b't', b'x', b'_', b'v', b'6', 0xc3, 0xa1, 0xff, 0xc3];
        assert!(!declared(&with_word(&first, &word)));
    }

    #[test]
    fn t14_b_first_value_kinds_are_skipped_whole_and_the_word_is_read() {
        let word = tx_v6_word();
        let firsts: Vec<(&str, Vec<u8>)> = vec![
            ("positive fixint", vec![0x2a]),
            ("negative fixint", vec![0xff]),
            ("uint64", rmp_serde::to_vec(&u64::MAX).unwrap()),
            ("int64", rmp_serde::to_vec(&i64::MIN).unwrap()),
            ("nil", vec![0xc0]),
            ("true", vec![0xc3]),
            ("float64", rmp_serde::to_vec(&1.5f64).unwrap()),
            ("string", rmp_serde::to_vec(&"a first value").unwrap()),
            ("bin8", vec![0xc4, 0x02, 0xaa, 0xbb]),
            ("bin16", [vec![0xc5, 0x01, 0x00], vec![7u8; 256]].concat()),
            ("nested array", rmp_serde::to_vec(&vec![vec![1u8, 2], vec![3]]).unwrap()),
            ("array16", [vec![0xdc, 0x00, 0x11], vec![1u8; 17]].concat()),
            (
                "map",
                rmp_serde::to_vec(&std::collections::BTreeMap::from([("k", vec![1u8, 2])])).unwrap(),
            ),
            ("map16", [vec![0xde, 0x00, 0x01, 0x01], vec![0x02]].concat()),
            ("fixext1", vec![0xd4, 0x05, 0x01]),
            ("fixext2", vec![0xd5, 0x05, 0x01, 0x02]),
            ("fixext4", [vec![0xd6, 0x05], vec![1u8; 4]].concat()),
            ("fixext8", [vec![0xd7, 0x05], vec![1u8; 8]].concat()),
            ("fixext16", [vec![0xd8, 0x05], vec![1u8; 16]].concat()),
            ("ext8", vec![0xc7, 0x03, 0x05, 0x01, 0x02, 0x03]),
            ("ext16", [vec![0xc8, 0x00, 0x02, 0x05], vec![1u8; 2]].concat()),
            (
                "ext32",
                [vec![0xc9, 0x00, 0x00, 0x00, 0x02, 0x05], vec![1u8; 2]].concat(),
            ),
        ];
        for (name, first) in firsts {
            assert_eq!(skip_value(&first, 0), Some(first.len()), "{name}");
            assert!(declared(&with_word(&first, &word)), "{name}");
        }
    }

    /// `levels` arrays nested into one another, the innermost holding one integer.
    fn nested_arrays(levels: usize) -> Vec<u8> {
        let mut out = vec![0x91; levels];
        out.push(0x01);
        out
    }

    #[test]
    fn t14_b_first_value_exactly_at_the_nesting_bound_is_skipped() {
        let first = nested_arrays(MAX_PEER_NESTING);
        assert_eq!(skip_value(&first, 0), Some(first.len()));
        assert!(declared(&with_word(&first, &tx_v6_word())));
    }

    #[test]
    fn t14_b_first_value_one_level_deeper_than_the_bound_declares_nothing() {
        let first = nested_arrays(MAX_PEER_NESTING + 1);
        assert_eq!(skip_value(&first, 0), None);
        assert!(!declared(&with_word(&first, &tx_v6_word())));
        // A much deeper value is rejected the same way.
        let deep = nested_arrays(1_000_000);
        assert_eq!(skip_value(&deep, 0), None);
        assert!(!declared(&with_word(&deep, &tx_v6_word())));
    }

    #[test]
    fn t14_b_maps_count_towards_the_nesting_bound() {
        // {"a": {"a": ... 1}}
        let level = [0x81, 0xa1, b'a'];
        let make = |levels: usize| {
            let mut out = Vec::new();
            for _ in 0..levels {
                out.extend_from_slice(&level);
            }
            out.push(0x01);
            out
        };
        assert!(skip_value(&make(MAX_PEER_NESTING), 0).is_some());
        assert_eq!(skip_value(&make(MAX_PEER_NESTING + 1), 0), None);
    }

    #[test]
    fn t14_b_bin32_declaring_more_than_is_present_declares_nothing() {
        // bin32 declares 0xFFFF_FFF0 bytes, three are present.
        let info = [vec![0xc6, 0xff, 0xff, 0xff, 0xf0, 1, 2, 3], tx_v6_word()].concat();
        assert!(!declared(&[&info[..8]].concat()));
        assert_eq!(skip_value(&info[..8], 0), None);
        assert!(!declared(&info));
    }

    #[test]
    fn t14_b_map32_declaring_more_entries_than_fit_declares_nothing() {
        // map32 declares 0xFFFF_FFFF entries, nothing follows the header.
        let info = [0xdf, 0xff, 0xff, 0xff, 0xff];
        assert_eq!(skip_value(&info, 0), None);
        let first = address_formats().remove(0);
        assert!(!declared(&with_word(&first, &info)));
        // The same as a first value, and as an array32 and a str32.
        assert!(!declared(&info));
        assert_eq!(skip_value(&[0xdd, 0xff, 0xff, 0xff, 0xff], 0), None);
        assert_eq!(skip_value(&[0xdb, 0xff, 0xff, 0xff, 0xff, b'a'], 0), None);
    }

    #[test]
    fn t14_b_reserved_marker_declares_nothing() {
        assert_eq!(skip_value(&[0xc1], 0), None);
        assert!(!declared(&with_word(&[0xc1], &tx_v6_word())));
        // Inside a container and inside the word.
        assert_eq!(skip_value(&[0x92, 0x01, 0xc1], 0), None);
        let first = address_formats().remove(0);
        let word = [0x81, 0xa5, b't', b'x', b'_', b'v', b'6', 0xc1];
        assert!(!declared(&with_word(&first, &word)));
    }

    #[test]
    fn t14_b_skip_returns_the_offset_after_one_value() {
        let bytes = [0x92, 0x01, 0xa1, b'x', 0x05, 0x06];
        assert_eq!(skip_value(&bytes, 0), Some(4));
        assert_eq!(skip_value(&bytes, 4), Some(5));
        assert_eq!(skip_value(&bytes, 6), None);
        assert_eq!(skip_value(&bytes, 100), None);
    }

    #[test]
    fn t14_b_every_prefix_of_a_value_is_truncated() {
        let value = rmp_serde::to_vec(&vec![("key", vec![1u8, 2, 3]), ("other", vec![])]).unwrap();
        assert_eq!(skip_value(&value, 0), Some(value.len()));
        for cut in 0..value.len() {
            assert_eq!(skip_value(&value[..cut], 0), None, "cut {cut}");
        }
    }

    #[test]
    fn t14_b_no_input_makes_the_reader_panic() {
        // Every two-byte and three-byte prefix pattern, as a first value and as a word.
        let word = tx_v6_word();
        for a in 0..=255u8 {
            for b in [0u8, 1, 0x7f, 0x80, 0xff] {
                for c in [0u8, 0xff] {
                    let bytes = [a, b, c];
                    let _ = declared_capabilities(Some(&bytes));
                    let _ = declared_capabilities(Some(&with_word(&bytes, &word)));
                    let _ = declared_capabilities(Some(&with_word(&[0x01], &bytes)));
                }
            }
        }
    }

    // --- (c) coins side: the acceptance check reads only the first value (R59.4A.3) ---

    #[cfg(not(target_arch = "wasm32"))]
    fn utxo_coin(is_segwit: bool) -> crate::utxo::utxo_standard::UtxoStandardCoin {
        use crate::utxo::rpc_clients::{NativeClient, NativeClientImpl, UtxoRpcClientEnum};
        use crate::utxo::utxo_tests::{utxo_coin_fields_for_test, utxo_coin_from_fields};
        use std::sync::Arc;

        let client = NativeClient(Arc::new(NativeClientImpl::default()));
        utxo_coin_from_fields(utxo_coin_fields_for_test(
            UtxoRpcClientEnum::Native(client),
            None,
            is_segwit,
        ))
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn t14_c_utxo_acceptance_check_gives_the_same_result_with_the_word() {
        use crate::utxo::utxo_common;

        for is_segwit in [false, true] {
            let coin = utxo_coin(is_segwit);
            for first in address_formats() {
                let without = utxo_common::is_coin_protocol_supported(&coin, &Some(first.clone()));
                let with = utxo_common::is_coin_protocol_supported(&coin, &Some(with_word(&first, &tx_v6_word())));
                assert!(without);
                assert_eq!(without, with);
            }
            // An unreadable first value is declined with and without the word.
            let garbage = vec![0xc1];
            assert!(!utxo_common::is_coin_protocol_supported(&coin, &Some(garbage.clone())));
            assert!(!utxo_common::is_coin_protocol_supported(
                &coin,
                &Some(with_word(&garbage, &tx_v6_word()))
            ));
            // Absent information is judged by the coin's own format, as before.
            assert_eq!(utxo_common::is_coin_protocol_supported(&coin, &None), !is_segwit);
        }
    }

    // --- (d) coins side: a coin that does not opt in sends exactly what it sent before (R59.4A.4) ---

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn t14_d_utxo_coin_protocol_info_is_the_encoded_address_format() {
        use crate::utxo::{utxo_common, UtxoCommonOps};

        for is_segwit in [false, true] {
            let coin = utxo_coin(is_segwit);
            let expected = rmp_serde::to_vec(coin.addr_format()).unwrap();
            assert_eq!(utxo_common::coin_protocol_info(&coin), expected);
            assert!(declared_capabilities(Some(&expected)).is_empty());
        }
    }

    #[test]
    fn t14_d_coin_of_another_family_publishes_nothing() {
        use crate::{MmCoin, TestCoin};

        assert_eq!(TestCoin::default().coin_protocol_info(), Vec::<u8>::new());
    }
}
