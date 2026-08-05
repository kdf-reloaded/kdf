//! # `get_private_keys` RPC — export private keys for activated coins.
//!
//! Returns the private key, public key, and address for each requested coin.
//! Coins must be activated before calling this method.
//!
//! ## Security
//!
//! - Private keys are sensitive material; call only over localhost / trusted channels.
//! - Keys are serialized once for the response and not persisted or logged.

use common::HttpStatusCode;
use crypto::{Bip32DerPathOps, ChildNumber, CryptoCtx, CryptoCtxError, DerivationPath, GlobalHDAccountArc,
             HDPathToCoin, KeyPairPolicy, Secp256k1Secret};
use derive_more::Display;
use http::StatusCode;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use ser_error_derive::SerializeErrorType;
use serde::{Deserialize, Serialize};

use crate::eth::{addr_from_raw_pubkey, checksum_address};
use crate::tendermint::account_id_from_pubkey_hex;
use crate::utxo::utxo_builder::UtxoConfBuilder;
use crate::utxo::{Address as UtxoAddress, AddressHashEnum, KeyPair as UtxoKeyPair, Private as UtxoPrivate,
                  UtxoActivationParams};
use crate::{coin_conf, lp_coinfind, CoinProtocol, MarketCoinOps};

// ZHTLC shielded-key export (R-K4) relies on the native-only `z_coin` / librustzcash
// stack, so these imports and the matching derivation path are gated off WASM.
#[cfg(not(target_arch = "wasm32"))]
use sapling::zip32::{ExtendedFullViewingKey, ExtendedSpendingKey};
#[cfg(not(target_arch = "wasm32"))]
use zcash_keys::encoding::{encode_extended_full_viewing_key, encode_extended_spending_key, encode_payment_address};
#[cfg(not(target_arch = "wasm32"))]
use zcash_protocol::constants::mainnet as z_mainnet_constants;

/// Maximum number of HD addresses derivable in a single `get_private_keys`
/// call (R-K4). A request whose `[start_index, end_index]` range exceeds this
/// bound is refused with a typed error.
const MAX_HD_ADDRESSES_PER_CALL: u32 = 100;

// ── Request / Response types ────────────────────────────────────────────

/// Key-export mode discriminator (R-K4). Absent / `iguana` selects the reduced,
/// always-available activated-coins export; `hd` selects the opt-in HD superset.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum GetPrivateKeysMode {
    #[default]
    Iguana,
    Hd,
}

#[derive(Deserialize)]
pub struct GetPrivateKeysRequest {
    /// Tickers of coins whose keys should be exported. In the reduced (default)
    /// form these MUST be activated; the opt-in superset relaxes that to any
    /// coin defined in the node configuration.
    pub coins: Vec<String>,
    /// Export mode (R-K4). Defaults to `iguana` (reduced form) when absent.
    #[serde(default)]
    pub mode: GetPrivateKeysMode,
    /// Opt-in: export coins that are merely configured, not activated.
    #[serde(default)]
    pub offline: bool,
    /// Opt-in (`hd` mode only): first BIP-44 address index of the range.
    #[serde(default)]
    pub start_index: Option<u32>,
    /// Opt-in (`hd` mode only): last BIP-44 address index of the range (inclusive).
    #[serde(default)]
    pub end_index: Option<u32>,
    /// Opt-in (`hd` mode only): BIP-44 account index.
    #[serde(default)]
    pub account_index: Option<u32>,
    /// Opt-in: include the shielded `viewing_key` for ZHTLC coins.
    #[serde(default)]
    pub include_shielded: bool,
}

impl GetPrivateKeysRequest {
    /// True when the request asks for any capability beyond the reduced,
    /// always-available activated-coins export — i.e. any part of the opt-in
    /// superset of R-K4 (HD mode, offline/no-activation export, HD index range
    /// parameters, or shielded viewing-key export).
    pub fn requests_superset(&self) -> bool {
        self.mode == GetPrivateKeysMode::Hd
            || self.offline
            || self.start_index.is_some()
            || self.end_index.is_some()
            || self.account_index.is_some()
            || self.include_shielded
    }
}

/// Per-coin key information returned to the caller.
#[derive(Serialize)]
pub struct CoinKeyInfo {
    pub coin: String,
    pub address: String,
    pub priv_key: String,
    /// Hex-encoded compressed public key.
    pub pubkey: String,
}

