//! Re-export hub for the Tendermint coin module.
//!
//! This file provides any remaining items that don't belong in the
//! responsibility-specific modules (types, helpers, swap_ops,
//! market_ops, mm_coin) and re-exports them for external use.
//!
//! The actual public API is surfaced through `tendermint/mod.rs`:
//!   `pub use tendermint_coin::*;`
//!   `pub use tendermint_types::*;`
//!   `pub use tendermint_token::*;`

use super::tendermint_types::*;
use crate::PrivKeyPolicy;
use crypto::privkey::key_pair_from_secret;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use serde_json::Value as Json;

/// Initialise a list of HTTP RPC clients from configuration nodes.
///
/// Each `RpcNode` is validated and turned into an `HttpClient`.
/// Returns the full list of clients, or an error with the invalid URLs.
pub(crate) fn init_rpc_clients(nodes: &[RpcNode]) -> MmResult<Vec<super::rpc::HttpClient>, TendermintInitErrorKind> {
    use mm2_err_handle::prelude::*;

    let mut clients = Vec::new();
    let mut errors = Vec::new();

    for node in nodes {
        match super::rpc::HttpClient::new(node.url.as_str()) {
            Ok(client) => clients.push(client),
            Err(e) => errors.push(format!("Url {} is invalid: {}", node.url, e)),
        }
    }

    if !errors.is_empty() {
        let combined: String = errors.join(", ");
        return MmError::err(TendermintInitErrorKind::RpcClientInitError(combined));
    }

    Ok(clients)
}

/// Build a [`TendermintCoin`] from coin configuration and an activation request.
///
/// This is the platform-coin constructor the V2 activation layer drives
/// (`enable_tendermint_with_assets`). It mirrors the EVM
/// `eth_coin_from_conf_and_request` builder: it validates the supplied RPC
/// nodes, resolves the chain identity / address prefix and the native denom /
/// decimals from `conf["protocol"]["protocol_data"]`, derives the Cosmos
/// account id from the in-context wallet private key, and assembles the coin.
///
/// Only the single-key ("Iguana") private-key signing policy is constructed
/// here; HD / Ledger / WalletConnect policies belong to the task-based
/// activation variant that is blocked on shared substrate (CRD §36.6).
pub async fn tendermint_coin_from_conf_and_request(
    ctx: &MmArc,
    ticker: String,
    conf: &Json,
    nodes: Vec<RpcNode>,
    priv_key: &[u8],
    get_balances: bool,
) -> MmResult<TendermintCoin, TendermintInitError> {
    let init_err = |kind: TendermintInitErrorKind| TendermintInitError {
        ticker: ticker.clone(),
        kind,
    };

    if nodes.is_empty() {
        return MmError::err(init_err(TendermintInitErrorKind::EmptyRpcUrls));
    }

    let tendermint_conf = TendermintConf::try_from_json(&ticker, conf)?;

    let protocol_info: TendermintProtocolInfo = serde_json::from_value(conf["protocol"]["protocol_data"].clone())
        .map_to_mm(|e| {
            init_err(TendermintInitErrorKind::Internal(format!(
                "Could not parse Tendermint protocol data: {e}"
            )))
        })?;

    let account_id = account_id_from_privkey(priv_key, &protocol_info.account_prefix).mm_err(&init_err)?;

    let key_pair =
        key_pair_from_secret(priv_key).mm_err(|e| init_err(TendermintInitErrorKind::InvalidPrivKey(e.to_string())))?;
    let tendermint_key_pair = TendermintKeyPair::new(key_pair.private().secret, *key_pair.public());
    let activation_policy =
        TendermintActivationPolicy::with_private_key_policy(PrivKeyPolicy::KeyPair(tendermint_key_pair));

    let rpc_clients = init_rpc_clients(&nodes).mm_err(&init_err)?;

    let coin_impl = TendermintCoinImpl::new(
        ticker,
        tendermint_conf,
        protocol_info,
        account_id,
        activation_policy,
        rpc_clients,
        ctx.weak(),
        get_balances,
    );

    Ok(TendermintCoin::from(coin_impl))
}
