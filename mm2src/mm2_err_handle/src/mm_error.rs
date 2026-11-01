//! # `MmError<E>` — traced error wrapper for the MarketMaker engine
//!
//! `MmError<E>` wraps any error type `E: NotMmError` together with an
//! ordered list of `TraceLocation`s recording every `?`-conversion or
//! explicit `mm_err` / `map_to_mm` call along the error's path through
//! the call stack. Combined with [`#[track_caller]`][std::panic::Location]
//! that location list is essentially a poor-man's stack trace that survives
//! across `From` conversions and across the `async`/`await` boundary.
//!
//! ## What you actually use
//!
//! Most callers never name `MmError` directly; they use these aliases and
//! extension traits from [`crate::prelude`]:
//!
//! - [`MmResult<T, E>`] — `Result<T, MmError<E>>`.
//! - [`MmError::err`], [`MmError::new`] — construct an `MmError` and
//!   record the caller's source location.
//! - [`crate::map_to_mm::MapToMmResult::map_to_mm`] — lift a
//!   `Result<T, E1>` into `MmResult<T, E2>`.
//! - [`crate::map_mm_error::MapMmError::mm_err`] — change the inner
//!   error type of an existing `MmResult`.
//! - [`crate::or_mm_error::OrMmError::or_mm_err`] — turn `Option<T>` into
//!   `MmResult<T, E>`.
//! - [`crate::map_to_mm_fut::MapToMmFutureExt::map_to_mm_fut`] — same,
//!   for `futures01::Future`.
//!
//! ## How tracing works
//!
//! Two mechanisms cooperate:
//!
//! 1. The `From<E1> for MmError<E2>` blanket impl is `#[track_caller]`,
//!    so the bare `?` operator records the conversion site automatically.
//! 2. The explicit `mm_err` / `map_to_mm` / `or_mm_err` helpers are
//!    likewise `#[track_caller]` and append a single
//!    [`TraceLocation`] to the chain on every call.
//!
//! Each location records `(filename, line)` where the filename is the
//! basename only (e.g. `lp_swap`, not the full repo path), so traces stay
//! readable in JSON-RPC error envelopes.
//!
//! ## JSON wire shape
//!
//! When an `MmError<E>` is serialized it produces this exact object
//! (the field names are part of the `mmrpc` protocol contract):
//!
//! | Field         | Source                                       |
//! |---------------|----------------------------------------------|
//! | `error`       | `etype.to_string()` — human description.     |
//! | `error_path`  | `MmError::path()` — dedup'd file chain.      |
//! | `error_trace` | `MmError::stack_trace()` — full file:line.   |
//! | `error_type`  | from `E`'s `#[serde(tag)]` discriminator.    |
//! | `error_data`  | from `E`'s `#[serde(content)]` payload.      |
//!
//! For the last two to materialize, the inner error type `E` **must** be
//! adjacently-tagged with `#[serde(tag = "error_type", content =
//! "error_data")]` and implement [`SerializeErrorType`] (typically via
//! `#[derive(SerializeErrorType)]`). The `SerMmErrorType` blanket trait
//! enforces this at the type level so that `MmError<E>: Serialize` only
//! when the contract is met.
//!
//! ## Format examples
//!
//! Error path (one entry per file, deduplicated, dot-separated, leaf-first
//! → root-last in code but emitted root-first):
//!
//! ```text
//! rpc.lp_coins.utxo
//! ```
//!
//! Stack trace (one entry per `?` / `mm_err`, leaf-first emission):
//!
//! ```text
//! rpc:392] lp_coins:1104] lp_coins:245] utxo:778]
//! ```
//!
//! ## Defining a serializable error type
//!
//! ```ignore
//! use derive_more::Display;
//! use serde::Serialize;
//! use ser_error_derive::SerializeErrorType;
//!
//! #[derive(Display, Serialize, SerializeErrorType)]
//! #[serde(tag = "error_type", content = "error_data")]
//! enum RpcError {
//!     TransportError { reason: String },
//!     InternalError,
//! }
//! ```

