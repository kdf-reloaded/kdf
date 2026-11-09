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
use crate::node_url::{host_only, scrub_node_url, scrub_urls_in_text};
use crate::rpc_command::get_my_address::PathToAddress;
use crate::utxo::utxo_builder::derive_activated_hd_secret;
use crate::PrivKeyPolicy;
use crypto::privkey::key_pair_from_secret;
use crypto::GlobalHDAccountArc;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use serde_json::Value as Json;

/// The largest number of decimal places a Tendermint coin may declare (CRD R36.3.1).
const MAX_TENDERMINT_DECIMALS: u8 = 18;

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
            // The configured string is never echoed (CRD ch.36 R36.5A.1): the node is named host-only, an
            // entry without a usable scheme and host by a fixed placeholder, and the lower layer's text is
            // scrubbed of the URL.
            Err(e) => errors.push(format!(
                "Url {} is invalid: {}",
                host_only(&node.url),
                scrub_urls_in_text(&scrub_node_url(&e.to_string(), &node.url))
            )),
        }
    }

    if !errors.is_empty() {
        let combined: String = errors.join(", ");
        return MmError::err(TendermintInitErrorKind::RpcClientInitError(combined));
    }

    Ok(clients)
}

/// The key a Tendermint platform coin signs with, and where it comes from.
enum ActivationKey<'a> {
    /// The single key of an iguana login.
    Single(&'a [u8]),
    /// The key at `path_to_address` under the coin's `derivation_path`, derived from the seed of an HD login
    /// (CRD ch.36 R36.1.2 `path_to_address`, R36.1.4 step 3).
    GlobalHd {
        global_hd: &'a GlobalHDAccountArc,
        path_to_address: &'a PathToAddress,
    },
}

/// Build a [`TendermintCoin`] from coin configuration and an activation request.
///
/// This is the platform-coin constructor the V2 activation layer drives
/// (`enable_tendermint_with_assets`) under an iguana login. It mirrors the EVM
/// `eth_coin_from_conf_and_request` builder: it validates the supplied RPC
/// nodes, resolves the chain identity / address prefix and the native denom /
/// decimals from `conf["protocol"]["protocol_data"]`, derives the Cosmos
/// account id from the given single private key, and assembles the coin.
///
/// Under an HD login use [`tendermint_coin_from_conf_and_request_with_global_hd`]. The WalletConnect, hardware
/// and public-key policies are not built here.
pub async fn tendermint_coin_from_conf_and_request(
    ctx: &MmArc,
    ticker: String,
    conf: &Json,
    nodes: Vec<RpcNode>,
    priv_key: &[u8],
    get_balances: bool,
) -> MmResult<TendermintCoin, TendermintInitError> {
    build_tendermint_coin(ctx, ticker, conf, nodes, ActivationKey::Single(priv_key), get_balances).await
}

/// Build a [`TendermintCoin`] under an HD login (CRD ch.36 R36.1.2, R36.1.4 step 3).
///
/// The coin's key is derived from the seed at the coin's configured `derivation_path` extended by
/// `path_to_address`, the way the other HD coins derive theirs, and the account id is derived from that key. The
/// public key of the activation policy is derived from the activated private key, not taken from the wallet's
/// root key.
///
/// # Errors
///
/// A coin configuration without `derivation_path`, or a path that cannot be derived, fails with
/// [`TendermintInitErrorKind::DerivationPathMissing`] or [`TendermintInitErrorKind::HdDerivationFailed`]; every
/// other failure is the one of [`tendermint_coin_from_conf_and_request`].
pub async fn tendermint_coin_from_conf_and_request_with_global_hd(
    ctx: &MmArc,
    ticker: String,
    conf: &Json,
    nodes: Vec<RpcNode>,
    global_hd: &GlobalHDAccountArc,
    path_to_address: &PathToAddress,
    get_balances: bool,
) -> MmResult<TendermintCoin, TendermintInitError> {
    let key = ActivationKey::GlobalHd {
        global_hd,
        path_to_address,
    };
    build_tendermint_coin(ctx, ticker, conf, nodes, key, get_balances).await
}

