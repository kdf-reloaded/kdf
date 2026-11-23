use super::*;

#[cfg(not(target_arch = "wasm32"))]
use crate::lightning::InvoiceForRPC;

/// Optional, recipient-supplied instructions that a payer consumes when
/// constructing its swap payment. Negotiated early in the swap and journalled so
/// it survives a restart. The `Lightning` arm carries a BOLT11 invoice (native
/// targets only); the `WatcherReward` arm carries the reward amount the payer
/// embeds. Carried by the `*PaymentInstructionsReceived` legacy swap events.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PaymentInstructions {
    #[cfg(not(target_arch = "wasm32"))]
    Lightning(InvoiceForRPC),
    WatcherReward(BigDecimal),
}

/// Exactly one smallest unit of a coin with `decimals` decimals, i.e. 10^-decimals, as a rational.
/// Valid for every `u8`: the scale is carried by `BigDecimal`, so no machine-integer power is formed.
pub(crate) fn one_smallest_unit(decimals: u8) -> MmNumber { BigDecimal::new(1.into(), i64::from(decimals)).into() }

pub type BalanceResult<T> = Result<T, MmError<BalanceError>>;
pub type BalanceFut<T> = Box<dyn Future<Item = T, Error = MmError<BalanceError>> + Send>;
pub type NonZeroBalanceFut<T> = Box<dyn Future<Item = T, Error = MmError<GetNonZeroBalance>> + Send>;
pub type NumConversResult<T> = Result<T, MmError<NumConversError>>;
pub type StakingInfosResult = Result<StakingInfos, MmError<StakingInfosError>>;
pub type StakingInfosFut = Box<dyn Future<Item = StakingInfos, Error = MmError<StakingInfosError>> + Send>;
pub type DelegationResult = Result<TransactionDetails, MmError<DelegationError>>;
pub type DelegationFut = Box<dyn Future<Item = TransactionDetails, Error = MmError<DelegationError>> + Send>;
pub type WithdrawResult = Result<TransactionDetails, MmError<WithdrawError>>;
pub type WithdrawFut = Box<dyn Future<Item = TransactionDetails, Error = MmError<WithdrawError>> + Send>;
pub type TradePreimageResult<T> = Result<T, MmError<TradePreimageError>>;
pub type TradePreimageFut<T> = Box<dyn Future<Item = T, Error = MmError<TradePreimageError>> + Send>;
pub type CoinFindResult<T> = Result<T, MmError<CoinFindError>>;
pub type TxHistoryFut<T> = Box<dyn Future<Item = T, Error = MmError<TxHistoryError>> + Send>;
pub type TxHistoryResult<T> = Result<T, MmError<TxHistoryError>>;
pub type RawTransactionResult = Result<RawTransactionRes, MmError<RawTransactionError>>;
pub type RawTransactionFut<'a> =
    Box<dyn Future<Item = RawTransactionRes, Error = MmError<RawTransactionError>> + Send + 'a>;
#[derive(Deserialize)]
pub struct RawTransactionRequest {
    pub coin: String,
    pub tx_hash: String,
}
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RawTransactionRes {
    /// Raw bytes of signed transaction in hexadecimal string, this should be return hexadecimal encoded signed transaction for get_raw_transaction
    pub tx_hex: BytesJson,
    /// The transaction in a coin's native JSON form, for a coin whose native
    /// serialisation of a transaction is JSON text rather than a binary encoding --
    /// currently Sia only (CRD ch.20 R-W12). It is the same serialisation `tx_hex`
    /// hex-encodes, emitted unencoded, and is omitted entirely for every other coin
    /// family, exactly as the equivalent field on `TransactionDetails` (R-W7).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tx_json: Option<Json>,
}
/// Previous utxo transaction data for signing
#[derive(Clone, Debug, Deserialize)]
pub struct PrevTxns {
    /// transaction hash
    pub tx_hash: String,
    /// transaction output index
    pub index: u32,
    /// transaction output script pub key
    pub script_pub_key: String,
    /// transaction output amount
    pub amount: BigDecimal,
}
/// sign_raw_transaction RPC request's params for signing raw utxo transactions
#[derive(Clone, Debug, Deserialize)]
pub struct SignUtxoTransactionParams {
    /// unsigned utxo transaction in hex
    pub tx_hex: String,
    /// optional data of previous transactions referred by unsigned transaction inputs
    pub prev_txns: Option<Vec<PrevTxns>>,
}
/// sign_raw_transaction RPC request's params for signing raw eth transactions
#[derive(Clone, Debug, Deserialize)]
pub struct SignEthTransactionParams {
    /// Eth transfer value
    pub value: Option<BigDecimal>,
    /// Eth to address
    pub to: Option<String>,
    /// Eth contract data
    pub data: Option<String>,
    /// Eth gas use limit
    pub gas_limit: u64,
    /// Legacy gas price in gwei
    pub gas_price: BigDecimal,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", content = "tx")]
pub enum SignRawTransactionEnum {
    UTXO(SignUtxoTransactionParams),
    ETH(SignEthTransactionParams),
}
/// sign_raw_transaction RPC request
#[derive(Clone, Debug, Deserialize)]
pub struct SignRawTransactionRequest {
    pub coin: String,
    #[serde(flatten)]
    pub tx: SignRawTransactionEnum,
}
/// A secp256k1 secret key used by Iguana (legacy single-key) mode.
pub type IguanaPrivKey = keys::Secret;
pub type SignatureResult<T> = Result<T, MmError<SignatureError>>;
pub type VerificationResult<T> = Result<T, MmError<VerificationError>>;
pub trait Transaction: fmt::Debug + 'static {
    /// Raw transaction bytes of the transaction
    fn tx_hex(&self) -> Vec<u8>;
    /// Serializable representation of tx hash for displaying purpose
    fn tx_hash(&self) -> BytesJson;
}
#[derive(Clone, Debug, PartialEq)]
pub enum TransactionEnum {
    UtxoTx(UtxoTx),
    SignedEthTx(SignedEthTx),
    #[cfg(not(target_arch = "wasm32"))]
    ZTransaction(ZTransaction),
    SiaTransaction(siacoin::SiaTransaction),
    CosmosTransaction(tendermint::CosmosTransaction),
    /// A Solana wire transaction (CRD ch.40 R40.6D.9): the swap's payment, claim, refund and dex fee.
    SolanaTransaction(solana::SolanaSwapTx),
}
ifrom!(TransactionEnum, UtxoTx);
ifrom!(TransactionEnum, SignedEthTx);
#[cfg(not(target_arch = "wasm32"))]
ifrom!(TransactionEnum, ZTransaction);
impl From<siacoin::SiaTransaction> for TransactionEnum {
    fn from(t: siacoin::SiaTransaction) -> TransactionEnum { TransactionEnum::SiaTransaction(t) }
}
impl From<solana::SolanaSwapTx> for TransactionEnum {
    fn from(t: solana::SolanaSwapTx) -> TransactionEnum { TransactionEnum::SolanaTransaction(t) }
}
impl From<tendermint::CosmosTransaction> for TransactionEnum {
    fn from(t: tendermint::CosmosTransaction) -> TransactionEnum { TransactionEnum::CosmosTransaction(t) }
}
impl Deref for TransactionEnum {
    type Target = dyn Transaction;
    fn deref(&self) -> &dyn Transaction {
        match self {
            TransactionEnum::UtxoTx(ref t) => t,
            TransactionEnum::SignedEthTx(ref t) => t,
            #[cfg(not(target_arch = "wasm32"))]
            TransactionEnum::ZTransaction(ref t) => t,
            TransactionEnum::SiaTransaction(ref t) => t,
            TransactionEnum::CosmosTransaction(ref t) => t,
            TransactionEnum::SolanaTransaction(ref t) => t,
        }
    }
}
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum TransactionErr {
    /// Keeps transactions while throwing errors.
    TxRecoverable(TransactionEnum, String),
    /// Simply for plain error messages.
    Plain(String),
    /// CRD ch.56 R5: an EVM node rejected the transaction, or its gas estimation, for a fee cap below the
    /// block base fee. A rejected broadcast keeps its signed transaction, as [`TransactionErr::TxRecoverable`]
    /// does: a node that did not answer may still have taken it.
    FeeCapBelowBaseFee {
        rejection: FeeCapBelowBaseFee,
        tx: Option<TransactionEnum>,
    },
}
impl TransactionErr {
    /// Returns transaction if the error includes it.
    #[inline]
    pub fn get_tx(&self) -> Option<TransactionEnum> {
        match self {
            TransactionErr::TxRecoverable(tx, _) => Some(tx.clone()),
            TransactionErr::FeeCapBelowBaseFee { tx, .. } => tx.clone(),
            TransactionErr::Plain(_) => None,
        }
    }