/// A single per-coin entry of the opt-in `iguana`-mode superset response (R-K4).
#[derive(Serialize)]
pub struct IguanaKeyEntry {
    pub coin: String,
    pub pubkey: String,
    pub address: String,
    pub priv_key: String,
    /// Shielded full viewing key (ZHTLC only); omitted otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewing_key: Option<String>,
}

/// A single derived address of the opt-in `hd`-mode superset response (R-K4).
#[derive(Serialize)]
pub struct HdAddressEntry {
    /// The full BIP-44-style derivation path of this address.
    pub derivation_path: String,
    /// The shielded derivation path (ZHTLC only); omitted otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub z_derivation_path: Option<String>,
    pub pubkey: String,
    pub address: String,
    pub priv_key: String,
    /// Shielded full viewing key (ZHTLC only); omitted otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewing_key: Option<String>,
}

/// A per-coin entry of the opt-in `hd`-mode superset response (R-K4).
#[derive(Serialize)]
pub struct HdKeyEntry {
    pub coin: String,
    pub addresses: Vec<HdAddressEntry>,
}

/// `get_private_keys` response (R-K4 interop). The reduced, always-available
/// form returns the `{keys: [...]}` object; the opt-in superset returns an
/// untagged array of per-coin objects whose shape depends on the export mode.
#[derive(Serialize)]
#[serde(untagged)]
pub enum GetPrivateKeysResponse {
    /// Reduced activated-coins form (R-K3).
    Reduced { keys: Vec<CoinKeyInfo> },
    /// Opt-in `iguana`-mode superset (R-K4).
    Iguana(Vec<IguanaKeyEntry>),
    /// Opt-in `hd`-mode superset (R-K4).
    Hd(Vec<HdKeyEntry>),
}

/// Internal carrier for a single derived key triple plus an optional shielded
/// viewing key, shared by the per-protocol derivation helpers.
struct DerivedKey {
    pubkey: String,
    address: String,
    priv_key: String,
    viewing_key: Option<String>,
}

// ── Error type ──────────────────────────────────────────────────────────

#[derive(Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum GetPrivateKeysError {
    #[display(fmt = "Coin not activated: {}", _0)]
    CoinNotActive(String),
    #[display(fmt = "Coin configuration not found: {}", _0)]
    CoinConfigNotFound(String),
    #[display(fmt = "Could not parse the protocol of {}: {}", ticker, reason)]
    CoinProtocolParseError { ticker: String, reason: String },
    #[display(fmt = "Key export failed for {}: {}", ticker, reason)]
    KeyExportFailed { ticker: String, reason: String },
    #[display(fmt = "Key derivation failed for {}: {}", ticker, reason)]
    KeyDerivationFailed { ticker: String, reason: String },
    #[display(fmt = "HD index range is inverted: start_index {} > end_index {}", start, end)]
    HdRangeInverted { start: u32, end: u32 },
    #[display(fmt = "HD index range of {} addresses exceeds the maximum of {}", requested, max)]
    HdRangeExceedsMax { requested: u32, max: u32 },
    #[display(fmt = "Required protocol prefix is missing for {}", _0)]
    MissingProtocolPrefix(String),
    #[display(fmt = "Index parameters (start_index/end_index/account_index) are valid only in `hd` mode")]
    IndexParamsOutsideHdMode,
    #[display(fmt = "Internal error: {}", _0)]
    Internal(String),
    #[display(fmt = "Hardware wallets do not expose private keys")]
    HardwareWalletNotSupported,
    #[display(
        fmt = "Insecure key export is disabled; set `allow_insecure_key_export=true` in MM2.json to enable the offline/HD/ZHTLC export superset"
    )]
    InsecureExportDisabled,
}

impl HttpStatusCode for GetPrivateKeysError {
    fn status_code(&self) -> StatusCode {
        match self {
            // Client-input faults → 400.
            GetPrivateKeysError::CoinNotActive(_)
            | GetPrivateKeysError::CoinConfigNotFound(_)
            | GetPrivateKeysError::HdRangeInverted { .. }
            | GetPrivateKeysError::HdRangeExceedsMax { .. }
            | GetPrivateKeysError::IndexParamsOutsideHdMode
            | GetPrivateKeysError::HardwareWalletNotSupported => StatusCode::BAD_REQUEST,
            // Derivation / data / internal faults → 500.
            GetPrivateKeysError::CoinProtocolParseError { .. }
            | GetPrivateKeysError::KeyExportFailed { .. }
            | GetPrivateKeysError::KeyDerivationFailed { .. }
            | GetPrivateKeysError::MissingProtocolPrefix(_)
            | GetPrivateKeysError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            GetPrivateKeysError::InsecureExportDisabled => StatusCode::FORBIDDEN,
        }
    }
}

