use crate::utxo::header_layout_fallback::ChainVariant;
use crate::utxo::rpc_clients::EstimateFeeMode;
use crate::utxo::{parse_hex_encoded_u32, UtxoCoinConf, DEFAULT_DYNAMIC_FEE_VOLATILITY_PERCENT, KMD_MTP_BLOCK_COUNT,
                  MATURE_CONFIRMATIONS_DEFAULT};
use crate::UtxoActivationParams;
use common::log::warn;
use crypto::trezor::utxo::TrezorUtxoCoin;
use crypto::{Bip32Error, ChildNumber};
use derive_more::Display;
use kdf_crypto::ChecksumType;
pub use keys::{Address, AddressFormat as UtxoAddressFormat, AddressHashEnum, KeyPair, Private, Public, Secret,
               Type as ScriptType};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use script::SignatureVersion;
use serde_json::{self as json, Value as Json};
use serialization::CoinVariant;
use std::num::NonZeroU64;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// The `signature_version` values the `SignatureVersion` enum deserializes; keep in step with it.
const ACCEPTED_SIGNATURE_VERSIONS: &str = "\"base\", \"witness_v0\", \"fork_id\", \"fork_id_rxd\"";

/// The top-level `MM2.json` compatibility switch that selects what a coin-configuration key holding an
/// unrecognised enumeration member does: `false` (the default) fails the activation, the boolean `true` warns
/// once per activation and falls back to the key's default, as the reference does. Only the boolean `true`
/// turns it on. The name is deliberately not specific to one key: any enumeration read from a coin
/// configuration can adopt it. The first consumer is `signature_version`.
pub const COIN_CONF_UNRECOGNISED_ENUM_FALLS_BACK_KEY: &str = "coin_conf_unrecognised_enum_falls_back";

/// What to do with a coin-configuration enumeration key that is present but holds an unrecognised member.
/// An absent key (or JSON `null`) is not affected: it always takes the key's default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnrecognisedEnumPolicy {
    /// Fail the build with an error naming the key and the value. The default.
    Fail,
    /// Take the key's default and warn, as the reference does silently.
    WarnAndFallBack,
    /// Take the key's default without a warning. For callers that read only address-related fields and never
    /// the signing algorithm, so a value they do not use cannot make them fail.
    NotUsed,
}

impl Default for UnrecognisedEnumPolicy {
    fn default() -> Self { UnrecognisedEnumPolicy::Fail }
}

/// The policy the node's `MM2.json` selects through [`COIN_CONF_UNRECOGNISED_ENUM_FALLS_BACK_KEY`].
pub fn unrecognised_enum_policy(ctx: &MmArc) -> UnrecognisedEnumPolicy {
    if ctx.conf[COIN_CONF_UNRECOGNISED_ENUM_FALLS_BACK_KEY].as_bool() == Some(true) {
        UnrecognisedEnumPolicy::WarnAndFallBack
    } else {
        UnrecognisedEnumPolicy::Fail
    }
}

pub type UtxoConfResult<T> = Result<T, MmError<UtxoConfError>>;

#[derive(Debug, Display)]
pub enum UtxoConfError {
    #[display(fmt = "'name' field is not found in config")]
    CurrencyNameIsNotSet,
    #[display(fmt = "'derivation_path' field is not found in config")]
    DerivationPathIsNotSet,
    #[display(fmt = "'trezor_coin' field is not found in config")]
    TrezorCoinIsNotSet,
    #[display(fmt = "Invalid 'derivation_path' purpose {}. BIP44 is supported only", found)]
    InvalidDerivationPathPurpose {
        found: ChildNumber,
    },
    #[display(
        fmt = "Invalid length '{}' of 'derivation_path'. Expected \"m/purpose'/coin_type'/\" path, i.e 2 children",
        found_children
    )]
    InvalidDerivationPathLen {
        found_children: usize,
    },
    #[display(fmt = "Error deserializing 'derivation_path': {}", _0)]
    ErrorDeserializingDerivationPath(String),
    InvalidConsensusBranchId(String),
    InvalidVersionGroupId(String),
    /// The coin's own `fee_estimate_cache_secs` is not an integer from 0 to 600 (CRD ch.38 R38.6.6k item 1).
    InvalidFeeEstimateCacheSecs(String),
    #[display(
        fmt = "Invalid 'fork_id' {}: expected a hexadecimal string such as \"0x40\" that fits 32 bits, or omit the key to use the coin's default",
        found
    )]
    InvalidForkId {
        found: String,
    },
    #[display(
        fmt = "Invalid 'signature_version' {}: expected one of {}, or omit the key to use the coin's default \
               (or set '{}' to true in MM2.json to warn and fall back instead of failing)",
        found,
        ACCEPTED_SIGNATURE_VERSIONS,
        COIN_CONF_UNRECOGNISED_ENUM_FALLS_BACK_KEY
    )]
    InvalidSignatureVersion {
        found: String,
    },
    InvalidAddressFormat(String),
    InvalidSpvConf(String),
    InvalidDecimals(String),
    #[display(
        fmt = "{}: unrecognized 'protocol.header_layout' value {}; expected \"riecoin\" or \"kawpow_auxpow_mixed\"",
        ticker,
        value
    )]
    InvalidHeaderLayout {
        ticker: String,
        value: String,
    },
    #[display(
        fmt = "'{}' in MM2.json must be an array of distinct chain_variant names ({}), got {}",
        HEADER_LAYOUT_FALLBACK_FAMILIES_KEY,
        "ChainVariant::names()",
        _0
    )]
    InvalidHeaderLayoutFallbackFamilies(String),
    #[display(
        fmt = "{}: unrecognized 'protocol.chain_variant' value {}; expected one of {}",
        ticker,
        value,
        "ChainVariant::names()"
    )]
    InvalidChainVariant {
        ticker: String,
        value: String,
    },
}

