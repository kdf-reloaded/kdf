//! The call, read and event surface of the public Tron-VM V1 swap contract (CRD ch.21 §21.11B.2), and the token
//! approval calls a TRC20 payment needs.
//!
//! Addresses inside the ABI encoding are the bare 20-byte payloads (CRD ch.21 §21.5). A token address of zero
//! is the contract's notation for native TRX; the spend and refund calls carry it as `None` here.

use super::TronSwapError;
use crate::eth::abi::{AbiError, Contract, Event, Function, Token};
use crate::eth::eth_types::ERC20_CONTRACT;
use crate::eth::tron::TronAddress;
use ethereum_types::{Address, H256, U256};
use std::sync::OnceLock;

/// The interface of the Tron-VM V1 swap contract as a JSON ABI.
///
/// Provenance: written out for this module from the ABI table of CRD ch.21 §21.11B.2 (the dictated signatures of
/// the four lifecycle methods, the `payments` read, the three events and the one error). The same interface is
/// readable from the public chain: a full node's contract query for a deployed instance lists it (§21.11B.6).
/// Unlike the EVM V1 contract, the secret hash and the payment hash are `bytes32`.
pub(crate) const TRON_SWAP_CONTRACT_ABI: &str = r#"[
  {"type":"function","name":"ethPayment","stateMutability":"payable","outputs":[],"inputs":[
    {"name":"id","type":"bytes32"},
    {"name":"receiver","type":"address"},
    {"name":"secretHash","type":"bytes32"},
    {"name":"lockTime","type":"uint64"}]},
  {"type":"function","name":"erc20Payment","stateMutability":"nonpayable","outputs":[],"inputs":[
    {"name":"id","type":"bytes32"},
    {"name":"amount","type":"uint256"},
    {"name":"tokenAddress","type":"address"},
    {"name":"receiver","type":"address"},
    {"name":"secretHash","type":"bytes32"},
    {"name":"lockTime","type":"uint64"}]},
  {"type":"function","name":"receiverSpend","stateMutability":"nonpayable","outputs":[],"inputs":[
    {"name":"id","type":"bytes32"},
    {"name":"amount","type":"uint256"},
    {"name":"secret","type":"bytes32"},
    {"name":"tokenAddress","type":"address"},
    {"name":"sender","type":"address"}]},
  {"type":"function","name":"senderRefund","stateMutability":"nonpayable","outputs":[],"inputs":[
    {"name":"id","type":"bytes32"},
    {"name":"amount","type":"uint256"},
    {"name":"secretHash","type":"bytes32"},
    {"name":"tokenAddress","type":"address"},
    {"name":"receiver","type":"address"}]},
  {"type":"function","name":"payments","stateMutability":"view","inputs":[
    {"name":"id","type":"bytes32"}],"outputs":[
    {"name":"paymentHash","type":"bytes32"},
    {"name":"lockTime","type":"uint64"},
    {"name":"state","type":"uint8"}]},
  {"type":"event","name":"PaymentSent","anonymous":false,"inputs":[
    {"name":"id","type":"bytes32","indexed":false}]},
  {"type":"event","name":"ReceiverSpent","anonymous":false,"inputs":[
    {"name":"id","type":"bytes32","indexed":false},
    {"name":"secret","type":"bytes32","indexed":false}]},
  {"type":"event","name":"SenderRefunded","anonymous":false,"inputs":[
    {"name":"id","type":"bytes32","indexed":false}]},
  {"type":"error","name":"SafeERC20FailedOperation","inputs":[
    {"name":"token","type":"address"}]}
]"#;

/// The parsed [`TRON_SWAP_CONTRACT_ABI`].
fn swap_contract() -> Result<&'static Contract, TronSwapError> {
    static CONTRACT: OnceLock<Result<Contract, AbiError>> = OnceLock::new();
    CONTRACT
        .get_or_init(|| Contract::load(TRON_SWAP_CONTRACT_ABI.as_bytes()))
        .as_ref()
        .map_err(|e| TronSwapError::Abi(e.clone()))
}

fn swap_function(name: &str) -> Result<&'static Function, TronSwapError> { Ok(swap_contract()?.function(name)?) }

fn swap_event(name: &str) -> Result<&'static Event, TronSwapError> { Ok(swap_contract()?.event(name)?) }

