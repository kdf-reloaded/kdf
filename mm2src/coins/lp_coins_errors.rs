use super::*;

#[derive(Debug, Deserialize, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum RawTransactionError {
    #[display(fmt = "No such coin {}", coin)]
    NoSuchCoin { coin: String },
    #[display(fmt = "Invalid  hash: {}", _0)]
    InvalidHashError(String),
    #[display(fmt = "Transport error: {}", _0)]
    Transport(String),
    #[display(fmt = "Hash does not exist: {}", _0)]
    HashNotExist(String),
    #[display(fmt = "Internal error: {}", _0)]
    InternalError(String),
    #[display(fmt = "Transaction decode error: {}", _0)]
    DecodeError(String),
    #[display(fmt = "Invalid param: {}", _0)]
    InvalidParam(String),
    #[display(fmt = "Non-existent previous output: {}", _0)]
    NonExistentPrevOutputError(String),
    #[display(fmt = "Signing error: {}", _0)]
    SigningError(String),
    #[display(fmt = "Not implemented for this coin {}", coin)]
    NotImplemented { coin: String },
    #[display(fmt = "Transaction error {}", _0)]
    TransactionError(String),
}
impl HttpStatusCode for RawTransactionError {
    fn status_code(&self) -> StatusCode {
        match self {
            RawTransactionError::InternalError(_) | RawTransactionError::SigningError(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            },
            RawTransactionError::NoSuchCoin { .. }
            | RawTransactionError::InvalidHashError(_)
            | RawTransactionError::HashNotExist(_)
            | RawTransactionError::DecodeError(_)
            | RawTransactionError::InvalidParam(_)
            | RawTransactionError::NonExistentPrevOutputError(_)
            | RawTransactionError::TransactionError(_) => StatusCode::BAD_REQUEST,
            RawTransactionError::NotImplemented { .. } => StatusCode::NOT_IMPLEMENTED,
            RawTransactionError::Transport(_) => StatusCode::BAD_GATEWAY,
        }
    }
}
impl From<CoinFindError> for RawTransactionError {
    fn from(e: CoinFindError) -> Self {
        match e {
            CoinFindError::NoSuchCoin { coin } => RawTransactionError::NoSuchCoin { coin },
        }
    }
}
#[derive(Debug, Display)]
pub enum TxHistoryError {
    ErrorSerializing(String),
    ErrorDeserializing(String),
    ErrorSaving(String),
    ErrorLoading(String),
    ErrorClearing(String),
    NotSupported(String),
    InternalError(String),
}
#[derive(Debug, Display)]
pub enum PrivKeyNotAllowed {
    #[display(fmt = "Hardware Wallet is not supported")]
    HardwareWalletNotSupported,
}
#[derive(Debug, Display, PartialEq, Serialize)]
pub enum UnexpectedDerivationMethod {
    #[display(fmt = "Iguana private key is unavailable")]
    IguanaPrivKeyUnavailable,
    #[display(fmt = "HD wallet is unavailable")]
    HDWalletUnavailable,
}
#[derive(Debug, Display)]
pub enum TradePreimageError {
    #[display(
        fmt = "Not enough {} to preimage the trade: available {}, required at least {}",
        coin,
        available,
        required
    )]
    NotSufficientBalance {
        coin: String,
        available: BigDecimal,
        required: BigDecimal,
    },
    #[display(fmt = "The amount {} less than minimum transaction amount {}", amount, threshold)]
    AmountIsTooSmall { amount: BigDecimal, threshold: BigDecimal },
    #[display(fmt = "Transport error: {}", _0)]
    Transport(String),
    #[display(fmt = "Internal error: {}", _0)]
    InternalError(String),
}
impl From<NumConversError> for TradePreimageError {
    fn from(e: NumConversError) -> Self { TradePreimageError::InternalError(e.to_string()) }
}
impl From<UnexpectedDerivationMethod> for TradePreimageError {
    fn from(e: UnexpectedDerivationMethod) -> Self { TradePreimageError::InternalError(e.to_string()) }
}
impl TradePreimageError {
    /// Construct [`TradePreimageError`] from [`GenerateTxError`] using additional `coin` and `decimals`.
    pub fn from_generate_tx_error(
        gen_tx_err: GenerateTxError,
        coin: String,
        decimals: u8,
        is_upper_bound: bool,
    ) -> TradePreimageError {
        match gen_tx_err {
            GenerateTxError::EmptyUtxoSet { required } => {
                let required = big_decimal_from_sat_unsigned(required, decimals);
                TradePreimageError::NotSufficientBalance {
                    coin,
                    available: BigDecimal::from(0),
                    required,
                }
            },
            GenerateTxError::EmptyOutputs => TradePreimageError::InternalError(gen_tx_err.to_string()),
            GenerateTxError::OutputValueLessThanDust { value, dust } => {
                if is_upper_bound {
                    // If the preimage value is [`TradePreimageValue::UpperBound`], then we had to pass the account balance as the output value.
                    if value == 0 {
                        let required = big_decimal_from_sat_unsigned(dust, decimals);
                        TradePreimageError::NotSufficientBalance {
                            coin,
                            available: big_decimal_from_sat_unsigned(value, decimals),
                            required,
                        }
                    } else {
                        let error = format!(
                            "Output value {} (equal to the account balance) less than dust {}. Probably, dust is not set or outdated",
                            value, dust
                        );
                        TradePreimageError::InternalError(error)
                    }
                } else {
                    let amount = big_decimal_from_sat_unsigned(value, decimals);
                    let threshold = big_decimal_from_sat_unsigned(dust, decimals);
                    TradePreimageError::AmountIsTooSmall { amount, threshold }
                }
            },
            GenerateTxError::DeductFeeFromOutputFailed {
                output_value, required, ..
            } => {
                let available = big_decimal_from_sat_unsigned(output_value, decimals);
                let required = big_decimal_from_sat_unsigned(required, decimals);
                TradePreimageError::NotSufficientBalance {
                    coin,
                    available,
                    required,
                }
            },
            GenerateTxError::NotEnoughUtxos { sum_utxos, required } => {
                let available = big_decimal_from_sat_unsigned(sum_utxos, decimals);
                let required = big_decimal_from_sat_unsigned(required, decimals);
                TradePreimageError::NotSufficientBalance {
                    coin,
                    available,
                    required,
                }
            },
            GenerateTxError::Transport(e) => TradePreimageError::Transport(e),
            GenerateTxError::Internal(e) => TradePreimageError::InternalError(e),
        }
    }
}
/// The reason of unsuccessful conversion of two internal numbers, e.g. `u64` from `BigNumber`.
#[derive(Debug, Display)]
pub struct NumConversError(pub(crate) String);
impl From<ParseBigDecimalError> for NumConversError {
    fn from(e: ParseBigDecimalError) -> Self { NumConversError::new(e.to_string()) }
}
impl NumConversError {
    pub fn new(description: String) -> NumConversError { NumConversError(description) }

