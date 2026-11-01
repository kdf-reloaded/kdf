//! Tests of the LBRY and PIVX header readers (CRD ch.37 R37.5.2, R37.5.5, R37.5.6, R37.5.7) and of
//! the Qtum reader on real Runebase lists.
//!
//! The fixtures are real bytes served by public Electrum servers (see `test_data/README.md`). The
//! expected sizes, times and medians below are computed from the fixture bytes by the small
//! walkers of this file, not by the reader under test.

use super::*;
use hex::FromHex;

const PIVX_FILES: [(&str, &str); 9] = [
    ("v3", include_str!("../test_data/pivx_100000_x11_v3.hex")),
    ("v3_to_v4", include_str!("../test_data/pivx_863780_x11_v3_to_v4.hex")),
    ("v4", include_str!("../test_data/pivx_1500000_x11_v4.hex")),
    ("v5", include_str!("../test_data/pivx_1900000_x11_v5.hex")),
    ("v6_to_v7", include_str!("../test_data/pivx_2153195_x11_v6_to_v7.hex")),
    ("v7", include_str!("../test_data/pivx_2400000_x11_v7.hex")),
    ("v7_to_v8", include_str!("../test_data/pivx_2700495_x11_v7_to_v8.hex")),
    ("v10_v11", include_str!("../test_data/pivx_3000000_x11_v10_v11.hex")),
    ("tip", include_str!("../test_data/pivx_5642858_x11_v11_tip.hex")),
];
const LBC_FILES: [&str; 4] = [
    include_str!("../test_data/lbc_1_x11.hex"),
    include_str!("../test_data/lbc_500000_x11.hex"),
    include_str!("../test_data/lbc_2000000_x11.hex"),
    include_str!("../test_data/lbc_2209626_x11_tip.hex"),
];
const RUNES_TIP: &str = include_str!("../test_data/runes_2953197_x11_qtum_tip.hex");
const RUNES_500K: &str = include_str!("../test_data/runes_500000_x11_qtum.hex");

fn fixture(hex: &str) -> Vec<u8> { hex.trim().from_hex().expect("fixture is valid hex") }

fn u32_at(bytes: &[u8], at: usize) -> u32 { u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) }

/// The size of the PIVX header at the start of `bytes`, from its version by the chapter's rule.
fn pivx_size(bytes: &[u8]) -> usize {
    match u32_at(bytes, 0) {
        4..=6 | 8.. => 112,
        _ => 80,
    }
}

/// Qtum header size at the start of `bytes`: 80 + 32 + 32 + 36, then a compact-size signature
/// (one-byte prefix in these fixtures).
fn qtum_size(bytes: &[u8]) -> usize {
    let sig_len_at = 180;
    assert!(bytes[sig_len_at] < 0xfd, "one-byte compact size");
    sig_len_at + 1 + usize::from(bytes[sig_len_at])
}

fn split(bytes: &[u8], size: fn(&[u8]) -> usize) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        let (head, tail) = rest.split_at(size(rest));
        out.push(head);
        rest = tail;
    }
    out
}

/// Time of a header: bytes 68..72 in std-80 based layouts, 100..104 after the LBRY claim-trie root.
fn time_of(header: &[u8], at: usize) -> u32 { u32_at(header, at) }

fn median_of(mut times: Vec<u32>) -> u32 {
    times.sort_unstable();
    times[times.len() / 2]
}

