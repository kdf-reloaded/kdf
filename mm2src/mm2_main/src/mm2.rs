/******************************************************************************
 * Copyright © 2014-2019 The SuperNET Developers.                             *
 *                                                                            *
 * See the AUTHORS, DEVELOPER-AGREEMENT and LICENSE files at                  *
 * the top-level directory of this distribution for the individual copyright  *
 * holder information and the developer policies on copyright and licensing.  *
 *                                                                            *
 * Unless otherwise agreed in a custom licensing agreement, no part of the    *
 * SuperNET software, including this file may be copied, modified, propagated *
 * or distributed except according to the terms contained in the LICENSE file *
 *                                                                            *
 * Removal or modification of this copyright notice is prohibited.            *
 *                                                                            *
 ******************************************************************************/
//
//  mm2.rs
//  marketmaker
//
//  Copyright © 2017-2019 SuperNET. All rights reserved.
//

#![cfg_attr(target_arch = "wasm32", allow(dead_code))]
#![cfg_attr(target_arch = "wasm32", allow(unused_imports))]

#[cfg(not(target_arch = "wasm32"))] use common::block_on;
use common::crash_reports::init_crash_reports;
use common::double_panic_crash;
use common::log::LogLevel;
use mm2_core::mm_ctx::MmCtxBuilder;

use common::log::info;
#[cfg(any(feature = "custom-swap-locktime", feature = "unsafe-rpc-wire-dump"))]
use common::log::warn;
#[cfg(feature = "custom-swap-locktime")]
use lp_swap::PAYMENT_LOCKTIME;
#[cfg(feature = "custom-swap-locktime")]
use std::sync::atomic::Ordering;

use derive_more::Display;
use gstuff::slurp;
use lazy_static::lazy_static;
use regex::Regex;

use serde::ser::Serialize;
use serde_json::{self as json, Value as Json};

use std::env;
use std::ffi::OsString;
#[cfg(not(target_arch = "wasm32"))] use std::path::PathBuf;
use std::process::exit;
use std::ptr::null;
use std::str;

#[path = "lp_native_dex.rs"] mod lp_native_dex;
use self::lp_native_dex::lp_init;
use coins::update_coins_config;
use mm2_err_handle::prelude::*;

#[cfg(not(target_arch = "wasm32"))]
#[path = "database.rs"]
pub mod database;

#[path = "lp_dispatcher.rs"] pub mod lp_dispatcher;
#[path = "lp_message_service.rs"] pub mod lp_message_service;
#[path = "lp_network.rs"] pub mod lp_network;
#[path = "lp_ordermatch.rs"] pub mod lp_ordermatch;
#[path = "lp_stats.rs"] pub mod lp_stats;
#[path = "lp_swap.rs"] pub mod lp_swap;
#[cfg(not(target_arch = "wasm32"))]
#[path = "lp_wallet.rs"]
pub mod lp_wallet;
#[path = "rpc.rs"] pub mod rpc;

#[cfg(any(test, target_arch = "wasm32"))]
#[path = "mm2_tests.rs"]
pub mod mm2_tests;

pub const MM_DATETIME: &str = env!("MM_DATETIME");
pub const MM_VERSION: &str = env!("MM_VERSION");
pub const PASSWORD_MAXIMUM_CONSECUTIVE_CHARACTERS: usize = 3;

#[cfg(feature = "custom-swap-locktime")]
const CUSTOM_PAYMENT_LOCKTIME_DEFAULT: u64 = 900;

#[derive(Serialize)]
pub struct MmVersionResult {
    result: &'static str,
    datetime: &'static str,
}

impl MmVersionResult {
    pub const fn new() -> MmVersionResult {
        MmVersionResult {
            result: MM_VERSION,
            datetime: MM_DATETIME,
        }
    }

    pub fn to_json(&self) -> Json { json::to_value(self).expect("expected valid JSON object") }
}

impl Default for MmVersionResult {
    fn default() -> Self { Self::new() }
}

pub struct LpMainParams {
    conf: Json,
    filter: Option<LogLevel>,
    /// Where the configuration and the coins list were loaded from, if KDF loaded them itself.
    /// `None` when the host application supplied the configuration directly.
    #[cfg(not(target_arch = "wasm32"))]
    sources: Option<ConfigSources>,
}

