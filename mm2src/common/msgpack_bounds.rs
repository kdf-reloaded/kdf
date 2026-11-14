//! Bounds on a MessagePack value received from a peer (CRD ch.28 R16a).
//!
//! [`check_msgpack_bounds`] walks one complete MessagePack value from the start of a byte slice
//! and checks it against two bounds before the value is decoded:
//!
//! - arrays and maps nest at most [`MAX_PEER_NESTING`] levels deep;
//! - every declared length and element count fits in the bytes that remain.
//!
//! The walk is iterative and uses a fixed-size array of per-level counters. It never allocates
//! from a declared length. It looks at the first value only and ignores the bytes after it, as
//! the decoder does. The encoding is the MessagePack specification's and does not change.
//!
//! [`check_peer_value`] is the form the decode sites use: it counts and logs a value that fails.
//! [`dropped_peer_values`] reads the process-wide total.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

/// The deepest nesting of arrays and maps that a peer's value may have.
pub const MAX_PEER_NESTING: usize = 64;

static DROPPED: AtomicU64 = AtomicU64::new(0);

/// Why a value is outside the bounds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MsgpackBoundsError {
    /// Arrays and maps nest deeper than [`MAX_PEER_NESTING`].
    TooDeep,
    /// The value, or a declared length or count in it, extends beyond the bytes present.
    Truncated,
    /// The byte is a marker that MessagePack reserves (`0xc1`).
    Reserved,
}

impl fmt::Display for MsgpackBoundsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MsgpackBoundsError::TooDeep => f.write_str("nesting exceeds the bound"),
            MsgpackBoundsError::Truncated => f.write_str("a declared length or count exceeds the remaining bytes"),
            MsgpackBoundsError::Reserved => f.write_str("reserved marker"),
        }
    }
}

impl std::error::Error for MsgpackBoundsError {}

/// What follows a marker byte.
enum Item {
    /// A scalar: this many bytes of data follow the header.
    Skip(u64),
    /// A container: this many child values follow the header (a map counts two per entry).
    Container(u64),
}

/// Reads the big-endian unsigned integer of `width` bytes at `pos`.
fn read_be(bytes: &[u8], pos: usize, width: usize) -> Result<u64, MsgpackBoundsError> {
    let end = pos.checked_add(width).ok_or(MsgpackBoundsError::Truncated)?;
    let field = bytes.get(pos..end).ok_or(MsgpackBoundsError::Truncated)?;
    Ok(field.iter().fold(0u64, |acc, b| (acc << 8) | u64::from(*b)))
}

/// Classifies the value that starts with `marker` (at `pos - 1`) and returns it together with the
/// position after its header.
fn read_header(bytes: &[u8], marker: u8, pos: usize) -> Result<(Item, usize), MsgpackBoundsError> {
    // A length field of `width` bytes, then `extra` further bytes that precede the data.
    let sized = |width: usize, extra: u64| -> Result<(Item, usize), MsgpackBoundsError> {
        let len = read_be(bytes, pos, width)?;
        let len = len.checked_add(extra).ok_or(MsgpackBoundsError::Truncated)?;
        Ok((Item::Skip(len), pos + width))
    };
    let counted = |width: usize, per_entry: u64| -> Result<(Item, usize), MsgpackBoundsError> {
        let count = read_be(bytes, pos, width)?;
        let children = count.checked_mul(per_entry).ok_or(MsgpackBoundsError::Truncated)?;
        Ok((Item::Container(children), pos + width))
    };
    match marker {
        // Positive and negative fixint, nil, false, true.
        0x00..=0x7f | 0xc0 | 0xc2 | 0xc3 | 0xe0..=0xff => Ok((Item::Skip(0), pos)),
        // Fixmap, fixarray, fixstr.
        0x80..=0x8f => Ok((Item::Container(u64::from(marker & 0x0f) * 2), pos)),
        0x90..=0x9f => Ok((Item::Container(u64::from(marker & 0x0f)), pos)),
        0xa0..=0xbf => Ok((Item::Skip(u64::from(marker & 0x1f)), pos)),
        0xc1 => Err(MsgpackBoundsError::Reserved),
        // bin8, bin16, bin32.
        0xc4 => sized(1, 0),
        0xc5 => sized(2, 0),
        0xc6 => sized(4, 0),
        // ext8, ext16, ext32: the length, a type byte, then the data.
        0xc7 => sized(1, 1),
        0xc8 => sized(2, 1),
        0xc9 => sized(4, 1),
        // float32, float64.
        0xca => Ok((Item::Skip(4), pos)),
        0xcb => Ok((Item::Skip(8), pos)),
        // uint8 to uint64, int8 to int64.
        0xcc | 0xd0 => Ok((Item::Skip(1), pos)),
        0xcd | 0xd1 => Ok((Item::Skip(2), pos)),
        0xce | 0xd2 => Ok((Item::Skip(4), pos)),
        0xcf | 0xd3 => Ok((Item::Skip(8), pos)),
        // fixext1, 2, 4, 8, 16: a type byte, then the data.
        0xd4 => Ok((Item::Skip(2), pos)),
        0xd5 => Ok((Item::Skip(3), pos)),
        0xd6 => Ok((Item::Skip(5), pos)),
        0xd7 => Ok((Item::Skip(9), pos)),
        0xd8 => Ok((Item::Skip(17), pos)),
        // str8, str16, str32.
        0xd9 => sized(1, 0),
        0xda => sized(2, 0),
        0xdb => sized(4, 0),
        // array16, array32, map16, map32.
        0xdc => counted(2, 1),
        0xdd => counted(4, 1),
        0xde => counted(2, 2),
        0xdf => counted(4, 2),
    }
}