#[test]
fn pivx_every_era_window_reads_with_exact_sizes_and_bytes() {
    for (name, hex) in PIVX_FILES {
        let served = fixture(hex);
        let pieces = split(&served, pivx_size);
        assert_eq!(pieces.len(), 11, "{name}");
        let headers = BlockHeader::list_from_served_bytes(&served, 11, CoinVariant::Pivx).unwrap();
        assert_eq!(headers.len(), 11, "{name}");
        for (header, piece) in headers.iter().zip(&pieces) {
            assert_eq!(
                &header.raw()[..],
                *piece,
                "{name}: re-encoding reproduces the served bytes"
            );
            assert_eq!(header.time, time_of(piece, 68), "{name}");
            assert_eq!(header.pivx_trailer.is_some(), piece.len() == 112, "{name}");
            assert_eq!(
                header.hash(),
                dhash256(piece),
                "{name}: identity is dSHA256 of the served bytes"
            );
        }
        // Each header alone, too.
        for piece in &pieces {
            let one = BlockHeader::from_served_bytes(piece, CoinVariant::Pivx).unwrap();
            assert_eq!(&one.raw()[..], *piece, "{name}");
        }
    }
}

#[test]
fn pivx_boundary_windows_have_the_expected_size_mix() {
    let sizes = |hex: &str| -> Vec<usize> { split(&fixture(hex), pivx_size).iter().map(|p| p.len()).collect() };
    let (a, b, c) = (PIVX_FILES[1].1, PIVX_FILES[4].1, PIVX_FILES[6].1);
    assert_eq!(sizes(a), [vec![80; 7], vec![112; 4]].concat());
    assert_eq!(sizes(b), [vec![112; 5], vec![80; 6]].concat());
    assert_eq!(sizes(c), [vec![80; 5], vec![112; 6]].concat());
    // Versions as the README states them.
    let versions = |hex: &str| -> Vec<u32> { split(&fixture(hex), pivx_size).iter().map(|p| u32_at(p, 0)).collect() };
    assert_eq!(versions(a), [vec![3; 7], vec![4; 4]].concat());
    assert_eq!(versions(b), [vec![6; 5], vec![7; 6]].concat());
    assert_eq!(versions(c), [vec![7; 5], vec![8; 6]].concat());
}

#[test]
fn pivx_tip_window_median_is_the_median_of_the_eleven_times() {
    let served = fixture(PIVX_FILES[8].1);
    let times: Vec<u32> = split(&served, pivx_size).iter().map(|p| time_of(p, 68)).collect();
    let parsed: Vec<u32> = BlockHeader::list_from_served_bytes(&served, 11, CoinVariant::Pivx)
        .unwrap()
        .iter()
        .map(|h| h.time)
        .collect();
    assert_eq!(parsed, times);
    assert_eq!(median_of(parsed), median_of(times));
}

#[test]
fn pivx_version_4_112_bytes_is_the_pivx_layout_not_equihash() {
    let served = fixture(PIVX_FILES[2].1);
    let first = &served[..112];
    assert_eq!(u32_at(first, 0), 4);
    let header = BlockHeader::from_served_bytes(first, CoinVariant::Pivx).unwrap();
    assert!(header.pivx_trailer.is_some());
    assert!(header.solution.is_none() && header.hash_final_sapling_root.is_none());
    assert!(matches!(header.nonce, BlockHeaderNonce::U32(_)));
    assert!(matches!(header.bits, BlockHeaderBits::Compact(_)));
    assert_eq!(&header.raw()[..], first);
    // The default reader (no chain_variant) still reads version 4 as Equihash and so rejects it,
    // exactly as before.
    assert!(BlockHeader::from_served_bytes(first, CoinVariant::Standard).is_err());
    assert!(BlockHeader::list_from_served_bytes(&served, 11, CoinVariant::Standard).is_err());
}

#[test]
fn the_default_reader_is_unchanged_on_pivx_bytes() {
    // 80-byte eras read as standard headers; the 112-byte eras are rejected (version 4 is
    // Equihash-shaped; the others leave 32 bytes unread).
    for (name, ok) in [("v3", true), ("v7", true), ("v4", false), ("v5", false), ("tip", false)] {
        let hex = PIVX_FILES.iter().find(|(n, _)| *n == name).unwrap().1;
        let served = fixture(hex);
        assert_eq!(
            BlockHeader::list_from_served_bytes(&served, 11, CoinVariant::Standard).is_ok(),
            ok,
            "{name}"
        );
    }
}

