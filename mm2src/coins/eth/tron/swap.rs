//! TRON version-1 atomic-swap dictated interop (Chapter 21 §21.11–§21.16).
//!
//! This module realises the *dictated* interop surface a Tron version-1 swap
//! counterparty must conform to: the Tron-deployed HTLC contract ABI (the
//! `bytes32`/SHA-256 secret-hash variant, R-SA1), the swap-identifier
//! derivation (R-SA2), the SHA-256 payment secret-hash (R-S2), the
//! payment/spend/refund call encodings and validations (R-L1–R-L5), the
//! contract-event normalisation (R-L6), the DEX taker-fee transfer
//! encode/validate (R-DF1/R-DF2), and the TRC20 approve-to-zero-first
//! allowance quirk (R-AP1).
//!
//! Everything here is pure (no node I/O) so it is fully unit-testable offline;
//! the async on-chain flows that drive it live in the swap-ops dispatch.

use ethabi::{Contract, Event, RawLog, Token};
use ethereum_types::{Address, H256, U256};
use prost::Message;
use sha2::{Digest, Sha256};

use super::address::TronAddress;
use super::proto::{ContractType, TransferContract, TriggerSmartContract};

/// The Tron-deployed version-1 HTLC contract ABI (R-SA1).
///
/// This is the published interop ABI of the Tron build of the project's
/// version-1 swap contract. It differs from the EVM build only in that the
/// secret-hash / payment-hash are `bytes32` (SHA-256) rather than `bytes20`
/// (RIPEMD-160 of SHA-256). Address arguments are the bare 20-byte EVM-style
/// payload (§21.5).
pub(crate) const TRON_SWAP_CONTRACT_ABI: &str = r#"[
  {"constant":false,"inputs":[
    {"name":"id","type":"bytes32"},
    {"name":"receiver","type":"address"},
    {"name":"secretHash","type":"bytes32"},
    {"name":"lockTime","type":"uint64"}],
   "name":"ethPayment","outputs":[],"payable":true,"stateMutability":"payable","type":"function"},
  {"constant":false,"inputs":[
    {"name":"id","type":"bytes32"},
    {"name":"amount","type":"uint256"},
    {"name":"tokenAddress","type":"address"},
    {"name":"receiver","type":"address"},
    {"name":"secretHash","type":"bytes32"},
    {"name":"lockTime","type":"uint64"}],
   "name":"erc20Payment","outputs":[],"payable":false,"stateMutability":"nonpayable","type":"function"},
  {"constant":false,"inputs":[
    {"name":"id","type":"bytes32"},
    {"name":"amount","type":"uint256"},
    {"name":"secret","type":"bytes32"},
    {"name":"tokenAddress","type":"address"},
    {"name":"sender","type":"address"}],
   "name":"receiverSpend","outputs":[],"payable":false,"stateMutability":"nonpayable","type":"function"},
  {"constant":false,"inputs":[
    {"name":"id","type":"bytes32"},
    {"name":"amount","type":"uint256"},
    {"name":"secretHash","type":"bytes32"},
    {"name":"tokenAddress","type":"address"},
    {"name":"receiver","type":"address"}],
   "name":"senderRefund","outputs":[],"payable":false,"stateMutability":"nonpayable","type":"function"},
  {"constant":true,"inputs":[{"name":"","type":"bytes32"}],
   "name":"payments","outputs":[
    {"name":"paymentHash","type":"bytes32"},
    {"name":"lockTime","type":"uint64"},
    {"name":"state","type":"uint8"}],
   "payable":false,"stateMutability":"view","type":"function"},
  {"anonymous":false,"inputs":[{"indexed":false,"name":"id","type":"bytes32"}],
   "name":"PaymentSent","type":"event"},
  {"anonymous":false,"inputs":[
    {"indexed":false,"name":"id","type":"bytes32"},
    {"indexed":false,"name":"secret","type":"bytes32"}],
   "name":"ReceiverSpent","type":"event"},
  {"anonymous":false,"inputs":[{"indexed":false,"name":"id","type":"bytes32"}],
   "name":"SenderRefunded","type":"event"}
]"#;

/// The standard TRC20 `approve(address,uint256)` ABI (R-AP1).
const TRC20_APPROVE_ABI: &str = r#"[
  {"constant":false,"inputs":[
    {"name":"spender","type":"address"},
    {"name":"value","type":"uint256"}],
   "name":"approve","outputs":[{"name":"","type":"bool"}],
   "payable":false,"stateMutability":"nonpayable","type":"function"},
  {"constant":false,"inputs":[
    {"name":"to","type":"address"},
    {"name":"value","type":"uint256"}],
   "name":"transfer","outputs":[{"name":"","type":"bool"}],
   "payable":false,"stateMutability":"nonpayable","type":"function"},
  {"constant":true,"inputs":[
    {"name":"owner","type":"address"},
    {"name":"spender","type":"address"}],
   "name":"allowance","outputs":[{"name":"","type":"uint256"}],
   "payable":false,"stateMutability":"view","type":"function"}
]"#;

lazy_static! {
    pub(crate) static ref TRON_SWAP_CONTRACT: Contract =
        Contract::load(TRON_SWAP_CONTRACT_ABI.as_bytes()).expect("hard-coded Tron swap-contract ABI must parse");
    static ref TRC20_TOKEN_CONTRACT: Contract =
        Contract::load(TRC20_APPROVE_ABI.as_bytes()).expect("hard-coded TRC20 ABI must parse");
}

/// Stored payment lifecycle state exposed via `payments(id)` (R-SA1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwapPaymentState {
    /// No payment recorded under this id.
    Uninitialised,
    /// Funds locked, awaiting spend or refund.
    PaymentSent,
    /// Receiver claimed the payment with the secret.
    ReceiverSpent,
    /// Sender refunded the payment after lock-time.
    SenderRefunded,
}

impl SwapPaymentState {
    /// Map the dictated small-unsigned state value to the lifecycle enum.
    pub fn from_u8(v: u8) -> Option<SwapPaymentState> {
        match v {
            0 => Some(SwapPaymentState::Uninitialised),
            1 => Some(SwapPaymentState::PaymentSent),
            2 => Some(SwapPaymentState::ReceiverSpent),
            3 => Some(SwapPaymentState::SenderRefunded),
            _ => None,
        }
    }
}

