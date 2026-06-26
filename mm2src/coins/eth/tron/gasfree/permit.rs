//! `PermitTransfer` signed authorization — TIP-712 (§49.6, dictated interop).
//!
//! The user authorizes a transfer by signing GasFree's `PermitTransfer`
//! structured data (Tron's EIP-712-shaped TIP-712). The domain and message
//! layout are dictated by the GasFree protocol; the signature is only valid if
//! the typed data is byte-exact (R3). The hashing reuses the shared EIP-712
//! encoder ([`mm2_eth::typed_data`]).
//!
//! Signing obligations enforced here: the signer key MUST correspond to the
//! `user` address (R4); `version` MUST equal 1 (R5); the `deadline` MUST be in
//! the future at signing time (R7); the result is a 65-byte secp256k1 signature
//! with `v` normalized to `27`/`28` (R6); the raw signature is redacted in
//! `Debug` (R9).

use super::error::GasFreeWithdrawError;
use ethereum_types::{Address as EthAddress, H256, U256};
use indexmap::IndexMap;
use mm2_eth::keys::{sign, KeyPair, Secret};
use mm2_eth::typed_data::{hash_typed_data, Eip712, TypedField};
use serde::Serialize;
use std::fmt;

/// GasFree TIP-712 domain name (§49.6).
pub const DOMAIN_NAME: &str = "GasFreeController";
/// GasFree TIP-712 domain version (§49.6).
pub const DOMAIN_VERSION: &str = "V1.0.0";
/// The pinned authorization version (R5).
pub const PERMIT_VERSION: u64 = 1;

/// The TIP-712 domain inputs (§49.6): the network chain id and the per-network
/// controller as the `verifyingContract`.
#[derive(Clone, Copy, Debug)]
pub struct PermitDomain {
    pub chain_id: u64,
    pub verifying_contract: EthAddress,
}

/// The `PermitTransfer` message (§49.6). Addresses are EVM-form (the Tron
/// address's underlying 20-byte address).
#[derive(Clone, Debug)]
pub struct PermitTransfer {
    pub token: EthAddress,
    pub service_provider: EthAddress,
    pub user: EthAddress,
    pub receiver: EthAddress,
    pub value: U256,
    pub max_fee: U256,
    pub deadline: u64,
    pub version: u64,
    pub nonce: u64,
}

impl PermitTransfer {
    /// Construct a `PermitTransfer` with `version` pinned to [`PERMIT_VERSION`].
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        token: EthAddress,
        service_provider: EthAddress,
        user: EthAddress,
        receiver: EthAddress,
        value: U256,
        max_fee: U256,
        deadline: u64,
        nonce: u64,
    ) -> Self {
        PermitTransfer {
            token,
            service_provider,
            user,
            receiver,
            value,
            max_fee,
            deadline,
            version: PERMIT_VERSION,
            nonce,
        }
    }
}

/// A 65-byte secp256k1 signature over the `PermitTransfer` typed data, with `v`
/// normalized to `27`/`28` (R6). The raw bytes are redacted in `Debug` (R9).
#[derive(Clone, PartialEq)]
pub struct PermitSignature([u8; 65]);

impl PermitSignature {
    /// The raw 65-byte signature (`r ‖ s ‖ v`, `v ∈ {27, 28}`).
    pub fn as_bytes(&self) -> &[u8; 65] { &self.0 }

    /// Serialize as 130 hex characters without a `0x` prefix (§49.4.6 / R6).
    pub fn to_hex(&self) -> String { hex::encode(self.0) }
}

impl fmt::Debug for PermitSignature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("PermitSignature(<redacted>)") }
}

fn addr_hex(addr: &EthAddress) -> String { format!("0x{:x}", addr) }

/// The EIP-712 type registry for the GasFree domain and `PermitTransfer`.
fn permit_types() -> IndexMap<String, Vec<TypedField>> {
    let field = |name: &str, ty: &str| TypedField {
        name: name.to_string(),
        field_type: ty.to_string(),
    };
    let mut types = IndexMap::new();
    types.insert("EIP712Domain".to_string(), vec![
        field("name", "string"),
        field("version", "string"),
        field("chainId", "uint256"),
        field("verifyingContract", "address"),
    ]);
    types.insert("PermitTransfer".to_string(), vec![
        field("token", "address"),
        field("serviceProvider", "address"),
        field("user", "address"),
        field("receiver", "address"),
        field("value", "uint256"),
        field("maxFee", "uint256"),
        field("deadline", "uint256"),
        field("version", "uint256"),
        field("nonce", "uint256"),
    ]);
    types
}

#[derive(Serialize)]
struct DomainValues {
    name: String,
    version: String,
    #[serde(rename = "chainId")]
    chain_id: String,
    #[serde(rename = "verifyingContract")]
    verifying_contract: String,
}

#[derive(Serialize)]
struct MessageValues {
    token: String,
    #[serde(rename = "serviceProvider")]
    service_provider: String,
    user: String,
    receiver: String,
    value: String,
    #[serde(rename = "maxFee")]
    max_fee: String,
    deadline: String,
    version: String,
    nonce: String,
}

