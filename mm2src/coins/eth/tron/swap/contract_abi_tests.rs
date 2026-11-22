//! Tests of the swap contract's call, read and event surface (CRD ch.21 §21.11B.2, §21.11C). The cases A3 to A8
//! and D1 to D5 are those of the TRON swap fixture note, each re-checked against the ABI table of §21.11B.2: the
//! selectors and event topics are recomputed here from the dictated signatures, independently of the ABI JSON.

use super::contract_abi::{decode_allowance, decode_call, decode_event, decode_payments_output, encode_allowance_query,
                          encode_approve, encode_call, encode_payments_query, PaymentRecord, PaymentState,
                          TronSwapCall, TronSwapEvent};
use super::ids_tests::{addr, secret_hash, ID, LOCK_TIME, SECRET};
use super::TronSwapError;
use crate::eth::abi::{encode, Token};
use ethereum_types::{H256, U256};
use kdf_crypto::keccak256;

/// The 4-byte selector of the method with the canonical signature `signature`.
fn selector(signature: &str) -> [u8; 4] {
    let mut out = [0u8; 4];
    out.copy_from_slice(&keccak256(signature.as_bytes()).as_slice()[..4]);
    out
}

/// The topic of the event with the canonical signature `signature`.
fn topic(signature: &str) -> H256 { H256::from_slice(keccak256(signature.as_bytes()).as_slice()) }

/// A 32-byte word with `value` right-aligned.
fn word(value: u64) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[24..].copy_from_slice(&value.to_be_bytes());
    out
}

/// The 32-byte words of call data after its selector.
fn words(data: &[u8]) -> Vec<[u8; 32]> {
    assert_eq!((data.len() - 4) % 32, 0, "whole words after the selector");
    data[4..]
        .chunks(32)
        .map(|chunk| <[u8; 32]>::try_from(chunk).expect("a whole word"))
        .collect()
}

fn eth_payment() -> TronSwapCall {
    TronSwapCall::EthPayment {
        id: ID,
        receiver: addr(0x07),
        secret_hash: secret_hash(),
        lock_time: u64::from(LOCK_TIME),
    }
}

fn erc20_payment() -> TronSwapCall {
    TronSwapCall::Erc20Payment {
        id: ID,
        amount: U256::from(123_456u64),
        token: addr(0x09),
        receiver: addr(0x07),
        secret_hash: secret_hash(),
        lock_time: u64::from(LOCK_TIME),
    }
}

fn receiver_spend() -> TronSwapCall {
    TronSwapCall::ReceiverSpend {
        id: ID,
        amount: U256::from(999u64),
        secret: SECRET,
        token: None,
        sender: addr(0x05),
    }
}

fn sender_refund() -> TronSwapCall {
    TronSwapCall::SenderRefund {
        id: ID,
        amount: U256::from(1u64),
        secret_hash: secret_hash(),
        token: Some(addr(0x01)),
        receiver: addr(0x02),
    }
}

/// Fixture A3. ABI `ethPayment(bytes32,address,bytes32,uint64)`: the id, the receiver's 20-byte payload, the
/// 32-byte hash, the lock time.
#[test]
fn a3_eth_payment_round_trips_in_the_contracts_argument_order() {
    let data = encode_call(&eth_payment()).unwrap();
    assert_eq!(data[..4], selector("ethPayment(bytes32,address,bytes32,uint64)"));
    assert_eq!(words(&data), vec![ID, word(0x07), secret_hash(), word(1_700_000_000)]);
    assert_eq!(decode_call(&data).unwrap(), eth_payment());
}

/// Fixture A4. ABI `erc20Payment(bytes32,uint256,address,address,bytes32,uint64)`: the amount comes second, the
/// token before the receiver.
#[test]
fn a4_erc20_payment_round_trips_in_the_contracts_argument_order() {
    let data = encode_call(&erc20_payment()).unwrap();
    assert_eq!(
        data[..4],
        selector("erc20Payment(bytes32,uint256,address,address,bytes32,uint64)")
    );
    assert_eq!(words(&data), vec![
        ID,
        word(123_456),
        word(0x09),
        word(0x07),
        secret_hash(),
        word(1_700_000_000)
    ]);
    assert_eq!(decode_call(&data).unwrap(), erc20_payment());
}

