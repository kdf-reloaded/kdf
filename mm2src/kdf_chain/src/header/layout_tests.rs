//! Tests of the Reloaded-only header layouts (CRD ch.37 R37.5.9), one to one with the R37.5.9 group
//! of the §37.8 acceptance criteria.
//!
//! The fixtures are real bytes served by public Electrum servers over protocol 1.4
//! (`blockchain.block.headers` / `blockchain.block.header`), recorded 2026-10-25:
//! - Riecoin: `ric.electrum1.cipig.net:20074` (SSL), tip 2,658,975 at the time;
//! - Meowcoin: `electrs.mewccrypto.com:50002` (SSL), tip 2,124,914 at the time. Native headers
//!   (version `0x30090000`, AuxPoW flag clear) are served as 120 bytes, merged-mined ones
//!   (version `0x30090100`, flag set) stripped to 80 bytes, interleaved in the chain's own order.

use super::*;
use hex::FromHex;

const RIC_LIST: &str = include_str!("../test_data/ric_2658945_x11_v20000000.hex");
const RIC_LONE: &str = include_str!("../test_data/ric_2658955_lone_v20000000.hex");
/// Heights 2,124,756..=2,124,766: 1,240 bytes; lengths 8 x 120, 2 x 80, 1 x 120.
const MEWC_MIXED: &str = include_str!("../test_data/mewc_2124756_x11_9native_2auxpow.hex");
/// Heights 2,124,727..=2,124,729: 280 bytes; lengths 80, 80, 120.
const MEWC_TAIL: &str = include_str!("../test_data/mewc_2124727_x3_80_80_120.hex");
/// Heights 2,124,580..=2,124,590: 1,320 bytes, eleven native headers.
const MEWC_ALL_NATIVE: &str = include_str!("../test_data/mewc_2124580_x11_all_native.hex");
/// Heights 2,124,789..=2,124,794: 480 bytes, six merged-mined headers.
const MEWC_ALL_AUXPOW: &str = include_str!("../test_data/mewc_2124789_x6_all_auxpow.hex");
const MEWC_LONE_NATIVE: &str = include_str!("../test_data/mewc_2124580_lone_native.hex");
const MEWC_LONE_AUXPOW: &str = include_str!("../test_data/mewc_2124789_lone_auxpow.hex");

const RIECOIN: CoinVariant = CoinVariant::Riecoin;
const MIXED: CoinVariant = CoinVariant::KawpowAuxpowMixed;

fn fixture(hex: &str) -> Vec<u8> { hex.trim().from_hex().expect("fixture is valid hex") }

/// Time field of the record starting at `start`: bytes 68..72 of every layout. Used only to state
/// the expected value independently of the parser under test.
fn time_at(bytes: &[u8], start: usize) -> u32 { u32::from_le_bytes(bytes[start + 68..start + 72].try_into().unwrap()) }

fn median_of(mut times: Vec<u32>) -> u32 {
    times.sort_unstable();
    times[times.len() / 2]
}

fn reencode(headers: &[BlockHeader]) -> Vec<u8> { headers.iter().flat_map(|h| h.raw().to_vec()).collect() }

fn lens(headers: &[BlockHeader]) -> Vec<usize> { headers.iter().map(|h| h.raw().len()).collect() }

// ---- "riecoin" -------------------------------------------------------------------------------

#[test]
fn riecoin_list_of_eleven_parses_and_median_is_of_parsed_times() {
    let served = fixture(RIC_LIST);
    assert_eq!(served.len(), 11 * 112);
    let headers = BlockHeader::list_from_served_bytes(&served, 11, RIECOIN).unwrap();
    assert_eq!(headers.len(), 11);
    assert!(headers.iter().all(|h| h.version == 0x2000_0000 && h.wide_time));
    let times: Vec<u32> = headers.iter().map(|h| h.time).collect();
    let expected: Vec<u32> = (0..11).map(|i| time_at(&served, i * 112)).collect();
    assert_eq!(times, expected);
    assert_eq!(median_of(times), 1_792_892_599);
    // Stored bytes = served bytes, 112 each.
    assert_eq!(lens(&headers), vec![112; 11]);
    assert_eq!(reencode(&headers), served);
}

