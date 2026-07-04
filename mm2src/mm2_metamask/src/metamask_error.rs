//! # Purpose
//!
//! Errors raised by the MetaMask integration: from the EIP-1193
//! transport layer (browser-side `window.ethereum.request` failures
//! and JSON-RPC error objects) all the way to the `MetamaskSession`
//! API (account-selection mismatches, internal failures).
//!
//! # Public exports
//!
//! - [`Eip1193Error`] — low-level transport / RPC error returned by
//!   the [`crate::Eip1193Provider`].
//! - [`MetamaskError`] / [`MetamaskResult`] — high-level error wrapper
//!   for the public MetaMask API surface.
//! - [`MetamaskRpcError`] — fieldless enum exposed to RPC callers so
//!   the GUI can handle MetaMask cases specifically.
//! - [`from_metamask_error`] / [`WithMetamaskRpcError`] /
//!   [`WithInternal`] — generic conversion glue used by RPC handlers.
//!
//! # Invariants
//!
//! - **MetaMask user-rejection code stays at `4001`.** This is part of
//!   the EIP-1193 spec; do not change.
//! - **Variant set is wire-stable.** `MetamaskRpcError` is serialised
//!   into RPC responses and is consumed by GUI clients; renaming or
//!   removing variants is a breaking change.
//! - **No legacy `web3::Error`.** Post LP-17 step 2 the crate compiles
//!   without any `rust-web3` fork; the `From<web3::Error>` impl that
//!   used to live here is replaced by [`From<Eip1193Error> for
//!   MetamaskError`] which classifies the same five categories
//!   (Decoder, InvalidResponse, Transport, Rpc, Io) one-to-one.

use derive_more::Display;
use jsonrpc_core::{Error as RpcError, ErrorCode as RpcErrorCode};
use mm2_err_handle::prelude::*;
use serde_derive::{Deserialize, Serialize};

/// MetaMask uses JSON-RPC error code 4001 for user-rejected requests.
const USER_REJECTED_CODE: RpcErrorCode = RpcErrorCode::ServerError(4001);

pub type MetamaskResult<T> = MmResult<T, MetamaskError>;

/// Low-level error from the EIP-1193 wasm-bindgen transport.
///
/// Mirrors the variant set the previous `web3::Error` matching was
/// written against so the `From<Eip1193Error> for MetamaskError`
/// classification below preserves behaviour exactly.
#[derive(Debug, Display)]
pub enum Eip1193Error {
    /// The browser provider returned a malformed JSON-RPC response that
    /// could not be deserialised into the expected shape.
    #[display(fmt = "EIP-1193 invalid response: {_0}")]
    InvalidResponse(String),
    /// The transport itself failed (provider missing, JS error, channel
    /// shut down, etc.).
    #[display(fmt = "EIP-1193 transport error: {_0}")]
    Transport(String),
    /// The provider returned a JSON-RPC error object (e.g. user
    /// rejection 4001, chain not added 4902, ...).
    #[display(fmt = "EIP-1193 RPC error: {_0:?}")]
    Rpc(RpcError),
    /// Unrecoverable internal error inside the transport plumbing.
    #[display(fmt = "EIP-1193 internal error")]
    Internal,
}

/// Errors originating from MetaMask interactions.
#[derive(Debug, Display)]
pub enum MetamaskError {
    #[display(fmt = "ETH provider not found")]
    EthProviderNotFound,
    #[display(fmt = "Expected exactly one selected ETH account")]
    ExpectedOneEthAccount,
    #[display(fmt = "Active account does not match the original")]
    UnexpectedAccountSelected,
    #[display(fmt = "Error serializing RPC arguments: {_0}")]
    ErrorSerializingArguments(String),
    #[display(fmt = "Error deserializing RPC result: {_0}")]
    ErrorDeserializingMethodResult(String),
    #[display(fmt = "User rejected the request")]
    UserCancelled,
    #[display(fmt = "RPC error: {_0:?}")]
    Rpc(RpcError),
    #[display(fmt = "Transport error: {_0:?}")]
    Transport(String),
    #[display(fmt = "Internal error: {_0}")]
    Internal(String),
}

impl From<Eip1193Error> for MetamaskError {
    fn from(e: Eip1193Error) -> Self {
        match e {
            Eip1193Error::InvalidResponse(msg) => MetamaskError::ErrorDeserializingMethodResult(msg),
            Eip1193Error::Transport(msg) => MetamaskError::Transport(msg),
            Eip1193Error::Rpc(rpc) => {
                if rpc.code == USER_REJECTED_CODE {
                    MetamaskError::UserCancelled
                } else {
                    MetamaskError::Rpc(rpc)
                }
            },
            Eip1193Error::Internal => MetamaskError::Internal("EIP-1193 transport internal error".to_owned()),
        }
    }
}

/// Fieldless enumeration of MetaMask-related errors for RPC responses.
///
/// Only includes error variants that the GUI/CLI must handle specifically.
#[derive(Clone, Debug, Deserialize, Display, Serialize, PartialEq)]
pub enum MetamaskRpcError {
    EthProviderNotFound,
    #[display(fmt = "User rejected the request")]
    UserCancelled,
    #[display(fmt = "Unexpected ETH account selected — re-select or re-initialize MetaMask")]
    UnexpectedAccountSelected,
    #[display(fmt = "MetaMask context not initialized — activate via 'task::connect_metamask::init'")]
    MetamaskCtxNotInitialized,
}

/// Marker trait for RPC error types that can wrap a [`MetamaskRpcError`].
pub trait WithMetamaskRpcError {
    fn metamask_rpc_error(err: MetamaskRpcError) -> Self;
}

/// Marker trait for RPC error types that have an "internal error" variant.
pub trait WithInternal {
    fn internal(err: String) -> Self;
}

/// Converts a [`MetamaskError`] into any RPC error type that implements
/// both [`WithMetamaskRpcError`] and [`WithInternal`].
pub fn from_metamask_error<T>(err: MetamaskError) -> T
where
    T: WithMetamaskRpcError + WithInternal,
{
    match err {
        MetamaskError::EthProviderNotFound => T::metamask_rpc_error(MetamaskRpcError::EthProviderNotFound),
        MetamaskError::UnexpectedAccountSelected => T::metamask_rpc_error(MetamaskRpcError::UnexpectedAccountSelected),
        MetamaskError::UserCancelled => T::metamask_rpc_error(MetamaskRpcError::UserCancelled),
        other => T::internal(other.to_string()),
    }
}