    #[inline]
    /// Returns plain text part of error.
    pub fn get_plain_text_format(&self) -> String {
        match self {
            TransactionErr::TxRecoverable(_, err) => err.to_string(),
            TransactionErr::Plain(err) => err.to_string(),
            TransactionErr::FeeCapBelowBaseFee { rejection, .. } => rejection.to_string(),
        }
    }
}
pub type TransactionFut = Box<dyn Future<Item = TransactionEnum, Error = TransactionErr> + Send>;
#[derive(Debug, PartialEq)]
pub enum FoundSwapTxSpend {
    Spent(TransactionEnum),
    Refunded(TransactionEnum),
}
pub enum CanRefundHtlc {
    CanRefundNow,
    // returns the number of seconds to sleep before HTLC becomes refundable
    HaveToWait(u64),
}
#[derive(Debug, Display, Eq, PartialEq)]
pub enum NegotiateSwapContractAddrErr {
    #[display(fmt = "InvalidOtherAddrLen, addr supplied {:?}", _0)]
    InvalidOtherAddrLen(BytesJson),
    #[display(fmt = "UnexpectedOtherAddr, addr supplied {:?}", _0)]
    UnexpectedOtherAddr(BytesJson),
    NoOtherAddrAndNoFallback,
    /// The coin was activated without a swap contract (a wallet-only coin), so it cannot take part in a swap.
    #[display(fmt = "The coin has no swap contract")]
    NoSwapContract,
}
/// Width of a per-coin hash-time-locked-contract public key, at the coin layer
/// and on the legacy swap wire alike (CRD ch.51 R62, R63).
///
/// The width belongs to the deployed wire format, not to secp256k1. A chain
/// whose native key is narrower occupies the field by the padding convention of
/// R64 instead of shortening it, so neither the swap machines nor the
/// negotiation message ever become key-length-polymorphic.
pub const SWAP_HTLC_PUBKEY_LEN: usize = 33;
/// Failure of one of the two coin-layer key operations bound by CRD ch.51 R63,
/// [`SwapOps::derive_htlc_pubkey`](crate::SwapOps::derive_htlc_pubkey) and
/// [`SwapOps::validate_other_pubkey`](crate::SwapOps::validate_other_pubkey).
#[derive(Debug, Display, Eq, PartialEq)]
pub enum HtlcPubkeyError {
    #[display(fmt = "HTLC public key must be exactly {} bytes, got {}", SWAP_HTLC_PUBKEY_LEN, _0)]
    UnexpectedLength(usize),
    #[display(fmt = "HTLC public key is not a valid {} point: {}", _0, _1)]
    NotOnCurve(&'static str, String),
    #[display(fmt = "Coin cannot derive an HTLC public key: {}", _0)]
    NotAvailable(String),
}
/// How a coin's legacy swap key differs from the key it pays from under a global-HD login (CRD ch.05 R36, R38).
/// The three cases are the three reasons of R38.
#[derive(Clone, Debug, PartialEq)]
pub enum SwapKeyDisagreement {
    /// An EVM coin: its signer is not the node's persistent key, which is what its swaps announce.
    EvmSigner,
    /// A UTXO coin enabled with a single key (legacy `enable` or `electrum`, or `IguanaPrivKey`): that key
    /// is not the node's persistent key.
    SingleKey,
    /// A UTXO coin activated at an HD address other than the one it pays from. Both paths in full.
    HdAddress {
        activated_path: String,
        paying_path: String,
    },
}
/// Where the burn portion of a DEX fee is sent.
#[derive(Clone, Debug, PartialEq)]
pub enum DexFeeBurnDestination {
    /// KMD-specific: value is attached to an OP_RETURN output (provably unspendable).
    KmdOpReturn,
    /// Non-KMD coins: value is sent to a designated burn address (P2PKH).
    /// The `burn_pubkey` is the compressed public key of the burn address.
    PreBurnAccount { burn_pubkey: Vec<u8> },
}
/// Represents the DEX fee for a taker swap, optionally split between a
/// fee-collection address and a burn output.
///
/// The `WithBurn` variant encodes a split (e.g. 75% to fee address, 25%
/// burned) configured per-network via `NetConfig::dex_fee_share()`.
#[derive(Clone, Debug, PartialEq)]
pub enum DexFee {
    /// No fee required (taker is an active burn-account pubkey — rare edge case).
    NoFee,
    /// Standard single-output fee: the entire amount goes to the DEX fee address.
    Standard(MmNumber),
    /// Split fee: `fee_amount` to the DEX fee address, `burn_amount` destroyed.
    WithBurn {
        fee_amount: MmNumber,
        burn_amount: MmNumber,
        burn_destination: DexFeeBurnDestination,
    },
}
impl DexFee {
    /// Total amount the taker must spend on the fee transaction.
    pub fn total_spend_amount(&self) -> MmNumber {
        match self {
            DexFee::NoFee => MmNumber::from(0),
            DexFee::Standard(amount) => amount.clone(),
            DexFee::WithBurn {
                fee_amount,
                burn_amount,
                ..
            } => fee_amount + burn_amount,
        }
    }

    /// The portion that goes to the fee-collection address.
    pub fn fee_amount(&self) -> MmNumber {
        match self {
            DexFee::NoFee => MmNumber::from(0),
            DexFee::Standard(amount) => amount.clone(),
            DexFee::WithBurn { fee_amount, .. } => fee_amount.clone(),
        }
    }

    /// The portion that is burned (zero for Standard / NoFee).
    pub fn burn_amount(&self) -> MmNumber {
        match self {
            DexFee::NoFee | DexFee::Standard(_) => MmNumber::from(0),
            DexFee::WithBurn { burn_amount, .. } => burn_amount.clone(),
        }
    }
}
impl fmt::Display for DexFee {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DexFee::NoFee => write!(f, "NoFee"),
            DexFee::Standard(amount) => write!(f, "Standard({})", amount),
            DexFee::WithBurn {
                fee_amount,
                burn_amount,
                ..
            } => write!(f, "WithBurn(fee={}, burn={})", fee_amount, burn_amount),
        }
    }
}

/// KMD path (CRD ch.08 R8 / ch.16 R8): split the (already dust-floored) total
/// fee between the fee address and an `OP_RETURN` burn output using three
/// exact-rational ranges, evaluated in order:
///
/// 1. `total <= dust` -- `Standard(dust)`: one fee output, no burn.
/// 2. `total * share >= dust` -- full split: `fee = total * share`,
///    `burn = total - fee`.
/// 3. otherwise (`dust < total` and `total * share < dust`) -- clamped
///    split: `fee = dust`, `burn = total - dust`. The fee-collection leg is
///    never below dust; only the burn leg can be.
///
/// This is the netid-8762 `v2.6.0-beta` wire contract (issue #11): it
/// replaces an earlier, incorrect implementation that always split 75/25
/// above dust and then exempted the resulting under-dust fee output from the
/// builder's dust guard (see ch.08 R15A's compatibility correction).
pub fn calc_dex_fee_for_op_return(total: MmNumber, min_tx_amount: MmNumber, fee_share: MmNumber) -> DexFee {
    if total <= min_tx_amount {
        return DexFee::Standard(min_tx_amount);
    }
    let fee_part = &total * &fee_share;
    if fee_part >= min_tx_amount {
        let burn_part = &total - &fee_part;
        // Ch.08 R10 safety fallback: never emit a degenerate WithBurn. Not
        // reachable for either production network's parameters (R8), but a
        // non-default share (e.g. 1) can still hit this.
        if fee_part <= MmNumber::from(0) || burn_part <= MmNumber::from(0) {
            return DexFee::Standard(total);
        }
        return DexFee::WithBurn {
            fee_amount: fee_part,
            burn_amount: burn_part,
            burn_destination: DexFeeBurnDestination::KmdOpReturn,
        };
    }
    let burn_part = &total - &min_tx_amount;
    if burn_part <= MmNumber::from(0) {
        return DexFee::Standard(total);
    }
    DexFee::WithBurn {
        fee_amount: min_tx_amount,
        burn_amount: burn_part,
        burn_destination: DexFeeBurnDestination::KmdOpReturn,
    }
}

/// Non-KMD path: split into two P2PKH outputs. Falls back to `Standard`
/// when either leg would be dust under the coin's `min_tx_amount`.
pub fn calc_dex_fee_for_burn_account(
    fee: MmNumber,
    min_tx_amount: MmNumber,
    fee_share: MmNumber,
    burn_pubkey: Vec<u8>,
) -> DexFee {
    let fee_part = &fee * &fee_share;
    let burn_part = &fee - &fee_part;
    if fee_part <= MmNumber::from(0)
        || burn_part <= MmNumber::from(0)
        || burn_part < min_tx_amount
        || fee_part < min_tx_amount
    {
        return DexFee::Standard(fee);
    }
    DexFee::WithBurn {
        fee_amount: fee_part,
        burn_amount: burn_part,
        burn_destination: DexFeeBurnDestination::PreBurnAccount { burn_pubkey },
    }
}

impl DexFee {
    /// Build a `DexFee` from a taker coin and the network burn policy at
    /// swap-initiation time (taker pubkey not yet known).
    pub fn new_from_taker_coin(
        taker_coin: &dyn MmCoin,
        net_cfg: &dyn mm2_net_config::NetConfig,
        base_fee: MmNumber,
    ) -> DexFee {
        if !net_cfg.burn_enabled() {
            return DexFee::Standard(base_fee);
        }
        let min_tx_amount = MmNumber::from(taker_coin.min_tx_amount());
        let fee_share: MmNumber = net_cfg.dex_fee_share().into();
        if taker_coin.should_burn_directly() {
            return calc_dex_fee_for_op_return(base_fee, min_tx_amount, fee_share);
        }
        if !taker_coin.should_burn_dex_fee() {
            return DexFee::Standard(base_fee);
        }
        let burn_pubkey = match taker_coin.burn_pubkey() {
            ref v if !v.is_empty() => v.clone(),
            _ => net_cfg.burn_addr_raw_pubkey().to_vec(),
        };
        if burn_pubkey.is_empty() {
            return DexFee::Standard(base_fee);
        }
        calc_dex_fee_for_burn_account(base_fee, min_tx_amount, fee_share, burn_pubkey)
    }

