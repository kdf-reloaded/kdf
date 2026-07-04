//! NFT swap V2 calldata layer (P10.3.7 — Slice 1).
//!
//! Pure-function builders and validators for the EtomicSwapMakerV2-NFT
//! contract entrypoints that lock an ERC-721 or ERC-1155 NFT into a
//! hash-time-locked contract (HTLC). Modelled on the fungible-token
//! maker swap V2 layer in [`crate::eth::eth_swap_v2::eth_maker_swap_v2`]
//! but parameterised by an NFT's `(token_address, token_id)` pair (and
//! `amount` for ERC-1155 partial fills).
//!
//! The NFT swap follows the same maker/taker/secret HTLC topology as the
//! fungible-token V2 swaps:
//!
//! 1. **`erc721MakerPayment` / `erc1155MakerPayment`** — Maker locks the
//!    NFT into the contract committing to `(takerSecretHash, makerSecretHash,
//!    paymentLockTime)`.
//! 2. **`spendErc{721,1155}MakerPayment`** — Taker reveals
//!    `makerSecret` (whose hash is `makerSecretHash`) to claim the NFT.
//! 3. **`refundErc{721,1155}MakerPaymentTimelock`** — After
//!    `paymentLockTime`, maker reclaims the NFT.
//! 4. **`refundErc{721,1155}MakerPaymentSecret`** — Taker can refund to
//!    maker by revealing `takerSecret`, used in cooperative aborts.
//!
//! This slice intentionally exposes only the maker-side calldata
//! surface. Wiring into [`crate::eth::EthCoin`]'s `SwapOps` and the V2
//! state machines is deferred to a follow-up slice (the maker is the
//! only side that locks an NFT; the taker side stays on fungible-token
//! V2 paths for NFT-for-fungible swaps, the only kind of NFT swap KDF
//! supports today).

use crate::eth::abi::{Contract, Function, Token};
use ethereum_types::{Address, U256};
use lazy_static::lazy_static;

/// Inline minimal ABI for the NFT swap V2 maker contract. Lives next to
/// the calldata builders so the contract surface stays self-documenting
/// and a single source of truth for selectors. The contract layout is
/// our own clean-room design — it deliberately mirrors the fungible
/// `MakerSwapV2` shape so the state-machine layer can be parametrised
/// over both contract families.
pub(crate) const MAKER_NFT_SWAP_V2_ABI: &str = r#"[
    {"inputs":[
        {"name":"id","type":"bytes32"},
        {"name":"taker","type":"address"},
        {"name":"takerSecretHash","type":"bytes32"},
        {"name":"makerSecretHash","type":"bytes32"},
        {"name":"paymentLockTime","type":"uint256"},
        {"name":"tokenAddress","type":"address"},
        {"name":"tokenId","type":"uint256"}
     ],"name":"erc721MakerPayment","outputs":[],"stateMutability":"nonpayable","type":"function"},
    {"inputs":[
        {"name":"id","type":"bytes32"},
        {"name":"amount","type":"uint256"},
        {"name":"taker","type":"address"},
        {"name":"takerSecretHash","type":"bytes32"},
        {"name":"makerSecretHash","type":"bytes32"},
        {"name":"paymentLockTime","type":"uint256"},
        {"name":"tokenAddress","type":"address"},
        {"name":"tokenId","type":"uint256"}
     ],"name":"erc1155MakerPayment","outputs":[],"stateMutability":"nonpayable","type":"function"},
    {"inputs":[
        {"name":"id","type":"bytes32"},
        {"name":"maker","type":"address"},
        {"name":"takerSecretHash","type":"bytes32"},
        {"name":"makerSecret","type":"bytes32"},
        {"name":"tokenAddress","type":"address"},
        {"name":"tokenId","type":"uint256"}
     ],"name":"spendErc721MakerPayment","outputs":[],"stateMutability":"nonpayable","type":"function"},
    {"inputs":[
        {"name":"id","type":"bytes32"},
        {"name":"amount","type":"uint256"},
        {"name":"maker","type":"address"},
        {"name":"takerSecretHash","type":"bytes32"},
        {"name":"makerSecret","type":"bytes32"},
        {"name":"tokenAddress","type":"address"},
        {"name":"tokenId","type":"uint256"}
     ],"name":"spendErc1155MakerPayment","outputs":[],"stateMutability":"nonpayable","type":"function"},
    {"inputs":[
        {"name":"id","type":"bytes32"},
        {"name":"taker","type":"address"},
        {"name":"takerSecretHash","type":"bytes32"},
        {"name":"makerSecretHash","type":"bytes32"},
        {"name":"tokenAddress","type":"address"},
        {"name":"tokenId","type":"uint256"},
        {"name":"paymentLockTime","type":"uint256"}
     ],"name":"refundErc721MakerPaymentTimelock","outputs":[],"stateMutability":"nonpayable","type":"function"},
    {"inputs":[
        {"name":"id","type":"bytes32"},
        {"name":"amount","type":"uint256"},
        {"name":"taker","type":"address"},
        {"name":"takerSecretHash","type":"bytes32"},
        {"name":"makerSecretHash","type":"bytes32"},
        {"name":"tokenAddress","type":"address"},
        {"name":"tokenId","type":"uint256"},
        {"name":"paymentLockTime","type":"uint256"}
     ],"name":"refundErc1155MakerPaymentTimelock","outputs":[],"stateMutability":"nonpayable","type":"function"},
    {"inputs":[
        {"name":"id","type":"bytes32"},
        {"name":"taker","type":"address"},
        {"name":"takerSecret","type":"bytes32"},
        {"name":"makerSecretHash","type":"bytes32"},
        {"name":"tokenAddress","type":"address"},
        {"name":"tokenId","type":"uint256"},
        {"name":"paymentLockTime","type":"uint256"}
     ],"name":"refundErc721MakerPaymentSecret","outputs":[],"stateMutability":"nonpayable","type":"function"},
    {"inputs":[
        {"name":"id","type":"bytes32"},
        {"name":"amount","type":"uint256"},
        {"name":"taker","type":"address"},
        {"name":"takerSecret","type":"bytes32"},
        {"name":"makerSecretHash","type":"bytes32"},
        {"name":"tokenAddress","type":"address"},
        {"name":"tokenId","type":"uint256"},
        {"name":"paymentLockTime","type":"uint256"}
     ],"name":"refundErc1155MakerPaymentSecret","outputs":[],"stateMutability":"nonpayable","type":"function"}
]"#;

