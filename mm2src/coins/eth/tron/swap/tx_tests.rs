//! Tests of [`TronSwapTx`] (CRD ch.21 §21.6, §21.11C): the bytes a swap record keeps, the transaction id, and the
//! contract call a transaction makes. The case D6 is that of the TRON swap fixture note.

use super::contract_abi::{encode_call, TronSwapCall};
use super::ids_tests::{addr, digest, secret_hash, ID, LOCK_TIME, SECRET};
use super::tx::{call_of, TronSwapTx, TronTrigger};
use super::TronSwapError;
use crate::eth::tron::proto::{TaposBlockData, Transaction as ProtoTransaction, TransactionRaw};
use crate::eth::tron::tx_builder::{build_trigger_smart_contract, build_trx_transfer};
use crate::{BytesJson, Transaction, TransactionEnum};
use ethereum_types::U256;
use prost::Message;

const NOW_MS: i64 = 1_700_000_000_000;
const SWAP_CONTRACT: u8 = 0x10;
const OWNER: u8 = 0x20;

fn tapos() -> TaposBlockData {
    TaposBlockData {
        ref_block_bytes: vec![0x01, 0x02],
        ref_block_hash: vec![0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a],
    }
}

/// A call of the swap contract by [`OWNER`], signed with a dummy 65-byte signature.
fn call_tx(call: &TronSwapCall, call_value: i64) -> TronSwapTx {
    let raw = build_trigger_smart_contract(
        &addr(OWNER),
        &addr(SWAP_CONTRACT),
        encode_call(call).unwrap(),
        call_value,
        &tapos(),
        NOW_MS,
        NOW_MS + 60_000,
        30_000_000,
    );
    TronSwapTx::new(raw, vec![0x5a; 65])
}

fn eth_payment() -> TronSwapCall {
    TronSwapCall::EthPayment {
        id: ID,
        receiver: addr(0x30),
        secret_hash: secret_hash(),
        lock_time: u64::from(LOCK_TIME),
    }
}

fn receiver_spend() -> TronSwapCall {
    TronSwapCall::ReceiverSpend {
        id: ID,
        amount: U256::from(5_000_000u64),
        secret: SECRET,
        token: None,
        sender: addr(OWNER),
    }
}

/// The bytes of a transaction are the protobuf `Transaction`, and its id is the SHA-256 of the raw body's bytes.
#[test]
fn the_bytes_are_the_protobuf_transaction_and_the_id_hashes_the_raw_body() {
    let tx = call_tx(&eth_payment(), 5_000_000);
    let bytes = tx.to_bytes();
    let proto = ProtoTransaction::decode(bytes.as_slice()).unwrap();
    assert_eq!(proto.raw_data.as_ref(), Some(tx.raw()));
    assert_eq!(proto.signature, vec![vec![0x5a; 65]]);
    assert_eq!(tx.signature(), &[0x5a; 65][..]);
    assert_eq!(tx.txid().as_bytes(), &digest(&tx.raw().encode_to_vec())[..]);

    assert_eq!(TronSwapTx::from_bytes(&bytes).unwrap(), tx);
}

/// A swap record keeps a transaction as its bytes and its hash: both survive the way through `TransactionEnum`.
#[test]
fn the_transaction_round_trips_through_the_transaction_enum() {
    let tx = call_tx(&eth_payment(), 5_000_000);
    let as_enum = TransactionEnum::from(tx.clone());
    assert_eq!(as_enum.tx_hex(), tx.to_bytes());
    assert_eq!(as_enum.tx_hash(), BytesJson::from(tx.txid().as_bytes().to_vec()));

    let back = TronSwapTx::from_bytes(&as_enum.tx_hex()).unwrap();
    assert_eq!(TransactionEnum::from(back), as_enum);
    assert!(matches!(&as_enum, TransactionEnum::TronTransaction(inner) if *inner == tx));
}

