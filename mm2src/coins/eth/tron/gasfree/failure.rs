//! The error type of the GasFree group and its collapse onto the arms of the withdraw RPC (CRD ch.63 §63.9).
//!
//! The group tells more failure kinds apart than the withdraw RPC shows. [`GasFreeError::rpc_arm`] is the
//! collapse of §63.9.3 (R17): every failure is reported through one member of the public gasless family
//! (§63.9.1) or through the shared transport or internal arm. Nothing here renders a credential or a signature.

use super::super::api::TronApiError;
use super::super::balance::TronBalanceError;
use derive_more::Display;

/// The longest provider text a surfaced error carries, in characters (§63.4.3, R10).
pub(crate) const PROVIDER_TEXT_MAX_CHARS: usize = 256;

/// A provider-supplied text made fit for an error: every run of whitespace becomes one space, the ends are
/// trimmed and the result is cut to [`PROVIDER_TEXT_MAX_CHARS`] characters (R10).
pub(crate) fn sanitize_provider_text(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(PROVIDER_TEXT_MAX_CHARS).collect()
}

/// A member of the public gasless error family of the withdraw RPC (§63.9.1). The wire values are those of the
/// table there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GaslessMember {
    Unavailable,
    PendingTransfer,
    MaxFeeExceeded,
    QuoteExpired,
    ProviderRejected,
    InvalidProviderResponse,
    TraceNotFound,
}

impl GaslessMember {
    /// The HTTP status the member is reported with (§63.9.1; binding).
    pub(crate) fn http_status(self) -> u16 {
        match self {
            GaslessMember::Unavailable => 503,
            GaslessMember::PendingTransfer => 409,
            GaslessMember::MaxFeeExceeded | GaslessMember::QuoteExpired => 400,
            GaslessMember::ProviderRejected | GaslessMember::InvalidProviderResponse => 502,
            GaslessMember::TraceNotFound => 404,
        }
    }

    /// The member's wire value (§63.9.1).
    pub(crate) fn wire_name(self) -> &'static str {
        match self {
            GaslessMember::Unavailable => "Unavailable",
            GaslessMember::PendingTransfer => "PendingTransfer",
            GaslessMember::MaxFeeExceeded => "MaxFeeExceeded",
            GaslessMember::QuoteExpired => "QuoteExpired",
            GaslessMember::ProviderRejected => "ProviderRejected",
            GaslessMember::InvalidProviderResponse => "InvalidProviderResponse",
            GaslessMember::TraceNotFound => "TraceNotFound",
        }
    }
}

/// The arm of the withdraw RPC a failure of the gasless rail is reported through (§63.9.1 to §63.9.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RpcArm {
    /// A member of the public gasless family, with the status [`GaslessMember::http_status`] gives.
    Gasless(GaslessMember),
    /// The shared insufficient-balance withdraw error, computed on the custody account (§63.9.2).
    NotSufficientBalance,
    /// The shared transport arm of the withdraw error, with that arm's own status.
    Transport,
    /// The shared internal-error arm of the withdraw error, with that arm's own status.
    InternalError,
}

/// Why a step of the GasFree group failed.
#[derive(Debug, Display)]
pub(crate) enum GasFreeError {
    /// The controller contract of the network derives another custody address than this build does (R29).
    #[display(
        fmt = "GasFree is not available on {}: the GasFree contract no longer derives the expected address",
        network
    )]
    DerivationMismatch { network: &'static str },
    /// A TRON node read failed; the node client's error as it is.
    #[display(fmt = "{}", _0)]
    Node(TronApiError),
    /// The on-chain balance of the custody address could not be read.
    #[display(fmt = "GasFree custody balance: {}", _0)]
    CustodyBalance(TronBalanceError),
    /// The configured provider URL is not usable (R13). The text never repeats the URL.
    #[display(fmt = "GasFree provider URL: {}", _0)]
    InvalidBaseUrl(&'static str),
    /// A credential cannot be sent as a request header. The text never repeats the credential.
    #[display(fmt = "GasFree provider credentials cannot be sent as a request header")]
    InvalidCredentials,
    /// The signing key does not belong to the `user` of the authorization (R4).
    #[display(fmt = "the signing key is not the key of the GasFree authorization's user")]
    SignerNotUser,
    /// The deadline of the authorization is not in the future (R7).
    #[display(
        fmt = "the GasFree authorization deadline {} is not after the current time {}",
        deadline,
        now
    )]
    DeadlineNotInFuture { deadline: u64, now: u64 },
    /// The typed data of the authorization could not be encoded or signed.
    #[display(fmt = "GasFree authorization: {}", _0)]
    Authorization(String),
    /// The provider did not answer within the per-call bound (R20).
    #[display(fmt = "GasFree provider did not answer in time: {}", _0)]
    Timeout(String),
    /// The provider could not be reached.
    #[display(fmt = "GasFree provider transport: {}", _0)]
    Transport(String),
    /// The provider refused the credentials: status (or envelope code) 401 or 403.
    #[display(fmt = "GasFree provider refused the credentials (status {})", status)]
    Unauthorized { status: u16 },
    /// The provider limits the request rate: status (or envelope code) 429.
    #[display(fmt = "GasFree provider limits the request rate (status 429)")]
    RateLimited,
    /// The provider failed: a 5xx status (or envelope code).
    #[display(fmt = "GasFree provider failed (status {}): {}", status, message)]
    ProviderFailed { status: u16, message: String },
    /// The provider refused the request: a 4xx status (or envelope code) other than 401, 403 and 429.
    #[display(fmt = "GasFree provider refused the request (status {}): {}", status, message)]
    Rejected { status: u16, message: String },
    /// The provider does not know the trace id of a trace request: status (or envelope code) 404 (D63.9 item 5).
    #[display(fmt = "GasFree provider does not know the trace id: {}", _0)]
    TraceNotFound(String),
    /// The provider's answer cannot be used.
    #[display(fmt = "GasFree provider answer cannot be used: {}", _0)]
    InvalidResponse(String),
    /// A request could not be formed, or an internal failure.
    #[display(fmt = "GasFree internal error: {}", _0)]
    Internal(String),
}