impl From<CryptoCtxError> for GetPrivateKeysError {
    fn from(e: CryptoCtxError) -> Self { GetPrivateKeysError::Internal(e.to_string()) }
}

// ── Opt-in switch ───────────────────────────────────────────────────────

/// Read the `allow_insecure_key_export` switch (R-K1) from the node config.
///
/// Default-false: only a configuration value that is the boolean `true`
/// enables the opt-in superset. This mirrors the `allow_weak_password`
/// convention but is a **separate** switch governing a different threat model.
pub fn allow_insecure_key_export(ctx: &MmArc) -> bool { ctx.conf["allow_insecure_key_export"].as_bool() == Some(true) }

// ── Handler ─────────────────────────────────────────────────────────────

/// Export private keys for the requested activated coins.
///
/// For Iguana wallets, each coin returns a single private key (the passphrase-derived key
/// in the coin's native format — WIF for UTXO, hex for EVM, etc.).
///
/// Hardware wallets are rejected — they never expose private keys to the host.
pub async fn get_private_keys(
    ctx: MmArc,
    req: GetPrivateKeysRequest,
) -> Result<GetPrivateKeysResponse, MmError<GetPrivateKeysError>> {
    // Secure-by-default gate (R-K1/R-K4): any request reaching for the
    // offline/no-activation, HD-range, or shielded-viewing-key superset is
    // refused unless the operator has opted in via `allow_insecure_key_export`.
    // The reduced activated-coins path below is unaffected when no superset
    // field is set, regardless of the switch (R-K3).
    if req.requests_superset() && !allow_insecure_key_export(&ctx) {
        return MmError::err(GetPrivateKeysError::InsecureExportDisabled);
    }

    // Refuse if this is a Trezor/HW session — hardware wallets never expose keys to the host.
    let crypto_ctx = CryptoCtx::from_ctx(&ctx).mm_err(GetPrivateKeysError::from)?;
    if crypto_ctx.hw_ctx().is_some() {
        return MmError::err(GetPrivateKeysError::HardwareWalletNotSupported);
    }

    // Opt-in superset (R-K4): offline export of merely-configured coins, HD
    // per-derivation-path ranges, and protocol-specific formatting, returned
    // via the untagged-union response shape.
    if req.requests_superset() {
        return export_superset(&ctx, &crypto_ctx, &req);
    }

    let mut keys = Vec::with_capacity(req.coins.len());

    for ticker in &req.coins {
        let coin = match lp_coinfind(&ctx, ticker).await {
            Ok(Some(c)) => c,
            Ok(None) => return MmError::err(GetPrivateKeysError::CoinNotActive(ticker.clone())),
            Err(e) => {
                return MmError::err(GetPrivateKeysError::Internal(format!(
                    "Error looking up {}: {}",
                    ticker, e
                )))
            },
        };

        // Derive the private key string in the coin's native format.
        let priv_key = coin
            .display_priv_key()
            .map_err(|e| GetPrivateKeysError::KeyExportFailed {
                ticker: ticker.clone(),
                reason: e,
            })
            .map_to_mm(|e| e)?;

        // Derive the address and public key.
        let address = coin
            .my_address()
            .map_err(|e| GetPrivateKeysError::KeyExportFailed {
                ticker: ticker.clone(),
                reason: e,
            })
            .map_to_mm(|e| e)?;

        let pubkey = coin
            .get_public_key()
            .map_err(|e| GetPrivateKeysError::KeyExportFailed {
                ticker: ticker.clone(),
                reason: e.to_string(),
            })
            .map_to_mm(|e| e)?;

        keys.push(CoinKeyInfo {
            coin: ticker.clone(),
            address,
            priv_key,
            pubkey,
        });
    }

    Ok(GetPrivateKeysResponse::Reduced { keys })
}

// ── Opt-in superset (R-K4) ──────────────────────────────────────────────

