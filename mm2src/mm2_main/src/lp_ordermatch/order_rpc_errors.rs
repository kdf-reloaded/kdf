//! The typed failures of the order-management RPC cores (CRD ch.43 §43.1A; plan `mmrpc-v2-completion` D3).
//!
//! `setprice`, `update_maker_order`, `my_orders`, `order_status`, `buy`, `sell`, `orderbook_depth` and
//! `orders_history_by_filter` used to fail with a string. Each now fails with
//! an error enum in the adjacently tagged form of the versioned methods (`error_type`, `error_data`), a status
//! (`HttpStatusCode`) and a `Display` text. The `Display` text of a variant is the text the legacy handler produced
//! for that failure, without its `file:line] ` prefix, so the legacy surface renders the same answer from it (see
//! `crate::mm2::rpc::legacy_adapter`).
//!
//! Conventions shared by the enums of this file:
//!
//! * The tokens of the versioned siblings are reused where the failure is the same (`NoSuchCoin`,
//!   `CoinIsWalletOnly`, `NotSufficientBalance`, `VolumeTooLow`, `PriceTooLow`, `Transport`, `FeeCapBelowBaseFee`,
//!   `InternalError`; `DbError` and `UUIDNotFound` as the swap and cancel methods name them).
//! * Where one token covers failures whose legacy texts differ, there is one variant per text and the extra
//!   variants carry `#[serde(rename = "<token>")]`: the wire sees the token once, the legacy surface keeps every
//!   text.
//! * A `String` payload (`Transport`, `DbError`, `InternalError`, `ChainDisagreement`) is the whole legacy text of the
//!   failure, so its `Display` prints it as it is.
//! * Statuses: 400 for a request the client got wrong or a state the client must resolve, 500 for transport, storage
//!   and node-context failures.

use super::*;
use crate::mm2::lp_swap::CheckBalanceError;
use coins::{BalanceError, FeeCapBelowBaseFee, TradePreimageError};

/// The conversions the maximum-volume computation needs, so that `setprice` and `update_maker_order` share it
/// ([`get_max_volume`]).
pub(crate) trait MakerVolumeError: From<BalanceError> + From<CheckBalanceError> + NotMmError {
    /// The failure of the receiver trade-fee preimage of `coin`.
    fn from_receiver_fee_error(error: TradePreimageError, coin: &str) -> Self;
}

/// The conversions of the failures that the order-creating and order-changing methods have in common (`setprice`,
/// `update_maker_order`, `buy` and `sell`). The enums name these variants alike (see the module documentation for
/// the renamed ones).
macro_rules! impl_common_order_error_conversions {
    ($error:ident) => {
        impl From<SwapKeyMismatch> for $error {
            fn from(mismatch: SwapKeyMismatch) -> Self {
                $error::SwapKeyMismatch {
                    coin: mismatch.coin,
                    reason: mismatch.reason,
                }
            }
        }

        impl From<BalanceError> for $error {
            fn from(error: BalanceError) -> Self {
                match error {
                    error @ (BalanceError::Transport(_) | BalanceError::InvalidResponse(_)) => {
                        $error::Transport(error.to_string())
                    },
                    error @ BalanceError::WalletStorageError(_) => $error::DbError(error.to_string()),
                    error @ (BalanceError::UnexpectedDerivationMethod(_) | BalanceError::Internal(_)) => {
                        $error::InternalError(error.to_string())
                    },
                    BalanceError::FeeCapBelowBaseFee(rejection) => $error::FeeCapBelowBaseFee(rejection),
                }
            }
        }

        impl From<CheckBalanceError> for $error {
            fn from(error: CheckBalanceError) -> Self {
                match error {
                    CheckBalanceError::NotSufficientBalance {
                        coin,
                        available,
                        required,
                        locked_by_swaps,
                    } => $error::NotSufficientBalance {
                        coin,
                        available,
                        required,
                        locked_by_swaps,
                    },
                    CheckBalanceError::NotSufficientBaseCoinBalance {
                        coin,
                        available,
                        required,
                        locked_by_swaps,
                    } => $error::NotSufficientBaseCoinBalance {
                        coin,
                        available,
                        required,
                        locked_by_swaps,
                    },
                    CheckBalanceError::VolumeTooLow {
                        coin,
                        volume,
                        threshold,
                    } => $error::VolumeTooLow {
                        coin,
                        volume,
                        threshold,
                    },
                    error @ CheckBalanceError::Transport(_) => $error::Transport(error.to_string()),
                    CheckBalanceError::FeeCapBelowBaseFee(rejection) => $error::FeeCapBelowBaseFee(rejection),
                    error @ CheckBalanceError::InternalError(_) => $error::InternalError(error.to_string()),
                }
            }
        }

        impl From<SpendSafetyError> for $error {
            fn from(error: SpendSafetyError) -> Self {
                match error {
                    SpendSafetyError::VolumeTooLow {
                        coin,
                        volume,
                        threshold,
                    } => $error::SpendUnsafeVolume {
                        coin,
                        volume,
                        threshold,
                    },
                    error @ SpendSafetyError::EstimateUnavailable { .. } => $error::Transport(error.to_string()),
                }
            }
        }
    };
}

