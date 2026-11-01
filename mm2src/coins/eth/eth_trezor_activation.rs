//! EVM Trezor hardware-wallet activation: the device-driven half of the
//! `task::enable_eth` activation path (CRD §50, sibling of the withdraw path in
//! [`super::eth_trezor_withdraw`]). Native, non-iOS only — the Trezor signing
//! policy exists only there.
//!
//! At activation the coin's enabled address and account public key are sourced
//! **from the connected device** (R50.1 / R50.4): the default enabled path
//! `m/44'/60'/0'/0/0` is read over the public Trezor Ethereum protocol, and the
//! resulting [`EthCoin`] is built under `EthSigner::Trezor` holding no local
//! secret. The interactive connect / PIN / passphrase / confirmation states are
//! surfaced through the supplied [`TrezorConnectProcessor`], which the caller
//! wires to the activation task's status / user-action vocabulary.

use super::eth_activation::{build_eth_hd_wallet, eth_coin_from_params_with_trezor, eth_full_derivation_path,
                            EthCoinActivationParams, EthCoinBuildError};
use super::*;
use crate::rpc_command::get_my_address::PathToAddress;
use crate::CryptoCtx;
use crypto::trezor::{ProcessTrezorResponse, TrezorProcessingError};
use crypto::{DerivationPath, HwProcessingError, Secp256k1ExtendedPublicKey, TrezorConnectProcessor};
use mm2_core::mm_ctx::MmArc;
use serde_json::Value as Json;
use std::str::FromStr;

/// Default enabled EVM derivation path (R50.4): account 0, external chain, address 0.
const EVM_TREZOR_DEFAULT_PATH: &str = "m/44'/60'/0'/0/0";

fn hw_processing_error_to_string<E: std::fmt::Display>(e: HwProcessingError<E>) -> String {
    match e {
        HwProcessingError::HwError(err) => err.to_string(),
        HwProcessingError::ProcessorError(err) => err.to_string(),
    }
}

fn trezor_processing_error_to_string<E: std::fmt::Display>(e: TrezorProcessingError<E>) -> String {
    match e {
        TrezorProcessingError::TrezorError(err) => err.to_string(),
        TrezorProcessingError::ProcessorError(err) => err.to_string(),
    }
}

/// Build an [`EthCoin`] under the Trezor signing policy from an already
/// device-derived address / account public key / derivation path (R50.1 — no
/// local secret is used or stored). This is the pure construction step; the
/// device read is performed by [`eth_coin_activate_with_trezor`].
pub async fn eth_coin_from_conf_and_request_with_trezor(
    ctx: &MmArc,
    ticker: &str,
    conf: &Json,
    req: &Json,
    protocol: CoinProtocol,
    derivation_path: DerivationPath,
    address: Address,
    public: Public,
) -> Result<EthCoin, String> {
    let params = try_s!(EthCoinActivationParams::from_legacy_request(req, conf));
    let signer = EthTrezorSigner {
        derivation_path,
        address,
        public,
    };
    eth_coin_from_params_with_trezor(
        ctx,
        ticker,
        conf,
        &params,
        signer,
        DerivationMethod::Iguana(address),
        protocol,
    )
    .await
    .map_err(|e| e.to_string())
}

/// Connect to the device (surfacing connect / PIN / passphrase through `processor`) and read the
/// address and the account public key at `derivation_path`. The device-reported public key must map
/// to the reported address before it is bound into a coin's signer.
async fn read_device_address<Processor>(
    ctx: &MmArc,
    derivation_path: &DerivationPath,
    processor: &Processor,
) -> Result<(Address, Public), String>
where
    Processor: TrezorConnectProcessor + Sync,
    Processor::Error: std::fmt::Display,
{
    let crypto_ctx = CryptoCtx::from_ctx(ctx).map_err(|e| e.to_string())?;
    let hw_ctx = crypto_ctx
        .hw_ctx()
        .ok_or_else(|| "Trezor device is not initialized".to_owned())?;

    // R50.13: connect to the device. The processor surfaces connect / PIN /
    // passphrase requests through the caller's task status / user-action
    // vocabulary and is reused for the subsequent read exchange.
    let client = hw_ctx.trezor(processor).await.map_err(|e| {
        format!(
            "Trezor connect error: {}",
            hw_processing_error_to_string(e.into_inner())
        )
    })?;

    let (address, public) = {
        let mut session = client.session().await.map_err(|e| e.to_string())?;

        let address_str = session
            .get_eth_address(derivation_path.clone(), false)
            .await
            .map_err(|e| e.to_string())?
            .process(processor)
            .await
            .map_err(|e| trezor_processing_error_to_string(e.into_inner()))?;
        let address = Address::from_str(address_str.trim_start_matches("0x"))
            .map_err(|e| format!("Invalid device EVM address '{}': {}", address_str, e))?;

        let xpub = session
            .get_eth_public_key(derivation_path.clone(), false)
            .await
            .map_err(|e| e.to_string())?
            .process(processor)
            .await
            .map_err(|e| trezor_processing_error_to_string(e.into_inner()))?;
        let ext = Secp256k1ExtendedPublicKey::from_str(&xpub).map_err(|e| format!("Invalid device EVM xpub: {}", e))?;
        let public = super::eth_hd_wallet::pubkey_from_extended(&ext);

        (address, public)
    };

    // Defensive: the device-reported leaf public key must map to the reported
    // EVM address before it is bound into the coin's signer.
    if super::public_to_address(&public) != address {
        return Err("Device EVM public key does not match the reported address".to_owned());
    }
    Ok((address, public))
}

