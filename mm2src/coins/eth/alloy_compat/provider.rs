//! # Purpose
//!
//! Construction helpers for the alloy [`Provider`] used by the EVM
//! coin family. Wraps the project-specific [`AlloyTransport`] in
//! alloy's standard `RpcClient` + `RootProvider` pipeline so call-sites
//! get the full set of EIP-1474 methods (`get_balance`,
//! `get_transaction_count`, `estimate_gas`, `call`, `get_logs`,
//! `send_raw_transaction`, `get_transaction_by_hash`,
//! `get_transaction_receipt`, `get_block_number`, `get_gas_price`,
//! `get_chain_id`, `client_version`, …) as native `async fn`s.
//!
//! # Public exports
//!
//! - [`KdfProvider`] — type alias for the concrete provider used
//!   throughout the EVM stack.
//! - [`build_provider`] — convenience constructor that takes the same
//!   arguments as the legacy `Web3Transport::with_event_handlers` and
//!   returns a ready-to-use [`KdfProvider`].
//! - [`build_direct_provider`] — a provider for one node addressed on its own.
//! - [`provider_with_own_node_order`] — a copy of a provider's node order.
//!
//! # Invariants
//!
//! - **Mainnet network:** the provider is parameterised on
//!   `alloy::network::Ethereum`, matching the only network handled by
//!   web3 today (chain-specific differences are encoded in `chain_id`,
//!   not in the network type).
//! - **`is_local = false`:** matches the legacy assumption that every
//!   configured RPC URL is a remote node. Setting this affects only
//!   alloy-internal latency hints, not wire bytes.

use alloy::network::Ethereum;
use alloy::providers::{Provider, RootProvider};
use alloy::rpc::client::RpcClient;

use crate::eth::alloy_compat::transport::AlloyTransport;
use crate::RpcTransportEventHandlerShared;

/// Concrete provider type used by every EVM coin in the project.
///
/// Always parameterised on the `Ethereum` network — chain selection is
/// handled at call time via `chain_id`, not via the type system.
pub type KdfProvider = RootProvider<Ethereum>;

/// Build a [`KdfProvider`] from a list of equivalent RPC URLs and a
/// list of metrics handlers. The provider fails over between the URLs
/// (CRD ch.35 §35.1.F).
pub fn build_provider(
    urls: Vec<String>,
    event_handlers: Vec<RpcTransportEventHandlerShared>,
) -> Result<KdfProvider, String> {
    let transport = AlloyTransport::with_event_handlers(urls, event_handlers)?;
    Ok(provider_from_transport(transport))
}

/// Build a [`KdfProvider`] for one node that is addressed on its own: the multi-node logic (nonce gathering and
/// the signed-transaction broadcast of CRD ch.35 R35.1.12) lives in the caller, so the request gets the
/// lower-layer bound instead of the failover cutoff.
pub fn build_direct_provider(
    url: String,
    event_handlers: Vec<RpcTransportEventHandlerShared>,
) -> Result<KdfProvider, String> {
    let transport = AlloyTransport::direct(url, event_handlers)?;
    Ok(provider_from_transport(transport))
}

/// A provider over the same nodes as `provider` that starts from `provider`'s current node order and keeps its
/// own order afterwards (CRD ch.35 R35.1.7 / D35.8 (i): a token that starts from a copy). `None` when the
/// provider is not backed by an [`AlloyTransport`].
pub fn provider_with_own_node_order(provider: &KdfProvider) -> Option<KdfProvider> {
    let transport = provider
        .client()
        .transport()
        .as_any()
        .downcast_ref::<AlloyTransport>()?;
    Some(provider_from_transport(transport.with_own_order()))
}

pub(crate) fn provider_from_transport(transport: AlloyTransport) -> KdfProvider {
    RootProvider::new(RpcClient::new(transport, false))
}