    /// Validation-time variant (CRD ch.16 R7). Step 1: if the taker coin is
    /// not a privacy coin and its resolved burn public key (the coin's own
    /// non-empty value, else the network's) is byte-for-byte equal to the
    /// taker's public key, return `NoFee` -- the burn-key holder pays no fee
    /// on its own trades. This does NOT depend on the network burn gate or
    /// on either per-coin burn predicate, and applies on both production
    /// networks (netid 8762 and 6133) and both swap protocols. Otherwise
    /// delegates to `new_from_taker_coin`.
    pub fn new_with_taker_pubkey(
        taker_coin: &dyn MmCoin,
        net_cfg: &dyn mm2_net_config::NetConfig,
        base_fee: MmNumber,
        taker_pubkey: &[u8],
    ) -> DexFee {
        if !taker_coin.is_privacy() {
            let burn_pubkey = match taker_coin.burn_pubkey() {
                ref v if !v.is_empty() => v.clone(),
                _ => net_cfg.burn_addr_raw_pubkey().to_vec(),
            };
            if !burn_pubkey.is_empty() && burn_pubkey.as_slice() == taker_pubkey {
                return DexFee::NoFee;
            }
        }
        DexFee::new_from_taker_coin(taker_coin, net_cfg, base_fee)
    }
}
/// Structured arguments for fee validation (replaces positional parameter lists).
pub struct ValidateFeeArgs<'a> {
    /// The fee transaction to validate.
    pub fee_tx: &'a TransactionEnum,
    /// Public key of the expected sender (taker).
    pub expected_sender: &'a [u8],
    /// Raw public key of the DEX fee recipient address.
    pub fee_addr: &'a [u8],
    /// The DEX fee specification (includes burn details if applicable).
    pub dex_fee: &'a DexFee,
    /// Earliest block number the fee tx should appear in.
    pub min_block_number: u64,
    /// Swap UUID (for logging / memo validation).
    pub uuid: &'a [u8],
}
#[derive(Clone, Debug)]
pub struct ValidatePaymentInput {
    pub payment_tx: Vec<u8>,
    pub time_lock: u32,
    pub taker_pub: Vec<u8>,
    pub maker_pub: Vec<u8>,
    pub secret_hash: Vec<u8>,
    pub amount: BigDecimal,
    pub swap_contract_address: Option<BytesJson>,
    pub try_spv_proof_until: u64,
    pub confirmations: u64,
}
/// Input for watcher-side taker fee validation.
#[derive(Clone, Debug)]
pub struct WatcherValidateTakerFeeInput {
    pub taker_fee_hash: Vec<u8>,
    pub sender_pubkey: Vec<u8>,
    pub min_block_number: u64,
    pub fee_addr: Vec<u8>,
    pub lock_duration: u64,
}
/// Input for watcher-side taker payment validation.
#[derive(Clone, Debug)]
pub struct WatcherValidatePaymentInput {
    pub payment_tx: Vec<u8>,
    pub taker_payment_refund_preimage: Vec<u8>,
    pub time_lock: u32,
    pub taker_pub: Vec<u8>,
    pub maker_pub: Vec<u8>,
    pub secret_hash: Vec<u8>,
    pub amount: BigDecimal,
    pub confirmations: u64,
    pub min_block_number: u64,
}
#[derive(Debug, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum WithdrawFee {
    /// UTXO withdraw fee: a rate `amount` per 1,000 bytes, applied exactly like `UtxoPerKbyte` (CRD ch.38
    /// R38.6.6l item 2): proportional and truncated, not per transaction and not per started kilobyte. With a
    /// change output the fee is `floor(a * s / 1000)` once over the whole size, change output included
    /// (R38.6.6b); the change output's share is not rounded on its own. The same holds for the SLP token
    /// withdraw (CRD ch.38 R38.6.6m), where `amount` is in the platform coin's units.
    UtxoFixed {
        amount: BigDecimal,
    },
    UtxoPerKbyte {
        amount: BigDecimal,
    },
    EthGas {
        /// in gwei
        gas_price: BigDecimal,
        gas: u64,
    },
    Qrc20Gas {
        /// in satoshi
        gas_limit: u64,
        gas_price: u64,
    },
    /// Cosmos explicit fee (CRD ch.49 R49.28): the signed transaction's gas limit and the gas price that enters
    /// the simulation-derived fee amount (CRD ch.18 R18.6B.1 item 1, R18.6B.2); the amount is **not**
    /// `gas_limit * gas_price`. `gas_price` is in the platform's base denom per gas unit. Only a Tendermint coin
    /// takes this form; the other families that take a fee descriptor refuse it as a fee type they do not take,
    /// ZCoin refuses every custom fee (CRD ch.49 R49.27a), a TRON withdraw refuses any descriptor
    /// (CRD ch.21 R-WP6), and Solana does not read the descriptor.
    CosmosGas {
        gas_limit: u64,
        gas_price: f64,
    },
    /// Cosmos priority fee (CRD ch.49 R49.29): the signed transaction's gas limit and a named level of the
    /// platform coin's `priority_gas_prices` (CRD ch.36 R36.3.5; CRD ch.18 R18.6B.1 item 2). Only a Tendermint
    /// coin takes this form; the other families that take a fee descriptor refuse it as a fee type they do not
    /// take, ZCoin refuses every custom fee (CRD ch.49 R49.27a), a TRON withdraw refuses any descriptor
    /// (CRD ch.21 R-WP6), and Solana does not read the descriptor.
    CosmosGasPriority {
        gas_limit: u64,
        gas_price_option: CosmosGasPriceOption,
    },
    /// The form a TRON result's `fee_details` carries in `type` (CRD ch.21 R-F2). It exists only so that a request
    /// with such a `fee` parses and a TRON withdraw can refuse it with `InvalidFeePolicy` like any other
    /// descriptor (CRD ch.21 R-WP6): the TRON fee is always estimated (CRD ch.21 R-W3), so the form has no
    /// members and any other member of the object is ignored. No family takes it: the other families that take
    /// a fee descriptor refuse it as a fee type they do not take, ZCoin refuses every custom fee (CRD ch.49
    /// R49.27a), a Tendermint request ignores it as it ignores any other foreign descriptor (CRD ch.49 R49.30,
    /// D49.1), and Solana does not read the descriptor.
    Tron,
}
/// The level named by a [`WithdrawFee::CosmosGasPriority`] descriptor (CRD ch.49 R49.29): the spellings are
/// exactly these four, case-sensitive. `Low`, `Average` and `High` select position 0, 1 and 2 of the platform
/// coin's `priority_gas_prices`; `Default` selects the coin's default gas price.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
pub enum CosmosGasPriceOption {
    Low,
    Average,
    High,
    Default,
}
pub struct WithdrawSenderAddress<Address, Pubkey> {
    pub(crate) address: Address,
    pub(crate) pubkey: Pubkey,
    pub(crate) derivation_path: Option<DerivationPath>,
}
impl<Address, Pubkey> From<HDAddress<Address, Pubkey>> for WithdrawSenderAddress<Address, Pubkey> {
    fn from(addr: HDAddress<Address, Pubkey>) -> Self {
        WithdrawSenderAddress {
            address: addr.address,
            pubkey: addr.pubkey,
            derivation_path: Some(addr.derivation_path),
        }
    }
}
#[derive(Clone, Deserialize)]
#[serde(untagged)]
pub enum WithdrawFrom {
    // AccountId { account_id: u32 },
    AddressId(HDAddressId),
    /// Don't use `Bip44DerivationPath` or `RpcDerivationPath` because if there is an error in the path,
    /// `serde::Deserialize` returns "data did not match any variant of untagged enum WithdrawFrom".
    /// It's better to show the user an informative error.
    DerivationPath {
        derivation_path: String,
    },
}
#[derive(Deserialize)]
pub struct WithdrawRequest {
    pub(crate) coin: String,
    pub(crate) from: Option<WithdrawFrom>,
    pub(crate) to: String,
    #[serde(default)]
    pub(crate) amount: BigDecimal,
    #[serde(default)]
    pub(crate) max: bool,
    pub(crate) fee: Option<WithdrawFee>,
    /// The memo of the transaction, for a coin family that carries one: Tendermint (CRD ch.36 R36.14.29, the memo of
    /// the transaction and of an unsigned result). Absent is the empty memo. Ignored by every other family.
    #[serde(default)]
    pub(crate) memo: Option<String>,
    /// TRON only (CRD ch.21 R21.10B.7): the transaction expires this many seconds after the time of its reference
    /// block. Absent is 60. Ignored by every other family.
    #[serde(default)]
    pub(crate) expiration_seconds: Option<u64>,
    /// The fee rail of the withdraw (CRD ch.63 §63.3.4). Absent is `native`. `gasless` and `auto` are served for a
    /// TRON coin only; every other family refuses them (CRD ch.63 R21).
    #[serde(default)]
    pub(crate) fee_method: Option<WithdrawFeeMethod>,
    /// The constraints of the gasless rail (CRD ch.63 §63.3.4). Refused without `fee_method` `gasless` or `auto`.
    #[serde(default)]
    pub(crate) gasless: Option<GaslessWithdrawOptions>,
}
/// The fee rail a withdraw request selects (CRD ch.63 §63.3.4).
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WithdrawFeeMethod {
    /// The coin's own fee model: the withdraw as it is without the member.
    Native,
    /// The gasless rail is required: the fee is paid in the token, through a fee-delegation provider.
    Gasless,
    /// The native rail first; the gasless rail only when the native rail cannot be funded.
    Auto,
}
/// The `gasless` object of a withdraw request (CRD ch.63 §63.3.4).
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct GaslessWithdrawOptions {
    /// The cap of this request on the provider fee, a decimal amount in token units. It is kept as sent, so that
    /// a value that is not a decimal amount is answered as a fee-validation error of the withdraw (CRD ch.63
    /// §63.9.4) and not as a request that does not decode; [`GaslessWithdrawOptions::max_fee_amount`] reads it.
    #[serde(default)]
    pub(crate) max_fee: Option<Json>,
    /// How long the signed authorization stays valid, in seconds from signing. Zero is refused; absent is 300.
    #[serde(default)]
    pub(crate) deadline_seconds: Option<u64>,
    /// With `fee_method` `gasless`: whether a deterministically unavailable gasless rail may be replaced by the
    /// native rail (CRD ch.63 §63.8 step 6).
    #[serde(default)]
    pub(crate) fallback_to_native: bool,
}
impl GaslessWithdrawOptions {
    /// `max_fee` as a decimal amount: `None` when the request gave none.
    ///
    /// # Errors
    ///
    /// [`WithdrawError::InvalidFeePolicy`] when the member is neither a JSON string nor a JSON number that reads
    /// as a decimal amount, or is below zero (CRD ch.63 §63.9.4). The text does not repeat the value.
    pub(crate) fn max_fee_amount(&self) -> Result<Option<BigDecimal>, WithdrawError> {
        let amount = match &self.max_fee {
            None | Some(Json::Null) => return Ok(None),
            Some(value @ (Json::String(_) | Json::Number(_))) => json::from_value::<BigDecimal>(value.clone())
                .map_err(|_| WithdrawError::InvalidFeePolicy("'gasless.max_fee' is not a decimal amount".to_owned()))?,
            Some(_) => {
                return Err(WithdrawError::InvalidFeePolicy(
                    "'gasless.max_fee' is not a decimal amount".to_owned(),
                ))
            },
        };
        if amount < BigDecimal::from(0) {
            return Err(WithdrawError::InvalidFeePolicy(
                "'gasless.max_fee' is below zero".to_owned(),
            ));
        }
        Ok(Some(amount))
    }
}
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum StakingDetails {
    Qtum(QtumDelegationRequest),
    Cosmos(Box<rpc_command::tendermint::staking::DelegationPayload>),
}
#[allow(dead_code)]
#[derive(Deserialize)]
pub struct AddDelegateRequest {
    pub coin: String,
    pub staking_details: StakingDetails,
}
#[allow(dead_code)]
#[derive(Deserialize)]
pub struct RemoveDelegateRequest {
    pub coin: String,
    pub staking_details: Option<StakingDetails>,
}
#[derive(Deserialize)]
pub struct GetStakingInfosRequest {
    pub coin: String,
}
#[derive(Serialize, Deserialize)]
pub struct SignatureRequest {
    pub(crate) coin: String,
    pub(crate) message: String,
}
#[derive(Serialize, Deserialize)]
pub struct VerificationRequest {
    pub(crate) coin: String,
    pub(crate) message: String,
    pub(crate) signature: String,
    pub(crate) address: String,
}
impl WithdrawRequest {
    pub fn new(
        coin: String,
        from: Option<WithdrawFrom>,
        to: String,
        amount: BigDecimal,
        max: bool,
        fee: Option<WithdrawFee>,
    ) -> WithdrawRequest {
        WithdrawRequest {
            coin,
            from,
            to,
            amount,
            max,
            fee,
            memo: None,
            expiration_seconds: None,
            fee_method: None,
            gasless: None,
        }
    }