use std::cell::UnsafeCell;
use std::fmt;
use std::panic::Location;

use derive_more::Display;
use http::StatusCode;
use itertools::Itertools;
use serde::{Serialize, Serializer};

use common::HttpStatusCode;
use ser_error::SerializeErrorType;

// ---------------------------------------------------------------------------
// Aliases & marker traits
// ---------------------------------------------------------------------------

/// Convenience alias for `Result<T, MmError<E>>`.
pub type MmResult<T, E> = Result<T, MmError<E>>;

/// Auto-trait that excludes `MmError<_>` (and friends) from being wrapped
/// inside another `MmError`.
///
/// The blanket `From<E1> for MmError<E2>` impl is otherwise too greedy and
/// would let us accidentally produce `MmError<MmError<E>>`.
pub auto trait NotMmError {}

impl<E> !NotMmError for MmError<E> {}

// Auto traits do not propagate through `?Sized` types; explicitly opt these
// in so trait objects work at error sites.
impl<T: ?Sized> NotMmError for Box<T> {}
impl<T: ?Sized> NotMmError for UnsafeCell<T> {}

// `Box<dyn Error + Send + Sync>` above only covers the case where the
// unsized trait object sits behind an actual `Box`. It doesn't cover one
// reached through `NonNull<dyn ...>` (or `*const dyn ...`, which `NonNull`
// itself wraps) -- auto traits still don't propagate through those either,
// for the same "unsized types opt out of structural auto-trait inference"
// reason as Box/UnsafeCell above.
//
// (Note: you cannot fix this by implementing `NotMmError` directly for the
// trait object type itself, e.g. `impl NotMmError for dyn Error + Send +
// Sync {}` -- rustc rejects that outright, E0321: "traits with a default
// impl... cannot be implemented for trait object ...". `NonNull<T>` is the
// right target -- a concrete, `Sized` generic struct that merely *wraps* a
// possibly-unsized `T`, exactly like `Box<T>` above, and legal for the same
// reason.)
//
// This was never reachable before because every such pointer we
// structurally recursed into happened to be wrapped in a `Box` at some
// level, until std's own `io::Error` stopped being one: its internal
// bit-packed representation (`core::io::error::repr_bitpacked::Repr`) holds
// its custom-error case as a raw `NonNull<dyn Error + Send + Sync>` (and
// that struct itself behind a further `NonNull<Custom>`) for the
// pointer-tagging trick that representation depends on, not a `Box`, on
// rustc 1.98+. rustc <1.98, including this workspace's pinned 1.97.1,
// boxed it at every level `Box<T>` above already covered -- this impl is a
// no-op there, not a conflicting one, for the same reason the Box/UnsafeCell
// impls above never conflict with the auto-derivation for types that
// already qualified structurally: an explicit impl for a concrete type
// always takes priority over the structural auto-trait fallback, whether
// or not the fallback would separately have agreed.
//
// Confirmed via a real CI break: mm2src/mm2_io/src/fs.rs and anything
// built on `io::Error` (e.g. FsJsonError below it) failed to satisfy
// `NotMmError` under rustc 1.98.0 specifically, tracing through exactly
// this pointer type.
impl<T: ?Sized> NotMmError for std::ptr::NonNull<T> {}

/// Combined bound used by [`MmError`]'s [`Serialize`] impl: the inner
/// error must be displayable, serializable in our adjacent-tagged JSON
/// shape, and not itself an `MmError`.
pub trait SerMmErrorType: SerializeErrorType + fmt::Display + NotMmError {}

impl<E> SerMmErrorType for E where E: SerializeErrorType + fmt::Display + NotMmError {}

// ---------------------------------------------------------------------------
// MmError
// ---------------------------------------------------------------------------