lazy_static! {
    pub(crate) static ref MAKER_NFT_SWAP_V2: Contract =
        Contract::load(MAKER_NFT_SWAP_V2_ABI.as_bytes()).expect("MAKER_NFT_SWAP_V2 ABI is valid");
}

/// Family of an NFT involved in a swap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NftKind {
    Erc721,
    Erc1155,
}

impl NftKind {
    fn payment_fn(self) -> &'static str {
        match self {
            NftKind::Erc721 => "erc721MakerPayment",
            NftKind::Erc1155 => "erc1155MakerPayment",
        }
    }

    fn spend_fn(self) -> &'static str {
        match self {
            NftKind::Erc721 => "spendErc721MakerPayment",
            NftKind::Erc1155 => "spendErc1155MakerPayment",
        }
    }

    fn refund_timelock_fn(self) -> &'static str {
        match self {
            NftKind::Erc721 => "refundErc721MakerPaymentTimelock",
            NftKind::Erc1155 => "refundErc1155MakerPaymentTimelock",
        }
    }

    fn refund_secret_fn(self) -> &'static str {
        match self {
            NftKind::Erc721 => "refundErc721MakerPaymentSecret",
            NftKind::Erc1155 => "refundErc1155MakerPaymentSecret",
        }
    }
}

/// Errors that can occur while building or validating NFT swap V2
/// calldata.
#[derive(Debug, PartialEq, Eq)]
pub enum NftSwapV2Error {
    /// The contract ABI did not contain the expected entrypoint, or the
    /// `ethabi` encoder rejected the input. Caller has a bug.
    Abi(String),
    /// The decoded calldata did not match the supplied expectations.
    /// Field name carries which argument differed.
    Mismatch { field: &'static str, detail: String },
    /// `amount` argument is required for ERC-1155 (and disallowed for
    /// ERC-721).
    AmountRequiredForErc1155,
    /// ERC-1155 `amount` must be non-zero.
    ZeroAmount,
}

impl std::fmt::Display for NftSwapV2Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NftSwapV2Error::Abi(e) => write!(f, "NFT swap V2 ABI error: {e}"),
            NftSwapV2Error::Mismatch { field, detail } => {
                write!(f, "NFT swap V2 calldata mismatch on field `{field}`: {detail}")
            },
            NftSwapV2Error::AmountRequiredForErc1155 => {
                write!(f, "NFT swap V2: ERC-1155 entrypoints require an `amount` argument")
            },
            NftSwapV2Error::ZeroAmount => write!(f, "NFT swap V2: ERC-1155 `amount` must be non-zero"),
        }
    }
}

impl std::error::Error for NftSwapV2Error {}

impl From<crate::eth::abi::AbiError> for NftSwapV2Error {
    fn from(e: crate::eth::abi::AbiError) -> Self { NftSwapV2Error::Abi(e.to_string()) }
}

/// Arguments that uniquely identify an NFT maker payment lock-up.
#[derive(Debug, Clone)]
pub struct NftMakerPaymentArgs {
    pub kind: NftKind,
    /// Pre-computed swap id (32 bytes, typically `keccak(timelock || maker_secret_hash)`).
    pub swap_id: [u8; 32],
    /// `Some(amount)` for ERC-1155, `None` for ERC-721.
    pub amount: Option<U256>,
    pub taker: Address,
    pub taker_secret_hash: [u8; 32],
    pub maker_secret_hash: [u8; 32],
    pub payment_time_lock: u64,
    pub token_address: Address,
    pub token_id: U256,
}

/// Arguments for the taker spend (reveals `maker_secret`) of an NFT
/// maker payment.
#[derive(Debug, Clone)]
pub struct NftSpendMakerPaymentArgs {
    pub kind: NftKind,
    pub swap_id: [u8; 32],
    pub amount: Option<U256>,
    pub maker: Address,
    pub taker_secret_hash: [u8; 32],
    pub maker_secret: [u8; 32],
    pub token_address: Address,
    pub token_id: U256,
}

/// Arguments for the maker timelock refund of an NFT maker payment.
#[derive(Debug, Clone)]
pub struct NftRefundTimelockArgs {
    pub kind: NftKind,
    pub swap_id: [u8; 32],
    pub amount: Option<U256>,
    pub taker: Address,
    pub taker_secret_hash: [u8; 32],
    pub maker_secret_hash: [u8; 32],
    pub token_address: Address,
    pub token_id: U256,
    pub payment_time_lock: u64,
}

/// Arguments for the cooperative-secret refund of an NFT maker payment.
#[derive(Debug, Clone)]
pub struct NftRefundSecretArgs {
    pub kind: NftKind,
    pub swap_id: [u8; 32],
    pub amount: Option<U256>,
    pub taker: Address,
    pub taker_secret: [u8; 32],
    pub maker_secret_hash: [u8; 32],
    pub token_address: Address,
    pub token_id: U256,
    pub payment_time_lock: u64,
}

/// Decoded public calldata of an NFT maker-payment transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedNftMakerPayment {
    pub kind: NftKind,
    pub swap_id: [u8; 32],
    pub amount: Option<U256>,
    pub taker: Address,
    pub taker_secret_hash: [u8; 32],
    pub maker_secret_hash: [u8; 32],
    pub payment_time_lock: u64,
    pub token_address: Address,
    pub token_id: U256,
}

/// Decoded public calldata of a taker spend of an NFT maker payment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedNftSpendMakerPayment {
    pub kind: NftKind,
    pub swap_id: [u8; 32],
    pub amount: Option<U256>,
    pub maker: Address,
    pub taker_secret_hash: [u8; 32],
    pub maker_secret: [u8; 32],
    pub token_address: Address,
    pub token_id: U256,
}

/// NFT maker-operation calldata kinds that can be decoded during production
/// recovery without guessing the token standard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NftMakerCalldataKind {
    MakerPayment,
    SpendMakerPayment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodedNftMakerCalldata {
    MakerPayment(DecodedNftMakerPayment),
    SpendMakerPayment(DecodedNftSpendMakerPayment),
}

