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

lazy_static! {
    static ref DEX_FEE_ADDR_RAW: Vec<u8> =
        hex::decode(DEX_FEE_ADDR_PUBKEY).expect("netid_8762: invalid DEX_FEE_ADDR_PUBKEY hex");
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

    fn seed_nodes(&self) -> &'static [&'static str] { SEED_NODES }
}