fn build_eip712(domain: &PermitDomain, permit: &PermitTransfer) -> Eip712<DomainValues, MessageValues> {
    Eip712 {
        types: permit_types(),
        domain: DomainValues {
            name: DOMAIN_NAME.to_string(),
            version: DOMAIN_VERSION.to_string(),
            chain_id: domain.chain_id.to_string(),
            verifying_contract: addr_hex(&domain.verifying_contract),
        },
        primary_type: "PermitTransfer".to_string(),
        message: MessageValues {
            token: addr_hex(&permit.token),
            service_provider: addr_hex(&permit.service_provider),
            user: addr_hex(&permit.user),
            receiver: addr_hex(&permit.receiver),
            value: permit.value.to_string(),
            max_fee: permit.max_fee.to_string(),
            deadline: permit.deadline.to_string(),
            version: permit.version.to_string(),
            nonce: permit.nonce.to_string(),
        },
    }
}

/// The 32-byte EIP-712 digest of the `PermitTransfer` typed data (R3).
pub fn permit_typed_data_hash(
    domain: &PermitDomain,
    permit: &PermitTransfer,
) -> Result<[u8; 32], GasFreeWithdrawError> {
    hash_typed_data(build_eip712(domain, permit)).map_err(|e| GasFreeWithdrawError::Signing(e.to_string()))
}