fn require_amount(args_amount: Option<U256>, kind: NftKind) -> Result<Option<U256>, NftSwapV2Error> {
    match (kind, args_amount) {
        (NftKind::Erc1155, Some(a)) if a.is_zero() => Err(NftSwapV2Error::ZeroAmount),
        (NftKind::Erc1155, Some(a)) => Ok(Some(a)),
        (NftKind::Erc1155, None) => Err(NftSwapV2Error::AmountRequiredForErc1155),
        // ERC-721 ignores `amount`; pass None through unconditionally so
        // a misconfigured caller doesn't accidentally encode it.
        (NftKind::Erc721, _) => Ok(None),
    }
}

/// Build calldata for `erc{721,1155}MakerPayment`.
pub fn encode_maker_payment(args: &NftMakerPaymentArgs) -> Result<Vec<u8>, NftSwapV2Error> {
    let amount = require_amount(args.amount, args.kind)?;
    let function = MAKER_NFT_SWAP_V2.function(args.kind.payment_fn())?;
    let mut tokens: Vec<Token> = vec![Token::FixedBytes(args.swap_id.to_vec())];
    if let Some(a) = amount {
        tokens.push(Token::Uint(a));
    }
    tokens.extend([
        Token::Address(args.taker),
        Token::FixedBytes(args.taker_secret_hash.to_vec()),
        Token::FixedBytes(args.maker_secret_hash.to_vec()),
        Token::Uint(U256::from(args.payment_time_lock)),
        Token::Address(args.token_address),
        Token::Uint(args.token_id),
    ]);
    Ok(function.encode_input(&tokens)?)
}

/// Build calldata for `spendErc{721,1155}MakerPayment`.
pub fn encode_spend_maker_payment(args: &NftSpendMakerPaymentArgs) -> Result<Vec<u8>, NftSwapV2Error> {
    let amount = require_amount(args.amount, args.kind)?;
    let function = MAKER_NFT_SWAP_V2.function(args.kind.spend_fn())?;
    let mut tokens: Vec<Token> = vec![Token::FixedBytes(args.swap_id.to_vec())];
    if let Some(a) = amount {
        tokens.push(Token::Uint(a));
    }
    tokens.extend([
        Token::Address(args.maker),
        Token::FixedBytes(args.taker_secret_hash.to_vec()),
        Token::FixedBytes(args.maker_secret.to_vec()),
        Token::Address(args.token_address),
        Token::Uint(args.token_id),
    ]);
    Ok(function.encode_input(&tokens)?)
}

/// Build calldata for `refundErc{721,1155}MakerPaymentTimelock`.
pub fn encode_refund_timelock(args: &NftRefundTimelockArgs) -> Result<Vec<u8>, NftSwapV2Error> {
    let amount = require_amount(args.amount, args.kind)?;
    let function = MAKER_NFT_SWAP_V2.function(args.kind.refund_timelock_fn())?;
    let mut tokens: Vec<Token> = vec![Token::FixedBytes(args.swap_id.to_vec())];
    if let Some(a) = amount {
        tokens.push(Token::Uint(a));
    }
    tokens.extend([
        Token::Address(args.taker),
        Token::FixedBytes(args.taker_secret_hash.to_vec()),
        Token::FixedBytes(args.maker_secret_hash.to_vec()),
        Token::Address(args.token_address),
        Token::Uint(args.token_id),
        Token::Uint(U256::from(args.payment_time_lock)),
    ]);
    Ok(function.encode_input(&tokens)?)
}

/// Build calldata for `refundErc{721,1155}MakerPaymentSecret`.
pub fn encode_refund_secret(args: &NftRefundSecretArgs) -> Result<Vec<u8>, NftSwapV2Error> {
    let amount = require_amount(args.amount, args.kind)?;
    let function = MAKER_NFT_SWAP_V2.function(args.kind.refund_secret_fn())?;
    let mut tokens: Vec<Token> = vec![Token::FixedBytes(args.swap_id.to_vec())];
    if let Some(a) = amount {
        tokens.push(Token::Uint(a));
    }
    tokens.extend([
        Token::Address(args.taker),
        Token::FixedBytes(args.taker_secret.to_vec()),
        Token::FixedBytes(args.maker_secret_hash.to_vec()),
        Token::Address(args.token_address),
        Token::Uint(args.token_id),
        Token::Uint(U256::from(args.payment_time_lock)),
    ]);
    Ok(function.encode_input(&tokens)?)
}

/// Decode calldata for `erc{721,1155}MakerPayment` against `kind`.
/// Returns the token list in declaration order.
pub fn decode_maker_payment(kind: NftKind, calldata: &[u8]) -> Result<Vec<Token>, NftSwapV2Error> {
    decode_call(kind.payment_fn(), calldata)
}

/// Decode calldata for `spendErc{721,1155}MakerPayment` against `kind`.
pub fn decode_spend_maker_payment(kind: NftKind, calldata: &[u8]) -> Result<Vec<Token>, NftSwapV2Error> {
    decode_call(kind.spend_fn(), calldata)
}

/// Decode NFT maker-operation calldata into a typed representation. The caller
/// must provide `kind`; recovery must park/refuse when the token standard is
/// unavailable instead of inferring it from calldata shape.
pub fn decode_nft_maker_calldata(
    kind: NftKind,
    calldata_kind: NftMakerCalldataKind,
    calldata: &[u8],
) -> Result<DecodedNftMakerCalldata, NftSwapV2Error> {
    match calldata_kind {
        NftMakerCalldataKind::MakerPayment => {
            decode_nft_maker_payment(kind, calldata).map(DecodedNftMakerCalldata::MakerPayment)
        },
        NftMakerCalldataKind::SpendMakerPayment => {
            decode_nft_spend_maker_payment(kind, calldata).map(DecodedNftMakerCalldata::SpendMakerPayment)
        },
    }
}

