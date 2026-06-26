//! GasFree error taxonomy and HTTP status mapping (§49.9).
//!
//! The observable contract is the **HTTP status mapping** (R-status), not the
//! variant identities or message strings. Provider-supplied text folded into a
//! surfaced error is sanitized (whitespace-collapsed, length-bounded) so
//! untrusted provider text cannot bloat or distort diagnostics (R10).

use common::HttpStatusCode;
use derive_more::Display;
use http::StatusCode;

/// Maximum length of provider-supplied text retained in a surfaced error (R10).
const MAX_PROVIDER_MSG_LEN: usize = 256;

/// Whitespace-collapse and length-bound provider-supplied text before it is
/// folded into a surfaced error (R10).
pub fn sanitize_provider_message(raw: &str) -> String {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.len() > MAX_PROVIDER_MSG_LEN {
        let mut s: String = collapsed.chars().take(MAX_PROVIDER_MSG_LEN).collect();
        s.push('…');
        s
    } else {
        collapsed
    }
}

/// Activation-time GasFree configuration errors (§49.3.1 / §49.3.2). These map
/// to an invalid-payload (`400`) at the activation boundary.
#[derive(Clone, Debug, Display, PartialEq)]
pub enum GasFreeConfigError {
    #[display(fmt = "GasFree is only valid on a Tron chain")]
    NotTron,
    #[display(fmt = "Token gasless config requires a platform GasFree provider")]
    MissingProvider,
    #[display(fmt = "GasFree base_url must be host-only (no path)")]
    PathInBaseUrl,
    #[display(fmt = "Invalid GasFree base_url: {}", _0)]
    InvalidBaseUrl(String),
    #[display(fmt = "Invalid GasFree service_provider address: {}", _0)]
    InvalidServiceProvider(String),
    #[display(fmt = "transfer_max_fee must be non-negative")]
    NegativeFeeCap,
}

/// Provider-interaction errors (§49.9). The status mapping is the observable
/// contract.
#[derive(Clone, Debug, Display, PartialEq)]
pub enum GasFreeProviderError {
    #[display(fmt = "Invalid GasFree request: {}", _0)]
    InvalidRequest(String),
    #[display(fmt = "GasFree provider request timed out")]
    Timeout,
    #[display(fmt = "GasFree transport error: {}", _0)]
    Transport(String),
    #[display(fmt = "Invalid GasFree provider response: {}", _0)]
    InvalidResponse(String),
    #[display(fmt = "GasFree upstream error: {}", _0)]
    Upstream(String),
    #[display(fmt = "GasFree provider rejected the request: {}", _0)]
    ProviderBadRequest(String),
    #[display(fmt = "GasFree provider authentication failed")]
    Unauthorized,
    #[display(fmt = "GasFree provider forbade the request")]
    Forbidden,
    #[display(fmt = "GasFree provider rate-limited the request")]
    RateLimited,
    #[display(fmt = "GasFree feature not implemented: {}", _0)]
    NotImplemented(String),
    #[display(fmt = "GasFree provider internal error: {}", _0)]
    Internal(String),
}