    pub fn new_max(coin: String, to: String) -> WithdrawRequest {
        WithdrawRequest {
            coin,
            from: None,
            to,
            amount: 0.into(),
            max: true,
            fee: None,
            memo: None,
            expiration_seconds: None,
            fee_method: None,
            gasless: None,
        }
    }

    /// Whether the request asks for the gasless rail: `fee_method` is `gasless` or `auto`.
    pub(crate) fn asks_for_gasless_rail(&self) -> bool {
        matches!(
            self.fee_method,
            Some(WithdrawFeeMethod::Gasless | WithdrawFeeMethod::Auto)
        )
    }

    /// The fee-rail check every withdraw passes before anything else is done with the request (CRD ch.63 §63.3.4,
    /// R21), for a coin of any family, on the direct method and on the task route. `serves_gasless_rail` says
    /// whether the coin is of the one family that has a gasless rail (a TRON coin).
    ///
    /// # Errors
    ///
    /// - [`WithdrawError::InvalidFeePolicy`] (HTTP 400) for `gasless` options without `fee_method` `gasless` or
    ///   `auto`, for every coin;
    /// - [`WithdrawError::Gasless`] with [`GaslessWithdrawError::Unavailable`] (HTTP 503) for `fee_method` `gasless`
    ///   or `auto` on a coin without a gasless rail.
    pub(crate) fn check_fee_rail(&self, serves_gasless_rail: bool) -> Result<(), WithdrawError> {
        if !self.asks_for_gasless_rail() {
            if self.gasless.is_some() {
                return Err(WithdrawError::InvalidFeePolicy(
                    "'gasless' options need 'fee_method' to be 'gasless' or 'auto'".to_owned(),
                ));
            }
            return Ok(());
        }
        if !serves_gasless_rail {
            return Err(WithdrawError::Gasless(GaslessWithdrawError::Unavailable(format!(
                "{} has no gasless withdraw rail",
                self.coin
            ))));
        }
        Ok(())
    }
}
/// [`WithdrawRequest::check_fee_rail`] for `coin`: only a TRON coin has a gasless rail.
pub(crate) fn check_withdraw_fee_rail(coin: &MmCoinEnum, request: &WithdrawRequest) -> Result<(), WithdrawError> {
    let serves_gasless_rail = matches!(coin, MmCoinEnum::EthCoin(eth) if eth.is_tron_family());
    request.check_fee_rail(serves_gasless_rail)
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum StakingInfosDetails {
    Qtum(QtumStakingInfosDetails),
}
impl From<QtumStakingInfosDetails> for StakingInfosDetails {
    fn from(qtum_staking_infos: QtumStakingInfosDetails) -> Self { StakingInfosDetails::Qtum(qtum_staking_infos) }
}
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct StakingInfos {
    pub staking_infos_details: StakingInfosDetails,
}
#[derive(Serialize)]
pub struct SignatureResponse {
    pub(crate) signature: String,
}
#[derive(Serialize)]
pub struct VerificationResponse {
    pub(crate) is_valid: bool,
}
/// Please note that no type should have the same structure as another type,
/// because this enum has the `untagged` deserialization.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type")]
pub enum TxFeeDetails {
    Utxo(UtxoFeeDetails),
    Eth(EthTxFeeDetails),
    Qrc20(Qrc20FeeDetails),
    Slp(SlpFeeDetails),
    Solana(SolanaFeeDetails),
    Sia(siacoin::SiaFeeDetails),
    Tendermint(tendermint::TendermintFeeDetails),
    /// TRON fee breakdown: bandwidth + energy + optional activation.
    Tron(crate::eth::tron::fee::TronTxFeeDetails),
    /// The fee of a TRC20 withdraw through the gasless rail, paid in the token (CRD ch.63 §63.3.5).
    TronGasless(crate::eth::tron::fee::TronGaslessFeeDetails),
}
/// Deserialize the TxFeeDetails as an untagged enum.
impl<'de> Deserialize<'de> for TxFeeDetails {
    fn deserialize<D>(deserializer: D) -> Result<Self, <D as Deserializer<'de>>::Error>
    where
        D: Deserializer<'de>,
    {
        // `#[serde(untagged)]` ignores the `type` tag `TxFeeDetails`'s own
        // `Serialize` derive writes and picks the first variant whose *shape*
        // the data happens to fit, so every arm of `TxFeeDetails` needs an
        // arm here too -- `Sia` and `Tendermint` never got one when they were
        // added to `TxFeeDetails`. A variant missing from this list isn't
        // merely mis-tagged on read: since one history file holds every tx as
        // one JSON array, a single unrepresentable entry fails deserializing
        // the *whole* file, which `load_history_from_file_impl` treats as
        // corruption and wipes on every single read, including the read
        // `my_tx_history_v2_rpc` does directly against the file on every RPC
        // call (see lp_coins_ops.rs / my_tx_history_v2.rs) -- so a coin with
        // any fee-details shape missing here can never show history at all,
        // not even transiently. Confirmed live: SC's cache reset with a
        // "TxFeeDetailsUnTagged" deserialization error on every KDF start and
        // every history RPC call, immediately discarding whatever the
        // background history loop had just fetched and saved.
        //
        // `Tendermint` is listed before `Utxo` deliberately: its shape
        // (`coin`, `amount`, `gas_limit`) is a strict superset of Utxo's
        // (`coin: Option<..>`, `amount`), so trying Utxo first would silently
        // swallow every Tendermint entry as a (wrongly re-tagged) Utxo one
        // instead of failing -- same bug, quieter symptom, not the one that
        // was reported but the same oversight. Genuine Utxo entries have no
        // `gas_limit` field, so this reordering can't misclassify them.
        //
        // `Solana { fee_amount, rent_amount, total_amount }` (CRD ch.40 R40.6B.12) needs no particular place: all
        // three members are required and `fee_amount` is in no other family's fee object, so it reads no other
        // family's entry (Sia has a `total_amount` but no `fee_amount`) and no other arm reads its entry (every
        // other family requires a member it lacks: `amount`, `coin`, `gas`, `miner_fee`, `bandwidth_used`).
        //
        // `TronGasless` (CRD ch.63 §63.3.5) needs no particular place either: it requires `gasfree_address` and
        // the fixed texts of `fee_method` and `provider_name`, which no other family's fee object has, and every
        // other arm requires a member it lacks (`amount`, `gas`, `miner_fee`, `fee_amount`, `policy`,
        // `bandwidth_used`).
        //
        // `Slp { amount, coin: String }` is deliberately NOT added: its shape
        // is *identical* to Utxo's whenever `coin` is present, so no
        // placement in this list can distinguish the two structurally --
        // fixing that needs the tag to actually be read, not guessed, and is
        // a separate, untested change left alone here.
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum TxFeeDetailsUnTagged {
            Tendermint(tendermint::TendermintFeeDetails),
            Utxo(UtxoFeeDetails),
            Eth(EthTxFeeDetails),
            Qrc20(Qrc20FeeDetails),
            Solana(SolanaFeeDetails),
            Sia(siacoin::SiaFeeDetails),
            Tron(crate::eth::tron::fee::TronTxFeeDetails),
            TronGasless(crate::eth::tron::fee::TronGaslessFeeDetails),
        }

        match Deserialize::deserialize(deserializer)? {
            TxFeeDetailsUnTagged::Tendermint(f) => Ok(TxFeeDetails::Tendermint(f)),
            TxFeeDetailsUnTagged::Utxo(f) => Ok(TxFeeDetails::Utxo(f)),
            TxFeeDetailsUnTagged::Eth(f) => Ok(TxFeeDetails::Eth(f)),
            TxFeeDetailsUnTagged::Qrc20(f) => Ok(TxFeeDetails::Qrc20(f)),
            TxFeeDetailsUnTagged::Solana(f) => Ok(TxFeeDetails::Solana(f)),
            TxFeeDetailsUnTagged::Sia(f) => Ok(TxFeeDetails::Sia(f)),
            TxFeeDetailsUnTagged::Tron(f) => Ok(TxFeeDetails::Tron(f)),
            TxFeeDetailsUnTagged::TronGasless(f) => Ok(TxFeeDetails::TronGasless(f)),
        }
    }
}
impl From<siacoin::SiaFeeDetails> for TxFeeDetails {
    fn from(d: siacoin::SiaFeeDetails) -> Self { TxFeeDetails::Sia(d) }
}
impl From<tendermint::TendermintFeeDetails> for TxFeeDetails {
    fn from(d: tendermint::TendermintFeeDetails) -> Self { TxFeeDetails::Tendermint(d) }
}
impl From<EthTxFeeDetails> for TxFeeDetails {
    fn from(eth_details: EthTxFeeDetails) -> Self { TxFeeDetails::Eth(eth_details) }
}
impl From<crate::eth::tron::fee::TronTxFeeDetails> for TxFeeDetails {
    fn from(tron_details: crate::eth::tron::fee::TronTxFeeDetails) -> Self { TxFeeDetails::Tron(tron_details) }
}
impl From<crate::eth::tron::fee::TronGaslessFeeDetails> for TxFeeDetails {
    fn from(details: crate::eth::tron::fee::TronGaslessFeeDetails) -> Self { TxFeeDetails::TronGasless(details) }
}
impl From<UtxoFeeDetails> for TxFeeDetails {
    fn from(utxo_details: UtxoFeeDetails) -> Self { TxFeeDetails::Utxo(utxo_details) }
}
impl From<Qrc20FeeDetails> for TxFeeDetails {
    fn from(qrc20_details: Qrc20FeeDetails) -> Self { TxFeeDetails::Qrc20(qrc20_details) }
}
impl From<SolanaFeeDetails> for TxFeeDetails {
    fn from(solana_details: SolanaFeeDetails) -> Self { TxFeeDetails::Solana(solana_details) }
}
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct KmdRewardsDetails {
    pub(crate) amount: BigDecimal,
    pub(crate) claimed_by_me: bool,
}
impl KmdRewardsDetails {
    pub fn claimed_by_me(amount: BigDecimal) -> KmdRewardsDetails {
        KmdRewardsDetails {
            amount,
            claimed_by_me: true,
        }
    }
}
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum TransactionType {
    StakingDelegation,
    RemoveDelegation,
    ClaimDelegationRewards,
    StandardTransfer,
    TokenTransfer(BytesJson),
    /// A Siacoin v1 transaction event (CRD ch.53 R53.5.10).
    SiaV1Transaction,
    /// A Siacoin v2 transaction event (CRD ch.53 R53.5.10).
    SiaV2Transaction,
    /// A Siacoin miner *or* foundation payout event (CRD ch.53 R53.5.10);
    /// both consensus payout kinds share this wire value.
    SiaMinerPayout,
    /// The fee a Tendermint account paid for a token transaction, as a row of the platform coin (CRD ch.36
    /// R36.14.8 item 3).
    FeeForTokenTx,
    /// An HTLC creation or claim row of a Tendermint coin (CRD ch.36 R36.14.6). `token_id` is `null` for the
    /// platform denom.
    CustomTendermintMsg {
        msg_type: crate::tendermint::htlc::CustomTendermintMsgType,
        token_id: Option<BytesJson>,
    },
    /// An IBC transfer row of a Tendermint coin (CRD ch.36 R36.14.6). `token_id` is `null` for the platform
    /// denom.
    TendermintIBCTransfer {
        token_id: Option<BytesJson>,
    },
}

impl TransactionType {
    /// The token a history row belongs to, when it belongs to one: the id the indexed history store tags the
    /// row with (CRD ch.36 R36.14.2). A row of the platform coin has none.
    pub fn history_token_id(&self) -> Option<&BytesJson> {
        match self {
            TransactionType::TokenTransfer(token_id) => Some(token_id),
            TransactionType::CustomTendermintMsg { token_id, .. }
            | TransactionType::TendermintIBCTransfer { token_id } => token_id.as_ref(),
            TransactionType::StakingDelegation
            | TransactionType::RemoveDelegation
            | TransactionType::ClaimDelegationRewards
            | TransactionType::StandardTransfer
            | TransactionType::SiaV1Transaction
            | TransactionType::SiaV2Transaction
            | TransactionType::SiaMinerPayout
            | TransactionType::FeeForTokenTx => None,
        }
    }
}

impl Default for TransactionType {
    fn default() -> Self { TransactionType::StandardTransfer }
}
/// Transaction details
///
/// `Serialize` is written by hand (see [`TransactionDetailsWire`]): a result serializes exactly as it always did,
/// `tx_hex` and `tx_hash` included even when empty, except the unsigned result of a watch-only Tendermint coin
/// and the result of the TRON gasless rail (`unsigned` is `Some`). Those omit `tx_hex`, and omit `tx_hash` while it
/// is empty (CRD ch.36 R36.14.17, ch.63 R25); the gasless rail fills `tx_hash` once the provider's transaction is
/// known (CRD ch.63 D63.9).
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct TransactionDetails {
    /// Raw bytes of signed transaction, this should be sent as is to `send_raw_transaction_bytes` RPC to broadcast the transaction.
    /// Empty in the unsigned result of a watch-only Tendermint coin (`unsigned` is `Some`, CRD ch.36 R36.14.17), and
    /// omitted from the serialized form there and nowhere else.
    #[serde(default)]
    pub tx_hex: BytesJson,
    /// The signed transaction in a coin's native JSON form, for a coin whose native
    /// serialisation of a transaction is JSON text rather than a binary encoding --
    /// currently Sia only (CRD ch.20 R-W7). It is the same serialisation `tx_hex`
    /// hex-encodes, emitted unencoded, and is omitted entirely for every other coin
    /// family; its absence means "this coin has no JSON carrier", never an error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tx_json: Option<Json>,
    /// Transaction hash in hexadecimal format. Empty and omitted in the unsigned result only (see `tx_hex`); the
    /// result of a TRON gasless withdraw that was submitted and settled carries the hash of the provider's
    /// transaction here (CRD ch.63 D63.9).
    #[serde(default)]
    pub(crate) tx_hash: String,
    /// Coins are sent from these addresses
    pub(crate) from: Vec<String>,
    /// Coins are sent to these addresses
    pub(crate) to: Vec<String>,
    /// Total tx amount
    pub(crate) total_amount: BigDecimal,
    /// The amount spent from "my" address
    pub(crate) spent_by_me: BigDecimal,
    /// The amount received by "my" address
    pub(crate) received_by_me: BigDecimal,
    /// Resulting "my" balance change
    pub(crate) my_balance_change: BigDecimal,
    /// Block height
    pub(crate) block_height: u64,
    /// Transaction timestamp
    pub(crate) timestamp: u64,
    /// Every coin can has specific fee details:
    /// In UTXO tx fee is paid with the coin itself (e.g. 1 BTC and 0.0001 BTC fee).
    /// But for ERC20 token transfer fee is paid with another coin: ETH, because it's ETH smart contract function call that requires gas to be burnt.
    pub(crate) fee_details: Option<TxFeeDetails>,
    /// The coin transaction belongs to
    pub(crate) coin: String,
    /// Internal MM2 id used for internal transaction identification, for some coins it might be equal to transaction hash
    pub(crate) internal_id: BytesJson,
    /// Amount of accrued rewards.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) kmd_rewards: Option<KmdRewardsDetails>,
    /// Type of transactions, default is StandardTransfer
    #[serde(default)]
    pub(crate) transaction_type: TransactionType,
    /// The memo of the transaction, for a coin family whose history rows carry one (Tendermint, CRD ch.36
    /// R36.14.9: always present on a history row, possibly empty). Omitted for every other family.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) memo: Option<String>,
    /// The unsigned document a watch-only Tendermint coin answers instead of a signed transaction (CRD ch.36
    /// R36.14.17), or the off-chain authorization of a TRON gasless withdraw (CRD ch.63 §63.8A). Its members sit
    /// flat in the result; `None` for every signed transaction.
    #[serde(default, flatten)]
    pub(crate) unsigned: Option<UnsignedTransaction>,
}
/// The serialized form of [`TransactionDetails`]: the members in their declared order and under their usual
/// skip rules. `tx_hex` is left out when, and only when, the result is unsigned; `tx_hash` when the result is
/// unsigned and the hash is empty.
#[derive(Serialize)]
struct TransactionDetailsWire<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    tx_hex: Option<&'a BytesJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tx_json: &'a Option<Json>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tx_hash: Option<&'a String>,
    from: &'a Vec<String>,
    to: &'a Vec<String>,
    total_amount: &'a BigDecimal,
    spent_by_me: &'a BigDecimal,
    received_by_me: &'a BigDecimal,
    my_balance_change: &'a BigDecimal,
    block_height: u64,
    timestamp: u64,
    fee_details: &'a Option<TxFeeDetails>,
    coin: &'a String,
    internal_id: &'a BytesJson,
    #[serde(skip_serializing_if = "Option::is_none")]
    kmd_rewards: &'a Option<KmdRewardsDetails>,
    transaction_type: &'a TransactionType,
    #[serde(skip_serializing_if = "Option::is_none")]
    memo: &'a Option<String>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    unsigned: Option<&'a UnsignedTransaction>,
}
impl Serialize for TransactionDetails {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let signed = !self.is_unsigned();
        TransactionDetailsWire {
            tx_hex: signed.then_some(&self.tx_hex),
            tx_json: &self.tx_json,
            tx_hash: (signed || !self.tx_hash.is_empty()).then_some(&self.tx_hash),
            from: &self.from,
            to: &self.to,
            total_amount: &self.total_amount,
            spent_by_me: &self.spent_by_me,
            received_by_me: &self.received_by_me,
            my_balance_change: &self.my_balance_change,
            block_height: self.block_height,
            timestamp: self.timestamp,
            fee_details: &self.fee_details,
            coin: &self.coin,
            internal_id: &self.internal_id,
            kmd_rewards: &self.kmd_rewards,
            transaction_type: &self.transaction_type,
            memo: &self.memo,
            unsigned: self.unsigned.as_ref(),
        }
        .serialize(serializer)
    }
}
/// The sign document of the direct sign mode (CRD ch.36 R36.14.17): the serialized transaction body and auth info,
/// the chain id and the account number of the signer.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct UnsignedSignDoc {
    pub body_bytes: Vec<u8>,
    pub auth_info_bytes: Vec<u8>,
    pub chain_id: String,
    pub account_number: u64,
}
/// What a watch-only Tendermint coin (CRD ch.36 R36.14.17) and the TRON gasless rail (CRD ch.63 §63.8A) answer in
/// place of `tx_hex` and `tx_hash`. The members of the variant sit flat in the withdraw result.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(untagged)]
pub enum UnsignedTransaction {
    /// The direct form, without the Ledger flag.
    SignDoc { sign_doc: UnsignedSignDoc },
    /// The Amino form, with the Ledger flag: the Amino sign document as a JSON object (CRD ch.36 R36.14.31 item 1)
    /// and the protobuf type URL of its message.
    Amino {
        legacy_amino_json: Json,
        original_tx_type_url: String,
    },
    /// The result of the TRON gasless rail (CRD ch.63 §63.8A, D63.8 (a)): a signed off-chain authorization. The
    /// direct `withdraw` method only signs it; the task route also submits it to the provider (CRD ch.63 D63.9).
    TronGasFree(crate::eth::tron::gasfree::authorization::GasFreeRelayPayload),
}
#[derive(Clone, Copy, Debug)]
pub struct BlockHeightAndTime {
    pub(crate) height: u64,
    pub(crate) timestamp: u64,
}
impl TransactionDetails {
    /// Whether this is a result without a transaction: the unsigned result of a watch-only coin, where nothing was
    /// signed or broadcast and the caller's own wallet signs the document (CRD ch.36 R36.14.17), or the result of
    /// the TRON gasless rail, whose authorization is not a transaction (CRD ch.63 R25).
    pub fn is_unsigned(&self) -> bool { self.unsigned.is_some() }

