//! # mm2_net_config - Compile-time Network Configuration
//!
//! Each netid gets a Rust module that implements [`NetConfig`], encoding all
//! P2P network parameters at compile time. The binary rejects unknown netids
//! at startup ("deny except config exists").
//!
//! ## Adding a new network
//!
//! 1. Create `mm2src/mm2_net_config/src/netid_XXXX.rs`
//! 2. Implement a unit struct + `NetConfig` for it
//! 3. Register it in [`net_config_for`] below

mod netid_6133;
mod netid_8762;
#[cfg(feature = "regtest-netid")] mod test_netids;

use num_rational::BigRational;

/// Compile-time network parameters for a single netid.
///
/// Every constant here is baked into the binary — there is no runtime
/// configuration file for these values.
pub trait NetConfig: Send + Sync + 'static {
    /// Numeric network identifier (matches the JSON config `"netid"` field).
    fn netid(&self) -> u16;

    /// Human-readable network name (for logging and error messages).
    fn network_name(&self) -> &'static str;

    // ── DEX Fee Address ──────────────────────────────────────────────

    /// Hex-encoded compressed public key that receives the DEX fee.
    fn dex_fee_addr_pubkey(&self) -> &'static str;

    /// Raw bytes of the DEX fee public key (decoded from hex at startup).
    fn dex_fee_addr_raw_pubkey(&self) -> &'static [u8];

    /// Z-address for shielded DEX fee collection (Zcash-based coins).
    fn dex_fee_z_addr(&self) -> &'static str;

    /// Hex-encoded ed25519 public key that receives the DEX fee on Siacoin
    /// and other Sia-style ed25519 chains.
    fn dex_fee_pubkey_ed25519(&self) -> &'static str;

    // ── Fee Rates ────────────────────────────────────────────────────

    /// Base DEX fee rate as a precise rational number (e.g. 1/777).
    fn dex_fee_rate(&self) -> BigRational;

    /// Tickers that receive a fee discount.
    fn fee_discount_tickers(&self) -> &'static [&'static str];

    /// Discounted DEX fee rate for the tickers above.
    fn dex_fee_rate_discounted(&self) -> BigRational;

    /// Optional network-level DEX fee floor.
    ///
    /// The production reference networks do not define an additional floor,
    /// so the default is zero and the taker coin's `min_tx_amount` remains the
    /// effective minimum.
    fn dex_fee_min_threshold(&self) -> BigRational { BigRational::from_integer(0.into()) }

    // ── Burn ─────────────────────────────────────────────────────────

    /// Whether this network permits a coin-specific DEX-fee burn path.
    fn burn_enabled(&self) -> bool { false }

    /// Share of the DEX fee that goes to the fee address (1.0 = no burn).
    /// Only meaningful when `burn_enabled()` returns true.
    fn dex_fee_share(&self) -> BigRational { BigRational::from_integer(1.into()) }

    /// Hex-encoded compressed public key for the burn address.
    /// Only meaningful when `burn_enabled()` returns true.
    fn burn_addr_pubkey(&self) -> &'static str { "" }

    /// Raw bytes of the burn address public key (decoded from hex at startup).
    /// Only meaningful when `burn_enabled()` returns true.
    fn burn_addr_raw_pubkey(&self) -> &'static [u8] { &[] }

    // ── Seed Nodes ───────────────────────────────────────────────────

    /// DNS hostnames of seed nodes for P2P bootstrapping.
    fn seed_nodes(&self) -> &'static [&'static str];
}

// ── Registry ─────────────────────────────────────────────────────────

/// Returns the network configuration for the given netid, or `None` if
/// the netid has no compiled configuration (binary refuses to start).
pub fn net_config_for(netid: u16) -> Option<&'static dyn NetConfig> {
    match netid {
        8762 => Some(&netid_8762::Netid8762),
        6133 => Some(&netid_6133::Netid6133),
        #[cfg(feature = "regtest-netid")]
        8100 => Some(&test_netids::Netid8100),
        #[cfg(feature = "regtest-netid")]
        8999 => Some(&test_netids::Netid8999),
        #[cfg(feature = "regtest-netid")]
        9000 => Some(&test_netids::Netid9000),
        #[cfg(feature = "regtest-netid")]
        9998 => Some(&test_netids::Netid9998),
        _ => None,
    }
}

