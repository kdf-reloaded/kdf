//! Bound Trezor connection-status query (native-only).
//!
//! Reports the current connection state of the Trezor hardware-wallet device
//! known to the running daemon, derived from the hardware-wallet handle held by
//! the central cryptographic context. The query is a non-mutating state probe:
//! it never contends for the device session nor enqueues a user-interaction
//! task.

use common::HttpStatusCode;
use crypto::CryptoCtx;
use derive_more::Display;
use http::StatusCode;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use primitives::hash::H160;
use ser_error_derive::SerializeErrorType;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

/// Request for the `trezor_connection_status` RPC.
#[derive(Debug, Deserialize)]
pub struct TrezorConnectionStatusRequest {
    /// Optional hex-encoded 20-byte hardware-wallet identifier
    /// (RIPEMD-160 of SHA-256 of the device's extended public key). When
    /// present, the connected device's identifier MUST equal it.
    #[serde(default)]
    pub device_pubkey: Option<String>,
}

/// Wire-visible connection-status discriminants.
#[derive(Debug, Serialize)]
pub enum TrezorConnectionStatus {
    /// The device is reachable (currently usable, or already in use by a
    /// concurrent task).
    Connected,
    /// The device is disconnected or in an incorrect state and SHOULD be
    /// re-initialised.
    Unreachable,
}

/// Response for the `trezor_connection_status` RPC.
#[derive(Debug, Serialize)]
pub struct TrezorConnectionStatusResponse {
    pub status: TrezorConnectionStatus,
}

/// Bound error surface for the `trezor_connection_status` RPC.
#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum TrezorConnectionStatusError {
    #[display(fmt = "No hardware wallet is initialized")]
    TrezorNotInitialized,
    #[display(fmt = "Found an unexpected hardware wallet device")]
    FoundUnexpectedDevice,
    #[display(fmt = "Internal error: {}", _0)]
    Internal(String),
}

impl HttpStatusCode for TrezorConnectionStatusError {
    fn status_code(&self) -> StatusCode {
        match self {
            TrezorConnectionStatusError::TrezorNotInitialized => StatusCode::BAD_REQUEST,
            TrezorConnectionStatusError::FoundUnexpectedDevice | TrezorConnectionStatusError::Internal(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            },
        }
    }
}

