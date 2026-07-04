// qrc20_types — Constants, structs, enums, error types, and utility functions.

use super::*;

/// Qtum amount is always 0 for the QRC20 UTXO outputs,
/// because we should pay only a fee in Qtum to send the QRC20 transaction.
pub const OUTPUT_QTUM_AMOUNT: u64 = 0;
pub const QRC20_GAS_LIMIT_DEFAULT: u64 = 100_000;
pub(crate) const QRC20_PAYMENT_GAS_LIMIT: u64 = 200_000;
pub const QRC20_GAS_PRICE_DEFAULT: u64 = 40;
pub const QRC20_DUST: u64 = 0;
// Keccak-256 hash of `Transfer` event
pub(crate) const QRC20_TRANSFER_TOPIC: &str = "ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";
pub(crate) const QRC20_PAYMENT_SENT_TOPIC: &str = "ccc9c05183599bd3135da606eaaf535daffe256e9de33c048014cffcccd4ad57";
pub(crate) const QRC20_RECEIVER_SPENT_TOPIC: &str = "36c177bcb01c6d568244f05261e2946c8c977fa50822f3fa098c470770ee1f3e";
pub(crate) const QRC20_SENDER_REFUNDED_TOPIC: &str = "1797d500133f8e427eb9da9523aa4a25cb40f50ebc7dbda3c7c81778973f35ba";

pub type Qrc20AbiResult<T> = Result<T, MmError<Qrc20AbiError>>;

#[derive(Display)]
pub enum Qrc20GenTxError {
    ErrorGeneratingUtxoTx(GenerateTxError),
    ErrorSigningTx(UtxoSignWithKeyPairError),
    PrivKeyNotAllowed(PrivKeyNotAllowed),
    UnexpectedDerivationMethod(UnexpectedDerivationMethod),
}

impl From<GenerateTxError> for Qrc20GenTxError {
    fn from(e: GenerateTxError) -> Self { Qrc20GenTxError::ErrorGeneratingUtxoTx(e) }
}

impl From<UtxoSignWithKeyPairError> for Qrc20GenTxError {
    fn from(e: UtxoSignWithKeyPairError) -> Self { Qrc20GenTxError::ErrorSigningTx(e) }
}

impl From<PrivKeyNotAllowed> for Qrc20GenTxError {
    fn from(e: PrivKeyNotAllowed) -> Self { Qrc20GenTxError::PrivKeyNotAllowed(e) }
}

impl From<UnexpectedDerivationMethod> for Qrc20GenTxError {
    fn from(e: UnexpectedDerivationMethod) -> Self { Qrc20GenTxError::UnexpectedDerivationMethod(e) }
}

impl From<UtxoRpcError> for Qrc20GenTxError {
    fn from(e: UtxoRpcError) -> Self { Qrc20GenTxError::ErrorGeneratingUtxoTx(GenerateTxError::from(e)) }
}