/// Decode `erc{721,1155}MakerPayment` calldata into typed public fields.
pub fn decode_nft_maker_payment(kind: NftKind, calldata: &[u8]) -> Result<DecodedNftMakerPayment, NftSwapV2Error> {
    let decoded = decode_maker_payment(kind, calldata)?;
    let mut idx = 0usize;
    let swap_id = read_fixed_bytes32(&decoded, &mut idx, "id")?;
    let amount = if kind == NftKind::Erc1155 {
        Some(read_uint(&decoded, &mut idx, "amount")?)
    } else {
        None
    };
    let taker = read_address(&decoded, &mut idx, "taker")?;
    let taker_secret_hash = read_fixed_bytes32(&decoded, &mut idx, "takerSecretHash")?;
    let maker_secret_hash = read_fixed_bytes32(&decoded, &mut idx, "makerSecretHash")?;
    let payment_time_lock = read_u64(&decoded, &mut idx, "paymentLockTime")?;
    let token_address = read_address(&decoded, &mut idx, "tokenAddress")?;
    let token_id = read_uint(&decoded, &mut idx, "tokenId")?;

    Ok(DecodedNftMakerPayment {
        kind,
        swap_id,
        amount,
        taker,
        taker_secret_hash,
        maker_secret_hash,
        payment_time_lock,
        token_address,
        token_id,
    })
}

/// Decode `spendErc{721,1155}MakerPayment` calldata into typed public fields.
pub fn decode_nft_spend_maker_payment(
    kind: NftKind,
    calldata: &[u8],
) -> Result<DecodedNftSpendMakerPayment, NftSwapV2Error> {
    let decoded = decode_spend_maker_payment(kind, calldata)?;
    let mut idx = 0usize;
    let swap_id = read_fixed_bytes32(&decoded, &mut idx, "id")?;
    let amount = if kind == NftKind::Erc1155 {
        Some(read_uint(&decoded, &mut idx, "amount")?)
    } else {
        None
    };
    let maker = read_address(&decoded, &mut idx, "maker")?;
    let taker_secret_hash = read_fixed_bytes32(&decoded, &mut idx, "takerSecretHash")?;
    let maker_secret = read_fixed_bytes32(&decoded, &mut idx, "makerSecret")?;
    let token_address = read_address(&decoded, &mut idx, "tokenAddress")?;
    let token_id = read_uint(&decoded, &mut idx, "tokenId")?;

    Ok(DecodedNftSpendMakerPayment {
        kind,
        swap_id,
        amount,
        maker,
        taker_secret_hash,
        maker_secret,
        token_address,
        token_id,
    })
}

fn decode_call(name: &str, calldata: &[u8]) -> Result<Vec<Token>, NftSwapV2Error> {
    if calldata.len() < 4 {
        return Err(NftSwapV2Error::Mismatch {
            field: "selector",
            detail: "calldata shorter than 4 bytes".to_owned(),
        });
    }
    let function = MAKER_NFT_SWAP_V2.function(name)?;
    let expected_selector = function.short_signature();
    if calldata[..4] != expected_selector {
        return Err(NftSwapV2Error::Mismatch {
            field: "selector",
            detail: format!(
                "expected selector {} for `{}`, got {}",
                hex::encode(expected_selector),
                name,
                hex::encode(&calldata[..4])
            ),
        });
    }
    // ethabi 17's `decode_input` expects parameter bytes WITHOUT the 4-byte
    // selector (the vendored ethabi 6.1 fork used to strip it internally). The
    // selector was validated above, so skip it here.
    Ok(function.decode_input(&calldata[4..])?)
}

/// Validate decoded `erc{721,1155}MakerPayment` calldata against the
/// expected [`NftMakerPaymentArgs`]. Returns `Ok(())` on full match.
pub fn validate_maker_payment(decoded: &[Token], args: &NftMakerPaymentArgs) -> Result<(), NftSwapV2Error> {
    let mut idx = 0usize;
    expect_fixed_bytes(decoded, &mut idx, "id", &args.swap_id)?;
    if args.kind == NftKind::Erc1155 {
        let amount = args.amount.ok_or(NftSwapV2Error::AmountRequiredForErc1155)?;
        expect_uint(decoded, &mut idx, "amount", amount)?;
    }
    expect_address(decoded, &mut idx, "taker", args.taker)?;
    expect_fixed_bytes(decoded, &mut idx, "takerSecretHash", &args.taker_secret_hash)?;
    expect_fixed_bytes(decoded, &mut idx, "makerSecretHash", &args.maker_secret_hash)?;
    expect_uint(decoded, &mut idx, "paymentLockTime", U256::from(args.payment_time_lock))?;
    expect_address(decoded, &mut idx, "tokenAddress", args.token_address)?;
    expect_uint(decoded, &mut idx, "tokenId", args.token_id)?;
    Ok(())
}

/// Compute the on-chain function selector (4-byte) for a given
/// `(kind, op)` pair. Useful for log-based event matching.
pub fn maker_payment_selector(kind: NftKind) -> [u8; 4] {
    MAKER_NFT_SWAP_V2
        .function(kind.payment_fn())
        .expect("ABI contains entrypoint")
        .short_signature()
}

// ──────────────────────────────────────────────────────────────────────
//  EVM transaction-call builders (P10.3.7.c)
// ──────────────────────────────────────────────────────────────────────

/// Fully-resolved EVM call ready to be handed to
/// [`crate::eth::EthCoin::sign_and_send_transaction`]: contract address,
/// calldata, gas limit and ETH value (always zero for NFT HTLCs — the
/// NFT itself is the value).
#[derive(Debug, Clone)]
pub struct NftCall {
    pub contract: Address,
    pub calldata: Vec<u8>,
    pub gas_limit: u64,
    pub value: U256,
}

/// Build the EVM call for `erc{721,1155}MakerPayment` (P10.3.7.c).
pub fn build_maker_payment_call(
    contract: Address,
    gas_limits: &crate::eth::eth_types::EthGasLimitV2,
    args: &NftMakerPaymentArgs,
) -> Result<NftCall, NftSwapV2Error> {
    Ok(NftCall {
        contract,
        calldata: encode_maker_payment(args)?,
        gas_limit: gas_limits.nft_gas_limit(args.kind, super::PaymentMethod::Send),
        value: U256::zero(),
    })
}

/// Build the EVM call for `spendErc{721,1155}MakerPayment`.
pub fn build_spend_maker_payment_call(
    contract: Address,
    gas_limits: &crate::eth::eth_types::EthGasLimitV2,
    args: &NftSpendMakerPaymentArgs,
) -> Result<NftCall, NftSwapV2Error> {
    Ok(NftCall {
        contract,
        calldata: encode_spend_maker_payment(args)?,
        gas_limit: gas_limits.nft_gas_limit(args.kind, super::PaymentMethod::Spend),
        value: U256::zero(),
    })
}

