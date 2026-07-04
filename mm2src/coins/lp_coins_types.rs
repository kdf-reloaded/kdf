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
}
ifrom!(TransactionEnum, UtxoTx);
ifrom!(TransactionEnum, SignedEthTx);
#[cfg(not(target_arch = "wasm32"))]
ifrom!(TransactionEnum, ZTransaction);
impl From<siacoin::SiaTransaction> for TransactionEnum {
    fn from(t: siacoin::SiaTransaction) -> TransactionEnum { TransactionEnum::SiaTransaction(t) }
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
}
impl TransactionErr {
    /// Returns transaction if the error includes it.
    #[inline]
    pub fn get_tx(&self) -> Option<TransactionEnum> {
        match self {
            TransactionErr::TxRecoverable(tx, _) => Some(tx.clone()),
            _ => None,
        }
    }

    #[inline]
    /// Returns plain text part of error.
    pub fn get_plain_text_format(&self) -> String {
        match self {
            TransactionErr::TxRecoverable(_, err) => err.to_string(),
            TransactionErr::Plain(err) => err.to_string(),
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
    /// No fee required (taker is the fee pubkey itself — rare edge case).
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

/// KMD path: total fee goes into a zero-value `OP_RETURN`. The
/// `min_tx_amount` dust check applies only to the fee itself; if the entire
/// fee is dust, fall back to `Standard` so the trade can still proceed.
pub fn calc_dex_fee_for_op_return(fee: MmNumber, min_tx_amount: MmNumber) -> DexFee {
    if fee < min_tx_amount {
        return DexFee::Standard(fee);
    }
    DexFee::WithBurn {
        fee_amount: MmNumber::from(0),
        burn_amount: fee,
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
    if burn_part < min_tx_amount || fee_part < min_tx_amount {
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
        if !net_cfg.burn_enabled() || !taker_coin.should_burn_dex_fee() {
            return DexFee::Standard(base_fee);
        }
        let min_tx_amount = MmNumber::from(taker_coin.min_tx_amount());
        if taker_coin.should_burn_directly() {
            return calc_dex_fee_for_op_return(base_fee, min_tx_amount);
        }
        let burn_pubkey = match taker_coin.burn_pubkey() {
            ref v if !v.is_empty() => v.clone(),
            _ => net_cfg.burn_addr_raw_pubkey().to_vec(),
        };
        let fee_share: MmNumber = net_cfg.dex_fee_share().into();
        calc_dex_fee_for_burn_account(base_fee, min_tx_amount, fee_share, burn_pubkey)
    }

    /// Validation-time variant. Returns `NoFee` when the taker is the burn
    /// pubkey itself (it is not charged a fee on its own trades). Otherwise
    /// delegates to `new_from_taker_coin`.
    pub fn new_with_taker_pubkey(
        taker_coin: &dyn MmCoin,
        net_cfg: &dyn mm2_net_config::NetConfig,
        base_fee: MmNumber,
        taker_pubkey: &[u8],
    ) -> DexFee {
        let burn_pubkey = match taker_coin.burn_pubkey() {
            ref v if !v.is_empty() => v.clone(),
            _ => net_cfg.burn_addr_raw_pubkey().to_vec(),
        };
        if !burn_pubkey.is_empty() && burn_pubkey.as_slice() == taker_pubkey {
            return DexFee::NoFee;
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
        }
    }
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
    #[cfg(not(target_arch = "wasm32"))]
    Solana(SolanaFeeDetails),
    Sia(siacoin::SiaFeeDetails),
    Tendermint(tendermint::TendermintFeeDetails),
    /// TRON fee breakdown: bandwidth + energy + optional activation.
    Tron(crate::eth::tron::fee::TronTxFeeDetails),
}
/// Deserialize the TxFeeDetails as an untagged enum.
impl<'de> Deserialize<'de> for TxFeeDetails {
    fn deserialize<D>(deserializer: D) -> Result<Self, <D as Deserializer<'de>>::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum TxFeeDetailsUnTagged {
            Utxo(UtxoFeeDetails),
            Eth(EthTxFeeDetails),
            Qrc20(Qrc20FeeDetails),
            #[cfg(not(target_arch = "wasm32"))]
            Solana(SolanaFeeDetails),
            Tron(crate::eth::tron::fee::TronTxFeeDetails),
        }

        match Deserialize::deserialize(deserializer)? {
            TxFeeDetailsUnTagged::Utxo(f) => Ok(TxFeeDetails::Utxo(f)),
            TxFeeDetailsUnTagged::Eth(f) => Ok(TxFeeDetails::Eth(f)),
            TxFeeDetailsUnTagged::Qrc20(f) => Ok(TxFeeDetails::Qrc20(f)),
            #[cfg(not(target_arch = "wasm32"))]
            TxFeeDetailsUnTagged::Solana(f) => Ok(TxFeeDetails::Solana(f)),
            TxFeeDetailsUnTagged::Tron(f) => Ok(TxFeeDetails::Tron(f)),
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
impl From<UtxoFeeDetails> for TxFeeDetails {
    fn from(utxo_details: UtxoFeeDetails) -> Self { TxFeeDetails::Utxo(utxo_details) }
}
impl From<Qrc20FeeDetails> for TxFeeDetails {
    fn from(qrc20_details: Qrc20FeeDetails) -> Self { TxFeeDetails::Qrc20(qrc20_details) }
}
#[cfg(not(target_arch = "wasm32"))]
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
}
impl Default for TransactionType {
    fn default() -> Self { TransactionType::StandardTransfer }
}
/// Transaction details
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TransactionDetails {
    /// Raw bytes of signed transaction, this should be sent as is to `send_raw_transaction_bytes` RPC to broadcast the transaction
    pub tx_hex: BytesJson,
    /// Transaction hash in hexadecimal format
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
}
#[derive(Clone, Copy, Debug)]
pub struct BlockHeightAndTime {
    pub(crate) height: u64,
    pub(crate) timestamp: u64,
}
impl TransactionDetails {
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
}
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "state", content = "additional_info")]
pub enum HistorySyncState {
    NotEnabled,
    NotStarted,
    InProgress(Json),
    Error(Json),
    Finished,
}
