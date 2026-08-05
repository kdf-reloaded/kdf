use crate::l2::EnableL2Error;
use crate::platform_coin_with_tokens::{EnablePlatformCoinWithTokensError, InitTokensAsMmCoinsError};
use crate::prelude::{CoinAddressInfo, CoinConfWithProtocolError, DerivationMethod};
use crate::standalone_coin::InitStandaloneCoinError;
use crate::token::EnableTokenError;
#[cfg(not(target_arch = "wasm32"))]
use crate::z_coin_activation::ZcoinInitError;
use coins::utxo::rpc_clients::UtxoRpcError;
use coins::{BalanceError, CoinProtocol, UnexpectedDerivationMethod};
use common::{HttpStatusCode, StatusCode};
use rpc_task::RpcTaskError;
use std::time::Duration;

// ---------------------------------------------------------------------------
// EnableTokenError — Display, From impls, HttpStatusCode
// ---------------------------------------------------------------------------

#[test]
fn test_enable_token_error_display() {
    let e = EnableTokenError::TokenIsAlreadyActivated("USDT".into());
    assert!(e.to_string().contains("USDT"));

    let e = EnableTokenError::TokenConfigIsNotFound("DAI".into());
    assert!(e.to_string().contains("DAI"));

    let e = EnableTokenError::TokenProtocolParseError {
        ticker: "BAD".into(),
        error: "malformed".into(),
    };
    let msg = e.to_string();
    assert!(msg.contains("BAD") && msg.contains("malformed"));

    let e = EnableTokenError::PlatformCoinIsNotActivated("ETH".into());
    assert!(e.to_string().contains("ETH"));

    let e = EnableTokenError::UnsupportedPlatformCoin {
        platform_coin_ticker: "BTC".into(),
        token_ticker: "USDT".into(),
    };
    let msg = e.to_string();
    assert!(msg.contains("BTC") && msg.contains("USDT"));

    let e = EnableTokenError::Transport("connection refused".into());
    assert!(e.to_string().contains("connection refused"));

    let e = EnableTokenError::Internal("oom".into());
    assert!(e.to_string().contains("oom"));
}

