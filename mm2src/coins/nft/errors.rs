//! Error hierarchies returned by the NFT module.
//!
//! The errors are intentionally opaque about their underlying causes: they
//! collapse third-party errors (HTTP transport, JSON, regex, database) into
//! string-carrying variants so that the public surface stays stable as the
//! storage and provider layers evolve. Conversions from concrete external
//! error types are added in the corresponding sub-phase modules.

use crate::lp_coins_errors::{NumConversError, UnexpectedDerivationMethod, WithdrawError};
use common::HttpStatusCode;
use derive_more::Display;
use http::StatusCode;
use ser_error_derive::SerializeErrorType;
use serde::{Deserialize, Serialize};

/// Errors returned by the read-only NFT lookup endpoints
/// (`get_nft_list`, `get_nft_metadata`, `get_nft_transfers`).
#[derive(Clone, Debug, Deserialize, Display, PartialEq, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum GetNftInfoError {
    /// The request payload failed validation.
    #[display(fmt = "Invalid request: {}", _0)]
    InvalidRequest(String),
    /// A network call failed (timeout, connection refused, …).
    #[display(fmt = "Transport: {}", _0)]
    Transport(String),
    /// A response payload could not be parsed.
    #[display(fmt = "Invalid response: {}", _0)]
    InvalidResponse(String),
    /// An unexpected internal failure occurred.
    #[display(fmt = "Internal: {}", _0)]
    Internal(String),
    /// The wallet does not own the requested token.
    #[display(fmt = "Token: token_address {token_address}, token_id {token_id} was not found in wallet")]
    TokenNotFoundInWallet {
        /// Hex-encoded NFT contract address.
        token_address: String,
        /// Decimal-encoded token identifier.
        token_id: String,
    },
    /// A persistent storage operation failed.
    #[display(fmt = "DB error: {}", _0)]
    Storage(String),
    /// A timestamp could not be parsed.
    #[display(fmt = "Timestamp: {}", _0)]
    InvalidTimestamp(String),
    /// The provider response was missing the contract type.
    #[display(fmt = "The contract type is required and should not be null.")]
    ContractTypeIsNull,
    /// A spam-protection step failed.
    #[display(fmt = "Spam filter: {}", _0)]
    SpamFilter(String),
    /// The number of confirmations could not be calculated.
    #[display(fmt = "Confirmations: {}", _0)]
    Confirmations(String),
    /// A numeric conversion failed.
    #[display(fmt = "Numeric: {}", _0)]
    Num(String),
}

impl From<NumConversError> for GetNftInfoError {
    fn from(e: NumConversError) -> Self { GetNftInfoError::Num(e.to_string()) }
}

impl From<UnexpectedDerivationMethod> for GetNftInfoError {
    fn from(e: UnexpectedDerivationMethod) -> Self { GetNftInfoError::Internal(e.to_string()) }
}

impl From<SpamFilterError> for GetNftInfoError {
    fn from(e: SpamFilterError) -> Self { GetNftInfoError::SpamFilter(e.to_string()) }
}

impl From<TransferConfirmationsError> for GetNftInfoError {
    fn from(e: TransferConfirmationsError) -> Self { GetNftInfoError::Confirmations(e.to_string()) }
}

impl From<LockDbError> for GetNftInfoError {
    fn from(e: LockDbError) -> Self { GetNftInfoError::Storage(e.to_string()) }
}

impl From<GetNftInfoError> for WithdrawError {
    fn from(e: GetNftInfoError) -> Self { WithdrawError::InternalError(e.to_string()) }
}