/// The receiver-fee conversion of the maker-order enums ([`MakerVolumeError`]); the taker enum does not read a
/// receiver fee on its own.
macro_rules! impl_maker_volume_error {
    ($error:ident) => {
        impl MakerVolumeError for $error {
            fn from_receiver_fee_error(error: TradePreimageError, coin: &str) -> Self {
                match error {
                    TradePreimageError::NotSufficientBalance {
                        coin,
                        available,
                        required,
                    } => $error::PreimageNotSufficientBalance {
                        coin,
                        available,
                        required,
                    },
                    TradePreimageError::AmountIsTooSmall { amount, threshold } => $error::PreimageAmountTooSmall {
                        coin: coin.to_owned(),
                        volume: amount,
                        threshold,
                    },
                    error @ TradePreimageError::Transport(_) => $error::Transport(error.to_string()),
                    TradePreimageError::FeeCapBelowBaseFee(rejection) => $error::FeeCapBelowBaseFee(rejection),
                    error @ TradePreimageError::InternalError(_) => $error::InternalError(error.to_string()),
                }
            }
        }
    };
}

/// The failures of `setprice` (and of [`create_maker_order`], which keeps its string error for its callers).
///
/// The classification follows the order of the checks in the order-creation core; every distinct legacy text has
/// its own variant (see the module documentation).
#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum SetPriceError {
    /// The base coin is not activated.
    #[display(fmt = "Base coin {} is not found", coin)]
    #[serde(rename = "NoSuchCoin")]
    NoSuchBaseCoin { coin: String },
    /// The rel coin is not activated.
    #[display(fmt = "Rel coin {} is not found", coin)]
    #[serde(rename = "NoSuchCoin")]
    NoSuchRelCoin { coin: String },
    /// The pair holds a coin that cannot trade (Lightning, Solana).
    #[display(fmt = "Trading is not supported for coin {}", coin)]
    TradingNotSupported { coin: String },
    /// The coin's swap key is not the key it pays from under a global-HD login (CRD ch.05 R36, R38).
    #[display(fmt = "{} cannot be swapped under this login: {}", coin, reason)]
    SwapKeyMismatch { coin: String, reason: String },
    /// The Electrum servers of a coin disagree about the chain (CRD ch.38 R38.6.4o item 12). The payload is the text.
    #[display(fmt = "{}", _0)]
    ChainDisagreement(String),
    /// The computer's clock is off by more than the limit (CRD ch.45 R45.6C.8). The payload is the refusal text as the
    /// clock check renders it.
    #[display(fmt = "{}", _0)]
    ClockOutOfRange(String),
    /// The base coin is wallet only.
    #[display(fmt = "Base coin {} is wallet only", coin)]
    #[serde(rename = "CoinIsWalletOnly")]
    BaseCoinIsWalletOnly { coin: String },
    /// The rel coin is wallet only.
    #[display(fmt = "Rel coin {} is wallet only", coin)]
    #[serde(rename = "CoinIsWalletOnly")]
    RelCoinIsWalletOnly { coin: String },
    /// Base and rel are the same coin.
    #[display(fmt = "Rel coin can not be same as base")]
    BaseEqualRel,
    #[display(
        fmt = "Not enough {} for swap: available {}, required at least {}, locked by swaps {:?}",
        coin,
        available,
        required,
        locked_by_swaps
    )]
    NotSufficientBalance {
        coin: String,
        available: BigDecimal,
        required: BigDecimal,
        #[serde(skip_serializing_if = "Option::is_none")]
        locked_by_swaps: Option<BigDecimal>,
    },
    #[display(
        fmt = "Not enough base coin {} balance for swap: available {}, required at least {}, locked by swaps {:?}",
        coin,
        available,
        required,
        locked_by_swaps
    )]
    NotSufficientBaseCoinBalance {
        coin: String,
        available: BigDecimal,
        required: BigDecimal,
        #[serde(skip_serializing_if = "Option::is_none")]
        locked_by_swaps: Option<BigDecimal>,
    },
    /// A fee-preimage reading of the receiver coin found too little balance (`max` volume).
    #[display(
        fmt = "Not enough {} to preimage the trade: available {}, required at least {}",
        coin,
        available,
        required
    )]
    #[serde(rename = "NotSufficientBalance")]
    PreimageNotSufficientBalance {
        coin: String,
        available: BigDecimal,
        required: BigDecimal,
    },
    /// The fee preimage of a swap leg is below the minimum transaction amount.
    #[display(
        fmt = "The volume {} of the {} coin less than minimum transaction amount {}",
        volume,
        coin,
        threshold
    )]
    VolumeTooLow {
        coin: String,
        volume: BigDecimal,
        threshold: BigDecimal,
    },
    /// A fee-preimage reading of the receiver coin is below the minimum transaction amount (`max` volume).
    #[display(fmt = "The amount {} less than minimum transaction amount {}", volume, threshold)]
    #[serde(rename = "VolumeTooLow")]
    PreimageAmountTooSmall {
        coin: String,
        volume: BigDecimal,
        threshold: BigDecimal,
    },
    /// The pre-trade spend-safety check refused a leg of the smallest trade (CRD ch.60 R60.6.7).
    #[display(
        fmt = "The volume {} of the {} coin is too low, required at least {}: a payment of that size could not be spent or refunded",
        volume,
        coin,
        threshold
    )]
    #[serde(rename = "VolumeTooLow")]
    SpendUnsafeVolume {
        coin: String,
        volume: BigDecimal,
        threshold: BigDecimal,
    },
    /// The maximum base volume is below the minimum trading volume of the base coin.
    #[display(fmt = "Max base vol {} is too low, required: {}", volume, threshold)]
    #[serde(rename = "VolumeTooLow")]
    MaxBaseVolTooLow {
        coin: String,
        volume: BigDecimal,
        threshold: BigDecimal,
    },
    /// The maximum rel volume is below the minimum trading volume of the rel coin.
    #[display(fmt = "Max rel vol {} is too low, required: {}", volume, threshold)]
    #[serde(rename = "VolumeTooLow")]
    RelVolTooLow {
        coin: String,
        volume: BigDecimal,
        threshold: BigDecimal,
    },
    /// The requested minimum volume is below the lowest the pair allows.
    #[display(fmt = "Min base vol {} is too low, required: {}", volume, threshold)]
    #[serde(rename = "VolumeTooLow")]
    MinBaseVolTooLow {
        coin: String,
        volume: BigDecimal,
        threshold: BigDecimal,
    },
    #[display(fmt = "Price {} is too low, required: {}", price, threshold)]
    PriceTooLow { price: BigDecimal, threshold: BigDecimal },
    /// The maximum volume of the order is below its minimum volume.
    #[display(fmt = "Max base vol {} is below min base vol: {}", max, min)]
    MaxBaseVolBelowMinBaseVol { min: BigDecimal, max: BigDecimal },
    /// A Tendermint pair whose HTLC coin holds less than the IBC routing minimum (feature `ibc-routing-for-swaps`).
    #[cfg(feature = "ibc-routing-for-swaps")]
    #[display(
        fmt = "IBC routing requires minimum balance on HTLC coin {}: required {}, current {}",
        coin,
        required,
        current
    )]
    IbcRoutingMinBalance {
        coin: String,
        required: BigDecimal,
        current: BigDecimal,
    },
    /// A node, balance or fee-estimate request failed. The payload is the whole text.
    #[display(fmt = "{}", _0)]
    Transport(String),
    /// An EVM node rejected an estimation for a fee cap below the block base fee (CRD ch.56 R5).
    #[display(fmt = "{}", _0)]
    FeeCapBelowBaseFee(FeeCapBelowBaseFee),
    /// A storage failure: the wallet storage, or the order history that the new order is saved in. The payload is the
    /// whole text.
    #[display(fmt = "{}", _0)]
    DbError(String),
    /// Anything else: a node context that cannot be had, or a state that no request can cause. The payload is the
    /// whole text.
    #[display(fmt = "{}", _0)]
    InternalError(String),
}