/// Walks the first value of `bytes`; returns the offset after it and the deepest nesting seen.
fn walk(bytes: &[u8]) -> Result<(usize, usize), MsgpackBoundsError> {
    // Child values still to read at each open level.
    let mut remaining = [0u64; MAX_PEER_NESTING];
    let mut open = 0usize;
    let mut deepest = 0usize;
    let mut pos = 0usize;
    loop {
        let marker = *bytes.get(pos).ok_or(MsgpackBoundsError::Truncated)?;
        pos += 1;
        let (item, after_header) = read_header(bytes, marker, pos)?;
        pos = after_header;
        // Every child takes at least one byte, so a count beyond the remaining bytes cannot be met.
        let left = (bytes.len() - pos) as u64;
        match item {
            Item::Skip(len) => {
                if len > left {
                    return Err(MsgpackBoundsError::Truncated);
                }
                // `len <= left`, so the sum is within `bytes.len()`.
                pos += len as usize;
            },
            Item::Container(children) => {
                if open == MAX_PEER_NESTING {
                    return Err(MsgpackBoundsError::TooDeep);
                }
                if children > left {
                    return Err(MsgpackBoundsError::Truncated);
                }
                deepest = deepest.max(open + 1);
                if children > 0 {
                    remaining[open] = children;
                    open += 1;
                    continue;
                }
            },
        }
        // A value is complete: close every level it was the last child of.
        loop {
            if open == 0 {
                return Ok((pos, deepest));
            }
            remaining[open - 1] -= 1;
            if remaining[open - 1] > 0 {
                break;
            }
            open -= 1;
        }
    }
}

/// Checks the first MessagePack value of `bytes` against the bounds and returns the offset after it.
///
/// The bytes after that value are not examined.
///
/// # Errors
///
/// [`MsgpackBoundsError`] if the value nests deeper than [`MAX_PEER_NESTING`], if a declared length
/// or count exceeds the remaining bytes, if the value is cut short, or if it holds a reserved marker.
pub fn check_msgpack_bounds(bytes: &[u8]) -> Result<usize, MsgpackBoundsError> { walk(bytes).map(|(end, _)| end) }

/// The deepest nesting of arrays and maps in the first value of `bytes`; a scalar has depth 0.
///
/// Returns `usize::MAX` for a value outside the bounds.
pub fn max_nesting_depth(bytes: &[u8]) -> usize { walk(bytes).map(|(_, depth)| depth).unwrap_or(usize::MAX) }

/// Counts one peer value dropped as malformed.
pub fn count_dropped_peer_value() { DROPPED.fetch_add(1, Ordering::Relaxed); }

/// Checks a value received from a peer; a value outside the bounds is counted and logged at the
/// debug level under the name of `site`. The bytes are never logged.
///
/// # Errors
///
/// As [`check_msgpack_bounds`].
pub fn check_peer_value(bytes: &[u8], site: &str) -> Result<usize, MsgpackBoundsError> {
    check_msgpack_bounds(bytes).inspect_err(|e| {
        count_dropped_peer_value();
        log::debug!("{}: dropped a value outside the decoding bounds ({})", site, e);
    })
}

/// The number of peer values dropped as malformed since the process started.
pub fn dropped_peer_values() -> u64 { DROPPED.load(Ordering::Relaxed) }