/// The `MM2.json` key of the node switch that names the coin families whose header reads may fall back
/// to a structural layout probe (CRD ch.37 R37.5.9e).
pub const HEADER_LAYOUT_FALLBACK_FAMILIES_KEY: &str = "header_layout_fallback_families";

/// The families the fallback applies to when `MM2.json` does not carry the key.
const HEADER_LAYOUT_FALLBACK_FAMILIES_DEFAULT: &[&str] = &["BTC"];

/// The `chain_variant` spellings (CRD ch.37 R37.5.1) a family in the switch may take.
/// Reads the node switch `header_layout_fallback_families` (CRD ch.37 R37.5.9e): an array whose every
/// element is one of the eight `chain_variant` spellings, without duplicates; default `["BTC"]` when
/// the key is absent. Anything else (a non-array, a non-string element, an unknown or lowercase
/// spelling, a duplicate) is a configuration error, so a typo cannot quietly change which coins are
/// probed. `[]` is valid and turns the fallback off.
pub fn header_layout_fallback_families(ctx: &MmArc) -> UtxoConfResult<Vec<String>> {
    let Some(value) = ctx.conf.get(HEADER_LAYOUT_FALLBACK_FAMILIES_KEY) else {
        return Ok(HEADER_LAYOUT_FALLBACK_FAMILIES_DEFAULT
            .iter()
            .map(|f| (*f).to_owned())
            .collect());
    };
    let invalid = || UtxoConfError::InvalidHeaderLayoutFallbackFamilies(value.to_string());
    let Json::Array(items) = value else {
        return MmError::err(invalid());
    };
    let mut families: Vec<String> = Vec::with_capacity(items.len());
    for item in items {
        let family = item
            .as_str()
            .filter(|f| ChainVariant::from_name(f).is_some())
            .or_mm_err(invalid)?;
        if families.iter().any(|f| f == family) {
            return MmError::err(invalid());
        }
        families.push(family.to_owned());
    }
    Ok(families)
}

impl From<Bip32Error> for UtxoConfError {
    fn from(e: Bip32Error) -> Self { UtxoConfError::ErrorDeserializingDerivationPath(e.to_string()) }
}

pub struct UtxoConfBuilder<'a> {
    conf: &'a Json,
    ticker: &'a str,
    params: &'a UtxoActivationParams,
    /// The node switch `header_layout_fallback_families`, resolved by the caller from `ctx.conf`.
    header_layout_fallback_families: Vec<String>,
    unrecognised_enum_policy: UnrecognisedEnumPolicy,
}

impl<'a> UtxoConfBuilder<'a> {
    pub fn new(conf: &'a Json, params: &'a UtxoActivationParams, ticker: &'a str) -> Self {
        UtxoConfBuilder {
            conf,
            ticker,
            params,
            header_layout_fallback_families: HEADER_LAYOUT_FALLBACK_FAMILIES_DEFAULT
                .iter()
                .map(|f| (*f).to_owned())
                .collect(),
            unrecognised_enum_policy: UnrecognisedEnumPolicy::default(),
        }
    }

    /// Sets the node-level `header_layout_fallback_families` policy (see [`header_layout_fallback_families`]).
    pub fn with_header_layout_fallback_families(mut self, families: Vec<String>) -> Self {
        self.header_layout_fallback_families = families;
        self
    }

