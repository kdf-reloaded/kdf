//! The legacy-surface adapter of the typed RPC cores (CRD ch.43 §43.1A; plan `mmrpc-v2-completion`, Part 1 §1.2).
//!
//! A typed core answers `Result<Response, MmError<Error>>` and knows nothing of the request surface it is called
//! from. The legacy surface (a request without an `mmrpc` key) answers a method in one of two shapes, and every
//! failure as the text of a `String`:
//!
//! * success: `{"result": X}` ([`legacy_result`]) or a bare object ([`legacy_bare`]), HTTP 200;
//! * failure: `Err(String)`, which the server answers as `{"error": <string>}` with HTTP 500 ([`legacy_error`]).
//!
//! A legacy handler keeps decoding its own inline body (so a malformed body keeps its text), calls the core, and
//! hands the core's result to one of the helpers here. Later phases reuse the same helpers for the other methods
//! they put on typed cores.
//!
//! The legacy error text is `file:line] ` of the call site followed by the `Display` text of the inner error of the
//! core, which is what `ERR!`/`try_s!` produced in the handlers before they moved onto typed cores. The helpers are
//! `#[track_caller]`, so the location is that of the handler that calls them, not of this file. The `file:line]`
//! tokens that an `MmError` trace used to add in front of the text (`try_s!` on an `MmError` printed its whole
//! trace) are not reproduced: the text after them is unchanged.

use http::Response;
use mm2_err_handle::prelude::*;
use serde::Serialize;
use std::fmt::Display;

/// The legacy error text: `file:line] ` of the caller (the form `ERRL!` gives) followed by `text`.
#[track_caller]
pub(crate) fn legacy_error_text(text: impl Display) -> String {
    let location = std::panic::Location::caller();
    format!("{}:{}] {}", gstuff::filename(location.file()), location.line(), text)
}

/// The legacy error text of a core's failure: [`legacy_error_text`] of the `Display` text of its inner error (not of
/// the `MmError`, whose `Display` also prints the trace).
#[track_caller]
pub(crate) fn legacy_error<E: Display + NotMmError>(error: MmError<E>) -> String {
    legacy_error_text(error.get_inner())
}

/// A 200 answer whose body is `{"result": <result>}`.
#[track_caller]
pub(crate) fn legacy_result_body<T: Serialize>(result: &T) -> Result<Response<Vec<u8>>, String> {
    #[derive(Serialize)]
    struct ResultBody<'a, T> {
        result: &'a T,
    }
    legacy_bare_body(&ResultBody { result })
}

/// A 200 answer whose body is `body` itself.
#[track_caller]
pub(crate) fn legacy_bare_body<T: Serialize>(body: &T) -> Result<Response<Vec<u8>>, String> {
    let bytes = serde_json::to_vec(body).map_err(|e| legacy_error_text(e))?;
    Response::builder().body(bytes).map_err(|e| legacy_error_text(e))
}

/// An answer with the HTTP `status` whose body is `{"error": <text>}`; the text carries no `file:line] ` prefix.
#[track_caller]
pub(crate) fn legacy_error_body(status: u16, text: impl Display) -> Result<Response<Vec<u8>>, String> {
    let body =
        serde_json::to_vec(&serde_json::json!({ "error": text.to_string() })).map_err(|e| legacy_error_text(e))?;
    Response::builder()
        .status(status)
        .body(body)
        .map_err(|e| legacy_error_text(e))
}

/// The legacy answer of a core whose success is the content of `{"result": ...}`.
#[track_caller]
pub(crate) fn legacy_result<T, E>(outcome: Result<T, MmError<E>>) -> Result<Response<Vec<u8>>, String>
where
    T: Serialize,
    E: Display + NotMmError,
{
    match outcome {
        Ok(result) => legacy_result_body(&result),
        Err(error) => Err(legacy_error(error)),
    }
}

/// The legacy answer of a core whose success is the whole body (a bare object, like `order_status`).
#[track_caller]
pub(crate) fn legacy_bare<T, E>(outcome: Result<T, MmError<E>>) -> Result<Response<Vec<u8>>, String>
where
    T: Serialize,
    E: Display + NotMmError,
{
    match outcome {
        Ok(body) => legacy_bare_body(&body),
        Err(error) => Err(legacy_error(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Debug)]
    struct Refusal(&'static str);

    impl Display for Refusal {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "refused: {}", self.0) }
    }

    /// The error text is the location of the caller, then the text of the inner error without the `MmError` trace.
    #[test]
    fn legacy_error_is_the_caller_location_then_the_inner_text() {
        let expected_line = line!() + 1;
        let text = legacy_error(MmError::new(Refusal("because")));
        assert_eq!(text, format!("legacy_adapter:{expected_line}] refused: because"));
    }

    #[test]
    fn legacy_result_wraps_the_success_under_result() {
        let answer = legacy_result::<_, Refusal>(Ok(json!({ "cancelled": [], "currently_matching": [] }))).unwrap();
        assert_eq!(answer.status(), 200);
        assert_eq!(
            String::from_utf8(answer.into_body()).unwrap(),
            r#"{"result":{"cancelled":[],"currently_matching":[]}}"#
        );
    }

    #[test]
    fn legacy_bare_keeps_the_success_as_the_whole_body() {
        let answer = legacy_bare::<_, Refusal>(Ok(json!({ "type": "Maker", "order": {} }))).unwrap();
        assert_eq!(answer.status(), 200);
        assert_eq!(
            String::from_utf8(answer.into_body()).unwrap(),
            r#"{"type":"Maker","order":{}}"#
        );
    }

    #[test]
    fn legacy_error_body_carries_the_status_and_the_unprefixed_text() {
        let answer = legacy_error_body(404, "Order with uuid x is not found").unwrap();
        assert_eq!(answer.status(), 404);
        assert_eq!(
            String::from_utf8(answer.into_body()).unwrap(),
            r#"{"error":"Order with uuid x is not found"}"#
        );
    }
}