#[cfg(test)]
mod tests {
    use super::*;

    /// `levels` one-element arrays around `inner`.
    fn nest_arrays(levels: usize, inner: &[u8]) -> Vec<u8> {
        let mut out = vec![0x91; levels];
        out.extend_from_slice(inner);
        out
    }

    /// `["Standard", <nested arrays>, nil]` whose total nesting is `depth`.
    fn positional_value(depth: usize) -> Vec<u8> {
        let mut out = vec![0x93, 0xa8];
        out.extend_from_slice(b"Standard");
        out.extend(nest_arrays(depth - 1, &[0xc0]));
        out.push(0xc0);
        out
    }

    /// `{"a": 1, "b": nil, "z": <nested arrays>}` whose total nesting is `depth`.
    fn keyed_value(depth: usize) -> Vec<u8> {
        let mut out = vec![0x83, 0xa1, b'a', 0x01, 0xa1, b'b', 0xc0, 0xa1, b'z'];
        out.extend(nest_arrays(depth - 1, &[0xc0]));
        out
    }

    /// `depth` maps, each holding the next under the key "k".
    fn nested_maps(depth: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for _ in 0..depth {
            out.extend_from_slice(&[0x81, 0xa1, b'k']);
        }
        out.push(0xc0);
        out
    }

    #[test]
    fn t4a_b_positional_value_at_the_bound_passes() {
        let bytes = positional_value(MAX_PEER_NESTING);
        assert_eq!(check_msgpack_bounds(&bytes), Ok(bytes.len()));
        assert_eq!(max_nesting_depth(&bytes), MAX_PEER_NESTING);
    }

    #[test]
    fn t4a_b_one_level_beyond_the_bound_is_dropped() {
        for bytes in [
            positional_value(MAX_PEER_NESTING + 1),
            keyed_value(MAX_PEER_NESTING + 1),
            nested_maps(MAX_PEER_NESTING + 1),
        ] {
            assert_eq!(check_msgpack_bounds(&bytes), Err(MsgpackBoundsError::TooDeep));
        }
    }

    #[test]
    fn t4a_b_keyed_value_at_the_bound_passes() {
        let bytes = keyed_value(MAX_PEER_NESTING);
        assert_eq!(check_msgpack_bounds(&bytes), Ok(bytes.len()));
        assert_eq!(max_nesting_depth(&bytes), MAX_PEER_NESTING);
    }

    #[test]
    fn t4a_b_ten_thousand_levels_are_dropped() {
        for bytes in [positional_value(10_000), keyed_value(10_000), nested_maps(10_000)] {
            assert_eq!(check_msgpack_bounds(&bytes), Err(MsgpackBoundsError::TooDeep));
        }
    }

    #[test]
    fn t4a_b_nested_maps_count_toward_the_bound() {
        let at_bound = nested_maps(MAX_PEER_NESTING);
        assert_eq!(check_msgpack_bounds(&at_bound), Ok(at_bound.len()));
        assert_eq!(max_nesting_depth(&at_bound), MAX_PEER_NESTING);
    }

    #[test]
    fn t4a_b_mixed_arrays_and_maps_count_toward_the_bound() {
        // [ {"k": [ {"k": ... nil } ] } ] alternating, 64 levels.
        let build = |levels: usize| {
            let mut out = Vec::new();
            for i in 0..levels {
                if i % 2 == 0 {
                    out.push(0x91);
                } else {
                    out.extend_from_slice(&[0x81, 0xa1, b'k']);
                }
            }
            out.push(0xc0);
            out
        };
        let at_bound = build(MAX_PEER_NESTING);
        assert_eq!(check_msgpack_bounds(&at_bound), Ok(at_bound.len()));
        assert_eq!(
            check_msgpack_bounds(&build(MAX_PEER_NESTING + 1)),
            Err(MsgpackBoundsError::TooDeep)
        );
    }

    #[test]
    fn t4a_b_empty_containers_count_toward_the_bound() {
        let mut at_bound = vec![0x91; MAX_PEER_NESTING - 1];
        at_bound.push(0x90);
        assert_eq!(check_msgpack_bounds(&at_bound), Ok(at_bound.len()));
        let mut beyond = vec![0x91; MAX_PEER_NESTING];
        beyond.push(0x90);
        assert_eq!(check_msgpack_bounds(&beyond), Err(MsgpackBoundsError::TooDeep));
    }