impl_common_order_error_conversions!(SetPriceError);
impl_maker_volume_error!(SetPriceError);

impl SetPriceError {
    /// Classifies the refusal of the order builder. `base` and `rel` name the coins whose volume limits it reports.
    pub(crate) fn from_maker_order_build_error(error: MakerOrderBuildError, base: &str, rel: &str) -> Self {
        match error {
            MakerOrderBuildError::BaseEqualRel => SetPriceError::BaseEqualRel,
            MakerOrderBuildError::TradingNotSupported { coin } => SetPriceError::TradingNotSupported { coin },
            MakerOrderBuildError::MaxBaseVolTooLow { actual, threshold } => SetPriceError::MaxBaseVolTooLow {
                coin: base.to_owned(),
                volume: actual.to_decimal(),
                threshold: threshold.to_decimal(),
            },
            MakerOrderBuildError::MinBaseVolTooLow { actual, threshold } => SetPriceError::MinBaseVolTooLow {
                coin: base.to_owned(),
                volume: actual.to_decimal(),
                threshold: threshold.to_decimal(),
            },
            MakerOrderBuildError::PriceTooLow { actual, threshold } => SetPriceError::PriceTooLow {
                price: actual.to_decimal(),
                threshold: threshold.to_decimal(),
            },
            MakerOrderBuildError::RelVolTooLow { actual, threshold } => SetPriceError::RelVolTooLow {
                coin: rel.to_owned(),
                volume: actual.to_decimal(),
                threshold: threshold.to_decimal(),
            },
            MakerOrderBuildError::MaxBaseVolBelowMinBaseVol { min, max } => SetPriceError::MaxBaseVolBelowMinBaseVol {
                min: min.to_decimal(),
                max: max.to_decimal(),
            },
            // `setprice` sets the confirmation settings before it builds the order.
            error @ MakerOrderBuildError::ConfSettingsNotSet => SetPriceError::InternalError(error.to_string()),
        }
    }
}