/// A call of one of the four lifecycle methods of the swap contract, with its arguments in the contract's order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TronSwapCall {
    /// `ethPayment(bytes32 id, address receiver, bytes32 secretHash, uint64 lockTime)`: locks native TRX. The
    /// amount is the call value of the transaction, not an argument.
    EthPayment {
        id: [u8; 32],
        receiver: TronAddress,
        secret_hash: [u8; 32],
        lock_time: u64,
    },
    /// `erc20Payment(bytes32 id, uint256 amount, address tokenAddress, address receiver, bytes32 secretHash,
    /// uint64 lockTime)`: locks a TRC20 amount.
    Erc20Payment {
        id: [u8; 32],
        amount: U256,
        token: TronAddress,
        receiver: TronAddress,
        secret_hash: [u8; 32],
        lock_time: u64,
    },
    /// `receiverSpend(bytes32 id, uint256 amount, bytes32 secret, address tokenAddress, address sender)`: claims a
    /// payment by revealing the secret. `token` is `None` for native TRX.
    ReceiverSpend {
        id: [u8; 32],
        amount: U256,
        secret: [u8; 32],
        token: Option<TronAddress>,
        sender: TronAddress,
    },
    /// `senderRefund(bytes32 id, uint256 amount, bytes32 secretHash, address tokenAddress, address receiver)`:
    /// refunds a payment after its lock time. `token` is `None` for native TRX.
    SenderRefund {
        id: [u8; 32],
        amount: U256,
        secret_hash: [u8; 32],
        token: Option<TronAddress>,
        receiver: TronAddress,
    },
}

impl TronSwapCall {
    /// The swap id the call names.
    pub fn id(&self) -> &[u8; 32] {
        match self {
            TronSwapCall::EthPayment { id, .. }
            | TronSwapCall::Erc20Payment { id, .. }
            | TronSwapCall::ReceiverSpend { id, .. }
            | TronSwapCall::SenderRefund { id, .. } => id,
        }
    }

    /// The name of the contract method.
    fn method(&self) -> &'static str {
        match self {
            TronSwapCall::EthPayment { .. } => "ethPayment",
            TronSwapCall::Erc20Payment { .. } => "erc20Payment",
            TronSwapCall::ReceiverSpend { .. } => "receiverSpend",
            TronSwapCall::SenderRefund { .. } => "senderRefund",
        }
    }
}

/// The four lifecycle methods, for matching a selector.
const CALL_METHODS: [&str; 4] = ["ethPayment", "erc20Payment", "receiverSpend", "senderRefund"];

fn word_token(word: &[u8; 32]) -> Token { Token::FixedBytes(word.to_vec()) }

fn address_token(address: &TronAddress) -> Token { Token::Address(address.to_evm_address()) }

/// The token argument of a spend or refund: the zero address is the contract's notation for native TRX.
fn asset_token(token: Option<TronAddress>) -> Token {
    Token::Address(token.map_or_else(Address::zero, |token| token.to_evm_address()))
}

fn word_of(token: &Token) -> Result<[u8; 32], TronSwapError> {
    match token {
        Token::FixedBytes(bytes) => <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| AbiError::InvalidData.into()),
        _ => Err(AbiError::InvalidData.into()),
    }
}

fn address_of(token: &Token) -> Result<TronAddress, TronSwapError> {
    match token {
        Token::Address(address) => Ok(TronAddress::from_evm_address(*address)),
        _ => Err(AbiError::InvalidData.into()),
    }
}

fn asset_of(token: &Token) -> Result<Option<TronAddress>, TronSwapError> {
    let address = address_of(token)?;
    Ok(if address.to_evm_address().is_zero() {
        None
    } else {
        Some(address)
    })
}

fn uint_of(token: &Token) -> Result<U256, TronSwapError> {
    match token {
        Token::Uint(value) => Ok(*value),
        _ => Err(AbiError::InvalidData.into()),
    }
}

fn u64_of(token: &Token) -> Result<u64, TronSwapError> {
    let value = uint_of(token)?;
    if value > U256::from(u64::MAX) {
        return Err(AbiError::InvalidData.into());
    }
    Ok(value.low_u64())
}