/// Traced error wrapper. See the module docs for the full contract.
#[derive(Clone, Debug, Display, Eq, PartialEq)]
#[display(fmt = "{} {}", "trace.formatted()", etype)]
pub struct MmError<E: NotMmError> {
    pub(crate) etype: E,
    pub(crate) trace: Vec<TraceLocation>,
}

/// Blanket `From` impl that drives the `?` operator. The `#[track_caller]`
/// attribute is what makes the trace meaningful — without it every `?`
/// conversion would point inside this `from` instead of at the call site.
impl<E1, E2> From<E1> for MmError<E2>
where
    E1: NotMmError,
    E2: From<E1> + NotMmError,
{
    #[track_caller]
    fn from(e1: E1) -> Self { MmError::new(E2::from(e1)) }
}

impl<E> Serialize for MmError<E>
where
    E: SerMmErrorType,
{
    fn serialize<S>(&self, serializer: S) -> Result<<S as Serializer>::Ok, <S as Serializer>::Error>
    where
        S: Serializer,
    {
        // Adjacent-tagged inner error flattens into `error_type` /
        // `error_data` thanks to the `SerializeErrorType` derive.
        #[derive(Serialize)]
        struct ErrorHelper<'a, E> {
            error: String,
            error_path: String,
            error_trace: String,
            #[serde(flatten)]
            etype: &'a E,
        }

        ErrorHelper {
            error: self.etype.to_string(),
            error_path: self.path(),
            error_trace: self.stack_trace(),
            etype: &self.etype,
        }
        .serialize(serializer)
    }
}

impl<E> HttpStatusCode for MmError<E>
where
    E: HttpStatusCode + NotMmError,
{
    fn status_code(&self) -> StatusCode { self.etype.status_code() }
}

// ---------------------------------------------------------------------------
// Trace plumbing
// ---------------------------------------------------------------------------

/// Owned trace fragment used to splice an `MmError`'s history onto another
/// `MmError` produced elsewhere (see [`MmError::split`] /
/// [`MmError::new_with_trace`]).
pub struct MmErrorTrace {
    trace: Vec<TraceLocation>,
}

impl MmErrorTrace {
    pub fn new(trace: Vec<TraceLocation>) -> MmErrorTrace { MmErrorTrace { trace } }
}

impl<E: NotMmError> MmError<E> {
    /// Construct a fresh `MmError` whose trace contains the caller's
    /// source location only.
    #[track_caller]
    pub fn new(etype: E) -> MmError<E> {
        MmError {
            etype,
            trace: vec![TraceLocation::from(Location::caller())],
        }
    }

    /// Construct an `MmError` and append the caller's source location to
    /// the supplied existing `trace`.
    #[track_caller]
    pub fn new_with_trace(etype: E, mut trace: MmErrorTrace) -> MmError<E> {
        trace.trace.push(TraceLocation::from(Location::caller()));
        MmError {
            etype,
            trace: trace.trace,
        }
    }

    /// Decompose into the inner error and its trace; the inverse of
    /// [`MmError::new_with_trace`].
    pub fn split(self) -> (E, MmErrorTrace) { (self.etype, MmErrorTrace::new(self.trace)) }

    /// Replace the inner error in place via `f`, appending the caller's
    /// source location to the trace. Trace history is preserved.
    #[track_caller]
    pub fn map<MapE, F>(mut self, f: F) -> MmError<MapE>
    where
        MapE: NotMmError,
        F: FnOnce(E) -> MapE,
    {
        self.trace.push(TraceLocation::from(Location::caller()));
        MmError {
            etype: f(self.etype),
            trace: self.trace,
        }
    }

    /// Shorthand for `Err(MmError::new(etype))`.
    #[track_caller]
    pub fn err<T>(etype: E) -> Result<T, MmError<E>> { Err(MmError::new(etype)) }

    /// Shorthand for `Err(MmError::new_with_trace(etype, trace))`.
    #[track_caller]
    pub fn err_with_trace<T>(etype: E, trace: MmErrorTrace) -> Result<T, MmError<E>> {
        Err(MmError::new_with_trace(etype, trace))
    }

