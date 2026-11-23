//! Tests of the TRON swap identifiers (CRD ch.21 §21.11B.2, §21.11C). The cases A1 and A2 are those of the TRON
//! swap fixture note, re-checked against §21.11B.2. The expected digests were computed outside this crate (a plain
//! SHA-256 over the bytes the comments spell out), and each test recomputes them with the crypto crate too.

use super::contract_abi::{encode_call, TronSwapCall};
use super::ids::{require_32_byte_hash, tron_payment_hash, tron_swap_id};
use super::TronSwapError;
use crate::eth::secret_hash_value::contract_secret_hash;
use crate::eth::tron::TronAddress;
use ethereum_types::{Address, U256};
use kdf_crypto::sha256;

/// The sample swap id of the fixtures.
pub(super) const ID: [u8; 32] = [0x11; 32];
/// The sample secret of the fixtures.
pub(super) const SECRET: [u8; 32] = [0x22; 32];
/// The sample lock time of the fixtures.
pub(super) const LOCK_TIME: u32 = 1_700_000_000;
/// SHA-256 of [`SECRET`].
const SECRET_HASH_HEX: &str = "9f72ea0cf49536e3c66c787f705186df9a4378083753ae9536d65b3ad7fcddc4";

pub(super) fn hex32(text: &str) -> [u8; 32] {
    let bytes = hex::decode(text).expect("hex");
    <[u8; 32]>::try_from(bytes.as_slice()).expect("32 bytes")
}

/// The sample secret hash: SHA-256 of the sample secret, 32 bytes.
pub(super) fn secret_hash() -> [u8; 32] { hex32(SECRET_HASH_HEX) }

/// A sample address: twenty zero bytes with only the last byte set.
pub(super) fn addr(last: u8) -> TronAddress {
    let mut payload = [0u8; 20];
    payload[19] = last;
    TronAddress::from_evm_address(Address::from(payload))
}

pub(super) fn digest(input: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(sha256(input).as_slice());
    out
}

/// Fixture A1. §21.11B.2: the secret hash is SHA-256(secret), 32 bytes, and the client passes it as it is.
#[test]
fn a1_the_secret_hash_is_a_plain_sha256_digest_of_32_bytes() {
    assert_eq!(digest(&SECRET), secret_hash());
    assert_eq!(require_32_byte_hash(&secret_hash()).unwrap(), secret_hash());
}

/// Fixture A2. §21.11B.2: the swap id is SHA-256(little-endian uint32(lockTime) ‖ secretHash) over the 32-byte
/// value. 1 700 000 000 is 0x6553f100, so the input starts with 00 f1 53 65.
#[test]
fn a2_the_swap_id_is_the_sha256_of_the_lock_time_and_the_32_byte_hash() {
    let id = tron_swap_id(LOCK_TIME, &secret_hash());
    assert_eq!(
        hex::encode(id),
        "13fb7b1b4f82774a52dfc6a651974afece373db4315c9ba2d41a16db64618496"
    );
    let mut input = vec![0x00, 0xf1, 0x53, 0x65];
    input.extend_from_slice(&secret_hash());
    assert_eq!(id, digest(&input));
    // Another lock time or another hash gives another id.
    assert_ne!(id, tron_swap_id(LOCK_TIME + 1, &secret_hash()));
    assert_ne!(id, tron_swap_id(LOCK_TIME, &[0x33; 32]));
}

