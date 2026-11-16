//! The typed failures of the wallet and coin RPC cores (CRD ch.43 §43.1R; plan `mmrpc-v2-completion`, group D):
//! `my_balance`, `disable_coin`, `send_raw_transaction`, `validateaddress`, `convertaddress`,
//! `convert_utxo_address`, `kmd_rewards_info`, `coins_needed_for_kick_start`, `set_required_confirmations` and
//! `set_requires_notarization`.
//!
//! These methods used to fail with a string. Each core now fails with an error enum in the adjacently tagged form of
//! the versioned methods (`error_type`, `error_data`), a status (`HttpStatusCode`) and a `Display` text. The
//! `Display` text of a variant is the text the legacy handler produced for that failure, without its `file:line] `
//! prefix, so the legacy surface renders the same answer from it (see `mm2_main`'s `rpc::legacy_adapter`).
//!
//! Conventions shared by the enums of this file (the tokens, payload kinds and statuses are those of the nearest
//! versioned sibling, the `withdraw` error set, CRD ch.43 R43.1C.8 and D43.1C.4 option (ii)):
//!
//! * `NoSuchCoin` is a record with `coin`, status 404 (as in `WithdrawError`); the `Display` text is the one the
//!   legacy handler of the method has for an unknown coin, which differs from method to method.
//! * `Transport` is a string payload, status 502 (as in `RawTransactionError` and the versioned `withdraw`);
//!   `InternalError` is a string payload, status 500. A string payload is the whole legacy text of the failure, so its
//!   `Display` prints it as it is.
//! * A refusal that the caller's request causes (`InvalidParam`, `InvalidAddress`, `UnsupportedError`, the two
//!   deactivation conflicts) is status 400.
//! * `FeeCapBelowBaseFee` keeps its record payload and its status 500, as in `WithdrawError`.

use super::*;

/// The failures of `my_balance`.
#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum MyBalanceRpcError {
    /// The coin is not activated.
    #[display(fmt = "No such coin: {}", coin)]
    NoSuchCoin { coin: String },
    /// The balance read failed in the coin's transport. The payload is the whole text.
    #[display(fmt = "{}", _0)]
    Transport(String),
    /// An EVM node rejected the balance read for a fee cap below the block base fee (CRD ch.56 R5).
    #[display(fmt = "{}", _0)]
    FeeCapBelowBaseFee(FeeCapBelowBaseFee),
    /// The coin registry, the coin's address or its storage failed. The payload is the whole text.
    #[display(fmt = "{}", _0)]
    InternalError(String),
}

impl From<BalanceError> for MyBalanceRpcError {
    fn from(error: BalanceError) -> Self {
        match error {
            error @ (BalanceError::Transport(_) | BalanceError::InvalidResponse(_)) => {
                MyBalanceRpcError::Transport(error.to_string())
            },
            BalanceError::FeeCapBelowBaseFee(rejection) => MyBalanceRpcError::FeeCapBelowBaseFee(rejection),
            error @ (BalanceError::UnexpectedDerivationMethod(_)
            | BalanceError::WalletStorageError(_)
            | BalanceError::Internal(_)) => MyBalanceRpcError::InternalError(error.to_string()),
        }
    }
}

impl HttpStatusCode for MyBalanceRpcError {
    fn status_code(&self) -> StatusCode {
        match self {
            MyBalanceRpcError::NoSuchCoin { .. } => StatusCode::NOT_FOUND,
            MyBalanceRpcError::Transport(_) => StatusCode::BAD_GATEWAY,
            MyBalanceRpcError::FeeCapBelowBaseFee(_) | MyBalanceRpcError::InternalError(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            },
        }
    }
}

/// The failures of `disable_coin`.
#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum DisableCoinError {
    /// The coin is not activated.
    #[display(fmt = "No such coin: {}", coin)]
    NoSuchCoin { coin: String },
    /// Swaps that are running use the coin. `swaps` lists their UUIDs as text (the `coins` crate and the swap code
    /// use different `uuid` crate versions).
    #[display(fmt = "There're active swaps using {}", coin)]
    ActiveSwapsBlockDeactivation { coin: String, swaps: Vec<String> },
    /// Orders of the coin are being matched, so they cannot be cancelled. `matching` lists them, `cancelled` lists
    /// the orders of the coin that the attempt did cancel.
    #[display(fmt = "There're currently matching orders using {}", coin)]
    MatchingOrdersBlockDeactivation {
        coin: String,
        matching: Vec<String>,
        cancelled: Vec<String>,
    },
    /// The coin registry, the swap list or the order container failed, or the coin was removed by someone else
    /// between the lookup and the removal. The payload is the whole text.
    #[display(fmt = "{}", _0)]
    InternalError(String),
}

