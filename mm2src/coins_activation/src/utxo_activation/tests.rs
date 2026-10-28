//! Wire-shape tests of the UTXO-family members (CRD ch. 55 §55.5-§55.8, T55.4, T55.5, T55.10-T55.15).

use super::common_impl::validate_electrum_connection_settings;
use super::init_utxo_standard_activation_error::InitUtxoStandardError;
use super::init_utxo_standard_statuses::UtxoStandardInProgressStatus;
use super::utxo_standard_activation_result::{UtxoStandardActivationResult, UtxoWalletBalance};
use crate::bch_with_tokens_activation::BchProtocolInfo;
use crate::prelude::TryFromCoinProtocol;
use crate::standalone_coin::InitStandaloneCoinError;
use coins::coin_balance::{coin_balance_map_for_ticker, HDAccountBalance, HDAddressBalance, HDWalletBalance};
use coins::hd_wallet::NewAccountCreatingError;
use coins::utxo::bch::BchActivationRequest;
use coins::utxo::rpc_clients::ElectrumProtocol;
use coins::utxo::utxo_builder::UtxoCoinBuildError;
use coins::utxo::{UtxoActivationParams, UtxoRpcMode};
use coins::{BalanceError, CoinBalance, CoinProtocol, PrivKeyActivationPolicy};
use common::mm_number::BigDecimal;
use common::{HttpStatusCode, StatusCode};
use crypto::{Bip44Chain, HwError, HwRpcError, RpcDerivationPath};
use rpc_task::RpcTaskError;
use serde_json::{json, Value as Json};
use std::str::FromStr;
use std::time::Duration;

fn wire_of(error: InitUtxoStandardError) -> (Json, StatusCode) {
    let error = InitStandaloneCoinError::from(error);
    (serde_json::to_value(&error).expect("serializable"), error.status_code())
}

fn utxo_params(extra: Json) -> UtxoActivationParams {
    let mut params = json!({
        "mode": { "rpc": "Electrum", "rpc_data": { "servers": [{ "url": "electrum.example.org:50001" }] } },
    });
    for (key, value) in extra.as_object().expect("an object").iter() {
        params[key] = value.clone();
    }
    serde_json::from_value(params).unwrap_or_else(|e| panic!("bad activation params: {}", e))
}

fn balance(spendable: &str, unspendable: &str) -> CoinBalance {
    CoinBalance {
        spendable: BigDecimal::from_str(spendable).unwrap(),
        unspendable: BigDecimal::from_str(unspendable).unwrap(),
    }
}

// ---------------------------------------------------------------------------
// R55.5.14 failure mapping (T55.13)
// ---------------------------------------------------------------------------

