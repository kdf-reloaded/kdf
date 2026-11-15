//! Tests of the typed cores of the group-A order-management RPC methods (CRD ch.43 §43.1A; plan
//! `mmrpc-v2-completion`, Part 1 §1.2 items 2-3): `setprice`, `update_maker_order`, `cancel_order`,
//! `cancel_all_orders`, `my_orders` and `order_status`.
//!
//! * The legacy arms keep their answers. Every expected legacy body below is the body that the handler of the dev
//!   branch (`d4928e1fb`) produced, derived from its code and marked with the handler that produced it; the
//!   expected error texts are those of its `ERR!`/`try_s!`/`ERRL!` sites. The `file:line] ` tokens in front of a
//!   legacy error text change with every edit of the source, so they are split off, checked to be present and to name
//!   `ordermatch_trading.rs`, and the text behind them is compared whole ([`assert_legacy_error`]).
//! * Every variant of every error enum has its serialized `error_type` token, its `error_data` JSON type, its HTTP
//!   status and its `Display` text checked (CRD ch.43 R43.8.6, the form of the tests of PR #312).
//! * Nothing here touches the versioned envelope: no method is registered on it yet.

use super::*;
use crate::mm2::database::my_orders::CREATE_MY_ORDERS_TABLE;
use crate::mm2::lp_network::P2PContext;
use crate::mm2::lp_swap::CheckBalanceError;
use coins::{BalanceError, CoinBalance, CoinsContext, FeeCapBelowBaseFee, MarketCoinOps, MmCoin, TestCoin, TradeFee,
            TradePreimageError, UnexpectedDerivationMethod};
use common::block_on;
use crypto::privkey::key_pair_from_seed;
use db_common::sqlite::rusqlite::Connection;
use futures::channel::mpsc;
use mm2_core::mm_ctx::MmCtxBuilder;
use mm2_p2p::atomicdex_behaviour::AdexBehaviourCmd;
use mocktopus::mocking::*;
use std::sync::Mutex;

fn bd(s: &str) -> BigDecimal { s.parse().unwrap() }

/// CRD ch.43 R43.8.6: the serialized `error_type` token, the JSON type of `error_data` (`None`: the key is absent), the
/// HTTP status and the `Display` text of an error.
fn assert_wire<E>(error: &E, token: &str, data: Option<&str>, status: u16, text: &str)
where
    E: serde::Serialize + HttpStatusCode + std::fmt::Display,
{
    let wire = json::to_value(error).unwrap();
    assert_eq!(wire["error_type"], token, "{text}: {wire}");
    match data {
        Some("object") => assert!(wire["error_data"].is_object(), "{text}: {wire}"),
        Some("string") => assert!(wire["error_data"].is_string(), "{text}: {wire}"),
        Some(other) => panic!("unknown kind of error_data: {other}"),
        None => assert!(wire.get("error_data").is_none(), "{text}: {wire}"),
    }
    assert_eq!(error.status_code().as_u16(), status, "{text}");
    assert_eq!(error.to_string(), text);
}

