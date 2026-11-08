//! The reference price service (CRD ch.62): where the node gets the USD prices of coins from.
//!
//! # Purpose
//!
//! One module owns the price sources, the fetch, the cross-check of the sources, the recognized price providers and
//! the warn-once log state. The simple market-maker bot (`simple_market_maker.rs`) and the swap-completion fiat
//! snapshot (`lp_swap/swap_rpc.rs`) both consume it.
//!
//! # Invariants
//!
//! - A *source* is a list of URLs tried in order; the first URL that answers is the source's answer, with no
//!   rotation. The sources of a node are fetched in parallel and combined (see [`combine_sources`]).
//! - A record enters the combined table only if its `price_provider` is recognized (see [`ProviderSet`]) and its
//!   `last_updated_timestamp` is not zero. A ticker served by several sources is kept only if their prices agree
//!   within [`PRICE_CONFLICT_TOLERANCE_PERCENT`]; otherwise it is a *conflict* and is absent from the table.
//! - On a native build each attempt is capped at [`PRICE_ATTEMPT_TIMEOUT_SECS`] seconds by the transport's bounded
//!   request, which has its own tests.
//! - A log line that names an endpoint names its host label ([`host_only`]) only, never the path or the query: a
//!   configured URL can carry a key. The text of an error that reaches a log line, a server's answer body included,
//!   is first scrubbed of the URL it was asked for (`scrub_node_url`).
//! - An endpoint failure, a ticker conflict and the bot's "no source answers" are logged once per episode: a WARN
//!   (for the last, an ERROR) when it starts, DEBUG while it lasts, an INFO when it ends. The endpoint state is
//!   shared by every consumer of the node ([`PriceServiceContext`]); the conflict state is kept per list of sources.
//!
//! # Wasm
//!
//! A browser build also wraps each fetch in the executor's timer for the same 10 s, but no test covers that code
//! yet; the cap is only claimed for native builds. A browser can read only an endpoint that sends CORS headers; of
//! the default endpoints only the GLEEC ones do.

use super::simple_market_maker_bot::PriceServiceRequestError;
use super::{Provider, TickerInfos, TickerInfosRegistry};
use common::log::{debug, info, warn};
use common::mm_number::MmNumber;
use common::StatusCode;
use mm2_core::mm_ctx::{from_ctx, MmArc};
use mm2_err_handle::prelude::*;
use mm2_net::host_label::{host_only, scrub_node_url};
use mm2_net::transport::SlurpResult;
use serde_json::Value as Json;
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, PoisonError};

/// The default price sources, in the order they are combined. The first is the GLEEC price service followed by its
/// cache (the second URL of a source is tried only if the first fails); the second is the cipig price service.
/// `expire_at` is the maximum age, in seconds (72 hours), of the tickers the service serves: without it the cipig
/// service serves tickers many months old.
pub const DEFAULT_PRICE_SOURCES: [&[&str]; 2] = [
    &[
        "https://prices.gleec.com/api/v2/tickers?expire_at=259200",
        "https://defistats.gleec.com/api/v3/prices/tickers_v2?expire_at=259200",
    ],
    &["https://prices.cipig.net:1717/api/v2/tickers?expire_at=259200"],
];
/// The price providers recognized unless `MM2.json` says otherwise (CRD ch.62 R62.5.2).
pub const DEFAULT_PRICE_PROVIDERS: [&str; 5] = ["binance", "coingecko", "coinpaprika", "forex", "livecoinwatch"];
/// The name of the top-level `MM2.json` key that replaces the default price sources.
pub const PRICE_URLS_CONF_KEY: &str = "price_urls";
/// The name of the top-level `MM2.json` key that replaces the default set of recognized price providers.
pub const PRICE_PROVIDERS_CONF_KEY: &str = "price_providers";
/// The provider name the feeds use for a price that could not be sourced; never recognized, even if listed.
const UNKNOWN_PROVIDER: &str = "unknown";
/// Two prices of one ticker agree if the higher exceeds the lower by at most this many percent (CRD ch.62 D62.2).
pub const PRICE_CONFLICT_TOLERANCE_PERCENT: u64 = 5;
/// The cap, in seconds, on one attempt to fetch one URL (CRD ch.62 D62.3).
pub const PRICE_ATTEMPT_TIMEOUT_SECS: f64 = 10.;