    /// Whether the transaction details block height should be updated (when tx is confirmed)
    pub fn should_update_block_height(&self) -> bool {
        // checking for std::u64::MAX because there was integer overflow
        // in case of electrum returned -1 so there could be records with MAX confirmations
        self.block_height == 0 || self.block_height == std::u64::MAX
    }

    /// Whether the transaction timestamp should be updated (when tx is confirmed)
    pub fn should_update_timestamp(&self) -> bool {
        // checking for std::u64::MAX because there was integer overflow
        // in case of electrum returned -1 so there could be records with MAX confirmations
        self.timestamp == 0
    }

    pub fn should_update_kmd_rewards(&self) -> bool { self.coin == "KMD" && self.kmd_rewards.is_none() }

    pub fn firo_negative_fee(&self) -> bool {
        match &self.fee_details {
            Some(TxFeeDetails::Utxo(utxo)) => utxo.amount < 0.into() && self.coin == "FIRO",
            _ => false,
        }
    }

    pub fn should_update(&self) -> bool {
        self.should_update_block_height()
            || self.should_update_timestamp()
            || self.should_update_kmd_rewards()
            || self.firo_negative_fee()
    }
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TradeFee {
    pub coin: String,
    pub amount: MmNumber,
    pub paid_from_trading_vol: bool,
}
#[derive(Clone, Debug, Default, PartialEq, PartialOrd, Serialize)]
pub struct CoinBalance {
    pub spendable: BigDecimal,
    pub unspendable: BigDecimal,
}
impl CoinBalance {
    pub fn new(spendable: BigDecimal) -> CoinBalance {
        CoinBalance {
            spendable,
            unspendable: BigDecimal::from(0),
        }
    }