impl HttpStatusCode for DisableCoinError {
    fn status_code(&self) -> StatusCode {
        match self {
            DisableCoinError::NoSuchCoin { .. } => StatusCode::NOT_FOUND,
            DisableCoinError::ActiveSwapsBlockDeactivation { .. }
            | DisableCoinError::MatchingOrdersBlockDeactivation { .. } => StatusCode::BAD_REQUEST,
            DisableCoinError::InternalError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// The failures of `send_raw_transaction`.
#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum SendRawTransactionError {
    /// The coin is not activated.
    #[display(fmt = "No such coin: {}", coin)]
    NoSuchCoin { coin: String },
    /// The request carries neither `tx_hex` nor `tx_json`, or the carrier it selected has the wrong type. The payload
    /// is the whole text.
    #[display(fmt = "{}", _0)]
    InvalidParam(String),
    /// The coin refused or could not broadcast the transaction: the coin's broadcast answers one text for a
    /// transaction that does not decode, for a node that refuses it and for a node that cannot be reached, so all
    /// three are this variant. The payload is the whole text.
    #[display(fmt = "{}", _0)]
    Transport(String),
    /// The coin registry failed, or the JSON carrier could not be serialized. The payload is the whole text.
    #[display(fmt = "{}", _0)]
    InternalError(String),
}

impl HttpStatusCode for SendRawTransactionError {
    fn status_code(&self) -> StatusCode {
        match self {
            SendRawTransactionError::NoSuchCoin { .. } => StatusCode::NOT_FOUND,
            SendRawTransactionError::InvalidParam(_) => StatusCode::BAD_REQUEST,
            SendRawTransactionError::Transport(_) => StatusCode::BAD_GATEWAY,
            SendRawTransactionError::InternalError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// The failures of `validateaddress`. An address that is not valid is not a failure: it is the success
/// `{"is_valid": false, "reason": ...}`.
#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum ValidateAddressError {
    /// The coin is not activated.
    #[display(fmt = "No such coin: {}", coin)]
    NoSuchCoin { coin: String },
    /// The coin registry failed. The payload is the whole text.
    #[display(fmt = "{}", _0)]
    InternalError(String),
}

impl HttpStatusCode for ValidateAddressError {
    fn status_code(&self) -> StatusCode {
        match self {
            ValidateAddressError::NoSuchCoin { .. } => StatusCode::NOT_FOUND,
            ValidateAddressError::InternalError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// The failures of `convertaddress`.
#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum ConvertAddressError {
    /// The coin is not activated.
    #[display(fmt = "No such coin: {}", coin)]
    NoSuchCoin { coin: String },
    /// The coin's conversion refused the address or the format, or the coin has no address conversion: the coin's
    /// conversion answers one text for all of them. The payload is the whole text.
    #[display(fmt = "{}", _0)]
    InvalidParam(String),
    /// The coin registry failed. The payload is the whole text.
    #[display(fmt = "{}", _0)]
    InternalError(String),
}

impl HttpStatusCode for ConvertAddressError {
    fn status_code(&self) -> StatusCode {
        match self {
            ConvertAddressError::NoSuchCoin { .. } => StatusCode::NOT_FOUND,
            ConvertAddressError::InvalidParam(_) => StatusCode::BAD_REQUEST,
            ConvertAddressError::InternalError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// The failures of `convert_utxo_address`.
#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum ConvertUtxoAddressError {
    /// The target coin is not activated, or the coin registry could not be read: the handler answers one text for
    /// both. `coin` is the target coin.
    #[display(fmt = "Coin {} is not activated", coin)]
    NoSuchCoin { coin: String },
    /// The address does not parse. The payload is the whole text.
    #[display(fmt = "{}", _0)]
    InvalidAddress(String),
    /// The target coin is not a UTXO coin. The payload is the whole text.
    #[display(fmt = "{}", _0)]
    UnsupportedError(String),
}

impl HttpStatusCode for ConvertUtxoAddressError {
    fn status_code(&self) -> StatusCode {
        match self {
            ConvertUtxoAddressError::NoSuchCoin { .. } => StatusCode::NOT_FOUND,
            ConvertUtxoAddressError::InvalidAddress(_) | ConvertUtxoAddressError::UnsupportedError(_) => {
                StatusCode::BAD_REQUEST
            },
        }
    }
}

/// The failures of `kmd_rewards_info`.
#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum KmdRewardsInfoError {
    /// KMD is not activated. `coin` is `KMD`.
    #[display(fmt = "{} is not activated", coin)]
    NoSuchCoin { coin: String },
    /// Reading the unspent outputs, their transactions or the median time past failed. The payload is the whole text.
    #[display(fmt = "{}", _0)]
    Transport(String),
    /// The coin registry failed, KMD is not a UTXO coin, or KMD runs under a wallet without an Iguana key. The
    /// payload is the whole text.
    #[display(fmt = "{}", _0)]
    InternalError(String),
}

impl HttpStatusCode for KmdRewardsInfoError {
    fn status_code(&self) -> StatusCode {
        match self {
            KmdRewardsInfoError::NoSuchCoin { .. } => StatusCode::NOT_FOUND,
            KmdRewardsInfoError::Transport(_) => StatusCode::BAD_GATEWAY,
            KmdRewardsInfoError::InternalError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// The failures of `coins_needed_for_kick_start`: the only one is a list of coins that cannot be read.
#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum CoinsNeededForKickStartError {
    /// The list of the coins that interrupted swaps and orders need is poisoned. The payload is the whole text.
    #[display(fmt = "{}", _0)]
    InternalError(String),
}

impl HttpStatusCode for CoinsNeededForKickStartError {
    fn status_code(&self) -> StatusCode {
        match self {
            CoinsNeededForKickStartError::InternalError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// The failures of `set_required_confirmations` and `set_requires_notarization`, which fail alike.
#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum CoinSettingError {
    /// The coin is not activated.
    #[display(fmt = "No such coin {}", coin)]
    NoSuchCoin { coin: String },
    /// The coin registry failed. The payload is the whole text.
    #[display(fmt = "{}", _0)]
    InternalError(String),
}

impl HttpStatusCode for CoinSettingError {
    fn status_code(&self) -> StatusCode {
        match self {
            CoinSettingError::NoSuchCoin { .. } => StatusCode::NOT_FOUND,
            CoinSettingError::InternalError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}