#[test]
fn riecoin_lone_header_round_trips_at_112_bytes() {
    let served = fixture(RIC_LONE);
    assert_eq!(served.len(), 112);
    let header = BlockHeader::from_served_bytes(&served, RIECOIN).unwrap();
    assert!(matches!(header.nonce, BlockHeaderNonce::H256(_)));
    assert_eq!(&header.raw()[..], &served[..]);
    // The lone header is the last one of the list fixture.
    let list = fixture(RIC_LIST);
    assert_eq!(served, list[10 * 112..]);
}

#[test]
fn riecoin_time_above_the_u32_range_is_a_parse_error_not_truncated() {
    let mut served = fixture(RIC_LONE);
    served[68..76].copy_from_slice(&(u64::from(u32::MAX) + 1).to_le_bytes());
    assert!(matches!(
        BlockHeader::from_served_bytes(&served, RIECOIN),
        Err(serialization::Error::Custom(_))
    ));
    let mut list = fixture(RIC_LIST);
    list[112 + 68..112 + 76].copy_from_slice(&(1u64 << 40).to_le_bytes());
    assert!(BlockHeader::list_from_served_bytes(&list, 11, RIECOIN).is_err());
    // The largest in-range value is accepted and survives the round trip.
    let mut served = fixture(RIC_LONE);
    served[68..76].copy_from_slice(&u64::from(u32::MAX).to_le_bytes());
    let header = BlockHeader::from_served_bytes(&served, RIECOIN).unwrap();
    assert_eq!(header.time, u32::MAX);
    assert_eq!(&header.raw()[..], &served[..]);
}

#[test]
fn riecoin_layout_is_fixed_length_with_no_80_byte_fallback() {
    let list = fixture(RIC_LIST);
    // Wrong count for the bytes, either way, rejects the list whole.
    assert!(BlockHeader::list_from_served_bytes(&list, 10, RIECOIN).is_err());
    assert!(BlockHeader::list_from_served_bytes(&list, 12, RIECOIN).is_err());
    // Exactly count x 80 bytes is not re-read as std-80 under this layout.
    assert!(BlockHeader::list_from_served_bytes(&list[..2 * 80], 2, RIECOIN).is_err());
    assert!(BlockHeader::from_served_bytes(&list[..80], RIECOIN).is_err());
    assert!(BlockHeader::from_served_bytes(&list[..111], RIECOIN).is_err());
}

// ---- "kawpow_auxpow_mixed" -------------------------------------------------------------------

#[test]
fn mixed_interleaved_list_parses_at_true_boundaries() {
    let served = fixture(MEWC_MIXED);
    assert_eq!(served.len(), 1_240);
    let headers = BlockHeader::list_from_served_bytes(&served, 11, MIXED).unwrap();
    assert_eq!(lens(&headers), [120, 120, 120, 120, 120, 120, 120, 120, 80, 80, 120]);
    // The native/merged-mined split is the AuxPoW flag of each header's own version.
    for h in &headers {
        assert_eq!(h.n_height.is_some(), h.version & 0x100 == 0, "{:#x}", h.version);
        assert_eq!(h.aux_pow, None);
    }
    assert_eq!(headers[0].version, 0x3009_0000);
    assert_eq!(headers[8].version, 0x3009_0100);
    let times: Vec<u32> = headers.iter().map(|h| h.time).collect();
    assert_eq!(times[8], 1_792_885_332);
    assert_eq!(times[10], 1_792_885_727);
    assert_eq!(median_of(times), 1_792_885_244);
    // Round trip: what is stored equals what was served.
    assert_eq!(reencode(&headers), served);
}

