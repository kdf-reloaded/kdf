//! The per-network GasFree constants (CRD ch.63 §63.4.1, §63.5, §63.6).
//!
//! Everything here is fixed per network and not configurable by any caller (R2). The controller and beacon
//! addresses, the chain ids and the path segments are the published values of §63.5, §63.6 and §63.4.1.
//!
//! The proxy creation bytecode is taken from the public chain (the §63.5 sourcing rule): each file under
//! `bytecode/` holds the creation code of the GasFree proxy of one network as the network's controller deploys it,
//! taken from the transaction that deployed the GasFree contracts there:
//!
//! | Network | Deploying transaction | Block |
//! |---|---|---|
//! | Mainnet | `7f5e37bc9d78822d5b72bf90ea65444208c6a42b9b3b2f811666a992ba42f037` | 69944703 |
//! | Nile | `df2f6668a4b7dd086f34876f919db63afa1e1649c81a07594552c85540feae1d` | 52537691 |
//! | Shasta | `a6a20ec22185c82111f1ceceb70ccb144040f71a659c5e15643bcab7fe445aae` | 66317600 |
//!
//! Each file is 997 bytes; the unit tests pin the keccak-256 of each.

use super::super::{Network, TronAddress};

/// The 4-byte selector of `initialize(address)` (§63.5 step 2).
pub(crate) const INITIALIZE_SELECTOR: [u8; 4] = [0xc4, 0xd6, 0x6d, 0xe8];

/// The TIP-712 domain name of the GasFree controller (§63.6).
pub(crate) const DOMAIN_NAME: &str = "GasFreeController";

/// The TIP-712 domain version of the GasFree controller (§63.6).
pub(crate) const DOMAIN_VERSION: &str = "V1.0.0";

/// The fixed GasFree values of one TRON network.
pub(crate) struct GasFreeNetwork {
    /// The network's name in texts.
    pub name: &'static str,
    /// The first segment of every provider request path (§63.4.1).
    pub path_segment: &'static str,
    /// The EIP-712 chain id (§63.6).
    pub chain_id: u64,
    /// The 20-byte payload of the controller contract's address, the `verifyingContract` (§63.5).
    controller: [u8; 20],
    /// The 20-byte payload of the beacon contract's address (§63.5).
    beacon: [u8; 20],
    /// The proxy creation bytecode (§63.5).
    pub creation_bytecode: &'static [u8],
}

impl GasFreeNetwork {
    /// The controller contract, the `verifyingContract` of the typed-data domain.
    pub(crate) fn controller(&self) -> TronAddress { TronAddress::from_evm_address(self.controller.into()) }

    /// The beacon contract.
    pub(crate) fn beacon(&self) -> TronAddress { TronAddress::from_evm_address(self.beacon.into()) }
}

/// `TFFAMQLZybALaLb4uxHA9RBE7pxhUAjF3U` and `TSP9UW6FQhT76XD2jWA6ipGMx3yGbjDffP`.
static MAINNET: GasFreeNetwork = GasFreeNetwork {
    name: "mainnet",
    path_segment: "tron",
    chain_id: 728_126_428,
    controller: [
        0x39, 0xdd, 0x12, 0xa5, 0x4e, 0x2b, 0xab, 0x7c, 0x82, 0xaa, 0x14, 0xa1, 0xe1, 0x58, 0xb3, 0x42, 0x63, 0xd2,
        0xd5, 0x10,
    ],
    beacon: [
        0xb4, 0x09, 0x0d, 0x33, 0x7e, 0xf8, 0xfb, 0xf8, 0x80, 0xc1, 0xc2, 0x87, 0x41, 0x2c, 0xfb, 0x72, 0x32, 0x4d,
        0x71, 0x1e,
    ],
    creation_bytecode: include_bytes!("bytecode/proxy_creation_mainnet.bin"),
};

/// `THQGuFzL87ZqhxkgqYEryRAd7gqFqL5rdc` and `TLtCGmaxH3PbuaF6kbybwteZcHptEdgQGC`.
static NILE: GasFreeNetwork = GasFreeNetwork {
    name: "nile",
    path_segment: "nile",
    chain_id: 3_448_148_188,
    controller: [
        0x51, 0x86, 0x88, 0xfb, 0xb3, 0x9c, 0xcf, 0x12, 0x53, 0xf2, 0xb1, 0x21, 0x76, 0x79, 0xfb, 0xe3, 0x16, 0x32,
        0x92, 0x88,
    ],
    beacon: [
        0x77, 0xb6, 0xb1, 0xf6, 0x6f, 0x7a, 0x45, 0xf4, 0xf4, 0x98, 0x1a, 0x1f, 0x97, 0x40, 0x32, 0x71, 0x4e, 0x04,
        0x62, 0x33,
    ],
    creation_bytecode: include_bytes!("bytecode/proxy_creation_nile.bin"),
};