    /// Selects what an unrecognised enumeration member in the coin configuration does; see
    /// [`UnrecognisedEnumPolicy`]. Without this call the build fails.
    pub fn with_unrecognised_enum_policy(mut self, policy: UnrecognisedEnumPolicy) -> Self {
        self.unrecognised_enum_policy = policy;
        self
    }

    pub fn build(&self) -> UtxoConfResult<UtxoCoinConf> {
        let (conf, warnings) = self.build_with_warnings()?;
        for warning in warnings {
            warn!("{warning}");
        }
        Ok(conf)
    }

    /// Builds the configuration and returns, instead of logging them, the warnings the build produced.
    fn build_with_warnings(&self) -> UtxoConfResult<(UtxoCoinConf, Vec<String>)> {
        let mut warnings = Vec::new();
        crate::request_fee_scope::per_coin_fee_estimate_cache_secs(&[self.conf])
            .map_to_mm(UtxoConfError::InvalidFeeEstimateCacheSecs)?;
        let checksum_type = self.checksum_type();
        let pub_addr_prefix = self.pub_addr_prefix();
        let p2sh_addr_prefix = self.p2sh_address_prefix();
        let pub_t_addr_prefix = self.pub_t_address_prefix();
        let p2sh_t_addr_prefix = self.p2sh_t_address_prefix();
        let sign_message_prefix = self.sign_message_prefix();

        let wif_prefix = self.wif_prefix();

        let bech32_hrp = self.bech32_hrp();

        let default_address_format = self.default_address_format();

        let asset_chain = self.asset_chain();
        let tx_version = self.tx_version();
        let overwintered = self.overwintered();

        let tx_fee_volatility_percent = self.tx_fee_volatility_percent();
        let version_group_id = self.version_group_id(tx_version, overwintered)?;
        let consensus_branch_id = self.consensus_branch_id(tx_version)?;
        let fork_id = self.fork_id()?;
        let signature_version = self.signature_version(fork_id, &mut warnings)?;

        // should be sufficient to detect zcash by overwintered flag
        let zcash = overwintered;

        let required_confirmations = self.required_confirmations();
        let requires_notarization = self.requires_notarization();

        let mature_confirmations = self.mature_confirmations();

        let is_pos = self.is_pos();
        let segwit = self.segwit();
        let force_min_relay_fee = self.conf["force_min_relay_fee"].as_bool().unwrap_or(false);
        let mtp_block_count = self.mtp_block_count();
        let estimate_fee_mode = self.estimate_fee_mode();
        let estimate_fee_blocks = self.estimate_fee_blocks();
        let trezor_coin = self.trezor_coin();
        let enable_spv_proof = self.enable_spv_proof();
        let header_layout = self.header_layout()?;
        let chain_variant = self.chain_variant()?;
        let protocol_header_reader = self.protocol_header_reader();

        let conf = UtxoCoinConf {
            ticker: self.ticker.to_owned(),
            is_pos,
            requires_notarization,
            overwintered,
            pub_addr_prefix,
            p2sh_addr_prefix,
            pub_t_addr_prefix,
            p2sh_t_addr_prefix,
            sign_message_prefix,
            bech32_hrp,
            segwit,
            wif_prefix,
            tx_version,
            default_address_format,
            asset_chain,
            tx_fee_volatility_percent,
            version_group_id,
            consensus_branch_id,
            zcash,
            checksum_type,
            signature_version,
            fork_id,
            required_confirmations: required_confirmations.into(),
            force_min_relay_fee,
            mtp_block_count,
            estimate_fee_mode,
            mature_confirmations,
            estimate_fee_blocks,
            trezor_coin,
            enable_spv_proof,
            header_layout,
            chain_variant,
            protocol_header_reader,
            header_layout_fallback_families: self.header_layout_fallback_families.clone(),
            header_layout_fallback_logged: Arc::new(AtomicBool::new(false)),
        };
        Ok((conf, warnings))
    }

    fn checksum_type(&self) -> ChecksumType {
        match self.ticker {
            "GRS" => ChecksumType::DGROESTL512,
            "SMART" => ChecksumType::KECCAK256,
            _ => ChecksumType::DSHA256,
        }
    }

    fn pub_addr_prefix(&self) -> u8 {
        let pubtype = self.conf["pubtype"]
            .as_u64()
            .unwrap_or(if self.ticker == "BTC" { 0 } else { 60 });
        pubtype as u8
    }

