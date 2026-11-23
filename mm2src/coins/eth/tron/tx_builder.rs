//! TRON transaction builder.
//!
//! Constructs unsigned TRON transactions for native TRX transfers, TRC20
//! token transfers and other smart-contract calls. The caller is responsible for signing the
//! resulting `TransactionRaw` via [`super::sign::sign_transaction_raw`].

use crate::eth::abi::Token;

use super::address::TronAddress;
use super::proto::{ContractType, TaposBlockData, TransactionContract, TransactionRaw, TransferContract,
                   TriggerSmartContract, TRANSFER_CONTRACT_TYPE_URL, TRIGGER_SMART_CONTRACT_TYPE_URL};

use ethereum_types::U256;
use prost::Message;

/// ABI-encode a TRC20 `transfer(address, uint256)` call.
///
/// Produces the 4-byte selector + 32-byte address + 32-byte amount (68 bytes total). The amount is in the token's
/// smallest unit and fills the whole 32-byte big-endian word, as the token standard's `uint256` does.
pub fn abi_encode_trc20_transfer(to: &TronAddress, amount_units: U256) -> Vec<u8> {
    // TRC20 uses the same ABI as ERC20: transfer(address,uint256)
    // Selector: 0xa9059cbb
    let selector: [u8; 4] = [0xa9, 0x05, 0x9c, 0xbb];
    let tokens = crate::eth::abi::encode(&[Token::Address(to.to_evm_address()), Token::Uint(amount_units)]);
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
/// - `expiration_ms`: the time the transaction expires at, in milliseconds since epoch
pub fn build_trx_transfer(
    from: &TronAddress,
    to: &TronAddress,
    amount_sun: i64,
    tapos: &TaposBlockData,
    now_ms: i64,
    expiration_ms: i64,
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
        expiration: expiration_ms,
        contract: vec![TransactionContract {
            r#type: ContractType::TransferContract as i32,
            parameter: Some(any),
        }],
        timestamp: now_ms,
        fee_limit: 0, // No fee_limit needed for native transfers.
    }
}

/// Build an unsigned smart-contract call (`TriggerSmartContract`).
///
/// - `from`: the caller's TRON address
/// - `contract_addr`: the contract that is called
/// - `data`: the ABI-encoded call data, selector first
/// - `call_value_sun`: the TRX the call carries, in SUN (0 for a call that carries none)
/// - `tapos`: TAPOS block data for anti-replay
/// - `now_ms`: current timestamp in milliseconds since epoch
/// - `expiration_ms`: the time the transaction expires at, in milliseconds since epoch
/// - `fee_limit_sun`: maximum TRX fee in SUN (energy cost limit)
#[allow(clippy::too_many_arguments)]
pub fn build_trigger_smart_contract(
    from: &TronAddress,
    contract_addr: &TronAddress,
    data: Vec<u8>,
    call_value_sun: i64,
    tapos: &TaposBlockData,
    now_ms: i64,
    expiration_ms: i64,
    fee_limit_sun: i64,
) -> TransactionRaw {
    let trigger = TriggerSmartContract {
        owner_address: from.to_bytes().to_vec(),
        contract_address: contract_addr.to_bytes().to_vec(),
        call_value: call_value_sun,
        data,
    };
    let any = prost_types::Any {
        type_url: TRIGGER_SMART_CONTRACT_TYPE_URL.to_string(),
        value: trigger.encode_to_vec(),
    };
    TransactionRaw {
        ref_block_bytes: tapos.ref_block_bytes.clone(),
        ref_block_hash: tapos.ref_block_hash.clone(),
        expiration: expiration_ms,
        contract: vec![TransactionContract {
            r#type: ContractType::TriggerSmartContract as i32,
            parameter: Some(any),
        }],
        timestamp: now_ms,
        fee_limit: fee_limit_sun,
    }
}