impl HttpStatusCode for SetPriceError {
    fn status_code(&self) -> StatusCode {
        match self {
            SetPriceError::NoSuchBaseCoin { .. }
            | SetPriceError::NoSuchRelCoin { .. }
            | SetPriceError::TradingNotSupported { .. }
            | SetPriceError::SwapKeyMismatch { .. }
            | SetPriceError::ChainDisagreement(_)
            | SetPriceError::BaseCoinIsWalletOnly { .. }
            | SetPriceError::RelCoinIsWalletOnly { .. }
            | SetPriceError::BaseEqualRel
            | SetPriceError::NotSufficientBalance { .. }
            | SetPriceError::NotSufficientBaseCoinBalance { .. }
            | SetPriceError::PreimageNotSufficientBalance { .. }
            | SetPriceError::VolumeTooLow { .. }
            | SetPriceError::PreimageAmountTooSmall { .. }
            | SetPriceError::SpendUnsafeVolume { .. }
            | SetPriceError::MaxBaseVolTooLow { .. }
            | SetPriceError::RelVolTooLow { .. }
            | SetPriceError::MinBaseVolTooLow { .. }
            | SetPriceError::PriceTooLow { .. }
            | SetPriceError::MaxBaseVolBelowMinBaseVol { .. } => StatusCode::BAD_REQUEST,
            // The node's own clock is off: not a fault of the request, and it ends by itself once a check passes.
            SetPriceError::ClockOutOfRange(_) => StatusCode::SERVICE_UNAVAILABLE,
            #[cfg(feature = "ibc-routing-for-swaps")]
            SetPriceError::IbcRoutingMinBalance { .. } => StatusCode::BAD_REQUEST,
            // Like `Transport`: a node's verdict on a fee the daemon chose, not a fault of the request.
            SetPriceError::Transport(_)
            | SetPriceError::FeeCapBelowBaseFee(_)
            | SetPriceError::DbError(_)
            | SetPriceError::InternalError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// The failures of `update_maker_order` (and of [`update_maker_order`], which keeps its string error for its
/// callers). Same conventions as [`SetPriceError`].
#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum UpdateMakerOrderError {
    /// No maker order of this node has the UUID.
    #[display(fmt = "There is no order with UUID {}", uuid)]
    UUIDNotFound { uuid: Uuid },
    /// The order has ongoing matches, so it cannot change.
    #[display(fmt = "Can't update an order that has ongoing matches")]
    OrderBeingMatched { uuid: Uuid },
    /// A coin of the order's pair is not activated any more.
    #[display(fmt = "Base coin {} and/or rel coin {} are not activated", base, rel)]
    CoinsNotActivated { base: String, rel: String },
    /// The coin's swap key is not the key it pays from under a global-HD login (CRD ch.05 R36, R38).
    #[display(fmt = "{} cannot be swapped under this login: {}", coin, reason)]
    SwapKeyMismatch { coin: String, reason: String },
    #[display(fmt = "Price {} is too low, required: {}", price, threshold)]
    PriceTooLow { price: BigDecimal, threshold: BigDecimal },
    /// The requested minimum volume is below the lowest the pair allows.
    #[display(fmt = "Min base vol {} is too low, required: {}", volume, threshold)]
    #[serde(rename = "VolumeTooLow")]
    MinBaseVolTooLow {
        coin: String,
        volume: BigDecimal,
        threshold: BigDecimal,
    },
    /// The pre-trade spend-safety check refused a leg of the smallest trade (CRD ch.60 R60.6.7).
    #[display(
        fmt = "The volume {} of the {} coin is too low, required at least {}: a payment of that size could not be spent or refunded",
        volume,
        coin,
        threshold
    )]
    #[serde(rename = "VolumeTooLow")]
    SpendUnsafeVolume {
        coin: String,
        volume: BigDecimal,
        threshold: BigDecimal,
    },
    /// The new volume (the old one plus `volume_delta`) is zero or negative.
    #[display(fmt = "New volume {} should be more than zero", volume)]
    NewVolumeNotPositive { volume: BigDecimal },
    /// The new volume does not exceed the amount reserved for the order's matches.
    #[display(
        fmt = "New volume {} should be more than reserved amount for order matches {}",
        volume,
        reserved
    )]
    NewVolumeBelowReserved { volume: BigDecimal, reserved: BigDecimal },
    #[display(
        fmt = "Not enough {} for swap: available {}, required at least {}, locked by swaps {:?}",
        coin,
        available,
        required,
        locked_by_swaps
    )]
    NotSufficientBalance {
        coin: String,
        available: BigDecimal,
        required: BigDecimal,
        #[serde(skip_serializing_if = "Option::is_none")]
        locked_by_swaps: Option<BigDecimal>,
    },
    #[display(
        fmt = "Not enough base coin {} balance for swap: available {}, required at least {}, locked by swaps {:?}",
        coin,
        available,
        required,
        locked_by_swaps
    )]
    NotSufficientBaseCoinBalance {
        coin: String,
        available: BigDecimal,
        required: BigDecimal,
        #[serde(skip_serializing_if = "Option::is_none")]
        locked_by_swaps: Option<BigDecimal>,
    },
    /// A fee-preimage reading of the receiver coin found too little balance (`max` volume).
    #[display(
        fmt = "Not enough {} to preimage the trade: available {}, required at least {}",
        coin,
        available,
        required
    )]
    #[serde(rename = "NotSufficientBalance")]
    PreimageNotSufficientBalance {
        coin: String,
        available: BigDecimal,
        required: BigDecimal,
    },
    /// The fee preimage of a swap leg is below the minimum transaction amount.
    #[display(
        fmt = "The volume {} of the {} coin less than minimum transaction amount {}",
        volume,
        coin,
        threshold
    )]
    VolumeTooLow {
        coin: String,
        volume: BigDecimal,
        threshold: BigDecimal,
    },
    /// A fee-preimage reading of the receiver coin is below the minimum transaction amount (`max` volume).
    #[display(fmt = "The amount {} less than minimum transaction amount {}", volume, threshold)]
    #[serde(rename = "VolumeTooLow")]
    PreimageAmountTooSmall {
        coin: String,
        volume: BigDecimal,
        threshold: BigDecimal,
    },
    /// The maximum volume of the updated order is below its minimum volume.
    #[display(fmt = "Max base vol {} is below min base vol: {}", max, min)]
    MaxBaseVolBelowMinBaseVol { min: BigDecimal, max: BigDecimal },
    /// The maximum volume of the updated order is below the minimum trading volume of the base coin.
    #[display(fmt = "Max base vol {} is too low, required: {}", volume, threshold)]
    #[serde(rename = "VolumeTooLow")]
    MaxBaseVolTooLow {
        coin: String,
        volume: BigDecimal,
        threshold: BigDecimal,
    },
    /// The maximum rel volume of the updated order is below the minimum trading volume of the rel coin.
    #[display(fmt = "Max rel vol {} is too low, required: {}", volume, threshold)]
    #[serde(rename = "VolumeTooLow")]
    RelVolTooLow {
        coin: String,
        volume: BigDecimal,
        threshold: BigDecimal,
    },
    /// The order left the container while the update was checked, or while it was applied.
    #[display(fmt = "Order with UUID: {} has been deleted", uuid)]
    OrderDeleted { uuid: Uuid },
    /// The order changed between the checks and the write; the client may retry.
    #[display(
        fmt = "Order state has changed after price/volume/balance checks. Please try to update the order again if it's still needed."
    )]
    OrderStateChanged,
    /// A node, balance or fee-estimate request failed. The payload is the whole text.
    #[display(fmt = "{}", _0)]
    Transport(String),
    /// An EVM node rejected an estimation for a fee cap below the block base fee (CRD ch.56 R5).
    #[display(fmt = "{}", _0)]
    FeeCapBelowBaseFee(FeeCapBelowBaseFee),
    /// A storage failure: the wallet storage, or the write of the updated order. The payload is the whole text.
    #[display(fmt = "{}", _0)]
    DbError(String),
    /// Anything else: a node context that cannot be had, or a state that no request can cause. The payload is the
    /// whole text.
    #[display(fmt = "{}", _0)]
    InternalError(String),
}