/// Fixture A5. ABI `receiverSpend(bytes32,uint256,bytes32,address,address)`; the zero token address is native
/// TRX. The secret is read back from the call data.
#[test]
fn a5_receiver_spend_round_trips_and_yields_the_secret() {
    let data = encode_call(&receiver_spend()).unwrap();
    assert_eq!(
        data[..4],
        selector("receiverSpend(bytes32,uint256,bytes32,address,address)")
    );
    assert_eq!(words(&data), vec![ID, word(999), SECRET, word(0), word(0x05)]);
    let decoded = decode_call(&data).unwrap();
    assert_eq!(decoded, receiver_spend());
    let TronSwapCall::ReceiverSpend { secret, token, .. } = decoded else {
        panic!("not a spend");
    };
    assert_eq!(secret, SECRET);
    assert_eq!(token, None);

    // A spend of a token names the token.
    let token_spend = TronSwapCall::ReceiverSpend {
        id: ID,
        amount: U256::from(999u64),
        secret: SECRET,
        token: Some(addr(0x09)),
        sender: addr(0x05),
    };
    let data = encode_call(&token_spend).unwrap();
    assert_eq!(words(&data)[3], word(0x09));
    assert_eq!(decode_call(&data).unwrap(), token_spend);
}

/// Fixture A6. ABI `senderRefund(bytes32,uint256,bytes32,address,address)`: its selector is not `ethPayment`'s,
/// and the four selectors are pairwise distinct.
#[test]
fn a6_sender_refund_has_its_own_selector() {
    let data = encode_call(&sender_refund()).unwrap();
    assert_eq!(
        data[..4],
        selector("senderRefund(bytes32,uint256,bytes32,address,address)")
    );
    assert_eq!(words(&data), vec![ID, word(1), secret_hash(), word(0x01), word(0x02)]);
    assert_eq!(decode_call(&data).unwrap(), sender_refund());

    let selectors: Vec<Vec<u8>> = [eth_payment(), erc20_payment(), receiver_spend(), sender_refund()]
        .iter()
        .map(|call| encode_call(call).unwrap()[..4].to_vec())
        .collect();
    for (i, a) in selectors.iter().enumerate() {
        for b in &selectors[i + 1..] {
            assert_ne!(a, b);
        }
    }
}

/// Fixture A7. The decoder picks the method by the selector alone, so a `receiverSpend` call never comes out as
/// an `ethPayment`; call data of no lifecycle method is refused on the selector, typed.
#[test]
fn a7_call_data_is_decoded_by_its_selector_only() {
    let spend = encode_call(&receiver_spend()).unwrap();
    assert!(matches!(
        decode_call(&spend).unwrap(),
        TronSwapCall::ReceiverSpend { .. }
    ));

    // The `payments` read, a token transfer and unknown bytes are no lifecycle call.
    let read = encode_payments_query(&ID).unwrap();
    let mut transfer = selector("transfer(address,uint256)").to_vec();
    transfer.extend_from_slice(&[0u8; 64]);
    let mut unknown = vec![0xde, 0xad, 0xbe, 0xef];
    unknown.extend_from_slice(&spend[4..]);
    for data in [read.as_slice(), transfer.as_slice(), unknown.as_slice()] {
        assert!(matches!(decode_call(data), Err(TronSwapError::WrongSelector)));
    }
    // Fewer than four bytes carry no selector.
    for data in [&[][..], &spend[..3]] {
        assert!(matches!(decode_call(data), Err(TronSwapError::WrongSelector)));
    }
    // A known selector over arguments that are cut short is an ABI error, never a call.
    assert!(matches!(
        decode_call(&spend[..spend.len() - 1]),
        Err(TronSwapError::Abi(_))
    ));
    // A lock time above 64 bits does not fit the contract's `uint64`.
    let mut payment = encode_call(&eth_payment()).unwrap();
    payment[4 + 3 * 32 + 23] = 1;
    assert!(matches!(decode_call(&payment), Err(TronSwapError::Abi(_))));
}

