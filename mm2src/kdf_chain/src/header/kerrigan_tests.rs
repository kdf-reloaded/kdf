//! Tests of the Kerrigan header reader (CRD ch.37 R37.5.11, T37.5.11.1, T37.5.11.2, T37.5.11.5).
//!
//! The fixtures are real Kerrigan chain data from a public Electrum server (see `test_data/README.md`).
//! The expected sizes, times and medians below are computed from the fixture bytes by the small walkers
//! of this file, or stated as constants read from the chain, not produced by the code under test.

use super::*;
use hex::FromHex;

const WINDOW: &str = include_str!("../test_data/krgn_188818_mixed_sizes.hex");
const WINDOW_ALL_SIZES: &str = include_str!("../test_data/krgn_191181_window11.hex");
const LONE_X11: &str = include_str!("../test_data/krgn_188629_lone_x11.hex");
const LONE_KAWPOW: &str = include_str!("../test_data/krgn_188823_lone_kawpow.hex");
const LONE_EQUIHASH_200_9: &str = include_str!("../test_data/krgn_188818_lone_equihash_200_9.hex");
const LONE_EQUIHASH_192_7: &str = include_str!("../test_data/krgn_188820_lone_equihash_192_7.hex");

/// The sizes of the eleven headers of the window, in order (8492 bytes).
const WINDOW_SIZES: [usize; 11] = [1487, 1487, 543, 543, 543, 124, 124, 1487, 1487, 543, 124];

fn fixture(hex: &str) -> Vec<u8> { hex.trim().from_hex().expect("fixture is valid hex") }

fn u32_at(bytes: &[u8], at: usize) -> u32 { u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) }

/// The algorithm of the header at the start of `bytes`: bits 8 to 11 of its version.
fn algorithm(bytes: &[u8]) -> u32 { (u32_at(bytes, 0) >> 8) & 0xF }

/// The size of the header at the start of `bytes`, from its algorithm by the chapter's table: 80 for X11,
/// 124 for KawPoW, and for either Equihash form 140 bytes, a compact-size length and the solution.
fn kerrigan_size(bytes: &[u8]) -> usize {
    match algorithm(bytes) {
        0 => 80,
        2 => 124,
        4 | 6 => {
            assert_eq!(bytes[140], 0xfd, "a three-byte compact size");
            140 + 3 + usize::from(u16::from_le_bytes([bytes[141], bytes[142]]))
        },
        other => panic!("not a Kerrigan algorithm: {other}"),
    }
}

/// Where the time field sits in the header at the start of `bytes`: byte 100 for Equihash (after the
/// 32-byte reserved field), byte 68 otherwise.
fn time_offset(bytes: &[u8]) -> usize {
    match algorithm(bytes) {
        4 | 6 => 100,
        _ => 68,
    }
}

fn time_of(header: &[u8]) -> u32 { u32_at(header, time_offset(header)) }

fn split(bytes: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        let (head, tail) = rest.split_at(kerrigan_size(rest));
        out.push(head);
        rest = tail;
    }
    out
}

fn median_of(mut times: Vec<u32>) -> u32 {
    times.sort_unstable();
    times[times.len() / 2]
}

fn reencode(headers: &[BlockHeader]) -> Vec<u8> { headers.iter().flat_map(|h| h.raw().to_vec()).collect() }

/// `bytes` with the algorithm bits of the version at the start replaced by `bits`.
fn with_algorithm_bits(bytes: &[u8], bits: u32) -> Vec<u8> {
    let mut out = bytes.to_vec();
    let version = (u32_at(bytes, 0) & !0x0F00) | (bits << 8);
    out[..4].copy_from_slice(&version.to_le_bytes());
    out
}

fn custom_text(error: serialization::Error) -> String {
    match error {
        serialization::Error::Custom(text) => text,
        other => panic!("expected a codec error that names the version, got {other:?}"),
    }
}