/// Build the EVM call for `refundErc{721,1155}MakerPaymentTimelock`.
pub fn build_refund_timelock_call(
    contract: Address,
    gas_limits: &crate::eth::eth_types::EthGasLimitV2,
    args: &NftRefundTimelockArgs,
) -> Result<NftCall, NftSwapV2Error> {
    Ok(NftCall {
        contract,
        calldata: encode_refund_timelock(args)?,
        gas_limit: gas_limits.nft_gas_limit(args.kind, super::PaymentMethod::RefundTimelock),
        value: U256::zero(),
    })
}

/// Build the EVM call for `refundErc{721,1155}MakerPaymentSecret`.
pub fn build_refund_secret_call(
    contract: Address,
    gas_limits: &crate::eth::eth_types::EthGasLimitV2,
    args: &NftRefundSecretArgs,
) -> Result<NftCall, NftSwapV2Error> {
    Ok(NftCall {
        contract,
        calldata: encode_refund_secret(args)?,
        gas_limit: gas_limits.nft_gas_limit(args.kind, super::PaymentMethod::RefundSecret),
        value: U256::zero(),
    })
}

// ──────────────────────────────────────────────────────────────────────
//  EthCoin maker-side NFT swap entrypoints (P10.3.7.c)
// ──────────────────────────────────────────────────────────────────────

use crate::eth::legacy_tx::Action;
use crate::eth::{EthCoin, EthTxFut};

#[derive(Debug)]
pub enum EthCoinNftError {
    Build(NftSwapV2Error),
    NoNftContract,
}

impl std::fmt::Display for EthCoinNftError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EthCoinNftError::Build(e) => write!(f, "{e}"),
            EthCoinNftError::NoNftContract => {
                write!(f, "EthCoin has no NFT swap V2 contract configured for this chain")
            },
        }
    }
}

impl std::error::Error for EthCoinNftError {}

impl From<NftSwapV2Error> for EthCoinNftError {
    fn from(e: NftSwapV2Error) -> Self { EthCoinNftError::Build(e) }
}

impl EthCoin {
    /// Look up the configured NFT swap V2 contract or return
    /// [`EthCoinNftError::NoNftContract`].
    pub fn nft_swap_v2_contract_addr(&self) -> Result<Address, EthCoinNftError> {
        self.nft_swap_v2_contract.ok_or(EthCoinNftError::NoNftContract)
    }

    /// Build the EVM call for an NFT maker-payment send (does not broadcast).
    pub fn build_send_nft_maker_payment(&self, args: &NftMakerPaymentArgs) -> Result<NftCall, EthCoinNftError> {
        let contract = self.nft_swap_v2_contract_addr()?;
        Ok(build_maker_payment_call(contract, &self.gas_limit_v2, args)?)
    }

    /// Build the EVM call for the taker spending an NFT maker payment.
    pub fn build_spend_nft_maker_payment(&self, args: &NftSpendMakerPaymentArgs) -> Result<NftCall, EthCoinNftError> {
        let contract = self.nft_swap_v2_contract_addr()?;
        Ok(build_spend_maker_payment_call(contract, &self.gas_limit_v2, args)?)
    }

    /// Build the EVM call for the maker timelock-refunding an NFT maker payment.
    pub fn build_refund_nft_maker_payment_timelock(
        &self,
        args: &NftRefundTimelockArgs,
    ) -> Result<NftCall, EthCoinNftError> {
        let contract = self.nft_swap_v2_contract_addr()?;
        Ok(build_refund_timelock_call(contract, &self.gas_limit_v2, args)?)
    }

    /// Build the EVM call for the cooperative-secret refund of an NFT maker payment.
    pub fn build_refund_nft_maker_payment_secret(
        &self,
        args: &NftRefundSecretArgs,
    ) -> Result<NftCall, EthCoinNftError> {
        let contract = self.nft_swap_v2_contract_addr()?;
        Ok(build_refund_secret_call(contract, &self.gas_limit_v2, args)?)
    }

    /// Decode the calldata of an on-chain `erc{721,1155}MakerPayment` tx and
    /// validate it matches `expected`. The transaction must call our
    /// configured NFT swap V2 contract.
    pub fn validate_nft_maker_payment_tx(
        &self,
        tx_to_address: Address,
        tx_calldata: &[u8],
        expected: &NftMakerPaymentArgs,
    ) -> Result<(), EthCoinNftError> {
        let contract = self.nft_swap_v2_contract_addr()?;
        if tx_to_address != contract {
            return Err(EthCoinNftError::Build(NftSwapV2Error::Mismatch {
                field: "to_address",
                detail: format!("expected {contract:?}, got {tx_to_address:?}"),
            }));
        }
        let decoded = decode_maker_payment(expected.kind, tx_calldata)?;
        validate_maker_payment(&decoded, expected)?;
        Ok(())
    }

    /// Sign and broadcast a fully-resolved [`NftCall`]. Public companion
    /// to the `build_*_nft_maker_payment` helpers, used by the
    /// mm2_main NFT maker swap V2 driver (P10.3.7.d).
    pub fn send_nft_call(&self, call: NftCall) -> EthTxFut {
        self.sign_and_send_transaction(
            call.value,
            Action::Call(call.contract),
            call.calldata,
            U256::from(call.gas_limit),
        )
    }
}

// ──────────────────────────────────────────────────────────────────────
//  Decoded-token helpers
// ──────────────────────────────────────────────────────────────────────

fn token_at<'a>(decoded: &'a [Token], idx: usize, field: &'static str) -> Result<&'a Token, NftSwapV2Error> {
    decoded.get(idx).ok_or(NftSwapV2Error::Mismatch {
        field,
        detail: format!("missing argument at position {idx}"),
    })
}

fn read_fixed_bytes32(decoded: &[Token], idx: &mut usize, field: &'static str) -> Result<[u8; 32], NftSwapV2Error> {
    match token_at(decoded, *idx, field)? {
        Token::FixedBytes(bytes) if bytes.len() == 32 => {
            let mut out = [0u8; 32];
            out.copy_from_slice(bytes);
            *idx += 1;
            Ok(out)
        },
        other => Err(NftSwapV2Error::Mismatch {
            field,
            detail: format!("expected bytes32, got {other:?}"),
        }),
    }
}