    fn p2sh_address_prefix(&self) -> u8 {
        self.conf["p2shtype"]
            .as_u64()
            .unwrap_or(if self.ticker == "BTC" { 5 } else { 85 }) as u8
    }

    fn pub_t_address_prefix(&self) -> u8 { self.conf["taddr"].as_u64().unwrap_or(0) as u8 }

    fn p2sh_t_address_prefix(&self) -> u8 { self.conf["taddr"].as_u64().unwrap_or(0) as u8 }

    fn sign_message_prefix(&self) -> Option<String> {
        json::from_value(self.conf["sign_message_prefix"].clone()).unwrap_or(None)
    }

    fn wif_prefix(&self) -> u8 {
        let wiftype = self.conf["wiftype"]
            .as_u64()
            .unwrap_or(if self.ticker == "BTC" { 128 } else { 188 });
        wiftype as u8
    }

    fn bech32_hrp(&self) -> Option<String> { json::from_value(self.conf["bech32_hrp"].clone()).unwrap_or(None) }

    fn default_address_format(&self) -> UtxoAddressFormat {
        let mut address_format: UtxoAddressFormat =
            json::from_value(self.conf["address_format"].clone()).unwrap_or(UtxoAddressFormat::Standard);

        if let UtxoAddressFormat::CashAddress {
            network: _,
            ref mut pub_addr_prefix,
            ref mut p2sh_addr_prefix,
        } = address_format
        {
            *pub_addr_prefix = self.pub_addr_prefix();
            *p2sh_addr_prefix = self.p2sh_address_prefix();
        }

        address_format
    }

    fn asset_chain(&self) -> bool { self.conf["asset"].as_str().is_some() }

    fn tx_version(&self) -> i32 { self.conf["txversion"].as_i64().unwrap_or(1) as i32 }

    fn overwintered(&self) -> bool { self.conf["overwintered"].as_u64().unwrap_or(0) == 1 }

    fn tx_fee_volatility_percent(&self) -> f64 {
        match self.conf["txfee_volatility_percent"].as_f64() {
            Some(volatility) => volatility,
            None => DEFAULT_DYNAMIC_FEE_VOLATILITY_PERCENT,
        }
    }

    fn version_group_id(&self, tx_version: i32, overwintered: bool) -> UtxoConfResult<u32> {
        let version_group_id = match self.conf["version_group_id"].as_str() {
            Some(s) => parse_hex_encoded_u32(s).mm_err(UtxoConfError::InvalidVersionGroupId)?,
            None => {
                if tx_version == 3 && overwintered {
                    0x03c4_8270
                } else if tx_version == 4 && overwintered {
                    0x892f_2085
                } else {
                    0
                }
            },
        };
        Ok(version_group_id)
    }

    fn consensus_branch_id(&self, tx_version: i32) -> UtxoConfResult<u32> {
        let consensus_branch_id = match self.conf["consensus_branch_id"].as_str() {
            Some(s) => parse_hex_encoded_u32(s).mm_err(UtxoConfError::InvalidConsensusBranchId)?,
            None => match tx_version {
                3 => 0x5ba8_1b19,
                4 => 0x76b8_09bb,
                _ => 0,
            },
        };
        Ok(consensus_branch_id)
    }

    fn signature_version(&self, fork_id: u32, warnings: &mut Vec<String>) -> UtxoConfResult<SignatureVersion> {
        let default = || {
            if self.ticker == "BCH" || fork_id != 0 {
                SignatureVersion::ForkId
            } else {
                SignatureVersion::Base
            }
        };
        let value = &self.conf["signature_version"];
        // An absent key (or an explicit `null`) takes the default. A present value that is not recognised
        // takes it only if the node's policy says so, and then not silently: a coin that signs under a
        // sighash algorithm nobody asked for is rejected by the network.
        if value.is_null() {
            return Ok(default());
        }
        match json::from_value(value.clone()) {
            Ok(signature_version) => Ok(signature_version),
            Err(_) => match self.unrecognised_enum_policy {
                UnrecognisedEnumPolicy::Fail => MmError::err(UtxoConfError::InvalidSignatureVersion {
                    found: value.to_string(),
                }),
                UnrecognisedEnumPolicy::WarnAndFallBack => {
                    let fallback = default();
                    warnings.push(format!(
                        "{}: unrecognised 'signature_version' {} in the coin configuration (expected one of {}), \
                         falling back to {:?} because '{}' is true",
                        self.ticker,
                        value,
                        ACCEPTED_SIGNATURE_VERSIONS,
                        fallback,
                        COIN_CONF_UNRECOGNISED_ENUM_FALLS_BACK_KEY
                    ));
                    Ok(fallback)
                },
                UnrecognisedEnumPolicy::NotUsed => Ok(default()),
            },
        }
    }