/// Typed errors for the Tron swap interop surface.
#[derive(Debug, derive_more::Display, PartialEq)]
pub enum TronSwapError {
    #[display(fmt = "ABI error: {}", _0)]
    Abi(String),
    #[display(fmt = "Protobuf decode error: {}", _0)]
    Proto(String),
    #[display(fmt = "Unexpected contract call: {}", _0)]
    UnexpectedCall(String),
    #[display(fmt = "Address error: {}", _0)]
    Address(String),
    #[display(fmt = "Payment validation failed: {}", _0)]
    Validation(String),
    #[display(fmt = "No event-indexer endpoint configured for spend/refund discovery")]
    NoEventEndpoint,
}

impl std::error::Error for TronSwapError {}

// ---------------------------------------------------------------------------
// Swap identifier (R-SA2) and secret-hash (R-S2)
// ---------------------------------------------------------------------------

/// Derive the 32-byte swap `id` used as the contract's payment-mapping key:
/// `SHA-256( little-endian uint32(lockTime) ‖ secretHash )` (R-SA2).
pub fn swap_id(lock_time: u32, secret_hash: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(lock_time.to_le_bytes());
    hasher.update(secret_hash);
    let out = hasher.finalize();
    let mut id = [0u8; 32];
    id.copy_from_slice(&out);
    id
}

/// The Tron payment secret-hash: `SHA-256(secret)`, a 32-byte value (R-S2).
pub fn sha256_secret_hash(secret: &[u8]) -> [u8; 32] {
    let out = Sha256::digest(secret);
    let mut h = [0u8; 32];
    h.copy_from_slice(&out);
    h
}

// ---------------------------------------------------------------------------
// Decoded call argument structs
// ---------------------------------------------------------------------------

/// Decoded `ethPayment` arguments (native-TRX HTLC lock).
#[derive(Clone, Debug, PartialEq)]
pub struct EthPaymentArgs {
    pub id: [u8; 32],
    pub receiver: Address,
    pub secret_hash: [u8; 32],
    pub lock_time: u64,
}

/// Decoded `erc20Payment` arguments (TRC20 HTLC lock).
#[derive(Clone, Debug, PartialEq)]
pub struct Erc20PaymentArgs {
    pub id: [u8; 32],
    pub amount: U256,
    pub token_addr: Address,
    pub receiver: Address,
    pub secret_hash: [u8; 32],
    pub lock_time: u64,
}

/// Decoded `receiverSpend` arguments (claim revealing the secret).
#[derive(Clone, Debug, PartialEq)]
pub struct ReceiverSpendArgs {
    pub id: [u8; 32],
    pub amount: U256,
    pub secret: [u8; 32],
    pub token_addr: Address,
    pub sender: Address,
}

// ---------------------------------------------------------------------------
// ABI encoders (R-L1/R-L3/R-L4)
// ---------------------------------------------------------------------------

fn swap_function(name: &str) -> Result<&'static ethabi::Function, TronSwapError> {
    TRON_SWAP_CONTRACT
        .function(name)
        .map_err(|e| TronSwapError::Abi(format!("missing {name}: {e}")))
}

fn fixed32(bytes: &[u8; 32]) -> Token { Token::FixedBytes(bytes.to_vec()) }

/// Encode an `ethPayment(id, receiver, secretHash, lockTime)` call (R-L1, native TRX).
pub fn encode_eth_payment(args: &EthPaymentArgs) -> Result<Vec<u8>, TronSwapError> {
    swap_function("ethPayment")?
        .encode_input(&[
            fixed32(&args.id),
            Token::Address(args.receiver),
            fixed32(&args.secret_hash),
            Token::Uint(U256::from(args.lock_time)),
        ])
        .map_err(|e| TronSwapError::Abi(e.to_string()))
}

/// Encode an `erc20Payment(id, amount, tokenAddress, receiver, secretHash, lockTime)` call (R-L1, TRC20).
pub fn encode_erc20_payment(args: &Erc20PaymentArgs) -> Result<Vec<u8>, TronSwapError> {
    swap_function("erc20Payment")?
        .encode_input(&[
            fixed32(&args.id),
            Token::Uint(args.amount),
            Token::Address(args.token_addr),
            Token::Address(args.receiver),
            fixed32(&args.secret_hash),
            Token::Uint(U256::from(args.lock_time)),
        ])
        .map_err(|e| TronSwapError::Abi(e.to_string()))
}

/// Encode a `receiverSpend(id, amount, secret, tokenAddress, sender)` call (R-L3).
pub fn encode_receiver_spend(args: &ReceiverSpendArgs) -> Result<Vec<u8>, TronSwapError> {
    swap_function("receiverSpend")?
        .encode_input(&[
            fixed32(&args.id),
            Token::Uint(args.amount),
            fixed32(&args.secret),
            Token::Address(args.token_addr),
            Token::Address(args.sender),
        ])
        .map_err(|e| TronSwapError::Abi(e.to_string()))
}

/// Encode a `senderRefund(id, amount, secretHash, tokenAddress, receiver)` call (R-L4).
pub fn encode_sender_refund(
    id: &[u8; 32],
    amount: U256,
    secret_hash: &[u8; 32],
    token_addr: Address,
    receiver: Address,
) -> Result<Vec<u8>, TronSwapError> {
    swap_function("senderRefund")?
        .encode_input(&[
            fixed32(id),
            Token::Uint(amount),
            fixed32(secret_hash),
            Token::Address(token_addr),
            Token::Address(receiver),
        ])
        .map_err(|e| TronSwapError::Abi(e.to_string()))
}

/// Encode a `payments(id)` read-only query's parameter portion (the 32-byte id),
/// for use as the `triggerconstantcontract` parameter (§21.9). The function
/// selector is supplied separately by the node call.
pub fn encode_payments_query_param(id: &[u8; 32]) -> Vec<u8> { id.to_vec() }

/// Encode a TRC20 `approve(spender, value)` call (R-AP1).
pub fn encode_trc20_approve(spender: Address, value: U256) -> Result<Vec<u8>, TronSwapError> {
    TRC20_TOKEN_CONTRACT
        .function("approve")
        .map_err(|e| TronSwapError::Abi(e.to_string()))?
        .encode_input(&[Token::Address(spender), Token::Uint(value)])
        .map_err(|e| TronSwapError::Abi(e.to_string()))
}

