//! # `get_my_address` RPC — the operator's own wallet address (R-K11).
//!
//! Returns the wallet address currently in use for one configured coin
//! **without requiring that coin to be activated first**. It surrenders only a
//! public address — never any secret key material — and is therefore the
//! address-surface companion to the read-only `get_public_key` /
//! `get_public_key_hash` methods.
//!
//! The current bound protocol scope is EVM / ETH-protocol coins; a coin whose
//! protocol does not support own-address resolution is refused.

use common::HttpStatusCode;
use crypto::{Bip32DerPathOps, Bip44Chain, ChildNumber, CryptoCtx, CryptoCtxError, GlobalHDAccountArc, HDPathToCoin,
             KeyPairPolicy, Secp256k1Secret};
use derive_more::Display;
use http::StatusCode;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use ser_error_derive::SerializeErrorType;
use serde::{Deserialize, Serialize};

use crate::eth::{addr_from_raw_pubkey, checksum_address};
use crate::{coin_conf, CoinProtocol};

// ── Request / Response types ────────────────────────────────────────────

/// Optional BIP-44 coordinate selecting a specific hierarchical-deterministic
/// address. Absent fields default to the first account's first external
/// address (account 0, external branch, address 0).
#[derive(Debug, Deserialize)]
pub struct PathToAddress {
    /// BIP-44 account index.
    #[serde(default)]
    pub account_id: u32,
    /// BIP-44 change-level branch selector (external vs internal).
    #[serde(default = "external_chain")]
    pub chain: Bip44Chain,
    /// BIP-44 address index.
    #[serde(default)]
    pub address_id: u32,
}

fn external_chain() -> Bip44Chain { Bip44Chain::External }

#[derive(Deserialize)]
pub struct GetMyAddressRequest {
    /// Ticker of the configured coin whose own-address is queried.
    pub coin: String,
    /// Optional HD coordinate; absent → account 0 / external / address 0.
    #[serde(default)]
    pub path_to_address: Option<PathToAddress>,
}

#[derive(Serialize)]
pub struct GetMyAddressResponse {
    /// Echoes the requested ticker.
    pub coin: String,
    /// The resolved address in the coin's native display form.
    pub wallet_address: String,
}

// ── Error type ──────────────────────────────────────────────────────────

#[derive(Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum GetMyAddressError {
    #[display(fmt = "Coin configuration check failed: {}", _0)]
    CoinsConfCheckError(String),
    #[display(fmt = "Coin is not supported: {}", _0)]
    CoinIsNotSupported(String),
    #[display(fmt = "Invalid request: {}", _0)]
    InvalidRequest(String),
    #[display(fmt = "Internal error: {}", _0)]
    Internal(String),
    #[display(fmt = "Failed to get eth address: {}", _0)]
    GetEthAddressError(String),
}

impl HttpStatusCode for GetMyAddressError {
    fn status_code(&self) -> StatusCode {
        match self {
            GetMyAddressError::CoinsConfCheckError(_)
            | GetMyAddressError::CoinIsNotSupported(_)
            | GetMyAddressError::InvalidRequest(_) => StatusCode::BAD_REQUEST,
            GetMyAddressError::Internal(_) | GetMyAddressError::GetEthAddressError(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            },
        }
    }
}

impl From<CryptoCtxError> for GetMyAddressError {
    fn from(e: CryptoCtxError) -> Self { GetMyAddressError::Internal(e.to_string()) }
}

// ── Handler ─────────────────────────────────────────────────────────────