/// Sign the `PermitTransfer` typed data (§49.6).
///
/// Refuses if `version != 1` (R5), if the deadline is not in the future at
/// `now_secs` (R7), or if the signing key does not correspond to `permit.user`
/// (R4). Returns a 65-byte signature with `v` normalized to `27`/`28` (R6).
pub fn sign_permit(
    secret: &Secret,
    domain: &PermitDomain,
    permit: &PermitTransfer,
    now_secs: u64,
) -> Result<PermitSignature, GasFreeWithdrawError> {
    if permit.version != PERMIT_VERSION {
        return Err(GasFreeWithdrawError::Signing(format!(
            "authorization version must be {PERMIT_VERSION}, got {}",
            permit.version
        )));
    }
    if permit.deadline <= now_secs {
        return Err(GasFreeWithdrawError::Signing(
            "authorization deadline must be in the future".to_owned(),
        ));
    }

    // R4: the signing key must control `permit.user`.
    let key_pair = KeyPair::from_secret(secret.clone())
        .map_err(|e| GasFreeWithdrawError::Signing(format!("invalid signing key: {e}")))?;
    if key_pair.address() != permit.user {
        return Err(GasFreeWithdrawError::Signing(
            "signing key does not correspond to the PermitTransfer 'user' address".to_owned(),
        ));
    }

    let digest = permit_typed_data_hash(domain, permit)?;
    let sig = sign(secret, &H256::from(digest)).map_err(|e| GasFreeWithdrawError::Signing(e.to_string()))?;

    let mut bytes = [0u8; 65];
    bytes[..32].copy_from_slice(sig.r());
    bytes[32..64].copy_from_slice(sig.s());
    // R6: normalize the recovery byte to the EIP-712 27/28 convention.
    bytes[64] = sig
        .v()
        .checked_add(27)
        .ok_or_else(|| GasFreeWithdrawError::Signing("recovery byte overflow".to_owned()))?;

    Ok(PermitSignature(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha3::{Digest, Keccak256};
    use std::str::FromStr;

    fn eth_from_u64(n: u64) -> EthAddress {
        let mut b = [0u8; 20];
        b[12..].copy_from_slice(&n.to_be_bytes());
        EthAddress::from(b)
    }

    fn kc(data: &[u8]) -> [u8; 32] {
        let mut o = [0u8; 32];
        o.copy_from_slice(&Keccak256::digest(data));
        o
    }

    fn pad_addr(a: &EthAddress) -> [u8; 32] {
        let mut w = [0u8; 32];
        w[12..32].copy_from_slice(a.as_ref());
        w
    }

    fn pad_u256(v: U256) -> [u8; 32] {
        let mut w = [0u8; 32];
        v.to_big_endian(&mut w);
        w
    }

    /// Independent reference implementation of the EIP-712 digest for the
    /// GasFree `PermitTransfer`, cross-checking the shared encoder (T2). Since
    /// the published per-network artifacts are not embedded, this pins the
    /// *algorithm* rather than a published address/contract vector.
    fn reference_hash(domain: &PermitDomain, p: &PermitTransfer) -> [u8; 32] {
        let domain_type = b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)";
        let mut denc = Vec::new();
        denc.extend_from_slice(&kc(domain_type));
        denc.extend_from_slice(&kc(DOMAIN_NAME.as_bytes()));
        denc.extend_from_slice(&kc(DOMAIN_VERSION.as_bytes()));
        denc.extend_from_slice(&pad_u256(U256::from(domain.chain_id)));
        denc.extend_from_slice(&pad_addr(&domain.verifying_contract));
        let domain_sep = kc(&denc);

        let permit_type = b"PermitTransfer(address token,address serviceProvider,address user,address receiver,uint256 value,uint256 maxFee,uint256 deadline,uint256 version,uint256 nonce)";
        let mut menc = Vec::new();
        menc.extend_from_slice(&kc(permit_type));
        menc.extend_from_slice(&pad_addr(&p.token));
        menc.extend_from_slice(&pad_addr(&p.service_provider));
        menc.extend_from_slice(&pad_addr(&p.user));
        menc.extend_from_slice(&pad_addr(&p.receiver));
        menc.extend_from_slice(&pad_u256(p.value));
        menc.extend_from_slice(&pad_u256(p.max_fee));
        menc.extend_from_slice(&pad_u256(U256::from(p.deadline)));
        menc.extend_from_slice(&pad_u256(U256::from(p.version)));
        menc.extend_from_slice(&pad_u256(U256::from(p.nonce)));
        let struct_hash = kc(&menc);

        let mut buf = vec![0x19, 0x01];
        buf.extend_from_slice(&domain_sep);
        buf.extend_from_slice(&struct_hash);
        kc(&buf)
    }

    fn domain() -> PermitDomain {
        PermitDomain {
            chain_id: 3_448_148_188,
            verifying_contract: eth_from_u64(0xc0_ffee),
        }
    }

    fn permit() -> PermitTransfer {
        PermitTransfer::new(
            eth_from_u64(0x1111),
            eth_from_u64(0x2222),
            eth_from_u64(0x3333),
            eth_from_u64(0x4444),
            U256::from(1_000_000u64),
            U256::from(2_000u64),
            1_900_000_000,
            7,
        )
    }

    fn test_secret_for(_user: &EthAddress) -> Secret {
        Secret::from_str("0000000000000000000000000000000000000000000000000000000000000001").unwrap()
    }

    #[test]
    fn encoder_matches_independent_reference() {
        let d = domain();
        let p = permit();
        let got = permit_typed_data_hash(&d, &p).unwrap();
        let want = reference_hash(&d, &p);
        assert_eq!(got, want, "shared encoder must match the EIP-712 reference");
    }

    #[test]
    fn hash_changes_with_message_fields() {
        let d = domain();
        let p = permit();
        let base = permit_typed_data_hash(&d, &p).unwrap();
        let mut p2 = p.clone();
        p2.nonce += 1;
        assert_ne!(base, permit_typed_data_hash(&d, &p2).unwrap());
        let mut p3 = p.clone();
        p3.value = U256::from(999u64);
        assert_ne!(base, permit_typed_data_hash(&d, &p3).unwrap());
    }

    #[test]
    fn sign_rejects_wrong_version() {
        let d = domain();
        let kp = KeyPair::from_secret(test_secret_for(&EthAddress::zero())).unwrap();
        let mut p = permit();
        p.user = kp.address();
        p.version = 2;
        let err = sign_permit(kp.secret(), &d, &p, 0).unwrap_err();
        assert!(matches!(err, GasFreeWithdrawError::Signing(_)));
    }

    #[test]
    fn sign_rejects_expired_deadline() {
        let d = domain();
        let kp = KeyPair::from_secret(test_secret_for(&EthAddress::zero())).unwrap();
        let mut p = permit();
        p.user = kp.address();
        p.deadline = 100;
        let err = sign_permit(kp.secret(), &d, &p, 200).unwrap_err();
        assert!(matches!(err, GasFreeWithdrawError::Signing(_)));
    }

    #[test]
    fn sign_rejects_signer_user_mismatch() {
        let d = domain();
        let kp = KeyPair::from_secret(test_secret_for(&EthAddress::zero())).unwrap();
        let mut p = permit();
        // user deliberately different from the key's address.
        p.user = eth_from_u64(0xabcd);
        assert_ne!(p.user, kp.address());
        let err = sign_permit(kp.secret(), &d, &p, 0).unwrap_err();
        assert!(matches!(err, GasFreeWithdrawError::Signing(_)));
    }

    #[test]
    fn sign_produces_65_bytes_v_normalized() {
        let d = domain();
        let kp = KeyPair::from_secret(test_secret_for(&EthAddress::zero())).unwrap();
        let mut p = permit();
        p.user = kp.address();
        let sig = sign_permit(kp.secret(), &d, &p, 0).unwrap();
        assert_eq!(sig.as_bytes().len(), 65);
        assert!(sig.as_bytes()[64] == 27 || sig.as_bytes()[64] == 28);
        assert_eq!(sig.to_hex().len(), 130);
        assert!(!sig.to_hex().starts_with("0x"));
    }

    #[test]
    fn signature_redacted_in_debug() {
        let d = domain();
        let kp = KeyPair::from_secret(test_secret_for(&EthAddress::zero())).unwrap();
        let mut p = permit();
        p.user = kp.address();
        let sig = sign_permit(kp.secret(), &d, &p, 0).unwrap();
        assert_eq!(format!("{:?}", sig), "PermitSignature(<redacted>)");
    }
}