/// Encode a TRC20 `transfer(to, value)` call (R-DF1 TRC20 taker-fee).
pub fn encode_trc20_transfer(to: Address, value: U256) -> Result<Vec<u8>, TronSwapError> {
    TRC20_TOKEN_CONTRACT
        .function("transfer")
        .map_err(|e| TronSwapError::Abi(e.to_string()))?
        .encode_input(&[Token::Address(to), Token::Uint(value)])
        .map_err(|e| TronSwapError::Abi(e.to_string()))
}

// ---------------------------------------------------------------------------
// ABI decoders (R-L2/R-L5)
// ---------------------------------------------------------------------------

fn token_fixed32(t: &Token) -> Result<[u8; 32], TronSwapError> {
    match t {
        Token::FixedBytes(b) if b.len() == 32 => {
            let mut a = [0u8; 32];
            a.copy_from_slice(b);
            Ok(a)
        },
        other => Err(TronSwapError::Abi(format!("expected bytes32, got {other:?}"))),
    }
}

fn token_address(t: &Token) -> Result<Address, TronSwapError> {
    match t {
        Token::Address(a) => Ok(*a),
        other => Err(TronSwapError::Abi(format!("expected address, got {other:?}"))),
    }
}

fn token_uint(t: &Token) -> Result<U256, TronSwapError> {
    match t {
        Token::Uint(u) => Ok(*u),
        other => Err(TronSwapError::Abi(format!("expected uint, got {other:?}"))),
    }
}

/// Decode an `ethPayment` call (selector + args) into [`EthPaymentArgs`] (R-L2).
pub fn decode_eth_payment_call(data: &[u8]) -> Result<EthPaymentArgs, TronSwapError> {
    let tokens = decode_call("ethPayment", data)?;
    Ok(EthPaymentArgs {
        id: token_fixed32(&tokens[0])?,
        receiver: token_address(&tokens[1])?,
        secret_hash: token_fixed32(&tokens[2])?,
        lock_time: token_uint(&tokens[3])?.as_u64(),
    })
}

/// Decode an `erc20Payment` call into [`Erc20PaymentArgs`] (R-L2).
pub fn decode_erc20_payment_call(data: &[u8]) -> Result<Erc20PaymentArgs, TronSwapError> {
    let tokens = decode_call("erc20Payment", data)?;
    Ok(Erc20PaymentArgs {
        id: token_fixed32(&tokens[0])?,
        amount: token_uint(&tokens[1])?,
        token_addr: token_address(&tokens[2])?,
        receiver: token_address(&tokens[3])?,
        secret_hash: token_fixed32(&tokens[4])?,
        lock_time: token_uint(&tokens[5])?.as_u64(),
    })
}

/// Decode a `receiverSpend` call into [`ReceiverSpendArgs`] (R-L5).
pub fn decode_receiver_spend_call(data: &[u8]) -> Result<ReceiverSpendArgs, TronSwapError> {
    let tokens = decode_call("receiverSpend", data)?;
    Ok(ReceiverSpendArgs {
        id: token_fixed32(&tokens[0])?,
        amount: token_uint(&tokens[1])?,
        secret: token_fixed32(&tokens[2])?,
        token_addr: token_address(&tokens[3])?,
        sender: token_address(&tokens[4])?,
    })
}

/// Extract the revealed secret from a `receiverSpend` call's data (R-L5).
pub fn extract_secret_from_spend(data: &[u8]) -> Result<[u8; 32], TronSwapError> {
    Ok(decode_receiver_spend_call(data)?.secret)
}

/// Decode `payments(id)` output `(bytes32 paymentHash, uint64 lockTime, uint8 state)` (R-SA1).
pub fn decode_payments_output(output: &[u8]) -> Result<([u8; 32], u64, SwapPaymentState), TronSwapError> {
    let func = swap_function("payments")?;
    let tokens = func
        .decode_output(output)
        .map_err(|e| TronSwapError::Abi(e.to_string()))?;
    if tokens.len() != 3 {
        return Err(TronSwapError::Abi(format!("payments returned {} tokens", tokens.len())));
    }
    let payment_hash = token_fixed32(&tokens[0])?;
    let lock_time = token_uint(&tokens[1])?.as_u64();
    let state_u = token_uint(&tokens[2])?.low_u32() as u8;
    let state = SwapPaymentState::from_u8(state_u)
        .ok_or_else(|| TronSwapError::Abi(format!("unknown payment state {state_u}")))?;
    Ok((payment_hash, lock_time, state))
}

fn decode_call(name: &str, data: &[u8]) -> Result<Vec<Token>, TronSwapError> {
    if data.len() < 4 {
        return Err(TronSwapError::Abi(format!("{name} call data shorter than selector")));
    }
    let func = swap_function(name)?;
    if data[..4] != func.short_signature() {
        return Err(TronSwapError::UnexpectedCall(format!(
            "selector 0x{} is not {name}",
            hex::encode(&data[..4])
        )));
    }
    func.decode_input(data).map_err(|e| TronSwapError::Abi(e.to_string()))
}

// ---------------------------------------------------------------------------
// Protobuf decode of a broadcast Tron transaction (R-L2/R-L5/R-DF2)
// ---------------------------------------------------------------------------

/// The single contract carried by a broadcast Tron swap/fee transaction.
#[derive(Clone, Debug, PartialEq)]
pub enum DecodedContract {
    Transfer(TransferContract),
    Trigger(TriggerSmartContract),
}