#[test]
fn test_enable_token_error_http_status_codes() {
    // 400 variants
    assert_eq!(
        EnableTokenError::TokenIsAlreadyActivated("X".into()).status_code(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        EnableTokenError::PlatformCoinIsNotActivated("X".into()).status_code(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        EnableTokenError::TokenConfigIsNotFound("X".into()).status_code(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        EnableTokenError::UnexpectedTokenProtocol {
            ticker: "X".into(),
            protocol: CoinProtocol::UTXO,
        }
        .status_code(),
        StatusCode::BAD_REQUEST
    );

    // 500 variants
    assert_eq!(
        EnableTokenError::TokenProtocolParseError {
            ticker: "X".into(),
            error: "e".into(),
        }
        .status_code(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
        EnableTokenError::UnsupportedPlatformCoin {
            platform_coin_ticker: "X".into(),
            token_ticker: "Y".into(),
        }
        .status_code(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
        EnableTokenError::Transport("e".into()).status_code(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
        EnableTokenError::Internal("e".into()).status_code(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
}

#[test]
fn test_enable_token_error_from_coin_conf_error() {
    let e: EnableTokenError = CoinConfWithProtocolError::ConfigIsNotFound("RICK".into()).into();
    assert!(matches!(e, EnableTokenError::TokenConfigIsNotFound(ref t) if t == "RICK"));

    let json_err = serde_json::from_str::<serde_json::Value>("bad_json").unwrap_err();
    let e: EnableTokenError = CoinConfWithProtocolError::CoinProtocolParseError {
        ticker: "MORTY".into(),
        err: json_err,
    }
    .into();
    assert!(matches!(e, EnableTokenError::TokenProtocolParseError { ref ticker, .. } if ticker == "MORTY"));

    let e: EnableTokenError = CoinConfWithProtocolError::UnexpectedProtocol {
        ticker: "BTC".into(),
        protocol: CoinProtocol::ETH { chain_id: None },
    }
    .into();
    assert!(matches!(e, EnableTokenError::UnexpectedTokenProtocol { ref ticker, .. } if ticker == "BTC"));
}

#[test]
fn test_enable_token_error_from_balance_error() {
    let e: EnableTokenError = BalanceError::Transport("timeout".into()).into();
    assert!(matches!(e, EnableTokenError::Transport(ref s) if s == "timeout"));

    let e: EnableTokenError = BalanceError::InvalidResponse("bad json".into()).into();
    assert!(matches!(e, EnableTokenError::Transport(ref s) if s == "bad json"));

    let e: EnableTokenError =
        BalanceError::UnexpectedDerivationMethod(UnexpectedDerivationMethod::HDWalletUnavailable).into();
    assert!(matches!(e, EnableTokenError::UnexpectedDerivationMethod(_)));

    let e: EnableTokenError = BalanceError::Internal("boom".into()).into();
    assert!(matches!(e, EnableTokenError::Internal(ref s) if s == "boom"));

    let e: EnableTokenError = BalanceError::WalletStorageError("corrupt".into()).into();
    assert!(matches!(e, EnableTokenError::Internal(ref s) if s == "corrupt"));
}

#[test]
fn test_enable_token_error_from_utxo_rpc_error() {
    let e: EnableTokenError = UtxoRpcError::InvalidResponse("resp bad".into()).into();
    assert!(matches!(e, EnableTokenError::Transport(ref s) if s == "resp bad"));

    let e: EnableTokenError = UtxoRpcError::Internal("internal".into()).into();
    assert!(matches!(e, EnableTokenError::Internal(ref s) if s == "internal"));
}

// ---------------------------------------------------------------------------
// EnableTokenRequest — serde deserialization
// ---------------------------------------------------------------------------

#[test]
fn test_enable_token_request_deser() {
    use crate::token::EnableTokenRequest;
    let json = r#"{"ticker": "USDT", "activation_params": {"mode": "simple"}}"#;
    let req: EnableTokenRequest<serde_json::Value> = serde_json::from_str(json).unwrap();
    // Fields are private, so just confirm it doesn't panic
    let _ = format!("{:?}", req);
}

#[test]
fn test_enable_token_request_missing_ticker() {
    use crate::token::EnableTokenRequest;
    let json = r#"{"activation_params": {}}"#;
    let result = serde_json::from_str::<EnableTokenRequest<serde_json::Value>>(json);
    assert!(result.is_err());
}

#[test]
fn test_enable_token_request_missing_params() {
    use crate::token::EnableTokenRequest;
    let json = r#"{"ticker": "USDT"}"#;
    let result = serde_json::from_str::<EnableTokenRequest<serde_json::Value>>(json);
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// EnableL2Error — Display, From impls, HttpStatusCode
// ---------------------------------------------------------------------------

#[test]
fn test_enable_l2_error_display() {
    let e = EnableL2Error::L2IsAlreadyActivated("LN".into());
    assert!(e.to_string().contains("LN"));

    let e = EnableL2Error::L2ConfigIsNotFound("LN".into());
    assert!(e.to_string().contains("LN"));

    let e = EnableL2Error::L2ProtocolParseError {
        ticker: "ZK".into(),
        error: "bad".into(),
    };
    assert!(e.to_string().contains("ZK") && e.to_string().contains("bad"));

    let e = EnableL2Error::PlatformCoinIsNotActivated("BTC".into());
    assert!(e.to_string().contains("BTC"));

    let e = EnableL2Error::UnsupportedPlatformCoin {
        platform_coin_ticker: "ETH".into(),
        l2_ticker: "LN".into(),
    };
    assert!(e.to_string().contains("ETH") && e.to_string().contains("LN"));

    let e = EnableL2Error::L2ConfigParseError("parse err".into());
    assert!(e.to_string().contains("parse err"));
}

#[test]
fn test_enable_l2_error_http_status_codes() {
    assert_eq!(
        EnableL2Error::L2IsAlreadyActivated("X".into()).status_code(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        EnableL2Error::PlatformCoinIsNotActivated("X".into()).status_code(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        EnableL2Error::L2ConfigIsNotFound("X".into()).status_code(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        EnableL2Error::UnexpectedL2Protocol {
            ticker: "X".into(),
            protocol: CoinProtocol::ETH { chain_id: None },
        }
        .status_code(),
        StatusCode::BAD_REQUEST
    );

    assert_eq!(
        EnableL2Error::L2ProtocolParseError {
            ticker: "X".into(),
            error: "e".into(),
        }
        .status_code(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
        EnableL2Error::UnsupportedPlatformCoin {
            platform_coin_ticker: "X".into(),
            l2_ticker: "Y".into(),
        }
        .status_code(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
        EnableL2Error::L2ConfigParseError("e".into()).status_code(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
        EnableL2Error::Transport("e".into()).status_code(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
        EnableL2Error::Internal("e".into()).status_code(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
}

#[test]
fn test_enable_l2_error_from_coin_conf_error() {
    let e: EnableL2Error = CoinConfWithProtocolError::ConfigIsNotFound("LN".into()).into();
    assert!(matches!(e, EnableL2Error::L2ConfigIsNotFound(ref t) if t == "LN"));

    let json_err = serde_json::from_str::<serde_json::Value>("bad").unwrap_err();
    let e: EnableL2Error = CoinConfWithProtocolError::CoinProtocolParseError {
        ticker: "LN".into(),
        err: json_err,
    }
    .into();
    assert!(matches!(e, EnableL2Error::L2ProtocolParseError { ref ticker, .. } if ticker == "LN"));

    let e: EnableL2Error = CoinConfWithProtocolError::UnexpectedProtocol {
        ticker: "LN".into(),
        protocol: CoinProtocol::UTXO,
    }
    .into();
    assert!(matches!(e, EnableL2Error::UnexpectedL2Protocol { ref ticker, .. } if ticker == "LN"));
}

// ---------------------------------------------------------------------------
// EnableL2Request — serde
// ---------------------------------------------------------------------------

#[test]
fn test_enable_l2_request_deser() {
    use crate::l2::EnableL2Request;
    let json = r#"{"ticker": "LIGHTNING", "activation_params": {"node_name": "test"}}"#;
    let req: EnableL2Request<serde_json::Value> = serde_json::from_str(json).unwrap();
    let _ = format!("{:?}", req);
}

#[test]
fn test_enable_l2_request_missing_ticker() {
    use crate::l2::EnableL2Request;
    let json = r#"{"activation_params": {}}"#;
    assert!(serde_json::from_str::<EnableL2Request<serde_json::Value>>(json).is_err());
}

// ---------------------------------------------------------------------------
// EnablePlatformCoinWithTokensError — Display, From impls, HttpStatusCode
// ---------------------------------------------------------------------------

#[test]
fn test_platform_error_display() {
    let e = EnablePlatformCoinWithTokensError::PlatformIsAlreadyActivated("BCH".into());
    assert!(e.to_string().contains("BCH"));

    let e = EnablePlatformCoinWithTokensError::PlatformCoinCreationError {
        ticker: "ETH".into(),
        error: "gas too low".into(),
    };
    assert!(e.to_string().contains("ETH") && e.to_string().contains("gas too low"));

    let e = EnablePlatformCoinWithTokensError::PrivKeyNotAllowed("trezor mode".into());
    assert!(e.to_string().contains("trezor mode"));
}

#[test]
fn test_platform_error_http_status_codes() {
    // 400 variants
    assert_eq!(
        EnablePlatformCoinWithTokensError::PlatformIsAlreadyActivated("X".into()).status_code(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        EnablePlatformCoinWithTokensError::PlatformConfigIsNotFound("X".into()).status_code(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        EnablePlatformCoinWithTokensError::TokenConfigIsNotFound("X".into()).status_code(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        EnablePlatformCoinWithTokensError::UnexpectedPlatformProtocol {
            ticker: "X".into(),
            protocol: CoinProtocol::ETH { chain_id: None },
        }
        .status_code(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        EnablePlatformCoinWithTokensError::UnexpectedTokenProtocol {
            ticker: "X".into(),
            protocol: CoinProtocol::UTXO,
        }
        .status_code(),
        StatusCode::BAD_REQUEST
    );

    // 500 variants
    assert_eq!(
        EnablePlatformCoinWithTokensError::CoinProtocolParseError {
            ticker: "X".into(),
            error: "e".into()
        }
        .status_code(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
        EnablePlatformCoinWithTokensError::TokenProtocolParseError {
            ticker: "X".into(),
            error: "e".into()
        }
        .status_code(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
        EnablePlatformCoinWithTokensError::PlatformCoinCreationError {
            ticker: "X".into(),
            error: "e".into()
        }
        .status_code(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
        EnablePlatformCoinWithTokensError::PrivKeyNotAllowed("e".into()).status_code(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
        EnablePlatformCoinWithTokensError::UnexpectedDerivationMethod("e".into()).status_code(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
        EnablePlatformCoinWithTokensError::Transport("e".into()).status_code(),
        StatusCode::BAD_GATEWAY
    );
    assert_eq!(
        EnablePlatformCoinWithTokensError::Internal("e".into()).status_code(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
}

#[test]
fn test_platform_error_from_coin_conf_error() {
    let e: EnablePlatformCoinWithTokensError = CoinConfWithProtocolError::ConfigIsNotFound("BCH".into()).into();
    assert!(matches!(
        e,
        EnablePlatformCoinWithTokensError::PlatformConfigIsNotFound(ref t) if t == "BCH"
    ));

    let json_err = serde_json::from_str::<serde_json::Value>("{bad").unwrap_err();
    let e: EnablePlatformCoinWithTokensError = CoinConfWithProtocolError::CoinProtocolParseError {
        ticker: "ETH".into(),
        err: json_err,
    }
    .into();
    assert!(matches!(
        e,
        EnablePlatformCoinWithTokensError::CoinProtocolParseError { ref ticker, .. } if ticker == "ETH"
    ));

    let e: EnablePlatformCoinWithTokensError = CoinConfWithProtocolError::UnexpectedProtocol {
        ticker: "BTC".into(),
        protocol: CoinProtocol::ETH { chain_id: None },
    }
    .into();
    assert!(matches!(
        e,
        EnablePlatformCoinWithTokensError::UnexpectedPlatformProtocol { ref ticker, .. } if ticker == "BTC"
    ));
}

#[test]
fn test_platform_error_from_init_tokens_error() {
    let e: EnablePlatformCoinWithTokensError = InitTokensAsMmCoinsError::TokenConfigIsNotFound("USDT".into()).into();
    assert!(matches!(
        e,
        EnablePlatformCoinWithTokensError::TokenConfigIsNotFound(ref t) if t == "USDT"
    ));

    let e: EnablePlatformCoinWithTokensError = InitTokensAsMmCoinsError::TokenProtocolParseError {
        ticker: "DAI".into(),
        error: "bad proto".into(),
    }
    .into();
    assert!(matches!(
        e,
        EnablePlatformCoinWithTokensError::TokenProtocolParseError { ref ticker, .. } if ticker == "DAI"
    ));

    let e: EnablePlatformCoinWithTokensError = InitTokensAsMmCoinsError::UnexpectedTokenProtocol {
        ticker: "USDC".into(),
        protocol: CoinProtocol::UTXO,
    }
    .into();
    assert!(matches!(
        e,
        EnablePlatformCoinWithTokensError::UnexpectedTokenProtocol { ref ticker, .. } if ticker == "USDC"
    ));

    let e: EnablePlatformCoinWithTokensError = InitTokensAsMmCoinsError::InvalidPubkey("bad key".into()).into();
    assert!(matches!(
        e,
        EnablePlatformCoinWithTokensError::Internal(ref s) if s == "bad key"
    ));
}

// ---------------------------------------------------------------------------
// InitTokensAsMmCoinsError — From impls
// ---------------------------------------------------------------------------

#[test]
fn test_init_tokens_error_from_coin_conf_error() {
    let e: InitTokensAsMmCoinsError = CoinConfWithProtocolError::ConfigIsNotFound("SLP".into()).into();
    assert!(matches!(
        e,
        InitTokensAsMmCoinsError::TokenConfigIsNotFound(ref t) if t == "SLP"
    ));
}

// ---------------------------------------------------------------------------
// EnablePlatformCoinWithTokensReq — serde
// ---------------------------------------------------------------------------

#[test]
fn test_platform_request_deser() {
    use crate::platform_coin_with_tokens::EnablePlatformCoinWithTokensReq;
    // The request uses #[serde(flatten)] so "ticker" is a required field
    // and all other fields come from the generic T.
    let json = r#"{"ticker": "ETH", "gas_limit": 21000}"#;
    let req: EnablePlatformCoinWithTokensReq<serde_json::Value> = serde_json::from_str(json).unwrap();
    let _ = format!("{:?}", req);
}

#[test]
fn test_platform_request_missing_ticker() {
    use crate::platform_coin_with_tokens::EnablePlatformCoinWithTokensReq;
    let json = r#"{"gas_limit": 21000}"#;
    assert!(serde_json::from_str::<EnablePlatformCoinWithTokensReq<serde_json::Value>>(json).is_err());
}

// ---------------------------------------------------------------------------
// TokenActivationRequest — serde (flatten)
// ---------------------------------------------------------------------------

#[test]
fn test_token_activation_request_deser() {
    use crate::platform_coin_with_tokens::TokenActivationRequest;
    let json = r#"{"ticker": "USDT", "extra_field": 42}"#;
    let req: TokenActivationRequest<serde_json::Value> = serde_json::from_str(json).unwrap();
    let _ = format!("{:?}", req);
}

// ---------------------------------------------------------------------------
// InitStandaloneCoinError — Display, From impls, HttpStatusCode
// ---------------------------------------------------------------------------

#[test]
fn test_standalone_error_display() {
    let e = InitStandaloneCoinError::CoinIsAlreadyActivated { ticker: "BTC".into() };
    let _ = e.to_string(); // doesn't have custom Display but should not panic

    let e = InitStandaloneCoinError::CoinConfigIsNotFound("LTC".into());
    assert!(e.to_string().contains("LTC"));

    let e = InitStandaloneCoinError::CoinProtocolParseError {
        ticker: "DASH".into(),
        error: "bad json".into(),
    };
    assert!(e.to_string().contains("DASH"));

    let e = InitStandaloneCoinError::CoinCreationError {
        ticker: "KMD".into(),
        error: "rpc fail".into(),
    };
    assert!(e.to_string().contains("KMD"));

    let e = InitStandaloneCoinError::PrivKeyNotAllowed("hw wallet".into());
    assert!(e.to_string().contains("hw wallet"));

    let e = InitStandaloneCoinError::TaskTimedOut {
        duration: Duration::from_secs(30),
    };
    assert!(e.to_string().contains("30"));
}

#[test]
fn test_standalone_error_http_status_codes() {
    // 400 variants
    assert_eq!(
        InitStandaloneCoinError::NoSuchTask(42).status_code(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        InitStandaloneCoinError::CoinIsAlreadyActivated { ticker: "X".into() }.status_code(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        InitStandaloneCoinError::CoinConfigIsNotFound("X".into()).status_code(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        InitStandaloneCoinError::CoinProtocolParseError {
            ticker: "X".into(),
            error: "e".into()
        }
        .status_code(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        InitStandaloneCoinError::UnexpectedCoinProtocol {
            ticker: "X".into(),
            protocol: CoinProtocol::ETH { chain_id: None },
        }
        .status_code(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        InitStandaloneCoinError::CoinCreationError {
            ticker: "X".into(),
            error: "e".into()
        }
        .status_code(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        InitStandaloneCoinError::PrivKeyNotAllowed("e".into()).status_code(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        InitStandaloneCoinError::UnexpectedDerivationMethod("e".into()).status_code(),
        StatusCode::BAD_REQUEST
    );

    // 408
    assert_eq!(
        InitStandaloneCoinError::TaskTimedOut {
            duration: Duration::from_secs(1)
        }
        .status_code(),
        StatusCode::REQUEST_TIMEOUT
    );

    // 500
    assert_eq!(
        InitStandaloneCoinError::Transport("e".into()).status_code(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
        InitStandaloneCoinError::Internal("e".into()).status_code(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
}

#[test]
fn test_standalone_error_from_coin_conf_error() {
    let e: InitStandaloneCoinError = CoinConfWithProtocolError::ConfigIsNotFound("BTC".into()).into();
    assert!(matches!(e, InitStandaloneCoinError::CoinConfigIsNotFound(ref t) if t == "BTC"));
}

#[test]
fn test_standalone_error_from_rpc_task_error() {
    let e: InitStandaloneCoinError = RpcTaskError::NoSuchTask(7).into();
    assert!(matches!(e, InitStandaloneCoinError::NoSuchTask(7)));

    let e: InitStandaloneCoinError = RpcTaskError::Timeout(Duration::from_secs(60)).into();
    assert!(matches!(e, InitStandaloneCoinError::TaskTimedOut { duration } if duration == Duration::from_secs(60)));

    let e: InitStandaloneCoinError = RpcTaskError::Internal("rpc boom".into()).into();
    assert!(matches!(e, InitStandaloneCoinError::Internal(ref s) if s.contains("rpc boom")));
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn test_zcoin_error_to_standalone_error_does_not_panic() {
    let e: InitStandaloneCoinError = ZcoinInitError::CoinCreationError {
        ticker: "ARRR".into(),
        error: "lightwalletd unavailable".into(),
    }
    .into();
    assert!(matches!(
        e,
        InitStandaloneCoinError::CoinCreationError { ref ticker, ref error }
            if ticker == "ARRR" && error.contains("lightwalletd unavailable")
    ));

    let e: InitStandaloneCoinError = ZcoinInitError::HardwareWalletsAreNotSupportedYet.into();
    assert!(matches!(e, InitStandaloneCoinError::PrivKeyNotAllowed(ref reason) if reason.contains("Hardware wallets")));
}

// ---------------------------------------------------------------------------
// InitStandaloneCoinReq — serde
// ---------------------------------------------------------------------------

// NOTE: InitStandaloneCoinReq is not publicly re-exported from standalone_coin,
// so we cannot test its serde from outside the module. The following tests are
// omitted: standalone_coin_req_deser, standalone_coin_req_missing_ticker.

// ---------------------------------------------------------------------------
// DerivationMethod — Serialize
// ---------------------------------------------------------------------------
#[test]
fn test_derivation_method_serialize_iguana() {
    let dm = DerivationMethod::Iguana;
    let json = serde_json::to_value(&dm).unwrap();
    assert_eq!(json["type"], "Iguana");
}

#[test]
fn test_derivation_method_serialize_hd() {
    let dm = DerivationMethod::HDWallet("m/44'/141'/0'".into());
    let json = serde_json::to_value(&dm).unwrap();
    assert_eq!(json["type"], "HDWallet");
    assert_eq!(json["data"], "m/44'/141'/0'");
}

// ---------------------------------------------------------------------------
// CoinAddressInfo — Serialize
// ---------------------------------------------------------------------------

#[test]
fn test_coin_address_info_serialize() {
    let info = CoinAddressInfo {
        derivation_method: DerivationMethod::Iguana,
        pubkey: "03abc123".into(),
        balances: serde_json::json!({"spendable": "1.0", "unspendable": "0.0"}),
    };
    let json = serde_json::to_value(&info).unwrap();
    assert_eq!(json["pubkey"], "03abc123");
    assert_eq!(json["derivation_method"]["type"], "Iguana");
}

// ---------------------------------------------------------------------------
// EnableTokenError — JSON serialization (tagged enum)
// ---------------------------------------------------------------------------

#[test]
fn test_enable_token_error_json_has_error_type_tag() {
    let e = EnableTokenError::TokenIsAlreadyActivated("USDT".into());
    let json = serde_json::to_value(&e).unwrap();
    assert_eq!(json["error_type"], "TokenIsAlreadyActivated");
    assert_eq!(json["error_data"], "USDT");
}

#[test]
fn test_enable_token_error_json_struct_variant() {
    let e = EnableTokenError::TokenProtocolParseError {
        ticker: "BAD".into(),
        error: "malformed".into(),
    };
    let json = serde_json::to_value(&e).unwrap();
    assert_eq!(json["error_type"], "TokenProtocolParseError");
    assert_eq!(json["error_data"]["ticker"], "BAD");
    assert_eq!(json["error_data"]["error"], "malformed");
}

// ---------------------------------------------------------------------------
// EnableL2Error — JSON serialization
// ---------------------------------------------------------------------------

#[test]
fn test_enable_l2_error_json_tag() {
    let e = EnableL2Error::L2IsAlreadyActivated("LN".into());
    let json = serde_json::to_value(&e).unwrap();
    assert_eq!(json["error_type"], "L2IsAlreadyActivated");
}

// ---------------------------------------------------------------------------
// EnablePlatformCoinWithTokensError — JSON serialization
// ---------------------------------------------------------------------------

#[test]
fn test_platform_error_json_tag() {
    let e = EnablePlatformCoinWithTokensError::PlatformIsAlreadyActivated("BCH".into());
    let json = serde_json::to_value(&e).unwrap();
    assert_eq!(json["error_type"], "PlatformIsAlreadyActivated");
    assert_eq!(json["error_data"], "BCH");
}

// ---------------------------------------------------------------------------
// InitStandaloneCoinError — JSON serialization
// ---------------------------------------------------------------------------

#[test]
fn test_standalone_error_json_tag() {
    let e = InitStandaloneCoinError::CoinConfigIsNotFound("BTC".into());
    let json = serde_json::to_value(&e).unwrap();
    assert_eq!(json["error_type"], "CoinConfigIsNotFound");
    assert_eq!(json["error_data"], "BTC");
}

#[test]
fn test_standalone_error_json_timeout() {
    let e = InitStandaloneCoinError::TaskTimedOut {
        duration: Duration::from_secs(30),
    };
    let json = serde_json::to_value(&e).unwrap();
    assert_eq!(json["error_type"], "TaskTimedOut");
    assert!(json["error_data"]["duration"].is_object() || json["error_data"]["duration"].is_number());
}

// ---------------------------------------------------------------------------
// Platform-coin task-activation framework (CRD ch. 48) — native only.
//
// The framework wraps the `?Send`-on-wasm one-shot activation inside a `Send`
// `RpcTask`, so it (and these tests) compile only on native targets.
// ---------------------------------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
mod platform_coin_task_activation {
    use crate::init_platform_coin_with_tokens::{InitPlatformCoinWithTokensInProgressStatus,
                                                InitPlatformCoinWithTokensTaskManagerShared};
    use coins::eth::EthCoin;
    use crypto::hw_rpc_task::HwRpcTaskUserAction;
    use crypto::trezor::TrezorPassphraseResponse;
    use rpc_task::rpc_common::RpcTaskUserActionRequest;
    use rpc_task::{RpcTaskError, RpcTaskManager};

    /// The in-progress surface (R48.3.1) reports the three coarse phases:
    /// activating, requesting balances, and finishing.
    #[test]
    fn in_progress_status_phases_serialize() {
        let activating = serde_json::to_value(InitPlatformCoinWithTokensInProgressStatus::ActivatingCoin).unwrap();
        let balances =
            serde_json::to_value(InitPlatformCoinWithTokensInProgressStatus::RequestingWalletBalance).unwrap();
        let finishing = serde_json::to_value(InitPlatformCoinWithTokensInProgressStatus::Finishing).unwrap();
        assert_eq!(activating, serde_json::json!("ActivatingCoin"));
        assert_eq!(balances, serde_json::json!("RequestingWalletBalance"));
        assert_eq!(finishing, serde_json::json!("Finishing"));
    }

    /// A3 / R48.5.2: the framework discriminants on an unknown `task_id`.
    /// `status` reports no task, while `cancel` and `user_action` surface the
    /// `NoSuchTask` framework discriminant — and crucially neither panics
    /// (R48.6.3: `user_action` is routed and validates its `task_id` even
    /// though the shipped non-interactive policies never enter the awaiting
    /// state).
    #[test]
    fn unknown_task_id_yields_framework_discriminants() {
        let manager: InitPlatformCoinWithTokensTaskManagerShared<EthCoin> = RpcTaskManager::new_shared();
        let unknown_task_id = 4242;

        let mut guard = manager.lock().unwrap();

        // status: unknown task -> no status entry.
        assert!(guard.task_status(unknown_task_id, true).is_none());

        // cancel: unknown task -> NoSuchTask (non-panicking).
        match guard.cancel_task(unknown_task_id) {
            Err(e) => assert!(matches!(e.into_inner(), RpcTaskError::NoSuchTask(id) if id == unknown_task_id)),
            Ok(()) => panic!("cancel of an unknown task_id must fail"),
        }

        // user_action: unknown task -> NoSuchTask (non-panicking, no fabricated confirmation).
        let action = HwRpcTaskUserAction::TrezorPassphrase(TrezorPassphraseResponse {
            passphrase: String::new(),
        });
        match guard.on_user_action(unknown_task_id, action) {
            Err(e) => assert!(matches!(e.into_inner(), RpcTaskError::NoSuchTask(id) if id == unknown_task_id)),
            Ok(()) => panic!("user_action on an unknown task_id must fail"),
        }
    }

    /// Same framework guarantee for the Tendermint task family
    /// (`task::enable_tendermint::*`): instantiating the manager over
    /// `TendermintCoin` confirms the per-coin `InitPlatformCoinWithTokensActivationOps`
    /// registration satisfies the framework bounds, and an unknown `task_id` on
    /// `status` yields the standard `NoSuchTask` framework discriminant without
    /// panicking.
    #[test]
    fn unknown_tendermint_task_id_yields_framework_discriminants() {
        let manager: InitPlatformCoinWithTokensTaskManagerShared<coins::tendermint::TendermintCoin> =
            RpcTaskManager::new_shared();
        let unknown_task_id = 4242;

        let mut guard = manager.lock().unwrap();

        // status: unknown task -> no status entry.
        assert!(guard.task_status(unknown_task_id, true).is_none());

        // cancel: unknown task -> NoSuchTask (non-panicking).
        match guard.cancel_task(unknown_task_id) {
            Err(e) => assert!(matches!(e.into_inner(), RpcTaskError::NoSuchTask(id) if id == unknown_task_id)),
            Ok(()) => panic!("cancel of an unknown task_id must fail"),
        }
    }

    /// R48.1.4: `user_action` keeps wire parity with the published surface —
    /// the request carries `{task_id, user_action}`. The user action is the
    /// hardware-wallet vocabulary (R48.6.2); a Trezor passphrase answer
    /// deserializes from its tagged `action_type` form.
    #[test]
    fn user_action_request_deserializes_hw_payload() {
        let json = r#"{"task_id": 7, "user_action": {"action_type": "TrezorPassphrase", "passphrase": "pp"}}"#;
        let req: RpcTaskUserActionRequest<HwRpcTaskUserAction> = serde_json::from_str(json).unwrap();
        assert_eq!(req.task_id, 7);
        assert!(matches!(req.user_action, HwRpcTaskUserAction::TrezorPassphrase(_)));
    }
}
