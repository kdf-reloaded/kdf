use common::HttpStatusCode;
use crypto::{CryptoCtx, CryptoCtxError, CryptoInitError};
use derive_more::Display;
use http::StatusCode;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use rpc::v1::types::H160 as H160Json;
use serde_json::Value as Json;

use crate::mm2::lp_network;

pub type GetPublicKeyRpcResult<T> = Result<T, MmError<GetPublicKeyError>>;

#[derive(Serialize, Display, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum GetPublicKeyError {
    Internal(String),
}

impl From<CryptoInitError> for GetPublicKeyError {
    fn from(_: CryptoInitError) -> Self { GetPublicKeyError::Internal("public_key not available".to_string()) }
}

impl From<CryptoCtxError> for GetPublicKeyError {
    fn from(_: CryptoCtxError) -> Self { GetPublicKeyError::Internal("public_key not available".to_string()) }
}

#[derive(Serialize)]
pub struct GetPublicKeyResponse {
    public_key: String,
}

impl HttpStatusCode for GetPublicKeyError {
    fn status_code(&self) -> StatusCode {
        match self {
            GetPublicKeyError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

pub async fn get_public_key(ctx: MmArc, _req: Json) -> GetPublicKeyRpcResult<GetPublicKeyResponse> {
    let public_key = CryptoCtx::from_ctx(&ctx)
        .mm_err(Into::into)?
        .mm2_internal_pubkey()
        .to_string();
    Ok(GetPublicKeyResponse { public_key })
}

#[derive(Serialize)]
pub struct GetPublicKeyHashResponse {
    public_key_hash: H160Json,
}

pub async fn get_public_key_hash(ctx: MmArc, _req: Json) -> GetPublicKeyRpcResult<GetPublicKeyHashResponse> {
    let public_key_hash = ctx.rmd160().to_owned().into();
    Ok(GetPublicKeyHashResponse { public_key_hash })
}

#[derive(Deserialize)]
pub struct PeerConnectionHealthcheckRequest {
    pub peer_address: String,
}

pub type PeerConnectionHealthcheckRpcResult<T> = Result<T, MmError<lp_network::PeerHealthcheckError>>;

pub async fn peer_connection_healthcheck(
    ctx: MmArc,
    req: PeerConnectionHealthcheckRequest,
) -> PeerConnectionHealthcheckRpcResult<bool> {
    lp_network::peer_connection_healthcheck(ctx, req.peer_address).await
}
