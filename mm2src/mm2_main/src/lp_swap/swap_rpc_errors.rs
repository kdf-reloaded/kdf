//! The typed failures of the swap-list RPC cores (CRD ch.43 §43.1R; plan `mmrpc-v2-completion`, group C):
//! `all_swaps_uuids_by_filter` and `import_swaps`.
//!
//! The legacy `all_swaps_uuids_by_filter` failed with a string. Its core now fails with an error enum in the
//! adjacently tagged form of the versioned methods (`error_type`, `error_data`), a status (`HttpStatusCode`) and a
//! `Display` text. The `Display` text of a variant is the text the legacy handler produced for that failure, without
//! its `file:line] ` prefix, so the legacy surface renders the same answer from it (see `crate::mm2::rpc::legacy_adapter`).
//!
//! The tokens, payload kinds and statuses are those of the nearest documented versioned sibling, `my_recent_swaps`
//! (CRD ch.43 R43.1A.7, D43.1C.4 option (ii)): `InvalidTimeStampRange` (no payload, 400) and `DbError` (string, 500). The
//! `FromUuidSwapNotFound` token of the sibling has no counterpart here: `all_swaps_uuids_by_filter` takes no `from_uuid`.

use super::my_swaps_storage::MySwapsError;
use super::*;
use std::fmt;

/// The failures of `all_swaps_uuids_by_filter`.
#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum AllSwapsUuidsByFilterError {
    /// `from_timestamp` is later than `to_timestamp`. Only the browser build's swap storage checks this; the native
    /// storage answers an empty list for such a range.
    #[display(fmt = "Invalid timestamp range")]
    InvalidTimeStampRange,
    /// The swap storage failed. The payload is the whole text of the storage error.
    #[display(fmt = "{}", _0)]
    DbError(String),
}

impl From<MySwapsError> for AllSwapsUuidsByFilterError {
    fn from(error: MySwapsError) -> Self {
        match error {
            MySwapsError::InvalidTimestampRange => AllSwapsUuidsByFilterError::InvalidTimeStampRange,
            // `FromUuidNotFound` needs a `from_uuid`, which this request does not carry: the storage never answers it
            // here, and its text would be the same.
            other => AllSwapsUuidsByFilterError::DbError(other.to_string()),
        }
    }
}

impl HttpStatusCode for AllSwapsUuidsByFilterError {
    fn status_code(&self) -> StatusCode {
        match self {
            AllSwapsUuidsByFilterError::InvalidTimeStampRange => StatusCode::BAD_REQUEST,
            AllSwapsUuidsByFilterError::DbError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// The failures of `import_swaps`: none. A record that cannot be stored is not a failure of the call; the answer lists
/// it under `skipped`, inside the success body (CRD ch.43 R43.1B.3), and the swap index and the call itself have no
/// other failing step. The versioned envelope needs an error type, so this is an enum without variants: no value of it
/// exists, so the call has no error wire form.
#[derive(Debug, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum ImportSwapsError {}

impl fmt::Display for ImportSwapsError {
    fn fmt(&self, _: &mut fmt::Formatter<'_>) -> fmt::Result { match *self {} }
}

impl HttpStatusCode for ImportSwapsError {
    fn status_code(&self) -> StatusCode { match *self {} }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    /// CRD ch.43 R43.8.6: the serialized `error_type` token, the JSON type of `error_data` (`None`: the key is absent),
    /// the HTTP status and the `Display` text of an error.
    fn assert_wire<E>(error: &E, token: &str, data: Option<&str>, status: u16, text: &str)
    where
        E: Serialize + HttpStatusCode + fmt::Display,
    {
        // No assertion message below carries the error, its payload or its text: only the token.
        let wire = json::to_value(error).unwrap();
        assert_eq!(wire["error_type"], token, "{token}: error_type");
        match data {
            Some("string") => assert!(wire["error_data"].is_string(), "{token}: error_data kind"),
            Some(other) => panic!("unknown kind of error_data: {other}"),
            None => assert!(wire.get("error_data").is_none(), "{token}: error_data kind"),
        }
        assert_eq!(error.status_code().as_u16(), status, "{token}: status");
        assert!(error.to_string() == text, "{token}: Display text");
    }

    #[test]
    fn all_swaps_uuids_by_filter_error_wire_forms() {
        assert_wire(
            &AllSwapsUuidsByFilterError::InvalidTimeStampRange,
            "InvalidTimeStampRange",
            None,
            400,
            "Invalid timestamp range",
        );
        assert_wire(
            &AllSwapsUuidsByFilterError::DbError("the storage failed".into()),
            "DbError",
            Some("string"),
            500,
            "the storage failed",
        );
    }

    /// The storage errors land in the variants of the nearest sibling, `my_recent_swaps`, with the text of the storage
    /// error.
    #[test]
    fn all_swaps_uuids_by_filter_error_classifies_the_storage_errors() {
        assert_wire(
            &AllSwapsUuidsByFilterError::from(MySwapsError::InvalidTimestampRange),
            "InvalidTimeStampRange",
            None,
            400,
            "Invalid timestamp range",
        );
        let cases = [
            (
                MySwapsError::ErrorSerializingItem("s".into()),
                "Error serializing swap: s",
            ),
            (
                MySwapsError::ErrorDeserializingItem("d".into()),
                "Error deserializing swap: d",
            ),
            (MySwapsError::ErrorSavingSwap("v".into()), "Error saving swap: v"),
            (MySwapsError::UnknownSqlError("q".into()), "Unknown SQL error: q"),
            (MySwapsError::InternalError("i".into()), "Internal error: i"),
        ];
        for (error, text) in cases {
            assert_wire(
                &AllSwapsUuidsByFilterError::from(error),
                "DbError",
                Some("string"),
                500,
                text,
            );
        }
        let uuid = Uuid::new_v4();
        assert_wire(
            &AllSwapsUuidsByFilterError::from(MySwapsError::FromUuidNotFound(uuid)),
            "DbError",
            Some("string"),
            500,
            &format!("'from_uuid' not found: {uuid}"),
        );
    }
}