    #[test]
    fn t4a_b_depth_is_per_branch_not_total() {
        // Many sibling containers at depth 2 do not add up.
        let mut bytes = vec![0xdc, 0x01, 0x00];
        for _ in 0..256 {
            bytes.extend_from_slice(&[0x91, 0xc0]);
        }
        assert_eq!(check_msgpack_bounds(&bytes), Ok(bytes.len()));
        assert_eq!(max_nesting_depth(&bytes), 2);
    }

    #[test]
    fn t4a_c_oversized_bin_str_ext_headers_are_dropped() {
        let declared = 0xFFFF_FFF0u32.to_be_bytes();
        let mut bin = vec![0xc6];
        bin.extend_from_slice(&declared);
        bin.extend_from_slice(&[1, 2, 3]);
        let mut string = vec![0xdb];
        string.extend_from_slice(&declared);
        string.extend_from_slice(b"abc");
        let mut ext = vec![0xc9];
        ext.extend_from_slice(&declared);
        ext.extend_from_slice(&[5, 1, 2]);
        for bytes in [bin, string, ext] {
            assert_eq!(check_msgpack_bounds(&bytes), Err(MsgpackBoundsError::Truncated));
            // The same header as a child of an array.
            let mut inside = vec![0x91];
            inside.extend_from_slice(&bytes);
            assert_eq!(check_msgpack_bounds(&inside), Err(MsgpackBoundsError::Truncated));
        }
    }

    #[test]
    fn t4a_c_oversized_array_and_map_counts_are_dropped() {
        for marker in [0xddu8, 0xdf] {
            let mut bytes = vec![marker];
            bytes.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
            assert_eq!(check_msgpack_bounds(&bytes), Err(MsgpackBoundsError::Truncated));
        }
        // 16-bit counts and counts that exceed the remaining bytes by one.
        assert_eq!(
            check_msgpack_bounds(&[0xdc, 0xff, 0xff]),
            Err(MsgpackBoundsError::Truncated)
        );
        assert_eq!(
            check_msgpack_bounds(&[0xde, 0xff, 0xff, 0xc0]),
            Err(MsgpackBoundsError::Truncated)
        );
        // A map of two entries needs four bytes at least.
        assert_eq!(
            check_msgpack_bounds(&[0x82, 1, 2, 3]),
            Err(MsgpackBoundsError::Truncated)
        );
        assert_eq!(check_msgpack_bounds(&[0x82, 1, 2, 3, 4]), Ok(5));
        // An array of three elements with two present.
        assert_eq!(check_msgpack_bounds(&[0x93, 1, 2]), Err(MsgpackBoundsError::Truncated));
    }

    #[test]
    fn t4a_c_the_reserved_marker_is_dropped() {
        assert_eq!(check_msgpack_bounds(&[0xc1]), Err(MsgpackBoundsError::Reserved));
        assert_eq!(check_msgpack_bounds(&[0x91, 0xc1]), Err(MsgpackBoundsError::Reserved));
    }

    #[test]
    fn t4a_c_empty_input_is_dropped() {
        assert_eq!(check_msgpack_bounds(&[]), Err(MsgpackBoundsError::Truncated));
    }

    #[test]
    fn t4a_c_every_prefix_of_a_valid_value_is_cut_short() {
        // A map holding a nested array, a string, a bin, an ext, a float and an integer.
        let mut value = vec![0x84, 0xa1, b'a'];
        value.extend_from_slice(&[0x92, 0x91, 0x92, 0x01, 0x02, 0xcd, 0x01, 0x00]);
        value.extend_from_slice(&[0xa1, b'b', 0xc4, 0x03, 9, 8, 7]);
        value.extend_from_slice(&[0xa1, b'c', 0xd6, 0x05, 1, 2, 3, 4]);
        value.extend_from_slice(&[0xa1, b'd', 0xcb, 0, 0, 0, 0, 0, 0, 0xf0, 0x3f]);
        assert_eq!(check_msgpack_bounds(&value), Ok(value.len()));
        for cut in 0..value.len() {
            assert_eq!(
                check_msgpack_bounds(&value[..cut]),
                Err(MsgpackBoundsError::Truncated),
                "prefix of {cut} bytes"
            );
        }
    }