impl HttpStatusCode for GasFreeProviderError {
    fn status_code(&self) -> StatusCode {
        match self {
            GasFreeProviderError::InvalidRequest(_) => StatusCode::BAD_REQUEST,
            GasFreeProviderError::Timeout => StatusCode::GATEWAY_TIMEOUT,
            GasFreeProviderError::Transport(_)
            | GasFreeProviderError::InvalidResponse(_)
            | GasFreeProviderError::Upstream(_) => StatusCode::BAD_GATEWAY,
            GasFreeProviderError::ProviderBadRequest(_) => StatusCode::BAD_REQUEST,
            GasFreeProviderError::Unauthorized => StatusCode::UNAUTHORIZED,
            GasFreeProviderError::Forbidden => StatusCode::FORBIDDEN,
            GasFreeProviderError::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            GasFreeProviderError::NotImplemented(_) => StatusCode::NOT_IMPLEMENTED,
            GasFreeProviderError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// Withdraw-level gasless errors (§49.9). The status mapping is the observable
/// contract.
#[derive(Clone, Debug, Display, PartialEq)]
pub enum GasFreeWithdrawError {
    #[display(fmt = "Gasless rail unavailable: {}", _0)]
    Unavailable(String),
    #[display(fmt = "An in-flight gasless transfer is already pending for this account")]
    PendingTransfer,
    #[display(fmt = "Gasless fee {} exceeds the accepted cap {}", fee, cap)]
    FeeCapExceeded { fee: String, cap: String },
    #[display(fmt = "Gasless quote expired before signing")]
    QuoteExpired,
    #[display(fmt = "GasFree provider rejected the transfer: {}", _0)]
    ProviderRejected(String),
    #[display(fmt = "Invalid GasFree provider response: {}", _0)]
    InvalidProviderResponse(String),
    #[display(fmt = "Gasless trace not found")]
    TraceNotFound,
    #[display(fmt = "Gasless rail misconfigured: {}", _0)]
    Config(String),
    #[display(fmt = "Gasless signing failed: {}", _0)]
    Signing(String),
    #[display(fmt = "{}", _0)]
    Provider(GasFreeProviderError),
}

impl HttpStatusCode for GasFreeWithdrawError {
    fn status_code(&self) -> StatusCode {
        match self {
            GasFreeWithdrawError::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            GasFreeWithdrawError::PendingTransfer => StatusCode::CONFLICT,
            GasFreeWithdrawError::FeeCapExceeded { .. } | GasFreeWithdrawError::QuoteExpired => StatusCode::BAD_REQUEST,
            GasFreeWithdrawError::ProviderRejected(_) | GasFreeWithdrawError::InvalidProviderResponse(_) => {
                StatusCode::BAD_GATEWAY
            },
            GasFreeWithdrawError::TraceNotFound => StatusCode::NOT_FOUND,
            // Misconfiguration / signer faults are caller-side input problems.
            GasFreeWithdrawError::Config(_) | GasFreeWithdrawError::Signing(_) => StatusCode::BAD_REQUEST,
            GasFreeWithdrawError::Provider(p) => p.status_code(),
        }
    }
}

impl From<GasFreeProviderError> for GasFreeWithdrawError {
    fn from(e: GasFreeProviderError) -> Self { GasFreeWithdrawError::Provider(e) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_whitespace_and_bounds_length() {
        let msg = "line one\n\n   line   two\t\tend";
        assert_eq!(sanitize_provider_message(msg), "line one line two end");

        let long = "x".repeat(1000);
        let out = sanitize_provider_message(&long);
        assert!(out.chars().count() <= MAX_PROVIDER_MSG_LEN + 1);
    }

    #[test]
    fn withdraw_status_mapping() {
        use GasFreeWithdrawError::*;
        assert_eq!(Unavailable("x".into()).status_code(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(PendingTransfer.status_code(), StatusCode::CONFLICT);
        assert_eq!(
            FeeCapExceeded {
                fee: "2".into(),
                cap: "1".into()
            }
            .status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(QuoteExpired.status_code(), StatusCode::BAD_REQUEST);
        assert_eq!(ProviderRejected("x".into()).status_code(), StatusCode::BAD_GATEWAY);
        assert_eq!(
            InvalidProviderResponse("x".into()).status_code(),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(TraceNotFound.status_code(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn provider_status_mapping() {
        use GasFreeProviderError::*;
        assert_eq!(InvalidRequest("x".into()).status_code(), StatusCode::BAD_REQUEST);
        assert_eq!(Timeout.status_code(), StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(Transport("x".into()).status_code(), StatusCode::BAD_GATEWAY);
        assert_eq!(InvalidResponse("x".into()).status_code(), StatusCode::BAD_GATEWAY);
        assert_eq!(Upstream("x".into()).status_code(), StatusCode::BAD_GATEWAY);
        assert_eq!(ProviderBadRequest("x".into()).status_code(), StatusCode::BAD_REQUEST);
        assert_eq!(Unauthorized.status_code(), StatusCode::UNAUTHORIZED);
        assert_eq!(Forbidden.status_code(), StatusCode::FORBIDDEN);
        assert_eq!(RateLimited.status_code(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(NotImplemented("x".into()).status_code(), StatusCode::NOT_IMPLEMENTED);
        assert_eq!(Internal("x".into()).status_code(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