    pub fn into_total(self) -> BigDecimal { self.spendable + self.unspendable }

    pub fn get_total(&self) -> BigDecimal { &self.spendable + &self.unspendable }
}
impl Add for CoinBalance {
    type Output = CoinBalance;

    fn add(self, rhs: Self) -> Self::Output {
        CoinBalance {
            spendable: self.spendable + rhs.spendable,
            unspendable: self.unspendable + rhs.unspendable,
        }
    }
}
/// The approximation is needed to cover the dynamic miner fee changing during a swap.
#[derive(Clone, Debug)]
pub enum FeeApproxStage {
    /// Do not increase the trade fee.
    WithoutApprox,
    /// Increase the trade fee slightly.
    StartSwap,
    /// Increase the trade fee significantly.
    OrderIssue,
    /// Increase the trade fee largely.
    TradePreimage,
}
#[derive(Debug)]
pub enum TradePreimageValue {
    Exact(BigDecimal),
    UpperBound(BigDecimal),
}
/// Identifies which kind of swap output is being created, carrying the
/// secret hash(es) needed for the HTLC script.
#[derive(Debug, Clone)]
pub enum SwapTxTypeWithSecretHash<'a> {
    /// V1 taker or maker payment (single secret hash).
    TakerOrMakerPayment { maker_secret_hash: &'a [u8] },
    /// Taker funding output (locked by taker secret).
    TakerFunding { taker_secret_hash: &'a [u8] },
    /// V2 maker payment (dual secret hashes: maker + taker).
    MakerPaymentV2 {
        maker_secret_hash: &'a [u8],
        taker_secret_hash: &'a [u8],
    },
    /// V2 taker payment (dual secret hashes: maker + taker).
    TakerPaymentV2 {
        maker_secret_hash: &'a [u8],
        taker_secret_hash: &'a [u8],
    },
}

impl<'a> SwapTxTypeWithSecretHash<'a> {
    /// Build the redeem script for this swap output, dispatching to the
    /// correct V1 or V2 builder. `first_pub`/`second_pub` are the two
    /// HTLC pubkeys whose ordering matches each variant's builder:
    ///   - `TakerOrMakerPayment`: `pub_0`, `pub_1` (V1 layout)
    ///   - `TakerFunding`        : `taker_pub`, `maker_pub`
    ///   - `TakerPaymentV2`      : `taker_pub`, `maker_pub`
    ///   - `MakerPaymentV2`      : `maker_pub`, `taker_pub`
    pub fn redeem_script(&self, time_lock: u32, first_pub: &keys::Public, second_pub: &keys::Public) -> script::Script {
        use crate::utxo::swap_proto_v2_scripts::{maker_payment_script, taker_funding_script, taker_payment_script};
        use crate::utxo::utxo_common::payment_script;
        match self {
            SwapTxTypeWithSecretHash::TakerOrMakerPayment { maker_secret_hash } => {
                payment_script(time_lock, maker_secret_hash, first_pub, second_pub)
            },
            SwapTxTypeWithSecretHash::TakerFunding { taker_secret_hash } => {
                taker_funding_script(time_lock, taker_secret_hash, first_pub, second_pub)
            },
            SwapTxTypeWithSecretHash::TakerPaymentV2 { maker_secret_hash, .. } => {
                taker_payment_script(time_lock, maker_secret_hash, first_pub, second_pub)
            },
            SwapTxTypeWithSecretHash::MakerPaymentV2 {
                maker_secret_hash,
                taker_secret_hash,
            } => maker_payment_script(time_lock, maker_secret_hash, taker_secret_hash, first_pub, second_pub),
        }
    }
}
/// A preimage bundled with the signature needed to complete the transaction.
#[derive(Debug)]
pub struct TxPreimageWithSig<Coin: ParseCoinAssocTypes + ?Sized> {
    pub preimage: Coin::Preimage,
    pub signature: Coin::Sig,
}
pub type GenPreimageResult<Coin> = MmResult<TxPreimageWithSig<Coin>, TxGenError>;
pub type ValidateSwapV2TxResult = MmResult<(), ValidateSwapV2TxError>;
pub type ValidateTakerFundingSpendPreimageResult = MmResult<(), ValidateTakerFundingSpendPreimageError>;
pub type ValidateTakerPaymentSpendPreimageResult = MmResult<(), ValidateTakerPaymentSpendPreimageError>;
/// Outcome of searching for how the taker funding UTXO was spent.
#[derive(Debug)]
pub enum FundingTxSpend<T: ParseCoinAssocTypes + ?Sized> {
    /// Funding was refunded via the timelock path.
    RefundedTimelock(T::Tx),
    /// Funding was refunded via the taker secret path.
    RefundedSecret { tx: T::Tx, secret: [u8; 32] },
    /// Funding was legitimately spent into the taker payment.
    TransferredToTakerPayment(T::Tx),
}
/// Arguments for sending maker payment (V2 protocol).
pub struct SendMakerPaymentArgs<'a, Coin: ParseCoinAssocTypes + ?Sized> {
    pub time_lock: u64,
    pub taker_secret_hash: &'a [u8],
    pub maker_secret_hash: &'a [u8],
    pub amount: BigDecimal,
    pub taker_pub: &'a Coin::Pubkey,
    pub swap_unique_data: &'a [u8],
}
/// Arguments for validating maker payment (V2 protocol).
pub struct ValidateMakerPaymentArgs<'a, Coin: ParseCoinAssocTypes + ?Sized> {
    pub maker_payment_tx: &'a Coin::Tx,
    pub time_lock: u64,
    pub taker_secret_hash: &'a [u8],
    pub maker_secret_hash: &'a [u8],
    pub amount: BigDecimal,
    pub maker_pub: &'a Coin::Pubkey,
    pub swap_unique_data: &'a [u8],
}
/// Arguments for refunding maker payment via timelock.
pub struct RefundMakerPaymentTimelockArgs<'a> {
    pub payment_tx: &'a [u8],
    pub time_lock: u64,
    pub taker_pub: &'a [u8],
    pub tx_type_with_secret_hash: SwapTxTypeWithSecretHash<'a>,
    pub swap_unique_data: &'a [u8],
    pub watcher_reward: bool,
    pub amount: BigDecimal,
}
/// Arguments for refunding maker payment via taker secret reveal.
pub struct RefundMakerPaymentSecretArgs<'a, Coin: ParseCoinAssocTypes + ?Sized> {
    pub maker_payment_tx: &'a Coin::Tx,
    pub time_lock: u64,
    pub taker_secret_hash: &'a [u8],
    pub maker_secret_hash: &'a [u8],
    pub taker_secret: &'a [u8; 32],
    pub taker_pub: &'a Coin::Pubkey,
    pub swap_unique_data: &'a [u8],
    pub amount: BigDecimal,
}
/// Arguments for spending maker payment (taker side — reveals maker secret).
pub struct SpendMakerPaymentArgs<'a, Coin: ParseCoinAssocTypes + ?Sized> {
    pub maker_payment_tx: &'a Coin::Tx,
    pub time_lock: u64,
    pub taker_secret_hash: &'a [u8],
    pub maker_secret_hash: &'a [u8],
    pub maker_secret: [u8; 32],
    pub maker_pub: &'a Coin::Pubkey,
    pub swap_unique_data: &'a [u8],
    pub amount: BigDecimal,
}
/// Arguments for sending taker funding.
pub struct SendTakerFundingArgs<'a> {
    pub funding_time_lock: u64,
    pub payment_time_lock: u64,
    pub taker_secret_hash: &'a [u8],
    pub maker_secret_hash: &'a [u8],
    pub maker_pub: &'a [u8],
    pub dex_fee: &'a DexFee,
    pub premium_amount: BigDecimal,
    pub trading_amount: BigDecimal,
    pub swap_unique_data: &'a [u8],
}
/// Arguments for validating taker funding (maker side).
pub struct ValidateTakerFundingArgs<'a, Coin: ParseCoinAssocTypes + ?Sized> {
    pub funding_tx: &'a Coin::Tx,
    pub funding_time_lock: u64,
    pub payment_time_lock: u64,
    pub taker_secret_hash: &'a [u8],
    pub maker_secret_hash: &'a [u8],
    pub taker_pub: &'a Coin::Pubkey,
    pub dex_fee: &'a DexFee,
    pub premium_amount: BigDecimal,
    pub trading_amount: BigDecimal,
    pub swap_unique_data: &'a [u8],
}
/// Arguments for refunding taker payment/funding via timelock.
pub struct RefundTakerPaymentArgs<'a> {
    pub payment_tx: &'a [u8],
    pub time_lock: u64,
    pub maker_pub: &'a [u8],
    pub tx_type_with_secret_hash: SwapTxTypeWithSecretHash<'a>,
    pub swap_unique_data: &'a [u8],
    pub watcher_reward: bool,
    pub dex_fee: &'a DexFee,
    pub premium_amount: BigDecimal,
    pub trading_amount: BigDecimal,
}
/// Arguments for refunding taker funding via taker secret reveal.
pub struct RefundFundingSecretArgs<'a, Coin: ParseCoinAssocTypes + ?Sized> {
    pub funding_tx: &'a Coin::Tx,
    pub funding_time_lock: u64,
    pub payment_time_lock: u64,
    pub maker_pubkey: &'a Coin::Pubkey,
    pub taker_secret: &'a [u8; 32],
    pub taker_secret_hash: &'a [u8],
    pub maker_secret_hash: &'a [u8],
    pub dex_fee: &'a DexFee,
    pub premium_amount: BigDecimal,
    pub trading_amount: BigDecimal,
    pub swap_unique_data: &'a [u8],
    pub watcher_reward: bool,
}
/// Arguments for generating the taker funding → taker-payment spend preimage.
pub struct GenTakerFundingSpendArgs<'a, Coin: ParseCoinAssocTypes + ?Sized> {
    pub funding_tx: &'a Coin::Tx,
    pub maker_pub: &'a Coin::Pubkey,
    pub taker_pub: &'a Coin::Pubkey,
    pub funding_time_lock: u64,
    pub taker_secret_hash: &'a [u8],
    pub taker_payment_time_lock: u64,
    pub maker_secret_hash: &'a [u8],
}
/// Arguments for generating/spending the taker payment.
pub struct GenTakerPaymentSpendArgs<'a, Coin: ParseCoinAssocTypes + ?Sized> {
    pub taker_tx: &'a Coin::Tx,
    pub time_lock: u64,
    pub maker_secret_hash: &'a [u8],
    pub maker_pub: &'a Coin::Pubkey,
    pub maker_address: &'a Coin::Address,
    pub taker_pub: &'a Coin::Pubkey,
    pub dex_fee: &'a DexFee,
    pub premium_amount: BigDecimal,
    pub trading_amount: BigDecimal,
    /// Raw bytes of the active network's DEX-fee-collection public key (CRD
    /// ch.16 R14 / chapter 08 R16 finding). The version-two taker-payment-spend
    /// fee-collection output MUST be derived from this, not from a
    /// network-fixed constant.
    pub dex_fee_addr_raw_pubkey: &'a [u8],
}
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "state", content = "additional_info")]
pub enum HistorySyncState {
    NotEnabled,
    NotStarted,
    InProgress(Json),
    Error(Json),
    Finished,
    /// A finished pass of a UTXO history collector that skipped transactions it could not read (CRD ch.39
    /// D39.11.4V.1). It serializes with the state name `Finished`, and its `additional_info` is the object
    /// `{"unreadable_transactions": [...]}`. A pass that skipped nothing is [`HistorySyncState::Finished`], which has
    /// no `additional_info`.
    #[serde(rename = "Finished")]
    FinishedWithUnreadable(Json),
}

