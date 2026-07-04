//! TRON transaction builder.
//!
//! Constructs unsigned TRON transactions for native TRX transfers and
//! TRC20 token transfers. The caller is responsible for signing the
//! resulting `TransactionRaw` via [`super::sign::sign_transaction_raw`].

use crate::eth::abi::Token;

use super::address::TronAddress;
use super::proto::{ContractType, TaposBlockData, TransactionContract, TransactionRaw, TransferContract,
                   TriggerSmartContract, DEFAULT_EXPIRATION_SEC, TRANSFER_CONTRACT_TYPE_URL,
                   TRIGGER_SMART_CONTRACT_TYPE_URL};

use prost::Message;

/// ABI-encode a TRC20 `transfer(address, uint256)` call.
///
/// Produces the 4-byte selector + 32-byte address + 32-byte amount (68 bytes total).
pub fn abi_encode_trc20_transfer(to: &TronAddress, amount_sun: u64) -> Vec<u8> {
    // TRC20 uses the same ABI as ERC20: transfer(address,uint256)
    // Selector: 0xa9059cbb
    let selector: [u8; 4] = [0xa9, 0x05, 0x9c, 0xbb];
    let tokens = crate::eth::abi::encode(&[Token::Address(to.to_evm_address()), Token::Uint(amount_sun.into())]);
    let mut data = Vec::with_capacity(4 + tokens.len());
    data.extend_from_slice(&selector);
    data.extend_from_slice(&tokens);
    data
}

/// Extract TAPOS block data from a block number and block ID.
///
/// - `block_num`: the block height
/// - `block_id`: 32-byte block hash (SHA-256 of block header)
pub fn tapos_from_block(block_num: u64, block_id: &[u8; 32]) -> TaposBlockData {
    let num_bytes = block_num.to_be_bytes();
    TaposBlockData {
        ref_block_bytes: vec![num_bytes[6], num_bytes[7]],
        ref_block_hash: block_id[8..16].to_vec(),
    }
}

/// Build an unsigned native TRX transfer transaction.
///
/// - `from`: sender TRON address
/// - `to`: recipient TRON address
/// - `amount_sun`: amount in SUN (1 TRX = 1,000,000 SUN)
/// - `tapos`: TAPOS block data for anti-replay
/// - `now_ms`: current timestamp in milliseconds since epoch
pub fn build_trx_transfer(
    from: &TronAddress,
    to: &TronAddress,
    amount_sun: i64,
    tapos: &TaposBlockData,
    now_ms: i64,
) -> TransactionRaw {
    let contract = TransferContract {
        owner_address: from.to_bytes().to_vec(),
        to_address: to.to_bytes().to_vec(),
        amount: amount_sun,
    };
    let any = prost_types::Any {
        type_url: TRANSFER_CONTRACT_TYPE_URL.to_string(),
        value: contract.encode_to_vec(),
    };
    TransactionRaw {
        ref_block_bytes: tapos.ref_block_bytes.clone(),
        ref_block_hash: tapos.ref_block_hash.clone(),
        expiration: now_ms + (DEFAULT_EXPIRATION_SEC as i64 * 1000),
        contract: vec![TransactionContract {
            r#type: ContractType::TransferContract as i32,
            parameter: Some(any),
        }],
        timestamp: now_ms,
        fee_limit: 0, // No fee_limit needed for native transfers.
    }
}