async fn build_tendermint_coin(
    ctx: &MmArc,
    ticker: String,
    conf: &Json,
    nodes: Vec<RpcNode>,
    key: ActivationKey<'_>,
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

    // The bound is checked here and not while parsing the coin protocol, so that an out-of-range value fails
    // the activation as a platform-coin creation error (CRD R36.3.2).
    if protocol_info.decimals > MAX_TENDERMINT_DECIMALS {
        return MmError::err(init_err(TendermintInitErrorKind::DecimalsAboveMaximum(
            protocol_info.decimals,
        )));
    }

    // The pair of a secret and the public key derived from that same secret.
    let key_pair_of = |secret: &[u8]| -> MmResult<TendermintKeyPair, TendermintInitError> {
        let key_pair = key_pair_from_secret(secret)
            .mm_err(|e| init_err(TendermintInitErrorKind::InvalidPrivKey(e.to_string())))?;
        Ok(TendermintKeyPair::new(key_pair.private().secret, *key_pair.public()))
    };

    let (account_id, activation_policy) = match key {
        ActivationKey::Single(priv_key) => {
            let account_id = account_id_from_privkey(priv_key, &protocol_info.account_prefix).mm_err(&init_err)?;
            let tendermint_key_pair = key_pair_of(priv_key)?;
            let policy = PrivKeyPolicy::KeyPair(tendermint_key_pair);
            (account_id, TendermintActivationPolicy::with_private_key_policy(policy))
        },
        ActivationKey::GlobalHd {
            global_hd,
            path_to_address,
        } => {
            let derivation_path = tendermint_conf
                .derivation_path
                .as_ref()
                .or_mm_err(|| init_err(TendermintInitErrorKind::DerivationPathMissing))?;
            let secret = derive_activated_hd_secret(global_hd, derivation_path, path_to_address)
                .mm_err(|e| init_err(TendermintInitErrorKind::HdDerivationFailed(e.to_string())))?;
            let account_id =
                account_id_from_privkey(secret.as_slice(), &protocol_info.account_prefix).mm_err(&init_err)?;
            // The public key is derived from the activated private key. Pairing the derived private key with
            // the wallet's root public key would make the coin's public key disagree with its address.
            let activated_key = key_pair_of(secret.as_slice())?;
            let policy = PrivKeyPolicy::HDWallet {
                activated_key,
                bip39_secp_priv_key: global_hd.root_priv_key().clone(),
                path_to_address: path_to_address.clone(),
            };
            (account_id, TendermintActivationPolicy::with_private_key_policy(policy))
        },
    };

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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod node_naming_tests {
    use super::*;

    const URL: &str = "http://user:secret@127.0.0.1:1/rpc/APIKEY?token=X";
    const SECRETS: [&str; 5] = ["user", "secret", "/rpc/", "APIKEY", "token=X"];

    /// T36.14 (R36.5A.1): an unusable node URL is not echoed, and a usable one that fails is named host-only.
    #[test]
    fn a_node_url_that_cannot_be_used_is_not_echoed() {
        let nodes = [
            RpcNode::for_test("secret-key-as-a-url"),
            RpcNode::for_test("ftp://user:secret@host.example/rpc/APIKEY?token=X"),
        ];
        let err = init_rpc_clients(&nodes).unwrap_err().to_string();
        assert!(err.contains("<invalid node URL>"), "{err}");
        for secret in SECRETS.iter().chain(&["secret-key-as-a-url"]) {
            assert!(!err.contains(secret), "a credential part is echoed");
        }
    }

    /// T36.14 (R36.5A.2): the debug form of a node entry is host-only.
    #[test]
    fn the_debug_form_of_a_node_entry_is_host_only() {
        let rendered = format!("{:?}", RpcNode::for_test(URL));
        assert!(rendered.contains("http://127.0.0.1:1"), "{rendered}");
        for secret in SECRETS {
            assert!(!rendered.contains(secret), "a credential part is rendered");
        }
    }
}