    /// An absent key (or JSON `null`) takes the coin's default. A present value must be a hexadecimal string
    /// that fits 32 bits: this is a malformed value, not an unrecognised enumeration member, so it fails under
    /// every [`UnrecognisedEnumPolicy`].
    fn fork_id(&self) -> UtxoConfResult<u32> {
        let default_fork_id = match self.ticker {
            "BCH" => 0x40,
            _ => 0,
        };
        let value = &self.conf["fork_id"];
        if value.is_null() {
            return Ok(default_fork_id);
        }
        let invalid = || UtxoConfError::InvalidForkId {
            found: value.to_string(),
        };
        let hex_string = value.as_str().or_mm_err(invalid)?;
        u32::from_str_radix(hex_string.trim_start_matches("0x"), 16).map_to_mm(|_| invalid())
    }

    fn required_confirmations(&self) -> u64 {
        // param from request should override the config
        self.params
            .required_confirmations
            .unwrap_or_else(|| self.conf["required_confirmations"].as_u64().unwrap_or(1))
    }

    fn requires_notarization(&self) -> AtomicBool {
        self.params
            .requires_notarization
            .unwrap_or_else(|| self.conf["requires_notarization"].as_bool().unwrap_or(false))
            .into()
    }

    fn mature_confirmations(&self) -> u32 {
        self.conf["mature_confirmations"]
            .as_u64()
            .map(|x| x as u32)
            .unwrap_or(MATURE_CONFIRMATIONS_DEFAULT)
    }

    fn is_pos(&self) -> bool { self.conf["isPoS"].as_u64() == Some(1) }

    fn segwit(&self) -> bool { self.conf["segwit"].as_bool().unwrap_or(false) }

    fn mtp_block_count(&self) -> NonZeroU64 {
        json::from_value(self.conf["mtp_block_count"].clone()).unwrap_or(KMD_MTP_BLOCK_COUNT)
    }

    fn estimate_fee_mode(&self) -> Option<EstimateFeeMode> {
        json::from_value(self.conf["estimate_fee_mode"].clone()).unwrap_or(None)
    }

    fn estimate_fee_blocks(&self) -> u32 { json::from_value(self.conf["estimate_fee_blocks"].clone()).unwrap_or(1) }

    fn trezor_coin(&self) -> Option<TrezorUtxoCoin> {
        json::from_value(self.conf["trezor_coin"].clone()).unwrap_or_default()
    }

    fn enable_spv_proof(&self) -> bool { self.conf["enable_spv_proof"].as_bool().unwrap_or(false) }

    /// Reads the Reloaded-only `protocol.header_layout` key (CRD ch.37 R37.5.9): from the
    /// raw `protocol` object beside `type`, as a plain case-sensitive string, outside the typed
    /// protocol descriptor. Absent means no effect; any present value that is not one of the closed
    /// value set (including a non-string) fails the coin build and falls back to nothing.
    ///
    /// Precedence (R37.5.9a): the layout read here governs the coin's header reads over the family
    /// `chain_variant` pins; `chain_variant` is still read and validated beside it.
    fn header_layout(&self) -> UtxoConfResult<Option<CoinVariant>> {
        let Some(value) = self.conf["protocol"].get("header_layout") else {
            return Ok(None);
        };
        value
            .as_str()
            .and_then(CoinVariant::from_header_layout)
            .map(Some)
            .or_mm_err(|| UtxoConfError::InvalidHeaderLayout {
                ticker: self.ticker.to_owned(),
                value: value.to_string(),
            })
    }

    /// Reads `protocol.chain_variant` (CRD ch.37 R37.5.1): from the raw `protocol` object beside
    /// `type`, as a plain string compared exactly and case-sensitively against the eight spellings,
    /// outside the typed protocol descriptor. Absent means no key; any present value that is not one
    /// of the eight (including a non-string) fails the coin build.
    fn chain_variant(&self) -> UtxoConfResult<Option<ChainVariant>> {
        let Some(value) = self.conf["protocol"].get("chain_variant") else {
            return Ok(None);
        };
        value
            .as_str()
            .and_then(ChainVariant::from_name)
            .map(Some)
            .or_mm_err(|| UtxoConfError::InvalidChainVariant {
                ticker: self.ticker.to_owned(),
                value: value.to_string(),
            })
    }

