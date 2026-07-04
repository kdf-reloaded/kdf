//! # Purpose
//!
//! Public MetaMask session API. Provides the three operations the
//! desktop wallet actually uses against MetaMask: requesting the
//! active account, switching the EVM chain, and producing an
//! EIP-712 signature.
//!
//! # Public exports
//!
//! - [`detect_metamask_provider`] — try to grab the browser-injected
//!   provider, returning [`crate::MetamaskError::EthProviderNotFound`]
//!   when no extension is present.
//! - [`MetamaskSession`] — RAII guard around a global mutex so chain
//!   switching and signing cannot interleave across concurrent tasks.
//!
//! # Invariants
//!
//! - **JSON-RPC method names are wire-stable.** `eth_requestAccounts`,
//!   `wallet_switchEthereumChain`, and `eth_signTypedData_v4` are the
//!   exact names MetaMask expects; do not rename.
//! - **Single in-flight session.** `SESSION_GUARD` enforces strict
//!   serialisation across the whole process; only one `MetamaskSession`
//!   may be live at any given moment.
//! - **Account match check is the caller's responsibility.** This
//!   module just signs whatever address it is handed; mismatches
//!   between the requested address and the currently active MetaMask
//!   account are detected at a layer above (see
//!   `MetamaskError::UnexpectedAccountSelected`).

use crate::eip_1193_provider::Eip1193Provider;
use crate::metamask_error::{MetamaskError, MetamaskResult};
use futures::lock::{Mutex as AsyncMutex, MutexGuard as AsyncMutexGuard};
use itertools::Itertools;
use lazy_static::lazy_static;
use mm2_err_handle::prelude::*;
use mm2_eth::typed_data::{Eip712, H256};
use serde::Serialize;
use serde_json::{json, Value as Json};

lazy_static! {
    /// Serialises MetaMask requests: only one in-flight at a time so the
    /// active chain ID cannot change mid-request.
    static ref SESSION_GUARD: AsyncMutex<()> = AsyncMutex::new(());
}

/// Tries to detect a browser-injected MetaMask (EIP-1193) provider.
pub fn detect_metamask_provider() -> MetamaskResult<Eip1193Provider> {
    Eip1193Provider::detect().or_mm_err(|| MetamaskError::EthProviderNotFound)
}

/// An exclusive session with the MetaMask extension.
///
/// Acquiring a session locks a global mutex so that chain-switching and
/// signing cannot interleave across concurrent tasks.
pub struct MetamaskSession<'a> {
    transport: &'a Eip1193Provider,
    _guard: AsyncMutexGuard<'a, ()>,
}

impl<'a> MetamaskSession<'a> {
    /// Acquires the global session lock.
    pub async fn lock(transport: &'a Eip1193Provider) -> Self {
        MetamaskSession {
            transport,
            _guard: SESSION_GUARD.lock().await,
        }
    }

    /// Requests the user's active ETH account via `eth_requestAccounts`.
    ///
    /// Expects exactly one account; returns an error otherwise.
    pub async fn eth_request_account(&self) -> MetamaskResult<String> {
        let accounts: Vec<String> = self
            .transport
            .call_method("eth_requestAccounts", vec![])
            .await
            .map_to_mm(MetamaskError::from)?;
        accounts
            .into_iter()
            .exactly_one()
            .map_to_mm(|_| MetamaskError::ExpectedOneEthAccount)
    }

    /// Asks MetaMask to switch to the given EVM chain.
    ///
    /// On success the wallet returns `null`; we accept any payload
    /// (the result is unused) and only surface errors.
    pub async fn wallet_switch_ethereum_chain(&self, chain_id: u64) -> MetamaskResult<()> {
        let req = json!({
            "chainId": format!("0x{chain_id:x}"),
        });
        let _: Json = self
            .transport
            .call_method("wallet_switchEthereumChain", vec![req])
            .await
            .map_to_mm(MetamaskError::from)?;
        Ok(())
    }

    /// Signs EIP-712 typed data via `eth_signTypedData_v4` and returns the
    /// message hash together with the hex-encoded signature.
    ///
    /// `user_address` must match the currently active MetaMask account.
    pub async fn sign_typed_data_v4<Domain, Message>(
        &self,
        user_address: String,
        request: Eip712<Domain, Message>,
    ) -> MetamaskResult<(H256, String)>
    where
        Domain: Serialize,
        Message: Serialize,
    {
        let addr_json = Json::String(user_address);
        let request_json =
            serde_json::to_string(&request).map_to_mm(|e| MetamaskError::ErrorSerializingArguments(e.to_string()))?;

        let hash = mm2_eth::typed_data::hash_typed_data(request)
            .map_err(|e| MetamaskError::Internal(format!("EIP-712 hashing error: {e}")))?;

        let signature: String = self
            .transport
            .call_method("eth_signTypedData_v4", vec![addr_json, Json::String(request_json)])
            .await
            .map_to_mm(MetamaskError::from)?;

        Ok((hash, signature))
    }

    /// Hands an unsigned transaction to the wallet to **sign and broadcast**
    /// via `eth_sendTransaction`, returning the broadcast transaction hash as a
    /// hex string (CRD R47.5.6/R47.5.8). The framework holds no key; the wallet
    /// signs with the account's own key and broadcasts it itself.
    ///
    /// `tx` is the EIP-1193 transaction object — `from`, `to`, `value`, `data`,
    /// `gas`, and either `gasPrice` or `maxFeePerGas`/`maxPriorityFeePerGas`
    /// (`nonce` optional). It is passed through verbatim as the single request
    /// parameter, mirroring the `call_method` pattern used elsewhere here.
    pub async fn eth_send_transaction(&self, tx: Json) -> MetamaskResult<String> {
        self.transport
            .call_method("eth_sendTransaction", vec![tx])
            .await
            .map_to_mm(MetamaskError::from)
    }

    /// Reads the wallet's currently-connected accounts via `eth_accounts`
    /// (read-only; does not prompt). Used for the per-operation active-account
    /// consistency re-check (CRD R47.5.9).
    pub async fn eth_accounts(&self) -> MetamaskResult<Vec<String>> {
        self.transport
            .call_method("eth_accounts", vec![])
            .await
            .map_to_mm(MetamaskError::from)
    }

    /// Reads the wallet's active EIP-155 chain id via `eth_chainId` (hex
    /// quantity string, e.g. `"0x1"`). Used for the chain-consistency check
    /// before broadcast (CRD R47.5.10).
    pub async fn eth_chain_id(&self) -> MetamaskResult<String> {
        self.transport
            .call_method("eth_chainId", vec![])
            .await
            .map_to_mm(MetamaskError::from)
    }
}