    pub fn description(&self) -> &str { &self.0 }
}
#[derive(Debug, Display, PartialEq)]
pub enum BalanceError {
    #[display(fmt = "Transport: {}", _0)]
    Transport(String),
    #[display(fmt = "Invalid response: {}", _0)]
    InvalidResponse(String),
    UnexpectedDerivationMethod(UnexpectedDerivationMethod),
    #[display(fmt = "Wallet storage error: {}", _0)]
    WalletStorageError(String),
    #[display(fmt = "Internal: {}", _0)]
    Internal(String),
}
#[derive(Debug, PartialEq, Display)]
pub enum GetNonZeroBalance {
    #[display(fmt = "Internal error when retrieving balance")]
    MyBalanceError(BalanceError),
    #[display(fmt = "Balance is zero")]
    BalanceIsZero,
}
impl From<BalanceError> for GetNonZeroBalance {
    fn from(e: BalanceError) -> Self { GetNonZeroBalance::MyBalanceError(e) }
}
impl From<NumConversError> for BalanceError {
    fn from(e: NumConversError) -> Self { BalanceError::Internal(e.to_string()) }
}
impl From<UnexpectedDerivationMethod> for BalanceError {
    fn from(e: UnexpectedDerivationMethod) -> Self { BalanceError::UnexpectedDerivationMethod(e) }
}
impl From<Bip32Error> for BalanceError {
    fn from(e: Bip32Error) -> Self { BalanceError::Internal(e.to_string()) }
}
#[derive(Debug, Deserialize, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum StakingInfosError {
    #[display(fmt = "Staking infos not available for: {}", coin)]
    CoinDoesntSupportStakingInfos { coin: String },
    #[display(fmt = "No such coin {}", coin)]
    NoSuchCoin { coin: String },
    #[display(fmt = "Derivation method is not supported: {}", _0)]
    UnexpectedDerivationMethod(String),
    #[display(fmt = "Transport error: {}", _0)]
    Transport(String),
    #[display(fmt = "Internal error: {}", _0)]
    Internal(String),
    #[display(fmt = "Invalid payload: {}", reason)]
    InvalidPayload { reason: String },
}
impl From<UtxoRpcError> for StakingInfosError {
    fn from(e: UtxoRpcError) -> Self {
        match e {
            UtxoRpcError::Transport(rpc) | UtxoRpcError::ResponseParseError(rpc) => {
                StakingInfosError::Transport(rpc.to_string())
            },
            UtxoRpcError::InvalidResponse(error) => StakingInfosError::Transport(error),
            UtxoRpcError::Internal(error) => StakingInfosError::Internal(error),
        }
    }
}
impl From<UnexpectedDerivationMethod> for StakingInfosError {
    fn from(e: UnexpectedDerivationMethod) -> Self { StakingInfosError::UnexpectedDerivationMethod(e.to_string()) }
}
impl From<Qrc20AddressError> for StakingInfosError {
    fn from(e: Qrc20AddressError) -> Self {
        match e {
            Qrc20AddressError::UnexpectedDerivationMethod(e) => StakingInfosError::UnexpectedDerivationMethod(e),
            Qrc20AddressError::ScriptHashTypeNotSupported { script_hash_type } => {
                StakingInfosError::Internal(format!("Script hash type '{}' is not supported", script_hash_type))
            },
        }
    }
}
impl HttpStatusCode for StakingInfosError {
    fn status_code(&self) -> StatusCode {
        match self {
            StakingInfosError::NoSuchCoin { .. }
            | StakingInfosError::CoinDoesntSupportStakingInfos { .. }
            | StakingInfosError::UnexpectedDerivationMethod(_)
            | StakingInfosError::InvalidPayload { .. } => StatusCode::BAD_REQUEST,
            StakingInfosError::Transport(_) | StakingInfosError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}
impl From<CoinFindError> for StakingInfosError {
    fn from(e: CoinFindError) -> Self {
        match e {
            CoinFindError::NoSuchCoin { coin } => StakingInfosError::NoSuchCoin { coin },
        }
    }
}
#[derive(Debug, Deserialize, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum DelegationError {
    #[display(
        fmt = "Not enough {} to delegate: available {}, required at least {}",
        coin,
        available,
        required
    )]
    NotSufficientBalance {
        coin: String,
        available: BigDecimal,
        required: BigDecimal,
    },
    #[display(fmt = "The amount {} is too small, required at least {}", amount, threshold)]
    AmountTooLow { amount: BigDecimal, threshold: BigDecimal },
    #[display(fmt = "Delegation not available for: {}", coin)]
    CoinDoesntSupportDelegation { coin: String },
    #[display(fmt = "No such coin {}", coin)]
    NoSuchCoin { coin: String },
    #[display(fmt = "{}", _0)]
    CannotInteractWithSmartContract(String),
    #[display(fmt = "{}", _0)]
    AddressError(String),
    #[display(fmt = "Already delegating to: {}", _0)]
    AlreadyDelegating(String),
    #[display(fmt = "Delegation is not supported, reason: {}", reason)]
    DelegationOpsNotSupported { reason: String },
    #[display(fmt = "Cannot undelegate {} from {}", delegator_addr, validator_addr)]
    CanNotUndelegate {
        delegator_addr: String,
        validator_addr: String,
    },
    #[display(fmt = "Requested {} to undelegate but only {} is available", requested, available)]
    TooMuchToUndelegate {
        available: BigDecimal,
        requested: BigDecimal,
    },
    #[display(fmt = "Reward {} is less than the claiming fee {}", reward, fee)]
    UnprofitableReward { reward: BigDecimal, fee: BigDecimal },
    #[display(fmt = "Nothing to claim for {}", coin)]
    NothingToClaim { coin: String },
    #[display(fmt = "Invalid payload: {}", reason)]
    InvalidPayload { reason: String },
    #[display(fmt = "Transport error: {}", _0)]
    Transport(String),
    #[display(fmt = "Internal error: {}", _0)]
    InternalError(String),
}
impl From<UtxoRpcError> for DelegationError {
    fn from(e: UtxoRpcError) -> Self {
        match e {
            UtxoRpcError::Transport(transport) | UtxoRpcError::ResponseParseError(transport) => {
                DelegationError::Transport(transport.to_string())
            },
            UtxoRpcError::InvalidResponse(resp) => DelegationError::Transport(resp),
            UtxoRpcError::Internal(internal) => DelegationError::InternalError(internal),
        }
    }
}
impl From<StakingInfosError> for DelegationError {
    fn from(e: StakingInfosError) -> Self {
        match e {
            StakingInfosError::CoinDoesntSupportStakingInfos { coin } => {
                DelegationError::CoinDoesntSupportDelegation { coin }
            },
            StakingInfosError::NoSuchCoin { coin } => DelegationError::NoSuchCoin { coin },
            StakingInfosError::Transport(e) => DelegationError::Transport(e),
            StakingInfosError::UnexpectedDerivationMethod(reason) => {
                DelegationError::DelegationOpsNotSupported { reason }
            },
            StakingInfosError::Internal(e) => DelegationError::InternalError(e),
            StakingInfosError::InvalidPayload { reason } => DelegationError::InvalidPayload { reason },
        }
    }
}
impl From<CoinFindError> for DelegationError {
    fn from(e: CoinFindError) -> Self {
        match e {
            CoinFindError::NoSuchCoin { coin } => DelegationError::NoSuchCoin { coin },
        }
    }
}
impl From<BalanceError> for DelegationError {
    fn from(e: BalanceError) -> Self {
        match e {
            BalanceError::Transport(error) | BalanceError::InvalidResponse(error) => DelegationError::Transport(error),
            BalanceError::UnexpectedDerivationMethod(e) => {
                DelegationError::DelegationOpsNotSupported { reason: e.to_string() }
            },
            e @ BalanceError::WalletStorageError(_) => DelegationError::InternalError(e.to_string()),
            BalanceError::Internal(internal) => DelegationError::InternalError(internal),
        }
    }
}
impl From<UtxoSignWithKeyPairError> for DelegationError {
    fn from(e: UtxoSignWithKeyPairError) -> Self {
        let error = format!("Error signing: {}", e);
        DelegationError::InternalError(error)
    }
}
impl From<PrivKeyNotAllowed> for DelegationError {
    fn from(e: PrivKeyNotAllowed) -> Self { DelegationError::DelegationOpsNotSupported { reason: e.to_string() } }
}
impl From<UnexpectedDerivationMethod> for DelegationError {
    fn from(e: UnexpectedDerivationMethod) -> Self {
        DelegationError::DelegationOpsNotSupported { reason: e.to_string() }
    }
}
impl From<ScriptHashTypeNotSupported> for DelegationError {
    fn from(e: ScriptHashTypeNotSupported) -> Self { DelegationError::AddressError(e.to_string()) }
}
impl HttpStatusCode for DelegationError {
    fn status_code(&self) -> StatusCode {
        match self {
            DelegationError::Transport(_) | DelegationError::InternalError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            _ => StatusCode::BAD_REQUEST,
        }
    }
}
impl DelegationError {
    pub fn from_generate_tx_error(gen_tx_err: GenerateTxError, coin: String, decimals: u8) -> DelegationError {
        match gen_tx_err {
            GenerateTxError::EmptyUtxoSet { required } => {
                let required = big_decimal_from_sat_unsigned(required, decimals);
                DelegationError::NotSufficientBalance {
                    coin,
                    available: BigDecimal::from(0),
                    required,
                }
            },
            GenerateTxError::EmptyOutputs => DelegationError::InternalError(gen_tx_err.to_string()),
            GenerateTxError::OutputValueLessThanDust { value, dust } => {
                let amount = big_decimal_from_sat_unsigned(value, decimals);
                let threshold = big_decimal_from_sat_unsigned(dust, decimals);
                DelegationError::AmountTooLow { amount, threshold }
            },
            GenerateTxError::DeductFeeFromOutputFailed {
                output_value, required, ..
            } => {
                let available = big_decimal_from_sat_unsigned(output_value, decimals);
                let required = big_decimal_from_sat_unsigned(required, decimals);
                DelegationError::NotSufficientBalance {
                    coin,
                    available,
                    required,
                }
            },
            GenerateTxError::NotEnoughUtxos { sum_utxos, required } => {
                let available = big_decimal_from_sat_unsigned(sum_utxos, decimals);
                let required = big_decimal_from_sat_unsigned(required, decimals);
                DelegationError::NotSufficientBalance {
                    coin,
                    available,
                    required,
                }
            },
            GenerateTxError::Transport(e) => DelegationError::Transport(e),
            GenerateTxError::Internal(e) => DelegationError::InternalError(e),
        }
    }
}
impl From<tendermint::TendermintCoinRpcError> for DelegationError {
    fn from(e: tendermint::TendermintCoinRpcError) -> Self {
        match e {
            tendermint::TendermintCoinRpcError::InvalidResponse(msg)
            | tendermint::TendermintCoinRpcError::RpcClientError(msg) => DelegationError::Transport(msg),
            tendermint::TendermintCoinRpcError::Prost(msg) | tendermint::TendermintCoinRpcError::InternalError(msg) => {
                DelegationError::InternalError(msg)
            },
            tendermint::TendermintCoinRpcError::UnexpectedAccountType { prefix } => {
                DelegationError::InternalError(format!("unexpected account type: {prefix}"))
            },
            tendermint::TendermintCoinRpcError::PerformanceFeeIsTooLow => {
                DelegationError::InternalError("performance fee is too low".into())
            },
        }
    }
}
#[derive(Clone, Debug, Deserialize, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum WithdrawError {
    /*                                              */
    /*------------ Trezor device errors ------------*/
    /*                                             */
    #[display(fmt = "Trezor device disconnected")]
    TrezorDisconnected,
    #[display(fmt = "Trezor internal error: {}", _0)]
    HardwareWalletInternal(String),
    #[display(fmt = "No Trezor device available")]
    NoTrezorDeviceAvailable,
    #[display(fmt = "Unexpected Hardware Wallet device: {}", _0)]
    FoundUnexpectedDevice(String),
    /*                                         */
    /*------------- WithdrawError -------------*/
    /*                                         */
    #[display(
        fmt = "'{}' coin doesn't support 'init_withdraw' yet. Consider using 'withdraw' request instead",
        coin
    )]
    CoinDoesntSupportInitWithdraw { coin: String },
    #[display(
        fmt = "Not enough {} to withdraw: available {}, required at least {}",
        coin,
        available,
        required
    )]
    NotSufficientBalance {
        coin: String,
        available: BigDecimal,
        required: BigDecimal,
    },
    #[display(fmt = "Balance is zero")]
    ZeroBalanceToWithdrawMax,
    #[display(fmt = "The amount {} is too small, required at least {}", amount, threshold)]
    AmountTooLow { amount: BigDecimal, threshold: BigDecimal },
    #[display(fmt = "Invalid address: {}", _0)]
    InvalidAddress(String),
    #[display(fmt = "Invalid fee policy: {}", _0)]
    InvalidFeePolicy(String),
    #[display(fmt = "No such coin {}", coin)]
    NoSuchCoin { coin: String },
    #[display(fmt = "Withdraw timed out {:?}", _0)]
    Timeout(Duration),
    #[display(fmt = "Unexpected user action. Expected '{}'", expected)]
    UnexpectedUserAction { expected: String },
    #[display(fmt = "Request should contain a 'from' address/account")]
    FromAddressNotFound,
    #[display(fmt = "Unexpected 'from' address: {}", _0)]
    UnexpectedFromAddress(String),
    #[display(fmt = "Unknown '{}' account", account_id)]
    UnknownAccount { account_id: u32 },
    #[display(fmt = "Transport error: {}", _0)]
    Transport(String),
    #[display(fmt = "Internal error: {}", _0)]
    InternalError(String),
    /// CRD §49.3 / §49.9: a Tron TRC20 gasless (GasFree) withdraw failed. The
    /// inner error owns the observable HTTP status mapping, which is delegated
    /// through the standard dispatcher. Inert for every non-Tron coin.
    #[display(fmt = "Gasless withdraw error: {}", _0)]
    Gasless(crate::eth::tron::gasfree::GasFreeWithdrawError),
    /// CRD R47.5.6a / R47.6.7: the requested withdraw is unsupported under the
    /// MetaMask signing policy (e.g. the non-EVM-keypair TRON family, which the
    /// delegated EVM `eth_sendTransaction` model cannot drive). WASM-only: the
    /// MetaMask policy exists only on the browser target.
    #[cfg(target_arch = "wasm32")]
    #[display(fmt = "Unsupported under the MetaMask signing policy: {}", _0)]
    UnsupportedUnderMetamask(String),
}
impl HttpStatusCode for WithdrawError {
    fn status_code(&self) -> StatusCode {
        match self {
            WithdrawError::NoSuchCoin { .. } => StatusCode::NOT_FOUND,
            WithdrawError::Timeout(_) => StatusCode::REQUEST_TIMEOUT,
            WithdrawError::CoinDoesntSupportInitWithdraw { .. }
            | WithdrawError::UnexpectedUserAction { .. }
            | WithdrawError::NotSufficientBalance { .. }
            | WithdrawError::ZeroBalanceToWithdrawMax
            | WithdrawError::AmountTooLow { .. }
            | WithdrawError::InvalidAddress(_)
            | WithdrawError::InvalidFeePolicy(_)
            | WithdrawError::FromAddressNotFound
            | WithdrawError::UnexpectedFromAddress(_)
            | WithdrawError::UnknownAccount { .. } => StatusCode::BAD_REQUEST,
            // CRD R47.6.7: unsupported operation under MetaMask maps to 400.
            #[cfg(target_arch = "wasm32")]
            WithdrawError::UnsupportedUnderMetamask(_) => StatusCode::BAD_REQUEST,
            WithdrawError::NoTrezorDeviceAvailable
            | WithdrawError::TrezorDisconnected
            | WithdrawError::FoundUnexpectedDevice(_) => StatusCode::GONE,
            WithdrawError::HardwareWalletInternal(_)
            | WithdrawError::Transport(_)
            | WithdrawError::InternalError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            // CRD §49.9: delegate to the gasless error's own status mapping.
            WithdrawError::Gasless(e) => e.status_code(),
        }
    }
}
impl From<NumConversError> for WithdrawError {
    fn from(e: NumConversError) -> Self { WithdrawError::InternalError(e.to_string()) }
}
impl From<BalanceError> for WithdrawError {
    fn from(e: BalanceError) -> Self {
        match e {
            BalanceError::Transport(error) | BalanceError::InvalidResponse(error) => WithdrawError::Transport(error),
            BalanceError::UnexpectedDerivationMethod(e) => WithdrawError::from(e),
            e @ BalanceError::WalletStorageError(_) => WithdrawError::InternalError(e.to_string()),
            BalanceError::Internal(internal) => WithdrawError::InternalError(internal),
        }
    }
}
impl From<CoinFindError> for WithdrawError {
    fn from(e: CoinFindError) -> Self {
        match e {
            CoinFindError::NoSuchCoin { coin } => WithdrawError::NoSuchCoin { coin },
        }
    }
}
impl From<UtxoSignWithKeyPairError> for WithdrawError {
    fn from(e: UtxoSignWithKeyPairError) -> Self {
        let error = format!("Error signing: {}", e);
        WithdrawError::InternalError(error)
    }
}
impl From<UnexpectedDerivationMethod> for WithdrawError {
    fn from(e: UnexpectedDerivationMethod) -> Self { WithdrawError::InternalError(e.to_string()) }
}
impl From<PrivKeyNotAllowed> for WithdrawError {
    fn from(e: PrivKeyNotAllowed) -> Self { WithdrawError::InternalError(e.to_string()) }
}
impl WithdrawError {
    /// Construct [`WithdrawError`] from [`GenerateTxError`] using additional `coin` and `decimals`.
    pub fn from_generate_tx_error(gen_tx_err: GenerateTxError, coin: String, decimals: u8) -> WithdrawError {
        match gen_tx_err {
            GenerateTxError::EmptyUtxoSet { required } => {
                let required = big_decimal_from_sat_unsigned(required, decimals);
                WithdrawError::NotSufficientBalance {
                    coin,
                    available: BigDecimal::from(0),
                    required,
                }
            },
            GenerateTxError::EmptyOutputs => WithdrawError::InternalError(gen_tx_err.to_string()),
            GenerateTxError::OutputValueLessThanDust { value, dust } => {
                let amount = big_decimal_from_sat_unsigned(value, decimals);
                let threshold = big_decimal_from_sat_unsigned(dust, decimals);
                WithdrawError::AmountTooLow { amount, threshold }
            },
            GenerateTxError::DeductFeeFromOutputFailed {
                output_value, required, ..
            } => {
                let available = big_decimal_from_sat_unsigned(output_value, decimals);
                let required = big_decimal_from_sat_unsigned(required, decimals);
                WithdrawError::NotSufficientBalance {
                    coin,
                    available,
                    required,
                }
            },
            GenerateTxError::NotEnoughUtxos { sum_utxos, required } => {
                let available = big_decimal_from_sat_unsigned(sum_utxos, decimals);
                let required = big_decimal_from_sat_unsigned(required, decimals);
                WithdrawError::NotSufficientBalance {
                    coin,
                    available,
                    required,
                }
            },
            GenerateTxError::Transport(e) => WithdrawError::Transport(e),
            GenerateTxError::Internal(e) => WithdrawError::InternalError(e),
        }
    }
}
#[derive(Serialize, Display, Debug, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum SignatureError {
    #[display(fmt = "Invalid request: {}", _0)]
    InvalidRequest(String),
    #[display(fmt = "Internal error: {}", _0)]
    InternalError(String),
    #[display(fmt = "Coin is not found: {}", _0)]
    CoinIsNotFound(String),
    #[display(fmt = "sign_message_prefix is not set in coin config")]
    PrefixNotFound,
}
impl HttpStatusCode for SignatureError {
    fn status_code(&self) -> StatusCode {
        match self {
            SignatureError::InvalidRequest(_) => StatusCode::BAD_REQUEST,
            SignatureError::CoinIsNotFound(_) => StatusCode::BAD_REQUEST,
            SignatureError::InternalError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            SignatureError::PrefixNotFound => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}
impl From<keys::Error> for SignatureError {
    fn from(e: keys::Error) -> Self { SignatureError::InternalError(e.to_string()) }
}
impl From<mm2_eth::keys::EthKeyError> for SignatureError {
    fn from(e: mm2_eth::keys::EthKeyError) -> Self { SignatureError::InternalError(e.to_string()) }
}
impl From<PrivKeyNotAllowed> for SignatureError {
    fn from(e: PrivKeyNotAllowed) -> Self { SignatureError::InternalError(e.to_string()) }
}
impl From<CoinFindError> for SignatureError {
    fn from(e: CoinFindError) -> Self { SignatureError::CoinIsNotFound(e.to_string()) }
}
#[derive(Serialize, Display, Debug, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum VerificationError {
    #[display(fmt = "Invalid request: {}", _0)]
    InvalidRequest(String),
    #[display(fmt = "Internal error: {}", _0)]
    InternalError(String),
    #[display(fmt = "Signature decoding error: {}", _0)]
    SignatureDecodingError(String),
    #[display(fmt = "Address decoding error: {}", _0)]
    AddressDecodingError(String),
    #[display(fmt = "Coin is not found: {}", _0)]
    CoinIsNotFound(String),
    #[display(fmt = "sign_message_prefix is not set in coin config")]
    PrefixNotFound,
}
impl HttpStatusCode for VerificationError {
    fn status_code(&self) -> StatusCode {
        match self {
            VerificationError::InvalidRequest(_) => StatusCode::BAD_REQUEST,
            VerificationError::SignatureDecodingError(_) => StatusCode::BAD_REQUEST,
            VerificationError::AddressDecodingError(_) => StatusCode::BAD_REQUEST,
            VerificationError::CoinIsNotFound(_) => StatusCode::BAD_REQUEST,
            VerificationError::InternalError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            VerificationError::PrefixNotFound => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}
impl From<base64::DecodeError> for VerificationError {
    fn from(e: base64::DecodeError) -> Self { VerificationError::SignatureDecodingError(e.to_string()) }
}
impl From<hex::FromHexError> for VerificationError {
    fn from(e: hex::FromHexError) -> Self { VerificationError::AddressDecodingError(e.to_string()) }
}
impl From<FromBase58Error> for VerificationError {
    fn from(e: FromBase58Error) -> Self {
        match e {
            FromBase58Error::InvalidBase58Character(c, _) => {
                VerificationError::AddressDecodingError(format!("Invalid Base58 Character: {}", c))
            },
            FromBase58Error::InvalidBase58Length => {
                VerificationError::AddressDecodingError(String::from("Invalid Base58 Length"))
            },
        }
    }
}
impl From<keys::Error> for VerificationError {
    fn from(e: keys::Error) -> Self { VerificationError::InternalError(e.to_string()) }
}
impl From<mm2_eth::keys::EthKeyError> for VerificationError {
    fn from(e: mm2_eth::keys::EthKeyError) -> Self { VerificationError::InternalError(e.to_string()) }
}
impl From<CoinFindError> for VerificationError {
    fn from(e: CoinFindError) -> Self { VerificationError::CoinIsNotFound(e.to_string()) }
}
/// Errors during transaction generation (preimage creation, signing).
#[derive(Debug, Display)]
pub enum TxGenError {
    #[display(fmt = "RPC error: {}", _0)]
    Rpc(String),
    #[display(fmt = "Numeric conversion error: {}", _0)]
    NumConversion(String),
    #[display(fmt = "Signing error: {}", _0)]
    Signing(String),
    #[display(fmt = "Legacy error: {}", _0)]
    Legacy(String),
    #[display(fmt = "Locktime overflow")]
    LocktimeOverflow,
    #[display(fmt = "Transaction fee too high")]
    TxFeeTooHigh,
    #[display(fmt = "Previous tx is not valid: {}", _0)]
    PrevTxIsNotValid(String),
    #[display(fmt = "Previous output value too low")]
    PrevOutputTooLow,
    #[display(fmt = "{}", _0)]
    Other(String),
}
/// Errors when validating an incoming V2 swap transaction.
#[derive(Debug, Display)]
pub enum ValidateSwapV2TxError {
    #[display(fmt = "Invalid destination or amount: {}", _0)]
    InvalidDestinationOrAmount(String),
    #[display(fmt = "Tx bytes mismatch: {}", _0)]
    TxBytesMismatch(String),
    #[display(fmt = "Unexpected payment state: {}", _0)]
    UnexpectedPaymentState(String),
    #[display(fmt = "Internal error: {}", _0)]
    InternalError(String),
    #[display(fmt = "RPC error: {}", _0)]
    Rpc(String),
    #[display(fmt = "Invalid transaction: {}", _0)]
    InvalidTransaction(String),
    #[display(fmt = "Wrong secret hash: {}", _0)]
    WrongSecretHash(String),
    #[display(fmt = "Wrong payment tx: {}", _0)]
    WrongPaymentTx(String),
    #[display(fmt = "Not enough confirmations: {}", _0)]
    NotEnoughConfirmations(String),
    #[display(fmt = "ABI error: {}", _0)]
    ABIError(String),
    #[display(fmt = "Protocol not supported: {}", _0)]
    ProtocolNotSupported(String),
    #[display(fmt = "Overflow: {}", _0)]
    Overflow(String),
}
/// Error validating a taker funding spend preimage.
#[derive(Debug, Display)]
pub enum ValidateTakerFundingSpendPreimageError {
    #[display(fmt = "Invalid preimage: {}", _0)]
    InvalidPreimage(String),
    #[display(fmt = "{}", _0)]
    InternalError(String),
}
/// Error validating a taker payment spend preimage.
#[derive(Debug, Display)]
pub enum ValidateTakerPaymentSpendPreimageError {
    #[display(fmt = "Invalid preimage: {}", _0)]
    InvalidPreimage(String),
    #[display(fmt = "{}", _0)]
    InternalError(String),
}
/// Error when searching for a payment spend on-chain.
#[derive(Debug, Display)]
pub enum FindPaymentSpendError {
    #[display(fmt = "Timeout waiting for payment spend (wait_until={}, now={})", wait_until, now)]
    Timeout { wait_until: u64, now: u64 },
    #[display(fmt = "Invalid input tx: {}", _0)]
    InvalidInputTx(String),
    #[display(fmt = "Internal error: {}", _0)]
    Internal(String),
    #[display(fmt = "ABI error: {}", _0)]
    ABIError(String),
    #[display(fmt = "Invalid data: {}", _0)]
    InvalidData(String),
    #[display(fmt = "Transport error: {}", _0)]
    Transport(String),
}
/// Error when searching for how taker funding was spent.
#[derive(Debug, Display)]
pub enum SearchForFundingSpendErr {
    #[display(fmt = "Invalid input tx: {}", _0)]
    InvalidInputTx(String),
    #[display(fmt = "Failed to process spend tx: {}", _0)]
    FailedToProcessSpendTx(String),
    #[display(fmt = "RPC error: {}", _0)]
    Rpc(String),
    #[display(fmt = "Block number conversion error: {}", _0)]
    FromBlockConversionErr(String),
    #[display(fmt = "Internal error: {}", _0)]
    Internal(String),
}