/// CRD ch.38 R38.6.4k-R item 5: the activation probe's failure is `Transport` and keeps the text that
/// names the coin and each server.
#[test]
fn unreachable_electrum_servers_are_transport_with_a_string() {
    let failure =
        "RIN: no Electrum server could be connected (2): a.example:50001: refused; b.example:50002: timed out";
    let (wire, status) = wire_of(InitUtxoStandardError::from_build_err(
        UtxoCoinBuildError::FailedToConnectToElectrums(failure.to_owned()),
        "RIN".to_owned(),
    ));
    assert_eq!(wire, json!({ "error_type": "Transport", "error_data": failure }));
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[test]
fn unusable_electrum_settings_and_internal_build_failures_are_internal() {
    for build_err in [
        UtxoCoinBuildError::NoUsableElectrumServers {
            ticker: "RIN".to_owned(),
        },
        UtxoCoinBuildError::Internal("boom".to_owned()),
    ] {
        let (wire, status) = wire_of(InitUtxoStandardError::from_build_err(build_err, "RIN".to_owned()));
        assert_eq!(wire["error_type"], json!("Internal"));
        assert!(wire["error_data"].is_string());
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }
}

#[test]
fn unreadable_native_configuration_is_a_coin_creation_error_object() {
    for build_err in [
        UtxoCoinBuildError::ErrorReadingNativeModeConf("cannot read the conf".to_owned()),
        UtxoCoinBuildError::RpcPortIsNotSet,
        UtxoCoinBuildError::ErrorDetectingFeeMethod("daemon".to_owned()),
        UtxoCoinBuildError::HwContextNotInitialized,
        UtxoCoinBuildError::CoinDoesntSupportTrezor,
    ] {
        let (wire, status) = wire_of(InitUtxoStandardError::from_build_err(build_err, "RIN".to_owned()));
        assert_eq!(wire["error_type"], json!("CoinCreationError"));
        assert_eq!(wire["error_data"]["ticker"], json!("RIN"));
        assert!(wire["error_data"]["error"].is_string());
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}

#[test]
fn block_height_failure_after_build_is_transport() {
    let (wire, status) = wire_of(InitUtxoStandardError::from_block_height_err(
        "connection reset".to_owned(),
    ));
    assert_eq!(
        wire,
        json!({ "error_type": "Transport", "error_data": "connection reset" })
    );
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[test]
fn balance_failures_map_by_class() {
    let ticker = || "RIN".to_owned();
    for transport in [
        BalanceError::Transport("down".to_owned()),
        BalanceError::InvalidResponse("garbage".to_owned()),
    ] {
        let (wire, status) = wire_of(InitUtxoStandardError::from_balance_err(transport, ticker()));
        assert_eq!(wire["error_type"], json!("Transport"));
        assert!(wire["error_data"].is_string());
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }
    let (wire, _) = wire_of(InitUtxoStandardError::from_balance_err(
        BalanceError::Internal("bug".to_owned()),
        ticker(),
    ));
    assert_eq!(wire["error_type"], json!("Internal"));
    let (wire, status) = wire_of(InitUtxoStandardError::from_balance_err(
        BalanceError::UnexpectedDerivationMethod(coins::UnexpectedDerivationMethod::HDWalletUnavailable),
        ticker(),
    ));
    assert_eq!(wire["error_type"], json!("CoinCreationError"));
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[test]
fn hd_account_creation_device_failures_map_by_class() {
    let account_error = |hw: HwError| {
        InitUtxoStandardError::from_enable_balance_err(
            coins::coin_balance::EnableCoinBalanceError::NewAccountCreatingError(
                NewAccountCreatingError::HardwareWalletError(hw),
            ),
            "RIN".to_owned(),
        )
    };

    for (hw, expected) in [
        (HwError::NoTrezorDeviceAvailable, "NoTrezorDeviceAvailable"),
        (HwError::DeviceDisconnected, "NoTrezorDeviceAvailable"),
        (HwError::CannotChooseDevice { count: 2 }, "FoundMultipleDevices"),
    ] {
        let (wire, status) = wire_of(account_error(hw));
        assert_eq!(wire, json!({ "error_type": "HwError", "error_data": expected }));
        assert_eq!(status, StatusCode::GONE);
    }

    // A failure the device reports itself is a coin creation failure, not a `HwError`.
    let (wire, status) = wire_of(account_error(HwError::DeviceFailure(HwRpcError::InvalidPin)));
    assert_eq!(wire["error_type"], json!("CoinCreationError"));
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // A connection timeout and an unanswered PIN request are task timeouts.
    let (wire, status) = wire_of(account_error(HwError::ConnectionTimedOut {
        timeout: Duration::from_secs(300),
    }));
    assert_eq!(
        wire,
        json!({ "error_type": "TaskTimedOut", "error_data": { "duration": { "secs": 300, "nanos": 0 } } })
    );
    assert_eq!(status, StatusCode::REQUEST_TIMEOUT);
    let (wire, _) = wire_of(InitUtxoStandardError::from_enable_balance_err(
        coins::coin_balance::EnableCoinBalanceError::NewAccountCreatingError(NewAccountCreatingError::RpcTaskError(
            RpcTaskError::Timeout(Duration::from_secs(600)),
        )),
        "RIN".to_owned(),
    ));
    assert_eq!(wire["error_type"], json!("TaskTimedOut"));
}

#[test]
fn registration_conflict_is_coin_is_already_activated() {
    let error: InitUtxoStandardError =
        coins::RegisterCoinError::CoinIsInitializedAlready { coin: "RIN".to_owned() }.into();
    let (wire, status) = wire_of(error);
    assert_eq!(
        wire,
        json!({ "error_type": "CoinIsAlreadyActivated", "error_data": { "ticker": "RIN" } })
    );
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

// ---------------------------------------------------------------------------
// R55.5.12 progress vocabulary (T55.5, T55.12)
// ---------------------------------------------------------------------------

#[test]
fn progress_vocabulary_serializes_exactly_the_bound_values() {
    for (status, expected) in [
        (UtxoStandardInProgressStatus::ActivatingCoin, json!("ActivatingCoin")),
        (
            UtxoStandardInProgressStatus::RequestingWalletBalance,
            json!("RequestingWalletBalance"),
        ),
        (UtxoStandardInProgressStatus::Finishing, json!("Finishing")),
        (
            UtxoStandardInProgressStatus::WaitingForTrezorToConnect,
            json!("WaitingForTrezorToConnect"),
        ),
        (
            UtxoStandardInProgressStatus::FollowHwDeviceInstructions,
            json!("FollowHwDeviceInstructions"),
        ),
    ] {
        assert_eq!(serde_json::to_value(status).unwrap(), expected);
    }
    // Payload-carrying values are single-key objects.
    assert_eq!(
        serde_json::to_value(UtxoStandardInProgressStatus::SyncingBlockHeaders {
            current_scanned_block: 10,
            last_block: 20,
        })
        .unwrap(),
        json!({ "SyncingBlockHeaders": { "current_scanned_block": 10, "last_block": 20 } })
    );
    assert_eq!(
        serde_json::to_value(UtxoStandardInProgressStatus::TemporaryError("retrying".to_owned())).unwrap(),
        json!({ "TemporaryError": "retrying" })
    );
}

// ---------------------------------------------------------------------------
// R55.5.13 success object (T55.11)
// ---------------------------------------------------------------------------

#[test]
fn success_object_has_a_ticker_and_a_ticker_keyed_iguana_balance() {
    let result = UtxoStandardActivationResult {
        ticker: "RIN".to_owned(),
        current_block: 123,
        wallet_balance: UtxoWalletBalance::iguana("RIN", "Raddress".to_owned(), balance("1.5", "0")),
    };
    assert_eq!(
        serde_json::to_value(&result).unwrap(),
        json!({
            "ticker": "RIN",
            "current_block": 123,
            "wallet_balance": {
                "wallet_type": "Iguana",
                "address": "Raddress",
                "balance": { "RIN": { "spendable": "1.5", "unspendable": "0" } },
            },
        })
    );
}

#[test]
fn success_object_hd_form_keys_every_balance_by_the_ticker() {
    let path = crypto::DerivationPath::from_str("m/44'/0'/0'").unwrap();
    let address_path = crypto::DerivationPath::from_str("m/44'/0'/0'/0/0").unwrap();
    let hd = HDWalletBalance {
        accounts: vec![HDAccountBalance {
            account_index: 0,
            derivation_path: RpcDerivationPath(path),
            total_balance: coin_balance_map_for_ticker("RIN", balance("2", "1")),
            addresses: vec![HDAddressBalance {
                address: "Raddress".to_owned(),
                derivation_path: RpcDerivationPath(address_path),
                chain: Bip44Chain::External,
                balance: coin_balance_map_for_ticker("RIN", balance("2", "1")),
            }],
        }],
    };
    let result = UtxoStandardActivationResult {
        ticker: "RIN".to_owned(),
        current_block: 5,
        wallet_balance: UtxoWalletBalance::HD(hd),
    };
    let wire = serde_json::to_value(&result).unwrap();
    assert_eq!(wire["wallet_balance"]["wallet_type"], json!("HD"));
    let account = &wire["wallet_balance"]["accounts"][0];
    assert_eq!(account["total_balance"]["RIN"]["spendable"], json!("2"));
    assert_eq!(account["addresses"][0]["chain"], json!("External"));
    assert_eq!(account["addresses"][0]["balance"]["RIN"]["unspendable"], json!("1"));
}

// ---------------------------------------------------------------------------
// R55.5.10 / R55.5.11 request forms (T55.14)
// ---------------------------------------------------------------------------

#[test]
fn priv_key_policy_is_accepted_as_a_string_and_as_an_object() {
    for (value, expected) in [
        (json!("Trezor"), "Trezor"),
        (json!({ "type": "Trezor" }), "Trezor"),
        (json!("ContextPrivKey"), "ContextPrivKey"),
        (json!({ "type": "ContextPrivKey" }), "ContextPrivKey"),
    ] {
        let params = utxo_params(json!({ "priv_key_policy": value }));
        assert_eq!(format!("{:?}", params.priv_key_policy), expected);
    }
    let default = utxo_params(json!({}));
    assert!(matches!(
        default.priv_key_policy,
        PrivKeyActivationPolicy::ContextPrivKey
    ));
    let rejected: Result<UtxoActivationParams, _> = serde_json::from_value(json!({
        "mode": { "rpc": "Native" },
        "priv_key_policy": { "type": "Nonsense" },
    }));
    assert!(rejected.is_err());
}

#[test]
fn path_to_address_defaults_and_is_parsed() {
    assert!(utxo_params(json!({})).path_to_address.is_none());
    let params = utxo_params(json!({ "path_to_address": { "account_id": 2, "chain": "Internal", "address_id": 5 } }));
    let path = params.path_to_address.expect("the path is kept");
    assert_eq!(
        (path.account_id, path.chain, path.address_id),
        (2, Bip44Chain::Internal, 5)
    );
    let partial = utxo_params(json!({ "path_to_address": { "address_id": 1 } }));
    let path = partial.path_to_address.expect("the path is kept");
    assert_eq!(
        (path.account_id, path.chain, path.address_id),
        (0, Bip44Chain::External, 1)
    );
}

#[test]
fn electrum_servers_accept_a_per_server_timeout_and_a_protocol() {
    let params = utxo_params(json!({
        "mode": { "rpc": "Electrum", "rpc_data": { "servers": [
            { "url": "a:1", "protocol": "SSL", "disable_cert_verification": true, "timeout_sec": 2.5 },
            { "url": "b:2" },
        ], "min_connected": 1, "max_connected": 2 } },
    }));
    let servers = match params.mode {
        UtxoRpcMode::Electrum { servers, .. } => servers,
        UtxoRpcMode::Native => panic!("Electrum mode expected"),
    };
    assert_eq!(servers[0].request_timeout(), Some(Duration::from_millis(2500)));
    assert!(matches!(servers[0].protocol, ElectrumProtocol::SSL));
    assert!(servers[0].disable_cert_verification);
    assert_eq!(servers[1].request_timeout(), None);
    #[cfg(not(target_arch = "wasm32"))]
    assert!(matches!(servers[1].protocol, ElectrumProtocol::TCP));
    #[cfg(target_arch = "wasm32")]
    assert!(matches!(servers[1].protocol, ElectrumProtocol::WSS));
}

#[test]
fn unknown_activation_keys_are_ignored() {
    let params = utxo_params(json!({ "slp_tokens_requests": [], "get_balances": false, "whatever": 1 }));
    assert!(!params.tx_history);
}

#[test]
fn electrum_connection_settings_are_validated() {
    let valid = utxo_params(json!({}));
    assert!(validate_electrum_connection_settings(&valid).is_ok());

    for mode in [
        json!({ "rpc": "Electrum", "rpc_data": { "servers": [] } }),
        json!({ "rpc": "Electrum", "rpc_data": { "servers": [{ "url": "a:1" }], "min_connected": 0 } }),
        json!({ "rpc": "Electrum", "rpc_data": { "servers": [{ "url": "a:1" }], "max_connected": 0 } }),
        json!({ "rpc": "Electrum", "rpc_data": { "servers": [{ "url": "a:1" }], "min_connected": 3, "max_connected": 2 } }),
    ] {
        let params = utxo_params(json!({ "mode": mode }));
        let error = validate_electrum_connection_settings(&params)
            .expect_err("the settings must be refused")
            .into_inner();
        let (wire, status) = wire_of(error);
        assert_eq!(wire["error_type"], json!("Internal"));
        assert!(wire["error_data"].is_string());
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    let native = utxo_params(json!({ "mode": { "rpc": "Native" } }));
    assert!(validate_electrum_connection_settings(&native).is_ok());
}

// ---------------------------------------------------------------------------
// §55.8 task::enable_bch (T55.15)
// ---------------------------------------------------------------------------

fn bch_request(activation_params: Json) -> BchActivationRequest {
    serde_json::from_value(activation_params).unwrap_or_else(|e| panic!("bad BCH request: {}", e))
}

#[test]
fn bch_activation_params_carry_the_utxo_fields_and_the_slp_fields() {
    let request = bch_request(json!({
        "mode": { "rpc": "Electrum", "rpc_data": { "servers": [{ "url": "a:1" }] } },
        "bchd_urls": ["https://bchd.example.org:8335"],
        "tx_history": true,
        "slp_tokens_requests": [],
        "get_balances": false,
    }));
    assert!(request.utxo_params.tx_history);
    assert!(!request.has_unsafe_slp_configuration());
}

#[test]
fn bch_empty_bchd_urls_is_unsafe_unless_allowed() {
    let empty = bch_request(json!({ "mode": { "rpc": "Native" }, "bchd_urls": [] }));
    assert!(empty.has_unsafe_slp_configuration());
    let allowed = bch_request(json!({ "mode": { "rpc": "Native" }, "bchd_urls": [], "allow_slp_unsafe_conf": true }));
    assert!(!allowed.has_unsafe_slp_configuration());
    let missing: Result<BchActivationRequest, _> = serde_json::from_value(json!({ "mode": { "rpc": "Native" } }));
    assert!(missing.is_err(), "`bchd_urls` is required");
}

#[test]
fn bch_member_accepts_only_the_bch_protocol() {
    let bch = BchProtocolInfo::try_from_coin_protocol(CoinProtocol::BCH {
        slp_prefix: "simpleledger".to_owned(),
    });
    assert_eq!(bch.expect("BCH is accepted").slp_prefix(), "simpleledger");
    assert!(BchProtocolInfo::try_from_coin_protocol(CoinProtocol::UTXO).is_err());
}