#[test]
fn mixed_tail_shape_80_80_120_parses() {
    let served = fixture(MEWC_TAIL);
    assert_eq!(served.len(), 280);
    let headers = BlockHeader::list_from_served_bytes(&served, 3, MIXED).unwrap();
    assert_eq!(lens(&headers), [80, 80, 120]);
    let times: Vec<u32> = headers.iter().map(|h| h.time).collect();
    assert_eq!(times, [1_792_882_513, 1_792_882_545, 1_792_882_604]);
    assert_eq!(median_of(times), 1_792_882_545);
    assert_eq!(reencode(&headers), served);
}

#[test]
fn mixed_uniform_lists_parse() {
    let native = fixture(MEWC_ALL_NATIVE);
    let headers = BlockHeader::list_from_served_bytes(&native, 11, MIXED).unwrap();
    assert_eq!(lens(&headers), vec![120; 11]);
    assert_eq!(reencode(&headers), native);

    let auxpow = fixture(MEWC_ALL_AUXPOW);
    let headers = BlockHeader::list_from_served_bytes(&auxpow, 6, MIXED).unwrap();
    assert_eq!(lens(&headers), vec![80; 6]);
    assert!(headers.iter().all(|h| h.version & 0x100 != 0 && h.aux_pow.is_none()));
    assert_eq!(reencode(&headers), auxpow);
}

#[test]
fn mixed_native_header_is_stored_as_120_bytes() {
    let served = fixture(MEWC_LONE_NATIVE);
    assert_eq!(served.len(), 120);
    let header = BlockHeader::from_served_bytes(&served, MIXED).unwrap();
    assert_eq!(header.raw().len(), 120);
    assert_eq!(&header.raw()[..], &served[..]);
    assert!(header.n_height.is_some() && header.n_nonce_u64.is_some() && header.mix_hash.is_some());
    // Its identity hash is over the full served bytes (no chain hash is specified for this form).
    assert_eq!(header.hash(), H256::from(dhash256(&served).take()));
}

#[test]
fn mixed_stripped_auxpow_header_is_80_bytes() {
    let served = fixture(MEWC_LONE_AUXPOW);
    assert_eq!(served.len(), 80);
    let header = BlockHeader::from_served_bytes(&served, MIXED).unwrap();
    assert_eq!(header.aux_pow, None);
    assert_eq!(&header.raw()[..], &served[..]);
    assert_eq!(header.hash(), H256::from(dhash256(&served).take()));
}

#[test]
fn mixed_lone_auxpow_header_with_its_proof_is_read_as_std80_plus_proof() {
    // A single flag-set header served longer than 80 bytes is std-80 followed by the AuxPoW proof
    // (R37.5.9b). The proof here is minimal: one empty coinbase, empty branches, a std-80 parent.
    let base = fixture(MEWC_LONE_AUXPOW);
    let mut served = base.clone();
    served.extend_from_slice(&1u32.to_le_bytes()); // coinbase version
    served.push(1); // one input
    served.extend_from_slice(&[0u8; 32]); // prev txid
    served.extend_from_slice(&u32::MAX.to_le_bytes()); // prev index
    served.push(0); // empty script
    served.extend_from_slice(&u32::MAX.to_le_bytes()); // sequence
    served.push(1); // one output
    served.extend_from_slice(&0u64.to_le_bytes()); // value
    served.push(0); // empty script
    served.extend_from_slice(&0u32.to_le_bytes()); // lock time
    served.extend_from_slice(&[0u8; 32]); // parent block hash
    served.push(0); // coinbase branch: no hashes
    served.extend_from_slice(&0i32.to_le_bytes()); // side mask
    served.push(0); // blockchain branch: no hashes
    served.extend_from_slice(&0i32.to_le_bytes());
    served.extend_from_slice(&base); // parent header, std-80
    let header = BlockHeader::from_served_bytes(&served, MIXED).unwrap();
    assert!(header.aux_pow.is_some());
    assert_eq!(&header.raw()[..], &served[..]);
    // In a list a flag-set header is exactly 80 bytes, so the same bytes are not a one-header list.
    assert!(BlockHeader::list_from_served_bytes(&served, 1, MIXED).is_err());
}