/// What the decoder does with call data that is not in the canonical form (CRD ch.21 §21.11C R21.11C.4): bytes
/// after the arguments and set high bits in an address word are accepted, and the address is its low 20
/// bytes; set high bits in the `uint64` word are refused.
#[test]
fn non_canonical_call_data_is_decoded_as_pinned() {
    let canonical = encode_call(&eth_payment()).unwrap();

    // Trailing bytes after the four argument words.
    let mut trailing = canonical.clone();
    trailing.extend_from_slice(&[0xee; 7]);
    assert_eq!(decode_call(&trailing).unwrap(), eth_payment());
    let mut trailing_word = canonical.clone();
    trailing_word.extend_from_slice(&[0xee; 32]);
    assert_eq!(decode_call(&trailing_word).unwrap(), eth_payment());

    // The receiver word (the second) with its twelve high bytes set.
    let mut dirty_address = canonical.clone();
    for byte in &mut dirty_address[4 + 32..4 + 32 + 12] {
        *byte = 0xff;
    }
    assert_eq!(decode_call(&dirty_address).unwrap(), eth_payment());

    // The lock time word (the fourth) with a bit above 64 set.
    let mut dirty_lock_time = canonical;
    dirty_lock_time[4 + 3 * 32] = 0x80;
    assert!(matches!(decode_call(&dirty_lock_time), Err(TronSwapError::Abi(_))));
}

/// Fixture A8. Contract output `(bytes32 paymentHash, uint64 lockTime, uint8 state)`, states 0 to 3.
#[test]
fn a8_the_payments_output_decodes_to_the_stored_record() {
    let output_of = |state: u64| {
        encode(&[
            Token::FixedBytes(vec![0xAB; 32]),
            Token::Uint(U256::from(1_700_000_000u64)),
            Token::Uint(U256::from(state)),
        ])
    };
    assert_eq!(output_of(1).len(), 96);
    assert_eq!(decode_payments_output(&output_of(1)).unwrap(), PaymentRecord {
        payment_hash: [0xAB; 32],
        lock_time: 1_700_000_000,
        state: PaymentState::PaymentSent,
    });
    for (number, state) in [
        (0, PaymentState::Uninitialized),
        (1, PaymentState::PaymentSent),
        (2, PaymentState::ReceiverSpent),
        (3, PaymentState::SenderRefunded),
    ] {
        assert_eq!(decode_payments_output(&output_of(number)).unwrap().state, state);
    }
    // Any other state is a typed error, never one of the four.
    for number in [4u64, 255] {
        assert!(matches!(
            decode_payments_output(&output_of(number)),
            Err(TronSwapError::UnknownPaymentState(state)) if state == number
        ));
    }
    // Short return data, such as the empty data of a failed read, is an ABI error.
    for output in [&[][..], &output_of(1)[..95]] {
        assert!(matches!(decode_payments_output(output), Err(TronSwapError::Abi(_))));
    }

    // The read itself: `payments(bytes32)` with the id.
    let query = encode_payments_query(&ID).unwrap();
    assert_eq!(query[..4], selector("payments(bytes32)"));
    assert_eq!(words(&query), vec![ID]);
}

/// The token approval calls: `approve(address,uint256)` keeps its selector whatever the amount, and the allowance
/// is one `uint256`.
#[test]
fn the_approval_calls_follow_the_token_standard() {
    let spender = addr(0x10);
    let zero = encode_approve(&spender, U256::zero()).unwrap();
    let hundred = encode_approve(&spender, U256::from(100u64)).unwrap();
    assert_eq!(zero[..4], selector("approve(address,uint256)"));
    assert_eq!(zero[..4], hundred[..4]);
    assert_eq!(words(&zero), vec![word(0x10), word(0)]);
    assert_eq!(words(&hundred), vec![word(0x10), word(100)]);

    let query = encode_allowance_query(&addr(0x20), &spender).unwrap();
    assert_eq!(query[..4], selector("allowance(address,address)"));
    assert_eq!(words(&query), vec![word(0x20), word(0x10)]);

    assert_eq!(decode_allowance(&word(100)).unwrap(), U256::from(100u64));
    assert_eq!(decode_allowance(&[0xff; 32]).unwrap(), U256::MAX);
    assert!(matches!(decode_allowance(&[]), Err(TronSwapError::Abi(_))));
}