/// Build an unsigned TRC20 token transfer transaction.
///
/// - `from`: sender TRON address
/// - `contract_addr`: TRC20 token contract address
/// - `to`: recipient TRON address
/// - `amount_units`: token amount in smallest unit, a 256-bit value
/// - `tapos`: TAPOS block data for anti-replay
/// - `now_ms`: current timestamp in milliseconds since epoch
/// - `expiration_ms`: the time the transaction expires at, in milliseconds since epoch
/// - `fee_limit_sun`: maximum TRX fee in SUN (energy cost limit)
#[allow(clippy::too_many_arguments)]
pub fn build_trc20_transfer(
    from: &TronAddress,
    contract_addr: &TronAddress,
    to: &TronAddress,
    amount_units: U256,
    tapos: &TaposBlockData,
    now_ms: i64,
    expiration_ms: i64,
    fee_limit_sun: i64,
) -> TransactionRaw {
    build_trigger_smart_contract(
        from,
        contract_addr,
        abi_encode_trc20_transfer(to, amount_units),
        0,
        tapos,
        now_ms,
        expiration_ms,
        fee_limit_sun,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eth::tron::address::TronAddress;
    use crate::eth::tron::proto::DEFAULT_EXPIRATION_SEC;
    use prost::Message as _;

    fn dummy_tapos() -> TaposBlockData {
        TaposBlockData {
            ref_block_bytes: vec![0x01, 0x02],
            ref_block_hash: vec![0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a],
        }
    }

    /// The expiry the tests build with: the default lifetime after `now_ms`.
    fn expiry(now_ms: i64) -> i64 { now_ms + DEFAULT_EXPIRATION_SEC as i64 * 1000 }

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

        let raw = build_trx_transfer(&from, &to, 1_000_000, &tapos, now_ms, expiry(now_ms));

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
        let raw = build_trx_transfer(&from, &to, 5_000_000, &tapos, 1700000000000, expiry(1700000000000));

        let encoded = raw.encode_to_vec();
        let decoded = TransactionRaw::decode(encoded.as_slice()).unwrap();
        assert_eq!(raw, decoded);
    }

    #[test]
    fn test_abi_encode_trc20_transfer() {
        let to = test_address_b();
        let data = abi_encode_trc20_transfer(&to, U256::from(1_000_000u64));
        // 4-byte selector + 32-byte address + 32-byte uint256 = 68 bytes.
        assert_eq!(data.len(), 68);
        assert_eq!(&data[..4], &[0xa9, 0x05, 0x9c, 0xbb]);
    }

    /// An amount that fits 64 bits keeps the call data it always had: the selector, the recipient's 20 bytes
    /// right-aligned in a word, and 1 000 000 = 0x0f4240 right-aligned in a word.
    #[test]
    fn test_abi_encode_trc20_transfer_small_amount_bytes() {
        let data = abi_encode_trc20_transfer(&test_address_b(), U256::from(1_000_000u64));
        let expected = concat!(
            "a9059cbb",
            "0000000000000000000000000000000000000000000000000000000000000002",
            "00000000000000000000000000000000000000000000000000000000000f4240",
        );
        assert_eq!(hex::encode(data), expected);
    }

    /// 10^24 units (a million whole tokens of 18 decimals) do not fit 64 bits. 10^24 = 0xd3c21bcecceda1000000,
    /// ten bytes, right-aligned in the amount word.
    #[test]
    fn test_abi_encode_trc20_transfer_amount_above_64_bits() {
        let amount = U256::from(1_000_000_000_000_000_000_000_000u128);
        let data = abi_encode_trc20_transfer(&test_address_b(), amount);
        let expected = concat!(
            "a9059cbb",
            "0000000000000000000000000000000000000000000000000000000000000002",
            "00000000000000000000000000000000000000000000d3c21bcecceda1000000",
        );
        assert_eq!(data.len(), 68);
        assert_eq!(hex::encode(data), expected);
    }

    #[test]
    fn test_build_trc20_transfer_structure() {
        let from = test_address_a();
        let contract_addr = TronAddress::from_hex("410000000000000000000000000000000000000099").unwrap();
        let to = test_address_b();
        let tapos = dummy_tapos();
        let now_ms = 1700000000000i64;
        let fee_limit = 100_000_000i64;

        let raw = build_trc20_transfer(
            &from,
            &contract_addr,
            &to,
            U256::from(5_000_000u64),
            &tapos,
            now_ms,
            expiry(now_ms),
            fee_limit,
        );

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

    /// A contract call carries the caller, the contract, the data and the call value it was given; a TRC20
    /// transfer is such a call with the transfer data and no call value.
    #[test]
    fn test_build_trigger_smart_contract_structure() {
        let from = test_address_a();
        let contract_addr = TronAddress::from_hex("410000000000000000000000000000000000000099").unwrap();
        let tapos = dummy_tapos();
        let now_ms = 1700000000000i64;
        let data = vec![0xde, 0xad, 0xbe, 0xef, 0x01];

        let raw = build_trigger_smart_contract(
            &from,
            &contract_addr,
            data.clone(),
            5_000_000,
            &tapos,
            now_ms,
            expiry(now_ms),
            30_000_000,
        );
        assert_eq!(raw.ref_block_bytes, tapos.ref_block_bytes);
        assert_eq!(raw.ref_block_hash, tapos.ref_block_hash);
        assert_eq!(raw.timestamp, now_ms);
        assert_eq!(raw.expiration, expiry(now_ms));
        assert_eq!(raw.fee_limit, 30_000_000);
        assert_eq!(raw.contract.len(), 1);
        assert_eq!(raw.contract[0].r#type, ContractType::TriggerSmartContract as i32);
        let any = raw.contract[0].parameter.as_ref().unwrap();
        assert_eq!(any.type_url, TRIGGER_SMART_CONTRACT_TYPE_URL);
        let inner = TriggerSmartContract::decode(any.value.as_slice()).unwrap();
        assert_eq!(inner.owner_address, from.to_bytes().to_vec());
        assert_eq!(inner.contract_address, contract_addr.to_bytes().to_vec());
        assert_eq!(inner.call_value, 5_000_000);
        assert_eq!(inner.data, data);

        let to = test_address_b();
        let transfer = build_trc20_transfer(
            &from,
            &contract_addr,
            &to,
            U256::from(5_000_000u64),
            &tapos,
            now_ms,
            expiry(now_ms),
            30_000_000,
        );
        let as_call = build_trigger_smart_contract(
            &from,
            &contract_addr,
            abi_encode_trc20_transfer(&to, U256::from(5_000_000u64)),
            0,
            &tapos,
            now_ms,
            expiry(now_ms),
            30_000_000,
        );
        assert_eq!(transfer, as_call);
    }

    #[test]
    fn test_build_trx_transfer_contract_wire_bytes() {
        let raw = build_trx_transfer(
            &test_address_a(),
            &test_address_b(),
            1_000_000,
            &dummy_tapos(),
            1700000000000,
            expiry(1700000000000),
        );
        let contract_bytes = raw.contract[0].encode_to_vec();
        assert_eq!(&contract_bytes[..3], &[0x08, 0x01, 0x12]);
    }

    #[test]
    fn test_build_trc20_transfer_contract_wire_bytes() {
        let contract_addr = TronAddress::from_hex("410000000000000000000000000000000000000099").unwrap();
        let raw = build_trc20_transfer(
            &test_address_a(),
            &contract_addr,
            &test_address_b(),
            U256::from(5_000_000u64),
            &dummy_tapos(),
            1700000000000,
            expiry(1700000000000),
            100_000_000,
        );
        let contract_bytes = raw.contract[0].encode_to_vec();
        assert_eq!(&contract_bytes[..3], &[0x08, 0x1f, 0x12]);
    }

    #[test]
    fn test_trx_transfer_raw_encoding_carries_contract_type() {
        let raw = build_trx_transfer(
            &test_address_a(),
            &test_address_b(),
            1_000_000,
            &dummy_tapos(),
            1700000000000,
            expiry(1700000000000),
        );
        let contract_bytes = raw.contract[0].encode_to_vec();
        let raw_bytes = raw.encode_to_vec();

        // The encoded contract message appears inside the encoded raw data
        // (field 11, wire type 2 => tag 0x5a, then length).
        assert!(
            raw_bytes
                .windows(contract_bytes.len())
                .any(|w| w == contract_bytes.as_slice()),
            "encoded contract not found in encoded raw data"
        );
        assert!(raw_bytes.contains(&0x5a));

        let decoded = TransactionContract::decode(contract_bytes.as_slice()).unwrap();
        assert_eq!(decoded.r#type, 1);
        let decoded_raw = TransactionRaw::decode(raw_bytes.as_slice()).unwrap();
        assert_eq!(decoded_raw.contract[0].r#type, 1);
    }
}