#[test]
fn the_window_reads_header_by_header_at_the_true_boundaries() {
    let served = fixture(WINDOW);
    assert_eq!(served.len(), 8492);
    let pieces = split(&served);
    assert_eq!(pieces.iter().map(|p| p.len()).collect::<Vec<_>>(), WINDOW_SIZES);
    let headers = BlockHeader::list_from_served_bytes(&served, 11, CoinVariant::Kerrigan).unwrap();
    assert_eq!(headers.len(), 11);
    for (index, (header, piece)) in headers.iter().zip(&pieces).enumerate() {
        assert_eq!(
            &header.raw()[..],
            *piece,
            "header {index}: re-encoding reproduces the served bytes"
        );
        assert_eq!(header.time, time_of(piece), "header {index}");
        assert!(
            header.time >= 1_794_969_614 && header.time <= 1_794_971_576,
            "header {index}"
        );
    }
    // The first and the last time, and the median of the eleven, as read from the chain.
    assert_eq!(headers[0].time, 1_794_969_614);
    assert_eq!(headers[10].time, 1_794_971_576);
    let times: Vec<u32> = headers.iter().map(|h| h.time).collect();
    assert_eq!(times, pieces.iter().map(|p| time_of(p)).collect::<Vec<_>>());
    assert_eq!(median_of(times), 1_794_970_885);
    assert_eq!(reencode(&headers), served);
}

/// CRD ch.37 T37.5.11.1: a real window of heights 191181 to 191191 holds all four sizes (80, 124, 543, 1487).
#[test]
fn the_window_of_all_four_sizes_reads_at_the_true_boundaries() {
    let served = fixture(WINDOW_ALL_SIZES);
    assert_eq!(served.len(), 7085);
    let sizes = [124, 124, 1487, 80, 543, 543, 543, 124, 543, 1487, 1487];
    let pieces = split(&served);
    assert_eq!(pieces.iter().map(|p| p.len()).collect::<Vec<_>>(), sizes);
    let headers = BlockHeader::list_from_served_bytes(&served, 11, CoinVariant::Kerrigan).unwrap();
    assert_eq!(headers.len(), 11);
    for (index, (header, piece)) in headers.iter().zip(&pieces).enumerate() {
        let bits = (header.version & 0x0F00) as usize;
        let expected = match bits {
            0x0000 => 80,
            0x0200 => 124,
            0x0400 => 1487,
            0x0600 => 543,
            other => panic!("header {index}: not a Kerrigan algorithm: {other:#x}"),
        };
        assert_eq!(
            piece.len(),
            expected,
            "header {index}: the algorithm bits match the size"
        );
        assert_eq!(
            &header.raw()[..],
            *piece,
            "header {index}: re-encoding reproduces the served bytes"
        );
        assert_eq!(header.time, time_of(piece), "header {index}");
    }
    assert_eq!(reencode(&headers), served);
    // The list read consumes the bytes exactly: one more header is not there to be read.
    assert!(BlockHeader::list_from_served_bytes(&served, 12, CoinVariant::Kerrigan).is_err());
}

#[test]
fn each_algorithm_fills_its_own_fields() {
    let served = fixture(WINDOW);
    let headers = BlockHeader::list_from_served_bytes(&served, 11, CoinVariant::Kerrigan).unwrap();
    for header in &headers {
        match header.version & 0x0F00 {
            0x200 => {
                // KawPoW: the 32-bit nonce is kept and the trailer is stored apart from the Ravencoin
                // fields, which would make the serializer drop that nonce.
                assert!(header.kerrigan_kawpow.is_some());
                assert!(matches!(header.nonce, BlockHeaderNonce::U32(_)));
                assert!(header.n_height.is_none() && header.n_nonce_u64.is_none() && header.mix_hash.is_none());
                assert!(header.solution.is_none() && header.hash_final_sapling_root.is_none());
                assert_eq!(header.raw().len(), 124);
            },
            0x400 | 0x600 => {
                let solution = header.solution.as_ref().expect("the Equihash form carries a solution");
                assert_eq!(
                    solution.len(),
                    if header.version & 0x0F00 == 0x400 { 1344 } else { 400 }
                );
                assert!(matches!(header.nonce, BlockHeaderNonce::H256(_)));
                assert!(matches!(header.bits, BlockHeaderBits::Compact(_)));
                assert_eq!(
                    header.hash_final_sapling_root,
                    Some(H256::default()),
                    "the reserved field is all zero on the chain and is kept"
                );
                assert!(header.kerrigan_kawpow.is_none());
            },
            other => panic!("the window holds no header of algorithm bits {other:#x}"),
        }
    }
}

