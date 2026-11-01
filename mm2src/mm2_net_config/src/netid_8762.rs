//! Network configuration for netid 8762 — original AtomicDEX network.
//!
//! Fee parameters match the original KomoDeFi codebase:
//! - Base rate:  1/777  (~0.129%)
//! - KMD rate:   9/7770 (~0.116%, 10% discount)
//! - KMD burn:   25% of the DEX fee via OP_RETURN

use lazy_static::lazy_static;
use num_rational::BigRational;

use crate::NetConfig;

/// DEX fee recipient public key (compressed, hex).
const DEX_FEE_ADDR_PUBKEY: &str = "03bc2c7ba671bae4a6fc835244c9762b41647b9827d4780a89a949b984a8ddcc06";

/// Z-address for shielded DEX fee (Zcash-based coins).
const DEX_FEE_Z_ADDR: &str = "zs1rp6426e9r6jkq2nsanl66tkd34enewrmr0uvj0zelhkcwmsy0uvxz2fhm9eu9rl3ukxvgzy2v9f";

/// Hex-encoded ed25519 public key for Siacoin-style DEX fee collection.
const DEX_FEE_PUBKEY_ED25519: &str = "77b0936728f63257b074c7b3fb2c4fad98df345f57de1ec418fc42619e4e29f8";

/// Seed nodes for P2P bootstrapping on netid 8762.
/// No hardcoded seeds — operators must provide `"seednodes"` in MM2.json.
const SEED_NODES: &[&str] = &[];

/// No-fee waiver / burn-account public key (compressed, hex). A taker whose
/// taker-coin swap public key equals this value pays no DEX fee (CRD ch.08
/// R15C / ch.16 R7), on both swap protocols, independently of
/// `burn_enabled()`. This is the `v2.6.0-beta` netid-8762 burn key.
const BURN_ADDR_PUBKEY: &str = "0369aa10c061cd9e085f4adb7399375ba001b54136145cb748eb4c48657be13153";

/// Version-two no-fee ticker set (CRD ch.16 R12B): KMD pairs are exempt from
/// the version-two dex fee on netid 8762, following the `v2.6.0-beta`
/// version-two swap machines.
const NO_FEE_TICKERS_V2: &[&str] = &["KMD"];

lazy_static! {
    static ref DEX_FEE_ADDR_RAW: Vec<u8> =
        hex::decode(DEX_FEE_ADDR_PUBKEY).expect("netid_8762: invalid DEX_FEE_ADDR_PUBKEY hex");
    static ref BURN_ADDR_RAW: Vec<u8> =
        hex::decode(BURN_ADDR_PUBKEY).expect("netid_8762: invalid BURN_ADDR_PUBKEY hex");
}

pub struct Netid8762;

impl NetConfig for Netid8762 {
    fn netid(&self) -> u16 { 8762 }

    fn network_name(&self) -> &'static str { "AtomicDEX" }

    fn dex_fee_addr_pubkey(&self) -> &'static str { DEX_FEE_ADDR_PUBKEY }

    fn dex_fee_addr_raw_pubkey(&self) -> &'static [u8] { &DEX_FEE_ADDR_RAW }

    fn dex_fee_z_addr(&self) -> &'static str { DEX_FEE_Z_ADDR }

    fn dex_fee_pubkey_ed25519(&self) -> &'static str { DEX_FEE_PUBKEY_ED25519 }

    fn dex_fee_rate(&self) -> BigRational {
        // 1/777 ≈ 0.129%
        BigRational::new(1.into(), 777.into())
    }

    fn fee_discount_tickers(&self) -> &'static [&'static str] { &["KMD"] }

    fn dex_fee_rate_discounted(&self) -> BigRational {
        // 9/7770 ≈ 0.116% (1/777 minus 10%)
        BigRational::new(9.into(), 7770.into())
    }

    fn burn_enabled(&self) -> bool { true }

    fn dex_fee_share(&self) -> BigRational { BigRational::new(3.into(), 4.into()) }

    fn burn_addr_pubkey(&self) -> &'static str { BURN_ADDR_PUBKEY }

    fn burn_addr_raw_pubkey(&self) -> &'static [u8] { &BURN_ADDR_RAW }

    fn no_fee_tickers_v2(&self) -> &'static [&'static str] { NO_FEE_TICKERS_V2 }

    fn seed_nodes(&self) -> &'static [&'static str] { SEED_NODES }
}