/// Export private keys for the opt-in superset (R-K4): any configured coin
/// (regardless of activation), per-protocol formatting, and — in `hd` mode —
/// a bounded range of BIP-44-style derivation paths.
fn export_superset(
    ctx: &MmArc,
    crypto_ctx: &CryptoCtx,
    req: &GetPrivateKeysRequest,
) -> Result<GetPrivateKeysResponse, MmError<GetPrivateKeysError>> {
    match req.mode {
        GetPrivateKeysMode::Iguana => {
            // Index parameters are valid only in `hd` mode (R-K5).
            if req.start_index.is_some() || req.end_index.is_some() || req.account_index.is_some() {
                return MmError::err(GetPrivateKeysError::IndexParamsOutsideHdMode);
            }

            // The single Iguana key is the wallet's internal secp256k1 secret.
            let secret = crypto_ctx.mm2_internal_privkey_secret();
            let mut entries = Vec::with_capacity(req.coins.len());
            for ticker in &req.coins {
                let conf = load_coin_conf(ctx, ticker)?;
                let protocol = parse_protocol(ticker, &conf)?;
                let derived = derive_for_protocol(ticker, &conf, &protocol, &secret)?;
                entries.push(IguanaKeyEntry {
                    coin: ticker.clone(),
                    pubkey: derived.pubkey,
                    address: derived.address,
                    priv_key: derived.priv_key,
                    viewing_key: derived.viewing_key,
                });
            }
            Ok(GetPrivateKeysResponse::Iguana(entries))
        },
        GetPrivateKeysMode::Hd => {
            // HD derivation requires a seed-backed (GlobalHD) wallet.
            let global_hd = match crypto_ctx.key_pair_policy() {
                KeyPairPolicy::GlobalHDAccount(hd) => hd,
                KeyPairPolicy::Iguana => {
                    return MmError::err(GetPrivateKeysError::KeyDerivationFailed {
                        ticker: req.coins.first().cloned().unwrap_or_default(),
                        reason: "`hd` mode requires an HD (seed-derived) wallet".to_owned(),
                    })
                },
            };

            // Validate and normalise the requested index range (R-K4 bounds).
            let account = req.account_index.unwrap_or(0);
            let start = req.start_index.unwrap_or(0);
            let end = req.end_index.unwrap_or(start);
            if end < start {
                return MmError::err(GetPrivateKeysError::HdRangeInverted { start, end });
            }
            let requested = end - start + 1;
            if requested > MAX_HD_ADDRESSES_PER_CALL {
                return MmError::err(GetPrivateKeysError::HdRangeExceedsMax {
                    requested,
                    max: MAX_HD_ADDRESSES_PER_CALL,
                });
            }

            let mut entries = Vec::with_capacity(req.coins.len());
            for ticker in &req.coins {
                let conf = load_coin_conf(ctx, ticker)?;
                let protocol = parse_protocol(ticker, &conf)?;
                let base = parse_base_derivation_path(ticker, &conf)?;

                let mut addresses = Vec::with_capacity(requested as usize);
                for index in start..=end {
                    let (path, secret) = derive_hd_secret(ticker, global_hd, &base, account, index)?;
                    let derived = derive_for_protocol(ticker, &conf, &protocol, &secret)?;
                    addresses.push(HdAddressEntry {
                        derivation_path: path.to_string(),
                        z_derivation_path: None,
                        pubkey: derived.pubkey,
                        address: derived.address,
                        priv_key: derived.priv_key,
                        viewing_key: derived.viewing_key,
                    });
                }
                entries.push(HdKeyEntry {
                    coin: ticker.clone(),
                    addresses,
                });
            }
            Ok(GetPrivateKeysResponse::Hd(entries))
        },
    }
}

/// Read a coin's configuration (offline; no activation required). Returns a
/// typed error when the ticker is absent from the node configuration.
fn load_coin_conf(ctx: &MmArc, ticker: &str) -> Result<serde_json::Value, MmError<GetPrivateKeysError>> {
    let conf = coin_conf(ctx, ticker);
    if conf.is_null() {
        return MmError::err(GetPrivateKeysError::CoinConfigNotFound(ticker.to_owned()));
    }
    Ok(conf)
}

/// Parse a coin's protocol descriptor from its configuration.
fn parse_protocol(ticker: &str, conf: &serde_json::Value) -> Result<CoinProtocol, MmError<GetPrivateKeysError>> {
    CoinProtocol::from_conf_json(conf["protocol"].clone()).map_to_mm(|e| GetPrivateKeysError::CoinProtocolParseError {
        ticker: ticker.to_owned(),
        reason: e.to_string(),
    })
}