/// The call data of `call`: the method's selector and its ABI-encoded arguments.
///
/// # Errors
///
/// [`TronSwapError::Abi`] if the built-in ABI does not load or does not take the arguments.
pub fn encode_call(call: &TronSwapCall) -> Result<Vec<u8>, TronSwapError> {
    let tokens = match call {
        TronSwapCall::EthPayment {
            id,
            receiver,
            secret_hash,
            lock_time,
        } => vec![
            word_token(id),
            address_token(receiver),
            word_token(secret_hash),
            Token::Uint(U256::from(*lock_time)),
        ],
        TronSwapCall::Erc20Payment {
            id,
            amount,
            token,
            receiver,
            secret_hash,
            lock_time,
        } => vec![
            word_token(id),
            Token::Uint(*amount),
            address_token(token),
            address_token(receiver),
            word_token(secret_hash),
            Token::Uint(U256::from(*lock_time)),
        ],
        TronSwapCall::ReceiverSpend {
            id,
            amount,
            secret,
            token,
            sender,
        } => vec![
            word_token(id),
            Token::Uint(*amount),
            word_token(secret),
            asset_token(*token),
            address_token(sender),
        ],
        TronSwapCall::SenderRefund {
            id,
            amount,
            secret_hash,
            token,
            receiver,
        } => vec![
            word_token(id),
            Token::Uint(*amount),
            word_token(secret_hash),
            asset_token(*token),
            address_token(receiver),
        ],
    };
    Ok(swap_function(call.method())?.encode_input(&tokens)?)
}

/// The call that the call data `data` makes. The method is chosen by the selector alone, so call data of one
/// method never decodes as another.
///
/// # Errors
///
/// [`TronSwapError::WrongSelector`] when `data` does not start with the selector of one of the four lifecycle
/// methods; [`TronSwapError::Abi`] when the arguments do not decode as that method's.
pub fn decode_call(data: &[u8]) -> Result<TronSwapCall, TronSwapError> {
    let Some((selector, arguments)) = data.split_first_chunk::<4>() else {
        return Err(TronSwapError::WrongSelector);
    };
    let mut method = None;
    for name in CALL_METHODS {
        let function = swap_function(name)?;
        if function.short_signature() == *selector {
            method = Some((name, function));
            break;
        }
    }
    let Some((name, function)) = method else {
        return Err(TronSwapError::WrongSelector);
    };
    let tokens = function.decode_input(arguments)?;
    match (name, tokens.as_slice()) {
        ("ethPayment", [id, receiver, secret_hash, lock_time]) => Ok(TronSwapCall::EthPayment {
            id: word_of(id)?,
            receiver: address_of(receiver)?,
            secret_hash: word_of(secret_hash)?,
            lock_time: u64_of(lock_time)?,
        }),
        ("erc20Payment", [id, amount, token, receiver, secret_hash, lock_time]) => Ok(TronSwapCall::Erc20Payment {
            id: word_of(id)?,
            amount: uint_of(amount)?,
            token: address_of(token)?,
            receiver: address_of(receiver)?,
            secret_hash: word_of(secret_hash)?,
            lock_time: u64_of(lock_time)?,
        }),
        ("receiverSpend", [id, amount, secret, token, sender]) => Ok(TronSwapCall::ReceiverSpend {
            id: word_of(id)?,
            amount: uint_of(amount)?,
            secret: word_of(secret)?,
            token: asset_of(token)?,
            sender: address_of(sender)?,
        }),
        ("senderRefund", [id, amount, secret_hash, token, receiver]) => Ok(TronSwapCall::SenderRefund {
            id: word_of(id)?,
            amount: uint_of(amount)?,
            secret_hash: word_of(secret_hash)?,
            token: asset_of(token)?,
            receiver: address_of(receiver)?,
        }),
        _ => Err(AbiError::InvalidData.into()),
    }
}

/// The state of a payment in the contract (CRD ch.21 §21.11B.2: `state` 0 to 3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaymentState {
    /// 0: the contract holds no payment under the id.
    Uninitialized,
    /// 1: the payment is locked.
    PaymentSent,
    /// 2: the receiver has claimed it.
    ReceiverSpent,
    /// 3: the sender has taken it back.
    SenderRefunded,
}

/// What `payments(id)` returns: `(bytes32 paymentHash, uint64 lockTime, uint8 state)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaymentRecord {
    /// The stored payment hash ([`super::tron_payment_hash`]).
    pub payment_hash: [u8; 32],
    /// The lock time, in seconds.
    pub lock_time: u64,
    /// The state of the payment.
    pub state: PaymentState,
}

/// The call data of the read `payments(id)`.
///
/// # Errors
///
/// [`TronSwapError::Abi`] if the built-in ABI does not load.
pub fn encode_payments_query(id: &[u8; 32]) -> Result<Vec<u8>, TronSwapError> {
    Ok(swap_function("payments")?.encode_input(&[word_token(id)])?)
}

