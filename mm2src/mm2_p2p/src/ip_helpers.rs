use std::net::Ipv4Addr;

/// Stable replacement for the unstable `Ipv4Addr::is_global()`.
///
/// Returns `true` if the address is globally routable per IANA registries.
/// Based on RFC 6890 / IANA IPv4 Special-Purpose Address Registry.
///
/// CRD ch.28 R24: all of `0.0.0.0/8` is not global, and within `192.0.0.0/24` only `192.0.0.9` and `192.0.0.10` are.
pub(crate) fn ipv4_is_global(ip: &Ipv4Addr) -> bool {
    // "This network" (0.0.0.0/8, which includes the unspecified address) - RFC 1122
    !(ip.octets()[0] == 0
        || ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        // Shared address space (100.64.0.0/10) - RFC 6598
        || (ip.octets()[0] == 100 && (ip.octets()[1] & 0xC0) == 64)
        // Protocol assignments (192.0.0.0/24) - RFC 6890, except 192.0.0.9 and 192.0.0.10, which the IANA
        // special-purpose registry marks as globally reachable (RFC 7723, RFC 8155)
        || (ip.octets()[0] == 192
            && ip.octets()[1] == 0
            && ip.octets()[2] == 0
            && !matches!(ip.octets()[3], 9 | 10))
        // Benchmarking (198.18.0.0/15) - RFC 2544
        || (ip.octets()[0] == 198 && (ip.octets()[1] & 0xFE) == 18)
        // Reserved for future use (240.0.0.0/4) except broadcast
        || ip.octets()[0] >= 240)
}

#[cfg(test)]
mod tests {
    use super::ipv4_is_global;
    use std::net::Ipv4Addr;

    fn global(ip: &str) -> bool { ipv4_is_global(&ip.parse::<Ipv4Addr>().unwrap()) }

    /// CRD ch.28 R24: all of `0.0.0.0/8` is not global.
    #[test]
    fn test_this_network_block_is_not_global() {
        for ip in ["0.0.0.0", "0.0.0.1", "0.1.2.3", "0.255.255.255"] {
            assert!(!global(ip), "{}", ip);
        }
        assert!(global("1.0.0.1"));
    }

    /// CRD ch.28 R24: `192.0.0.9` and `192.0.0.10` are global, the rest of `192.0.0.0/24` is not, and the
    /// neighbouring blocks are unchanged.
    #[test]
    fn test_protocol_assignment_block_exceptions() {
        assert!(global("192.0.0.9"));
        assert!(global("192.0.0.10"));
        for ip in ["192.0.0.0", "192.0.0.1", "192.0.0.8", "192.0.0.11", "192.0.0.255"] {
            assert!(!global(ip), "{}", ip);
        }
        assert!(global("192.0.1.1"));
        assert!(!global("192.0.2.1"));
    }
}