fn read_address(decoded: &[Token], idx: &mut usize, field: &'static str) -> Result<Address, NftSwapV2Error> {
    match token_at(decoded, *idx, field)? {
        Token::Address(addr) => {
            *idx += 1;
            Ok(*addr)
        },
        other => Err(NftSwapV2Error::Mismatch {
            field,
            detail: format!("expected address, got {other:?}"),
        }),
    }
}

fn read_uint(decoded: &[Token], idx: &mut usize, field: &'static str) -> Result<U256, NftSwapV2Error> {
    match token_at(decoded, *idx, field)? {
        Token::Uint(value) => {
            *idx += 1;
            Ok(*value)
        },
        other => Err(NftSwapV2Error::Mismatch {
            field,
            detail: format!("expected uint256, got {other:?}"),
        }),
    }
}

fn read_u64(decoded: &[Token], idx: &mut usize, field: &'static str) -> Result<u64, NftSwapV2Error> {
    let value = read_uint(decoded, idx, field)?;
    if value > U256::from(u64::MAX) {
        return Err(NftSwapV2Error::Mismatch {
            field,
            detail: format!("uint256 value {value} does not fit in u64"),
        });
    }
    Ok(value.low_u64())
}

fn expect_fixed_bytes(
    decoded: &[Token],
    idx: &mut usize,
    field: &'static str,
    expected: &[u8],
) -> Result<(), NftSwapV2Error> {
    match token_at(decoded, *idx, field)? {
        Token::FixedBytes(bytes) if bytes.as_slice() == expected => {
            *idx += 1;
            Ok(())
        },
        other => Err(NftSwapV2Error::Mismatch {
            field,
            detail: format!("expected FixedBytes({}), got {other:?}", hex::encode(expected)),
        }),
    }
}

fn expect_address(
    decoded: &[Token],
    idx: &mut usize,
    field: &'static str,
    expected: Address,
) -> Result<(), NftSwapV2Error> {
    match token_at(decoded, *idx, field)? {
        Token::Address(addr) if *addr == expected => {
            *idx += 1;
            Ok(())
        },
        other => Err(NftSwapV2Error::Mismatch {
            field,
            detail: format!("expected Address({expected:?}), got {other:?}"),
        }),
    }
}