/// One price source: URLs tried in order, the first that answers is used.
pub type PriceSource = Vec<String>;

/// The tickers of one document or of the combined table.
pub type PriceTable = HashMap<String, PricedTicker>;

/// A ticker record together with the exact name of its price provider.
#[derive(Debug)]
pub struct PricedTicker {
    pub infos: TickerInfos,
    /// The `price_provider` string as the feed wrote it; the recognized set is tested against it.
    pub provider: String,
}

impl PricedTicker {
    /// The record as the bot's registry holds it. The records of a combined table are recognized, so a provider the
    /// bot's enum does not name is one the operator listed in `price_providers`.
    fn into_registry_infos(self) -> TickerInfos {
        let mut infos = self.infos;
        if infos.price_provider == Provider::Unknown {
            infos.price_provider = Provider::Configured;
        }
        infos
    }
}

#[derive(Deserialize)]
struct ProviderName {
    price_provider: String,
}

/// The default price sources as a list of sources.
pub fn default_price_sources() -> Vec<PriceSource> {
    DEFAULT_PRICE_SOURCES
        .iter()
        .map(|source| source.iter().map(|url| url.to_string()).collect())
        .collect()
}

// ---------------------------------------------------------------------------------------------------------------
// Configuration: the sources and the recognized providers
// ---------------------------------------------------------------------------------------------------------------

fn url_from_json(value: &Json) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(str::to_owned)
}

/// Reads a `price_urls` value: a non-empty array whose items are each a URL string (one source) or a non-empty
/// array of URL strings (one source with in-order fallback). `None` if the value has any other form.
pub fn parse_price_sources(value: &Json) -> Option<Vec<PriceSource>> {
    let items = value.as_array()?;
    if items.is_empty() {
        return None;
    }
    items
        .iter()
        .map(|item| match item {
            Json::String(_) => url_from_json(item).map(|url| vec![url]),
            Json::Array(urls) if !urls.is_empty() => urls.iter().map(url_from_json).collect::<Option<Vec<String>>>(),
            _ => None,
        })
        .collect()
}

/// Reads a comma-separated `price_url` string as one source whose URLs are tried in order. `None` if it holds no URL.
pub fn parse_price_url_string(value: &str) -> Option<PriceSource> {
    let urls: Vec<String> = value
        .split(',')
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(str::to_owned)
        .collect();
    if urls.is_empty() {
        None
    } else {
        Some(urls)
    }
}

/// What the top-level `MM2.json` key `price_urls` holds.
#[derive(Debug, PartialEq)]
pub enum PriceUrlsConf {
    /// The key is absent or `null`.
    Absent,
    /// A well-formed value (see [`parse_price_sources`]).
    Valid(Vec<PriceSource>),
    /// Anything else.
    Invalid,
}

pub fn read_price_urls_conf(conf: &Json) -> PriceUrlsConf {
    match conf.get(PRICE_URLS_CONF_KEY) {
        None | Some(Json::Null) => PriceUrlsConf::Absent,
        Some(value) => parse_price_sources(value).map_or(PriceUrlsConf::Invalid, PriceUrlsConf::Valid),
    }
}

/// The set of price provider names a record may carry to be recognized.
#[derive(Clone, Debug, PartialEq)]
pub struct ProviderSet(BTreeSet<String>);

impl ProviderSet {
    /// The set recognized unless `MM2.json` says otherwise.
    pub fn default_set() -> ProviderSet {
        ProviderSet(DEFAULT_PRICE_PROVIDERS.iter().map(|name| name.to_string()).collect())
    }