#[test]
fn lone_headers_parse_their_time_and_reserialize_byte_identically() {
    // (name, hex, bytes, version, time read from the chain, offset of that time)
    let cases = [
        ("X11 188629", LONE_X11, 80, 0x2000_0000u32, 1_794_947_320u32, 68),
        ("KawPoW 188823", LONE_KAWPOW, 124, 0x2000_0200, 1_794_970_885, 68),
        (
            "Equihash(200,9) 188818",
            LONE_EQUIHASH_200_9,
            1487,
            0x2000_0400,
            1_794_969_614,
            100,
        ),
        (
            "Equihash(192,7) 188820",
            LONE_EQUIHASH_192_7,
            543,
            0x2000_0600,
            1_794_970_194,
            100,
        ),
    ];
    for (name, hex, size, version, time, offset) in cases {
        let served = fixture(hex);
        assert_eq!(served.len(), size, "{name}");
        let header = BlockHeader::from_served_bytes(&served, CoinVariant::Kerrigan).unwrap();
        assert_eq!(header.version, version, "{name}");
        assert_eq!(header.time, time, "{name}");
        assert_eq!(u32_at(&served, offset), time, "{name}: the time sits at byte {offset}");
        assert_ne!(header.time, 0, "{name}: never the zero reserved field");
        assert_eq!(
            &header.raw()[..],
            &served[..],
            "{name}: byte-identical re-serialization"
        );
        assert_eq!(
            header.hash(),
            dhash256(&served),
            "{name}: the identity hash stays a double SHA-256, not the chain's X11"
        );
    }
}

#[test]
fn the_kawpow_trailer_keeps_its_fields_after_the_32_bit_nonce() {
    let served = fixture(LONE_KAWPOW);
    let header = BlockHeader::from_served_bytes(&served, CoinVariant::Kerrigan).unwrap();
    assert!(matches!(header.nonce, BlockHeaderNonce::U32(n) if n == u32_at(&served, 76)));
    let trailer = header.kerrigan_kawpow.expect("a KawPoW header carries the trailer");
    assert_eq!(trailer.height, u32_at(&served, 80));
    assert_eq!(trailer.height, 188_823, "the chain's own height field");
    assert_eq!(trailer.nonce64, u64::from_le_bytes(served[84..92].try_into().unwrap()));
    assert_eq!(&trailer.mix_hash[..], &served[92..124]);
}

#[test]
fn the_x11_header_is_the_standard_80_bytes_without_extension_fields() {
    let served = fixture(LONE_X11);
    let header = BlockHeader::from_served_bytes(&served, CoinVariant::Kerrigan).unwrap();
    assert!(header.kerrigan_kawpow.is_none() && header.solution.is_none() && header.hash_final_sapling_root.is_none());
    assert!(matches!(header.nonce, BlockHeaderNonce::U32(_)));
}

#[test]
fn the_window_is_refused_by_every_reader_that_is_not_kerrigan() {
    // T37.5.11.5: without the Kerrigan layout the list read yields no value.
    let served = fixture(WINDOW);
    for variant in [
        CoinVariant::Standard,
        CoinVariant::Qtum,
        CoinVariant::Riecoin,
        CoinVariant::KawpowAuxpowMixed,
        CoinVariant::Lbc,
        CoinVariant::Pivx,
    ] {
        assert!(
            BlockHeader::list_from_served_bytes(&served, 11, variant).is_err(),
            "{variant:?} must not read the Kerrigan window"
        );
    }
}

#[test]
fn versions_with_an_unused_algorithm_value_are_refused_naming_the_version() {
    let x11 = fixture(LONE_X11);
    for bits in [1u32, 3, 5, 7, 8, 9, 10, 11, 12, 13, 14, 15] {
        let patched = with_algorithm_bits(&x11, bits);
        let version = u32_at(&patched, 0);
        let text = custom_text(BlockHeader::from_served_bytes(&patched, CoinVariant::Kerrigan).unwrap_err());
        assert!(text.contains(&format!("{version:#010x}")), "bits {bits}: {text}");
        // The same version inside a list is refused too.
        let mut window = fixture(WINDOW);
        let at = WINDOW_SIZES[..5].iter().sum::<usize>();
        window[at..at + 4].copy_from_slice(&version.to_le_bytes());
        assert!(
            BlockHeader::list_from_served_bytes(&window, 11, CoinVariant::Kerrigan).is_err(),
            "bits {bits} inside a list"
        );
    }
    // The algorithm the bytes were mined with is read, and KawPoW needs its 44-byte trailer.
    assert!(BlockHeader::from_served_bytes(&with_algorithm_bits(&x11, 0), CoinVariant::Kerrigan).is_ok());
    assert!(BlockHeader::from_served_bytes(&with_algorithm_bits(&x11, 2), CoinVariant::Kerrigan).is_err());
}