/// Convenience: returns config or panics with a descriptive message.
/// Use only at startup when early-exit is appropriate.
pub fn net_config_or_panic(netid: u16) -> &'static dyn NetConfig {
    net_config_for(netid).unwrap_or_else(|| {
        panic!(
            "No compiled network configuration for netid {}. \
             Supported netids: {}",
            netid,
            supported_netids_display()
        )
    })
}

/// Comma-separated list of all supported netids (for error messages).
fn supported_netids_display() -> String {
    SUPPORTED_NETIDS
        .iter()
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// All compiled netids, for validation and display.
#[cfg(not(feature = "regtest-netid"))]
pub const SUPPORTED_NETIDS: &[u16] = &[8762, 6133];

/// All compiled netids, for validation and display.
/// With the `regtest-netid` feature, the test-only netids used by
/// `docker_tests` and `mm2_tests` fixtures are included.
#[cfg(feature = "regtest-netid")]
pub const SUPPORTED_NETIDS: &[u16] = &[8762, 6133, 8100, 8999, 9000, 9998];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_known_netids_resolve() {
        for &netid in SUPPORTED_NETIDS {
            let cfg = net_config_for(netid).expect("supported netid should resolve");
            assert_eq!(cfg.netid(), netid);
            assert!(!cfg.network_name().is_empty());
            assert!(!cfg.dex_fee_addr_pubkey().is_empty());
            assert!(!cfg.dex_fee_addr_raw_pubkey().is_empty());
            assert!(!cfg.dex_fee_z_addr().is_empty());
            assert!(!cfg.dex_fee_pubkey_ed25519().is_empty());
            // seed_nodes() may be empty — operators provide seeds via MM2.json config
        }
    }

    #[test]
    fn test_unknown_netid_returns_none() {
        assert!(net_config_for(0).is_none());
        assert!(net_config_for(9999).is_none());
        assert!(net_config_for(7777).is_none());
    }

    #[test]
    fn test_netid_8762_has_kmd_burn_policy() {
        let cfg = net_config_for(8762).unwrap();
        assert!(cfg.burn_enabled());
        assert_eq!(cfg.dex_fee_share(), BigRational::new(3.into(), 4.into()));
        // Netid 8762 burns KMD directly via OP_RETURN and has no account-burn key.
        assert!(cfg.burn_addr_pubkey().is_empty());
        assert!(cfg.burn_addr_raw_pubkey().is_empty());
    }

    #[test]
    fn test_netid_6133_disables_burn() {
        let cfg = net_config_for(6133).unwrap();
        assert!(!cfg.burn_enabled());
        assert_eq!(cfg.dex_fee_share(), BigRational::from_integer(1.into()));
        // The inactive compatibility key remains well-formed.
        assert!(!cfg.burn_addr_pubkey().is_empty());
        assert!(!cfg.burn_addr_raw_pubkey().is_empty());
        let decoded = hex::decode(cfg.burn_addr_pubkey()).expect("burn pubkey hex should be valid");
        assert_eq!(cfg.burn_addr_raw_pubkey(), decoded.as_slice());
    }

    #[test]
    fn test_fee_rates_are_positive() {
        for &netid in SUPPORTED_NETIDS {
            let cfg = net_config_for(netid).unwrap();
            assert!(cfg.dex_fee_rate() > BigRational::from_integer(0.into()));
            assert!(cfg.dex_fee_rate_discounted() > BigRational::from_integer(0.into()));
            assert_eq!(cfg.dex_fee_min_threshold(), BigRational::from_integer(0.into()));
            // Discounted rate should be <= base rate
            assert!(cfg.dex_fee_rate_discounted() <= cfg.dex_fee_rate());
        }
    }

    #[test]
    fn test_raw_pubkey_matches_hex() {
        for &netid in SUPPORTED_NETIDS {
            let cfg = net_config_for(netid).unwrap();
            let decoded = hex::decode(cfg.dex_fee_addr_pubkey()).expect("pubkey hex should be valid");
            assert_eq!(cfg.dex_fee_addr_raw_pubkey(), decoded.as_slice());
        }
    }
}
