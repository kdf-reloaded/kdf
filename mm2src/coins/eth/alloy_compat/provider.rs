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
//! - `direct_node_line` — the R35.1.16 log line for the node of a direct provider.
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
use crate::eth::node_url::ListedNode;
use crate::RpcTransportEventHandlerShared;

/// Concrete provider type used by every EVM coin in the project.
///
/// Always parameterised on the `Ethereum` network — chain selection is
/// handled at call time via `chain_id`, not via the type system.
pub type KdfProvider = RootProvider<Ethereum>;

/// Build a [`KdfProvider`] from a list of equivalent RPC nodes and a
/// list of metrics handlers. The provider fails over between the nodes
/// (CRD ch.35 §35.1.F). Each node carries the ticker and the list position its log lines name (R35.1.16).
pub fn build_provider(
    nodes: Vec<ListedNode>,
    event_handlers: Vec<RpcTransportEventHandlerShared>,
) -> Result<KdfProvider, String> {
    let transport = AlloyTransport::with_event_handlers(nodes, event_handlers)?;
    Ok(provider_from_transport(transport))
}

/// Build a [`KdfProvider`] for one node that is addressed on its own: the multi-node logic (nonce gathering and
/// the signed-transaction broadcast of CRD ch.35 R35.1.12) lives in the caller, so the request gets the
/// lower-layer bound instead of the failover cutoff.
pub fn build_direct_provider(
    node: ListedNode,
    event_handlers: Vec<RpcTransportEventHandlerShared>,
) -> Result<KdfProvider, String> {
    let transport = AlloyTransport::direct(node, event_handlers)?;
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

/// A log line that names the node of a provider that talks to one node on its own, in the form of CRD ch.35
/// R35.1.16: `[<ticker>] <before><host> (node <i> of <n>)<after>`. `None` when the provider is not backed by an
/// [`AlloyTransport`] or has no node.
pub(crate) fn direct_node_line(provider: &KdfProvider, before: &str, after: &str) -> Option<String> {
    provider
        .client()
        .transport()
        .as_any()
        .downcast_ref::<AlloyTransport>()?
        .first_node_line(before, after)
}

pub(crate) fn provider_from_transport(transport: AlloyTransport) -> KdfProvider {
    RootProvider::new(RpcClient::new(transport, false))
}