impl_common_order_error_conversions!(UpdateMakerOrderError);
impl_maker_volume_error!(UpdateMakerOrderError);

impl UpdateMakerOrderError {
    /// Classifies the refusal of an order validator. `base` and `rel` name the coins whose volume limits it reports.
    pub(crate) fn from_maker_order_build_error(error: MakerOrderBuildError, base: &str, rel: &str) -> Self {
        match error {
            MakerOrderBuildError::MaxBaseVolTooLow { actual, threshold } => UpdateMakerOrderError::MaxBaseVolTooLow {
                coin: base.to_owned(),
                volume: actual.to_decimal(),
                threshold: threshold.to_decimal(),
            },
            MakerOrderBuildError::MinBaseVolTooLow { actual, threshold } => UpdateMakerOrderError::MinBaseVolTooLow {
                coin: base.to_owned(),
                volume: actual.to_decimal(),
                threshold: threshold.to_decimal(),
            },
            MakerOrderBuildError::PriceTooLow { actual, threshold } => UpdateMakerOrderError::PriceTooLow {
                price: actual.to_decimal(),
                threshold: threshold.to_decimal(),
            },
            MakerOrderBuildError::RelVolTooLow { actual, threshold } => UpdateMakerOrderError::RelVolTooLow {
                coin: rel.to_owned(),
                volume: actual.to_decimal(),
                threshold: threshold.to_decimal(),
            },
            MakerOrderBuildError::MaxBaseVolBelowMinBaseVol { min, max } => {
                UpdateMakerOrderError::MaxBaseVolBelowMinBaseVol {
                    min: min.to_decimal(),
                    max: max.to_decimal(),
                }
            },
            // The validators of an update (`validate_price`, `validate_and_get_min_vol`, `validate_max_vol`) never
            // return these.
            error @ (MakerOrderBuildError::BaseEqualRel
            | MakerOrderBuildError::TradingNotSupported { .. }
            | MakerOrderBuildError::ConfSettingsNotSet) => UpdateMakerOrderError::InternalError(error.to_string()),
        }
    }
}