/// Reports the current connection state of the daemon's Trezor device.
///
/// Resolves the status as a state value, not a long-running task: a disconnected
/// handle reports `Unreachable`; a session already held by a concurrent task is
/// treated as `Connected` without contending for it; otherwise a lightweight,
/// non-mutating connectivity check decides between `Connected` and
/// `Unreachable`.
pub async fn trezor_connection_status_rpc(
    ctx: MmArc,
    req: TrezorConnectionStatusRequest,
) -> Result<TrezorConnectionStatusResponse, MmError<TrezorConnectionStatusError>> {
    let crypto_ctx =
        CryptoCtx::from_ctx(&ctx).map_err(|e| MmError::new(TrezorConnectionStatusError::Internal(e.to_string())))?;
    let hw_ctx = crypto_ctx
        .hw_ctx()
        .ok_or_else(|| MmError::new(TrezorConnectionStatusError::TrezorNotInitialized))?;

    if let Some(expected_hex) = req.device_pubkey {
        let expected =
            H160::from_str(&expected_hex).map_to_mm(|_| TrezorConnectionStatusError::FoundUnexpectedDevice)?;
        if hw_ctx.rmd160() != expected {
            return MmError::err(TrezorConnectionStatusError::FoundUnexpectedDevice);
        }
    }

    let status = if hw_ctx.is_connected().await {
        TrezorConnectionStatus::Connected
    } else {
        TrezorConnectionStatus::Unreachable
    };
    Ok(TrezorConnectionStatusResponse { status })
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::block_on;
    use mm2_core::mm_ctx::{MmArc, MmCtxBuilder};
    use primitives::hash::H264;
    use serde_json::json;
    use std::str::FromStr;

    const TEST_TREZOR_PUBKEY: &str = "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";

    fn ctx_with_disconnected_trezor() -> (MmArc, String) {
        let ctx = MmCtxBuilder::new().into_mm_arc();
        let crypto_ctx =
            CryptoCtx::init_with_iguana_passphrase(ctx.clone(), "trezor connection status test passphrase").unwrap();
        let hw_ctx = crypto_ctx.init_trezor_ctx_for_tests(H264::from_str(TEST_TREZOR_PUBKEY).unwrap(), None);
        (ctx, hex::encode(hw_ctx.rmd160().as_slice()))
    }

    #[test]
    fn status_serializes_to_bound_discriminants() {
        let connected = serde_json::to_value(TrezorConnectionStatusResponse {
            status: TrezorConnectionStatus::Connected,
        })
        .unwrap();
        assert_eq!(connected, json!({ "status": "Connected" }));

        let unreachable = serde_json::to_value(TrezorConnectionStatusResponse {
            status: TrezorConnectionStatus::Unreachable,
        })
        .unwrap();
        assert_eq!(unreachable, json!({ "status": "Unreachable" }));
    }

    #[test]
    fn error_serializes_with_bound_tokens_and_status_codes() {
        let not_initialized = serde_json::to_value(&TrezorConnectionStatusError::TrezorNotInitialized).unwrap();
        assert_eq!(not_initialized, json!({ "error_type": "TrezorNotInitialized" }));
        assert_eq!(
            TrezorConnectionStatusError::TrezorNotInitialized.status_code(),
            StatusCode::BAD_REQUEST
        );

        let unexpected = serde_json::to_value(&TrezorConnectionStatusError::FoundUnexpectedDevice).unwrap();
        assert_eq!(unexpected, json!({ "error_type": "FoundUnexpectedDevice" }));
        assert_eq!(
            TrezorConnectionStatusError::FoundUnexpectedDevice.status_code(),
            StatusCode::INTERNAL_SERVER_ERROR
        );

        let internal = serde_json::to_value(&TrezorConnectionStatusError::Internal("boom".to_string())).unwrap();
        assert_eq!(internal, json!({ "error_type": "Internal", "error_data": "boom" }));
        assert_eq!(
            TrezorConnectionStatusError::Internal(String::new()).status_code(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn no_hardware_wallet_handle_yields_not_initialized() {
        let ctx = MmCtxBuilder::new().into_mm_arc();
        CryptoCtx::init_with_iguana_passphrase(ctx.clone(), "trezor connection status test passphrase").unwrap();

        let err = block_on(trezor_connection_status_rpc(ctx, TrezorConnectionStatusRequest {
            device_pubkey: None,
        }))
        .unwrap_err();
        assert!(matches!(
            err.into_inner(),
            TrezorConnectionStatusError::TrezorNotInitialized
        ));
    }

    #[test]
    fn no_hardware_wallet_context_yields_not_initialized_even_with_device_assertion() {
        let ctx = MmCtxBuilder::new().into_mm_arc();
        CryptoCtx::init_with_iguana_passphrase(ctx.clone(), "trezor connection status test passphrase").unwrap();

        let err = block_on(trezor_connection_status_rpc(ctx, TrezorConnectionStatusRequest {
            device_pubkey: Some("not-a-device-id".to_owned()),
        }))
        .unwrap_err();
        let err = err.into_inner();
        assert!(matches!(err, TrezorConnectionStatusError::TrezorNotInitialized));
        assert_eq!(err.status_code(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn initialized_disconnected_trezor_reports_unreachable() {
        let (ctx, _device_pubkey) = ctx_with_disconnected_trezor();

        let response = block_on(trezor_connection_status_rpc(ctx, TrezorConnectionStatusRequest {
            device_pubkey: None,
        }))
        .unwrap();
        assert!(matches!(response.status, TrezorConnectionStatus::Unreachable));
    }

    #[test]
    fn device_pubkey_is_optional_assertion() {
        let (ctx, device_pubkey) = ctx_with_disconnected_trezor();

        let omitted = block_on(trezor_connection_status_rpc(
            ctx.clone(),
            TrezorConnectionStatusRequest { device_pubkey: None },
        ))
        .unwrap();
        assert!(matches!(omitted.status, TrezorConnectionStatus::Unreachable));

        let matching = block_on(trezor_connection_status_rpc(
            ctx.clone(),
            TrezorConnectionStatusRequest {
                device_pubkey: Some(device_pubkey),
            },
        ))
        .unwrap();
        assert!(matches!(matching.status, TrezorConnectionStatus::Unreachable));

        let err = block_on(trezor_connection_status_rpc(ctx, TrezorConnectionStatusRequest {
            device_pubkey: Some("1111111111111111111111111111111111111111".to_owned()),
        }))
        .unwrap_err();
        let err = err.into_inner();
        assert!(matches!(err, TrezorConnectionStatusError::FoundUnexpectedDevice));
        assert_eq!(err.status_code(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