    /// Whether `name` is a recognized provider. The literal `unknown` never is.
    pub fn recognizes(&self, name: &str) -> bool { name != UNKNOWN_PROVIDER && self.0.contains(name) }
}

/// What the top-level `MM2.json` key `price_providers` holds.
#[derive(Debug, PartialEq)]
pub enum PriceProvidersConf {
    /// The key is absent or `null`.
    Absent,
    /// A non-empty array of strings. `unknown`, if listed, is left out of the set and `listed_unknown` is set.
    Valid {
        providers: ProviderSet,
        listed_unknown: bool,
    },
    /// Anything else.
    Invalid,
}

/// Reads a `price_providers` value: a non-empty array of strings.
fn parse_price_providers(items: &[Json]) -> Option<PriceProvidersConf> {
    if items.is_empty() {
        return None;
    }
    let names = items.iter().map(Json::as_str).collect::<Option<Vec<&str>>>()?;
    Some(PriceProvidersConf::Valid {
        listed_unknown: names.contains(&UNKNOWN_PROVIDER),
        providers: ProviderSet(
            names
                .into_iter()
                .filter(|name| *name != UNKNOWN_PROVIDER)
                .map(str::to_owned)
                .collect(),
        ),
    })
}

pub fn read_price_providers_conf(conf: &Json) -> PriceProvidersConf {
    match conf.get(PRICE_PROVIDERS_CONF_KEY) {
        None | Some(Json::Null) => PriceProvidersConf::Absent,
        Some(Json::Array(items)) => parse_price_providers(items).unwrap_or(PriceProvidersConf::Invalid),
        Some(_) => PriceProvidersConf::Invalid,
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Warn-once transitions
// ---------------------------------------------------------------------------------------------------------------

/// The state of an endpoint, or of a ticker, as far as the log lines are concerned.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Health {
    /// Nothing has been seen yet.
    #[default]
    Unknown,
    Working,
    Failing,
}

/// What a step of [`health_step`] asks the caller to log.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Notice {
    /// A failure episode starts: one WARN.
    Warn,
    /// The episode goes on: DEBUG only.
    Debug,
    /// The episode ends: one INFO.
    Info,
    /// Nothing worth a line.
    Quiet,
}

/// The transition function of the warn-once rule (CRD ch.62 D62.3): `failed` is the outcome of the latest attempt
/// (for a ticker: whether its sources disagree). Returns the new state and what to log.
pub fn health_step(previous: Health, failed: bool) -> (Health, Notice) {
    match (previous, failed) {
        (Health::Failing, true) => (Health::Failing, Notice::Debug),
        (_, true) => (Health::Failing, Notice::Warn),
        (Health::Failing, false) => (Health::Working, Notice::Info),
        (_, false) => (Health::Working, Notice::Quiet),
    }
}

/// The text of a failure as a log line shows it, cut to 200 characters.
fn failure_reason(failure: &PriceServiceRequestError) -> String { format!("{:?}", failure).chars().take(200).collect() }

/// The key of a list of sources in the per-list conflict state. Only the hash is kept as a key.
fn sources_key(sources: &[PriceSource]) -> u64 {
    let mut hasher = DefaultHasher::new();
    sources.hash(&mut hasher);
    hasher.finish()
}

