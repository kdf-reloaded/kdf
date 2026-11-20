//! The TRON platform on the mmrpc 2.0 activation route (CRD ch.21 §21.4, R21.10B.2, R21.10B.4, R21.10B.5).
//!
//! `enable_eth_with_tokens` and `task::enable_eth::*` build a TRX platform coin through
//! [`enable_tron_platform_coin`]: the availability gate, the checks that need neither a key nor a node, the node
//! list of the request, and the coin builder of the `coins` crate. Under a global-HD login the coin is an HD wallet
//! on the coin's `derivation_path` (CRD ch.21 R-A2, R21.10B.4) unless the request names the `IguanaPrivKey` policy;
//! otherwise it is the single-key coin of the node's internal key. The platform's TRC20 tokens go through the same
//! token initializer as ERC-20 tokens (`erc20_token_activation`) and use the platform's HD wallet.
//!
//! # Invariants
//!
//! - While `coins::TRON_ACTIVATION_AVAILABLE` is `false` no TRON coin is built or registered: the route answers
//!   the same "not available" refusal as the legacy `enable` and `electrum` routes. Unit tests open the gate per
//!   context with [`TRON_TEST_OVERRIDE_KEY`], a key that exists in test builds only.
//! - Every refusal of this module comes before any key is read and any node is contacted.
//! - A node's headers and the API key are secrets: no error of this module renders one.

use super::*;
use coins::eth::tron::nodes::{parse_nodes, TronApiKey, TronNode, TronNodeConfigError};
use coins::eth::tron::{tron_coin_from_params, tron_coin_from_params_with_global_hd, tron_required_confirmations,
                       TronCoinParams};
use coins::{TRON_ACTIVATION_AVAILABLE, TRON_NOT_AVAILABLE};

/// The `MM2.json` key that opens the TRON gate for one test context. Read in test builds only.
#[cfg(test)]
pub(crate) const TRON_TEST_OVERRIDE_KEY: &str = "tron_activation_available_for_test";

/// Whether the TRON activation routes are open for `ctx`: the build switch, or, in a test build, the override key
/// of the context.
pub(crate) fn tron_activation_available(ctx: &MmArc) -> bool { TRON_ACTIVATION_AVAILABLE || test_override(ctx) }

#[cfg(test)]
fn test_override(ctx: &MmArc) -> bool { ctx.conf[TRON_TEST_OVERRIDE_KEY].as_bool() == Some(true) }

#[cfg(not(test))]
fn test_override(_ctx: &MmArc) -> bool { false }

/// The refusal of the platform route while TRON is switched off: the text of the legacy routes, as a
/// `PlatformCoinCreationError` like the other families that are switched off.
fn not_available(ticker: &str) -> MmError<ActivationErr> {
    MmError::new(ActivationErr::PlatformCoinCreationError {
        ticker: ticker.to_owned(),
        error: TRON_NOT_AVAILABLE.to_owned(),
    })
}

/// What the request says about its signing policy, reduced to the facts the TRON rules read. A separate value so
/// that the rules can be tested on a native build, where the browser-only MetaMask policy does not exist.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct KeyPolicyFacts {
    pub trezor: bool,
    pub wallet_connect: bool,
    /// The `Metamask` value of `priv_key_policy`.
    pub metamask_policy: bool,
    /// The `Metamask` value of `rpc_mode`.
    pub metamask_rpc_mode: bool,
    /// The `IguanaPrivKey` value of `priv_key_policy`: the single key of the node, whatever the login. It is not a
    /// refusal; it selects the coin the activation builds under a global-HD login.
    pub iguana_priv_key: bool,
}

/// Why a TRON platform activation is refused by its key policy, in the order the rules are checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum KeyPolicyRefusal {
    /// A TRON chain has no EVM chain identifier (CRD ch.21 R21.10B.4).
    WalletConnect,
    /// The TRON withdraw refuses a MetaMask key (CRD ch.47 R47.5.6a), so the activation refuses it too (CRD ch.21
    /// D21.10B.3 (b), decided 2026-11-17), whichever of the two settings names it.
    MetaMask,
    /// The device is never driven for TRON (CRD ch.21 R21.10B.4, ch.49 R49.9).
    Trezor,
}