impl std::error::Error for GasFreeError {}

impl GasFreeError {
    /// The arm of the withdraw RPC this failure is reported through (§63.9.3, R17; R29 for the derivation check).
    pub(crate) fn rpc_arm(&self) -> RpcArm {
        match self {
            GasFreeError::DerivationMismatch { .. } => RpcArm::Gasless(GaslessMember::Unavailable),
            GasFreeError::DeadlineNotInFuture { .. } => RpcArm::Gasless(GaslessMember::QuoteExpired),
            GasFreeError::Rejected { .. } => RpcArm::Gasless(GaslessMember::ProviderRejected),
            GasFreeError::TraceNotFound(_) => RpcArm::Gasless(GaslessMember::TraceNotFound),
            GasFreeError::InvalidResponse(_) => RpcArm::Gasless(GaslessMember::InvalidProviderResponse),
            GasFreeError::Node(_)
            | GasFreeError::CustodyBalance(_)
            | GasFreeError::Timeout(_)
            | GasFreeError::Transport(_)
            | GasFreeError::Unauthorized { .. }
            | GasFreeError::RateLimited
            | GasFreeError::ProviderFailed { .. } => RpcArm::Transport,
            GasFreeError::InvalidBaseUrl(_)
            | GasFreeError::InvalidCredentials
            | GasFreeError::SignerNotUser
            | GasFreeError::Authorization(_)
            | GasFreeError::Internal(_) => RpcArm::InternalError,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_members_carry_the_binding_statuses() {
        let cases = [
            (GaslessMember::Unavailable, "Unavailable", 503),
            (GaslessMember::PendingTransfer, "PendingTransfer", 409),
            (GaslessMember::MaxFeeExceeded, "MaxFeeExceeded", 400),
            (GaslessMember::QuoteExpired, "QuoteExpired", 400),
            (GaslessMember::ProviderRejected, "ProviderRejected", 502),
            (GaslessMember::InvalidProviderResponse, "InvalidProviderResponse", 502),
            (GaslessMember::TraceNotFound, "TraceNotFound", 404),
        ];
        for (member, name, status) in cases {
            assert_eq!(member.wire_name(), name);
            assert_eq!(member.http_status(), status);
        }
    }

    /// ch.63 T4: provider text is collapsed and bounded to 256 characters.
    #[test]
    fn provider_text_is_collapsed_and_bounded() {
        assert_eq!(sanitize_provider_text("  a\t\tb\r\n c  "), "a b c");
        assert_eq!(sanitize_provider_text(""), "");
        let long = "é ".repeat(400);
        let cut = sanitize_provider_text(&long);
        assert_eq!(cut.chars().count(), PROVIDER_TEXT_MAX_CHARS);
        assert!(!cut.contains("  "));
        let exact = "x".repeat(PROVIDER_TEXT_MAX_CHARS);
        assert_eq!(sanitize_provider_text(&exact), exact);
    }

    #[test]
    fn each_failure_collapses_onto_its_arm() {
        let transport = [
            GasFreeError::Node(TronApiError::Timeout("t".to_owned())),
            GasFreeError::Timeout("t".to_owned()),
            GasFreeError::Transport("t".to_owned()),
            GasFreeError::Unauthorized { status: 401 },
            GasFreeError::RateLimited,
            GasFreeError::ProviderFailed {
                status: 503,
                message: String::new(),
            },
        ];
        for error in transport {
            assert_eq!(error.rpc_arm(), RpcArm::Transport);
        }
        let internal = [
            GasFreeError::InvalidBaseUrl("a path"),
            GasFreeError::InvalidCredentials,
            GasFreeError::SignerNotUser,
            GasFreeError::Authorization("a".to_owned()),
            GasFreeError::Internal("i".to_owned()),
        ];
        for error in internal {
            assert_eq!(error.rpc_arm(), RpcArm::InternalError);
        }
        assert_eq!(
            GasFreeError::Rejected {
                status: 400,
                message: String::new()
            }
            .rpc_arm(),
            RpcArm::Gasless(GaslessMember::ProviderRejected)
        );
        assert_eq!(
            GasFreeError::InvalidResponse("r".to_owned()).rpc_arm(),
            RpcArm::Gasless(GaslessMember::InvalidProviderResponse)
        );
        assert_eq!(
            GasFreeError::TraceNotFound("t".to_owned()).rpc_arm(),
            RpcArm::Gasless(GaslessMember::TraceNotFound)
        );
        assert_eq!(
            GasFreeError::DeadlineNotInFuture { deadline: 1, now: 2 }.rpc_arm(),
            RpcArm::Gasless(GaslessMember::QuoteExpired)
        );
        let mismatch = GasFreeError::DerivationMismatch { network: "nile" };
        assert_eq!(mismatch.rpc_arm(), RpcArm::Gasless(GaslessMember::Unavailable));
        assert_eq!(GaslessMember::Unavailable.http_status(), 503);
        assert_eq!(
            mismatch.to_string(),
            "GasFree is not available on nile: the GasFree contract no longer derives the expected address"
        );
    }
}