/// Removes the URL asked for (and the parts of it a server may echo: userinfo, path, query) from the text of an
/// error, so that an answer body or a lower layer's message cannot carry a key of a configured URL into a log.
fn scrub_request_error(error: PriceServiceRequestError, url: &str) -> PriceServiceRequestError {
    match error {
        PriceServiceRequestError::HttpProcessError(text) => {
            PriceServiceRequestError::HttpProcessError(scrub_node_url(&text, url))
        },
        PriceServiceRequestError::ParsingAnswerError(text) => {
            PriceServiceRequestError::ParsingAnswerError(scrub_node_url(&text, url))
        },
        PriceServiceRequestError::Internal(text) => PriceServiceRequestError::Internal(scrub_node_url(&text, url)),
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The node context
// ---------------------------------------------------------------------------------------------------------------

/// The price service of one node: the sources and providers its `MM2.json` selects and the shared warn-once state.
pub struct PriceServiceContext {
    /// The sources every consumer uses unless its own request names others.
    pub sources: Vec<PriceSource>,
    /// The recognized price providers.
    pub providers: ProviderSet,
    endpoints: Mutex<HashMap<String, Health>>,
    /// The tickers in conflict, per list of sources (see [`sources_key`]): a consumer with sources of its own and
    /// one on the sources of the node must not overwrite each other's episodes.
    conflicts: Mutex<HashMap<u64, BTreeSet<String>>>,
    /// The state of the bot's "no source answers" episode (see [`PriceServiceContext::note_no_source`]).
    no_source: Mutex<Health>,
}

impl PriceServiceContext {
    pub fn new(sources: Vec<PriceSource>, providers: ProviderSet) -> PriceServiceContext {
        PriceServiceContext {
            sources,
            providers,
            endpoints: Mutex::new(HashMap::new()),
            conflicts: Mutex::new(HashMap::new()),
            no_source: Mutex::new(Health::Unknown),
        }
    }

    /// Obtains the price service context of the node, creating it from `MM2.json` on first use. A malformed
    /// `price_urls` or `price_providers` is reported once, here, by the name of the key, never its value.
    pub fn from_ctx(ctx: &MmArc) -> Result<Arc<PriceServiceContext>, String> {
        from_ctx(&ctx.price_service_ctx, || Ok(PriceServiceContext::from_conf(&ctx.conf)))
    }

    pub fn from_conf(conf: &Json) -> PriceServiceContext {
        let sources = match read_price_urls_conf(conf) {
            PriceUrlsConf::Valid(sources) => sources,
            PriceUrlsConf::Absent => default_price_sources(),
            PriceUrlsConf::Invalid => {
                warn!(
                    "The MM2.json key '{}' is not a non-empty array of URLs or of arrays of URLs; using the default \
                     price sources",
                    PRICE_URLS_CONF_KEY
                );
                default_price_sources()
            },
        };
        let providers = match read_price_providers_conf(conf) {
            PriceProvidersConf::Valid {
                providers,
                listed_unknown,
            } => {
                if listed_unknown {
                    warn!(
                        "The MM2.json key '{}' lists '{}'; that name is never recognized",
                        PRICE_PROVIDERS_CONF_KEY, UNKNOWN_PROVIDER
                    );
                }
                providers
            },
            PriceProvidersConf::Absent => ProviderSet::default_set(),
            PriceProvidersConf::Invalid => {
                warn!(
                    "The MM2.json key '{}' is not a non-empty array of strings; using the default price providers",
                    PRICE_PROVIDERS_CONF_KEY
                );
                ProviderSet::default_set()
            },
        };
        PriceServiceContext::new(sources, providers)
    }

    /// Records the outcome of an attempt on the endpoint with the host label `host` (`failure` is `None` if it
    /// answered), logs the line [`health_step`] asks for and returns what was asked.
    fn note_endpoint(&self, host: &str, failure: Option<&PriceServiceRequestError>) -> Notice {
        let notice = {
            let mut endpoints = self.endpoints.lock().unwrap_or_else(PoisonError::into_inner);
            let state = endpoints.entry(host.to_owned()).or_default();
            let (next, notice) = health_step(*state, failure.is_some());
            *state = next;
            notice
        };
        let reason: String = failure.map(failure_reason).unwrap_or_default();
        match notice {
            Notice::Warn => warn!("Price service {} could not be used: {}", host, reason),
            Notice::Debug => debug!("Price service {} still fails: {}", host, reason),
            Notice::Info => info!("Price service {} answers again", host),
            Notice::Quiet => (),
        }
        notice
    }

    /// Records whether the bot's latest fetch found no source answering (`failed`) and returns what [`health_step`]
    /// asks for. The INFO line of the end of an episode is logged here; the line that starts an episode (an ERROR,
    /// with the number of cancelled orders) and the DEBUG lines are the bot's, which knows the count.
    pub fn note_no_source(&self, failed: bool) -> Notice {
        let notice = {
            let mut state = self.no_source.lock().unwrap_or_else(PoisonError::into_inner);
            let (next, notice) = health_step(*state, failed);
            *state = next;
            notice
        };
        if notice == Notice::Info {
            info!("A price source answers again");
        }
        notice
    }

    /// Records which tickers are in conflict in `combined`, a combination of the `sources`, logs the lines
    /// [`health_step`] asks for and returns, per ticker, what was asked.
    fn note_conflicts(&self, sources: &[PriceSource], combined: &Combined) -> Vec<(String, Notice)> {
        let mut notices: Vec<(String, Notice, String)> = Vec::new();
        {
            let mut all_previous = self.conflicts.lock().unwrap_or_else(PoisonError::into_inner);
            let previous = all_previous.entry(sources_key(sources)).or_default();
            let mut current = BTreeSet::new();
            for conflict in &combined.conflicts {
                let was = if previous.contains(&conflict.ticker) {
                    Health::Failing
                } else {
                    Health::Working
                };
                let (_, notice) = health_step(was, true);
                current.insert(conflict.ticker.clone());
                notices.push((conflict.ticker.clone(), notice, conflict.describe()));
            }
            for ticker in previous.iter().filter(|ticker| !current.contains(*ticker)) {
                let (_, notice) = health_step(Health::Failing, false);
                // A ticker that vanished from both sources has not "agreed again".
                if combined.table.contains_key(ticker) {
                    notices.push((
                        ticker.clone(),
                        notice,
                        format!("The sources agree again on the price of {}", ticker),
                    ));
                }
            }
            *previous = current;
        }
        notices
            .into_iter()
            .map(|(ticker, notice, line)| {
                match notice {
                    Notice::Warn => warn!("{}", line),
                    Notice::Debug => debug!("{}", line),
                    Notice::Info => info!("{}", line),
                    Notice::Quiet => (),
                }
                (ticker, notice)
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Combining the sources
// ---------------------------------------------------------------------------------------------------------------

/// The answer of one source: the host label of the URL that answered and its tickers.
#[derive(Debug)]
pub struct SourceTable {
    pub host: String,
    pub records: PriceTable,
}

/// One side of a conflict: a source's price of the ticker.
#[derive(Debug)]
pub struct ConflictSide {
    pub host: String,
    pub price: MmNumber,
    pub provider: String,
}

/// A ticker whose sources disagree by more than [`PRICE_CONFLICT_TOLERANCE_PERCENT`]: its lowest and highest price.
#[derive(Debug)]
pub struct Conflict {
    pub ticker: String,
    pub low: ConflictSide,
    pub high: ConflictSide,
}

impl Conflict {
    fn describe(&self) -> String {
        format!(
            "The price sources disagree on {} by more than {} %: {} ({}, from {}) against {} ({}, from {}); the \
             ticker is not priced",
            self.ticker,
            PRICE_CONFLICT_TOLERANCE_PERCENT,
            self.low.price,
            self.low.provider,
            self.low.host,
            self.high.price,
            self.high.provider,
            self.high.host,
        )
    }
}

/// The result of [`combine_sources`].
#[derive(Debug, Default)]
pub struct Combined {
    pub table: PriceTable,
    pub conflicts: Vec<Conflict>,
}

fn price_at(records: &[(String, PricedTicker)], index: usize) -> &MmNumber { &records[index].1.infos.last_price }

fn within_tolerance(low: &MmNumber, high: &MmNumber) -> bool {
    let spread = (high.clone() - low.clone()) * MmNumber::from(100u64);
    spread <= low.clone() * MmNumber::from(PRICE_CONFLICT_TOLERANCE_PERCENT)
}

/// Combines the answers of the sources (given in source order) into one table (CRD ch.62 D62.2).
///
/// 1. A record is dropped if its provider is not recognized under `providers` or its timestamp is zero.
/// 2. A ticker in one source is kept.
/// 3. A ticker in several sources whose prices differ by at most [`PRICE_CONFLICT_TOLERANCE_PERCENT`] of the lowest
///    is kept from the source with the newest timestamp; on a tie, from the earlier source.
/// 4. Any other ticker is a conflict and is absent from the table.
pub fn combine_sources(sources: Vec<SourceTable>, providers: &ProviderSet) -> Combined {
    let mut candidates: BTreeMap<String, Vec<(String, PricedTicker)>> = BTreeMap::new();
    for source in sources {
        for (ticker, record) in source.records {
            if !providers.recognizes(&record.provider) || record.infos.last_updated_timestamp == 0 {
                continue;
            }
            candidates
                .entry(ticker)
                .or_default()
                .push((source.host.clone(), record));
        }
    }

    let mut combined = Combined::default();
    for (ticker, mut records) in candidates {
        if records.len() == 1 {
            if let Some((_, record)) = records.pop() {
                combined.table.insert(ticker, record);
            }
            continue;
        }
        let low = (0..records.len()).min_by(|a, b| price_at(&records, *a).cmp(price_at(&records, *b)));
        let high = (0..records.len()).max_by(|a, b| price_at(&records, *a).cmp(price_at(&records, *b)));
        let (Some(low), Some(high)) = (low, high) else {
            continue;
        };
        if within_tolerance(price_at(&records, low), price_at(&records, high)) {
            let mut winner = 0;
            for (index, (_, record)) in records.iter().enumerate() {
                if record.infos.last_updated_timestamp > records[winner].1.infos.last_updated_timestamp {
                    winner = index;
                }
            }
            let (_, record) = records.swap_remove(winner);
            combined.table.insert(ticker, record);
        } else {
            let side = |index: usize| ConflictSide {
                host: records[index].0.clone(),
                price: records[index].1.infos.last_price.clone(),
                provider: records[index].1.provider.clone(),
            };
            let conflict = Conflict {
                low: side(low),
                high: side(high),
                ticker,
            };
            combined.conflicts.push(conflict);
        }
    }
    combined
}

/// The key under which `table` holds `ticker` (CRD ch.62 R62.4.3): the ticker itself or, failing that, its part
/// before the first `-` (`USDT-PLG20` -> `USDT`). A ticker whose only hyphen is its last character has no such part.
pub fn ticker_key<'a, T>(table: &HashMap<String, T>, ticker: &'a str) -> Option<&'a str> {
    if table.contains_key(ticker) {
        return Some(ticker);
    }
    let limit = ticker.len().saturating_sub(1);
    let pos = ticker.find('-').unwrap_or(limit);
    if pos < limit && table.contains_key(&ticker[0..pos]) {
        return Some(&ticker[0..pos]);
    }
    None
}

/// The USD prices of the `base` and `rel` coins (CRD ch.62 R62.5.1): both tickers resolve under [`ticker_key`] and the
/// `rel` price is not zero. The records of a combined table are recognized and have a timestamp by construction.
/// A zero `base` price is a defined result.
pub fn pair_usd_prices(table: &PriceTable, base: &str, rel: &str) -> Option<(MmNumber, MmNumber)> {
    let base_price = &table.get(ticker_key(table, base)?)?.infos.last_price;
    let rel_price = &table.get(ticker_key(table, rel)?)?.infos.last_price;
    if rel_price.is_zero() {
        return None;
    }
    Some((base_price.clone(), rel_price.clone()))
}

/// The bot's registry of a combined table.
pub fn registry_from_table(table: PriceTable) -> TickerInfosRegistry {
    TickerInfosRegistry(
        table
            .into_iter()
            .map(|(ticker, record)| (ticker, record.into_registry_infos()))
            .collect(),
    )
}

// ---------------------------------------------------------------------------------------------------------------
// Fetching
// ---------------------------------------------------------------------------------------------------------------

/// Reads one price document: a JSON object of ticker -> record (CRD ch.62 R62.4.1).
pub fn parse_price_document(body: &str) -> Result<PriceTable, MmError<PriceServiceRequestError>> {
    let records: HashMap<String, TickerInfos> = serde_json::from_str(body)?;
    let mut names: HashMap<String, ProviderName> = serde_json::from_str(body)?;
    let mut table = PriceTable::with_capacity(records.len());
    for (ticker, infos) in records {
        let Some(name) = names.remove(&ticker) else {
            return MmError::err(PriceServiceRequestError::ParsingAnswerError(
                "a ticker record has no price_provider".to_string(),
            ));
        };
        table.insert(ticker, PricedTicker {
            infos,
            provider: name.price_provider,
        });
    }
    Ok(table)
}

#[cfg(not(target_arch = "wasm32"))]
async fn slurp_price_url(url: &str) -> SlurpResult {
    let request = http::Request::builder().uri(url).body(Vec::new())?;
    mm2_net::transport::slurp_req_bounded(request, PRICE_ATTEMPT_TIMEOUT_SECS).await
}

#[cfg(target_arch = "wasm32")]
async fn slurp_price_url(url: &str) -> SlurpResult {
    use common::custom_futures::FutureTimerExt;

    match Box::pin(mm2_net::transport::slurp_url(url))
        .timeout_secs(PRICE_ATTEMPT_TIMEOUT_SECS)
        .await
    {
        Ok(answer) => answer,
        Err(err) => MmError::err(mm2_net::transport::SlurpError::Timeout {
            uri: host_only(url),
            error: err.to_string(),
        }),
    }
}

/// Fetches and reads one price document from `url`. The text of an error never contains the URL, a server's
/// echo of it included (see [`scrub_request_error`]).
pub async fn request_price_table(url: &str) -> Result<PriceTable, MmError<PriceServiceRequestError>> {
    request_unscrubbed_price_table(url)
        .await
        .mm_err(|error| scrub_request_error(error, url))
}

async fn request_unscrubbed_price_table(url: &str) -> Result<PriceTable, MmError<PriceServiceRequestError>> {
    debug!("Fetching price from: {}", host_only(url));
    let (status, _headers, body) = slurp_price_url(url).await.mm_err(Into::into)?;
    let body: String = std::str::from_utf8(&body)?.trim().into();
    if status != StatusCode::OK {
        return MmError::err(PriceServiceRequestError::HttpProcessError(body));
    }
    parse_price_document(&body)
}

/// Fetches one source: its URLs in order; the first that answers is the answer.
async fn fetch_source(
    ctx: &PriceServiceContext,
    source: &[String],
) -> Result<SourceTable, MmError<PriceServiceRequestError>> {
    let mut last_error: Option<MmError<PriceServiceRequestError>> = None;
    for url in source {
        let host = host_only(url);
        match request_price_table(url).await {
            Ok(records) => {
                ctx.note_endpoint(&host, None);
                return Ok(SourceTable { host, records });
            },
            Err(err) => {
                ctx.note_endpoint(&host, Some(err.get_inner()));
                last_error = Some(err);
            },
        }
    }
    match last_error {
        Some(err) => Err(err),
        None => MmError::err(PriceServiceRequestError::Internal(
            "a price source has no URL".to_string(),
        )),
    }
}

/// Fetches all `sources` in parallel and combines the answers (see [`combine_sources`]).
///
/// A source that fails is left out; the others are used alone. An error is returned only if no source answered.
pub async fn fetch_combined(
    ctx: &PriceServiceContext,
    sources: &[PriceSource],
) -> Result<PriceTable, MmError<PriceServiceRequestError>> {
    if sources.is_empty() {
        return MmError::err(PriceServiceRequestError::Internal("no price source".to_string()));
    }
    let answers = futures::future::join_all(sources.iter().map(|source| fetch_source(ctx, source))).await;

    let mut tables = Vec::with_capacity(answers.len());
    let mut last_error: Option<MmError<PriceServiceRequestError>> = None;
    for answer in answers {
        match answer {
            Ok(table) => tables.push(table),
            Err(err) => last_error = Some(err),
        }
    }
    if tables.is_empty() {
        return match last_error {
            Some(err) => Err(err),
            None => MmError::err(PriceServiceRequestError::Internal("no price source".to_string())),
        };
    }

    let combined = combine_sources(tables, &ctx.providers);
    ctx.note_conflicts(sources, &combined);
    debug!("price registry size: {}", combined.table.len());
    Ok(combined.table)
}

/// A loopback HTTP server for the tests: it answers every request with a canned status line and body.
#[cfg(all(test, not(target_arch = "wasm32")))]
pub mod stub_server {
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    pub const OK: &str = "HTTP/1.1 200 OK";
    pub const SERVER_ERROR: &str = "HTTP/1.1 500 Internal Server Error";
    pub const NOT_FOUND: &str = "HTTP/1.1 404 Not Found";

    /// Starts the server. Returns the URL of its endpoint and the count of the requests it has received.
    pub fn serve(status_line: &'static str, body: impl Into<String>) -> (String, Arc<AtomicUsize>) {
        let body: String = body.into();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        std::thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                let mut request = [0u8; 4096];
                let _ = stream.read(&mut request);
                counter.fetch_add(1, Ordering::SeqCst);
                let answer = format!(
                    "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(answer.as_bytes());
            }
        });
        (format!("http://127.0.0.1:{port}/api/v2/tickers?expire_at=259200"), hits)
    }

    /// Starts a server that answers every request with `status_line` and a body that echoes the request: the
    /// request path with its query, and the full URL it was reached by. Returns its port and the request count.
    pub fn serve_echoing(status_line: &'static str) -> (u16, Arc<AtomicUsize>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        std::thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                let mut request = [0u8; 4096];
                let read = stream.read(&mut request).unwrap_or(0);
                counter.fetch_add(1, Ordering::SeqCst);
                let text = String::from_utf8_lossy(&request[..read]).to_string();
                let path = text.split_whitespace().nth(1).unwrap_or("/").to_string();
                let body = format!("Cannot GET {path} (requested as http://127.0.0.1:{port}{path})");
                let answer = format!(
                    "{status_line}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(answer.as_bytes());
            }
        });
        (port, hits)
    }

    /// The URL of an endpoint nobody listens on: a request to it fails at once.
    pub fn dead_url() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        format!("http://127.0.0.1:{port}/api/v2/tickers?expire_at=259200")
    }

    /// A price document with one full record per `(ticker, price, provider, timestamp)`.
    pub fn price_document(entries: &[(&str, &str, &str, u64)]) -> String {
        let records: Vec<String> = entries
            .iter()
            .map(|(ticker, price, provider, timestamp)| {
                format!(
                    r#""{ticker}":{{"ticker":"{ticker}","last_price":"{price}","last_updated":"2026-11-07T15:43:01Z","last_updated_timestamp":{timestamp},"volume24h":"1.0","price_provider":"{provider}","volume_provider":"{provider}","sparkline_7d":null,"sparkline_provider":"unknown","change_24h":"1.0","change_24h_provider":"{provider}"}}"#
                )
            })
            .collect();
        format!("{{{}}}", records.join(","))
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "price_service_tests.rs"]
mod price_service_tests;