/// The record in the return data of `payments(id)`.
///
/// # Errors
///
/// [`TronSwapError::Abi`] when `output` does not decode as the three values;
/// [`TronSwapError::UnknownPaymentState`] for a state outside 0 to 3.
pub fn decode_payments_output(output: &[u8]) -> Result<PaymentRecord, TronSwapError> {
    let tokens = swap_function("payments")?.decode_output(output)?;
    let [payment_hash, lock_time, state] = tokens.as_slice() else {
        return Err(AbiError::InvalidData.into());
    };
    let state = match u64_of(state)? {
        0 => PaymentState::Uninitialized,
        1 => PaymentState::PaymentSent,
        2 => PaymentState::ReceiverSpent,
        3 => PaymentState::SenderRefunded,
        other => return Err(TronSwapError::UnknownPaymentState(other)),
    };
    Ok(PaymentRecord {
        payment_hash: word_of(payment_hash)?,
        lock_time: u64_of(lock_time)?,
        state,
    })
}

/// The call data of the token call `approve(spender, amount)`.
///
/// # Errors
///
/// [`TronSwapError::Abi`] if the token ABI does not take the arguments.
pub fn encode_approve(spender: &TronAddress, amount: U256) -> Result<Vec<u8>, TronSwapError> {
    Ok(ERC20_CONTRACT
        .function("approve")?
        .encode_input(&[address_token(spender), Token::Uint(amount)])?)
}

/// The call data of the token read `allowance(owner, spender)`.
///
/// # Errors
///
/// [`TronSwapError::Abi`] if the token ABI does not take the arguments.
pub fn encode_allowance_query(owner: &TronAddress, spender: &TronAddress) -> Result<Vec<u8>, TronSwapError> {
    Ok(ERC20_CONTRACT
        .function("allowance")?
        .encode_input(&[address_token(owner), address_token(spender)])?)
}

/// The allowance in the return data of `allowance(owner, spender)`.
///
/// # Errors
///
/// [`TronSwapError::Abi`] when `output` does not decode as one `uint256`.
pub fn decode_allowance(output: &[u8]) -> Result<U256, TronSwapError> {
    let tokens = ERC20_CONTRACT.function("allowance")?.decode_output(output)?;
    match tokens.as_slice() {
        [allowance] => uint_of(allowance),
        _ => Err(AbiError::InvalidData.into()),
    }
}

/// An event of the swap contract, from a log entry of a transaction's receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TronSwapEvent {
    /// `PaymentSent(bytes32 id)`.
    PaymentSent { id: [u8; 32] },
    /// `ReceiverSpent(bytes32 id, bytes32 secret)`: carries the revealed secret.
    ReceiverSpent { id: [u8; 32], secret: [u8; 32] },
    /// `SenderRefunded(bytes32 id)`.
    SenderRefunded { id: [u8; 32] },
}

impl TronSwapEvent {
    /// The swap id the event names.
    pub fn id(&self) -> &[u8; 32] {
        match self {
            TronSwapEvent::PaymentSent { id }
            | TronSwapEvent::ReceiverSpent { id, .. }
            | TronSwapEvent::SenderRefunded { id } => id,
        }
    }
}

/// The event of a log entry with the topic list `topics` (the signature topic first) and the data `data`.
///
/// The caller checks that the entry comes from the swap contract; a secret read from an event is used only after
/// it has been checked against the secret hash.
///
/// # Errors
///
/// [`TronSwapError::UnknownEvent`] when the entry has no topic or its first topic is none of the three events;
/// [`TronSwapError::Abi`] when the entry does not decode as that event.
pub fn decode_event(topics: &[H256], data: &[u8]) -> Result<TronSwapEvent, TronSwapError> {
    let Some(signature) = topics.first() else {
        return Err(TronSwapError::UnknownEvent);
    };
    let payment_sent = swap_event("PaymentSent")?;
    let receiver_spent = swap_event("ReceiverSpent")?;
    let sender_refunded = swap_event("SenderRefunded")?;
    if *signature == payment_sent.signature() {
        match payment_sent.decode_log(topics, data)?.as_slice() {
            [id] => Ok(TronSwapEvent::PaymentSent { id: word_of(id)? }),
            _ => Err(AbiError::InvalidData.into()),
        }
    } else if *signature == receiver_spent.signature() {
        match receiver_spent.decode_log(topics, data)?.as_slice() {
            [id, secret] => Ok(TronSwapEvent::ReceiverSpent {
                id: word_of(id)?,
                secret: word_of(secret)?,
            }),
            _ => Err(AbiError::InvalidData.into()),
        }
    } else if *signature == sender_refunded.signature() {
        match sender_refunded.decode_log(topics, data)?.as_slice() {
            [id] => Ok(TronSwapEvent::SenderRefunded { id: word_of(id)? }),
            _ => Err(AbiError::InvalidData.into()),
        }
    } else {
        Err(TronSwapError::UnknownEvent)
    }
}