fn log_data(words: &[[u8; 32]]) -> Vec<u8> { words.concat() }

/// Fixture D1. `ReceiverSpent(bytes32 id, bytes32 secret)` from a log entry: the signature topic, and the id and
/// the secret as the entry's data.
#[test]
fn d1_receiver_spent_decodes_to_the_id_and_the_secret() {
    let topics = [topic("ReceiverSpent(bytes32,bytes32)")];
    let event = decode_event(&topics, &log_data(&[ID, SECRET])).unwrap();
    assert_eq!(event, TronSwapEvent::ReceiverSpent { id: ID, secret: SECRET });
    assert_eq!(event.id(), &ID);
}

/// Fixture D2. `PaymentSent(bytes32 id)` and `SenderRefunded(bytes32 id)`.
#[test]
fn d2_payment_sent_and_sender_refunded_decode_to_their_ids() {
    assert_eq!(
        decode_event(&[topic("PaymentSent(bytes32)")], &log_data(&[ID])).unwrap(),
        TronSwapEvent::PaymentSent { id: ID }
    );
    assert_eq!(
        decode_event(&[topic("SenderRefunded(bytes32)")], &log_data(&[ID])).unwrap(),
        TronSwapEvent::SenderRefunded { id: ID }
    );
}

/// Fixture D3. The three event topics are pairwise distinct.
#[test]
fn d3_the_event_topics_are_pairwise_distinct() {
    let topics = [
        topic("PaymentSent(bytes32)"),
        topic("ReceiverSpent(bytes32,bytes32)"),
        topic("SenderRefunded(bytes32)"),
    ];
    assert_ne!(topics[0], topics[1]);
    assert_ne!(topics[0], topics[2]);
    assert_ne!(topics[1], topics[2]);
}

/// Fixture D4. An unknown topic is refused, typed; so is an entry without topics. An entry of a known event whose
/// data does not fit is an ABI error.
#[test]
fn d4_an_unknown_topic_is_refused() {
    let data = log_data(&[ID]);
    assert!(matches!(
        decode_event(&[H256::from([0x99; 32])], &data),
        Err(TronSwapError::UnknownEvent)
    ));
    assert!(matches!(decode_event(&[], &data), Err(TronSwapError::UnknownEvent)));
    // `ReceiverSpent` needs the secret too; an extra topic does not fit an event without indexed parameters.
    assert!(matches!(
        decode_event(&[topic("ReceiverSpent(bytes32,bytes32)")], &data),
        Err(TronSwapError::Abi(_))
    ));
    assert!(matches!(
        decode_event(&[topic("PaymentSent(bytes32)"), H256::from(ID)], &data),
        Err(TronSwapError::Abi(_))
    ));
}

/// Fixture D5. Of several events, the one whose id is the swap's is picked; none when no id matches.
#[test]
fn d5_the_event_of_a_swap_is_found_by_its_id() {
    let other = [0x44; 32];
    let events: Vec<TronSwapEvent> = [
        (topic("PaymentSent(bytes32)"), log_data(&[other])),
        (topic("ReceiverSpent(bytes32,bytes32)"), log_data(&[other, [0x55; 32]])),
        (topic("ReceiverSpent(bytes32,bytes32)"), log_data(&[ID, SECRET])),
    ]
    .iter()
    .map(|(topic, data)| decode_event(&[*topic], data).unwrap())
    .collect();

    let spent_secret = |id: &[u8; 32]| {
        events.iter().find_map(|event| match event {
            TronSwapEvent::ReceiverSpent { id: spent, secret } if spent == id => Some(*secret),
            _ => None,
        })
    };
    assert_eq!(spent_secret(&ID), Some(SECRET));
    assert_eq!(spent_secret(&other), Some([0x55; 32]));
    assert_eq!(spent_secret(&[0x66; 32]), None);
}