impl HttpStatusCode for UpdateMakerOrderError {
    fn status_code(&self) -> StatusCode {
        match self {
            UpdateMakerOrderError::UUIDNotFound { .. }
            | UpdateMakerOrderError::OrderBeingMatched { .. }
            | UpdateMakerOrderError::CoinsNotActivated { .. }
            | UpdateMakerOrderError::SwapKeyMismatch { .. }
            | UpdateMakerOrderError::PriceTooLow { .. }
            | UpdateMakerOrderError::MinBaseVolTooLow { .. }
            | UpdateMakerOrderError::SpendUnsafeVolume { .. }
            | UpdateMakerOrderError::NewVolumeNotPositive { .. }
            | UpdateMakerOrderError::NewVolumeBelowReserved { .. }
            | UpdateMakerOrderError::NotSufficientBalance { .. }
            | UpdateMakerOrderError::NotSufficientBaseCoinBalance { .. }
            | UpdateMakerOrderError::PreimageNotSufficientBalance { .. }
            | UpdateMakerOrderError::VolumeTooLow { .. }
            | UpdateMakerOrderError::PreimageAmountTooSmall { .. }
            | UpdateMakerOrderError::MaxBaseVolBelowMinBaseVol { .. }
            | UpdateMakerOrderError::MaxBaseVolTooLow { .. }
            | UpdateMakerOrderError::RelVolTooLow { .. }
            | UpdateMakerOrderError::OrderDeleted { .. }
            | UpdateMakerOrderError::OrderStateChanged => StatusCode::BAD_REQUEST,
            // Like `Transport`: a node's verdict on a fee the daemon chose, not a fault of the request.
            UpdateMakerOrderError::Transport(_)
            | UpdateMakerOrderError::FeeCapBelowBaseFee(_)
            | UpdateMakerOrderError::DbError(_)
            | UpdateMakerOrderError::InternalError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// The failures of `my_orders`: the only one is a node context that cannot be had.
#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum MyOrdersRpcError {
    /// The order-matching context of the node could not be obtained. The payload is the whole text.
    #[display(fmt = "{}", _0)]
    InternalError(String),
}

impl HttpStatusCode for MyOrdersRpcError {
    fn status_code(&self) -> StatusCode {
        match self {
            MyOrdersRpcError::InternalError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// The failures of `order_status`.
#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum OrderStatusError {
    /// Neither the active orders nor the order history know the UUID.
    #[display(fmt = "Order with uuid {} is not found", uuid)]
    UUIDNotFound { uuid: Uuid },
    /// The order history could not be read. The payload is the whole text.
    #[display(fmt = "{}", _0)]
    DbError(String),
    /// The order-matching context of the node could not be obtained. The payload is the whole text.
    #[display(fmt = "{}", _0)]
    InternalError(String),
}

impl From<my_orders_storage::MyOrdersError> for OrderStatusError {
    fn from(error: my_orders_storage::MyOrdersError) -> Self {
        match error {
            my_orders_storage::MyOrdersError::NoSuchOrder { uuid } => OrderStatusError::UUIDNotFound { uuid },
            error => OrderStatusError::DbError(error.to_string()),
        }
    }
}

impl HttpStatusCode for OrderStatusError {
    fn status_code(&self) -> StatusCode {
        match self {
            OrderStatusError::UUIDNotFound { .. } => StatusCode::BAD_REQUEST,
            OrderStatusError::DbError(_) | OrderStatusError::InternalError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// The failures of `buy` and `sell` (and of [`lp_auto_buy`], which keeps its string error for its callers). Both
/// methods fail alike; the one difference is the order of the two coin lookups (see [`buy_core`], [`sell_core`]).
///
/// The classification follows the order of the checks of the taker-order core; every distinct legacy text has its own
/// variant (see the module documentation).
#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum TakerOrderError {
    /// Base and rel are the same coin (the check of the request).
    #[display(fmt = "Base and rel must be different coins")]
    BaseEqualRel,
    /// The base coin is not activated. The legacy text does not name the coin.
    #[display(fmt = "Base coin is not found or inactive")]
    #[serde(rename = "NoSuchCoin")]
    NoSuchBaseCoin { coin: String },
    /// The rel coin is not activated. The legacy text does not name the coin.
    #[display(fmt = "Rel coin is not found or inactive")]
    #[serde(rename = "NoSuchCoin")]
    NoSuchRelCoin { coin: String },
    /// The pair holds a coin that cannot trade (Lightning, Solana).
    #[display(fmt = "Trading is not supported for coin {}", coin)]
    TradingNotSupported { coin: String },
    /// The coin's swap key is not the key it pays from under a global-HD login (CRD ch.05 R36, R38).
    #[display(fmt = "{} cannot be swapped under this login: {}", coin, reason)]
    SwapKeyMismatch { coin: String, reason: String },
    /// The Electrum servers of a coin disagree about the chain (CRD ch.38 R38.6.4o item 12). The payload is the text.
    #[display(fmt = "{}", _0)]
    ChainDisagreement(String),
    /// The computer's clock is off by more than the limit (CRD ch.45 R45.6C.8). The payload is the refusal text as the
    /// clock check renders it.
    #[display(fmt = "{}", _0)]
    ClockOutOfRange(String),
    /// The base coin is wallet only.
    #[display(fmt = "Base coin {} is wallet only", coin)]
    #[serde(rename = "CoinIsWalletOnly")]
    BaseCoinIsWalletOnly { coin: String },
    /// The rel coin is wallet only.
    #[display(fmt = "Rel coin {} is wallet only", coin)]
    #[serde(rename = "CoinIsWalletOnly")]
    RelCoinIsWalletOnly { coin: String },
    #[display(
        fmt = "Not enough {} for swap: available {}, required at least {}, locked by swaps {:?}",
        coin,
        available,
        required,
        locked_by_swaps
    )]
    NotSufficientBalance {
        coin: String,
        available: BigDecimal,
        required: BigDecimal,
        #[serde(skip_serializing_if = "Option::is_none")]
        locked_by_swaps: Option<BigDecimal>,
    },
    #[display(
        fmt = "Not enough base coin {} balance for swap: available {}, required at least {}, locked by swaps {:?}",
        coin,
        available,
        required,
        locked_by_swaps
    )]
    NotSufficientBaseCoinBalance {
        coin: String,
        available: BigDecimal,
        required: BigDecimal,
        #[serde(skip_serializing_if = "Option::is_none")]
        locked_by_swaps: Option<BigDecimal>,
    },
    /// The fee preimage of a swap leg is below the minimum transaction amount.
    #[display(
        fmt = "The volume {} of the {} coin less than minimum transaction amount {}",
        volume,
        coin,
        threshold
    )]
    VolumeTooLow {
        coin: String,
        volume: BigDecimal,
        threshold: BigDecimal,
    },
    /// The node's own orderbook holds every order that `match_by` names, and none of them declares a capability
    /// that `coin` requires of a counterparty, so none can be matched (CRD ch.59 R59.4C.6). `coin` and `capability`
    /// are those of the named order with the lowest identifier; `orders` are the named identifiers in ascending
    /// order.
    #[display(
        fmt = "None of the orders named in match_by can be matched: {} requires a counterparty that declares {}",
        coin,
        capability
    )]
    OrderNotMatchableMissingCapability {
        coin: String,
        capability: String,
        orders: Vec<Uuid>,
    },
    /// The pre-trade spend-safety check refused a leg of the smallest trade (CRD ch.60 R60.6.7).
    #[display(
        fmt = "The volume {} of the {} coin is too low, required at least {}: a payment of that size could not be spent or refunded",
        volume,
        coin,
        threshold
    )]
    #[serde(rename = "VolumeTooLow")]
    SpendUnsafeVolume {
        coin: String,
        volume: BigDecimal,
        threshold: BigDecimal,
    },
    /// The price is below the lowest a taker order accepts. The text carries the fixed minimum, which is also the
    /// `threshold`.
    #[display(fmt = "Price is too low, minimum is 0.00000001")]
    PriceTooLow { price: BigDecimal, threshold: BigDecimal },
    /// The order builder found the same ticker on both sides although the request named two coins.
    #[display(fmt = "Rel coin can not be same as base")]
    #[serde(rename = "BaseEqualRel")]
    RelCoinSameAsBase,
    /// The base amount of the order is below the minimum trading volume of the base coin.
    #[display(fmt = "Base amount {} is too low, required: {}", volume, threshold)]
    #[serde(rename = "VolumeTooLow")]
    BaseAmountTooLow {
        coin: String,
        volume: BigDecimal,
        threshold: BigDecimal,
    },
    /// The rel amount of the order (the base amount times the price) is below the minimum trading volume of the rel
    /// coin.
    #[display(fmt = "Rel amount {} is too low, required: {}", volume, threshold)]
    #[serde(rename = "VolumeTooLow")]
    RelAmountTooLow {
        coin: String,
        volume: BigDecimal,
        threshold: BigDecimal,
    },
    /// The requested minimum volume is below the lowest the pair allows.
    #[display(fmt = "Min volume {} is too low, required: {}", volume, threshold)]
    #[serde(rename = "VolumeTooLow")]
    MinVolumeTooLow {
        coin: String,
        volume: BigDecimal,
        threshold: BigDecimal,
    },
    /// The volume of the order is below its minimum volume.
    #[display(fmt = "Max base vol {} is below min base vol: {}", max, min)]
    MaxBaseVolBelowMinBaseVol { min: BigDecimal, max: BigDecimal },
    /// A node, balance or fee-estimate request failed. The payload is the whole text.
    #[display(fmt = "{}", _0)]
    Transport(String),
    /// An EVM node rejected an estimation for a fee cap below the block base fee (CRD ch.56 R5).
    #[display(fmt = "{}", _0)]
    FeeCapBelowBaseFee(FeeCapBelowBaseFee),
    /// A storage failure: the wallet storage, or the order history that the new order is saved in. The payload is the
    /// whole text.
    #[display(fmt = "{}", _0)]
    DbError(String),
    /// Anything else: a node context that cannot be had, or a state that no request can cause. The payload is the
    /// whole text.
    #[display(fmt = "{}", _0)]
    InternalError(String),
}