/// Decode a broadcast Tron transaction (protobuf `Transaction` bytes) and
/// extract its single contract entry (R-L2). Tron swap/fee transactions carry
/// exactly one contract.
pub fn decode_single_contract(tx_bytes: &[u8]) -> Result<DecodedContract, TronSwapError> {
    let tx = super::proto::Transaction::decode(tx_bytes).map_err(|e| TronSwapError::Proto(e.to_string()))?;
    let raw = tx
        .raw_data
        .ok_or_else(|| TronSwapError::Proto("transaction has no raw_data".to_owned()))?;
    let contract = raw
        .contract
        .into_iter()
        .next()
        .ok_or_else(|| TronSwapError::Proto("transaction has no contract entry".to_owned()))?;
    let any = contract
        .parameter
        .ok_or_else(|| TronSwapError::Proto("contract has no parameter".to_owned()))?;
    match ContractType::try_from(contract.r#type) {
        Ok(ContractType::TransferContract) => {
            let inner =
                TransferContract::decode(any.value.as_slice()).map_err(|e| TronSwapError::Proto(e.to_string()))?;
            Ok(DecodedContract::Transfer(inner))
        },
        Ok(ContractType::TriggerSmartContract) => {
            let inner =
                TriggerSmartContract::decode(any.value.as_slice()).map_err(|e| TronSwapError::Proto(e.to_string()))?;
            Ok(DecodedContract::Trigger(inner))
        },
        other => Err(TronSwapError::UnexpectedCall(format!("contract type {other:?}"))),
    }
}

/// Parse a 21-byte (0x41-prefixed) Tron protobuf address payload to its EVM form.
fn evm_from_proto_addr(bytes: &[u8]) -> Result<Address, TronSwapError> {
    TronAddress::from_bytes(bytes)
        .map(|a| a.to_evm_address())
        .map_err(|e| TronSwapError::Address(e.to_string()))
}

// ---------------------------------------------------------------------------
// Payment validation (R-L2)
// ---------------------------------------------------------------------------

/// Negotiated terms a counterparty's payment must satisfy.
pub struct ExpectedPayment {
    /// The Tron swap-contract address (EVM-20 form).
    pub swap_contract: Address,
    /// The funder (counterparty) address (EVM-20 form).
    pub funder: Address,
    /// The receiver (our) address (EVM-20 form).
    pub receiver: Address,
    /// 32-byte SHA-256 secret-hash.
    pub secret_hash: [u8; 32],
    /// Lock-time (unix seconds).
    pub lock_time: u64,
    /// Locked amount, in SUN for TRX or token base units for TRC20.
    pub amount: U256,
    /// For TRC20: the token contract address (EVM-20 form); `None` for native TRX.
    pub token_addr: Option<Address>,
}

/// Validate a counterparty's broadcast payment transaction against the
/// negotiated terms (R-L2). Decodes the protobuf form, ABI-decodes the
/// `ethPayment`/`erc20Payment` call, and cross-checks funder, recipient,
/// token, amount, secret-hash, lock-time, and the derived swap id.
pub fn validate_payment_tx(tx_bytes: &[u8], expected: &ExpectedPayment) -> Result<(), TronSwapError> {
    let trigger = match decode_single_contract(tx_bytes)? {
        DecodedContract::Trigger(t) => t,
        DecodedContract::Transfer(_) => {
            return Err(TronSwapError::Validation(
                "payment must be a TriggerSmartContract, found a TransferContract".to_owned(),
            ))
        },
    };

    let called_contract = evm_from_proto_addr(&trigger.contract_address)?;
    if called_contract != expected.swap_contract {
        return Err(TronSwapError::Validation(format!(
            "payment called {called_contract:?}, expected swap contract {:?}",
            expected.swap_contract
        )));
    }
    let funder = evm_from_proto_addr(&trigger.owner_address)?;
    if funder != expected.funder {
        return Err(TronSwapError::Validation(format!(
            "payment funder {funder:?}, expected {:?}",
            expected.funder
        )));
    }

    let expected_id = swap_id(expected.lock_time as u32, &expected.secret_hash);

    match expected.token_addr {
        None => {
            let args = decode_eth_payment_call(&trigger.data)?;
            check_eq("swap id", &args.id, &expected_id)?;
            check_addr("receiver", args.receiver, expected.receiver)?;
            check_eq("secret hash", &args.secret_hash, &expected.secret_hash)?;
            check_lock_time(args.lock_time, expected.lock_time)?;
            let call_value = U256::from(trigger.call_value.max(0) as u64);
            if call_value != expected.amount {
                return Err(TronSwapError::Validation(format!(
                    "native payment call value {call_value} != expected {}",
                    expected.amount
                )));
            }
        },
        Some(token) => {
            let args = decode_erc20_payment_call(&trigger.data)?;
            check_eq("swap id", &args.id, &expected_id)?;
            check_addr("receiver", args.receiver, expected.receiver)?;
            check_addr("token", args.token_addr, token)?;
            check_eq("secret hash", &args.secret_hash, &expected.secret_hash)?;
            check_lock_time(args.lock_time, expected.lock_time)?;
            if args.amount != expected.amount {
                return Err(TronSwapError::Validation(format!(
                    "trc20 payment amount {} != expected {}",
                    args.amount, expected.amount
                )));
            }
        },
    }
    Ok(())
}

fn check_eq(label: &str, got: &[u8; 32], want: &[u8; 32]) -> Result<(), TronSwapError> {
    if got != want {
        return Err(TronSwapError::Validation(format!(
            "{label} mismatch: got 0x{}, expected 0x{}",
            hex::encode(got),
            hex::encode(want)
        )));
    }
    Ok(())
}

fn check_addr(label: &str, got: Address, want: Address) -> Result<(), TronSwapError> {
    if got != want {
        return Err(TronSwapError::Validation(format!(
            "{label} mismatch: got {got:?}, expected {want:?}"
        )));
    }
    Ok(())
}