    /// Borrow the inner error.
    pub fn get_inner(&self) -> &E { &self.etype }

    /// Move out the inner error, dropping the trace.
    pub fn into_inner(self) -> E { self.etype }

    /// Render the trace as a deduplicated dot-separated file path
    /// (root-first), e.g. `mm2.lp_swap.utxo.rpc_client`.
    pub fn path(&self) -> String {
        self.trace
            .iter()
            .map(|src| src.file)
            .rev()
            .dedup()
            .collect::<Vec<_>>()
            .join(".")
    }

    /// Render the full trace as space-separated `file:line]` tokens
    /// (root-first), e.g. `mm2:379] lp_swap:21] utxo:1105]`.
    pub fn stack_trace(&self) -> String {
        self.trace
            .iter()
            .map(|src| src.formatted())
            .rev()
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Common rendering trait for trace types. Implemented for
/// [`TraceLocation`] and `Vec<T: FormattedTrace>`.
pub trait FormattedTrace {
    fn formatted(&self) -> String;
}

/// One entry of an `MmError`'s trace: the file basename and 1-based line
/// number where the conversion happened. Renders as `file:line]`.
#[derive(Clone, Debug, Display, Eq, PartialEq)]
#[display(fmt = "{}:{}]", file, line)]
pub struct TraceLocation {
    file: &'static str,
    line: u32,
}

impl From<&'static Location<'static>> for TraceLocation {
    fn from(location: &'static Location<'static>) -> Self {
        TraceLocation {
            file: gstuff::filename(location.file()),
            line: location.line(),
        }
    }
}

impl FormattedTrace for TraceLocation {
    fn formatted(&self) -> String { self.to_string() }
}

impl TraceLocation {
    pub fn new(file: &'static str, line: u32) -> TraceLocation { TraceLocation { file, line } }

    pub fn file(&self) -> &'static str { self.file }

    pub fn line(&self) -> u32 { self.line }
}

impl<T: FormattedTrace> FormattedTrace for Vec<T> {
    fn formatted(&self) -> String {
        self.iter()
            .map(|src| src.formatted())
            .rev()
            .collect::<Vec<_>>()
            .join(" ")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::*;
    use futures01::Future;
    use ser_error_derive::SerializeErrorType;
    use serde_json::{self as json, json};

    enum ErrorKind {
        NotSufficientBalance { actual: u64, required: u64 },
    }

    #[derive(Display, Serialize, SerializeErrorType)]
    #[serde(tag = "error_type", content = "error_data")]
    enum ForwardedError {
        #[display(fmt = "Not sufficient balance. Top up your balance by {}", missing)]
        NotSufficientBalance { missing: u64 },
    }

    impl From<ErrorKind> for ForwardedError {
        fn from(kind: ErrorKind) -> Self {
            match kind {
                ErrorKind::NotSufficientBalance { actual, required } => ForwardedError::NotSufficientBalance {
                    missing: required - actual,
                },
            }
        }
    }

    #[test]
    fn test_mm_error() {
        const GENERATED_LINE: u32 = line!() + 2;
        fn generate_error(actual: u64, required: u64) -> Result<(), MmError<ErrorKind>> {
            Err(MmError::new(ErrorKind::NotSufficientBalance { actual, required }))
        }

        const FORWARDED_LINE: u32 = line!() + 2;
        fn forward_error(actual: u64, required: u64) -> Result<(), MmError<ForwardedError>> {
            generate_error(actual, required).mm_err(Into::into)?;
            unreachable!("'generate_error' must return an error")
        }

        let actual = 1000;
        let required = 1500;
        let missing = required - actual;
        let error = forward_error(actual, required).expect_err("'forward_error' must return an error");

        let expected_display = format!(
            "mm_error:{}] mm_error:{}] Not sufficient balance. Top up your balance by {}",
            FORWARDED_LINE, GENERATED_LINE, missing
        );
        assert_eq!(error.to_string(), expected_display);

        let expected_path = "mm_error";
        assert_eq!(error.path(), expected_path);

        let expected_stack_trace = format!("mm_error:{}] mm_error:{}]", FORWARDED_LINE, GENERATED_LINE);
        assert_eq!(error.stack_trace(), expected_stack_trace);

        let actual_json = json::to_value(error).expect("!json::to_value");
        let expected_json = json!({
            "error": format!("Not sufficient balance. Top up your balance by {}", missing),
            "error_path": expected_path,
            "error_trace": expected_stack_trace,
            "error_type": "NotSufficientBalance",
            "error_data": { "missing": missing },
        });
        assert_eq!(actual_json, expected_json);
    }

    #[test]
    fn test_map_error() {
        let res: Result<(), _> = Err("An error".to_string());

        let into_mm_with_line = line!() + 1;
        let mm_res = res.map_to_mm(|e| e.len()).expect_err("Expected MmError<usize>");
        assert_eq!(mm_res.etype, 8);
        assert_eq!(mm_res.trace, vec![TraceLocation::new("mm_error", into_mm_with_line)]);

        let error_line = line!() + 1;
        let mm_res: Result<(), _> = None.or_mm_err(|| "An error".to_owned());
        let mm_err = mm_res.expect_err("Expected MmError<String>");

        assert_eq!(mm_err.etype, "An error");
        assert_eq!(mm_err.trace, vec![TraceLocation::new("mm_error", error_line)]);
    }

    #[test]
    fn test_map_fut() {
        fn generate_error(desc: &str) -> Box<dyn Future<Item = (), Error = String> + Send> {
            Box::new(futures01::future::err(desc.to_owned()))
        }

        let into_mm_line = line!() + 2;
        let mm_err = generate_error("An error")
            .map_to_mm_fut(|error| error.len())
            .wait()
            .expect_err("Expected an error");
        assert_eq!(mm_err.etype, 8);
        assert_eq!(mm_err.trace, vec![TraceLocation::new("mm_error", into_mm_line)]);
    }

    #[derive(Display)]
    #[allow(dead_code)]
    enum ForwardedErrorWithBox {
        #[display(fmt = "Not sufficient balance. Top up your balance by {}", missing)]
        NotSufficientBalance {
            missing: u64,
        },
        Box(Box<dyn std::error::Error>),
    }

    impl From<ErrorKind> for ForwardedErrorWithBox {
        fn from(kind: ErrorKind) -> Self {
            match kind {
                ErrorKind::NotSufficientBalance { actual, required } => ForwardedErrorWithBox::NotSufficientBalance {
                    missing: required - actual,
                },
            }
        }
    }

    #[test]
    fn test_mm_error_with_box() {
        const GENERATED_LINE: u32 = line!() + 2;
        fn generate_error_for_box(actual: u64, required: u64) -> Result<(), MmError<ErrorKind>> {
            Err(MmError::new(ErrorKind::NotSufficientBalance { actual, required }))
        }

        const FORWARDED_LINE: u32 = line!() + 2;
        fn forward_error_for_box(actual: u64, required: u64) -> Result<(), MmError<ForwardedErrorWithBox>> {
            generate_error_for_box(actual, required).mm_err(Into::into)?;
            unreachable!("'generate_error' must return an error")
        }

        let actual = 1000;
        let required = 1500;
        let missing = required - actual;
        let error = forward_error_for_box(actual, required).expect_err("'forward_error' must return an error");

        let expected_display = format!(
            "mm_error:{}] mm_error:{}] Not sufficient balance. Top up your balance by {}",
            FORWARDED_LINE, GENERATED_LINE, missing
        );
        assert_eq!(error.to_string(), expected_display);

        let expected_path = "mm_error";
        assert_eq!(error.path(), expected_path);

        let expected_stack_trace = format!("mm_error:{}] mm_error:{}]", FORWARDED_LINE, GENERATED_LINE);
        assert_eq!(error.stack_trace(), expected_stack_trace);
    }
}