/// A transaction that a UTXO history collector skipped in the current pass because it cannot be read (CRD ch.39
/// D39.11.4V.1, R38.5A.9). The list of them lives in memory only and is rebuilt on every pass.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct UnreadableTransaction {
    /// The transaction hash as the history rows show it.
    pub tx_hash: String,
    /// A short fixed reason. It never carries a transaction body, hex or key.
    pub error: String,
}

impl HistorySyncState {
    /// `InProgress` of a UTXO history collector: `transactions_left`, and the `unreadable_transactions` member only
    /// when the pass has skipped a transaction. With nothing skipped the answer is exactly `{"transactions_left": n}`.
    pub(crate) fn utxo_in_progress(transactions_left: usize, unreadable: &[UnreadableTransaction]) -> Self {
        let mut info = json!({ "transactions_left": transactions_left });
        if !unreadable.is_empty() {
            info["unreadable_transactions"] = json!(unreadable);
        }
        HistorySyncState::InProgress(info)
    }

    /// The finished state of a UTXO history collector: plain `Finished` when the pass skipped nothing.
    pub(crate) fn utxo_finished(unreadable: &[UnreadableTransaction]) -> Self {
        if unreadable.is_empty() {
            HistorySyncState::Finished
        } else {
            HistorySyncState::FinishedWithUnreadable(json!({ "unreadable_transactions": unreadable }))
        }
    }

    /// The running `InProgress` state with the `unreadable_transactions` member set, so that a skip shows at once;
    /// `None` for any other state (a pass that is not running has no `transactions_left` to keep).
    pub(crate) fn in_progress_with_unreadable(&self, unreadable: &[UnreadableTransaction]) -> Option<Self> {
        let HistorySyncState::InProgress(info) = self else {
            return None;
        };
        let mut info = info.clone();
        info.as_object_mut()?
            .insert("unreadable_transactions".to_owned(), json!(unreadable));
        Some(HistorySyncState::InProgress(info))
    }

    /// Whether the history is finished, with or without skipped transactions.
    pub fn is_finished(&self) -> bool {
        matches!(
            self,
            HistorySyncState::Finished | HistorySyncState::FinishedWithUnreadable(_)
        )
    }
}

#[cfg(test)]
mod tx_fee_details_tests {
    //! Regression coverage for `TxFeeDetails`'s hand-rolled untagged
    //! `Deserialize`: every `TxFeeDetails` arm must round-trip through it, or
    //! a single such entry in a saved history file makes the whole file
    //! (a JSON array shared by every tx of that coin) fail to load -- which
    //! `load_history_from_file_impl` then treats as corruption and silently
    //! wipes, on every read, forever (this is exactly what happened to SC).
    use super::*;

    fn roundtrip(details: TxFeeDetails) {
        let serialized = json::to_string(&details).expect("TxFeeDetails always serializes");
        let deserialized: TxFeeDetails =
            json::from_str(&serialized).unwrap_or_else(|e| panic!("{} failed to deserialize back: {}", serialized, e));
        assert_eq!(details, deserialized, "serialized as: {}", serialized);
    }

    #[test]
    fn test_sia_fee_details_roundtrip() {
        roundtrip(TxFeeDetails::Sia(siacoin::SiaFeeDetails {
            coin: "SC".to_owned(),
            policy: siacoin::SiaFeePolicy::Unknown,
            total_amount: "0.001".parse().unwrap(),
        }));
    }

    /// CRD ch.63 §63.3.5: the gasless fee details read back as themselves, with `trace_id` written as `null`.
    #[test]
    fn test_tron_gasless_fee_details_roundtrip() {
        use crate::eth::tron::fee::{GaslessFeeMethod, GaslessProviderName, TronGaslessFeeDetails};
        let details = TxFeeDetails::TronGasless(TronGaslessFeeDetails {
            coin: "USDT-TRC20".to_owned(),
            fee_method: GaslessFeeMethod::Gasless,
            provider_name: GaslessProviderName::GasFree,
            gasfree_address: "TUGC4eNuEgbaLotwxzzXEck1fRWru6n8ye".to_owned(),
            transfer_fee: "2".parse().unwrap(),
            activation_fee: "0".parse().unwrap(),
            total_token_fee: "2".parse().unwrap(),
            signed_max_fee: Some("2.5".parse().unwrap()),
            trace_id: None,
        });
        let document = json::to_value(&details).unwrap();
        assert_eq!(document["type"], "TronGasless");
        assert_eq!(document["fee_method"], "gasless");
        assert_eq!(document["provider_name"], "gasfree");
        assert!(document.as_object().unwrap().contains_key("trace_id") && document["trace_id"].is_null());
        roundtrip(details);
    }

    #[test]
    fn test_tendermint_fee_details_roundtrip() {
        roundtrip(TxFeeDetails::Tendermint(tendermint::TendermintFeeDetails {
            coin: "ATOM".to_owned(),
            amount: "0.002".parse().unwrap(),
            uamount: 0,
            gas_limit: 100000,
        }));
    }

    fn solana_details() -> SolanaFeeDetails {
        SolanaFeeDetails::new("0.000005".parse().unwrap(), "0.00203928".parse().unwrap())
    }