fn expect_uint(decoded: &[Token], idx: &mut usize, field: &'static str, expected: U256) -> Result<(), NftSwapV2Error> {
    match token_at(decoded, *idx, field)? {
        Token::Uint(u) if *u == expected => {
            *idx += 1;
            Ok(())
        },
        other => Err(NftSwapV2Error::Mismatch {
            field,
            detail: format!("expected Uint({expected}), got {other:?}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(byte: u8) -> Address { Address::from([byte; 20]) }

    fn hash32(byte: u8) -> [u8; 32] { [byte; 32] }

    fn sample_erc721_args() -> NftMakerPaymentArgs {
        NftMakerPaymentArgs {
            kind: NftKind::Erc721,
            swap_id: hash32(0xAA),
            amount: None,
            taker: addr(0x11),
            taker_secret_hash: hash32(0xBB),
            maker_secret_hash: hash32(0xCC),
            payment_time_lock: 1_900_000_000,
            token_address: addr(0x22),
            token_id: U256::from(1234u64),
        }
    }

    fn sample_erc1155_args() -> NftMakerPaymentArgs {
        NftMakerPaymentArgs {
            kind: NftKind::Erc1155,
            swap_id: hash32(0xDD),
            amount: Some(U256::from(7u64)),
            taker: addr(0x33),
            taker_secret_hash: hash32(0xEE),
            maker_secret_hash: hash32(0x99),
            payment_time_lock: 1_950_000_000,
            token_address: addr(0x44),
            token_id: U256::from(56u64),
        }
    }

    #[test]
    fn maker_payment_selector_is_stable_for_erc721() {
        let sel = maker_payment_selector(NftKind::Erc721);
        // Recompute via ethabi to confirm it matches.
        let want = MAKER_NFT_SWAP_V2
            .function("erc721MakerPayment")
            .unwrap()
            .short_signature();
        assert_eq!(sel, want);
    }

    #[test]
    fn maker_payment_selector_is_stable_for_erc1155() {
        let sel = maker_payment_selector(NftKind::Erc1155);
        let want = MAKER_NFT_SWAP_V2
            .function("erc1155MakerPayment")
            .unwrap()
            .short_signature();
        assert_eq!(sel, want);
    }

    #[test]
    fn erc721_and_erc1155_selectors_differ() {
        assert_ne!(
            maker_payment_selector(NftKind::Erc721),
            maker_payment_selector(NftKind::Erc1155)
        );
    }

    #[test]
    fn encode_then_decode_roundtrip_erc721_payment() {
        let args = sample_erc721_args();
        let calldata = encode_maker_payment(&args).expect("encode");
        // First 4 bytes are selector.
        assert_eq!(calldata[..4], maker_payment_selector(NftKind::Erc721));
        let decoded = decode_maker_payment(NftKind::Erc721, &calldata).expect("decode");
        validate_maker_payment(&decoded, &args).expect("validate");
    }

    #[test]
    fn encode_then_decode_roundtrip_erc1155_payment() {
        let args = sample_erc1155_args();
        let calldata = encode_maker_payment(&args).expect("encode");
        assert_eq!(calldata[..4], maker_payment_selector(NftKind::Erc1155));
        let decoded = decode_maker_payment(NftKind::Erc1155, &calldata).expect("decode");
        validate_maker_payment(&decoded, &args).expect("validate");
    }

    #[test]
    fn typed_maker_payment_decode_recovers_public_restart_fields() {
        let args = sample_erc1155_args();
        let calldata = encode_maker_payment(&args).expect("encode");
        let decoded = decode_nft_maker_payment(NftKind::Erc1155, &calldata).expect("typed decode");

        assert_eq!(decoded.kind, args.kind);
        assert_eq!(decoded.swap_id, args.swap_id);
        assert_eq!(decoded.amount, args.amount);
        assert_eq!(decoded.taker, args.taker);
        assert_eq!(decoded.taker_secret_hash, args.taker_secret_hash);
        assert_eq!(decoded.maker_secret_hash, args.maker_secret_hash);
        assert_eq!(decoded.payment_time_lock, args.payment_time_lock);
        assert_eq!(decoded.token_address, args.token_address);
        assert_eq!(decoded.token_id, args.token_id);
    }

    #[test]
    fn validate_rejects_wrong_taker() {
        let args = sample_erc721_args();
        let calldata = encode_maker_payment(&args).expect("encode");
        let decoded = decode_maker_payment(NftKind::Erc721, &calldata).expect("decode");
        let mut bad = args.clone();
        bad.taker = addr(0xFF);
        let err = validate_maker_payment(&decoded, &bad).unwrap_err();
        match err {
            NftSwapV2Error::Mismatch { field, .. } => assert_eq!(field, "taker"),
            other => panic!("expected Mismatch on taker, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_wrong_token_id() {
        let args = sample_erc1155_args();
        let calldata = encode_maker_payment(&args).expect("encode");
        let decoded = decode_maker_payment(NftKind::Erc1155, &calldata).expect("decode");
        let mut bad = args.clone();
        bad.token_id = U256::from(9999u64);
        let err = validate_maker_payment(&decoded, &bad).unwrap_err();
        match err {
            NftSwapV2Error::Mismatch { field, .. } => assert_eq!(field, "tokenId"),
            other => panic!("expected Mismatch on tokenId, got {other:?}"),
        }
    }

    #[test]
    fn decode_rejects_wrong_kind() {
        let args = sample_erc721_args();
        let calldata = encode_maker_payment(&args).expect("encode");
        let err = decode_maker_payment(NftKind::Erc1155, &calldata).unwrap_err();
        match err {
            NftSwapV2Error::Mismatch { field, .. } => assert_eq!(field, "selector"),
            other => panic!("expected selector mismatch, got {other:?}"),
        }
    }

    #[test]
    fn erc1155_requires_amount() {
        let mut args = sample_erc1155_args();
        args.amount = None;
        let err = encode_maker_payment(&args).unwrap_err();
        assert_eq!(err, NftSwapV2Error::AmountRequiredForErc1155);
    }

    #[test]
    fn erc1155_rejects_zero_amount() {
        let mut args = sample_erc1155_args();
        args.amount = Some(U256::zero());
        let err = encode_maker_payment(&args).unwrap_err();
        assert_eq!(err, NftSwapV2Error::ZeroAmount);
    }

    #[test]
    fn erc721_ignores_amount() {
        let mut args = sample_erc721_args();
        args.amount = Some(U256::from(42u64));
        // Encoding still succeeds and produces the no-amount form.
        let calldata = encode_maker_payment(&args).expect("encode");
        let decoded = decode_maker_payment(NftKind::Erc721, &calldata).expect("decode");
        let mut clean = args.clone();
        clean.amount = None;
        validate_maker_payment(&decoded, &clean).expect("validate");
    }

    #[test]
    fn spend_payment_roundtrip_erc721() {
        let args = NftSpendMakerPaymentArgs {
            kind: NftKind::Erc721,
            swap_id: hash32(0x10),
            amount: None,
            maker: addr(0x55),
            taker_secret_hash: hash32(0x20),
            maker_secret: hash32(0x30),
            token_address: addr(0x66),
            token_id: U256::from(7u64),
        };
        let calldata = encode_spend_maker_payment(&args).expect("encode");
        let decoded = decode_spend_maker_payment(NftKind::Erc721, &calldata).expect("decode");
        // Spot-check: third token is `taker` for payment, `maker` for spend.
        assert!(matches!(decoded.first(), Some(Token::FixedBytes(b)) if b.as_slice() == args.swap_id));
        assert!(matches!(decoded.get(1), Some(Token::Address(a)) if *a == args.maker));
    }

    #[test]
    fn spend_payment_roundtrip_erc1155() {
        let args = NftSpendMakerPaymentArgs {
            kind: NftKind::Erc1155,
            swap_id: hash32(0x40),
            amount: Some(U256::from(3u64)),
            maker: addr(0x77),
            taker_secret_hash: hash32(0x50),
            maker_secret: hash32(0x60),
            token_address: addr(0x88),
            token_id: U256::from(99u64),
        };
        let calldata = encode_spend_maker_payment(&args).expect("encode");
        let decoded = decode_spend_maker_payment(NftKind::Erc1155, &calldata).expect("decode");
        // Position 1 must be `amount` for ERC-1155 spend.
        assert!(matches!(decoded.get(1), Some(Token::Uint(u)) if *u == U256::from(3u64)));
    }

    #[test]
    fn typed_spend_decode_is_available_through_production_calldata_helper() {
        let args = NftSpendMakerPaymentArgs {
            kind: NftKind::Erc721,
            swap_id: hash32(0x41),
            amount: None,
            maker: addr(0x78),
            taker_secret_hash: hash32(0x51),
            maker_secret: hash32(0x61),
            token_address: addr(0x89),
            token_id: U256::from(100u64),
        };
        let calldata = encode_spend_maker_payment(&args).expect("encode");

        match decode_nft_maker_calldata(NftKind::Erc721, NftMakerCalldataKind::SpendMakerPayment, &calldata)
            .expect("decode")
        {
            DecodedNftMakerCalldata::SpendMakerPayment(decoded) => {
                assert_eq!(decoded.swap_id, args.swap_id);
                assert_eq!(decoded.maker, args.maker);
                assert_eq!(decoded.maker_secret, args.maker_secret);
                assert_eq!(decoded.token_address, args.token_address);
                assert_eq!(decoded.token_id, args.token_id);
            },
            other => panic!("expected spend calldata, got {other:?}"),
        }
    }

    #[test]
    fn refund_timelock_encodes_for_both_kinds() {
        for (kind, amount) in [(NftKind::Erc721, None), (NftKind::Erc1155, Some(U256::from(2u64)))] {
            let args = NftRefundTimelockArgs {
                kind,
                swap_id: hash32(0x70),
                amount,
                taker: addr(0xAA),
                taker_secret_hash: hash32(0x80),
                maker_secret_hash: hash32(0x90),
                token_address: addr(0xBB),
                token_id: U256::from(1u64),
                payment_time_lock: 1_700_000_000,
            };
            let calldata = encode_refund_timelock(&args).expect("encode");
            assert!(calldata.len() >= 4, "calldata must include selector");
        }
    }

    #[test]
    fn refund_secret_encodes_for_both_kinds() {
        for (kind, amount) in [(NftKind::Erc721, None), (NftKind::Erc1155, Some(U256::from(5u64)))] {
            let args = NftRefundSecretArgs {
                kind,
                swap_id: hash32(0xA0),
                amount,
                taker: addr(0xCC),
                taker_secret: hash32(0xB0),
                maker_secret_hash: hash32(0xC0),
                token_address: addr(0xDD),
                token_id: U256::from(8u64),
                payment_time_lock: 1_800_000_000,
            };
            let calldata = encode_refund_secret(&args).expect("encode");
            assert!(calldata.len() >= 4);
        }
    }

    #[test]
    fn decode_rejects_short_calldata() {
        let err = decode_maker_payment(NftKind::Erc721, &[0u8, 1, 2]).unwrap_err();
        match err {
            NftSwapV2Error::Mismatch { field, .. } => assert_eq!(field, "selector"),
            other => panic!("expected selector mismatch, got {other:?}"),
        }
    }

    // P10.3.7.b — gas limit dispatch tests
    use crate::eth::eth_swap_v2::PaymentMethod;
    use crate::eth::eth_types::EthGasLimitV2;

    #[test]
    fn nft_gas_limit_dispatches_per_kind_and_method() {
        let g = EthGasLimitV2::default();
        // Defaults defined in EthGasLimitV2::default(): 200k for ERC-721,
        // 220k for ERC-1155, across all four maker-side methods.
        for method in [
            PaymentMethod::Send,
            PaymentMethod::Spend,
            PaymentMethod::RefundTimelock,
            PaymentMethod::RefundSecret,
        ] {
            assert_eq!(g.nft_gas_limit(NftKind::Erc721, method), 200_000);
            assert_eq!(g.nft_gas_limit(NftKind::Erc1155, method), 220_000);
        }
    }

    #[test]
    fn nft_gas_limit_methods_are_distinct_fields() {
        // Mutating one method's slot must not affect others.
        let mut g = EthGasLimitV2::default();
        g.maker.nft_erc721_payment = 1;
        g.maker.nft_erc721_taker_spend = 2;
        g.maker.nft_erc721_maker_refund_timelock = 3;
        g.maker.nft_erc721_maker_refund_secret = 4;
        assert_eq!(g.nft_gas_limit(NftKind::Erc721, PaymentMethod::Send), 1);
        assert_eq!(g.nft_gas_limit(NftKind::Erc721, PaymentMethod::Spend), 2);
        assert_eq!(g.nft_gas_limit(NftKind::Erc721, PaymentMethod::RefundTimelock), 3);
        assert_eq!(g.nft_gas_limit(NftKind::Erc721, PaymentMethod::RefundSecret), 4);
        // ERC-1155 row untouched.
        assert_eq!(g.nft_gas_limit(NftKind::Erc1155, PaymentMethod::Send), 220_000);
    }

    // P10.3.7.c — call builder tests
    fn contract() -> Address { Address::from([0xC0; 20]) }

    #[test]
    fn build_maker_payment_call_carries_contract_value_and_gas() {
        let g = EthGasLimitV2::default();
        let args = sample_erc1155_args();
        let call = build_maker_payment_call(contract(), &g, &args).expect("build");
        assert_eq!(call.contract, contract());
        assert_eq!(call.value, U256::zero());
        assert_eq!(call.gas_limit, g.maker.nft_erc1155_payment);
        // Selector must match ERC-1155 maker payment.
        assert_eq!(call.calldata[..4], maker_payment_selector(NftKind::Erc1155));
        // And the full body decodes back to our args.
        let decoded = decode_maker_payment(NftKind::Erc1155, &call.calldata).expect("decode");
        validate_maker_payment(&decoded, &args).expect("validate");
    }

    #[test]
    fn build_spend_call_uses_spend_gas_limit() {
        let g = EthGasLimitV2::default();
        let args = NftSpendMakerPaymentArgs {
            kind: NftKind::Erc721,
            swap_id: hash32(0x10),
            amount: None,
            maker: addr(0x11),
            taker_secret_hash: hash32(0x20),
            maker_secret: hash32(0x30),
            token_address: addr(0x22),
            token_id: U256::from(7u64),
        };
        let call = build_spend_maker_payment_call(contract(), &g, &args).expect("build");
        assert_eq!(call.gas_limit, g.maker.nft_erc721_taker_spend);
    }

    #[test]
    fn build_refund_timelock_call_uses_refund_timelock_gas_limit() {
        let g = EthGasLimitV2::default();
        let args = NftRefundTimelockArgs {
            kind: NftKind::Erc1155,
            swap_id: hash32(0x40),
            amount: Some(U256::from(2u64)),
            taker: addr(0xAA),
            taker_secret_hash: hash32(0x50),
            maker_secret_hash: hash32(0x60),
            token_address: addr(0xBB),
            token_id: U256::from(3u64),
            payment_time_lock: 1_700_000_000,
        };
        let call = build_refund_timelock_call(contract(), &g, &args).expect("build");
        assert_eq!(call.gas_limit, g.maker.nft_erc1155_maker_refund_timelock);
    }

    #[test]
    fn build_refund_secret_call_uses_refund_secret_gas_limit() {
        let g = EthGasLimitV2::default();
        let args = NftRefundSecretArgs {
            kind: NftKind::Erc721,
            swap_id: hash32(0x70),
            amount: None,
            taker: addr(0xCC),
            taker_secret: hash32(0x80),
            maker_secret_hash: hash32(0x90),
            token_address: addr(0xDD),
            token_id: U256::from(8u64),
            payment_time_lock: 1_800_000_000,
        };
        let call = build_refund_secret_call(contract(), &g, &args).expect("build");
        assert_eq!(call.gas_limit, g.maker.nft_erc721_maker_refund_secret);
    }

    #[test]
    fn build_call_propagates_amount_required_error() {
        let g = EthGasLimitV2::default();
        let mut bad = sample_erc1155_args();
        bad.amount = None;
        let err = build_maker_payment_call(contract(), &g, &bad).unwrap_err();
        assert!(matches!(err, NftSwapV2Error::AmountRequiredForErc1155));
    }
}