impl_common_order_error_conversions!(TakerOrderError);

impl From<capability_presentation::NamedOrdersRefusal> for TakerOrderError {
    fn from(refusal: capability_presentation::NamedOrdersRefusal) -> Self {
        TakerOrderError::OrderNotMatchableMissingCapability {
            coin: refusal.coin,
            capability: refusal.capability.to_owned(),
            orders: refusal.orders,
        }
    }
}

impl TakerOrderError {
    /// Classifies the refusal of the taker-order builder. `base` and `rel` name the coins whose amounts it reports.
    pub(crate) fn from_taker_order_build_error(error: TakerOrderBuildError, base: &str, rel: &str) -> Self {
        match error {
            TakerOrderBuildError::BaseEqualRel => TakerOrderError::RelCoinSameAsBase,
            TakerOrderBuildError::TradingNotSupported { coin } => TakerOrderError::TradingNotSupported { coin },
            TakerOrderBuildError::BaseAmountTooLow { actual, threshold } => TakerOrderError::BaseAmountTooLow {
                coin: base.to_owned(),
                volume: actual.to_decimal(),
                threshold: threshold.to_decimal(),
            },
            TakerOrderBuildError::RelAmountTooLow { actual, threshold } => TakerOrderError::RelAmountTooLow {
                coin: rel.to_owned(),
                volume: actual.to_decimal(),
                threshold: threshold.to_decimal(),
            },
            TakerOrderBuildError::MinVolumeTooLow { actual, threshold } => TakerOrderError::MinVolumeTooLow {
                coin: base.to_owned(),
                volume: actual.to_decimal(),
                threshold: threshold.to_decimal(),
            },
            TakerOrderBuildError::MaxBaseVolBelowMinBaseVol { min, max } => {
                TakerOrderError::MaxBaseVolBelowMinBaseVol {
                    min: min.to_decimal(),
                    max: max.to_decimal(),
                }
            },
            // The sender key is the node's own and the confirmation settings are set before the builder runs: no
            // request can cause either.
            error @ (TakerOrderBuildError::SenderPubkeyIsZero | TakerOrderBuildError::ConfsSettingsNotSet) => {
                TakerOrderError::InternalError(error.to_string())
            },
        }
    }
}