#[test]
fn mixed_list_with_a_header_that_does_not_match_its_own_version_is_rejected_whole() {
    let mixed = fixture(MEWC_MIXED);
    // A flag-clear header followed by only 80 bytes: the list stops early (count 2, 170 bytes).
    let mut short_native = mixed[..80].to_vec();
    short_native.extend_from_slice(&fixture(MEWC_LONE_AUXPOW));
    short_native.extend_from_slice(&[0u8; 10]);
    assert_eq!(short_native.len(), 170);
    assert!(BlockHeader::list_from_served_bytes(&short_native, 2, MIXED).is_err());
    // A flag-set header followed by a 120-byte body: bit 8 forced on a native header.
    let mut flagged = fixture(MEWC_LONE_NATIVE);
    flagged[1] |= 0x01;
    assert_eq!(flagged.len(), 120);
    assert!(BlockHeader::list_from_served_bytes(&flagged, 1, MIXED).is_err());
    // Trailing bytes after a complete list, and a list that runs out early, are both rejected.
    assert!(BlockHeader::list_from_served_bytes(&mixed, 10, MIXED).is_err());
    assert!(BlockHeader::list_from_served_bytes(&mixed, 12, MIXED).is_err());
}

#[test]
fn mixed_count_times_80_fallback_applies_only_to_a_list_of_exactly_that_length() {
    let mixed = fixture(MEWC_MIXED);
    // Two flag-clear-looking records of 80 bytes: exactly 2 x 80, so the std-80 re-read applies.
    let native = fixture(MEWC_LONE_NATIVE);
    let mut exact = native[..80].to_vec();
    exact.extend_from_slice(&native[..80]);
    let headers = BlockHeader::list_from_served_bytes(&exact, 2, MIXED).unwrap();
    assert_eq!(lens(&headers), [80, 80]);
    // Any other length does not get the fallback.
    let mut other = exact.clone();
    other.push(0);
    assert!(BlockHeader::list_from_served_bytes(&other, 2, MIXED).is_err());
    // A lone flag-clear header of exactly 80 bytes is std-80; one of 79 bytes is malformed.
    assert_eq!(
        BlockHeader::from_served_bytes(&native[..80], MIXED)
            .unwrap()
            .raw()
            .len(),
        80
    );
    assert!(BlockHeader::from_served_bytes(&mixed[..79], MIXED).is_err());
}

// ---- key absent: existing behavior ----------------------------------------------------------

#[test]
fn without_the_layout_the_same_lists_are_still_rejected() {
    // R37.5.6 as before: Standard reads 80 bytes per header and the list does not end where the
    // bytes end.
    assert!(BlockHeader::list_from_served_bytes(&fixture(RIC_LIST), 11, CoinVariant::Standard).is_err());
    assert!(BlockHeader::list_from_served_bytes(&fixture(MEWC_MIXED), 11, CoinVariant::Standard).is_err());
    assert!(BlockHeader::list_from_served_bytes(&fixture(MEWC_TAIL), 3, CoinVariant::Standard).is_err());
    assert!(BlockHeader::from_served_bytes(&fixture(RIC_LONE), CoinVariant::Standard).is_err());
    assert!(BlockHeader::from_served_bytes(&fixture(MEWC_LONE_NATIVE), CoinVariant::Standard).is_err());
    // The Qtum hint is likewise unchanged.
    assert!(BlockHeader::list_from_served_bytes(&fixture(RIC_LIST), 11, CoinVariant::Qtum).is_err());
}