/// Resolve the operator's own wallet address for a configured coin without
/// requiring a prior enable/activation call (R-K11).
pub async fn get_my_address_rpc(
    ctx: MmArc,
    req: GetMyAddressRequest,
) -> Result<GetMyAddressResponse, MmError<GetMyAddressError>> {
    let conf = coin_conf(&ctx, &req.coin);
    if conf.is_null() {
        return MmError::err(GetMyAddressError::CoinsConfCheckError(format!(
            "Coin {} is not present in the coins configuration",
            req.coin
        )));
    }

    let protocol: CoinProtocol = CoinProtocol::from_conf_json(conf["protocol"].clone()).map_to_mm(|e| {
        GetMyAddressError::CoinsConfCheckError(format!("Failed to parse protocol of {}: {}", req.coin, e))
    })?;

    let wallet_address = match protocol {
        CoinProtocol::ETH { .. } | CoinProtocol::ERC20 { .. } => my_eth_address(&ctx, &req, &conf)?,
        _ => return MmError::err(GetMyAddressError::CoinIsNotSupported(req.coin.clone())),
    };

    Ok(GetMyAddressResponse {
        coin: req.coin,
        wallet_address,
    })
}

/// Resolve the EVM address from the running signing identity. An HD wallet
/// honours the `path_to_address` coordinate; a single-address (Iguana) wallet
/// returns its sole address.
fn my_eth_address(
    ctx: &MmArc,
    req: &GetMyAddressRequest,
    conf: &serde_json::Value,
) -> Result<String, MmError<GetMyAddressError>> {
    let crypto_ctx = CryptoCtx::from_ctx(ctx).mm_err(GetMyAddressError::from)?;
    let secret = match crypto_ctx.key_pair_policy() {
        KeyPairPolicy::Iguana => crypto_ctx.mm2_internal_privkey_secret(),
        KeyPairPolicy::GlobalHDAccount(hd) => {
            let account = req.path_to_address.as_ref().map(|p| p.account_id).unwrap_or(0);
            let chain = req
                .path_to_address
                .as_ref()
                .map(|p| p.chain)
                .unwrap_or(Bip44Chain::External);
            let address_id = req.path_to_address.as_ref().map(|p| p.address_id).unwrap_or(0);
            derive_eth_hd_secret(&req.coin, conf, hd, account, chain, address_id)?
        },
    };
    eth_address_from_secret(&secret)
}

/// Derive the secp256k1 secret at `m/44'/coin'/account'/chain/address_id` for an
/// HD wallet, using the coin's configured base derivation path.
fn derive_eth_hd_secret(
    ticker: &str,
    conf: &serde_json::Value,
    hd: &GlobalHDAccountArc,
    account: u32,
    chain: Bip44Chain,
    address_id: u32,
) -> Result<Secp256k1Secret, MmError<GetMyAddressError>> {
    if conf["derivation_path"].is_null() {
        return MmError::err(GetMyAddressError::CoinsConfCheckError(format!(
            "{}: no `derivation_path` configured",
            ticker
        )));
    }
    let base: HDPathToCoin = serde_json::from_value(conf["derivation_path"].clone()).map_to_mm(|e| {
        GetMyAddressError::CoinsConfCheckError(format!("invalid `derivation_path` for {}: {}", ticker, e))
    })?;

    let mut path = base.to_derivation_path();
    path.push(ChildNumber::new(account, true).map_to_mm(|e| GetMyAddressError::Internal(e.to_string()))?);
    path.push(chain.to_child_number());
    path.push(ChildNumber::new(address_id, false).map_to_mm(|e| GetMyAddressError::Internal(e.to_string()))?);

    hd.derive_secp256k1_secret(&path)
        .mm_err(|e| GetMyAddressError::Internal(e.to_string()))
}

/// Transform a secp256k1 secret into its EIP-55 checksummed EVM address.
fn eth_address_from_secret(secret: &Secp256k1Secret) -> Result<String, MmError<GetMyAddressError>> {
    let secp = secp256k1::Secp256k1::new();
    let secret_key = secp256k1::SecretKey::from_slice(secret.as_slice())
        .map_to_mm(|e| GetMyAddressError::GetEthAddressError(e.to_string()))?;
    let public_key = secp256k1::PublicKey::from_secret_key(&secp, &secret_key);
    let pubkey_compressed = public_key.serialize();

    let eth_address = addr_from_raw_pubkey(&pubkey_compressed).map_to_mm(GetMyAddressError::GetEthAddressError)?;

    Ok(checksum_address(&format!("{:#x}", eth_address)))
}