fn check_lock_time(got: u64, want: u64) -> Result<(), TronSwapError> {
    if got != want {
        return Err(TronSwapError::Validation(format!(
            "lock time mismatch: got {got}, expected {want}"
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// DEX taker-fee validation (R-DF2)
// ---------------------------------------------------------------------------

/// Validate a counterparty's broadcast taker-fee transaction (R-DF2): confirm
/// it is the expected transfer (native or TRC20) to the dex-fee recipient with
/// at least the expected amount.
///
/// * `token_addr` — `Some(token)` for a TRC20 fee, `None` for a native TRX fee.
pub fn validate_dex_fee_tx(
    tx_bytes: &[u8],
    expected_recipient: Address,
    expected_amount: U256,
    token_addr: Option<Address>,
) -> Result<(), TronSwapError> {
    match (decode_single_contract(tx_bytes)?, token_addr) {
        (DecodedContract::Transfer(t), None) => {
            let to = evm_from_proto_addr(&t.to_address)?;
            check_addr("fee recipient", to, expected_recipient)?;
            let value = U256::from(t.amount.max(0) as u64);
            if value < expected_amount {
                return Err(TronSwapError::Validation(format!(
                    "fee value {value} < expected {expected_amount}"
                )));
            }
            Ok(())
        },
        (DecodedContract::Trigger(t), Some(token)) => {
            let called = evm_from_proto_addr(&t.contract_address)?;
            check_addr("fee token contract", called, token)?;
            let func = TRC20_TOKEN_CONTRACT
                .function("transfer")
                .map_err(|e| TronSwapError::Abi(e.to_string()))?;
            if t.data.len() < 4 || t.data[..4] != func.short_signature() {
                return Err(TronSwapError::Validation("fee tx is not a TRC20 transfer".to_owned()));
            }
            let tokens = func
                .decode_input(&t.data)
                .map_err(|e| TronSwapError::Abi(e.to_string()))?;
            let to = token_address(&tokens[0])?;
            check_addr("fee recipient", to, expected_recipient)?;
            let value = token_uint(&tokens[1])?;
            if value < expected_amount {
                return Err(TronSwapError::Validation(format!(
                    "fee value {value} < expected {expected_amount}"
                )));
            }
            Ok(())
        },
        (DecodedContract::Transfer(_), Some(_)) => Err(TronSwapError::Validation(
            "expected a TRC20 transfer fee, found a native TransferContract".to_owned(),
        )),
        (DecodedContract::Trigger(_), None) => Err(TronSwapError::Validation(
            "expected a native fee, found a TriggerSmartContract".to_owned(),
        )),
    }
}

// ---------------------------------------------------------------------------
// TRC20 allowance management (R-AP1)
// ---------------------------------------------------------------------------

/// Whether a USDT-style "approve-to-zero-first" reset is required before
/// setting the swap contract's allowance (R-AP1).
///
/// USDT-style TRC20 tokens reject a direct non-zero→non-zero allowance change.
/// If the current allowance is already non-zero but insufficient, the funder
/// must first set it to zero, then set the required value.
pub fn needs_approve_to_zero(current_allowance: U256, required_allowance: U256) -> bool {
    !current_allowance.is_zero() && current_allowance < required_allowance
}

/// Whether the current allowance is already sufficient (no approval needed).
pub fn allowance_sufficient(current_allowance: U256, required_allowance: U256) -> bool {
    current_allowance >= required_allowance
}

// ---------------------------------------------------------------------------
// Contract-event normalisation (R-L6)
// ---------------------------------------------------------------------------

/// A normalised swap-contract event, from either the indexed contract-event
/// source or a transaction-receipt log (R-L6).
#[derive(Clone, Debug, PartialEq)]
pub enum SwapEvent {
    PaymentSent { id: [u8; 32] },
    ReceiverSpent { id: [u8; 32], secret: [u8; 32] },
    SenderRefunded { id: [u8; 32] },
}

fn swap_event(name: &str) -> Result<&'static Event, TronSwapError> {
    TRON_SWAP_CONTRACT
        .event(name)
        .map_err(|e| TronSwapError::Abi(format!("missing event {name}: {e}")))
}

/// The `topic0` signature hash of the `ReceiverSpent` event.
pub fn receiver_spent_topic() -> Result<H256, TronSwapError> { Ok(swap_event("ReceiverSpent")?.signature()) }

/// The `topic0` signature hash of the `PaymentSent` event.
pub fn payment_sent_topic() -> Result<H256, TronSwapError> { Ok(swap_event("PaymentSent")?.signature()) }

/// The `topic0` signature hash of the `SenderRefunded` event.
pub fn sender_refunded_topic() -> Result<H256, TronSwapError> { Ok(swap_event("SenderRefunded")?.signature()) }

/// Decode a swap-contract event from a transaction-receipt log (R-L6): the
/// `(topics, data)` pair as Tron exposes it in `gettransactioninfobyid` logs.
pub fn decode_event_from_log(topics: &[H256], data: &[u8]) -> Result<SwapEvent, TronSwapError> {
    let topic0 = *topics
        .first()
        .ok_or_else(|| TronSwapError::Abi("log has no topics".to_owned()))?;
    let raw = RawLog {
        topics: topics.to_vec(),
        data: data.to_vec(),
    };
    if topic0 == payment_sent_topic()? {
        let log = swap_event("PaymentSent")?
            .parse_log(raw)
            .map_err(|e| TronSwapError::Abi(e.to_string()))?;
        Ok(SwapEvent::PaymentSent {
            id: log_fixed32(&log, "id")?,
        })
    } else if topic0 == receiver_spent_topic()? {
        let log = swap_event("ReceiverSpent")?
            .parse_log(raw)
            .map_err(|e| TronSwapError::Abi(e.to_string()))?;
        Ok(SwapEvent::ReceiverSpent {
            id: log_fixed32(&log, "id")?,
            secret: log_fixed32(&log, "secret")?,
        })
    } else if topic0 == sender_refunded_topic()? {
        let log = swap_event("SenderRefunded")?
            .parse_log(raw)
            .map_err(|e| TronSwapError::Abi(e.to_string()))?;
        Ok(SwapEvent::SenderRefunded {
            id: log_fixed32(&log, "id")?,
        })
    } else {
        Err(TronSwapError::UnexpectedCall(format!(
            "unknown event topic 0x{}",
            hex::encode(topic0.0)
        )))
    }
}

fn log_fixed32(log: &ethabi::Log, name: &str) -> Result<[u8; 32], TronSwapError> {
    let param = log
        .params
        .iter()
        .find(|p| p.name == name)
        .ok_or_else(|| TronSwapError::Abi(format!("event missing param {name}")))?;
    token_fixed32(&param.value)
}

/// Normalise a swap-contract event from the indexed contract-event source
/// (R-L6): the event indexer returns each event's parameters as named hex
/// strings. `event_name` selects the event and `result` carries its decoded
/// fields (e.g. `{"id": "0x..", "secret": "0x.."}`).
pub fn decode_event_from_indexer(
    event_name: &str,
    result: &std::collections::HashMap<String, String>,
) -> Result<SwapEvent, TronSwapError> {
    let get32 = |key: &str| -> Result<[u8; 32], TronSwapError> {
        let raw = result
            .get(key)
            .ok_or_else(|| TronSwapError::Abi(format!("indexer event missing field {key}")))?;
        let hexs = raw.strip_prefix("0x").unwrap_or(raw);
        let bytes = hex::decode(hexs).map_err(|e| TronSwapError::Abi(e.to_string()))?;
        if bytes.len() != 32 {
            return Err(TronSwapError::Abi(format!(
                "field {key} is {} bytes, expected 32",
                bytes.len()
            )));
        }
        let mut a = [0u8; 32];
        a.copy_from_slice(&bytes);
        Ok(a)
    };
    match event_name {
        "PaymentSent" => Ok(SwapEvent::PaymentSent { id: get32("id")? }),
        "ReceiverSpent" => Ok(SwapEvent::ReceiverSpent {
            id: get32("id")?,
            secret: get32("secret")?,
        }),
        "SenderRefunded" => Ok(SwapEvent::SenderRefunded { id: get32("id")? }),
        other => Err(TronSwapError::UnexpectedCall(format!("unknown indexer event {other}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn addr(byte: u8) -> Address {
        let mut a = [0u8; 20];
        a[19] = byte;
        Address::from(a)
    }

    fn sample_id() -> [u8; 32] { [0x11; 32] }
    fn sample_secret() -> [u8; 32] { [0x22; 32] }
    fn sample_secret_hash() -> [u8; 32] { sha256_secret_hash(&sample_secret()) }

    // ---- R-S2 / R-SA2 ----

    #[test]
    fn sha256_secret_hash_is_32_bytes() {
        let h = sha256_secret_hash(b"some secret");
        assert_eq!(h.len(), 32);
        // Matches a plain SHA-256 digest.
        let expect = Sha256::digest(b"some secret");
        assert_eq!(h.to_vec(), expect.to_vec());
    }

    #[test]
    fn swap_id_matches_dictated_derivation() {
        let lock_time: u32 = 1_700_000_000;
        let secret_hash = sample_secret_hash();
        // SHA-256( LE u32(lockTime) || secretHash ).
        let mut hasher = Sha256::new();
        hasher.update(lock_time.to_le_bytes());
        hasher.update(secret_hash);
        let expect = hasher.finalize();
        assert_eq!(swap_id(lock_time, &secret_hash).to_vec(), expect.to_vec());
    }

    // ---- R-SA1 ABI round-trips (T1) ----

    #[test]
    fn eth_payment_round_trip() {
        let args = EthPaymentArgs {
            id: sample_id(),
            receiver: addr(7),
            secret_hash: sample_secret_hash(),
            lock_time: 1_700_000_000,
        };
        let data = encode_eth_payment(&args).unwrap();
        assert_eq!(decode_eth_payment_call(&data).unwrap(), args);
    }

    #[test]
    fn erc20_payment_round_trip() {
        let args = Erc20PaymentArgs {
            id: sample_id(),
            amount: U256::from(123_456u64),
            token_addr: addr(9),
            receiver: addr(7),
            secret_hash: sample_secret_hash(),
            lock_time: 1_700_000_000,
        };
        let data = encode_erc20_payment(&args).unwrap();
        assert_eq!(decode_erc20_payment_call(&data).unwrap(), args);
    }

    #[test]
    fn receiver_spend_round_trip_and_secret_extraction() {
        let args = ReceiverSpendArgs {
            id: sample_id(),
            amount: U256::from(999u64),
            secret: sample_secret(),
            token_addr: Address::zero(),
            sender: addr(5),
        };
        let data = encode_receiver_spend(&args).unwrap();
        assert_eq!(decode_receiver_spend_call(&data).unwrap(), args);
        assert_eq!(extract_secret_from_spend(&data).unwrap(), sample_secret());
    }

    #[test]
    fn sender_refund_encodes_with_distinct_selector() {
        let data =
            encode_sender_refund(&sample_id(), U256::from(1u64), &sample_secret_hash(), addr(1), addr(2)).unwrap();
        // Selector must differ from the other swap calls.
        let eth = encode_eth_payment(&EthPaymentArgs {
            id: sample_id(),
            receiver: addr(1),
            secret_hash: sample_secret_hash(),
            lock_time: 1,
        })
        .unwrap();
        assert_ne!(data[..4], eth[..4]);
    }

    #[test]
    fn decode_rejects_wrong_selector() {
        let spend = encode_receiver_spend(&ReceiverSpendArgs {
            id: sample_id(),
            amount: U256::from(1u64),
            secret: sample_secret(),
            token_addr: Address::zero(),
            sender: addr(1),
        })
        .unwrap();
        // Decoding a receiverSpend as an ethPayment must fail on selector.
        assert!(matches!(
            decode_eth_payment_call(&spend),
            Err(TronSwapError::UnexpectedCall(_))
        ));
    }

    #[test]
    fn payments_output_round_trip() {
        let payment_hash = [0xAB; 32];
        // ethabi 6.1.0 has no `encode_output`; encode the return tuple directly.
        let encoded = ethabi::encode(&[
            Token::FixedBytes(payment_hash.to_vec()),
            Token::Uint(U256::from(1_700_000_000u64)),
            Token::Uint(U256::from(1u64)),
        ]);
        let (ph, lt, state) = decode_payments_output(&encoded).unwrap();
        assert_eq!(ph, payment_hash);
        assert_eq!(lt, 1_700_000_000);
        assert_eq!(state, SwapPaymentState::PaymentSent);
    }

    // ---- R-L2 protobuf decode + validation (T3) ----

    use super::super::proto::{Transaction, TransactionRaw};
    use super::super::tx_builder::tapos_from_block;
    use crate::eth::tron::proto::{ContractType, TransactionContract, TriggerSmartContract,
                                  TRIGGER_SMART_CONTRACT_TYPE_URL};

    fn tron_addr(byte: u8) -> TronAddress {
        let mut a = [0u8; 20];
        a[19] = byte;
        TronAddress::from_evm_address(Address::from(a))
    }

    fn wrap_trigger_tx(owner: &TronAddress, contract: &TronAddress, call_value: i64, data: Vec<u8>) -> Vec<u8> {
        let trigger = TriggerSmartContract {
            owner_address: owner.to_bytes().to_vec(),
            contract_address: contract.to_bytes().to_vec(),
            call_value,
            data,
        };
        let any = prost_types::Any {
            type_url: TRIGGER_SMART_CONTRACT_TYPE_URL.to_string(),
            value: trigger.encode_to_vec(),
        };
        let tapos = tapos_from_block(1, &[0u8; 32]);
        let raw = TransactionRaw {
            ref_block_bytes: tapos.ref_block_bytes,
            ref_block_hash: tapos.ref_block_hash,
            expiration: 0,
            contract: vec![TransactionContract {
                r#type: ContractType::TriggerSmartContract as i32,
                parameter: Some(any),
            }],
            timestamp: 0,
            fee_limit: 0,
        };
        Transaction {
            raw_data: Some(raw),
            signature: vec![vec![0u8; 65]],
        }
        .encode_to_vec()
    }

    fn expected_native(
        swap: &TronAddress,
        funder: &TronAddress,
        receiver: &TronAddress,
        lock_time: u64,
    ) -> ExpectedPayment {
        ExpectedPayment {
            swap_contract: swap.to_evm_address(),
            funder: funder.to_evm_address(),
            receiver: receiver.to_evm_address(),
            secret_hash: sample_secret_hash(),
            lock_time,
            amount: U256::from(5_000_000u64),
            token_addr: None,
        }
    }

    #[test]
    fn validate_native_payment_positive() {
        let swap = tron_addr(0x10);
        let funder = tron_addr(0x20);
        let receiver = tron_addr(0x30);
        let lock_time = 1_700_000_000u64;
        let id = swap_id(lock_time as u32, &sample_secret_hash());
        let data = encode_eth_payment(&EthPaymentArgs {
            id,
            receiver: receiver.to_evm_address(),
            secret_hash: sample_secret_hash(),
            lock_time,
        })
        .unwrap();
        let tx = wrap_trigger_tx(&funder, &swap, 5_000_000, data);
        let expected = expected_native(&swap, &funder, &receiver, lock_time);
        validate_payment_tx(&tx, &expected).unwrap();
    }

    #[test]
    fn validate_native_payment_wrong_amount() {
        let swap = tron_addr(0x10);
        let funder = tron_addr(0x20);
        let receiver = tron_addr(0x30);
        let lock_time = 1_700_000_000u64;
        let id = swap_id(lock_time as u32, &sample_secret_hash());
        let data = encode_eth_payment(&EthPaymentArgs {
            id,
            receiver: receiver.to_evm_address(),
            secret_hash: sample_secret_hash(),
            lock_time,
        })
        .unwrap();
        // Broadcast call value differs from the negotiated amount.
        let tx = wrap_trigger_tx(&funder, &swap, 4_000_000, data);
        let expected = expected_native(&swap, &funder, &receiver, lock_time);
        assert!(matches!(
            validate_payment_tx(&tx, &expected),
            Err(TronSwapError::Validation(_))
        ));
    }

    #[test]
    fn validate_native_payment_wrong_recipient() {
        let swap = tron_addr(0x10);
        let funder = tron_addr(0x20);
        let receiver = tron_addr(0x30);
        let lock_time = 1_700_000_000u64;
        let id = swap_id(lock_time as u32, &sample_secret_hash());
        // Payment built to a different receiver than expected.
        let data = encode_eth_payment(&EthPaymentArgs {
            id,
            receiver: tron_addr(0x99).to_evm_address(),
            secret_hash: sample_secret_hash(),
            lock_time,
        })
        .unwrap();
        let tx = wrap_trigger_tx(&funder, &swap, 5_000_000, data);
        let expected = expected_native(&swap, &funder, &receiver, lock_time);
        assert!(matches!(
            validate_payment_tx(&tx, &expected),
            Err(TronSwapError::Validation(_))
        ));
    }

    #[test]
    fn validate_native_payment_wrong_secret_hash() {
        let swap = tron_addr(0x10);
        let funder = tron_addr(0x20);
        let receiver = tron_addr(0x30);
        let lock_time = 1_700_000_000u64;
        let bad_hash = sha256_secret_hash(b"different secret");
        let id = swap_id(lock_time as u32, &bad_hash);
        let data = encode_eth_payment(&EthPaymentArgs {
            id,
            receiver: receiver.to_evm_address(),
            secret_hash: bad_hash,
            lock_time,
        })
        .unwrap();
        let tx = wrap_trigger_tx(&funder, &swap, 5_000_000, data);
        let expected = expected_native(&swap, &funder, &receiver, lock_time);
        assert!(matches!(
            validate_payment_tx(&tx, &expected),
            Err(TronSwapError::Validation(_))
        ));
    }

    #[test]
    fn validate_native_payment_wrong_lock_time() {
        let swap = tron_addr(0x10);
        let funder = tron_addr(0x20);
        let receiver = tron_addr(0x30);
        let lock_time = 1_700_000_000u64;
        let other = 1_700_009_999u64;
        let id = swap_id(other as u32, &sample_secret_hash());
        let data = encode_eth_payment(&EthPaymentArgs {
            id,
            receiver: receiver.to_evm_address(),
            secret_hash: sample_secret_hash(),
            lock_time: other,
        })
        .unwrap();
        let tx = wrap_trigger_tx(&funder, &swap, 5_000_000, data);
        let expected = expected_native(&swap, &funder, &receiver, lock_time);
        assert!(matches!(
            validate_payment_tx(&tx, &expected),
            Err(TronSwapError::Validation(_))
        ));
    }

    #[test]
    fn validate_trc20_payment_positive_and_wrong_token() {
        let swap = tron_addr(0x10);
        let funder = tron_addr(0x20);
        let receiver = tron_addr(0x30);
        let token = tron_addr(0x40);
        let lock_time = 1_700_000_000u64;
        let amount = U256::from(7_000_000u64);
        let id = swap_id(lock_time as u32, &sample_secret_hash());
        let data = encode_erc20_payment(&Erc20PaymentArgs {
            id,
            amount,
            token_addr: token.to_evm_address(),
            receiver: receiver.to_evm_address(),
            secret_hash: sample_secret_hash(),
            lock_time,
        })
        .unwrap();
        let tx = wrap_trigger_tx(&funder, &swap, 0, data);
        let mut expected = ExpectedPayment {
            swap_contract: swap.to_evm_address(),
            funder: funder.to_evm_address(),
            receiver: receiver.to_evm_address(),
            secret_hash: sample_secret_hash(),
            lock_time,
            amount,
            token_addr: Some(token.to_evm_address()),
        };
        validate_payment_tx(&tx, &expected).unwrap();
        // Wrong expected token rejects.
        expected.token_addr = Some(tron_addr(0x41).to_evm_address());
        assert!(matches!(
            validate_payment_tx(&tx, &expected),
            Err(TronSwapError::Validation(_))
        ));
    }

    // ---- R-DF2 dex-fee validation ----

    #[test]
    fn validate_native_dex_fee() {
        let recipient = tron_addr(0x55);
        let from = tron_addr(0x66);
        let transfer = TransferContract {
            owner_address: from.to_bytes().to_vec(),
            to_address: recipient.to_bytes().to_vec(),
            amount: 1_000_000,
        };
        let any = prost_types::Any {
            type_url: super::super::proto::TRANSFER_CONTRACT_TYPE_URL.to_string(),
            value: transfer.encode_to_vec(),
        };
        let raw = TransactionRaw {
            ref_block_bytes: vec![0, 0],
            ref_block_hash: vec![0; 8],
            expiration: 0,
            contract: vec![TransactionContract {
                r#type: ContractType::TransferContract as i32,
                parameter: Some(any),
            }],
            timestamp: 0,
            fee_limit: 0,
        };
        let tx = Transaction {
            raw_data: Some(raw),
            signature: vec![vec![0u8; 65]],
        }
        .encode_to_vec();
        validate_dex_fee_tx(&tx, recipient.to_evm_address(), U256::from(1_000_000u64), None).unwrap();
        // Under-payment rejected.
        assert!(validate_dex_fee_tx(&tx, recipient.to_evm_address(), U256::from(2_000_000u64), None).is_err());
        // Wrong recipient rejected.
        assert!(validate_dex_fee_tx(&tx, tron_addr(0x77).to_evm_address(), U256::from(1_000_000u64), None).is_err());
    }

    #[test]
    fn validate_trc20_dex_fee() {
        let recipient = tron_addr(0x55);
        let from = tron_addr(0x66);
        let token = tron_addr(0x44);
        let data = encode_trc20_transfer(recipient.to_evm_address(), U256::from(3_000_000u64)).unwrap();
        let tx = wrap_trigger_tx(&from, &token, 0, data);
        validate_dex_fee_tx(
            &tx,
            recipient.to_evm_address(),
            U256::from(3_000_000u64),
            Some(token.to_evm_address()),
        )
        .unwrap();
        // Native-expected on a TRC20 fee tx is rejected.
        assert!(validate_dex_fee_tx(&tx, recipient.to_evm_address(), U256::from(3_000_000u64), None).is_err());
    }

    // ---- R-AP1 approve-to-zero-first ----

    #[test]
    fn approve_to_zero_first_decision() {
        // Fresh (zero) allowance: no zero-first reset needed.
        assert!(!needs_approve_to_zero(U256::zero(), U256::from(100u64)));
        // Existing non-zero but insufficient allowance: must reset to zero first.
        assert!(needs_approve_to_zero(U256::from(10u64), U256::from(100u64)));
        // Already sufficient: no approval at all.
        assert!(allowance_sufficient(U256::from(100u64), U256::from(100u64)));
        assert!(!needs_approve_to_zero(U256::from(100u64), U256::from(100u64)));
    }

    #[test]
    fn approve_encodes_zero_then_value() {
        let spender = addr(3);
        let zero = encode_trc20_approve(spender, U256::zero()).unwrap();
        let value = encode_trc20_approve(spender, U256::from(100u64)).unwrap();
        // Same selector, different trailing amount word.
        assert_eq!(zero[..4], value[..4]);
        assert_ne!(zero, value);
    }

    // ---- R-L6 event normalisation (T2) ----

    #[test]
    fn event_log_and_indexer_agree_receiver_spent() {
        let id = sample_id();
        let secret = sample_secret();
        let event = TRON_SWAP_CONTRACT.event("ReceiverSpent").unwrap();
        let data = ethabi::encode(&[Token::FixedBytes(id.to_vec()), Token::FixedBytes(secret.to_vec())]);
        let from_log = decode_event_from_log(&[event.signature()], &data).unwrap();
        assert_eq!(from_log, SwapEvent::ReceiverSpent { id, secret });

        let mut map = HashMap::new();
        map.insert("id".to_string(), format!("0x{}", hex::encode(id)));
        map.insert("secret".to_string(), format!("0x{}", hex::encode(secret)));
        let from_indexer = decode_event_from_indexer("ReceiverSpent", &map).unwrap();
        assert_eq!(from_log, from_indexer);
    }

    #[test]
    fn event_log_payment_sent_and_sender_refunded() {
        let id = sample_id();
        let data = ethabi::encode(&[Token::FixedBytes(id.to_vec())]);

        let sent = decode_event_from_log(&[payment_sent_topic().unwrap()], &data).unwrap();
        assert_eq!(sent, SwapEvent::PaymentSent { id });

        let refunded = decode_event_from_log(&[sender_refunded_topic().unwrap()], &data).unwrap();
        assert_eq!(refunded, SwapEvent::SenderRefunded { id });
    }

    #[test]
    fn event_topics_are_distinct() {
        let a = payment_sent_topic().unwrap();
        let b = receiver_spent_topic().unwrap();
        let c = sender_refunded_topic().unwrap();
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_ne!(a, c);
    }

    #[test]
    fn unknown_event_topic_rejected() {
        let bogus = H256::from([0x99; 32]);
        let data = ethabi::encode(&[Token::FixedBytes(sample_id().to_vec())]);
        assert!(matches!(
            decode_event_from_log(&[bogus], &data),
            Err(TronSwapError::UnexpectedCall(_))
        ));
    }
}