impl LpMainParams {
    pub fn with_conf(conf: Json) -> LpMainParams {
        LpMainParams {
            conf,
            filter: None,
            #[cfg(not(target_arch = "wasm32"))]
            sources: None,
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn config_sources(mut self, sources: ConfigSources) -> LpMainParams {
        self.sources = Some(sources);
        self
    }

    pub fn log_filter(mut self, filter: Option<LogLevel>) -> LpMainParams {
        self.filter = filter;
        self
    }
}

#[derive(Debug, Display, PartialEq)]
pub enum PasswordPolicyError {
    #[display(fmt = "Password can't contain the word password")]
    ContainsTheWordPassword,
    #[display(fmt = "Password length should be between 8 and 32")]
    PasswordLength,
    #[display(fmt = "Password should contain at least 1 digit")]
    PasswordMissDigit,
    #[display(fmt = "Password should contain at least 1 lowercase character")]
    PasswordMissLowercase,
    #[display(fmt = "Password should contain at least 1 uppercase character")]
    PasswordMissUppercase,
    #[display(fmt = "Password should contain at least 1 special character")]
    PasswordMissSpecialCharacter,
    #[display(fmt = "Password can't contain the same character 3 times in a row")]
    PasswordConsecutiveCharactersExceeded,
}

pub fn password_policy(password: &str) -> Result<(), MmError<PasswordPolicyError>> {
    lazy_static! {
        static ref REGEX_NUMBER: Regex = Regex::new(".*[0-9].*").unwrap();
        static ref REGEX_LOWERCASE: Regex = Regex::new(".*[a-z].*").unwrap();
        static ref REGEX_UPPERCASE: Regex = Regex::new(".*[A-Z].*").unwrap();
        static ref REGEX_SPECIFIC_CHARS: Regex = Regex::new(".*[^A-Za-z0-9].*").unwrap();
    }
    if password.to_lowercase().contains("password") {
        return MmError::err(PasswordPolicyError::ContainsTheWordPassword);
    }
    let password_len = password.chars().count();
    if !(8..=32).contains(&password_len) {
        return MmError::err(PasswordPolicyError::PasswordLength);
    }
    if !REGEX_NUMBER.is_match(password) {
        return MmError::err(PasswordPolicyError::PasswordMissDigit);
    }
    if !REGEX_LOWERCASE.is_match(password) {
        return MmError::err(PasswordPolicyError::PasswordMissLowercase);
    }
    if !REGEX_UPPERCASE.is_match(password) {
        return MmError::err(PasswordPolicyError::PasswordMissUppercase);
    }
    if !REGEX_SPECIFIC_CHARS.is_match(password) {
        return MmError::err(PasswordPolicyError::PasswordMissSpecialCharacter);
    }
    if !common::is_acceptable_input_on_repeated_characters(password, PASSWORD_MAXIMUM_CONSECUTIVE_CHARACTERS) {
        return MmError::err(PasswordPolicyError::PasswordConsecutiveCharactersExceeded);
    }
    Ok(())
}

#[test]
fn check_password_policy() {
    // Length
    assert_eq!(
        password_policy("123").unwrap_err().into_inner(),
        PasswordPolicyError::PasswordLength
    );

    // Miss special character
    assert_eq!(
        password_policy("pass123worD").unwrap_err().into_inner(),
        PasswordPolicyError::PasswordMissSpecialCharacter
    );

    // Miss digit
    assert_eq!(
        password_policy("SecretPassSoStrong$*").unwrap_err().into_inner(),
        PasswordPolicyError::PasswordMissDigit
    );

    // Miss lowercase
    assert_eq!(
        password_policy("SECRETPASS-SOSTRONG123*").unwrap_err().into_inner(),
        PasswordPolicyError::PasswordMissLowercase
    );

    // Miss uppercase
    assert_eq!(
        password_policy("secretpass-sostrong123*").unwrap_err().into_inner(),
        PasswordPolicyError::PasswordMissUppercase
    );

    // Miss uppercase
    assert_eq!(
        password_policy("SecretPassSoStrong123*aaa").unwrap_err().into_inner(),
        PasswordPolicyError::PasswordConsecutiveCharactersExceeded
    );

    // Contains Password uppercase
    assert_eq!(
        password_policy("Password123*$").unwrap_err().into_inner(),
        PasswordPolicyError::ContainsTheWordPassword
    );

    // Contains Password lowercase
    assert_eq!(
        password_policy("Foopassword123*$").unwrap_err().into_inner(),
        PasswordPolicyError::ContainsTheWordPassword
    );

    // Valid passwords
    password_policy("StrongPass123*").unwrap();
    password_policy(r#"StrongPass123[]\± "#).unwrap();
    password_policy("StrongPass123£StrongPass123£Pass").unwrap();
}

#[cfg(feature = "custom-swap-locktime")]
/// Reads `payment_locktime` from conf arg and assigns it into `PAYMENT_LOCKTIME` in lp_swap.
/// Assigns 900 if `payment_locktime` is invalid or not provided.
fn initialize_payment_locktime(conf: &Json) {
    match conf["payment_locktime"].as_u64() {
        Some(lt) => PAYMENT_LOCKTIME.store(lt, Ordering::Relaxed),
        None => {
            warn!(
                "payment_locktime is either invalid type or not provided in the configuration or
                MM2.json file. payment_locktime will be proceeded as {} seconds.",
                CUSTOM_PAYMENT_LOCKTIME_DEFAULT
            );
        },
    };
}

#[cfg(feature = "unsafe-rpc-wire-dump")]
fn maybe_warn_unsafe_rpc_wire_dump_enabled() {
    let wire_dump_enabled = matches!(env::var("MM2_RPC_WIRE_DUMP").ok().as_deref(), Some("1"));
    if !wire_dump_enabled {
        return;
    }

    let secrets_enabled = matches!(env::var("MM2_RPC_WIRE_DUMP_SECRETS").ok().as_deref(), Some("1"));
    if secrets_enabled {
        warn!(
            "UNSAFE MODE: MM2_RPC_WIRE_DUMP=1 and MM2_RPC_WIRE_DUMP_SECRETS=1 are active; RPC wire dumps include secrets"
        );
    } else {
        warn!("UNSAFE MODE: MM2_RPC_WIRE_DUMP=1 is active; RPC wire dumps are enabled with secret redaction");
    }
}

/// * `ctx_cb` - callback used to share the `MmCtx` ID with the call site.
pub async fn lp_main(params: LpMainParams, ctx_cb: &dyn Fn(u32)) -> Result<(), String> {
    let log_filter = params.filter.unwrap_or_default();
    if let Err(e) = init_logger(log_filter) {
        log!("Logger initialization failed: "(e))
    }

    #[cfg(not(target_arch = "wasm32"))]
    for line in startup_path_report(params.sources.as_ref(), env::var("MM_LOG").ok().as_deref()) {
        info!("{}", line);
    }

    #[cfg(feature = "unsafe-rpc-wire-dump")]
    maybe_warn_unsafe_rpc_wire_dump_enabled();

    let conf = params.conf;
    if !conf["rpc_password"].is_null() {
        if !conf["rpc_password"].is_string() {
            return ERR!("rpc_password must be string");
        }

        let is_weak_password_accepted = conf["allow_weak_password"].as_bool() == Some(true);

        if conf["rpc_password"].as_str() == Some("") {
            return ERR!("rpc_password must not be empty");
        }

        if !is_weak_password_accepted && cfg!(not(test)) {
            match password_policy(conf["rpc_password"].as_str().unwrap()) {
                Ok(_) => {},
                Err(err) => return Err(format!("{}", err)),
            }
        }
    }

    #[cfg(feature = "custom-swap-locktime")]
    initialize_payment_locktime(&conf);

    let ctx = MmCtxBuilder::new()
        .with_conf(conf)
        .with_log_level(log_filter)
        .with_version(MM_VERSION.into())
        .into_mm_arc();
    ctx_cb(try_s!(ctx.ffi_handle()));
    try_s!(lp_init(ctx).await);
    Ok(())
}

#[allow(dead_code)]
fn help() {
    // Removed options:
    // "client" - In MM2 anyone can be a Maker, the "client" option is no longer applicable.

    pintln! (
            "Command-line options.\n"
            "The first command-line argument is special and designates the mode.\n"
            "\n"
            "  help                       ..  Display this message.\n"
            "  version, --version         ..  Display the version and exit.\n"
            "  btc2kmd {WIF or BTC}       ..  Convert a BTC WIF into a KMD WIF.\n"
            "  events                     ..  Listen to a feed coming from a separate MM daemon and print it to stdout.\n"
            "  vanity {substring}         ..  Tries to find an address with the given substring.\n"
            "  update_config {SRC} {DST}  ..  Update the configuration of coins from the SRC config and save it to DST file.\n"
            "  {JSON configuration}       ..  Run the MarketMaker daemon.\n"
            "\n"
            "Some (but not all) of the JSON configuration parameters (* - required):\n"
            "\n"
            // We don't want to break the existing RPC API,
            // so the "refrel=coinmarketcap" designator will act as autoselect,
            // using the CoinGecko behind the scenes unless the "cmc_key" is given.
            // In the future, when MM2 is more widely used and thus we're working more tighly with the GUIs (BarterDEX, HyperDEX, dICO),
            // we might add the "refrel=cmc" and "refrel=coingecko" RPC options.
    /* we're not using the `portfolio` crate at present
            "  cmc_key        ..  CoinMarketCap Professional API Key. Switches from CoinGecko to CoinMarketCap.\n"
            "                     The Key can be obtained from 'https://pro.coinmarketcap.com/account'.\n"
    */
            "                     NB: The 'coins' command-line configuration must have the lowercased coin names in the 'name' field,\n"
          r#"                     {"coins": [{"name": "dash", "coin": "DASH", ...}, ...], ...}."# "\n"
            // cf. https://github.com/atomiclabs/hyperdex/blob/1d4ed3234b482e769124725c7e979eef5cd72d24/app/marketmaker/supported-currencies.js#L12
            "  coins          ..  Information about the currencies: their ticker symbols, names, ports, addresses, etc.\n"
            "                     If the field isn't present on the command line then we try loading it from the 'coins' file.\n"
            "  crash          ..  Simulate a crash to check how the crash handling works.\n"
            "  dbdir          ..  MM database path. 'DB' by default.\n"
            "  gui            ..  The information about GUI app using MM2 instance. Included in swap statuses shared with network.\n"
            "                 ..  It's recommended to put essential info to this field (application name, OS, version, etc).\n"
            "                 ..  e.g. AtomicDEX iOS 1.0.1000.\n"
            "  myipaddr       ..  IP address to bind to for P2P networking.\n"
            "  netid          ..  Subnetwork. Affects ports and keys.\n"
            "  passphrase *   ..  Wallet seed.\n"
            "                     Compressed WIFs and hexadecimal ECDSA keys (prefixed with 0x) are also accepted.\n"
            "  panic          ..  Simulate a panic to see if backtrace works.\n"
            // cf. https://github.com/atomiclabs/hyperdex/pull/563/commits/6d17c0c994693b768e30130855c679a7849a2b27
            "  rpccors        ..  Access-Control-Allow-Origin header value to be used in all the RPC responses.\n"
            "                     Default is currently 'http://localhost:3000'\n"
            "  rpcip          ..  IP address to bind to for RPC server. Overrides the 127.0.0.1 default\n"
            "  rpc_password   ..  RPC password used to authorize non-public RPC calls\n"
            "                     MM generates password from passphrase if this field is not set\n"
            "  rpc_local_only ..  MM forbids some RPC requests from not loopback (localhost) IPs as additional security measure.\n"
            "                     Defaults to `true`, set `false` to disable. `Use with caution`.\n"
            "  rpcport        ..  If > 1000 overrides the 7783 default.\n"
            "  i_am_seed      ..  Activate the seed node mode (acting as a relay for mm2 clients).\n"
            "                     Defaults to `false`.\n"
            "  seednodes      ..  Seednode IPs that node will use.\n"
            "                     At least one seed IP must be present if the node is not a seed itself.\n"
            "  stderr         ..  Print a message to stderr and exit.\n"
            "  userhome       ..  System home directory of a user ('/root' by default).\n"
            "  wif            ..  `1` to add WIFs to the information we provide about a coin.\n"
            "\n"
            "Environment variables:\n"
            "\n"
            // Per-process (we might have several MM instances running but they will share the same log).
            "  MM_CONF_PATH   ..  File path. MM2 will try to load the JSON configuration from this file.\n"
            "                     File must contain valid json with structure mentioned above.\n"
            "                     Defaults to `MM2.json`\n"
            "  MM_COINS_PATH  ..  File path. MM2 will try to load coins data from this file.\n"
            "                     File must contain valid json.\n"
            "                     Recommended: https://github.com/jl777/coins/blob/master/coins.\n"
            "                     Defaults to `coins`.\n"
            "  MM_LOG         ..  File path. Must end with '.log'. MM will log to this file.\n"
            "\n"
            // Generated from https://github.com/KomodoPlatform/developer-docs/tree/sidd.
            // (SHossain, siddhartha-crypto).
            "See also the online documentation at\n"
            "https://developers.atomicdex.io\n"
        )
}

#[cfg(not(target_arch = "wasm32"))]
#[allow(dead_code)] // Not used by mm2_lib.
pub fn mm2_main() {
    use libc::c_char;

    init_crash_reports();
    log!({"KDF-Reloaded DeFi Framework {} DT {}", MM_VERSION, MM_DATETIME});

    // Temporarily simulate `argv[]` for the C version of the main method.
    let args: Vec<String> = env::args()
        .map(|mut arg| {
            arg.push('\0');
            arg
        })
        .collect();
    let mut args: Vec<*const c_char> = args.iter().map(|s| s.as_ptr() as *const c_char).collect();
    args.push(null());

    let args_os: Vec<OsString> = env::args_os().collect();

    // NB: The first argument is special, being used as the mode switcher.
    // The other arguments might be used to pass the data to the various MM modes,
    // we're not checking them for the mode switches in order not to risk [untrusted] data being mistaken for a mode switch.
    let first_arg = args_os.get(1).and_then(|arg| arg.to_str());

    if first_arg == Some("panic") {
        panic!("panic message")
    }
    if first_arg == Some("crash") {
        double_panic_crash()
    }
    if first_arg == Some("stderr") {
        eprintln!("This goes to stderr");
        return;
    }
    if first_arg == Some("update_config") {
        match on_update_config(&args_os) {
            Ok(_) => println!("Success"),
            Err(e) => eprintln!("{}", e),
        }
        return;
    }

    if first_arg == Some("--help") || first_arg == Some("-h") || first_arg == Some("help") {
        help();
        return;
    }
    if cfg!(windows) && first_arg == Some("/?") {
        help();
        return;
    }
    // The version banner has already been logged above; exit successfully
    // instead of parsing the flag as a JSON configuration.
    if first_arg == Some("--version") || first_arg == Some("version") {
        return;
    }

    if let Err(err) = run_lp_main(first_arg, &|_| ()) {
        log!((err));
        exit(1);
    }
}

/// Returns `path` made absolute against the current working directory for log output.
/// No symlink resolution and no filesystem access; falls back to the path as given on failure.
pub(crate) fn absolute_path_for_log(path: &std::path::Path) -> String {
    match std::path::absolute(path) {
        Ok(abs) => abs.display().to_string(),
        Err(_) => path.display().to_string(),
    }
}

/// Where the runtime configuration was read from.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug, PartialEq)]
pub enum ConfigSource {
    /// The first command-line argument.
    CommandLine,
    /// A file; `from_env` is true when the path came from `MM_CONF_PATH`.
    File { path: PathBuf, from_env: bool },
}

/// Where the coins list was read from.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug, PartialEq)]
pub enum CoinsSource {
    /// `coins` was present in the configuration; `ignored_env_path` is the `MM_COINS_PATH` value
    /// when that variable was set but therefore not used.
    Inline { ignored_env_path: Option<PathBuf> },
    /// A file; `from_env` is true when the path came from `MM_COINS_PATH`.
    File { path: PathBuf, from_env: bool },
}

/// Sources of the configuration and of the coins list (paths and kinds only, never contents).
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug, PartialEq)]
pub struct ConfigSources {
    pub conf: ConfigSource,
    pub coins: CoinsSource,
}

#[cfg(not(target_arch = "wasm32"))]
fn path_origin(from_env: bool, env_name: &str) -> String {
    if from_env {
        format!("via {env_name}")
    } else {
        "default name".to_owned()
    }
}

/// Builds the INFO lines reporting which files KDF used at startup. Paths and source kinds only.
/// `mm_log` is the raw `MM_LOG` value.
#[cfg(not(target_arch = "wasm32"))]
fn startup_path_report(sources: Option<&ConfigSources>, mm_log: Option<&str>) -> Vec<String> {
    use std::path::Path;

    let mut lines = Vec::new();
    match sources {
        None => lines.push("Configuration: supplied by the host application (no configuration file read)".to_owned()),
        Some(sources) => {
            lines.push(match &sources.conf {
                ConfigSource::CommandLine => "Configuration: from command-line argument".to_owned(),
                ConfigSource::File { path, from_env } => format!(
                    "Configuration: from file {} ({})",
                    absolute_path_for_log(path),
                    path_origin(*from_env, "MM_CONF_PATH")
                ),
            });
            lines.push(match &sources.coins {
                CoinsSource::Inline { ignored_env_path: None } => "Coins: inline in the configuration".to_owned(),
                CoinsSource::Inline {
                    ignored_env_path: Some(ignored),
                } => format!(
                    "Coins: inline in the configuration (MM_COINS_PATH={} is set but ignored)",
                    absolute_path_for_log(ignored)
                ),
                CoinsSource::File { path, from_env } => format!(
                    "Coins: from file {} ({})",
                    absolute_path_for_log(path),
                    path_origin(*from_env, "MM_COINS_PATH")
                ),
            });
        },
    }
    match mm_log {
        // Mirrors the `.log` suffix rule applied when the log file is opened.
        Some(path) if path.ends_with(".log") => lines.push(format!(
            "Log file: {} (via MM_LOG)",
            absolute_path_for_log(Path::new(path))
        )),
        Some(_) => lines.push("Log file: MM_LOG is set but does not end in .log, file logging is disabled".to_owned()),
        None => {},
    }
    lines
}

#[cfg(not(target_arch = "wasm32"))]
/// Parses and returns the `first_arg` as JSON, together with the sources the configuration
/// and the coins list were loaded from.
/// Attempts to load the config from `MM2.json` file if `first_arg` is None
pub fn get_mm2config(first_arg: Option<&str>) -> Result<(Json, ConfigSources), String> {
    let conf_path_env = env::var("MM_CONF_PATH").ok();
    let conf_path = conf_path_env.clone().unwrap_or_else(|| "MM2.json".into());
    let conf_from_file = slurp(&conf_path);
    let (conf, conf_source) = match first_arg {
        Some(s) => (s, ConfigSource::CommandLine),
        None => {
            if conf_from_file.is_empty() {
                return ERR!(
                    "Config is not set from command line arg and {} file doesn't exist.",
                    conf_path
                );
            }
            (try_s!(std::str::from_utf8(&conf_from_file)), ConfigSource::File {
                path: PathBuf::from(&conf_path),
                from_env: conf_path_env.is_some(),
            })
        },
    };

    let mut conf: Json = match json::from_str(conf) {
        Ok(json) => json,
        // Don't include raw config payload in the error to avoid leaking sensitive values.
        Err(_) => return ERR!("Couldn't parse mm2 config to JSON format!"),
    };

    let coins_path_env = env::var("MM_COINS_PATH").ok();
    let coins_source = if conf["coins"].is_null() {
        let coins_path = coins_path_env.clone().unwrap_or_else(|| "coins".into());
        let coins_from_file = slurp(&coins_path);
        if coins_from_file.is_empty() {
            return ERR!(
                "No coins are set in JSON config and '{}' file doesn't exist",
                coins_path
            );
        }
        let coins: Json = match json::from_slice(&coins_from_file) {
            Ok(j) => j,
            Err(e) => {
                return ERR!(
                    "Error {} parsing the coins file, please ensure it contains valid json",
                    e
                )
            },
        };

        if !coins.is_array() {
            return ERR!("'coins' file must contain a JSON array");
        }

        conf["coins"] = coins;
        CoinsSource::File {
            path: PathBuf::from(&coins_path),
            from_env: coins_path_env.is_some(),
        }
    } else if !conf["coins"].is_array() {
        return ERR!("'coins' field must be a JSON array");
    } else {
        CoinsSource::Inline {
            ignored_env_path: coins_path_env.map(PathBuf::from),
        }
    };

    Ok((conf, ConfigSources {
        conf: conf_source,
        coins: coins_source,
    }))
}

/// Runs LP_main with result of `get_mm2config()`.
///
/// * `ctx_cb` - Invoked with the MM context handle,
///   allowing the `run_lp_main` caller to communicate with MM.
#[cfg(not(target_arch = "wasm32"))]
pub fn run_lp_main(first_arg: Option<&str>, ctx_cb: &dyn Fn(u32)) -> Result<(), String> {
    let (conf, sources) = get_mm2config(first_arg)?;

    let log_filter = LogLevel::from_env();

    let params = LpMainParams::with_conf(conf)
        .log_filter(log_filter)
        .config_sources(sources);
    try_s!(block_on(lp_main(params, ctx_cb)));
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn on_update_config(args: &[OsString]) -> Result<(), String> {
    use mm2_io::fs::safe_slurp;

    let src_path = args.get(2).ok_or(ERRL!("Expect path to the source coins config."))?;
    let dst_path = args.get(3).ok_or(ERRL!("Expect destination path."))?;

    let config = try_s!(safe_slurp(src_path));
    let mut config: Json = try_s!(json::from_slice(&config));

    let result = if config.is_array() {
        try_s!(update_coins_config(config))
    } else {
        // try to get config["coins"] as array
        let conf_obj = config.as_object_mut().ok_or(ERRL!("Expected coin list"))?;
        let coins = conf_obj.remove("coins").ok_or(ERRL!("Expected coin list"))?;
        let updated_coins = try_s!(update_coins_config(coins));
        conf_obj.insert("coins".into(), updated_coins);
        config
    };

    let buf = Vec::new();
    let formatter = json::ser::PrettyFormatter::with_indent(b"\t");
    let mut ser = json::Serializer::with_formatter(buf, formatter);
    try_s!(result.serialize(&mut ser));
    try_s!(std::fs::write(dst_path, ser.into_inner()));
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn init_logger(level: LogLevel) -> Result<(), String> {
    use common::log::UnifiedLoggerBuilder;

    UnifiedLoggerBuilder::default()
        .level_filter(level)
        .console(false)
        .mm_log(true)
        .try_init()
}

#[cfg(target_arch = "wasm32")]
fn init_logger(level: LogLevel) -> Result<(), String> {
    common::log::WasmLoggerBuilder::default().level_filter(level).try_init()
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::{get_mm2config, startup_path_report, CoinsSource, ConfigSource, ConfigSources};
    use lazy_static::lazy_static;
    use std::env;
    use std::ffi::OsString;
    use std::fs;
    use std::sync::Mutex;

    lazy_static! {
        static ref ENV_LOCK: Mutex<()> = Mutex::new(());
    }

    struct EnvVarGuard {
        key: &'static str,
        old: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let old = env::var_os(key);
            // SAFETY: test-only process-global env mutation is serialized by ENV_LOCK.
            unsafe { env::set_var(key, value) };
            EnvVarGuard { key, old }
        }

        fn remove(key: &'static str) -> Self {
            let old = env::var_os(key);
            // SAFETY: test-only process-global env mutation is serialized by ENV_LOCK.
            unsafe { env::remove_var(key) };
            EnvVarGuard { key, old }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.old.as_ref() {
                Some(value) => {
                    // SAFETY: test-only process-global env mutation is serialized by ENV_LOCK.
                    unsafe { env::set_var(self.key, value) };
                },
                None => {
                    // SAFETY: test-only process-global env mutation is serialized by ENV_LOCK.
                    unsafe { env::remove_var(self.key) };
                },
            }
        }
    }

    #[test]
    fn runtime_config_without_passphrase_uses_mm_coins_path() {
        let _env_lock = ENV_LOCK.lock().expect("env lock poisoned");

        let tmp_dir = env::temp_dir().join(format!("kdf-mm2config-test-{}", common::now_ms()));
        fs::create_dir_all(&tmp_dir).expect("unable to create temp dir");
        let coins_path = tmp_dir.join("kdf_coins.json");
        fs::write(&coins_path, br#"[{"coin":"DOC"}]"#).expect("unable to write coins file");

        let _coins_guard = EnvVarGuard::set("MM_COINS_PATH", coins_path.to_str().expect("valid coins path"));
        let _conf_guard = EnvVarGuard::remove("MM_CONF_PATH");

        let (conf, sources) = get_mm2config(Some(r#"{"gui":"test","netid":8762,"rpc_password":"StrongPass123*"}"#))
            .expect("config should load from runtime JSON and MM_COINS_PATH");
        assert_eq!(sources.conf, ConfigSource::CommandLine);
        assert_eq!(sources.coins, CoinsSource::File {
            path: coins_path.clone(),
            from_env: true
        });

        assert!(conf["coins"].is_array(), "coins should be loaded from MM_COINS_PATH");
        assert!(
            conf["passphrase"].is_null(),
            "missing passphrase should remain unset at config stage"
        );

        fs::remove_file(coins_path).expect("unable to remove coins file");
        fs::remove_dir(tmp_dir).expect("unable to remove temp dir");
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = env::temp_dir().join(format!("kdf-mm2config-{}-{}", tag, common::now_ms()));
        fs::create_dir_all(&dir).expect("unable to create temp dir");
        dir
    }

    #[test]
    fn missing_default_config_file_is_an_error() {
        let _env_lock = ENV_LOCK.lock().expect("env lock poisoned");
        let _conf_guard = EnvVarGuard::remove("MM_CONF_PATH");
        let _coins_guard = EnvVarGuard::remove("MM_COINS_PATH");
        // The default name resolves relative to the CWD, which the test binary does not control;
        // only assert when the file is absent.
        if std::path::Path::new("MM2.json").exists() || std::path::Path::new("coins").exists() {
            return;
        }
        assert!(
            get_mm2config(None).is_err(),
            "no command line arg and no MM2.json must fail"
        );
    }

    #[test]
    fn config_from_mm_conf_path_file_with_inline_coins_reports_ignored_coins_env() {
        let _env_lock = ENV_LOCK.lock().expect("env lock poisoned");
        let dir = temp_dir("conf-file");
        let conf_path = dir.join("my.json");
        fs::write(&conf_path, br#"{"netid":8762,"coins":[{"coin":"DOC"}]}"#).expect("unable to write conf");
        let _conf_guard = EnvVarGuard::set("MM_CONF_PATH", conf_path.to_str().expect("utf8 path"));
        let _coins_guard = EnvVarGuard::set("MM_COINS_PATH", "ignored-coins");

        let (_, sources) = get_mm2config(None).expect("config should load from MM_CONF_PATH");
        assert_eq!(sources.conf, ConfigSource::File {
            path: conf_path.clone(),
            from_env: true
        });
        assert_eq!(sources.coins, CoinsSource::Inline {
            ignored_env_path: Some("ignored-coins".into())
        });

        let lines = startup_path_report(Some(&sources), None);
        assert_eq!(lines, vec![
            format!("Configuration: from file {} (via MM_CONF_PATH)", conf_path.display()),
            format!(
                "Coins: inline in the configuration (MM_COINS_PATH={} is set but ignored)",
                std::path::absolute("ignored-coins").expect("absolute").display()
            ),
        ]);

        fs::remove_dir_all(dir).expect("unable to remove temp dir");
    }

    #[test]
    fn command_line_config_wins_over_conf_file_and_inline_coins_without_env() {
        let _env_lock = ENV_LOCK.lock().expect("env lock poisoned");
        let _conf_guard = EnvVarGuard::set("MM_CONF_PATH", "/nonexistent/never-read.json");
        let _coins_guard = EnvVarGuard::remove("MM_COINS_PATH");

        let (_, sources) = get_mm2config(Some(r#"{"coins":[]}"#)).expect("config should parse");
        assert_eq!(sources.conf, ConfigSource::CommandLine);
        assert_eq!(sources.coins, CoinsSource::Inline { ignored_env_path: None });
        assert_eq!(startup_path_report(Some(&sources), None), vec![
            "Configuration: from command-line argument".to_owned(),
            "Coins: inline in the configuration".to_owned(),
        ]);
    }

    #[test]
    fn report_makes_relative_paths_absolute_and_never_leaks_content() {
        let sources = ConfigSources {
            conf: ConfigSource::File {
                path: "MM2.json".into(),
                from_env: false,
            },
            coins: CoinsSource::File {
                path: "coins".into(),
                from_env: false,
            },
        };
        let cwd = env::current_dir().expect("cwd");
        let lines = startup_path_report(Some(&sources), Some("rel/kdf.log"));
        assert_eq!(lines, vec![
            format!(
                "Configuration: from file {} (default name)",
                cwd.join("MM2.json").display()
            ),
            format!("Coins: from file {} (default name)", cwd.join("coins").display()),
            format!("Log file: {} (via MM_LOG)", cwd.join("rel/kdf.log").display()),
        ]);
    }

    #[test]
    fn report_handles_host_supplied_config_and_bad_mm_log() {
        let lines = startup_path_report(None, Some("/tmp/not-a-log.txt"));
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("Configuration: supplied by the host application"));
        assert!(lines[1].contains("does not end in .log"));
        assert_eq!(startup_path_report(None, None).len(), 1);
    }
}