/// Parse the base (coin-level) BIP-44 derivation path from a coin's config.
fn parse_base_derivation_path(
    ticker: &str,
    conf: &serde_json::Value,
) -> Result<HDPathToCoin, MmError<GetPrivateKeysError>> {
    if conf["derivation_path"].is_null() {
        return MmError::err(GetPrivateKeysError::MissingProtocolPrefix(format!(
            "{}: no `derivation_path` configured",
            ticker
        )));
    }
    serde_json::from_value(conf["derivation_path"].clone()).map_to_mm(|e| GetPrivateKeysError::KeyDerivationFailed {
        ticker: ticker.to_owned(),
        reason: format!("invalid `derivation_path`: {}", e),
    })
}

/// Derive the secp256k1 secret at `m/44'/coin'/account'/0/index` for an HD wallet.
fn derive_hd_secret(
    ticker: &str,
    global_hd: &GlobalHDAccountArc,
    base: &HDPathToCoin,
    account: u32,
    index: u32,
) -> Result<(DerivationPath, Secp256k1Secret), MmError<GetPrivateKeysError>> {
    let mut path = base.to_derivation_path();
    let child = |value: u32, hardened: bool| {
        ChildNumber::new(value, hardened).map_to_mm(|e| GetPrivateKeysError::KeyDerivationFailed {
            ticker: ticker.to_owned(),
            reason: e.to_string(),
        })
    };
    path.push(child(account, true)?);
    path.push(child(0, false)?);
    path.push(child(index, false)?);

    let secret = global_hd
        .derive_secp256k1_secret(&path)
        .mm_err(|e| GetPrivateKeysError::KeyDerivationFailed {
            ticker: ticker.to_owned(),
            reason: e.to_string(),
        })?;
    Ok((path, secret))
}

/// Format a derived secret into the coin's native key/address representation.
fn derive_for_protocol(
    ticker: &str,
    conf: &serde_json::Value,
    protocol: &CoinProtocol,
    secret: &Secp256k1Secret,
) -> Result<DerivedKey, MmError<GetPrivateKeysError>> {
    match protocol {
        CoinProtocol::UTXO | CoinProtocol::QTUM | CoinProtocol::BCH { .. } => derive_utxo(ticker, conf, secret),
        CoinProtocol::ETH { .. } | CoinProtocol::ERC20 { .. } => derive_evm(ticker, secret),
        CoinProtocol::TENDERMINT { account_prefix, .. } => derive_tendermint(ticker, account_prefix, secret),
        #[cfg(not(target_arch = "wasm32"))]
        CoinProtocol::ZHTLC(_) => derive_zhtlc(ticker, secret),
        other => MmError::err(GetPrivateKeysError::KeyDerivationFailed {
            ticker: ticker.to_owned(),
            reason: format!("key export is not supported for protocol {:?}", other),
        }),
    }
}

/// UTXO formatting: WIF private key + legacy/segwit address (R-K4).
fn derive_utxo(
    ticker: &str,
    conf: &serde_json::Value,
    secret: &Secp256k1Secret,
) -> Result<DerivedKey, MmError<GetPrivateKeysError>> {
    // Build minimal activation params for offline conf parsing (no RPC mode is
    // exercised; only the configured prefixes / address format are read).
    let params = UtxoActivationParams::from_legacy_req(&serde_json::json!({ "method": "enable" })).mm_err(|e| {
        GetPrivateKeysError::KeyDerivationFailed {
            ticker: ticker.to_owned(),
            reason: e.to_string(),
        }
    })?;
    let utxo_conf =
        UtxoConfBuilder::new(conf, &params, ticker)
            .build()
            .mm_err(|e| GetPrivateKeysError::KeyDerivationFailed {
                ticker: ticker.to_owned(),
                reason: e.to_string(),
            })?;

    let private = UtxoPrivate {
        prefix: utxo_conf.wif_prefix,
        secret: *secret,
        compressed: true,
        checksum_type: utxo_conf.checksum_type,
    };
    let wif = private.to_string();
    let key_pair = UtxoKeyPair::from_private(private).map_to_mm(|e| GetPrivateKeysError::KeyDerivationFailed {
        ticker: ticker.to_owned(),
        reason: e.to_string(),
    })?;

    let address = UtxoAddress {
        prefix: utxo_conf.pub_addr_prefix,
        t_addr_prefix: utxo_conf.pub_t_addr_prefix,
        hash: AddressHashEnum::AddressHash(key_pair.public().address_hash()),
        checksum_type: utxo_conf.checksum_type,
        hrp: utxo_conf.bech32_hrp.clone(),
        addr_format: utxo_conf.default_address_format.clone(),
    };
    let address = address
        .display_address()
        .map_to_mm(|reason| GetPrivateKeysError::KeyDerivationFailed {
            ticker: ticker.to_owned(),
            reason,
        })?;

    Ok(DerivedKey {
        pubkey: hex::encode(key_pair.public().to_vec()),
        address,
        priv_key: wif,
        viewing_key: None,
    })
}