    #[test]
    fn t4a_c_every_marker_family_is_walked_to_its_exact_end() {
        let blob = |header: &[u8], data: usize| {
            let mut out = header.to_vec();
            out.extend(vec![0xaa; data]);
            out
        };
        let values: Vec<Vec<u8>> = vec![
            // nil, false, true, fixints.
            vec![0xc0],
            vec![0xc2],
            vec![0xc3],
            vec![0x00],
            vec![0x7f],
            vec![0xe0],
            vec![0xff],
            // uint8 to uint64.
            blob(&[0xcc], 1),
            blob(&[0xcd], 2),
            blob(&[0xce], 4),
            blob(&[0xcf], 8),
            // int8 to int64.
            blob(&[0xd0], 1),
            blob(&[0xd1], 2),
            blob(&[0xd2], 4),
            blob(&[0xd3], 8),
            // float32, float64.
            blob(&[0xca], 4),
            blob(&[0xcb], 8),
            // fixstr, str8, str16, str32.
            vec![0xa0],
            blob(&[0xa3], 3),
            blob(&[0xbf], 31),
            blob(&[0xd9, 40], 40),
            blob(&[0xda, 0x01, 0x00], 256),
            blob(&[0xdb, 0, 0, 0x01, 0x00], 256),
            // bin8, bin16, bin32.
            blob(&[0xc4, 0], 0),
            blob(&[0xc4, 5], 5),
            blob(&[0xc5, 0x01, 0x00], 256),
            blob(&[0xc6, 0, 0, 0x01, 0x00], 256),
            // fixarray, array16, array32.
            vec![0x90],
            vec![0x93, 1, 2, 3],
            vec![0xdc, 0x00, 0x00],
            vec![0xdc, 0x00, 0x02, 0xc0, 0xc0],
            vec![0xdd, 0, 0, 0, 0],
            vec![0xdd, 0, 0, 0, 1, 0xc0],
            // fixmap, map16, map32.
            vec![0x80],
            vec![0x82, 1, 2, 3, 4],
            vec![0xde, 0x00, 0x00],
            vec![0xde, 0x00, 0x01, 1, 2],
            vec![0xdf, 0, 0, 0, 0],
            vec![0xdf, 0, 0, 0, 1, 1, 2],
            // fixext1, 2, 4, 8, 16: a type byte and the data.
            blob(&[0xd4, 7], 1),
            blob(&[0xd5, 7], 2),
            blob(&[0xd6, 7], 4),
            blob(&[0xd7, 7], 8),
            blob(&[0xd8, 7], 16),
            // ext8, ext16, ext32: the length, a type byte, then the data.
            blob(&[0xc7, 0, 7], 0),
            blob(&[0xc7, 3, 7], 3),
            blob(&[0xc8, 0x01, 0x00, 7], 256),
            blob(&[0xc9, 0, 0, 0x01, 0x00, 7], 256),
        ];
        for value in values {
            assert_eq!(check_msgpack_bounds(&value), Ok(value.len()), "{value:02x?}");
            // Bytes after the value are not part of it.
            let mut with_tail = value.clone();
            with_tail.extend_from_slice(&[0xc1, 0xdd, 0xff]);
            assert_eq!(check_msgpack_bounds(&with_tail), Ok(value.len()), "{value:02x?}");
            // Every strict prefix of the value is cut short.
            for cut in 0..value.len() {
                assert_eq!(
                    check_msgpack_bounds(&value[..cut]),
                    Err(MsgpackBoundsError::Truncated),
                    "{value:02x?} cut at {cut}"
                );
            }
        }
    }

    #[test]
    fn t4a_e_bytes_after_the_first_value_are_ignored() {
        let mut bytes = positional_value(8);
        let end = bytes.len();
        bytes.extend_from_slice(&[0xc1, 0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(check_msgpack_bounds(&bytes), Ok(end));
        // Even a tail that is itself over-deep is not examined.
        bytes.truncate(end);
        bytes.extend(nest_arrays(10_000, &[0xc0]));
        assert_eq!(check_msgpack_bounds(&bytes), Ok(end));
    }

    #[test]
    fn should_count_dropped_values() {
        let before = dropped_peer_values();
        assert_eq!(check_peer_value(&[0x93, 1], "test"), Err(MsgpackBoundsError::Truncated));
        assert!(dropped_peer_values() > before);
        // A value inside the bounds is not counted by this call.
        assert_eq!(check_peer_value(&[0x01], "test"), Ok(1));
    }

    #[test]
    fn should_report_the_depth_of_scalars_and_flat_containers() {
        assert_eq!(max_nesting_depth(&[0x01]), 0);
        assert_eq!(max_nesting_depth(&[0x93, 1, 2, 3]), 1);
        assert_eq!(max_nesting_depth(&[0x91, 0x91, 0x01]), 2);
        assert_eq!(max_nesting_depth(&[0xc1]), usize::MAX);
    }
}