/// The first refusal of the TRON key-policy rules for a request whose facts are `facts`. The login does not
/// matter: under a global-HD login the default, `ContextPrivKey` and HD-wallet policies build the HD wallet, and
/// `IguanaPrivKey` the single key of the node.
pub(super) fn key_policy_refusal(facts: KeyPolicyFacts) -> Option<KeyPolicyRefusal> {
    if facts.wallet_connect {
        Some(KeyPolicyRefusal::WalletConnect)
    } else if facts.metamask_policy || facts.metamask_rpc_mode {
        Some(KeyPolicyRefusal::MetaMask)
    } else if facts.trezor {
        Some(KeyPolicyRefusal::Trezor)
    } else {
        None
    }
}

impl KeyPolicyRefusal {
    /// The wire error of the refusal: `Internal` for the policies the reference refuses (R21.10B.4) and for the
    /// MetaMask refusal (D21.10B.3 (b): the token of the route).
    pub(super) fn into_error(self) -> ActivationErr {
        match self {
            KeyPolicyRefusal::WalletConnect => ActivationErr::Internal(
                "TRON does not support the WalletConnect policy: a TRON chain has no EVM chain identifier".to_owned(),
            ),
            KeyPolicyRefusal::MetaMask => ActivationErr::Internal(
                "TRON does not support the MetaMask policy or rpc_mode: a TRON withdraw cannot be signed with a \
                 MetaMask key"
                    .to_owned(),
            ),
            KeyPolicyRefusal::Trezor => {
                ActivationErr::Internal("TRON does not support the Trezor hardware wallet policy".to_owned())
            },
        }
    }
}

impl EthWithTokensActivationRequest {
    /// The facts of the request's signing policy that the TRON rules read.
    pub(super) fn key_policy_facts(&self) -> KeyPolicyFacts {
        KeyPolicyFacts {
            trezor: self.is_trezor_policy(),
            wallet_connect: matches!(self.priv_key_policy, Some(EthPrivKeyPolicy::WalletConnect { .. })),
            iguana_priv_key: matches!(self.priv_key_policy, Some(EthPrivKeyPolicy::IguanaPrivKey)),
            #[cfg(target_arch = "wasm32")]
            metamask_policy: matches!(self.priv_key_policy, Some(EthPrivKeyPolicy::Metamask)),
            #[cfg(not(target_arch = "wasm32"))]
            metamask_policy: false,
            #[cfg(target_arch = "wasm32")]
            metamask_rpc_mode: self.rpc_mode == Some(EthRpcMode::Metamask),
            #[cfg(not(target_arch = "wasm32"))]
            metamask_rpc_mode: false,
        }
    }
}

/// The node list of the request as TRON nodes (CRD ch.21 R21.10B.2, R-N9): each entry's `url`, its optional
/// `headers`, and the `tron_api_key` of `MM2.json` for the TronGrid hosts. `komodo_proxy` is parsed with the request
/// and, as on the EVM route, does not sign requests (CRD ch.35). No node is contacted.
///
/// # Errors
///
/// `AtLeastOneNodeRequired` for an empty or absent list; `InvalidPayload` for an unparsable URL, a scheme other
/// than `http` and `https`, or a header that is not valid; `Internal` for an invalid `tron_api_key`. No error
/// renders a URL beyond its host, a header, or the key.
pub(super) fn tron_nodes(
    ctx: &MmArc,
    request: &EthWithTokensActivationRequest,
) -> Result<Vec<TronNode>, MmError<ActivationErr>> {
    let entries: Vec<Json> = request
        .nodes
        .iter()
        .flatten()
        .map(|node| {
            let mut entry = serde_json::Map::new();
            entry.insert("url".to_owned(), Json::String(node.url.clone()));
            if !node.headers.is_null() {
                entry.insert("headers".to_owned(), node.headers.clone());
            }
            Json::Object(entry)
        })
        .collect();
    let api_key = TronApiKey::from_conf(&ctx.conf).map_to_mm(|e| ActivationErr::Internal(e.to_string()))?;
    parse_nodes(&Json::Array(entries), api_key.as_ref()).map_to_mm(|e| match &e {
        TronNodeConfigError::NoNodes => ActivationErr::AtLeastOneNodeRequired(e.to_string()),
        TronNodeConfigError::BadEntry { position } => ActivationErr::InvalidPayload(format!(
            "TRON node {position} has a 'headers' value that is not an object of header names and values"
        )),
        other => ActivationErr::InvalidPayload(other.to_string()),
    })
}