/// EVM formatting: `0x`-prefixed hex secret + EIP-55 checksummed address (R-K4).
fn derive_evm(ticker: &str, secret: &Secp256k1Secret) -> Result<DerivedKey, MmError<GetPrivateKeysError>> {
    let secp = secp256k1::Secp256k1::new();
    let secret_key = secp256k1::SecretKey::from_slice(secret.as_slice()).map_to_mm(|e| {
        GetPrivateKeysError::KeyDerivationFailed {
            ticker: ticker.to_owned(),
            reason: e.to_string(),
        }
    })?;
    let public_key = secp256k1::PublicKey::from_secret_key(&secp, &secret_key);
    let pubkey_compressed = public_key.serialize();

    let eth_address =
        addr_from_raw_pubkey(&pubkey_compressed).map_to_mm(|reason| GetPrivateKeysError::KeyDerivationFailed {
            ticker: ticker.to_owned(),
            reason,
        })?;

    Ok(DerivedKey {
        pubkey: hex::encode(pubkey_compressed),
        address: checksum_address(&format!("{:#x}", eth_address)),
        priv_key: format!("0x{}", hex::encode(secret.as_slice())),
        viewing_key: None,
    })
}

/// Tendermint formatting: hex secret + bech32 account address (R-K4).
fn derive_tendermint(
    ticker: &str,
    account_prefix: &str,
    secret: &Secp256k1Secret,
) -> Result<DerivedKey, MmError<GetPrivateKeysError>> {
    let signing_key = cosmrs::crypto::secp256k1::SigningKey::from_slice(secret.as_slice()).map_to_mm(|e| {
        GetPrivateKeysError::KeyDerivationFailed {
            ticker: ticker.to_owned(),
            reason: e.to_string(),
        }
    })?;
    let pubkey_bytes = signing_key.public_key().to_bytes();
    let pubkey_hex = hex::encode(&pubkey_bytes);

    let account_id = account_id_from_pubkey_hex(account_prefix, &pubkey_hex).map_to_mm(|e| {
        GetPrivateKeysError::KeyDerivationFailed {
            ticker: ticker.to_owned(),
            reason: e.to_string(),
        }
    })?;

    Ok(DerivedKey {
        pubkey: pubkey_hex,
        address: account_id.to_string(),
        priv_key: hex::encode(secret.as_slice()),
        viewing_key: None,
    })
}