impl HttpStatusCode for GetNftInfoError {
    fn status_code(&self) -> StatusCode {
        match self {
            GetNftInfoError::InvalidRequest(_) | GetNftInfoError::Confirmations(_) => StatusCode::BAD_REQUEST,
            GetNftInfoError::InvalidResponse(_) | GetNftInfoError::InvalidTimestamp(_) => StatusCode::FAILED_DEPENDENCY,
            GetNftInfoError::ContractTypeIsNull => StatusCode::NOT_FOUND,
            GetNftInfoError::Transport(_)
            | GetNftInfoError::Internal(_)
            | GetNftInfoError::TokenNotFoundInWallet { .. }
            | GetNftInfoError::Storage(_)
            | GetNftInfoError::SpamFilter(_)
            | GetNftInfoError::Num(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// Errors returned by the cache-mutating NFT endpoints (`update_nft`,
/// `refresh_nft_metadata`, `withdraw_nft`).
#[derive(Clone, Debug, Deserialize, Display, PartialEq, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum UpdateNftError {
    /// A persistent storage operation failed.
    #[display(fmt = "DB error: {}", _0)]
    Storage(String),
    /// An unexpected internal failure occurred.
    #[display(fmt = "Internal: {}", _0)]
    Internal(String),
    /// A read-side lookup failed.
    GetNftInfoError(GetNftInfoError),
    /// The wallet does not own the requested token.
    #[display(fmt = "Token: token_address {token_address}, token_id {token_id} was not found in wallet")]
    TokenNotFoundInWallet {
        /// Hex-encoded NFT contract address.
        token_address: String,
        /// Decimal-encoded token identifier.
        token_id: String,
    },
    /// The cached amount is below the amount being transferred out.
    #[display(
        fmt = "Insufficient amount NFT token in the cache: amount in list table before transfer {amount_list}, transferred {amount_history}"
    )]
    InsufficientAmountInCache {
        /// Cached amount before the transfer was applied.
        amount_list: String,
        /// Amount that the transfer attempts to move.
        amount_history: String,
    },
    /// The last scanned block number is older than the latest stored block.
    #[display(
        fmt = "Last scanned nft block {last_scanned_block} should be >= last block number {last_nft_block} in nft table"
    )]
    InvalidBlockOrder {
        /// Last block scanned by the providers layer.
        last_scanned_block: String,
        /// Latest block stored in the NFT table.
        last_nft_block: String,
    },
    /// The last scanned block is missing while NFT records exist.
    #[display(fmt = "Last scanned block not found, while the last NFT block exists: {last_nft_block}")]
    LastScannedBlockNotFound {
        /// Latest block stored in the NFT table.
        last_nft_block: String,
    },
    /// An ERC-721 receive transfer was observed for a token already owned.
    #[display(fmt = "Attempt to receive duplicate ERC721 token in transaction hash: {tx_hash}")]
    AttemptToReceiveAlreadyOwnedErc721 {
        /// Transaction hash of the offending transfer.
        tx_hash: String,
    },
    /// A hex string failed to decode.
    #[display(fmt = "Invalid hex string: {}", _0)]
    InvalidHexString(String),
    /// Updating the spam/phishing data failed.
    UpdateSpamPhishingError(UpdateSpamPhishingError),
    /// A network or JSON failure while talking to the provider.
    #[display(fmt = "Provider: {}", _0)]
    Provider(String),
    /// A serde-related failure occurred.
    #[display(fmt = "Serde: {}", _0)]
    SerdeError(String),
    /// Spam-protection failed for the response payload.
    SpamFilterError(SpamFilterError),
    /// The requested coin is not registered.
    #[display(fmt = "No such coin {coin}")]
    NoSuchCoin {
        /// Coin ticker that was looked up.
        coin: String,
    },
    /// The requested coin does not support NFT operations.
    #[display(fmt = "{coin} coin doesn't support NFT")]
    CoinDoesntSupportNft {
        /// Coin ticker that was looked up.
        coin: String,
    },
    /// An unsupported derivation method was encountered.
    #[display(fmt = "Unexpected derivation method: {}", _0)]
    UnexpectedDerivationMethod(String),
}

impl From<GetNftInfoError> for UpdateNftError {
    fn from(e: GetNftInfoError) -> Self { UpdateNftError::GetNftInfoError(e) }
}

impl From<UpdateSpamPhishingError> for UpdateNftError {
    fn from(e: UpdateSpamPhishingError) -> Self { UpdateNftError::UpdateSpamPhishingError(e) }
}

impl From<SpamFilterError> for UpdateNftError {
    fn from(e: SpamFilterError) -> Self { UpdateNftError::SpamFilterError(e) }
}

impl From<UnexpectedDerivationMethod> for UpdateNftError {
    fn from(e: UnexpectedDerivationMethod) -> Self { UpdateNftError::UnexpectedDerivationMethod(e.to_string()) }
}

impl From<LockDbError> for UpdateNftError {
    fn from(e: LockDbError) -> Self { UpdateNftError::Storage(e.to_string()) }
}