/// §21.11B.2: the payment hash is SHA-256 over the packed `(receiver, sender, secretHash, tokenAddress, amount)`:
/// three 20-byte payloads, the 32-byte hash and a 32-byte amount word, 124 bytes, with the zero address for TRX.
#[test]
fn the_payment_hash_is_the_sha256_of_the_packed_tuple() {
    let (receiver, sender, token) = (addr(0x07), addr(0x05), addr(0x09));
    let packed = |token: [u8; 20], amount: u64| {
        let mut bytes = Vec::with_capacity(124);
        bytes.extend_from_slice(receiver.to_evm_address().as_bytes());
        bytes.extend_from_slice(sender.to_evm_address().as_bytes());
        bytes.extend_from_slice(&secret_hash());
        bytes.extend_from_slice(&token);
        bytes.extend_from_slice(&[0u8; 24]);
        bytes.extend_from_slice(&amount.to_be_bytes());
        assert_eq!(bytes.len(), 124);
        bytes
    };

    // Native TRX: 5 000 000 SUN, the zero address in the token place.
    let trx = tron_payment_hash(&receiver, &sender, &secret_hash(), None, U256::from(5_000_000u64));
    assert_eq!(
        hex::encode(trx),
        "1d33f02eef38c2949207371168eec667c1e5d55eb9c4002f6585b98966d51b41"
    );
    assert_eq!(trx, digest(&packed([0u8; 20], 5_000_000)));

    // A TRC20 token: 123 456 units of the token ...09.
    let trc20 = tron_payment_hash(&receiver, &sender, &secret_hash(), Some(&token), U256::from(123_456u64));
    assert_eq!(
        hex::encode(trc20),
        "4fae25966d81f823c32ac20d4d174842de95a3e2df203ad5b8c4a95f333a84a8"
    );
    let mut token_payload = [0u8; 20];
    token_payload[19] = 0x09;
    assert_eq!(trc20, digest(&packed(token_payload, 123_456)));

    // A change of any one member changes the hash.
    let amount = U256::from(5_000_000u64);
    let others = [
        tron_payment_hash(&addr(0x99), &sender, &secret_hash(), None, amount),
        tron_payment_hash(&receiver, &addr(0x99), &secret_hash(), None, amount),
        tron_payment_hash(&receiver, &sender, &[0x33; 32], None, amount),
        tron_payment_hash(&receiver, &sender, &secret_hash(), Some(&token), amount),
        tron_payment_hash(&receiver, &sender, &secret_hash(), None, amount + 1),
    ];
    for other in others {
        assert_ne!(other, trx);
    }
    // The amount is a whole 256-bit word: a value above 64 bits is hashed in full.
    let wide = U256::from(1u64) << 200;
    assert_ne!(
        tron_payment_hash(&receiver, &sender, &secret_hash(), None, wide),
        tron_payment_hash(&receiver, &sender, &secret_hash(), None, U256::zero())
    );
}

/// CRD ch.21 R21.B6: a TRON swap operation refuses a secret hash that is not 32 bytes.
#[test]
fn only_a_32_byte_secret_hash_is_accepted() {
    assert_eq!(require_32_byte_hash(&[0x5a; 32]).unwrap(), [0x5a; 32]);
    for width in [0usize, 20, 31, 33] {
        let refused = require_32_byte_hash(&vec![0x5a; width]);
        assert!(
            matches!(refused, Err(TronSwapError::HashWidth { got }) if got == width),
            "{width}: {refused:?}"
        );
    }
}

/// CRD ch.21 R21.B6, ch.51 R72A: the EVM family derives its `bytes20` contract value from a 32-byte secret hash
/// with RIPEMD-160. No TRON helper does: the 32 bytes reach the call data, the swap id and the payment hash as
/// they are, and the narrowed value appears nowhere.
#[test]
fn no_tron_helper_narrows_a_32_byte_secret_hash() {
    let hash = secret_hash();
    let narrowed = contract_secret_hash(&hash).expect("the EVM derivation takes 32 bytes");
    assert_ne!(&hash[..20], &narrowed[..]);

    assert_eq!(require_32_byte_hash(&hash).unwrap(), hash);

    let calls = [
        TronSwapCall::EthPayment {
            id: ID,
            receiver: addr(0x07),
            secret_hash: hash,
            lock_time: u64::from(LOCK_TIME),
        },
        TronSwapCall::Erc20Payment {
            id: ID,
            amount: U256::from(123_456u64),
            token: addr(0x09),
            receiver: addr(0x07),
            secret_hash: hash,
            lock_time: u64::from(LOCK_TIME),
        },
        TronSwapCall::SenderRefund {
            id: ID,
            amount: U256::from(1u64),
            secret_hash: hash,
            token: None,
            receiver: addr(0x07),
        },
    ];
    for call in calls {
        let data = encode_call(&call).unwrap();
        assert!(data[4..].chunks(32).any(|word| word == hash), "{call:?}");
        assert!(!data.windows(20).any(|window| window == narrowed), "{call:?}");
    }

    let mut narrowed_input = LOCK_TIME.to_le_bytes().to_vec();
    narrowed_input.extend_from_slice(&narrowed);
    assert_ne!(tron_swap_id(LOCK_TIME, &hash), digest(&narrowed_input));
}