/// ZHTLC shielded formatting (R-K4): the Sapling spending key is master-derived
/// (ZIP-32 master node) from the same secp256k1 secret that backs the
/// transparent key for this entry, on the fixed mainnet Sapling parameters.
/// `priv_key` carries the encoded extended spending key, `viewing_key` the
/// encoded extended full viewing key, and `address` the default shielded
/// payment address. No shielded derivation path is produced (master node), so
/// `z_derivation_path` is left unpopulated by the caller.
#[cfg(not(target_arch = "wasm32"))]
fn derive_zhtlc(ticker: &str, secret: &Secp256k1Secret) -> Result<DerivedKey, MmError<GetPrivateKeysError>> {
    // Transparent compressed secp256k1 public key of the backing secret. This is
    // the same key that master-derives the shielded material and keeps the
    // `pubkey` field meaningful and consistent with the other protocols.
    let secp = secp256k1::Secp256k1::new();
    let secret_key = secp256k1::SecretKey::from_slice(secret.as_slice()).map_to_mm(|e| {
        GetPrivateKeysError::KeyDerivationFailed {
            ticker: ticker.to_owned(),
            reason: e.to_string(),
        }
    })?;
    let pubkey_compressed = secp256k1::PublicKey::from_secret_key(&secp, &secret_key).serialize();

    let z_spending_key = ExtendedSpendingKey::master(secret.as_slice());
    let priv_key =
        encode_extended_spending_key(z_mainnet_constants::HRP_SAPLING_EXTENDED_SPENDING_KEY, &z_spending_key);
    #[allow(deprecated)]
    let efvk = z_spending_key.to_extended_full_viewing_key();
    let viewing_key =
        encode_extended_full_viewing_key(z_mainnet_constants::HRP_SAPLING_EXTENDED_FULL_VIEWING_KEY, &efvk);
    let (_, payment_address) = z_spending_key.default_address();
    let address = encode_payment_address(z_mainnet_constants::HRP_SAPLING_PAYMENT_ADDRESS, &payment_address);

    Ok(DerivedKey {
        pubkey: hex::encode(pubkey_compressed),
        address,
        priv_key,
        viewing_key: Some(viewing_key),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_status_codes() {
        assert_eq!(
            GetPrivateKeysError::CoinNotActive("X".into()).status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            GetPrivateKeysError::HardwareWalletNotSupported.status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            GetPrivateKeysError::Internal("x".into()).status_code(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            GetPrivateKeysError::InsecureExportDisabled.status_code(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            GetPrivateKeysError::IndexParamsOutsideHdMode.status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            GetPrivateKeysError::HdRangeInverted { start: 5, end: 1 }.status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            GetPrivateKeysError::HdRangeExceedsMax {
                requested: 200,
                max: MAX_HD_ADDRESSES_PER_CALL
            }
            .status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            GetPrivateKeysError::CoinConfigNotFound("X".into()).status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            GetPrivateKeysError::KeyDerivationFailed {
                ticker: "X".into(),
                reason: "x".into()
            }
            .status_code(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    fn reduced_req(coins: Vec<String>) -> GetPrivateKeysRequest {
        GetPrivateKeysRequest {
            coins,
            mode: GetPrivateKeysMode::Iguana,
            offline: false,
            start_index: None,
            end_index: None,
            account_index: None,
            include_shielded: false,
        }
    }

    #[test]
    fn test_requests_superset() {
        // Reduced (always-available) form requests no superset capability.
        assert!(!reduced_req(vec!["RICK".into()]).requests_superset());

        // Each opt-in capability independently flags the request as superset.
        let hd = GetPrivateKeysRequest {
            mode: GetPrivateKeysMode::Hd,
            ..reduced_req(vec![])
        };
        assert!(hd.requests_superset());
        assert!(GetPrivateKeysRequest {
            offline: true,
            ..reduced_req(vec![])
        }
        .requests_superset());
        assert!(GetPrivateKeysRequest {
            start_index: Some(0),
            ..reduced_req(vec![])
        }
        .requests_superset());
        assert!(GetPrivateKeysRequest {
            end_index: Some(5),
            ..reduced_req(vec![])
        }
        .requests_superset());
        assert!(GetPrivateKeysRequest {
            account_index: Some(1),
            ..reduced_req(vec![])
        }
        .requests_superset());
        assert!(GetPrivateKeysRequest {
            include_shielded: true,
            ..reduced_req(vec![])
        }
        .requests_superset());
    }

    #[test]
    fn test_allow_insecure_key_export_switch() {
        use mm2_core::mm_ctx::MmCtxBuilder;
        use serde_json::json;

        // Absent → false.
        let ctx = MmCtxBuilder::new().with_conf(json!({})).into_mm_arc();
        assert!(!allow_insecure_key_export(&ctx));

        // Explicit false → false.
        let ctx = MmCtxBuilder::new()
            .with_conf(json!({ "allow_insecure_key_export": false }))
            .into_mm_arc();
        assert!(!allow_insecure_key_export(&ctx));

        // Truthy only for the boolean `true` (mirrors `allow_weak_password`).
        let ctx = MmCtxBuilder::new()
            .with_conf(json!({ "allow_insecure_key_export": "true" }))
            .into_mm_arc();
        assert!(!allow_insecure_key_export(&ctx));

        let ctx = MmCtxBuilder::new()
            .with_conf(json!({ "allow_insecure_key_export": true }))
            .into_mm_arc();
        assert!(allow_insecure_key_export(&ctx));
    }

    #[test]
    fn test_superset_gated_when_switch_off() {
        use common::block_on;
        use mm2_core::mm_ctx::MmCtxBuilder;
        use serde_json::json;

        // Switch off + a superset request → refused before any crypto/coin work.
        let ctx = MmCtxBuilder::new().with_conf(json!({})).into_mm_arc();
        let req = GetPrivateKeysRequest {
            offline: true,
            ..reduced_req(vec!["RICK".into()])
        };
        match block_on(get_private_keys(ctx, req)) {
            Ok(_) => panic!("expected InsecureExportDisabled"),
            Err(e) => {
                assert_eq!(e.get_inner().status_code(), StatusCode::FORBIDDEN);
                assert!(matches!(e.into_inner(), GetPrivateKeysError::InsecureExportDisabled));
            },
        }
    }

    // A fixed, valid secp256k1 secret used across the derivation-format tests.
    fn sample_secret() -> Secp256k1Secret { Secp256k1Secret::from([0x11u8; 32]) }

    fn unwrap_derived(r: Result<DerivedKey, MmError<GetPrivateKeysError>>) -> DerivedKey {
        r.unwrap_or_else(|e| panic!("derivation failed: {}", e))
    }

    #[test]
    fn test_derive_evm_format() {
        let d = unwrap_derived(derive_evm("ETH", &sample_secret()));
        // EVM private key is the `0x`-prefixed hex of the 32-byte secret (R-K4).
        assert_eq!(d.priv_key, format!("0x{}", "11".repeat(32)));
        // EIP-55 checksummed 20-byte address.
        assert!(d.address.starts_with("0x"));
        assert_eq!(d.address.len(), 42);
        // Hex-encoded 33-byte compressed secp256k1 public key.
        assert_eq!(d.pubkey.len(), 66);
        assert!(d.viewing_key.is_none());
    }

    #[test]
    fn test_derive_utxo_format_and_pubkey_invariant() {
        let conf = serde_json::json!({
            "coin": "BTC",
            "pubtype": 0,
            "p2shtype": 5,
            "wiftype": 128,
            "derivation_path": "m/44'/0'"
        });
        let utxo = unwrap_derived(derive_utxo("BTC", &conf, &sample_secret()));
        // BTC mainnet P2PKH address (version byte 0) is Base58Check starting '1'.
        assert!(utxo.address.starts_with('1'), "got {}", utxo.address);
        // WIF is non-empty and the compressed flag yields the 'K'/'L' prefix family.
        assert!(!utxo.priv_key.is_empty());
        assert!(utxo.viewing_key.is_none());

        // The compressed secp256k1 public key must be identical regardless of the
        // protocol formatting path (UTXO vs EVM both expose the same key).
        let evm = unwrap_derived(derive_evm("ETH", &sample_secret()));
        assert_eq!(utxo.pubkey, evm.pubkey);
    }

    #[test]
    fn test_derive_tendermint_format() {
        let d = unwrap_derived(derive_tendermint("ATOM", "cosmos", &sample_secret()));
        // Bech32 account address bound by the configured prefix.
        assert!(d.address.starts_with("cosmos1"), "got {}", d.address);
        // Tendermint private key is the hex of the 32-byte secret (R-K4).
        assert_eq!(d.priv_key, "11".repeat(32));
        // Hex-encoded 33-byte compressed secp256k1 public key.
        assert_eq!(d.pubkey.len(), 66);
        assert!(d.viewing_key.is_none());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn test_derive_zhtlc_format_and_pubkey_invariant() {
        let z = unwrap_derived(derive_zhtlc("ARRR", &sample_secret()));
        // Encoded extended spending key, full viewing key and shielded payment
        // address carry their bound mainnet Sapling HRPs (R-K4).
        assert!(
            z.priv_key
                .starts_with(z_mainnet_constants::HRP_SAPLING_EXTENDED_SPENDING_KEY),
            "got {}",
            z.priv_key
        );
        let viewing_key = z.viewing_key.as_ref().expect("ZHTLC carries a viewing_key");
        assert!(
            viewing_key.starts_with(z_mainnet_constants::HRP_SAPLING_EXTENDED_FULL_VIEWING_KEY),
            "got {}",
            viewing_key
        );
        assert!(
            z.address.starts_with(z_mainnet_constants::HRP_SAPLING_PAYMENT_ADDRESS),
            "got {}",
            z.address
        );

        // The `pubkey` is the transparent compressed secp256k1 key of the backing
        // secret, identical to the other protocols' derivation of the same secret.
        let evm = unwrap_derived(derive_evm("ETH", &sample_secret()));
        assert_eq!(z.pubkey, evm.pubkey);
    }
}