#[test]
fn pivx_rejects_malformed_lists_and_headers() {
    let served = fixture(PIVX_FILES[7].1); // 11 x 112
    let pivx = CoinVariant::Pivx;
    let mut trailing = served.clone();
    trailing.push(0);
    assert!(BlockHeader::list_from_served_bytes(&trailing, 11, pivx).is_err());
    assert!(BlockHeader::list_from_served_bytes(&served[..served.len() - 1], 11, pivx).is_err());
    // One header short: bytes of 10, count 11, and the reverse.
    assert!(BlockHeader::list_from_served_bytes(&served[..10 * 112], 11, pivx).is_err());
    assert!(BlockHeader::list_from_served_bytes(&served, 10, pivx).is_err());
    assert!(BlockHeader::list_from_served_bytes(&served, 12, pivx).is_err());
    assert!(BlockHeader::list_from_served_bytes(&[], 11, pivx).is_err());
    // A version-7 header served with 112 bytes is not std-80 and is not 80 bytes: rejected.
    let v7 = fixture(PIVX_FILES[5].1);
    let mut padded = v7[..80].to_vec();
    padded.extend_from_slice(&[0u8; 32]);
    assert!(BlockHeader::from_served_bytes(&padded, pivx).is_err());
    // A 112-byte header cut to 100 bytes.
    assert!(BlockHeader::from_served_bytes(&served[..100], pivx).is_err());
    // Trailing byte on a lone header.
    assert!(BlockHeader::from_served_bytes(&served[..113], pivx).is_err());
}

#[test]
fn pivx_structural_guard_rereads_exactly_80_byte_input_as_std_80() {
    // R37.5.6 applies to the version-chosen trailing field: a version-4 header served as exactly
    // 80 bytes is read as std-80 (a stripped header), and a list of exactly count x 80 bytes is
    // re-read with every header as std-80.
    let served = fixture(PIVX_FILES[2].1);
    let short = &served[..80];
    let header = BlockHeader::from_served_bytes(short, CoinVariant::Pivx).unwrap();
    assert!(header.pivx_trailer.is_none());
    assert_eq!(&header.raw()[..], short);
    let eighty: Vec<u8> = served.chunks(112).flat_map(|c| c[..80].to_vec()).collect();
    assert_eq!(eighty.len(), 11 * 80);
    let headers = BlockHeader::list_from_served_bytes(&eighty, 11, CoinVariant::Pivx).unwrap();
    assert!(headers.iter().all(|h| h.pivx_trailer.is_none()));
    assert_eq!(reencode(&headers), eighty);
    // 79 or 81 bytes get no re-read.
    assert!(BlockHeader::from_served_bytes(&served[..79], CoinVariant::Pivx).is_err());
    assert!(BlockHeader::from_served_bytes(&served[..81], CoinVariant::Pivx).is_err());
}

fn reencode(headers: &[BlockHeader]) -> Vec<u8> { headers.iter().flat_map(|h| h.raw().to_vec()).collect() }

#[test]
fn lbc_windows_read_unconditionally_at_112_bytes() {
    for hex in LBC_FILES {
        let served = fixture(hex);
        assert_eq!(served.len(), 11 * 112);
        let headers = BlockHeader::list_from_served_bytes(&served, 11, CoinVariant::Lbc).unwrap();
        for (header, piece) in headers.iter().zip(served.chunks(112)) {
            assert_eq!(header.version, 0x2000_0000);
            assert!(header.claim_trie_root.is_some());
            assert_eq!(
                header.claim_trie_root.unwrap().as_slice(),
                &piece[68..100],
                "claim-trie root sits after the Merkle root"
            );
            assert_eq!(header.time, time_of(piece, 100));
            assert_eq!(&header.raw()[..], piece);
            assert_eq!(header.hash(), dhash256(piece));
            // Not read as Qtum, AuxPoW, KawPoW or Firo by its version value.
            assert!(header.hash_state_root.is_none() && header.aux_pow.is_none() && header.n_height.is_none());
        }
        let one = BlockHeader::from_served_bytes(&served[..112], CoinVariant::Lbc).unwrap();
        assert_eq!(&one.raw()[..], &served[..112]);
    }
}

