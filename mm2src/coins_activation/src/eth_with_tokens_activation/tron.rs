//! The TRON platform on the mmrpc 2.0 activation route (CRD ch.21 §21.4, R21.10B.2, R21.10B.4, R21.10B.5).
//!
//! `enable_eth_with_tokens` and `task::enable_eth::*` build a TRX platform coin through
//! [`enable_tron_platform_coin`]: the checks that need neither a key nor a node, the node
//! list of the request, and the coin builder of the `coins` crate. Under a global-HD login the coin is an HD wallet
//! on the coin's `derivation_path` (CRD ch.21 R-A2, R21.10B.4) unless the request names the `IguanaPrivKey` policy;
//! otherwise it is the single-key coin of the node's internal key. The platform's TRC20 tokens go through the same
//! token initializer as ERC-20 tokens (`erc20_token_activation`) and use the platform's HD wallet.
//!
//! # Invariants
//!
//! - A TRON coin activates as a wallet-only coin: while `coins::TRON_SWAPS_AVAILABLE` is `false` it reports
//!   `wallet_only` whatever its coins-config entry says (CRD ch.21 R-D1, R21.B3), and its swap-contract members are
//!   validated and not bound. With the switch on, a coin that can trade is bound to them (CRD ch.21 R21.B2).
//! - Every refusal of this module comes before any key is read and any node is contacted.
//! - A node's headers and the API key are secrets: no error of this module renders one. Neither does it render a
//!   value of the request's `tron_gasless_provider` (CRD ch.63 R9).

use super::*;
use coins::eth::tron::nodes::{parse_nodes, TronApiKey, TronNode, TronNodeConfigError};
use coins::eth::tron::{tron_coin_from_params, tron_coin_from_params_with_global_hd, tron_required_confirmations,
                       tron_swap_contracts, GasFreeBinding, TronCoinParams};

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

/// The GasFree provider binding of the request for a platform on `network` (CRD ch.63 §63.3.1): `None` without a
/// `tron_gasless_provider`. Neither a node nor the provider is contacted.
///
/// # Errors
///
/// `InvalidPayload` for a `base_url` that is more than a scheme and a host (R13) and for a `service_provider` that
/// is not a well-formed TRON address. The text repeats no value of the request.
pub(super) fn tron_gasfree_binding(
    request: &EthWithTokensActivationRequest,
    network: &TronNetwork,
) -> Result<Option<GasFreeBinding>, MmError<ActivationErr>> {
    request
        .tron_gasless_provider
        .as_ref()
        .map(|provider| GasFreeBinding::from_request(network, provider))
        .transpose()
        .map_to_mm(|e| ActivationErr::InvalidPayload(e.to_string()))
}

/// The refusals that need neither a key nor a node, in their order: the NFT request (`Internal`, before any key or
/// node work), then the key policy (R21.10B.4).
pub(super) fn tron_precheck(request: &EthWithTokensActivationRequest) -> Result<(), MmError<ActivationErr>> {
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
/// R21.10B.2): the checks of [`tron_precheck`], the swap-contract rule of F21.B3 and the binding of R21.B2, the
/// GasFree provider binding of [`tron_gasfree_binding`], the node list, and the coin. The
/// coin is the HD wallet of the coin's `derivation_path` under a global-HD login (R-A2, R21.10B.4), and the
/// single-key coin of the node's internal key under an iguana login or for the `IguanaPrivKey` policy. The tokens
/// of the request are enabled afterwards by the shared tail of the activation.
///
/// # Errors
///
/// The refusals above, `InvalidPayload` for a swap contract that F21.B3 refuses, for a `tron_gasless_provider` that
/// [`tron_gasfree_binding`] refuses and for a global-HD activation of a coin config without `derivation_path`, and
/// the build errors of [`tron_coin_from_params`].
pub(super) async fn enable_tron_platform_coin(
    ctx: &MmArc,
    ticker: &str,
    platform_conf: &Json,
    request: &EthWithTokensActivationRequest,
    network: &TronNetwork,
) -> Result<EthCoin, MmError<ActivationErr>> {
    enable_tron_platform_coin_with_switch(
        ctx,
        ticker,
        platform_conf,
        request,
        network,
        coins::TRON_SWAPS_AVAILABLE,
    )
    .await
}

/// [`enable_tron_platform_coin`] with the TRON swap switch given, so a test can activate a coin that can trade.
pub(super) async fn enable_tron_platform_coin_with_switch(
    ctx: &MmArc,
    ticker: &str,
    platform_conf: &Json,
    request: &EthWithTokensActivationRequest,
    network: &TronNetwork,
    swaps_available: bool,
) -> Result<EthCoin, MmError<ActivationErr>> {
    tron_precheck(request)?;
    // F21.B3: ignored for a wallet-only coin, required and non-zero for a trading coin. This validation reads the
    // coin config alone, so it holds with the swap switch off too. The binding (R21.B2) is `None` for a wallet-only
    // coin, which every TRON coin is while the switch is off.
    let evm_params = activation_params(ctx, platform_conf, request, true)?;
    let swap_contracts = tron_swap_contracts(
        platform_conf,
        evm_params.swap_contract_address,
        evm_params.fallback_swap_contract,
        swaps_available,
    )
    .mm_err(ActivationErr::from)?;
    let gasfree = tron_gasfree_binding(request, network)?;
    let nodes = tron_nodes(ctx, request)?;

    let crypto_ctx = CryptoCtx::from_ctx(ctx).mm_err(|e| ActivationErr::Internal(e.to_string()))?;
    let params = TronCoinParams {
        nodes,
        required_confirmations: tron_required_confirmations(request.required_confirmations, platform_conf),
        // The history is enabled only when the node list also has a keyed TronGrid node (CRD ch.21 §21.10C).
        tx_history: request.tx_history(),
        gasfree,
        swap_contracts,
        swap_evidence_node: request.swap_evidence_node.clone(),
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