/// The refusals that need neither a key nor a node, in their order: the availability gate, the NFT request
/// (`Internal`, before any key or node work), then the key policy (R21.10B.4).
pub(super) fn tron_precheck(
    ctx: &MmArc,
    ticker: &str,
    request: &EthWithTokensActivationRequest,
) -> Result<(), MmError<ActivationErr>> {
    if !tron_activation_available(ctx) {
        return Err(not_available(ticker));
    }
    if request.nft_req.is_some() {
        return MmError::err(ActivationErr::Internal(
            "TRON has no NFT support: the request carries nft_req".to_owned(),
        ));
    }
    match key_policy_refusal(request.key_policy_facts()) {
        Some(refusal) => MmError::err(refusal.into_error()),
        None => Ok(()),
    }
}

/// Builds the TRX platform coin of a `enable_eth_with_tokens` or `task::enable_eth` request (CRD ch.21 R-A1,
/// R21.10B.2): the checks of [`tron_precheck`], the swap-contract rule of F21.B3, the node list, and the coin. The
/// coin is the HD wallet of the coin's `derivation_path` under a global-HD login (R-A2, R21.10B.4), and the
/// single-key coin of the node's internal key under an iguana login or for the `IguanaPrivKey` policy. The tokens
/// of the request are enabled afterwards by the shared tail of the activation.
///
/// # Errors
///
/// The refusals above, `InvalidPayload` for a swap contract that F21.B3 refuses and for a global-HD activation of
/// a coin config without `derivation_path`, and the build errors of [`tron_coin_from_params`].
pub(super) async fn enable_tron_platform_coin(
    ctx: &MmArc,
    ticker: &str,
    platform_conf: &Json,
    request: &EthWithTokensActivationRequest,
    network: &TronNetwork,
) -> Result<EthCoin, MmError<ActivationErr>> {
    tron_precheck(ctx, ticker, request)?;
    // F21.B3: ignored for a wallet-only coin, required and non-zero for a trading coin. The coin itself keeps the
    // swap-contract binding empty (R-A1 step 5); only the request is validated here.
    activation_params(ctx, platform_conf, request, true)?;
    let nodes = tron_nodes(ctx, request)?;

    let crypto_ctx = CryptoCtx::from_ctx(ctx).mm_err(|e| ActivationErr::Internal(e.to_string()))?;
    let params = TronCoinParams {
        nodes,
        required_confirmations: tron_required_confirmations(request.required_confirmations, platform_conf),
        // The history is enabled only when the node list also has a keyed TronGrid node (CRD ch.21 §21.10C).
        tx_history: request.tx_history(),
    };
    let protocol = CoinProtocol::TRX {
        network: network.clone(),
    };
    let coin = match crypto_ctx.key_pair_policy() {
        KeyPairPolicy::GlobalHDAccount(global_hd) if !request.key_policy_facts().iguana_priv_key => {
            tron_coin_from_params_with_global_hd(
                ctx,
                ticker,
                platform_conf,
                params,
                global_hd,
                &request.path_to_address(),
                request.gap_limit(),
                protocol,
            )
            .await
        },
        // `IguanaPrivKey` pins the single-address coin to the node's internal key under either login (CRD ch.21
        // status note of 2026-11-17, kept).
        _ => {
            tron_coin_from_params(
                ctx,
                ticker,
                platform_conf,
                params,
                crypto_ctx.mm2_internal_privkey_slice(),
                protocol,
            )
            .await
        },
    };
    coin.mm_err(ActivationErr::from)
}

#[cfg(all(test, not(target_arch = "wasm32")))] mod tests;