#[test]
fn lbc_tip_window_median_is_the_median_of_the_eleven_times() {
    let served = fixture(LBC_FILES[3]);
    let times: Vec<u32> = served.chunks(112).map(|p| time_of(p, 100)).collect();
    let parsed: Vec<u32> = BlockHeader::list_from_served_bytes(&served, 11, CoinVariant::Lbc)
        .unwrap()
        .iter()
        .map(|h| h.time)
        .collect();
    assert_eq!(parsed, times);
    assert_eq!(median_of(parsed), median_of(times));
}

#[test]
fn lbc_has_no_std_80_reread_and_rejects_malformed_input() {
    // R37.5.6: the guard does not apply to a layout the family fixes unconditionally.
    let served = fixture(LBC_FILES[0]);
    let lbc = CoinVariant::Lbc;
    let eighty: Vec<u8> = served.chunks(112).flat_map(|c| c[..80].to_vec()).collect();
    assert!(BlockHeader::list_from_served_bytes(&eighty, 11, lbc).is_err());
    assert!(BlockHeader::from_served_bytes(&served[..80], lbc).is_err());
    let mut trailing = served.clone();
    trailing.push(0);
    assert!(BlockHeader::list_from_served_bytes(&trailing, 11, lbc).is_err());
    assert!(BlockHeader::list_from_served_bytes(&served[..served.len() - 1], 11, lbc).is_err());
    assert!(BlockHeader::list_from_served_bytes(&served[..10 * 112], 11, lbc).is_err());
    assert!(BlockHeader::list_from_served_bytes(&served, 10, lbc).is_err());
    assert!(BlockHeader::list_from_served_bytes(&served, 12, lbc).is_err());
    assert!(BlockHeader::from_served_bytes(&served[..113], lbc).is_err());
}

#[test]
fn qtum_reads_the_real_runebase_lists_exactly() {
    for (hex, size, tip) in [(RUNES_TIP, 246, true), (RUNES_500K, 251, false)] {
        let served = fixture(hex);
        assert_eq!(served.len(), 11 * size, "tip={tip}");
        let pieces = split(&served, qtum_size);
        assert_eq!(pieces.len(), 11);
        assert!(pieces.iter().all(|p| p.len() == size));
        let headers = BlockHeader::list_from_served_bytes(&served, 11, CoinVariant::Qtum)
            .unwrap_or_else(|e| panic!("the Qtum reader rejects the Runebase list (tip={tip}): {e:?}"));
        let times: Vec<u32> = pieces.iter().map(|p| time_of(p, 68)).collect();
        assert_eq!(headers.iter().map(|h| h.time).collect::<Vec<_>>(), times);
        assert_eq!(reencode(&headers), served, "exact byte consumption and re-encoding");
        assert_eq!(median_of(headers.iter().map(|h| h.time).collect()), median_of(times));
        assert!(headers.iter().all(|h| h.hash_state_root.is_some()));
        let one = BlockHeader::from_served_bytes(pieces[0], CoinVariant::Qtum).unwrap();
        assert_eq!(&one.raw()[..], pieces[0]);
        // Wrong count or a trailing byte is rejected whole.
        assert!(BlockHeader::list_from_served_bytes(&served, 10, CoinVariant::Qtum).is_err());
        let mut trailing = served.clone();
        trailing.push(0);
        assert!(BlockHeader::list_from_served_bytes(&trailing, 11, CoinVariant::Qtum).is_err());
    }
}