/// `TQghdCeVDA6CnuNVTUhfaAyPfTetqZWNpm` and `TQ1jvA3nLDMDNbJoMPLzTPoqAg8NvZ5CCW`.
static SHASTA: GasFreeNetwork = GasFreeNetwork {
    name: "shasta",
    path_segment: "shasta",
    chain_id: 2_494_104_990,
    controller: [
        0xa1, 0x6a, 0xb5, 0x74, 0x66, 0xb7, 0x94, 0xd8, 0x73, 0x15, 0xd6, 0x91, 0x85, 0x3b, 0xda, 0x59, 0xfd, 0x7b,
        0xf4, 0x96,
    ],
    beacon: [
        0x9a, 0x0c, 0x66, 0x5b, 0xdc, 0xac, 0xa8, 0x5a, 0x40, 0x77, 0x94, 0x93, 0x9e, 0x31, 0x04, 0xf1, 0x4f, 0xd6,
        0xf2, 0xb5,
    ],
    creation_bytecode: include_bytes!("bytecode/proxy_creation_shasta.bin"),
};

/// The fixed GasFree values of `network`.
pub(crate) fn gasfree_network(network: &Network) -> &'static GasFreeNetwork {
    match network {
        Network::Mainnet => &MAINNET,
        Network::Nile => &NILE,
        Network::Shasta => &SHASTA,
    }
}

#[cfg(test)]
mod tests {
    use super::super::keccak;
    use super::*;

    /// The hash the public GasFree SDK v1.1.2 bytes for Shasta have: 1208 bytes of the implementation that was
    /// live before the Shasta controller's upgrade of 2026-07-04.
    const SDK_SHASTA_BYTECODE_KECCAK: &str = "490791365cc571bb1659ec928507a7b5f69ab6c65a8cdf7ee9a4a8c61b2f3477";

    #[test]
    fn the_published_constants_of_each_network_are_bound() {
        let cases = [
            (
                Network::Mainnet,
                "tron",
                728_126_428u64,
                "TFFAMQLZybALaLb4uxHA9RBE7pxhUAjF3U",
                "TSP9UW6FQhT76XD2jWA6ipGMx3yGbjDffP",
            ),
            (
                Network::Nile,
                "nile",
                3_448_148_188,
                "THQGuFzL87ZqhxkgqYEryRAd7gqFqL5rdc",
                "TLtCGmaxH3PbuaF6kbybwteZcHptEdgQGC",
            ),
            (
                Network::Shasta,
                "shasta",
                2_494_104_990,
                "TQghdCeVDA6CnuNVTUhfaAyPfTetqZWNpm",
                "TQ1jvA3nLDMDNbJoMPLzTPoqAg8NvZ5CCW",
            ),
        ];
        for (network, segment, chain_id, controller, beacon) in cases {
            let bound = gasfree_network(&network);
            assert_eq!(bound.path_segment, segment);
            assert_eq!(bound.chain_id, chain_id);
            assert_eq!(bound.controller().to_base58(), controller);
            assert_eq!(bound.beacon().to_base58(), beacon);
        }
    }

    #[test]
    fn the_initialize_selector_is_the_head_of_the_signature_hash() {
        assert_eq!(keccak(b"initialize(address)")[..4], INITIALIZE_SELECTOR);
    }

    /// ch.63 T1: the embedded bytes are the ones the deploying transactions carry.
    #[test]
    fn the_embedded_creation_bytecode_has_the_pinned_hash() {
        let cases = [
            (
                Network::Mainnet,
                "28122b164158c3961d140d546ce24bb496fa91bff2b3cbc3f073d4eb5d998b22",
            ),
            (
                Network::Nile,
                "8a1613b444604b6dc63c348f90741c31aef84360be2e9aaa142422c90059fcd4",
            ),
            (
                Network::Shasta,
                "52a8ac900543cec486e4d3c7bfd67d80b2a3e4b42e1b6a694382ee8498cdafc9",
            ),
        ];
        for (network, expected) in cases {
            let bytecode = gasfree_network(&network).creation_bytecode;
            assert_eq!(bytecode.len(), 997);
            assert_eq!(hex::encode(keccak(bytecode)), expected);
        }
    }

    #[test]
    fn the_shasta_bytecode_is_not_the_one_the_sdk_publishes() {
        let bytecode = gasfree_network(&Network::Shasta).creation_bytecode;
        assert_ne!(bytecode.len(), 1208);
        assert_ne!(hex::encode(keccak(bytecode)), SDK_SHASTA_BYTECODE_KECCAK);
    }

    #[test]
    fn the_mainnet_and_nile_bytecode_differ() {
        assert_ne!(
            gasfree_network(&Network::Mainnet).creation_bytecode,
            gasfree_network(&Network::Nile).creation_bytecode
        );
    }
}
