//! Network configuration for netid 6133 — GLEEC DEX network.
//!
//! # Provenance
//!
//! The numeric parameters below (fee rates, public keys, z-addresses) are
//! network parameters used for inter-operation on netid 6133.
//! They are treated as externally observable compatibility values;
//! project-wide provenance details are tracked in `docs/reloaded-rewrite/34-provenance-ledger.md`.
//!
//! # Parameters
//!
//! - Base rate:  2/100  (2%)
//! - GLEEC rate: 1/100  (1%, 50% discount)
//! - Burn: disabled

use lazy_static::lazy_static;
use num_rational::BigRational;

use crate::NetConfig;

/// DEX fee recipient public key (compressed, hex) — GLEEC fee address.
const DEX_FEE_ADDR_PUBKEY: &str = "03a778d9bd346fa704cf3e2508cd074d93a1bbc1e504fbecbb0a8d48e7cccbbf5c";

/// Inactive compatibility key for the historical pre-burn account.
///
/// Netid 6133 currently disables burn structurally. The key remains equal to
/// the fee key so persisted/configuration-facing compatibility values do not
/// drift if the dormant account-burn substrate is inspected.
const BURN_ADDR_PUBKEY: &str = "03a778d9bd346fa704cf3e2508cd074d93a1bbc1e504fbecbb0a8d48e7cccbbf5c";

/// Z-address for shielded DEX fee (Zcash-based coins).
/// On GLEEC, the burn z-address is the same as the fee z-address (burn disabled for z-txs).
const DEX_FEE_Z_ADDR: &str = "zs1lgdrlg6kv6lmf0n9ps2uhj6sc8rdn30vx44qzu7hqa5ms4a4fwytlr8yuwrqyvhk6l6r5fevw50";

/// Hex-encoded ed25519 public key for Siacoin-style DEX fee collection.
///
/// The GLEEC 2022 fork did not override the upstream Siacoin fee key, so
/// netid 6133 inherits the same observed network constant. Operators who
/// want a distinct destination on 6133 can change this value here.
const DEX_FEE_PUBKEY_ED25519: &str = "77b0936728f63257b074c7b3fb2c4fad98df345f57de1ec418fc42619e4e29f8";

/// Seed nodes for P2P bootstrapping on netid 6133.
/// No hardcoded seeds — operators must provide `"seednodes"` in MM2.json.
const SEED_NODES: &[&str] = &[];

lazy_static! {
    static ref DEX_FEE_ADDR_RAW: Vec<u8> =
        hex::decode(DEX_FEE_ADDR_PUBKEY).expect("netid_6133: invalid DEX_FEE_ADDR_PUBKEY hex");
    static ref BURN_ADDR_RAW: Vec<u8> =
        hex::decode(BURN_ADDR_PUBKEY).expect("netid_6133: invalid BURN_ADDR_PUBKEY hex");
}

pub struct Netid6133;

impl NetConfig for Netid6133 {
    fn netid(&self) -> u16 { 6133 }

    fn network_name(&self) -> &'static str { "GLEEC" }

    fn dex_fee_addr_pubkey(&self) -> &'static str { DEX_FEE_ADDR_PUBKEY }

    fn dex_fee_addr_raw_pubkey(&self) -> &'static [u8] { &DEX_FEE_ADDR_RAW }

    fn dex_fee_z_addr(&self) -> &'static str { DEX_FEE_Z_ADDR }

    fn dex_fee_pubkey_ed25519(&self) -> &'static str { DEX_FEE_PUBKEY_ED25519 }

    fn dex_fee_rate(&self) -> BigRational {
        // 2/100 = 2%
        BigRational::new(2.into(), 100.into())
    }

    fn fee_discount_tickers(&self) -> &'static [&'static str] { &["GLEEC"] }

    fn dex_fee_rate_discounted(&self) -> BigRational {
        // 1/100 = 1% (50% discount for GLEEC trades)
        BigRational::new(1.into(), 100.into())
    }

    fn burn_addr_pubkey(&self) -> &'static str { BURN_ADDR_PUBKEY }

    fn burn_addr_raw_pubkey(&self) -> &'static [u8] { &BURN_ADDR_RAW }

    fn seed_nodes(&self) -> &'static [&'static str] { SEED_NODES }
}