/// Bytes that are no signed transaction of the exchanged form are refused, typed.
#[test]
fn malformed_bytes_are_refused() {
    let tx = call_tx(&eth_payment(), 5_000_000);
    let refused = |bytes: &[u8]| matches!(TronSwapTx::from_bytes(bytes), Err(TronSwapError::Transaction(_)));

    // Nothing at all, and bytes that are no protobuf message.
    assert!(refused(&[]));
    assert!(refused(&[0xff, 0xff, 0xff]));
    // No signature, two signatures, a signature of another length.
    for signature in [vec![], vec![vec![0x5a; 65], vec![0x5a; 65]], vec![vec![0x5a; 64]]] {
        let bytes = ProtoTransaction {
            raw_data: Some(tx.raw().clone()),
            signature,
        }
        .encode_to_vec();
        assert!(refused(&bytes));
    }
    // A raw body with a field this build does not model (tag 10, the `data` member, here two bytes): its id could
    // not be kept, so it is refused and not silently re-encoded.
    let mut raw_bytes = tx.raw().encode_to_vec();
    raw_bytes.extend_from_slice(&[0x52, 0x02, 0xab, 0xcd]);
    let mut bytes = vec![0x0a];
    prost::encoding::encode_varint(raw_bytes.len() as u64, &mut bytes);
    bytes.extend_from_slice(&raw_bytes);
    bytes.extend_from_slice(&[0x12, 65]);
    bytes.extend_from_slice(&[0x5a; 65]);
    assert!(refused(&bytes));
}

/// The contract call of a built transaction: the caller, the contract, the call value and the decoded call.
#[test]
fn call_of_reads_the_contract_the_call_value_and_the_call() {
    let tx = call_tx(&eth_payment(), 5_000_000);
    assert_eq!(tx.trigger().unwrap(), TronTrigger {
        owner: addr(OWNER),
        contract: addr(SWAP_CONTRACT),
        call_value: 5_000_000,
        data: encode_call(&eth_payment()).unwrap(),
    });
    assert_eq!(call_of(&tx).unwrap(), (addr(SWAP_CONTRACT), 5_000_000, eth_payment()));
    assert_eq!(tx.raw().fee_limit, 30_000_000);

    // The same holds after the way through the bytes.
    let received = TronSwapTx::from_bytes(&tx.to_bytes()).unwrap();
    assert_eq!(
        call_of(&received).unwrap(),
        (addr(SWAP_CONTRACT), 5_000_000, eth_payment())
    );
}

/// Fixture D6. A surfaced spend transaction yields the secret; a surfaced payment transaction yields the swap id.
#[test]
fn d6_a_spend_yields_the_secret_and_a_payment_the_swap_id() {
    let (_, _, spend) = call_of(&call_tx(&receiver_spend(), 0)).unwrap();
    let TronSwapCall::ReceiverSpend { secret, .. } = spend else {
        panic!("not a spend");
    };
    assert_eq!(secret, SECRET);
    assert_eq!(digest(&secret), secret_hash());

    let (_, _, payment) = call_of(&call_tx(&eth_payment(), 5_000_000)).unwrap();
    assert_eq!(payment.id(), &ID);
}

/// A TRX transfer is no contract call; a contract call that is no swap-contract method is refused on its selector.
#[test]
fn call_of_refuses_what_is_no_swap_contract_call() {
    let transfer = build_trx_transfer(&addr(OWNER), &addr(0x30), 1_000_000, &tapos(), NOW_MS, NOW_MS + 60_000);
    let transfer = TronSwapTx::new(transfer, vec![0x5a; 65]);
    assert!(matches!(transfer.trigger(), Err(TronSwapError::NotAContractCall)));
    assert!(matches!(call_of(&transfer), Err(TronSwapError::NotAContractCall)));

    // A transaction without a contract, or with two.
    let empty = TransactionRaw {
        ref_block_bytes: vec![0x01, 0x02],
        ref_block_hash: vec![0; 8],
        expiration: NOW_MS + 60_000,
        contract: vec![],
        timestamp: NOW_MS,
        fee_limit: 0,
    };
    assert!(matches!(
        TronSwapTx::new(empty, vec![0x5a; 65]).trigger(),
        Err(TronSwapError::NotAContractCall)
    ));
    let mut doubled = call_tx(&eth_payment(), 5_000_000).raw().clone();
    doubled.contract.push(doubled.contract[0].clone());
    assert!(matches!(
        TronSwapTx::new(doubled, vec![0x5a; 65]).trigger(),
        Err(TronSwapError::NotAContractCall)
    ));

    // A token transfer is a contract call, and no call of the swap contract.
    let token_transfer = build_trigger_smart_contract(
        &addr(OWNER),
        &addr(0x40),
        crate::eth::tron::tx_builder::abi_encode_trc20_transfer(&addr(0x55), U256::from(3_000_000u64)),
        0,
        &tapos(),
        NOW_MS,
        NOW_MS + 60_000,
        30_000_000,
    );
    let token_transfer = TronSwapTx::new(token_transfer, vec![0x5a; 65]);
    assert_eq!(token_transfer.trigger().unwrap().contract, addr(0x40));
    assert!(matches!(call_of(&token_transfer), Err(TronSwapError::WrongSelector)));
}