/// Activate an EVM platform coin under the Trezor signing policy: connect to the
/// device (surfacing connect / PIN / passphrase through `processor`), read the
/// enabled address + account public key at the default EVM path from the device,
/// and build the [`EthCoin`] with `EthSigner::Trezor` (R50.1 / R50.4). No local
/// secret is derived or stored.
///
/// The coin built here is bound to the single default address (no HD wallet);
/// [`eth_coin_activate_with_trezor_hd`] is the HD form `task::enable_eth` uses.
pub async fn eth_coin_activate_with_trezor<Processor>(
    ctx: &MmArc,
    ticker: &str,
    conf: &Json,
    req: &Json,
    protocol: CoinProtocol,
    processor: &Processor,
) -> Result<EthCoin, String>
where
    Processor: TrezorConnectProcessor + Sync,
    Processor::Error: std::fmt::Display,
{
    let derivation_path =
        DerivationPath::from_str(EVM_TREZOR_DEFAULT_PATH).map_err(|e| format!("Invalid EVM derivation path: {}", e))?;
    let (address, public) = read_device_address(ctx, &derivation_path, processor).await?;
    eth_coin_from_conf_and_request_with_trezor(ctx, ticker, conf, req, protocol, derivation_path, address, public).await
}

/// Activate an EVM platform coin under the Trezor signing policy as an HD wallet whose root identity is
/// the connected device (CRD R50.1, R35.3.2): the enabled address is the one `path_to_address` selects below
/// the coin's configured `derivation_path`, read from the device.
///
/// The checks that need no device exchange run first: an initialized hardware-wallet context, hardware
/// support declared by the coin configuration (`trezor_coin`), and a usable `derivation_path`.
///
/// # Errors
///
/// [`EthCoinBuildError::InvalidPayload`] for an unusable `derivation_path`, [`EthCoinBuildError::Transport`]
/// when no node answers, and [`EthCoinBuildError::Internal`] for every hardware failure.
pub async fn eth_coin_activate_with_trezor_hd<Processor>(
    ctx: &MmArc,
    ticker: &str,
    conf: &Json,
    params: &EthCoinActivationParams,
    protocol: CoinProtocol,
    path_to_address: &PathToAddress,
    gap_limit: u32,
    processor: &Processor,
) -> Result<EthCoin, MmError<EthCoinBuildError>>
where
    Processor: TrezorConnectProcessor + Sync,
    Processor::Error: std::fmt::Display,
{
    let crypto_ctx = CryptoCtx::from_ctx(ctx).mm_err(|e| EthCoinBuildError::Internal(e.to_string()))?;
    if crypto_ctx.hw_ctx().is_none() {
        return MmError::err(EthCoinBuildError::Internal(
            "Trezor device is not initialized; connect a Trezor (task::init_trezor) first".to_owned(),
        ));
    }
    if conf["trezor_coin"].as_str().is_none() {
        return MmError::err(EthCoinBuildError::Internal(format!(
            "Coin {} does not support the Trezor hardware wallet",
            ticker
        )));
    }
    let full_path = eth_full_derivation_path(conf, path_to_address)?;
    let hd_wallet = build_eth_hd_wallet(ctx, ticker, conf, gap_limit).await?;

    let (address, public) = read_device_address(ctx, &full_path, processor)
        .await
        .map_to_mm(EthCoinBuildError::Internal)?;
    let signer = EthTrezorSigner {
        derivation_path: full_path,
        address,
        public,
    };
    eth_coin_from_params_with_trezor(
        ctx,
        ticker,
        conf,
        params,
        signer,
        DerivationMethod::HDWallet(hd_wallet),
        protocol,
    )
    .await
}