/// Build an unsigned TRC20 token transfer transaction.
///
/// - `from`: sender TRON address
/// - `contract_addr`: TRC20 token contract address
/// - `to`: recipient TRON address
/// - `amount_sun`: token amount in smallest unit
/// - `tapos`: TAPOS block data for anti-replay
/// - `now_ms`: current timestamp in milliseconds since epoch
/// - `fee_limit_sun`: maximum TRX fee in SUN (energy cost limit)
pub fn build_trc20_transfer(
    from: &TronAddress,
    contract_addr: &TronAddress,
    to: &TronAddress,
    amount_sun: u64,
    tapos: &TaposBlockData,
    now_ms: i64,
    fee_limit_sun: i64,
) -> TransactionRaw {
    let data = abi_encode_trc20_transfer(to, amount_sun);
    let trigger = TriggerSmartContract {
        owner_address: from.to_bytes().to_vec(),
        contract_address: contract_addr.to_bytes().to_vec(),
        call_value: 0,
        data,
    };
    let any = prost_types::Any {
        type_url: TRIGGER_SMART_CONTRACT_TYPE_URL.to_string(),
        value: trigger.encode_to_vec(),
    };
    TransactionRaw {
        ref_block_bytes: tapos.ref_block_bytes.clone(),
        ref_block_hash: tapos.ref_block_hash.clone(),
        expiration: now_ms + (DEFAULT_EXPIRATION_SEC as i64 * 1000),
        contract: vec![TransactionContract {
            r#type: ContractType::TriggerSmartContract as i32,
            parameter: Some(any),
        }],
        timestamp: now_ms,
        fee_limit: fee_limit_sun,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eth::tron::address::TronAddress;
    use prost::Message as _;

    fn dummy_tapos() -> TaposBlockData {
        TaposBlockData {
            ref_block_bytes: vec![0x01, 0x02],
            ref_block_hash: vec![0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a],
        }
    }

    fn test_address_a() -> TronAddress { TronAddress::from_hex("410000000000000000000000000000000000000001").unwrap() }

    fn test_address_b() -> TronAddress { TronAddress::from_hex("410000000000000000000000000000000000000002").unwrap() }

    #[test]
    fn test_tapos_from_block() {
        let block_num: u64 = 0x0102_0304_0506_0708;
        let block_id: [u8; 32] = [
            0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F, 0x20, 0x21,
            0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x2B, 0x2C, 0x2D, 0x2E, 0x2F,
        ];
        let tapos = tapos_from_block(block_num, &block_id);
        // ref_block_bytes = last 2 bytes of block_num big-endian
        assert_eq!(tapos.ref_block_bytes, vec![0x07, 0x08]);
        // ref_block_hash = block_id[8..16]
        assert_eq!(tapos.ref_block_hash, vec![
            0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F
        ]);
    }

    #[test]
    fn test_build_trx_transfer_structure() {
        let from = test_address_a();
        let to = test_address_b();
        let tapos = dummy_tapos();
        let now_ms = 1700000000000i64;

        let raw = build_trx_transfer(&from, &to, 1_000_000, &tapos, now_ms);

        assert_eq!(raw.ref_block_bytes, tapos.ref_block_bytes);
        assert_eq!(raw.ref_block_hash, tapos.ref_block_hash);
        assert_eq!(raw.timestamp, now_ms);
        assert_eq!(raw.expiration, now_ms + DEFAULT_EXPIRATION_SEC as i64 * 1000);
        assert_eq!(raw.fee_limit, 0);
        assert_eq!(raw.contract.len(), 1);
        assert_eq!(raw.contract[0].r#type, ContractType::TransferContract as i32);

        // Decode the inner contract.
        let any = raw.contract[0].parameter.as_ref().unwrap();
        assert_eq!(any.type_url, TRANSFER_CONTRACT_TYPE_URL);
        let inner = TransferContract::decode(any.value.as_slice()).unwrap();
        assert_eq!(inner.owner_address, from.to_bytes().to_vec());
        assert_eq!(inner.to_address, to.to_bytes().to_vec());
        assert_eq!(inner.amount, 1_000_000);
    }

    #[test]
    fn test_build_trx_transfer_roundtrip() {
        let from = test_address_a();
        let to = test_address_b();
        let tapos = dummy_tapos();
        let raw = build_trx_transfer(&from, &to, 5_000_000, &tapos, 1700000000000);

        let encoded = raw.encode_to_vec();
        let decoded = TransactionRaw::decode(encoded.as_slice()).unwrap();
        assert_eq!(raw, decoded);
    }

    #[test]
    fn test_abi_encode_trc20_transfer() {
        let to = test_address_b();
        let data = abi_encode_trc20_transfer(&to, 1_000_000);
        // 4-byte selector + 32-byte address + 32-byte uint256 = 68 bytes.
        assert_eq!(data.len(), 68);
        assert_eq!(&data[..4], &[0xa9, 0x05, 0x9c, 0xbb]);
    }

    #[test]
    fn test_build_trc20_transfer_structure() {
        let from = test_address_a();
        let contract_addr = TronAddress::from_hex("410000000000000000000000000000000000000099").unwrap();
        let to = test_address_b();
        let tapos = dummy_tapos();
        let now_ms = 1700000000000i64;
        let fee_limit = 100_000_000i64;

        let raw = build_trc20_transfer(&from, &contract_addr, &to, 5_000_000, &tapos, now_ms, fee_limit);

        assert_eq!(raw.fee_limit, fee_limit);
        assert_eq!(raw.contract.len(), 1);
        assert_eq!(raw.contract[0].r#type, ContractType::TriggerSmartContract as i32);

        let any = raw.contract[0].parameter.as_ref().unwrap();
        assert_eq!(any.type_url, TRIGGER_SMART_CONTRACT_TYPE_URL);
        let inner = TriggerSmartContract::decode(any.value.as_slice()).unwrap();
        assert_eq!(inner.owner_address, from.to_bytes().to_vec());
        assert_eq!(inner.contract_address, contract_addr.to_bytes().to_vec());
        assert_eq!(inner.call_value, 0);
        assert_eq!(inner.data.len(), 68); // ABI-encoded transfer call
    }
}
