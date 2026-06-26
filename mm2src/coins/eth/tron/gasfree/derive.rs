//! GasFree custody-address derivation — Tron `CREATE2` (§49.5, dictated interop).
//!
//! The custody address is derived **locally and deterministically** so the
//! wallet can independently verify the provider's reported address (R2). The
//! algorithm is byte-exact per the Tron `CREATE2` rule (the `0x41` prefix in
//! place of Ethereum's `0xff`) and the published GasFree per-network contract
//! artifacts (§49.5 / §49.13).
//!
//! NOTE: the per-network artifacts (`controller`, `beacon`, `creation_bytecode`)
//! are placeholders today (see `config::network_artifacts`); the derivation
//! *algorithm* is what these tests pin. Plug in the published GasFree SDK
//! constants to reproduce the published address vectors (T1).

use super::config::GasFreeArtifacts;
use crate::eth::tron::address::TronAddress;
use ethereum_types::Address as EthAddress;
use sha3::{Digest, Keccak256};

/// Ethereum/EVM keccak-256 (legacy padding), as used by `CREATE2`.
fn keccak256(data: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&Keccak256::digest(data));
    out
}

/// The 4-byte selector of `initialize(address)` (§49.5 step 2).
///
/// This is the well-known public selector `0xc4d66de8`.
pub fn initialize_selector() -> [u8; 4] {
    let h = keccak256(b"initialize(address)");
    [h[0], h[1], h[2], h[3]]
}

/// ABI-encode the tuple `(address beacon, bytes data)` (§49.5 step 3).
///
/// Layout: `beacon(32) ‖ offset=0x40(32) ‖ len(32) ‖ data(right-padded to 32)`.
fn abi_encode_address_bytes(beacon: &EthAddress, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(96 + data.len().div_ceil(32) * 32);

    // word 0: address, left-padded to 32 bytes.
    let mut word = [0u8; 32];
    word[12..32].copy_from_slice(beacon.as_ref());
    out.extend_from_slice(&word);

    // word 1: offset to the bytes payload = 0x40 (two preceding words).
    let mut offset = [0u8; 32];
    offset[31] = 0x40;
    out.extend_from_slice(&offset);

    // word 2: length of the bytes payload.
    let mut len = [0u8; 32];
    len[24..32].copy_from_slice(&(data.len() as u64).to_be_bytes());
    out.extend_from_slice(&len);

    // payload, right-padded to a 32-byte boundary.
    out.extend_from_slice(data);
    let rem = data.len() % 32;
    if rem != 0 {
        out.extend(std::iter::repeat_n(0u8, 32 - rem));
    }
    out
}

/// Derive the GasFree custody address for `user` under `artifacts` (§49.5).
pub fn derive_gasfree_address(user: &EthAddress, artifacts: &GasFreeArtifacts) -> TronAddress {
    // 1. Salt: the user's 20-byte EVM address, right-aligned into 32 bytes.
    let mut salt = [0u8; 32];
    salt[12..32].copy_from_slice(user.as_ref());

    // 2. init calldata: selector(4) ‖ salt(32).
    let mut init_calldata = Vec::with_capacity(36);
    init_calldata.extend_from_slice(&initialize_selector());
    init_calldata.extend_from_slice(&salt);

    // 3. init code: creation bytecode ‖ abi.encode(beacon, init calldata).
    let mut init_code = Vec::with_capacity(artifacts.creation_bytecode.len() + 128);
    init_code.extend_from_slice(artifacts.creation_bytecode);
    init_code.extend_from_slice(&abi_encode_address_bytes(&artifacts.beacon, &init_calldata));

    // 4. init-code hash.
    let init_code_hash = keccak256(&init_code);

    // 5. preimage: 0x41 ‖ controller(20) ‖ salt(32) ‖ init-code hash(32).
    let mut preimage = Vec::with_capacity(1 + 20 + 32 + 32);
    preimage.push(0x41);
    preimage.extend_from_slice(artifacts.controller.as_ref());
    preimage.extend_from_slice(&salt);
    preimage.extend_from_slice(&init_code_hash);

    // 6. address: last 20 bytes of keccak256(preimage), as a Tron address.
    let digest = keccak256(&preimage);
    let mut addr = [0u8; 20];
    addr.copy_from_slice(&digest[12..32]);
    TronAddress::from_evm_address(EthAddress::from(addr))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eth_from_u64(n: u64) -> EthAddress {
        let mut b = [0u8; 20];
        b[12..].copy_from_slice(&n.to_be_bytes());
        EthAddress::from(b)
    }

    // Synthetic (non-published) artifacts that exercise the algorithm. Replace
    // with the published GasFree SDK per-network constants for true vectors.
    fn synthetic_artifacts() -> GasFreeArtifacts {
        GasFreeArtifacts {
            controller: eth_from_u64(0x1111),
            beacon: eth_from_u64(0x2222),
            creation_bytecode: &[0x60, 0x80, 0x60, 0x40, 0x52],
        }
    }

    fn user() -> EthAddress { eth_from_u64(0xdead_beef) }

    #[test]
    fn initialize_selector_is_known_public_value() {
        // `initialize(address)` → 0xc4d66de8 (OpenZeppelin proxy selector).
        assert_eq!(initialize_selector(), [0xc4, 0xd6, 0x6d, 0xe8]);
    }

    #[test]
    fn abi_encode_address_bytes_layout() {
        let beacon = eth_from_u64(0x2222);
        // 36-byte calldata: selector(4) + salt(32).
        let data = vec![0xAB; 36];
        let enc = abi_encode_address_bytes(&beacon, &data);
        // 3 head words + ceil(36/32)=2 payload words = 5 * 32 = 160 bytes.
        assert_eq!(enc.len(), 160);
        // word0 = beacon address, right-aligned.
        assert_eq!(&enc[24..32], &beacon.0[12..20]);
        // word1 = 0x40 offset.
        assert_eq!(enc[63], 0x40);
        // word2 = length 36.
        assert_eq!(enc[95], 36);
        // payload starts at 96, padded with zeros after 36 bytes.
        assert_eq!(&enc[96..132], &data[..]);
        assert!(enc[132..160].iter().all(|b| *b == 0));
    }

    #[test]
    fn derivation_is_deterministic() {
        let a = synthetic_artifacts();
        let d1 = derive_gasfree_address(&user(), &a);
        let d2 = derive_gasfree_address(&user(), &a);
        assert_eq!(d1, d2);
        // Output must be a well-formed Tron address (0x41 prefix, valid base58).
        assert!(d1.to_base58().starts_with('T'));
    }

    #[test]
    fn derivation_changes_with_user() {
        let a = synthetic_artifacts();
        let d1 = derive_gasfree_address(&user(), &a);
        let d2 = derive_gasfree_address(&eth_from_u64(0xfeed), &a);
        assert_ne!(d1, d2);
    }

    #[test]
    fn derivation_changes_with_controller() {
        let a1 = synthetic_artifacts();
        let mut a2 = synthetic_artifacts();
        a2.controller = eth_from_u64(0x9999);
        assert_ne!(
            derive_gasfree_address(&user(), &a1),
            derive_gasfree_address(&user(), &a2)
        );
    }

    #[test]
    fn derivation_changes_with_beacon() {
        let a1 = synthetic_artifacts();
        let mut a2 = synthetic_artifacts();
        a2.beacon = eth_from_u64(0x8888);
        assert_ne!(
            derive_gasfree_address(&user(), &a1),
            derive_gasfree_address(&user(), &a2)
        );
    }
}