impl HttpStatusCode for UpdateNftError {
    fn status_code(&self) -> StatusCode {
        match self {
            UpdateNftError::TokenNotFoundInWallet { .. } => StatusCode::NOT_FOUND,
            UpdateNftError::NoSuchCoin { .. } | UpdateNftError::CoinDoesntSupportNft { .. } => StatusCode::BAD_REQUEST,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// Errors returned by the `enable_nft` activation endpoint.
#[derive(Clone, Debug, Deserialize, Display, PartialEq, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum EnableNftError {
    /// The EVM platform coin backing the requested NFT ticker has not been
    /// enabled yet, so the wallet's owner address is unknown.
    #[display(fmt = "Platform coin {coin} must be activated before its NFT support")]
    PlatformCoinIsNotActivated {
        /// Platform-coin ticker that needs to be enabled first.
        coin: String,
    },
    /// The NFT subsystem is already active for the requested ticker.
    #[display(fmt = "NFT support is already active for ticker {ticker}")]
    AlreadyActivated {
        /// NFT pseudo-coin ticker that is already active.
        ticker: String,
    },
    /// The requested ticker does not resolve to a supported NFT protocol.
    #[display(fmt = "Ticker {ticker} does not map to a supported NFT protocol")]
    InvalidNftTicker {
        /// Ticker that failed to resolve.
        ticker: String,
    },
    /// The resolved platform coin is not an EVM coin.
    #[display(fmt = "Platform coin {coin} is not an EVM coin and cannot back NFT support")]
    UnsupportedPlatform {
        /// Platform-coin ticker that is not EVM.
        coin: String,
    },
    /// The inline protocol's declared platform disagrees with the platform
    /// resolved from the ticker.
    #[display(fmt = "Protocol platform {declared} does not match resolved platform {resolved}")]
    PlatformMismatch {
        /// Platform ticker carried by the inline protocol.
        declared: String,
        /// Platform ticker resolved from the request ticker.
        resolved: String,
    },
    /// The caller-supplied provider URL was invalid or the initial crawl
    /// could not reach the indexer.
    #[display(fmt = "Initial inventory crawl failed: {}", _0)]
    CrawlFailed(String),
    /// A persistent storage operation failed.
    #[display(fmt = "DB error: {}", _0)]
    Storage(String),
    /// An unexpected internal failure occurred.
    #[display(fmt = "Internal: {}", _0)]
    Internal(String),
}

impl HttpStatusCode for EnableNftError {
    fn status_code(&self) -> StatusCode {
        match self {
            EnableNftError::PlatformCoinIsNotActivated { .. }
            | EnableNftError::AlreadyActivated { .. }
            | EnableNftError::InvalidNftTicker { .. }
            | EnableNftError::UnsupportedPlatform { .. }
            | EnableNftError::PlatformMismatch { .. } => StatusCode::BAD_REQUEST,
            EnableNftError::CrawlFailed(_) => StatusCode::FAILED_DEPENDENCY,
            EnableNftError::Storage(_) | EnableNftError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// Errors raised by the spam-protection helpers (regex compilation,
/// JSON sanitization, …).
#[derive(Clone, Debug, Deserialize, Display, PartialEq, Serialize)]
pub enum SpamFilterError {
    /// A regular expression failed to compile.
    #[display(fmt = "Regex: {}", _0)]
    Regex(String),
    /// A JSON value could not be sanitized.
    #[display(fmt = "Serde: {}", _0)]
    Serde(String),
}

/// Errors raised by the spam/phishing update job that talks to the
/// anti-spam service.
#[derive(Clone, Debug, Deserialize, Display, PartialEq, Serialize)]
pub enum UpdateSpamPhishingError {
    /// The request payload failed validation.
    #[display(fmt = "Invalid request: {}", _0)]
    InvalidRequest(String),
    /// A network call failed.
    #[display(fmt = "Transport: {}", _0)]
    Transport(String),
    /// A response payload could not be parsed.
    #[display(fmt = "Invalid response: {}", _0)]
    InvalidResponse(String),
    /// An unexpected internal failure occurred.
    #[display(fmt = "Internal: {}", _0)]
    Internal(String),
    /// A persistent storage operation failed.
    #[display(fmt = "DB error: {}", _0)]
    Storage(String),
}

/// Errors raised when parsing a [`crate::nft::Chain`] from a string.
#[derive(Clone, Debug, Display, PartialEq)]
pub enum ParseChainError {
    /// The provided value does not match any supported chain.
    #[display(fmt = "Unsupported chain identifier")]
    Unsupported,
}

/// Errors raised when parsing a [`crate::nft::ContractType`] from a string.
#[derive(Clone, Debug, Display, PartialEq)]
pub enum ParseContractTypeError {
    /// The provided value does not match any supported contract type.
    #[display(fmt = "Unsupported contract type")]
    Unsupported,
}

/// Errors raised when parsing a [`crate::nft::TransferStatus`] from a string.
#[derive(Clone, Debug, Display, PartialEq)]
pub enum ParseTransferStatusError {
    /// The provided value does not match any supported transfer status.
    #[display(fmt = "Unsupported transfer status")]
    Unsupported,
}

/// Errors raised when fetching token metadata from a remote URL.
#[derive(Clone, Debug, Display, PartialEq)]
pub enum MetadataFetchError {
    /// A network call failed.
    #[display(fmt = "Transport: {}", _0)]
    Transport(String),
    /// A response payload could not be parsed.
    #[display(fmt = "Invalid response: {}", _0)]
    InvalidResponse(String),
    /// An unexpected internal failure occurred.
    #[display(fmt = "Internal: {}", _0)]
    Internal(String),
}

/// Errors raised when locking the NFT cache database.
#[derive(Clone, Debug, Display, PartialEq)]
pub enum LockDbError {
    /// The persistent storage backend reported an error.
    #[display(fmt = "Storage: {}", _0)]
    Storage(String),
}

/// Errors raised when calculating the number of confirmations for a
/// recorded transfer.
#[derive(Clone, Debug, Deserialize, Display, PartialEq, Serialize)]
pub enum TransferConfirmationsError {
    /// The requested coin is not registered.
    #[display(fmt = "No such coin {coin}")]
    NoSuchCoin {
        /// Coin ticker that was looked up.
        coin: String,
    },
    /// The requested coin does not support NFT operations.
    #[display(fmt = "{coin} coin doesn't support NFT")]
    CoinDoesntSupportNft {
        /// Coin ticker that was looked up.
        coin: String,
    },
    /// The current block could not be retrieved.
    #[display(fmt = "Get current block error: {}", _0)]
    GetCurrentBlockErr(String),
}

/// Errors returned by `clear_nft_db`.
#[derive(Clone, Debug, Deserialize, Display, PartialEq, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum ClearNftDbError {
    /// A persistent storage operation failed.
    #[display(fmt = "DB error: {}", _0)]
    Storage(String),
    /// An unexpected internal failure occurred.
    #[display(fmt = "Internal: {}", _0)]
    Internal(String),
    /// The request payload failed validation.
    #[display(fmt = "Invalid request: {}", _0)]
    InvalidRequest(String),
}

impl From<LockDbError> for ClearNftDbError {
    fn from(e: LockDbError) -> Self { ClearNftDbError::Storage(e.to_string()) }
}

impl HttpStatusCode for ClearNftDbError {
    fn status_code(&self) -> StatusCode {
        match self {
            ClearNftDbError::InvalidRequest(_) => StatusCode::BAD_REQUEST,
            ClearNftDbError::Storage(_) | ClearNftDbError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_nft_info_error_status_codes() {
        assert_eq!(
            GetNftInfoError::InvalidRequest("x".into()).status_code(),
            StatusCode::BAD_REQUEST,
        );
        assert_eq!(
            GetNftInfoError::Transport("x".into()).status_code(),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
        assert_eq!(GetNftInfoError::ContractTypeIsNull.status_code(), StatusCode::NOT_FOUND,);
        assert_eq!(
            GetNftInfoError::InvalidResponse("x".into()).status_code(),
            StatusCode::FAILED_DEPENDENCY,
        );
    }

    #[test]
    fn update_nft_error_status_codes() {
        assert_eq!(
            UpdateNftError::NoSuchCoin { coin: "ETH".into() }.status_code(),
            StatusCode::BAD_REQUEST,
        );
        assert_eq!(
            UpdateNftError::TokenNotFoundInWallet {
                token_address: "0x0".into(),
                token_id: "1".into(),
            }
            .status_code(),
            StatusCode::NOT_FOUND,
        );
        assert_eq!(
            UpdateNftError::Internal("x".into()).status_code(),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }

    #[test]
    fn clear_nft_db_status_codes() {
        assert_eq!(
            ClearNftDbError::InvalidRequest("x".into()).status_code(),
            StatusCode::BAD_REQUEST,
        );
        assert_eq!(
            ClearNftDbError::Storage("x".into()).status_code(),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }

    #[test]
    fn lock_db_error_lifts_into_get_nft_info_error() {
        let lifted: GetNftInfoError = LockDbError::Storage("disk full".into()).into();
        match lifted {
            GetNftInfoError::Storage(msg) => assert!(msg.contains("disk full")),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn spam_filter_error_lifts_into_update_nft_error() {
        let lifted: UpdateNftError = SpamFilterError::Regex("bad".into()).into();
        assert!(matches!(lifted, UpdateNftError::SpamFilterError(_)));
    }

    #[test]
    fn get_nft_info_error_serializes_with_tag() {
        let json = serde_json::to_string(&GetNftInfoError::Internal("boom".into())).unwrap();
        assert!(json.contains("\"error_type\":\"Internal\""));
        assert!(json.contains("\"error_data\":\"boom\""));
    }
}
