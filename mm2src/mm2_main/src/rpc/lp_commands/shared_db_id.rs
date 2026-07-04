use common::HttpStatusCode;
use derive_more::Display;
use http::StatusCode;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use ser_error_derive::SerializeErrorType;
use serde::Serialize;
use serde_json::Value as Json;

pub type GetSharedDbIdResult<T> = Result<T, MmError<GetSharedDbIdError>>;

/// Error type kept for a uniform `handle_mmrpc` signature. The handler reads an
/// always-available identifier (the pinned value, or the all-zero default before
/// pinning), so no variant is returned under normal operation.
#[derive(Debug, Serialize, Display, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum GetSharedDbIdError {
    Internal(String),
}

impl HttpStatusCode for GetSharedDbIdError {
    fn status_code(&self) -> StatusCode {
        match self {
            GetSharedDbIdError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[derive(Serialize)]
pub struct GetSharedDbIdResponse {
    shared_db_id: String,
}

/// Returns the process-level shared-database identifier (R18) as a lowercase
/// hexadecimal string of the 20-byte hash. The request payload is ignored.
pub async fn get_shared_db_id(ctx: MmArc, _req: Json) -> GetSharedDbIdResult<GetSharedDbIdResponse> {
    let shared_db_id = hex::encode(ctx.shared_db_id().as_slice());
    Ok(GetSharedDbIdResponse { shared_db_id })
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::block_on;
    use mm2_core::mm_ctx::MmCtxBuilder;
    use primitives::hash::H160;

    #[test]
    fn get_shared_db_id_returns_pinned_lowercase_hex() {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        let expected = H160::from([
            0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0xde, 0xad,
            0xbe, 0xef,
        ]);
        ctx.shared_db_id.pin(expected).unwrap();

        let resp = block_on(get_shared_db_id(ctx.clone(), Json::Null)).unwrap();
        assert_eq!(resp.shared_db_id, "0123456789abcdef0123456789abcdefdeadbeef");

        // Two queries against the same context return the same identifier.
        let resp2 = block_on(get_shared_db_id(ctx, Json::Null)).unwrap();
        assert_eq!(resp.shared_db_id, resp2.shared_db_id);
    }

    #[test]
    fn get_shared_db_id_defaults_to_all_zero_before_pinning() {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        let resp = block_on(get_shared_db_id(ctx, Json::Null)).unwrap();
        assert_eq!(resp.shared_db_id, "0".repeat(40));
    }
}