#[test]
fn an_equihash_solution_longer_than_1400_bytes_is_refused() {
    let equihash = fixture(LONE_EQUIHASH_200_9);
    // The chain's own bound is the only length check: 1400 reads, 1401 does not, whatever the algorithm.
    for (length, ok) in [(1400usize, true), (1401, false), (1344, true), (0, true)] {
        let mut header = equihash[..140].to_vec();
        let prefix = match length {
            0 => vec![0x00],
            n => {
                let mut p = vec![0xfd];
                p.extend_from_slice(&(n as u16).to_le_bytes());
                p
            },
        };
        header.extend_from_slice(&prefix);
        header.extend(std::iter::repeat_n(0x7e, length));
        let read = BlockHeader::from_served_bytes(&header, CoinVariant::Kerrigan);
        assert_eq!(read.is_ok(), ok, "solution length {length}");
        if let Ok(read) = read {
            assert_eq!(&read.raw()[..], &header[..], "solution length {length}");
        }
    }
    // A length within the bound with no bytes behind it runs off the end.
    let mut cut = equihash[..140].to_vec();
    cut.extend_from_slice(&[0xfd, 0x78, 0x05]);
    assert!(BlockHeader::from_served_bytes(&cut, CoinVariant::Kerrigan).is_err());
}

#[test]
fn a_list_that_is_short_long_or_miscounted_is_refused_whole() {
    let served = fixture(WINDOW);
    let kerrigan = CoinVariant::Kerrigan;
    assert!(BlockHeader::list_from_served_bytes(&served[..served.len() - 1], 11, kerrigan).is_err());
    let mut long = served.clone();
    long.push(0);
    assert!(BlockHeader::list_from_served_bytes(&long, 11, kerrigan).is_err());
    assert!(BlockHeader::list_from_served_bytes(&served, 10, kerrigan).is_err());
    assert!(BlockHeader::list_from_served_bytes(&served, 12, kerrigan).is_err());
    assert!(BlockHeader::list_from_served_bytes(&[], 11, kerrigan).is_err());
    // Ten headers read alone from the first ten pieces.
    let ten = &served[..served.len() - WINDOW_SIZES[10]];
    assert!(BlockHeader::list_from_served_bytes(ten, 10, kerrigan).is_ok());
    assert!(BlockHeader::list_from_served_bytes(ten, 11, kerrigan).is_err());
}

#[test]
fn a_lone_header_that_is_cut_or_padded_is_refused() {
    let kerrigan = CoinVariant::Kerrigan;
    let equihash = fixture(LONE_EQUIHASH_200_9);
    // An Equihash header served as 80 bytes (the standard length) is not re-read as a standard header.
    assert!(BlockHeader::from_served_bytes(&equihash[..80], kerrigan).is_err());
    assert!(BlockHeader::from_served_bytes(&equihash[..equihash.len() - 1], kerrigan).is_err());
    assert!(BlockHeader::from_served_bytes(&equihash[..140], kerrigan).is_err());
    let mut padded = equihash.clone();
    padded.push(0);
    assert!(BlockHeader::from_served_bytes(&padded, kerrigan).is_err());
    // KawPoW at the 120-byte Ravencoin length, and one byte over.
    let kawpow = fixture(LONE_KAWPOW);
    assert!(BlockHeader::from_served_bytes(&kawpow[..120], kerrigan).is_err());
    let mut padded = kawpow.clone();
    padded.push(0);
    assert!(BlockHeader::from_served_bytes(&padded, kerrigan).is_err());
    // X11 at 79 and 81 bytes.
    let x11 = fixture(LONE_X11);
    assert!(BlockHeader::from_served_bytes(&x11[..79], kerrigan).is_err());
    let mut padded = x11.clone();
    padded.push(0);
    assert!(BlockHeader::from_served_bytes(&padded, kerrigan).is_err());
}

#[test]
fn the_x11_header_reads_the_same_under_the_standard_reader() {
    // The X11 header is an ordinary 80-byte header for the standard reader too.
    let x11 = fixture(LONE_X11);
    let standard = BlockHeader::from_served_bytes(&x11, CoinVariant::Standard).unwrap();
    let kerrigan = BlockHeader::from_served_bytes(&x11, CoinVariant::Kerrigan).unwrap();
    assert_eq!(standard, kerrigan);
    assert!(standard.kerrigan_kawpow.is_none());
}