fn token_of<E: serde::Serialize>(error: &E) -> String {
    json::to_value(error).unwrap()["error_type"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Splits the `file:line] ` tokens that `try_s!` and `ERR!` put in front of a legacy error text.
fn split_location(error: &str) -> (Vec<String>, String) {
    let token = regex::Regex::new(r"^([A-Za-z0-9_]+):\d+\] ").unwrap();
    let mut files = Vec::new();
    let mut rest = error;
    while let Some(found) = token.captures(rest) {
        files.push(found[1].to_owned());
        rest = &rest[found[0].len()..];
    }
    (files, rest.to_owned())
}

/// A legacy error: one `file:line] ` token that names `ordermatch_trading` (`gstuff::filename` drops the extension), then exactly `text`.
fn assert_legacy_error(error: &str, text: &str) {
    let (files, rest) = split_location(error);
    assert_eq!(files, vec!["ordermatch_trading".to_owned()], "{error}");
    assert_eq!(rest, text, "{error}");
}

fn body_text(answer: Response<Vec<u8>>) -> String { String::from_utf8(answer.into_body()).unwrap() }

// ----- The wire forms of the error enums -----

#[test]
fn set_price_error_wire_forms() {
    let rejection = FeeCapBelowBaseFee {
        max_fee_per_gas: None,
        base_fee: None,
        node: None,
    };
    let rejection_text = rejection.to_string();
    let cases: Vec<(SetPriceError, &str, Option<&str>, u16, String)> = vec![
        (
            SetPriceError::NoSuchBaseCoin { coin: "BTC".into() },
            "NoSuchCoin",
            Some("object"),
            400,
            "Base coin BTC is not found".into(),
        ),
        (
            SetPriceError::NoSuchRelCoin { coin: "BTC".into() },
            "NoSuchCoin",
            Some("object"),
            400,
            "Rel coin BTC is not found".into(),
        ),
        (
            SetPriceError::TradingNotSupported {
                coin: "tBTC-lightning".into(),
            },
            "TradingNotSupported",
            Some("object"),
            400,
            "Trading is not supported for coin tBTC-lightning".into(),
        ),
        (
            SetPriceError::SwapKeyMismatch {
                coin: "ETH".into(),
                reason: "a reason".into(),
            },
            "SwapKeyMismatch",
            Some("object"),
            400,
            "ETH cannot be swapped under this login: a reason".into(),
        ),
        (
            SetPriceError::ChainDisagreement("ETH: the Electrum servers disagree".into()),
            "ChainDisagreement",
            Some("string"),
            400,
            "ETH: the Electrum servers disagree".into(),
        ),
        (
            SetPriceError::ClockOutOfRange("The computer's clock is off by 12.50 s".into()),
            "ClockOutOfRange",
            Some("string"),
            503,
            "The computer's clock is off by 12.50 s".into(),
        ),
        (
            SetPriceError::BaseCoinIsWalletOnly { coin: "ETH".into() },
            "CoinIsWalletOnly",
            Some("object"),
            400,
            "Base coin ETH is wallet only".into(),
        ),
        (
            SetPriceError::RelCoinIsWalletOnly { coin: "ETH".into() },
            "CoinIsWalletOnly",
            Some("object"),
            400,
            "Rel coin ETH is wallet only".into(),
        ),
        (
            SetPriceError::BaseEqualRel,
            "BaseEqualRel",
            None,
            400,
            "Rel coin can not be same as base".into(),
        ),
        (
            SetPriceError::NotSufficientBalance {
                coin: "KMD".into(),
                available: bd("1"),
                required: bd("2"),
                locked_by_swaps: None,
            },
            "NotSufficientBalance",
            Some("object"),
            400,
            "Not enough KMD for swap: available 1, required at least 2, locked by swaps None".into(),
        ),
        (
            SetPriceError::NotSufficientBaseCoinBalance {
                coin: "KMD".into(),
                available: bd("1"),
                required: bd("2"),
                locked_by_swaps: None,
            },
            "NotSufficientBaseCoinBalance",
            Some("object"),
            400,
            "Not enough base coin KMD balance for swap: available 1, required at least 2, locked by swaps None".into(),
        ),
        (
            SetPriceError::PreimageNotSufficientBalance {
                coin: "KMD".into(),
                available: bd("1"),
                required: bd("2"),
            },
            "NotSufficientBalance",
            Some("object"),
            400,
            "Not enough KMD to preimage the trade: available 1, required at least 2".into(),
        ),
        (
            SetPriceError::VolumeTooLow {
                coin: "KMD".into(),
                volume: bd("1"),
                threshold: bd("2"),
            },
            "VolumeTooLow",
            Some("object"),
            400,
            "The volume 1 of the KMD coin less than minimum transaction amount 2".into(),
        ),
        (
            SetPriceError::PreimageAmountTooSmall {
                coin: "KMD".into(),
                volume: bd("1"),
                threshold: bd("2"),
            },
            "VolumeTooLow",
            Some("object"),
            400,
            "The amount 1 less than minimum transaction amount 2".into(),
        ),
        (
            SetPriceError::SpendUnsafeVolume {
                coin: "KMD".into(),
                volume: bd("1"),
                threshold: bd("2"),
            },
            "VolumeTooLow",
            Some("object"),
            400,
            "The volume 1 of the KMD coin is too low, required at least 2: a payment of that size could not be spent or refunded".into(),
        ),
        (
            SetPriceError::MaxBaseVolTooLow {
                coin: "DYN".into(),
                volume: bd("1"),
                threshold: bd("2"),
            },
            "VolumeTooLow",
            Some("object"),
            400,
            "Max base vol 1 is too low, required: 2".into(),
        ),
        (
            SetPriceError::RelVolTooLow {
                coin: "KMD".into(),
                volume: bd("1"),
                threshold: bd("2"),
            },
            "VolumeTooLow",
            Some("object"),
            400,
            "Max rel vol 1 is too low, required: 2".into(),
        ),
        (
            SetPriceError::MinBaseVolTooLow {
                coin: "DYN".into(),
                volume: bd("1"),
                threshold: bd("2"),
            },
            "VolumeTooLow",
            Some("object"),
            400,
            "Min base vol 1 is too low, required: 2".into(),
        ),
        (
            SetPriceError::PriceTooLow {
                price: bd("1"),
                threshold: bd("2"),
            },
            "PriceTooLow",
            Some("object"),
            400,
            "Price 1 is too low, required: 2".into(),
        ),
        (
            SetPriceError::MaxBaseVolBelowMinBaseVol {
                min: bd("2"),
                max: bd("1"),
            },
            "MaxBaseVolBelowMinBaseVol",
            Some("object"),
            400,
            "Max base vol 1 is below min base vol: 2".into(),
        ),
        (
            SetPriceError::Transport("Transport: no route".into()),
            "Transport",
            Some("string"),
            500,
            "Transport: no route".into(),
        ),
        (
            SetPriceError::FeeCapBelowBaseFee(rejection),
            "FeeCapBelowBaseFee",
            Some("object"),
            500,
            rejection_text,
        ),
        (
            SetPriceError::DbError("Error saving an order: disk full".into()),
            "DbError",
            Some("string"),
            500,
            "Error saving an order: disk full".into(),
        ),
        (
            SetPriceError::InternalError("lp_coins_ops.rs:1] no context".into()),
            "InternalError",
            Some("string"),
            500,
            "lp_coins_ops.rs:1] no context".into(),
        ),
    ];
    for (error, token, data, status, text) in cases {
        assert_wire(&error, token, data, status, &text);
    }
}

#[test]
fn update_maker_order_error_wire_forms() {
    let uuid = Uuid::new_v4();
    let rejection = FeeCapBelowBaseFee {
        max_fee_per_gas: None,
        base_fee: None,
        node: None,
    };
    let rejection_text = rejection.to_string();
    let cases: Vec<(UpdateMakerOrderError, &str, Option<&str>, u16, String)> = vec![
        (
            UpdateMakerOrderError::UUIDNotFound { uuid },
            "UUIDNotFound",
            Some("object"),
            400,
            format!("There is no order with UUID {uuid}"),
        ),
        (
            UpdateMakerOrderError::OrderBeingMatched { uuid },
            "OrderBeingMatched",
            Some("object"),
            400,
            "Can't update an order that has ongoing matches".into(),
        ),
        (
            UpdateMakerOrderError::CoinsNotActivated {
                base: "DYN".into(),
                rel: "KMD".into(),
            },
            "CoinsNotActivated",
            Some("object"),
            400,
            "Base coin DYN and/or rel coin KMD are not activated".into(),
        ),
        (
            UpdateMakerOrderError::SwapKeyMismatch {
                coin: "ETH".into(),
                reason: "a reason".into(),
            },
            "SwapKeyMismatch",
            Some("object"),
            400,
            "ETH cannot be swapped under this login: a reason".into(),
        ),
        (
            UpdateMakerOrderError::PriceTooLow {
                price: bd("1"),
                threshold: bd("2"),
            },
            "PriceTooLow",
            Some("object"),
            400,
            "Price 1 is too low, required: 2".into(),
        ),
        (
            UpdateMakerOrderError::MinBaseVolTooLow {
                coin: "DYN".into(),
                volume: bd("1"),
                threshold: bd("2"),
            },
            "VolumeTooLow",
            Some("object"),
            400,
            "Min base vol 1 is too low, required: 2".into(),
        ),
        (
            UpdateMakerOrderError::SpendUnsafeVolume {
                coin: "KMD".into(),
                volume: bd("1"),
                threshold: bd("2"),
            },
            "VolumeTooLow",
            Some("object"),
            400,
            "The volume 1 of the KMD coin is too low, required at least 2: a payment of that size could not be spent or refunded".into(),
        ),
        (
            UpdateMakerOrderError::NewVolumeNotPositive { volume: bd("0") },
            "NewVolumeNotPositive",
            Some("object"),
            400,
            "New volume 0 should be more than zero".into(),
        ),
        (
            UpdateMakerOrderError::NewVolumeBelowReserved {
                volume: bd("1"),
                reserved: bd("2"),
            },
            "NewVolumeBelowReserved",
            Some("object"),
            400,
            "New volume 1 should be more than reserved amount for order matches 2".into(),
        ),
        (
            UpdateMakerOrderError::NotSufficientBalance {
                coin: "KMD".into(),
                available: bd("1"),
                required: bd("2"),
                locked_by_swaps: None,
            },
            "NotSufficientBalance",
            Some("object"),
            400,
            "Not enough KMD for swap: available 1, required at least 2, locked by swaps None".into(),
        ),
        (
            UpdateMakerOrderError::NotSufficientBaseCoinBalance {
                coin: "KMD".into(),
                available: bd("1"),
                required: bd("2"),
                locked_by_swaps: None,
            },
            "NotSufficientBaseCoinBalance",
            Some("object"),
            400,
            "Not enough base coin KMD balance for swap: available 1, required at least 2, locked by swaps None".into(),
        ),
        (
            UpdateMakerOrderError::PreimageNotSufficientBalance {
                coin: "KMD".into(),
                available: bd("1"),
                required: bd("2"),
            },
            "NotSufficientBalance",
            Some("object"),
            400,
            "Not enough KMD to preimage the trade: available 1, required at least 2".into(),
        ),
        (
            UpdateMakerOrderError::VolumeTooLow {
                coin: "KMD".into(),
                volume: bd("1"),
                threshold: bd("2"),
            },
            "VolumeTooLow",
            Some("object"),
            400,
            "The volume 1 of the KMD coin less than minimum transaction amount 2".into(),
        ),
        (
            UpdateMakerOrderError::PreimageAmountTooSmall {
                coin: "KMD".into(),
                volume: bd("1"),
                threshold: bd("2"),
            },
            "VolumeTooLow",
            Some("object"),
            400,
            "The amount 1 less than minimum transaction amount 2".into(),
        ),
        (
            UpdateMakerOrderError::MaxBaseVolBelowMinBaseVol {
                min: bd("2"),
                max: bd("1"),
            },
            "MaxBaseVolBelowMinBaseVol",
            Some("object"),
            400,
            "Max base vol 1 is below min base vol: 2".into(),
        ),
        (
            UpdateMakerOrderError::MaxBaseVolTooLow {
                coin: "DYN".into(),
                volume: bd("1"),
                threshold: bd("2"),
            },
            "VolumeTooLow",
            Some("object"),
            400,
            "Max base vol 1 is too low, required: 2".into(),
        ),
        (
            UpdateMakerOrderError::RelVolTooLow {
                coin: "KMD".into(),
                volume: bd("1"),
                threshold: bd("2"),
            },
            "VolumeTooLow",
            Some("object"),
            400,
            "Max rel vol 1 is too low, required: 2".into(),
        ),
        (
            UpdateMakerOrderError::OrderDeleted { uuid },
            "OrderDeleted",
            Some("object"),
            400,
            format!("Order with UUID: {uuid} has been deleted"),
        ),
        (
            UpdateMakerOrderError::OrderStateChanged,
            "OrderStateChanged",
            None,
            400,
            "Order state has changed after price/volume/balance checks. Please try to update the order again if it's still needed.".into(),
        ),
        (
            UpdateMakerOrderError::Transport("Transport: no route".into()),
            "Transport",
            Some("string"),
            500,
            "Transport: no route".into(),
        ),
        (
            UpdateMakerOrderError::FeeCapBelowBaseFee(rejection),
            "FeeCapBelowBaseFee",
            Some("object"),
            500,
            rejection_text,
        ),
        (
            UpdateMakerOrderError::DbError("Error on saving updated order state to database:disk full".into()),
            "DbError",
            Some("string"),
            500,
            "Error on saving updated order state to database:disk full".into(),
        ),
        (
            UpdateMakerOrderError::InternalError("lp_ordermatch.rs:1] no context".into()),
            "InternalError",
            Some("string"),
            500,
            "lp_ordermatch.rs:1] no context".into(),
        ),
    ];
    for (error, token, data, status, text) in cases {
        assert_wire(&error, token, data, status, &text);
    }
}

#[test]
fn my_orders_order_status_and_cancel_error_wire_forms() {
    let uuid = Uuid::new_v4();
    assert_wire(
        &MyOrdersRpcError::InternalError("lp_ordermatch.rs:1] no context".into()),
        "InternalError",
        Some("string"),
        500,
        "lp_ordermatch.rs:1] no context",
    );

    assert_wire(
        &OrderStatusError::UUIDNotFound { uuid },
        "UUIDNotFound",
        Some("object"),
        400,
        &format!("Order with uuid {uuid} is not found"),
    );
    assert_wire(
        &OrderStatusError::DbError("Error loading an order: disk gone".into()),
        "DbError",
        Some("string"),
        500,
        "Error loading an order: disk gone",
    );
    assert_wire(
        &OrderStatusError::InternalError("lp_ordermatch.rs:1] no context".into()),
        "InternalError",
        Some("string"),
        500,
        "lp_ordermatch.rs:1] no context",
    );

    assert_wire(
        &CancelAllOrdersError::InternalError("lp_ordermatch.rs:1] no context".into()),
        "InternalError",
        Some("string"),
        500,
        "lp_ordermatch.rs:1] no context",
    );

    // `cancel_order`: the three variants keep their tokens (and their data shapes).
    assert_wire(
        &CancelOrderError::CannotRetrieveOrderMatchContext,
        "CannotRetrieveOrderMatchContext",
        None,
        500,
        "Cannot retrieve order match context.",
    );
    assert_wire(
        &CancelOrderError::OrderBeingMatched { uuid },
        "OrderBeingMatched",
        Some("object"),
        400,
        &format!("Order {uuid} is being matched now, can't cancel"),
    );
    assert_wire(
        &CancelOrderError::UUIDNotFound { uuid },
        "UUIDNotFound",
        Some("object"),
        400,
        &format!("Order with uuid {uuid} is not found"),
    );
}

#[test]
fn order_status_maps_the_storage_failures() {
    use my_orders_storage::MyOrdersError;
    let uuid = Uuid::new_v4();
    let not_found = OrderStatusError::from(MyOrdersError::NoSuchOrder { uuid });
    assert_eq!(token_of(&not_found), "UUIDNotFound");
    assert_eq!(not_found.to_string(), MyOrdersError::NoSuchOrder { uuid }.to_string());
    for source in [
        MyOrdersError::ErrorLoading("a".into()),
        MyOrdersError::ErrorDeserializing("a".into()),
        MyOrdersError::ErrorSaving("a".into()),
        MyOrdersError::ErrorSerializing("a".into()),
        MyOrdersError::InternalError("a".into()),
    ] {
        let text = source.to_string();
        let error = OrderStatusError::from(source);
        assert_eq!(token_of(&error), "DbError");
        assert_eq!(error.to_string(), text);
    }
}

#[test]
fn cancel_all_orders_request_has_the_cancel_by_member() {
    let req: CancelAllOrdersReq = json::from_value(json!({ "cancel_by": { "type": "All" } })).unwrap();
    assert!(matches!(req.cancel_by, CancelBy::All));
    let req: CancelAllOrdersReq =
        json::from_value(json!({ "cancel_by": { "type": "Pair", "data": { "base": "DYN", "rel": "KMD" } } })).unwrap();
    assert!(matches!(req.cancel_by, CancelBy::Pair { ref base, ref rel } if base == "DYN" && rel == "KMD"));
    assert!(json::from_value::<CancelAllOrdersReq>(json!({})).is_err());
}

// ----- The conversions of the failures the two order-changing methods share -----

/// For every error enum that has the shared variants: a source error converts to the variant of its class, with the
/// same `Display` text as the source (the legacy text), and the token the versioned siblings use.
macro_rules! shared_conversion_tests {
    ($module:ident, $error:ident) => {
        mod $module {
            use super::*;

            /// Converts `source` and returns the typed error, the text of the source and the token of the typed error.
            macro_rules! convert {
                ($source:expr) => {{
                    let source = $source;
                    let text = source.to_string();
                    let error = $error::from(source);
                    let token = token_of(&error);
                    (error.to_string(), text, token)
                }};
            }

            fn expect(case: (String, String, String), token: &str) {
                let (display, source_text, actual_token) = case;
                assert_eq!(display, source_text);
                assert_eq!(actual_token, token);
            }

            #[test]
            fn balance_errors_keep_their_text_and_are_classified() {
                expect(convert!(BalanceError::Transport("x".into())), "Transport");
                expect(convert!(BalanceError::InvalidResponse("x".into())), "Transport");
                expect(convert!(BalanceError::WalletStorageError("x".into())), "DbError");
                expect(
                    convert!(BalanceError::UnexpectedDerivationMethod(
                        UnexpectedDerivationMethod::HDWalletUnavailable
                    )),
                    "InternalError",
                );
                expect(convert!(BalanceError::Internal("x".into())), "InternalError");
                let rejection = FeeCapBelowBaseFee {
                    max_fee_per_gas: None,
                    base_fee: None,
                    node: None,
                };
                expect(
                    convert!(BalanceError::FeeCapBelowBaseFee(rejection)),
                    "FeeCapBelowBaseFee",
                );
            }

            #[test]
            fn check_balance_errors_keep_their_text_and_are_classified() {
                expect(
                    convert!(CheckBalanceError::NotSufficientBalance {
                        coin: "KMD".into(),
                        available: bd("1"),
                        required: bd("2"),
                        locked_by_swaps: Some(bd("0.5")),
                    }),
                    "NotSufficientBalance",
                );
                expect(
                    convert!(CheckBalanceError::NotSufficientBaseCoinBalance {
                        coin: "KMD".into(),
                        available: bd("1"),
                        required: bd("2"),
                        locked_by_swaps: None,
                    }),
                    "NotSufficientBaseCoinBalance",
                );
                expect(
                    convert!(CheckBalanceError::VolumeTooLow {
                        coin: "KMD".into(),
                        volume: bd("1"),
                        threshold: bd("2"),
                    }),
                    "VolumeTooLow",
                );
                expect(convert!(CheckBalanceError::Transport("x".into())), "Transport");
                expect(
                    convert!(CheckBalanceError::InternalError("x".into())),
                    "InternalError",
                );
                let rejection = FeeCapBelowBaseFee {
                    max_fee_per_gas: None,
                    base_fee: None,
                    node: None,
                };
                expect(
                    convert!(CheckBalanceError::FeeCapBelowBaseFee(rejection)),
                    "FeeCapBelowBaseFee",
                );
            }

            #[test]
            fn spend_safety_errors_keep_their_text_and_are_classified() {
                expect(
                    convert!(SpendSafetyError::VolumeTooLow {
                        coin: "KMD".into(),
                        volume: bd("1"),
                        threshold: bd("2"),
                    }),
                    "VolumeTooLow",
                );
                expect(
                    convert!(SpendSafetyError::EstimateUnavailable {
                        coin: "KMD".into(),
                        error: "no estimate".into(),
                    }),
                    "Transport",
                );
            }

            #[test]
            fn swap_key_mismatch_keeps_its_text() {
                expect(
                    convert!(SwapKeyMismatch {
                        coin: "ETH".into(),
                        reason: "a reason".into(),
                    }),
                    "SwapKeyMismatch",
                );
            }

            #[test]
            fn receiver_fee_errors_keep_their_text_and_are_classified() {
                let rejection = FeeCapBelowBaseFee {
                    max_fee_per_gas: None,
                    base_fee: None,
                    node: None,
                };
                for (source, token) in [
                    (
                        TradePreimageError::NotSufficientBalance {
                            coin: "KMD".into(),
                            available: bd("1"),
                            required: bd("2"),
                        },
                        "NotSufficientBalance",
                    ),
                    (
                        TradePreimageError::AmountIsTooSmall {
                            amount: bd("1"),
                            threshold: bd("2"),
                        },
                        "VolumeTooLow",
                    ),
                    (TradePreimageError::Transport("x".into()), "Transport"),
                    (
                        TradePreimageError::FeeCapBelowBaseFee(rejection),
                        "FeeCapBelowBaseFee",
                    ),
                    (TradePreimageError::InternalError("x".into()), "InternalError"),
                ] {
                    let text = source.to_string();
                    let error = $error::from_receiver_fee_error(source, "KMD");
                    assert_eq!(error.to_string(), text);
                    assert_eq!(token_of(&error), token);
                }
            }
        }
    };
}

shared_conversion_tests!(set_price_conversions, SetPriceError);
shared_conversion_tests!(update_maker_order_conversions, UpdateMakerOrderError);

#[test]
fn set_price_classifies_the_order_builder_refusals() {
    let classify = |error: MakerOrderBuildError| {
        let text = error.to_string();
        let typed = SetPriceError::from_maker_order_build_error(error, "DYN", "KMD");
        assert_eq!(typed.to_string(), text);
        (token_of(&typed), json::to_value(&typed).unwrap())
    };
    assert_eq!(classify(MakerOrderBuildError::BaseEqualRel).0, "BaseEqualRel");
    assert_eq!(
        classify(MakerOrderBuildError::TradingNotSupported { coin: "X".into() }).0,
        "TradingNotSupported"
    );
    // The coin of a volume refusal is the base coin for the base volumes, the rel coin for the rel volume.
    let (token, wire) = classify(MakerOrderBuildError::MaxBaseVolTooLow {
        actual: 1.into(),
        threshold: 2.into(),
    });
    assert_eq!(
        (token.as_str(), wire["error_data"]["coin"].as_str()),
        ("VolumeTooLow", Some("DYN"))
    );
    let (token, wire) = classify(MakerOrderBuildError::MinBaseVolTooLow {
        actual: 1.into(),
        threshold: 2.into(),
    });
    assert_eq!(
        (token.as_str(), wire["error_data"]["coin"].as_str()),
        ("VolumeTooLow", Some("DYN"))
    );
    let (token, wire) = classify(MakerOrderBuildError::RelVolTooLow {
        actual: 1.into(),
        threshold: 2.into(),
    });
    assert_eq!(
        (token.as_str(), wire["error_data"]["coin"].as_str()),
        ("VolumeTooLow", Some("KMD"))
    );
    assert_eq!(
        classify(MakerOrderBuildError::PriceTooLow {
            actual: 1.into(),
            threshold: 2.into()
        })
        .0,
        "PriceTooLow"
    );
    assert_eq!(
        classify(MakerOrderBuildError::MaxBaseVolBelowMinBaseVol {
            min: 2.into(),
            max: 1.into()
        })
        .0,
        "MaxBaseVolBelowMinBaseVol"
    );
    assert_eq!(classify(MakerOrderBuildError::ConfSettingsNotSet).0, "InternalError");
}

#[test]
fn update_maker_order_classifies_the_validator_refusals() {
    let classify = |error: MakerOrderBuildError| {
        let text = error.to_string();
        let typed = UpdateMakerOrderError::from_maker_order_build_error(error, "DYN", "KMD");
        assert_eq!(typed.to_string(), text);
        token_of(&typed)
    };
    assert_eq!(
        classify(MakerOrderBuildError::PriceTooLow {
            actual: 1.into(),
            threshold: 2.into()
        }),
        "PriceTooLow"
    );
    assert_eq!(
        classify(MakerOrderBuildError::MinBaseVolTooLow {
            actual: 1.into(),
            threshold: 2.into()
        }),
        "VolumeTooLow"
    );
    assert_eq!(
        classify(MakerOrderBuildError::MaxBaseVolTooLow {
            actual: 1.into(),
            threshold: 2.into()
        }),
        "VolumeTooLow"
    );
    assert_eq!(
        classify(MakerOrderBuildError::RelVolTooLow {
            actual: 1.into(),
            threshold: 2.into()
        }),
        "VolumeTooLow"
    );
    assert_eq!(
        classify(MakerOrderBuildError::MaxBaseVolBelowMinBaseVol {
            min: 2.into(),
            max: 1.into()
        }),
        "MaxBaseVolBelowMinBaseVol"
    );
    // The validators of an update never return these three.
    for error in [
        MakerOrderBuildError::BaseEqualRel,
        MakerOrderBuildError::TradingNotSupported { coin: "X".into() },
        MakerOrderBuildError::ConfSettingsNotSet,
    ] {
        assert_eq!(classify(error), "InternalError");
    }
}

// ----- A node for the cores and the legacy arms -----

/// The coin mocks of the t60 tests, so that the order paths run without a network: a minimum trading volume of
/// 0.0001, a balance of 100, no minimum transaction amount and zero trade fees.
fn mock_coins() {
    TestCoin::min_trading_vol.mock_safe(|_| MockResult::Return(MmNumber::from("0.0001")));
    TestCoin::min_tx_amount.mock_safe(|_| MockResult::Return(BigDecimal::from(0)));
    TestCoin::my_balance
        .mock_safe(|_| MockResult::Return(Box::new(futures01::future::ok(CoinBalance::new(BigDecimal::from(100))))));
    TestCoin::get_sender_trade_fee.mock_safe(|coin, _value, _stage| {
        let fee = TradeFee {
            coin: coin.ticker().to_owned(),
            amount: MmNumber::from(0),
            paid_from_trading_vol: false,
        };
        MockResult::Return(Box::pin(async move { Ok(fee) }))
    });
    TestCoin::get_receiver_trade_fee.mock_safe(|coin, _stage| {
        let fee = TradeFee {
            coin: coin.ticker().to_owned(),
            amount: MmNumber::from(0),
            paid_from_trading_vol: false,
        };
        MockResult::Return(Box::new(futures01::future::ok(fee)))
    });
}

/// A node with the coins `DYN` and `KMD`, real order files in a temporary directory, an in-memory history database and
/// a channel that receives what it publishes.
pub(crate) struct Node {
    pub(crate) ctx: MmArc,
    dbdir: std::path::PathBuf,
    _p2p_rx: mpsc::Receiver<AdexBehaviourCmd>,
}

impl Drop for Node {
    fn drop(&mut self) { std::fs::remove_dir_all(&self.dbdir).ok(); }
}

impl Node {
    pub(crate) fn new() -> Node {
        mock_coins();
        let dbdir = std::env::temp_dir().join(format!("kdf-order-rpc-cores-{}", Uuid::new_v4()));
        let conf = json!({ "dbdir": dbdir.to_string_lossy(), "rpc_password": "pw" });
        let ctx = MmCtxBuilder::new()
            .with_conf(conf)
            .with_secp256k1_key_pair(key_pair_from_seed("passphrase").unwrap())
            .into_mm_arc();
        let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();
        for ticker in ["DYN", "KMD"] {
            block_on(coins_ctx.add_coin(MmCoinEnum::Test(TestCoin::new(ticker)))).unwrap();
        }
        std::fs::create_dir_all(my_maker_orders_dir(&ctx)).unwrap();
        std::fs::create_dir_all(my_orders_history_dir(&ctx)).unwrap();
        let connection = Connection::open_in_memory().unwrap();
        connection.execute(CREATE_MY_ORDERS_TABLE, []).unwrap();
        let _ = ctx.sqlite_connection.pin(Arc::new(Mutex::new(connection)));
        let (tx, p2p_rx) = mpsc::channel(1000);
        P2PContext::new(tx).store_to_mm_arc(&ctx);
        Node {
            ctx,
            dbdir,
            _p2p_rx: p2p_rx,
        }
    }

    fn ordermatch_ctx(&self) -> Arc<OrdermatchContext> { OrdermatchContext::from_ctx(&self.ctx).unwrap() }

    /// Adds an own maker order DYN/KMD (volume 10, minimum 1, price 0.5) to the container. With `on_disk` it is also
    /// saved, as `setprice` saves a new order. It is old enough for an update to change `updated_at`.
    pub(crate) fn add_order(&self, on_disk: bool, save_in_history: bool) -> MakerOrder {
        let base = MmCoinEnum::Test(TestCoin::new("DYN"));
        let rel = MmCoinEnum::Test(TestCoin::new("KMD"));
        let mut order = MakerOrderBuilder::new(&base, &rel)
            .with_max_base_vol(MmNumber::from("10"))
            .with_min_base_vol(Some(MmNumber::from("1")))
            .with_price(MmNumber::from("0.5"))
            .with_conf_settings(OrderConfirmationsSettings::default())
            .with_save_in_history(save_in_history)
            .build_unchecked();
        order.created_at -= 10_000;
        order.updated_at = Some(order.created_at);
        order.save_in_history = save_in_history;
        if on_disk {
            block_on(save_my_new_maker_order(self.ctx.clone(), &order)).unwrap();
        }
        self.insert(&order);
        order
    }

    /// An order with one match that was neither connected nor answered: it has an ongoing match.
    pub(crate) fn add_order_with_an_ongoing_match(&self) -> MakerOrder {
        let mut order: MakerOrder = json::from_str(ORDER_JSON).unwrap();
        for order_match in order.matches.values_mut() {
            order_match.connect = None;
            order_match.connected = None;
        }
        assert!(order.has_ongoing_matches());
        self.insert(&order);
        order
    }

    fn insert(&self, order: &MakerOrder) {
        let handle = Arc::new(AsyncMutex::new(order.clone()));
        self.ordermatch_ctx().maker_orders_ctx.lock().add_order(order, handle);
    }

    /// The order of the container with this UUID, as it is now.
    pub(crate) fn order(&self, uuid: Uuid) -> MakerOrder {
        let handle = self.ordermatch_ctx().maker_orders_ctx.lock().get_order(&uuid).unwrap();
        let order = block_on(handle.lock()).clone();
        order
    }

    pub(crate) fn in_container(&self, uuid: &Uuid) -> bool {
        self.ordermatch_ctx().maker_orders_ctx.lock().contains_key(uuid)
    }
}

/// A maker order with one match that is connected (cancellable). A test clears `connect` and `connected` to make it
/// an order with an ongoing match.
const ORDER_JSON: &str = r#"{"max_base_vol":"1","max_base_vol_rat":[[1,[1]],[1,[1]]],"min_base_vol":"0","min_base_vol_rat":[[0,[]],[1,[1]]],"price":"1","price_rat":[[1,[1]],[1,[1]]],"created_at":1589265312093,"updated_at":1589265312093,"base":"ETH","rel":"JST","matches":{"2f9afe84-7a89-4194-8947-45fba563118f":{"request":{"base":"ETH","rel":"JST","base_amount":"0.1","base_amount_rat":[[1,[1]],[1,[10]]],"rel_amount":"0.2","rel_amount_rat":[[1,[1]],[1,[5]]],"action":"Buy","uuid":"2f9afe84-7a89-4194-8947-45fba563118f","method":"request","sender_pubkey":"031d4256c4bc9f99ac88bf3dba21773132281f65f9bf23a59928bce08961e2f3","dest_pub_key":"0000000000000000000000000000000000000000000000000000000000000000","match_by":{"type":"Any"}},"reserved":{"base":"ETH","rel":"JST","base_amount":"0.1","base_amount_rat":[[1,[1]],[1,[10]]],"rel_amount":"0.1","rel_amount_rat":[[1,[1]],[1,[10]]],"taker_order_uuid":"2f9afe84-7a89-4194-8947-45fba563118f","maker_order_uuid":"5f6516ea-ccaa-453a-9e37-e1c2c0d527e3","method":"reserved","sender_pubkey":"c6a78589e18b482aea046975e6d0acbdea7bf7dbf04d9d5bd67fda917815e3ed","dest_pub_key":"031d4256c4bc9f99ac88bf3dba21773132281f65f9bf23a59928bce08961e2f3"},"connect":{"taker_order_uuid":"2f9afe84-7a89-4194-8947-45fba563118f","maker_order_uuid":"5f6516ea-ccaa-453a-9e37-e1c2c0d527e3","method":"connect","sender_pubkey":"031d4256c4bc9f99ac88bf3dba21773132281f65f9bf23a59928bce08961e2f3","dest_pub_key":"c6a78589e18b482aea046975e6d0acbdea7bf7dbf04d9d5bd67fda917815e3ed"},"connected":{"taker_order_uuid":"2f9afe84-7a89-4194-8947-45fba563118f","maker_order_uuid":"5f6516ea-ccaa-453a-9e37-e1c2c0d527e3","method":"connected","sender_pubkey":"c6a78589e18b482aea046975e6d0acbdea7bf7dbf04d9d5bd67fda917815e3ed","dest_pub_key":"031d4256c4bc9f99ac88bf3dba21773132281f65f9bf23a59928bce08961e2f3"},"last_updated":1589265314408}},"started_swaps":["2f9afe84-7a89-4194-8947-45fba563118f"],"uuid":"5f6516ea-ccaa-453a-9e37-e1c2c0d527e3"}"#;

// ----- `setprice` -----

/// The dev handler `set_price` answered `json::to_vec(&json!({ "result": MakerOrderForRpc::from(&maker_order) }))`.
fn dev_order_result_body(order: &MakerOrder) -> String {
    json::to_string(&json!({ "result": MakerOrderForRpc::from(order) })).unwrap()
}

fn set_price_body(extra: Json) -> Json {
    let mut body = json!({ "base": "DYN", "rel": "KMD", "price": "1", "volume": "1", "save_in_history": false });
    for (key, value) in extra.as_object().unwrap() {
        body[key] = value.clone();
    }
    body
}

#[test]
fn set_price_legacy_answers_the_order_under_result() {
    let node = Node::new();
    let answer = block_on(set_price(
        node.ctx.clone(),
        set_price_body(json!({ "min_volume": "0.1" })),
    ))
    .unwrap();
    assert_eq!(answer.status(), 200);
    let body = body_text(answer);
    let uuid: Uuid = json::from_str::<Json>(&body).unwrap()["result"]["uuid"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(body, dev_order_result_body(&node.order(uuid)));
}

#[test]
fn set_price_legacy_refusals_keep_their_text() {
    let node = Node::new();
    let refuse = |body: Json| block_on(set_price(node.ctx.clone(), body)).unwrap_err();

    // dev `create_maker_order_impl`: `ERR!("Base coin {} is not found", req.base)` and the rel twin.
    assert_legacy_error(
        &refuse(set_price_body(json!({ "base": "NOPE" }))),
        "Base coin NOPE is not found",
    );
    assert_legacy_error(
        &refuse(set_price_body(json!({ "rel": "NOPE" }))),
        "Rel coin NOPE is not found",
    );

    // dev `set_price`: a body that does not decode keeps the text of `json::from_value` after one `try_s!` prefix.
    let undecodable = refuse(json!({ "base": "DYN" }));
    assert_legacy_error(&undecodable, "missing field `rel`");

    // dev `create_maker_order_impl`: `try_s!(builder.build())`, the text of `MakerOrderBuildError::PriceTooLow`.
    let too_low = MmNumber::from("0.000000001");
    let expected = validate_price(too_low).err().unwrap().to_string();
    assert_legacy_error(&refuse(set_price_body(json!({ "price": "0.000000001" }))), &expected);

    // The same through `create_maker_order`, which keeps its string error.
    let req: SetPriceReq = json::from_value(set_price_body(json!({ "base": "NOPE" }))).unwrap();
    assert_legacy_error(
        &block_on(create_maker_order(&node.ctx, req)).unwrap_err(),
        "Base coin NOPE is not found",
    );
}

#[test]
fn set_price_core_fails_with_the_class_of_the_failure() {
    let node = Node::new();
    let core = |body: Json| {
        let req: SetPriceReq = json::from_value(body).unwrap();
        block_on(set_price_core(node.ctx.clone(), req))
    };

    let error = core(set_price_body(json!({ "base": "NOPE" }))).err().unwrap();
    assert!(
        matches!(error.get_inner(), SetPriceError::NoSuchBaseCoin { coin } if coin == "NOPE"),
        "{error}"
    );
    let error = core(set_price_body(json!({ "rel": "NOPE" }))).err().unwrap();
    assert!(
        matches!(error.get_inner(), SetPriceError::NoSuchRelCoin { coin } if coin == "NOPE"),
        "{error}"
    );

    let error = core(set_price_body(json!({ "price": "0.000000001" }))).err().unwrap();
    assert!(
        matches!(error.get_inner(), SetPriceError::PriceTooLow { .. }),
        "{error}"
    );
    assert_eq!(error.get_inner().status_code().as_u16(), 400);

    // A maximum volume below the minimum trading volume of the base coin: the refusal of the order builder.
    let error = core(set_price_body(json!({ "volume": "0.00001" }))).err().unwrap();
    assert!(
        matches!(error.get_inner(), SetPriceError::MaxBaseVolTooLow { coin, .. } if coin == "DYN"),
        "{error}"
    );

    // A minimum volume above the maximum volume.
    let error = core(set_price_body(json!({ "volume": "1", "min_volume": "2" })))
        .err()
        .unwrap();
    assert!(
        matches!(error.get_inner(), SetPriceError::MaxBaseVolBelowMinBaseVol { .. }),
        "{error}"
    );

    // A refused order is not placed.
    assert!(node.ordermatch_ctx().maker_orders_ctx.lock().clone_orders().is_empty());
}

#[test]
fn set_price_core_refuses_a_wallet_only_coin_with_its_side() {
    use coins::{lp_register_coin, RegisterCoinParams};

    let ctx = MmCtxBuilder::new()
        .with_conf(json!({ "coins": [{ "coin": "WALLET", "wallet_only": true }, { "coin": "TRADE" }] }))
        .into_mm_arc();
    for ticker in ["WALLET", "TRADE"] {
        block_on(lp_register_coin(
            &ctx,
            TestCoin::new(ticker).into(),
            RegisterCoinParams {
                ticker: ticker.to_owned(),
                tx_history: false,
            },
        ))
        .unwrap_or_else(|e| panic!("cannot register {ticker}: {e}"));
    }
    let req = |base: &str, rel: &str| -> SetPriceReq {
        json::from_value(json!({ "base": base, "rel": rel, "price": 1, "volume": 1 })).unwrap()
    };
    let error = block_on(set_price_core(ctx.clone(), req("WALLET", "TRADE")))
        .err()
        .unwrap();
    assert!(
        matches!(error.get_inner(), SetPriceError::BaseCoinIsWalletOnly { coin } if coin == "WALLET"),
        "{error}"
    );
    assert_eq!(token_of(error.get_inner()), "CoinIsWalletOnly");
    let error = block_on(set_price_core(ctx.clone(), req("TRADE", "WALLET")))
        .err()
        .unwrap();
    assert!(
        matches!(error.get_inner(), SetPriceError::RelCoinIsWalletOnly { coin } if coin == "WALLET"),
        "{error}"
    );

    // dev `create_maker_order_impl`: `ERR!("Base coin {} is wallet only", req.base)` and the rel twin.
    let answer = block_on(set_price(
        ctx.clone(),
        json!({ "base": "WALLET", "rel": "TRADE", "price": 1, "volume": 1 }),
    ))
    .unwrap_err();
    assert_legacy_error(&answer, "Base coin WALLET is wallet only");
    let answer = block_on(set_price(
        ctx,
        json!({ "base": "TRADE", "rel": "WALLET", "price": 1, "volume": 1 }),
    ))
    .unwrap_err();
    assert_legacy_error(&answer, "Rel coin WALLET is wallet only");
}

/// CRD ch.45 R45.6C.8: while the clock check refuses, `setprice` fails with `ClockOutOfRange` (HTTP 503, the refusal
/// text as the payload), and the legacy arm answers the text it answered before the typed core (the dev handler:
/// `ERR!("{}", error)`).
#[test]
fn set_price_is_refused_while_the_clock_check_refuses() {
    use crate::mm2::lp_clock_check::{clock_refusal, CheckResult, ClockCheckCtx};

    let node = Node::new();
    ClockCheckCtx::from_ctx(&node.ctx).unwrap().apply_runtime_result(
        &CheckResult::Failed {
            offset: 12.5,
            answers: 3,
        },
        false,
        std::time::Instant::now(),
    );
    let refusal = clock_refusal(&node.ctx).unwrap();

    let req: SetPriceReq = json::from_value(set_price_body(json!({}))).unwrap();
    let error = block_on(set_price_core(node.ctx.clone(), req)).err().unwrap();
    assert!(
        matches!(error.get_inner(), SetPriceError::ClockOutOfRange(text) if *text == refusal),
        "{error}"
    );
    assert_eq!(error.get_inner().status_code().as_u16(), 503);

    let legacy = block_on(set_price(node.ctx.clone(), set_price_body(json!({})))).unwrap_err();
    assert_legacy_error(&legacy, &refusal);
    assert!(node.ordermatch_ctx().maker_orders_ctx.lock().clone_orders().is_empty());
}

// ----- `update_maker_order` -----

#[test]
fn update_maker_order_legacy_answers_the_updated_order_under_result() {
    let node = Node::new();
    let order = node.add_order(true, true);
    let answer = block_on(update_maker_order_rpc(
        node.ctx.clone(),
        json!({ "uuid": order.uuid, "new_price": "0.6" }),
    ))
    .unwrap();
    assert_eq!(answer.status(), 200);
    // dev `update_maker_order_rpc`: `json::to_vec(&json!({ "result": MakerOrderForRpc::from(&order) }))`.
    assert_eq!(body_text(answer), dev_order_result_body(&node.order(order.uuid)));
    assert_eq!(node.order(order.uuid).price, MmNumber::from("0.6"));
}

#[test]
fn update_maker_order_legacy_refusals_keep_their_text() {
    let node = Node::new();
    let refuse = |body: Json| block_on(update_maker_order_rpc(node.ctx.clone(), body)).unwrap_err();

    // dev `update_maker_order_impl`: `ERR!("There is no order with UUID {}", req.uuid)`.
    let unknown = Uuid::new_v4();
    assert_legacy_error(
        &refuse(json!({ "uuid": unknown })),
        &format!("There is no order with UUID {unknown}"),
    );

    // dev `update_maker_order_impl`: `ERR!("Can't update an order that has ongoing matches")`.
    let matching = node.add_order_with_an_ongoing_match();
    assert_legacy_error(
        &refuse(json!({ "uuid": matching.uuid })),
        "Can't update an order that has ongoing matches",
    );

    // dev `update_maker_order_impl`: `try_s!(validate_price(..))`, the text of `MakerOrderBuildError::PriceTooLow`.
    let order = node.add_order(true, true);
    let expected = validate_price(MmNumber::from(0)).err().unwrap().to_string();
    assert_legacy_error(&refuse(json!({ "uuid": order.uuid, "new_price": 0 })), &expected);

    // dev `update_maker_order_impl`: `ERR!("New volume {} should be more than zero", volume)`.
    assert_legacy_error(
        &refuse(json!({ "uuid": order.uuid, "volume_delta": "-10" })),
        "New volume 0 should be more than zero",
    );

    // dev `update_maker_order_rpc`: a body that does not decode keeps the text of `json::from_value`.
    assert_legacy_error(&refuse(json!({ "new_price": "1" })), "missing field `uuid`");

    // A refused update changes nothing.
    assert_eq!(node.order(order.uuid), order);
}

#[test]
fn update_maker_order_core_fails_with_the_class_of_the_failure() {
    let node = Node::new();
    let core = |body: Json| {
        let req: MakerOrderUpdateReq = json::from_value(body).unwrap();
        block_on(update_maker_order_core(node.ctx.clone(), req))
    };

    let unknown = Uuid::new_v4();
    let error = core(json!({ "uuid": unknown })).err().unwrap();
    assert!(
        matches!(error.get_inner(), UpdateMakerOrderError::UUIDNotFound { uuid } if *uuid == unknown),
        "{error}"
    );

    let matching = node.add_order_with_an_ongoing_match();
    let error = core(json!({ "uuid": matching.uuid })).err().unwrap();
    assert!(
        matches!(error.get_inner(), UpdateMakerOrderError::OrderBeingMatched { uuid } if *uuid == matching.uuid),
        "{error}"
    );

    let order = node.add_order(true, true);
    let error = core(json!({ "uuid": order.uuid, "new_price": 0 })).err().unwrap();
    assert!(
        matches!(error.get_inner(), UpdateMakerOrderError::PriceTooLow { .. }),
        "{error}"
    );
    let error = core(json!({ "uuid": order.uuid, "volume_delta": "-10" }))
        .err()
        .unwrap();
    assert!(
        matches!(error.get_inner(), UpdateMakerOrderError::NewVolumeNotPositive { volume } if *volume == bd("0")),
        "{error}"
    );
    // A minimum volume above the maximum volume of the order.
    let error = core(json!({ "uuid": order.uuid, "min_volume": "20" })).err().unwrap();
    assert!(
        matches!(
            error.get_inner(),
            UpdateMakerOrderError::MaxBaseVolBelowMinBaseVol { .. }
        ),
        "{error}"
    );

    // An update that fails changes nothing, and the one that succeeds answers the updated order.
    assert_eq!(node.order(order.uuid), order);
    let updated = core(json!({ "uuid": order.uuid, "new_price": "0.7" })).unwrap();
    assert_eq!(updated.0.price, MmNumber::from("0.7"));
}

// ----- `cancel_order` -----

#[test]
fn cancel_order_legacy_answers_success_not_found_and_being_matched() {
    let node = Node::new();

    // dev `cancel_order_rpc`: `json!({ "result": "success" })`.
    let order = node.add_order(true, true);
    let answer = block_on(cancel_order_rpc(node.ctx.clone(), json!({ "uuid": order.uuid }))).unwrap();
    assert_eq!(answer.status(), 200);
    assert_eq!(body_text(answer), r#"{"result":"success"}"#);
    assert!(!node.in_container(&order.uuid));

    // dev `cancel_order_rpc`: HTTP 404 and `json!({ "error": format!("Order with uuid {} is not found", req.uuid) })`,
    // an answer (not an `Err`), with no `file:line] ` prefix.
    let answer = block_on(cancel_order_rpc(node.ctx.clone(), json!({ "uuid": order.uuid }))).unwrap();
    assert_eq!(answer.status(), 404);
    assert_eq!(
        body_text(answer),
        format!(r#"{{"error":"Order with uuid {} is not found"}}"#, order.uuid)
    );

    // dev `cancel_order_rpc`: `ERR!("Order {} is being matched now, can't cancel", req.uuid)`.
    let matching = node.add_order_with_an_ongoing_match();
    let error = block_on(cancel_order_rpc(node.ctx.clone(), json!({ "uuid": matching.uuid }))).unwrap_err();
    assert_legacy_error(
        &error,
        &format!("Order {} is being matched now, can't cancel", matching.uuid),
    );
    assert!(node.in_container(&matching.uuid));

    // dev `cancel_order_rpc`: a body that does not decode keeps the text of `json::from_value`.
    let error = block_on(cancel_order_rpc(node.ctx.clone(), json!({}))).unwrap_err();
    assert_legacy_error(&error, "missing field `uuid`");
}

#[test]
fn cancel_order_core_keeps_its_variants() {
    let node = Node::new();
    let unknown = Uuid::new_v4();
    let error = block_on(cancel_order(node.ctx.clone(), CancelOrderReq { uuid: unknown }))
        .err()
        .unwrap();
    assert!(
        matches!(error.get_inner(), CancelOrderError::UUIDNotFound { uuid } if *uuid == unknown),
        "{error}"
    );
    let matching = node.add_order_with_an_ongoing_match();
    let error = block_on(cancel_order(node.ctx.clone(), CancelOrderReq { uuid: matching.uuid }))
        .err()
        .unwrap();
    assert!(
        matches!(error.get_inner(), CancelOrderError::OrderBeingMatched { uuid } if *uuid == matching.uuid),
        "{error}"
    );
    let order = node.add_order(true, true);
    let response = block_on(cancel_order(node.ctx.clone(), CancelOrderReq { uuid: order.uuid })).unwrap();
    assert_eq!(response.result, "success");
}

// ----- `cancel_all_orders` -----

#[test]
fn cancel_all_orders_legacy_answers_the_lists_under_result() {
    let node = Node::new();
    let order = node.add_order(true, true);
    let matching = node.add_order_with_an_ongoing_match();

    // dev `cancel_all_orders_rpc`: `json!({ "result": { "cancelled": .., "currently_matching": .. } })`.
    let answer = block_on(cancel_all_orders_rpc(
        node.ctx.clone(),
        json!({ "cancel_by": { "type": "Pair", "data": { "base": "NO", "rel": "PAIR" } } }),
    ))
    .unwrap();
    assert_eq!(answer.status(), 200);
    assert_eq!(
        body_text(answer),
        r#"{"result":{"cancelled":[],"currently_matching":[]}}"#
    );

    let answer = block_on(cancel_all_orders_rpc(
        node.ctx.clone(),
        json!({ "cancel_by": { "type": "All" } }),
    ))
    .unwrap();
    assert_eq!(
        body_text(answer),
        format!(
            r#"{{"result":{{"cancelled":["{}"],"currently_matching":["{}"]}}}}"#,
            order.uuid, matching.uuid
        )
    );
    assert!(!node.in_container(&order.uuid));
    assert!(node.in_container(&matching.uuid));

    // dev `cancel_all_orders_rpc`: `try_s!(json::from_value(req["cancel_by"].clone()))`.
    let expected = json::from_value::<CancelBy>(Json::Null).err().unwrap().to_string();
    let error = block_on(cancel_all_orders_rpc(node.ctx.clone(), json!({}))).unwrap_err();
    assert_legacy_error(&error, &expected);
}

// ----- `my_orders` -----

/// The structs that the dev handler `my_orders` serialized, kept here as they were (the member order is the wire order).
#[derive(Serialize)]
struct DevMyOrdersResponse<'a> {
    result: DevMyOrdersResult<'a>,
}

#[derive(Serialize)]
struct DevMyOrdersResult<'a> {
    maker_orders: HashMap<&'a Uuid, MakerOrderForMyOrdersRpc<'a>>,
    taker_orders: HashMap<&'a Uuid, TakerOrderForRpc<'a>>,
}

#[test]
fn my_orders_legacy_answers_the_orders_under_result() {
    let node = Node::new();

    let answer = block_on(my_orders(node.ctx.clone())).unwrap();
    assert_eq!(answer.status(), 200);
    assert_eq!(body_text(answer), r#"{"result":{"maker_orders":{},"taker_orders":{}}}"#);

    let order = node.add_order(false, false);
    let answer = block_on(my_orders(node.ctx.clone())).unwrap();
    let uuid = order.uuid;
    let expected = json::to_string(&DevMyOrdersResponse {
        result: DevMyOrdersResult {
            maker_orders: HashMap::from([(&uuid, MakerOrderForMyOrdersRpc::from(&order))]),
            taker_orders: HashMap::new(),
        },
    })
    .unwrap();
    assert_eq!(body_text(answer), expected);
}

#[test]
fn my_orders_core_answers_the_active_orders() {
    let node = Node::new();
    let order = node.add_order(false, false);
    let response = block_on(my_orders_core(node.ctx.clone(), MyOrdersRequest {})).unwrap();
    assert_eq!(response.maker_orders.len(), 1);
    assert_eq!(response.maker_orders[&order.uuid], order);
    assert!(response.taker_orders.is_empty());
}

// ----- `order_status` -----

#[test]
fn order_status_legacy_answers_the_bare_object() {
    let node = Node::new();

    // dev `order_status`, an active maker order: `json!({ "type": "Maker", "order": MakerOrderForMyOrdersRpc::from(&order) })`.
    let order = node.add_order(true, true);
    let answer = block_on(order_status(node.ctx.clone(), json!({ "uuid": order.uuid }))).unwrap();
    assert_eq!(answer.status(), 200);
    let expected =
        json::to_string(&json!({ "type": "Maker", "order": MakerOrderForMyOrdersRpc::from(&order) })).unwrap();
    assert_eq!(body_text(answer), expected);

    // dev `order_status`, the history fallback: `json!(OrderForRpcWithCancellationReason { .. })`.
    block_on(cancel_order_rpc(node.ctx.clone(), json!({ "uuid": order.uuid }))).unwrap();
    let stored = match block_on(MyOrdersStorage::new(node.ctx.clone()).load_order_from_history(order.uuid)) {
        Ok(stored) => stored,
        Err(e) => panic!("the cancelled order is in the history: {e}"),
    };
    let answer = block_on(order_status(node.ctx.clone(), json!({ "uuid": order.uuid }))).unwrap();
    let expected = json::to_string(&json!(OrderForRpcWithCancellationReason {
        order: OrderForRpc::from(&stored),
        cancellation_reason: "Cancelled",
    }))
    .unwrap();
    assert_eq!(body_text(answer), expected);

    // dev `order_status`: `try_s!(storage.load_order_from_history(req.uuid).await)`; the text behind the trace of the
    // storage error is `MyOrdersError::NoSuchOrder`.
    let unknown = Uuid::new_v4();
    let error = block_on(order_status(node.ctx.clone(), json!({ "uuid": unknown }))).unwrap_err();
    assert_legacy_error(&error, &format!("Order with uuid {unknown} is not found"));

    // dev `order_status`: a body that does not decode keeps the text of `json::from_value`.
    let error = block_on(order_status(node.ctx.clone(), json!({}))).unwrap_err();
    assert_legacy_error(&error, "missing field `uuid`");
}

#[test]
fn order_status_core_answers_by_where_the_order_is() {
    let node = Node::new();
    let order = node.add_order(true, true);
    let response = block_on(order_status_core(node.ctx.clone(), OrderStatusReq { uuid: order.uuid })).unwrap();
    assert!(matches!(response, OrderStatusResponse::Maker(ref maker) if *maker == order));

    let unknown = Uuid::new_v4();
    let error = block_on(order_status_core(node.ctx.clone(), OrderStatusReq { uuid: unknown }))
        .err()
        .unwrap();
    assert!(
        matches!(error.get_inner(), OrderStatusError::UUIDNotFound { uuid } if *uuid == unknown),
        "{error}"
    );
}