impl Qrc20GenTxError {
    pub(crate) fn into_withdraw_error(self, coin: String, decimals: u8) -> WithdrawError {
        match self {
            Qrc20GenTxError::ErrorGeneratingUtxoTx(gen_err) => {
                WithdrawError::from_generate_tx_error(gen_err, coin, decimals)
            },
            Qrc20GenTxError::ErrorSigningTx(sign_err) => WithdrawError::InternalError(sign_err.to_string()),
            Qrc20GenTxError::PrivKeyNotAllowed(priv_err) => WithdrawError::InternalError(priv_err.to_string()),
            Qrc20GenTxError::UnexpectedDerivationMethod(addr_err) => WithdrawError::InternalError(addr_err.to_string()),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Qrc20ActivationParams {
    pub(crate) swap_contract_address: H160,
    pub(crate) fallback_swap_contract: Option<H160>,
    #[serde(flatten)]
    pub(crate) utxo_params: UtxoActivationParams,
}

#[derive(Debug, Display)]
pub enum Qrc20FromLegacyReqErr {
    InvalidSwapContractAddr(json::Error),
    InvalidFallbackSwapContract(json::Error),
    InvalidUtxoParams(UtxoFromLegacyReqErr),
}

impl From<UtxoFromLegacyReqErr> for Qrc20FromLegacyReqErr {
    fn from(err: UtxoFromLegacyReqErr) -> Self { Qrc20FromLegacyReqErr::InvalidUtxoParams(err) }
}

impl Qrc20ActivationParams {
    pub fn from_legacy_req(req: &Json) -> Result<Self, MmError<Qrc20FromLegacyReqErr>> {
        let swap_contract_address = json::from_value(req["swap_contract_address"].clone())
            .map_to_mm(Qrc20FromLegacyReqErr::InvalidSwapContractAddr)?;
        let fallback_swap_contract = json::from_value(req["fallback_swap_contract"].clone())
            .map_to_mm(Qrc20FromLegacyReqErr::InvalidFallbackSwapContract)?;
        let utxo_params = UtxoActivationParams::from_legacy_req(req).mm_err(Into::into)?;
        Ok(Qrc20ActivationParams {
            swap_contract_address,
            fallback_swap_contract,
            utxo_params,
        })
    }
}

#[derive(Debug)]
pub struct Qrc20CoinFields {
    pub utxo: UtxoCoinFields,
    pub platform: String,
    pub contract_address: H160,
    pub swap_contract_address: H160,
    pub fallback_swap_contract: Option<H160>,
}

#[derive(Clone, Debug)]
pub struct Qrc20Coin(pub(crate) Arc<Qrc20CoinFields>);

impl Deref for Qrc20Coin {
    type Target = Qrc20CoinFields;
    fn deref(&self) -> &Qrc20CoinFields { &*self.0 }
}

impl AsRef<UtxoCoinFields> for Qrc20Coin {
    fn as_ref(&self) -> &UtxoCoinFields { &self.utxo }
}

impl qtum::QtumBasedCoin for Qrc20Coin {}

#[derive(Clone, Debug, PartialEq)]
pub struct ContractCallOutput {
    pub value: u64,
    pub script_pubkey: ScriptBytes,
    pub gas_limit: u64,
    pub gas_price: u64,
}

impl From<ContractCallOutput> for TransactionOutput {
    fn from(out: ContractCallOutput) -> Self {
        TransactionOutput {
            value: out.value,
            script_pubkey: out.script_pubkey,
        }
    }
}

/// Functions of ERC20/EtomicSwap smart contracts that may change the blockchain state.
#[derive(Debug, Eq, PartialEq)]
pub enum MutContractCallType {
    Transfer,
    Erc20Payment,
    ReceiverSpend,
    SenderRefund,
}

impl MutContractCallType {
    pub(crate) fn as_function_name(&self) -> &'static str {
        match self {
            MutContractCallType::Transfer => "transfer",
            MutContractCallType::Erc20Payment => "erc20Payment",
            MutContractCallType::ReceiverSpend => "receiverSpend",
            MutContractCallType::SenderRefund => "senderRefund",
        }
    }

    pub(crate) fn as_function(&self) -> &'static Function {
        match self {
            MutContractCallType::Transfer => eth::ERC20_CONTRACT.function(self.as_function_name()).unwrap(),
            MutContractCallType::Erc20Payment
            | MutContractCallType::ReceiverSpend
            | MutContractCallType::SenderRefund => eth::SWAP_CONTRACT.function(self.as_function_name()).unwrap(),
        }
    }

    pub fn from_script_pubkey(script: &[u8]) -> Result<Option<MutContractCallType>, String> {
        lazy_static! {
            static ref TRANSFER_SHORT_SIGN: [u8; 4] =
                eth::ERC20_CONTRACT.function("transfer").unwrap().short_signature();
            static ref ERC20_PAYMENT_SHORT_SIGN: [u8; 4] =
                eth::SWAP_CONTRACT.function("erc20Payment").unwrap().short_signature();
            static ref RECEIVER_SPEND_SHORT_SIGN: [u8; 4] =
                eth::SWAP_CONTRACT.function("receiverSpend").unwrap().short_signature();
            static ref SENDER_REFUND_SHORT_SIGN: [u8; 4] =
                eth::SWAP_CONTRACT.function("senderRefund").unwrap().short_signature();
        }

        if script.len() < 4 {
            return ERR!("Length of the script pubkey less than 4: {:?}", script);
        }

        if script.starts_with(TRANSFER_SHORT_SIGN.as_ref()) {
            return Ok(Some(MutContractCallType::Transfer));
        }
        if script.starts_with(ERC20_PAYMENT_SHORT_SIGN.as_ref()) {
            return Ok(Some(MutContractCallType::Erc20Payment));
        }
        if script.starts_with(RECEIVER_SPEND_SHORT_SIGN.as_ref()) {
            return Ok(Some(MutContractCallType::ReceiverSpend));
        }
        if script.starts_with(SENDER_REFUND_SHORT_SIGN.as_ref()) {
            return Ok(Some(MutContractCallType::SenderRefund));
        }
        Ok(None)
    }

    #[allow(dead_code)]
    pub(crate) fn short_signature(&self) -> [u8; 4] { self.as_function().short_signature() }
}

pub struct GenerateQrc20TxResult {
    pub signed: UtxoTx,
    pub miner_fee: u64,
    pub gas_fee: u64,
}

#[derive(Debug, Display)]
pub enum Qrc20AbiError {
    #[display(fmt = "Invalid QRC20 ABI params: {}", _0)]
    InvalidParams(String),
    #[display(fmt = "QRC20 ABI error: {}", _0)]
    AbiError(String),
}

impl From<crate::eth::abi::AbiError> for Qrc20AbiError {
    fn from(e: crate::eth::abi::AbiError) -> Qrc20AbiError { Qrc20AbiError::AbiError(e.to_string()) }
}

impl From<Qrc20AbiError> for GenerateTxError {
    fn from(e: Qrc20AbiError) -> Self { GenerateTxError::Internal(e.to_string()) }
}

impl From<Qrc20AbiError> for TradePreimageError {
    fn from(e: Qrc20AbiError) -> Self {
        // `Qrc20ABIError` is always an internal error
        TradePreimageError::InternalError(e.to_string())
    }
}

impl From<Qrc20AbiError> for WithdrawError {
    fn from(e: Qrc20AbiError) -> Self {
        // `Qrc20ABIError` is always an internal error
        WithdrawError::InternalError(e.to_string())
    }
}

impl From<Qrc20AbiError> for UtxoRpcError {
    fn from(e: Qrc20AbiError) -> Self {
        // `Qrc20ABIError` is always an internal error
        UtxoRpcError::Internal(e.to_string())
    }
}

pub fn qrc20_swap_id(time_lock: u32, secret_hash: &[u8]) -> Vec<u8> {
    let mut input = vec![];
    input.extend_from_slice(&time_lock.to_le_bytes());
    input.extend_from_slice(secret_hash);
    sha256(&input).to_vec()
}

pub fn contract_addr_into_rpc_format(address: &H160) -> H160Json { H160Json::from(address.0) }

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Qrc20FeeDetails {
    /// Coin name
    pub coin: String,
    /// Standard UTXO miner fee based on transaction size
    pub miner_fee: BigDecimal,
    /// Gas limit in satoshi.
    pub gas_limit: u64,
    /// Gas price in satoshi.
    pub gas_price: u64,
    /// Total used gas.
    pub total_gas_fee: BigDecimal,
}

/// Parse the given topic to `H160` address.
pub(crate) fn address_from_log_topic(topic: &str) -> Result<H160, String> {
    if topic.len() != 64 {
        return ERR!(
            "Topic {:?} is expected to be H256 encoded topic (with length of 64)",
            topic
        );
    }

    // skip the first 24 characters to parse the last 40 characters to H160.
    // https://github.com/qtumproject/qtum-electrum/blob/v4.0.2/electrum/wallet.py#L2112
    let hash = try_s!(H160Json::from_str(&topic[24..]));
    Ok(hash.0.into())
}

pub(crate) fn address_to_log_topic(address: &H160) -> String {
    let zeros = std::str::from_utf8(&[b'0'; 24]).expect("Expected a valid str from slice of '0' chars");
    let mut topic = format!("{:02x}", address);
    topic.insert_str(0, zeros);
    topic
}

pub struct TransferEventDetails {
    pub(crate) contract_address: H160,
    pub(crate) amount: U256,
    pub(crate) sender: H160,
    pub(crate) receiver: H160,
}

pub(crate) fn transfer_event_from_log(log: &LogEntry) -> Result<TransferEventDetails, String> {
    let contract_address = if log.address.starts_with("0x") {
        try_s!(qtum::contract_addr_from_str(&log.address))
    } else {
        let address = format!("0x{}", log.address);
        try_s!(qtum::contract_addr_from_str(&address))
    };

    if log.topics.len() != 3 {
        return ERR!("'Transfer' event must have 3 topics, found, {}", log.topics.len());
    }

    // https://github.com/qtumproject/qtum-electrum/blob/v4.0.2/electrum/wallet.py#L2111
    let amount = try_s!(U256::from_str(&log.data));

    // https://github.com/qtumproject/qtum-electrum/blob/v4.0.2/electrum/wallet.py#L2112
    let sender = try_s!(address_from_log_topic(&log.topics[1]));
    // https://github.com/qtumproject/qtum-electrum/blob/v4.0.2/electrum/wallet.py#L2113
    let receiver = try_s!(address_from_log_topic(&log.topics[2]));
    Ok(TransferEventDetails {
        contract_address,
        amount,
        sender,
        receiver,
    })
}