    /// The reader of a coin with neither header key: Qtum for the QTUM and QRC20 protocol types,
    /// standard otherwise. A documented divergence from R37.5.1, which maps an absent key to the
    /// default family for every protocol type; the maintainer keeps it.
    fn protocol_header_reader(&self) -> CoinVariant {
        match self.conf["protocol"]["type"].as_str() {
            Some("QTUM") | Some("QRC20") => CoinVariant::Qtum,
            _ => CoinVariant::Standard,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utxo::EnableCoinScanPolicy;
    use crate::utxo::UtxoRpcMode;
    use crate::PrivKeyActivationPolicy;
    use serde_json::json;

    fn params() -> UtxoActivationParams {
        UtxoActivationParams {
            mode: UtxoRpcMode::Native,
            utxo_merge_params: None,
            tx_history: false,
            required_confirmations: None,
            requires_notarization: None,
            address_format: None,
            gap_limit: None,
            min_addresses_number: None,
            scan_policy: EnableCoinScanPolicy::default(),
            priv_key_policy: PrivKeyActivationPolicy::IguanaPrivKey,
            check_utxo_maturity: None,
            path_to_address: None,
        }
    }

    fn build(ticker: &str, conf: Json) -> UtxoConfResult<UtxoCoinConf> {
        UtxoConfBuilder::new(&conf, &params(), ticker).build()
    }

    fn build_with(ticker: &str, conf: Json, policy: UnrecognisedEnumPolicy) -> UtxoConfResult<UtxoCoinConf> {
        UtxoConfBuilder::new(&conf, &params(), ticker)
            .with_unrecognised_enum_policy(policy)
            .build()
    }

    fn warnings_of(ticker: &str, conf: Json, policy: UnrecognisedEnumPolicy) -> (UtxoCoinConf, Vec<String>) {
        UtxoConfBuilder::new(&conf, &params(), ticker)
            .with_unrecognised_enum_policy(policy)
            .build_with_warnings()
            .unwrap()
    }

    /// CRD ch.38 R38.6.6k item 1: a coin's own `fee_estimate_cache_secs` is an integer from 0 to 600, or the
    /// build fails naming the key.
    #[test]
    fn fee_estimate_cache_secs_of_a_coin_is_zero_to_six_hundred() {
        for secs in [json!(0), json!(15), json!(600)] {
            build("RIN", json!({"coin": "RIN", "fee_estimate_cache_secs": secs})).unwrap();
        }
        build("RIN", json!({"coin": "RIN"})).unwrap();
        for bad in [json!(601), json!(-1), json!("5"), json!(1.5), json!(null)] {
            let err = build("RIN", json!({"coin": "RIN", "fee_estimate_cache_secs": bad.clone()})).unwrap_err();
            assert!(err.to_string().contains("fee_estimate_cache_secs"), "{bad}: {err}");
        }
    }

    fn rxd_conf() -> Json { json!({"coin": "RXD", "fork_id": "0x40", "signature_version": "fork_id_rxd"}) }

    /// A coin entry whose `signature_version` is genuinely unrecognised (the Radiant value used to play this
    /// role before the fourth accepted value existed).
    fn unknown_value_conf() -> Json { json!({"coin": "RXD", "fork_id": "0x40", "signature_version": "fork_id_xyz"}) }

    #[test]
    fn policy_follows_the_mm2_json_switch_and_only_the_boolean_true_turns_it_on() {
        use mm2_core::mm_ctx::MmCtxBuilder;
        let policy = |conf: Json| unrecognised_enum_policy(&MmCtxBuilder::new().with_conf(conf).into_mm_arc());
        assert_eq!(policy(json!({})), UnrecognisedEnumPolicy::Fail);
        assert_eq!(
            policy(json!({ COIN_CONF_UNRECOGNISED_ENUM_FALLS_BACK_KEY: true })),
            UnrecognisedEnumPolicy::WarnAndFallBack
        );
        for off in [json!(false), json!("true"), json!(1), json!(null)] {
            let conf = json!({ COIN_CONF_UNRECOGNISED_ENUM_FALLS_BACK_KEY: off });
            assert_eq!(policy(conf), UnrecognisedEnumPolicy::Fail, "{off}");
        }
    }

    #[test]
    fn the_default_policy_is_fail() {
        // `build` without `with_unrecognised_enum_policy` is what a caller that forgets the call gets.
        assert!(build("RXD", unknown_value_conf()).is_err());
        assert_eq!(UnrecognisedEnumPolicy::default(), UnrecognisedEnumPolicy::Fail);
    }

    #[test]
    fn unrecognised_signature_version_warns_once_and_falls_back_when_the_switch_selects_it() {
        let (conf, warnings) = warnings_of("RXD", unknown_value_conf(), UnrecognisedEnumPolicy::WarnAndFallBack);
        // The entry's `fork_id` is non-zero, so the absent-key default is `ForkId`.
        assert_eq!(conf.signature_version, SignatureVersion::ForkId);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        let w = &warnings[0];
        assert!(w.contains("signature_version"), "the key must be named: {w}");
        assert!(w.contains("fork_id_xyz"), "the value must be named: {w}");
        assert!(w.contains(COIN_CONF_UNRECOGNISED_ENUM_FALLS_BACK_KEY), "{w}");

        // The fallback is the absent-key default, not a fixed value.
        let conf = build_with(
            "XYZ",
            json!({"coin": "XYZ", "signature_version": "nope"}),
            UnrecognisedEnumPolicy::WarnAndFallBack,
        )
        .unwrap();
        assert_eq!(conf.signature_version, SignatureVersion::Base);
    }

    #[test]
    fn not_used_policy_falls_back_without_a_warning() {
        let (conf, warnings) = warnings_of("RXD", unknown_value_conf(), UnrecognisedEnumPolicy::NotUsed);
        assert_eq!(conf.signature_version, SignatureVersion::ForkId);
        assert!(warnings.is_empty());
    }

    #[test]
    fn no_warning_for_recognised_absent_or_null_values_under_either_switch_setting() {
        for policy in [UnrecognisedEnumPolicy::Fail, UnrecognisedEnumPolicy::WarnAndFallBack] {
            for conf in [
                json!({"coin": "XYZ"}),
                json!({"coin": "XYZ", "signature_version": null}),
                json!({"coin": "XYZ", "signature_version": "witness_v0"}),
            ] {
                let (_, warnings) = warnings_of("XYZ", conf.clone(), policy);
                assert!(warnings.is_empty(), "{policy:?} {conf}: {warnings:?}");
            }
            let conf = build_with("XYZ", json!({"coin": "XYZ"}), policy).unwrap();
            assert_eq!(conf.signature_version, SignatureVersion::Base);
        }
    }

    #[test]
    fn a_malformed_fork_id_fails_instead_of_panicking_under_every_policy() {
        for policy in [
            UnrecognisedEnumPolicy::Fail,
            UnrecognisedEnumPolicy::WarnAndFallBack,
            UnrecognisedEnumPolicy::NotUsed,
        ] {
            for bad in ["0xZZ", "", "0x", "zz", "0x1ffffffff"] {
                let err = build_with("XYZ", json!({"coin": "XYZ", "fork_id": bad}), policy)
                    .err()
                    .unwrap_or_else(|| panic!("fork_id {bad:?} must fail under {policy:?}"));
                let msg = err.to_string();
                assert!(msg.contains("fork_id") && msg.contains(bad), "{msg}");
            }
        }
    }

    #[test]
    fn a_non_string_fork_id_fails_instead_of_taking_the_default() {
        for bad in [json!(64), json!(true), json!(["0x40"]), json!({"a": 1})] {
            let err = build("XYZ", json!({"coin": "XYZ", "fork_id": bad}))
                .err()
                .unwrap_or_else(|| panic!("fork_id {bad} must fail"));
            assert!(err.to_string().contains(&bad.to_string()), "{err}");
        }
    }

    #[test]
    fn fork_id_is_parsed_and_an_absent_one_keeps_the_default() {
        assert_eq!(build("XYZ", json!({"coin": "XYZ"})).unwrap().fork_id, 0);
        assert_eq!(
            build("XYZ", json!({"coin": "XYZ", "fork_id": null})).unwrap().fork_id,
            0
        );
        assert_eq!(build("BCH", json!({"coin": "BCH"})).unwrap().fork_id, 0x40);
        assert_eq!(
            build("XYZ", json!({"coin": "XYZ", "fork_id": "0x40"})).unwrap().fork_id,
            0x40
        );
        assert_eq!(
            build("XYZ", json!({"coin": "XYZ", "fork_id": "40"})).unwrap().fork_id,
            0x40
        );
        assert_eq!(
            build("XYZ", json!({"coin": "XYZ", "fork_id": "0x0"})).unwrap().fork_id,
            0
        );
    }

    #[test]
    fn unrecognised_signature_version_fails_the_build() {
        let err = build("RXD", unknown_value_conf())
            .err()
            .expect("an unrecognised signature_version must not fall back to a default");
        let msg = err.to_string();
        assert!(msg.contains("fork_id_xyz"), "the offending value must be named: {msg}");
        assert!(msg.contains("witness_v0"), "the accepted set must be listed: {msg}");

        // A present value of the wrong JSON type is equally unrecognised.
        assert!(build("XYZ", json!({"coin": "XYZ", "signature_version": 1})).is_err());
        assert!(build("XYZ", json!({"coin": "XYZ", "signature_version": ""})).is_err());
    }

    #[test]
    fn recognised_signature_version_is_honoured() {
        for (name, expected) in [
            ("base", SignatureVersion::Base),
            ("witness_v0", SignatureVersion::WitnessV0),
            ("fork_id", SignatureVersion::ForkId),
            ("fork_id_rxd", SignatureVersion::ForkIdRxd),
        ] {
            let conf = build("XYZ", json!({"coin": "XYZ", "signature_version": name})).unwrap();
            assert_eq!(conf.signature_version, expected);
        }
    }

    /// CRD ch.58 T1: the fourth value resolves to the Radiant dialect under every switch setting, with
    /// no warning; the other three strings resolve to their own versions; unknown values follow R58.2.2.
    /// The unknown-value cases use the fall-back setting because the default setting (#54) fails
    /// activation for them instead of resolving.
    #[test]
    fn t1_signature_version_resolution_for_all_four_strings_and_unknown_values() {
        for policy in [
            UnrecognisedEnumPolicy::Fail,
            UnrecognisedEnumPolicy::WarnAndFallBack,
            UnrecognisedEnumPolicy::NotUsed,
        ] {
            let (conf, warnings) = warnings_of("RXD", rxd_conf(), policy);
            assert_eq!(conf.signature_version, SignatureVersion::ForkIdRxd, "{policy:?}");
            assert!(warnings.is_empty(), "{policy:?}: {warnings:?}");
        }
        for (name, expected) in [
            ("base", SignatureVersion::Base),
            ("witness_v0", SignatureVersion::WitnessV0),
            ("fork_id", SignatureVersion::ForkId),
            ("fork_id_rxd", SignatureVersion::ForkIdRxd),
        ] {
            let conf = build_with(
                "XYZ",
                json!({"coin": "XYZ", "signature_version": name}),
                UnrecognisedEnumPolicy::WarnAndFallBack,
            )
            .unwrap();
            assert_eq!(conf.signature_version, expected, "{name}");
        }
        let fb = UnrecognisedEnumPolicy::WarnAndFallBack;
        let conf = build_with(
            "XYZ",
            json!({"coin": "XYZ", "fork_id": "0x40", "signature_version": "nope"}),
            fb,
        )
        .unwrap();
        assert_eq!(conf.signature_version, SignatureVersion::ForkId);
        let conf = build_with("XYZ", json!({"coin": "XYZ", "signature_version": "nope"}), fb).unwrap();
        assert_eq!(conf.signature_version, SignatureVersion::Base);
        let conf = build_with(
            "XYZ",
            json!({"coin": "XYZ", "fork_id": "0x0", "signature_version": "nope"}),
            fb,
        )
        .unwrap();
        assert_eq!(conf.signature_version, SignatureVersion::Base);
        let conf = build_with("BCH", json!({"coin": "BCH", "signature_version": "nope"}), fb).unwrap();
        assert_eq!(conf.signature_version, SignatureVersion::ForkId);
    }

    #[test]
    fn absent_signature_version_keeps_the_default() {
        // No `fork_id` (or a zero one) and not BCH: `Base`.
        let conf = build("XYZ", json!({"coin": "XYZ"})).unwrap();
        assert_eq!(conf.signature_version, SignatureVersion::Base);
        let conf = build("XYZ", json!({"coin": "XYZ", "fork_id": "0x00"})).unwrap();
        assert_eq!(conf.signature_version, SignatureVersion::Base);

        // Non-zero `fork_id`: `ForkId`.
        let conf = build("XYZ", json!({"coin": "XYZ", "fork_id": "0x40"})).unwrap();
        assert_eq!(conf.signature_version, SignatureVersion::ForkId);

        // BCH has both defaults built in.
        let conf = build("BCH", json!({"coin": "BCH"})).unwrap();
        assert_eq!(conf.signature_version, SignatureVersion::ForkId);

        // An explicit JSON `null` is treated as absent.
        let conf = build("XYZ", json!({"coin": "XYZ", "signature_version": null})).unwrap();
        assert_eq!(conf.signature_version, SignatureVersion::Base);
        let conf = build(
            "XYZ",
            json!({"coin": "XYZ", "fork_id": "0x40", "signature_version": null}),
        )
        .unwrap();
        assert_eq!(conf.signature_version, SignatureVersion::ForkId);
    }
}