    /// T40.6B.13: the Solana object is exactly the tag and the three members, the last three decimal strings, and
    /// the total is the sum.
    #[test]
    fn test_solana_fee_details_wire_shape() {
        let details = TxFeeDetails::Solana(solana_details());
        let wire: Json = json::to_value(&details).unwrap();
        let members: std::collections::BTreeSet<&str> = wire.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            members,
            ["type", "fee_amount", "rent_amount", "total_amount"]
                .into_iter()
                .collect()
        );
        assert_eq!(wire["type"], "Solana");
        assert_eq!(wire["fee_amount"], "0.000005");
        assert_eq!(wire["rent_amount"], "0.00203928");
        assert_eq!(wire["total_amount"], "0.00204428");
        let no_rent = json::to_value(TxFeeDetails::Solana(SolanaFeeDetails::new(
            "0.000005".parse().unwrap(),
            0.into(),
        )))
        .unwrap();
        assert_eq!(no_rent["rent_amount"], "0");
        assert_eq!(no_rent["total_amount"], "0.000005");
    }

    /// T40.6B.13: the untagged reader reads the new shape back as Solana, with and without rent.
    #[test]
    fn test_solana_fee_details_roundtrip() {
        roundtrip(TxFeeDetails::Solana(solana_details()));
        roundtrip(TxFeeDetails::Solana(SolanaFeeDetails::new(
            "0.000005".parse().unwrap(),
            0.into(),
        )));
    }

    /// T40.6B.13: every other family's sample still reads as its own family, and none of them as Solana.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn test_other_families_are_not_read_as_solana() {
        roundtrip(TxFeeDetails::Utxo(UtxoFeeDetails {
            coin: None,
            amount: "0.0001".parse().unwrap(),
        }));
        roundtrip(TxFeeDetails::Eth(EthTxFeeDetails {
            coin: "ETH".to_owned(),
            gas: 21000,
            gas_price: "20".parse().unwrap(),
            total_fee: "0.00042".parse().unwrap(),
        }));
        roundtrip(TxFeeDetails::Qrc20(Qrc20FeeDetails {
            coin: "QTUM".to_owned(),
            miner_fee: "0.0001".parse().unwrap(),
            gas_limit: 100000,
            gas_price: 40,
            total_gas_fee: "0.04".parse().unwrap(),
        }));
        roundtrip(TxFeeDetails::Tron(crate::eth::tron::fee::TronTxFeeDetails {
            coin: "TRX".to_owned(),
            bandwidth_used: 267,
            energy_used: 0,
            bandwidth_fee: "0.267".parse().unwrap(),
            energy_fee: 0.into(),
            total_fee: "1.267".parse().unwrap(),
            account_creation_fee: Some(1.into()),
        }));
        roundtrip(TxFeeDetails::Tron(crate::eth::tron::fee::TronTxFeeDetails {
            coin: "TRX".to_owned(),
            bandwidth_used: 345,
            energy_used: 14_000,
            bandwidth_fee: 0.into(),
            energy_fee: "5.88".parse().unwrap(),
            total_fee: "5.88".parse().unwrap(),
            account_creation_fee: None,
        }));
        roundtrip(TxFeeDetails::Sia(siacoin::SiaFeeDetails {
            coin: "SC".to_owned(),
            policy: siacoin::SiaFeePolicy::Fixed,
            total_amount: "0.001".parse().unwrap(),
        }));
        roundtrip(TxFeeDetails::Tendermint(tendermint::TendermintFeeDetails {
            coin: "ATOM".to_owned(),
            amount: "0.002".parse().unwrap(),
            uamount: 0,
            gas_limit: 100000,
        }));
        // A Sia object next to a Solana-looking `total_amount`: it is still Sia.
        let sia: TxFeeDetails =
            json::from_str(r#"{"type":"Sia","coin":"SC","policy":"Fixed","total_amount":"0.001"}"#).unwrap();
        assert!(matches!(sia, TxFeeDetails::Sia(_)));
    }

    /// A real Utxo entry (no `gas_limit`) must keep deserializing as `Utxo`
    /// now that `Tendermint` -- whose shape is a strict superset of Utxo's --
    /// is tried first.
    #[test]
    fn test_utxo_fee_details_roundtrip_unaffected_by_tendermint_reordering() {
        roundtrip(TxFeeDetails::Utxo(UtxoFeeDetails {
            coin: Some("RIN".to_owned()),
            amount: "0.0001".parse().unwrap(),
        }));
    }
}

#[cfg(test)]
mod transaction_details_wire_tests {
    //! The serialized form of `TransactionDetails` for every result that is not the unsigned result of a watch-only
    //! Tendermint coin is the one it always was (CRD ch.36 R36.14.17): the members in their declared order, `tx_hex`
    //! and `tx_hash` present even when empty, and a row stored before the `unsigned` member existed reads and
    //! writes back byte for byte.
    use super::*;

    const STORED_BCH_ROW: &str = r#"{"tx_hex":"0400","tx_hash":"f05d786bd4b647a5720094bf0a2c6f23b5e131c451d750a96102898f7b5458e8","from":["bchtest:from"],"to":["bchtest:to"],"total_amount":"1.5","spent_by_me":"1.5","received_by_me":"0","my_balance_change":"-1.5","block_height":100,"timestamp":200,"fee_details":{"type":"Utxo","coin":null,"amount":"0.00001"},"coin":"DOC","internal_id":"f05d786bd4b647a5720094bf0a2c6f23b5e131c451d750a96102898f7b5458e8","transaction_type":{"TokenTransfer":"aabb"}}"#;

    const EMPTY_HEX_ROW: &str = r#"{"tx_hex":"","tx_hash":"","from":[],"to":[],"total_amount":"0","spent_by_me":"0","received_by_me":"0","my_balance_change":"0","block_height":0,"timestamp":0,"fee_details":null,"coin":"SOL","internal_id":"","transaction_type":"StandardTransfer"}"#;

    #[test]
    fn a_stored_bch_row_with_a_token_transfer_round_trips_byte_for_byte() {
        let row: TransactionDetails = json::from_str(STORED_BCH_ROW).expect("a stored row deserializes");
        assert!(!row.is_unsigned());
        assert!(matches!(row.transaction_type, TransactionType::TokenTransfer(_)));
        assert!(row.fee_details.is_some());
        assert_eq!(json::to_string(&row).expect("a row serializes"), STORED_BCH_ROW);
    }

    #[test]
    fn a_row_with_an_empty_hex_and_hash_keeps_both_members() {
        // A Solana-style history row is built with an empty `tx_hex`; the member stays on the wire, as before.
        let row: TransactionDetails = json::from_str(EMPTY_HEX_ROW).expect("a row deserializes");
        assert!(!row.is_unsigned() && row.tx_hex.is_empty() && row.tx_hash.is_empty());
        let wire = json::to_string(&row).expect("a row serializes");
        assert_eq!(wire, EMPTY_HEX_ROW);
        assert!(wire.contains(r#""tx_hex":"""#) && wire.contains(r#""tx_hash":"""#));
    }

    #[test]
    fn only_an_unsigned_result_omits_the_two_members() {
        let mut row: TransactionDetails = json::from_str(EMPTY_HEX_ROW).expect("a row deserializes");
        row.unsigned = Some(UnsignedTransaction::Amino {
            legacy_amino_json: json!({}),
            original_tx_type_url: "/x".to_owned(),
        });
        let wire: Json = json::to_value(&row).expect("a row serializes");
        assert!(wire.get("tx_hex").is_none() && wire.get("tx_hash").is_none(), "{wire}");
        assert_eq!(wire["legacy_amino_json"], json!({}));
        assert_eq!(wire["original_tx_type_url"], "/x");
        assert!(wire.get("internal_id").is_some());
    }

    /// CRD ch.63 D63.9: an unsigned result that carries a hash writes `tx_hash` and still omits `tx_hex`.
    #[test]
    fn an_unsigned_result_with_a_hash_writes_the_hash_and_omits_the_hex() {
        let mut row: TransactionDetails = json::from_str(EMPTY_HEX_ROW).expect("a row deserializes");
        row.unsigned = Some(UnsignedTransaction::Amino {
            legacy_amino_json: json!({}),
            original_tx_type_url: "/x".to_owned(),
        });
        row.tx_hash = "a1b2".to_owned();
        let wire: Json = json::to_value(&row).expect("a row serializes");
        assert_eq!(wire["tx_hash"], "a1b2");
        assert!(wire.get("tx_hex").is_none(), "{wire}");
    }
}

#[cfg(test)]
mod history_sync_state_tests {
    use super::*;

    fn skipped(hash: &str, error: &str) -> UnreadableTransaction {
        UnreadableTransaction {
            tx_hash: hash.to_owned(),
            error: error.to_owned(),
        }
    }

    #[test]
    fn finished_without_a_skip_is_exactly_the_state_member() {
        let wire = json::to_string(&HistorySyncState::utxo_finished(&[])).unwrap();
        assert_eq!(wire, r#"{"state":"Finished"}"#);
        assert_eq!(
            json::to_string(&HistorySyncState::Finished).unwrap(),
            r#"{"state":"Finished"}"#
        );
    }

    #[test]
    fn in_progress_without_a_skip_is_exactly_the_old_answer() {
        let wire = json::to_string(&HistorySyncState::utxo_in_progress(3, &[])).unwrap();
        assert_eq!(
            wire,
            r#"{"state":"InProgress","additional_info":{"transactions_left":3}}"#
        );
        let old = json::to_string(&HistorySyncState::InProgress(json!({ "transactions_left": 3 }))).unwrap();
        assert_eq!(wire, old);
    }

    #[test]
    fn finished_with_a_skip_carries_the_list_under_the_finished_state() {
        let state = HistorySyncState::utxo_finished(&[skipped("ab", "unreadable")]);
        assert!(state.is_finished());
        assert_eq!(
            json::to_value(&state).unwrap(),
            json!({
                "state": "Finished",
                "additional_info": { "unreadable_transactions": [{ "tx_hash": "ab", "error": "unreadable" }] },
            })
        );
    }

    #[test]
    fn a_running_state_gains_the_list_and_keeps_transactions_left() {
        let running = HistorySyncState::InProgress(json!({ "transactions_left": 4 }));
        let next = running
            .in_progress_with_unreadable(&[skipped("ab", "unreadable")])
            .expect("a running state takes the list");
        assert_eq!(
            json::to_value(&next).unwrap(),
            json!({
                "state": "InProgress",
                "additional_info": {
                    "transactions_left": 4,
                    "unreadable_transactions": [{ "tx_hash": "ab", "error": "unreadable" }],
                },
            })
        );
        assert!(HistorySyncState::Finished
            .in_progress_with_unreadable(&[skipped("ab", "unreadable")])
            .is_none());
        assert!(HistorySyncState::NotStarted
            .in_progress_with_unreadable(&[skipped("ab", "unreadable")])
            .is_none());
    }

    #[test]
    fn in_progress_with_a_skip_has_the_list_beside_transactions_left() {
        let state = HistorySyncState::utxo_in_progress(2, &[skipped("ab", "unreadable")]);
        assert!(!state.is_finished());
        assert_eq!(
            json::to_value(&state).unwrap(),
            json!({
                "state": "InProgress",
                "additional_info": {
                    "transactions_left": 2,
                    "unreadable_transactions": [{ "tx_hash": "ab", "error": "unreadable" }],
                },
            })
        );
    }
}