impl HttpStatusCode for TakerOrderError {
    fn status_code(&self) -> StatusCode {
        match self {
            TakerOrderError::BaseEqualRel
            | TakerOrderError::NoSuchBaseCoin { .. }
            | TakerOrderError::NoSuchRelCoin { .. }
            | TakerOrderError::TradingNotSupported { .. }
            | TakerOrderError::SwapKeyMismatch { .. }
            | TakerOrderError::ChainDisagreement(_)
            | TakerOrderError::BaseCoinIsWalletOnly { .. }
            | TakerOrderError::RelCoinIsWalletOnly { .. }
            | TakerOrderError::NotSufficientBalance { .. }
            | TakerOrderError::NotSufficientBaseCoinBalance { .. }
            | TakerOrderError::VolumeTooLow { .. }
            | TakerOrderError::OrderNotMatchableMissingCapability { .. }
            | TakerOrderError::SpendUnsafeVolume { .. }
            | TakerOrderError::PriceTooLow { .. }
            | TakerOrderError::RelCoinSameAsBase
            | TakerOrderError::BaseAmountTooLow { .. }
            | TakerOrderError::RelAmountTooLow { .. }
            | TakerOrderError::MinVolumeTooLow { .. }
            | TakerOrderError::MaxBaseVolBelowMinBaseVol { .. } => StatusCode::BAD_REQUEST,
            // The node's own clock is off: not a fault of the request, and it ends by itself once a check passes.
            TakerOrderError::ClockOutOfRange(_) => StatusCode::SERVICE_UNAVAILABLE,
            // Like `Transport`: a node's verdict on a fee the daemon chose, not a fault of the request.
            TakerOrderError::Transport(_)
            | TakerOrderError::FeeCapBelowBaseFee(_)
            | TakerOrderError::DbError(_)
            | TakerOrderError::InternalError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// The failures of `orderbook_depth`. The tokens and payload kinds are those of the nearest versioned sibling,
/// `orderbook` (`OrderbookRpcError`): every payload is the whole legacy text.
#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum OrderbookDepthError {
    /// A requested pair holds a wallet-only coin. The payload is the legacy text, which lists every such pair as the
    /// request spelled it.
    #[display(fmt = "{}", _0)]
    CoinIsWalletOnly(String),
    /// The request to a relay for the depth of the pairs the node holds no orderbook for failed. The payload is the
    /// whole text.
    #[display(fmt = "{}", _0)]
    P2PSubscribeError(String),
    /// The order-matching context of the node could not be obtained, or the node's own key could not be read. The
    /// payload is the whole text.
    #[display(fmt = "{}", _0)]
    Internal(String),
}

impl HttpStatusCode for OrderbookDepthError {
    fn status_code(&self) -> StatusCode {
        match self {
            OrderbookDepthError::CoinIsWalletOnly(_) => StatusCode::BAD_REQUEST,
            OrderbookDepthError::P2PSubscribeError(_) | OrderbookDepthError::Internal(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            },
        }
    }
}

/// The failures of `orders_history_by_filter`.
#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum OrdersHistoryError {
    /// The order history could not be read. The payload is the whole text.
    #[display(fmt = "{}", _0)]
    DbError(String),
    /// The order-matching context of the node could not be obtained. The payload is the whole text.
    #[display(fmt = "{}", _0)]
    InternalError(String),
}

impl HttpStatusCode for OrdersHistoryError {
    fn status_code(&self) -> StatusCode {
        match self {
            OrdersHistoryError::DbError(_) | OrdersHistoryError::InternalError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}
