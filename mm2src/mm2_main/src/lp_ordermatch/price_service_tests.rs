//! Tests of the price service (CRD ch.62): the saved answers of both default price services, the combination of the
//! sources, the `MM2.json` keys, the recognized providers, the warn-once transitions and the fetch. The services
//! are loopback servers that answer canned texts; no test reaches the network.

use super::stub_server::{dead_url, price_document, serve, serve_echoing, NOT_FOUND, OK, SERVER_ERROR};
use super::*;
use common::block_on;
use std::sync::atomic::Ordering;

/// Trimmed answers of the two default price services, saved on 2026-11-07.
const DEFISTATS_ANSWER: &str = include_str!("../for_tests/price_defistats_tickers_v2.json");
const CIPIG_ANSWER: &str = include_str!("../for_tests/price_cipig_tickers.json");

fn source_table(host: &str, entries: &[(&str, &str, &str, u64)]) -> SourceTable {
    SourceTable {
        host: host.to_string(),
        records: parse_price_document(&price_document(entries)).unwrap(),
    }
}

fn combine(tables: Vec<SourceTable>) -> Combined { combine_sources(tables, &ProviderSet::default_set()) }

fn price_in(combined: &Combined, ticker: &str) -> MmNumber { combined.table[ticker].infos.last_price.clone() }

fn context(sources: Vec<PriceSource>) -> PriceServiceContext {
    PriceServiceContext::new(sources, ProviderSet::default_set())
}

fn http_failure() -> PriceServiceRequestError { PriceServiceRequestError::HttpProcessError("boom".to_string()) }

// ---------------------------------------------------------------------------------------------------------------
// The saved answers
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn default_sources_are_the_gleec_source_with_its_cache_and_the_cipig_source() {
    assert_eq!(default_price_sources(), vec![
        vec![
            "https://prices.gleec.com/api/v2/tickers?expire_at=259200".to_string(),
            "https://defistats.gleec.com/api/v3/prices/tickers_v2?expire_at=259200".to_string(),
        ],
        vec!["https://prices.cipig.net:1717/api/v2/tickers?expire_at=259200".to_string()],
    ]);
}

#[test]
fn the_saved_answers_parse() {
    let defistats = parse_price_document(DEFISTATS_ANSWER).unwrap();
    for ticker in ["RIN", "KMD", "BTC", "MATIC", "1INCH"] {
        assert!(defistats.contains_key(ticker), "{ticker} is missing");
    }
    let rin = &defistats["RIN"];
    assert_eq!(rin.provider, "livecoinwatch");
    assert_eq!(rin.infos.price_provider, Provider::Livecoinwatch);
    assert!(rin.infos.sparkline_7_d.is_none());
    assert_eq!(rin.infos.last_price, MmNumber::from("0.0004366169"));
    assert_eq!(rin.infos.last_updated_timestamp, 1794066181);

    let cipig = parse_price_document(CIPIG_ANSWER).unwrap();
    for ticker in ["RIN", "KMD", "BTC", "MON", "POL", "1INCH"] {
        assert!(cipig.contains_key(ticker), "{ticker} is missing");
    }
    assert_eq!(cipig["MON"].provider, "coinpaprika");
    assert_eq!(cipig["MON"].infos.last_price, MmNumber::from("0.016405422"));
}

#[test]
fn a_malformed_document_is_refused() {
    assert!(parse_price_document("not json").is_err());
    assert!(parse_price_document("[]").is_err());
    // One incomplete record fails the whole document (CRD ch.62 R62.4.1).
    let incomplete = r#"{"RIN":{"ticker":"RIN","last_price":"1"}}"#;
    assert!(parse_price_document(incomplete).is_err());
}

#[test]
fn the_saved_answers_combine_without_a_conflict() {
    let combined = combine(vec![
        SourceTable {
            host: "defistats".to_string(),
            records: parse_price_document(DEFISTATS_ANSWER).unwrap(),
        },
        SourceTable {
            host: "cipig".to_string(),
            records: parse_price_document(CIPIG_ANSWER).unwrap(),
        },
    ]);
    assert!(combined.conflicts.is_empty());
    // In both sources and in agreement: the newer record (cipig's) wins.
    assert_eq!(price_in(&combined, "RIN"), MmNumber::from("0.0004324538"));
    assert_eq!(price_in(&combined, "KMD"), MmNumber::from("0.0067748"));
    assert_eq!(price_in(&combined, "1INCH"), MmNumber::from("0.0797"));
    // In one source only: kept as it is.
    assert_eq!(price_in(&combined, "MATIC"), MmNumber::from("0.0909358654"));
    assert_eq!(price_in(&combined, "MON"), MmNumber::from("0.016405422"));
    assert_eq!(price_in(&combined, "POL"), MmNumber::from("0.09061"));
}

// ---------------------------------------------------------------------------------------------------------------
// Combining (CRD ch.62 D62.2)
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn within_five_percent_the_newer_record_wins() {
    let combined = combine(vec![
        source_table("a", &[("BTC", "100", "binance", 10)]),
        source_table("b", &[("BTC", "104.9", "coingecko", 20)]),
    ]);
    assert!(combined.conflicts.is_empty());
    assert_eq!(price_in(&combined, "BTC"), MmNumber::from("104.9"));
    assert_eq!(combined.table["BTC"].provider, "coingecko");

    let combined = combine(vec![
        source_table("a", &[("BTC", "100", "binance", 30)]),
        source_table("b", &[("BTC", "104.9", "coingecko", 20)]),
    ]);
    assert_eq!(price_in(&combined, "BTC"), MmNumber::from("100"));
}

#[test]
fn exactly_five_percent_agrees_and_beyond_it_is_a_conflict() {
    let combined = combine(vec![
        source_table("a", &[("BTC", "100", "binance", 10)]),
        source_table("b", &[("BTC", "105", "binance", 20)]),
    ]);
    assert!(combined.conflicts.is_empty());
    assert_eq!(price_in(&combined, "BTC"), MmNumber::from("105"));

    let combined = combine(vec![
        source_table("a", &[("BTC", "100", "binance", 10)]),
        source_table("b", &[("BTC", "105.01", "binance", 20)]),
    ]);
    assert!(!combined.table.contains_key("BTC"));
    assert_eq!(combined.conflicts.len(), 1);
    let conflict = &combined.conflicts[0];
    assert_eq!(conflict.ticker, "BTC");
    assert_eq!(
        (conflict.low.host.as_str(), conflict.low.price.clone()),
        ("a", MmNumber::from("100"))
    );
    assert_eq!(
        (conflict.high.host.as_str(), conflict.high.price.clone()),
        ("b", MmNumber::from("105.01"))
    );
}

#[test]
fn a_tie_on_the_timestamp_keeps_the_earlier_source() {
    let combined = combine(vec![
        source_table("a", &[("BTC", "100", "binance", 10)]),
        source_table("b", &[("BTC", "101", "coingecko", 10)]),
    ]);
    assert_eq!(price_in(&combined, "BTC"), MmNumber::from("100"));
}

#[test]
fn a_ticker_of_one_source_is_kept() {
    let combined = combine(vec![
        source_table("a", &[("BTC", "100", "binance", 10), ("KMD", "0.25", "coingecko", 10)]),
        source_table("b", &[("BTC", "100", "binance", 10)]),
    ]);
    assert_eq!(price_in(&combined, "KMD"), MmNumber::from("0.25"));
    assert!(combined.conflicts.is_empty());
}

#[test]
fn unrecognized_and_zero_timestamp_records_are_dropped_before_combining() {
    let combined = combine(vec![
        source_table("a", &[
            ("AAA", "100", "gleeccex", 10),
            ("BBB", "100", "unknown", 10),
            ("CCC", "100", "coinmarketcap", 10),
            ("DDD", "100", "binance", 0),
        ]),
        source_table("b", &[
            ("AAA", "300", "binance", 10),
            ("BBB", "300", "binance", 10),
            ("CCC", "300", "binance", 10),
            ("DDD", "300", "binance", 10),
        ]),
    ]);
    // The dropped record does not take part, so there is no conflict with the other source's record.
    assert!(combined.conflicts.is_empty());
    for ticker in ["AAA", "BBB", "CCC", "DDD"] {
        assert_eq!(price_in(&combined, ticker), MmNumber::from("300"), "{ticker}");
    }
    // Dropped everywhere: absent.
    let combined = combine(vec![source_table("a", &[("AAA", "100", "gleeccex", 10)])]);
    assert!(combined.table.is_empty() && combined.conflicts.is_empty());
}

/// The KMD case of 2026-11-07: one source priced it from coingecko, the other from coinpaprika at half the value.
#[test]
fn two_providers_a_factor_two_apart_are_a_conflict() {
    let combined = combine(vec![
        source_table("gleec", &[("KMD", "0.00678", "coingecko", 10)]),
        source_table("cipig", &[("KMD", "0.00313", "coinpaprika", 20)]),
    ]);
    assert!(!combined.table.contains_key("KMD"));
    assert_eq!(combined.conflicts.len(), 1);
    assert_eq!(combined.conflicts[0].low.provider, "coinpaprika");
    assert_eq!(combined.conflicts[0].high.provider, "coingecko");
}

#[test]
fn with_three_sources_the_widest_pair_decides() {
    let agree = combine(vec![
        source_table("a", &[("BTC", "100", "binance", 10)]),
        source_table("b", &[("BTC", "102", "binance", 30)]),
        source_table("c", &[("BTC", "104", "binance", 20)]),
    ]);
    assert_eq!(price_in(&agree, "BTC"), MmNumber::from("102"));
    let disagree = combine(vec![
        source_table("a", &[("BTC", "100", "binance", 10)]),
        source_table("b", &[("BTC", "102", "binance", 30)]),
        source_table("c", &[("BTC", "110", "binance", 20)]),
    ]);
    assert!(!disagree.table.contains_key("BTC"));
    assert_eq!(disagree.conflicts.len(), 1);
}

// ---------------------------------------------------------------------------------------------------------------
// Lookup and pair prices (CRD ch.62 R62.4.3, R62.5.1)
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn a_token_ticker_is_looked_up_by_its_asset_when_the_exact_key_is_missing() {
    let combined = combine(vec![source_table("a", &[
        ("USDT", "1.0", "binance", 10),
        ("KMD", "0.25", "coingecko", 10),
        ("KMD-BEP20", "0.26", "coingecko", 10),
    ])]);
    assert_eq!(ticker_key(&combined.table, "USDT"), Some("USDT"));
    assert_eq!(ticker_key(&combined.table, "USDT-PLG20"), Some("USDT"));
    assert_eq!(ticker_key(&combined.table, "KMD-BEP20"), Some("KMD-BEP20"));
    assert_eq!(ticker_key(&combined.table, "KMD-ERC20"), Some("KMD"));
    assert_eq!(ticker_key(&combined.table, "NOSUCH-ERC20"), None);
    assert_eq!(ticker_key(&combined.table, "USDT-"), None);
    assert_eq!(ticker_key(&combined.table, ""), None);
}

#[test]
fn the_pair_prices_need_both_coins_and_a_non_zero_rel_price() {
    let combined = combine(vec![source_table("a", &[
        ("RIN", "0.02", "livecoinwatch", 10),
        ("KMD", "0.25", "coingecko", 10),
        ("ZERO", "0", "binance", 10),
    ])]);
    let (rin, kmd) = pair_usd_prices(&combined.table, "RIN", "KMD").unwrap();
    assert_eq!((rin, kmd), (MmNumber::from("0.02"), MmNumber::from("0.25")));
    assert!(pair_usd_prices(&combined.table, "RIN", "NOSUCH").is_none());
    assert!(pair_usd_prices(&combined.table, "RIN", "ZERO").is_none());
    // A zero base price is a defined result.
    assert!(pair_usd_prices(&combined.table, "ZERO", "RIN").is_some());
}

/// R62.5.1 (d) in the bot: a zero rel price, an unrecognized provider and a zero timestamp give no rate (and no
/// division by zero).
#[test]
fn the_bot_gets_no_rate_for_a_refused_pair() {
    let document = |rel_price: &str, provider: &str, timestamp: u64| {
        let source = SourceTable {
            host: "a".to_string(),
            records: parse_price_document(&price_document(&[
                ("RIN", "0.02", "livecoinwatch", 10),
                ("KMD", rel_price, provider, timestamp),
            ]))
            .unwrap(),
        };
        registry_from_table(combine(vec![source]).table)
    };
    let rate = |registry: TickerInfosRegistry| registry.get_cex_rates("RIN".to_string(), "KMD".to_string());
    assert!(rate(document("0.25", "coingecko", 10)).is_some());
    assert!(rate(document("0", "coingecko", 10)).is_none());
    assert!(rate(document("0.25", "gleeccex", 10)).is_none());
    assert!(rate(document("0.25", "coingecko", 0)).is_none());
}

// ---------------------------------------------------------------------------------------------------------------
// The MM2.json keys (CRD ch.62 D62.1, D62.4)
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn without_the_price_urls_key_the_default_sources_are_used() {
    for conf in [json!({}), json!({"netid": 8762}), json!({"price_urls": null})] {
        assert_eq!(read_price_urls_conf(&conf), PriceUrlsConf::Absent, "{conf}");
        assert_eq!(
            PriceServiceContext::from_conf(&conf).sources,
            default_price_sources(),
            "{conf}"
        );
    }
}

#[test]
fn the_price_urls_key_replaces_the_default_sources() {
    let conf = json!({"price_urls": ["https://a.example/t", [" https://b.example/t ", "https://c.example/t"]]});
    let expected = vec![vec!["https://a.example/t".to_string()], vec![
        "https://b.example/t".to_string(),
        "https://c.example/t".to_string(),
    ]];
    assert_eq!(read_price_urls_conf(&conf), PriceUrlsConf::Valid(expected.clone()));
    // A replacement, not an addition: none of the default URLs remains.
    assert_eq!(PriceServiceContext::from_conf(&conf).sources, expected);
}

#[test]
fn a_malformed_price_urls_key_gives_the_default_sources() {
    let malformed_values = [
        json!("https://a.example/t"),
        json!([]),
        json!([5]),
        json!([[]]),
        json!([[5]]),
        json!([["https://a.example/t", 5]]),
        json!([""]),
        json!({}),
        json!(5),
    ];
    for malformed in malformed_values {
        let conf = json!({ "price_urls": malformed });
        assert_eq!(read_price_urls_conf(&conf), PriceUrlsConf::Invalid, "{conf}");
        assert_eq!(
            PriceServiceContext::from_conf(&conf).sources,
            default_price_sources(),
            "{conf}"
        );
    }
}

#[test]
fn a_price_url_string_is_one_source_in_order() {
    assert_eq!(
        parse_price_url_string(" https://a.example/t ,, https://b.example/t "),
        Some(vec![
            "https://a.example/t".to_string(),
            "https://b.example/t".to_string()
        ])
    );
    assert_eq!(parse_price_url_string(""), None);
    assert_eq!(parse_price_url_string(" , "), None);
}

#[test]
fn the_default_providers_accept_livecoinwatch_and_refuse_coinmarketcap() {
    let providers = ProviderSet::default_set();
    for accepted in ["binance", "coingecko", "coinpaprika", "forex", "livecoinwatch"] {
        assert!(providers.recognizes(accepted), "{accepted}");
    }
    for refused in ["coinmarketcap", "unknown", "gleeccex", "", "Binance"] {
        assert!(!providers.recognizes(refused), "{refused:?}");
    }
}

#[test]
fn the_price_providers_key_replaces_the_set() {
    let conf = json!({"price_providers": ["binance", "gleeccex"]});
    let ctx = PriceServiceContext::from_conf(&conf);
    assert!(ctx.providers.recognizes("gleeccex") && ctx.providers.recognizes("binance"));
    assert!(
        !ctx.providers.recognizes("livecoinwatch"),
        "a replacement, not an addition"
    );
    for conf in [json!({}), json!({"price_providers": null})] {
        assert_eq!(read_price_providers_conf(&conf), PriceProvidersConf::Absent);
        assert_eq!(
            PriceServiceContext::from_conf(&conf).providers,
            ProviderSet::default_set()
        );
    }
}

#[test]
fn a_malformed_price_providers_key_gives_the_default_set() {
    for malformed in [
        json!("binance"),
        json!([]),
        json!([5]),
        json!(["binance", 5]),
        json!({}),
    ] {
        let conf = json!({ "price_providers": malformed });
        assert_eq!(read_price_providers_conf(&conf), PriceProvidersConf::Invalid, "{conf}");
        assert_eq!(
            PriceServiceContext::from_conf(&conf).providers,
            ProviderSet::default_set(),
            "{conf}"
        );
    }
}

#[test]
fn unknown_stays_unrecognized_even_when_listed() {
    let conf = json!({"price_providers": ["unknown", "forex"]});
    match read_price_providers_conf(&conf) {
        PriceProvidersConf::Valid {
            providers,
            listed_unknown,
        } => {
            assert!(listed_unknown);
            assert!(!providers.recognizes("unknown"));
            assert!(providers.recognizes("forex"));
        },
        other => panic!("unexpected {other:?}"),
    }
    let ctx = PriceServiceContext::from_conf(&conf);
    assert!(!ctx.providers.recognizes("unknown"));
}

/// A wrong list disables pricing for the tickers whose providers it leaves out.
#[test]
fn a_wrong_provider_list_disables_pricing() {
    let providers = ProviderSet(["nosuchprovider".to_string()].into_iter().collect());
    let combined = combine_sources(
        vec![source_table("a", &[
            ("BTC", "100", "binance", 10),
            ("KMD", "0.25", "coingecko", 10),
        ])],
        &providers,
    );
    assert!(combined.table.is_empty());
}

// ---------------------------------------------------------------------------------------------------------------
// Warn-once (CRD ch.62 D62.3)
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn a_failure_warns_once_logs_debug_while_it_lasts_and_an_info_on_recovery() {
    let mut state = Health::Unknown;
    let mut notices = vec![];
    for failed in [true, true, true, false, false, true, false] {
        let (next, notice) = health_step(state, failed);
        state = next;
        notices.push(notice);
    }
    assert_eq!(notices, [
        Notice::Warn,
        Notice::Debug,
        Notice::Debug,
        Notice::Info,
        Notice::Quiet,
        Notice::Warn,
        Notice::Info,
    ]);
    // A node that sees an endpoint working first says nothing; a working endpoint that fails warns.
    assert_eq!(health_step(Health::Unknown, false), (Health::Working, Notice::Quiet));
    assert_eq!(health_step(Health::Working, true), (Health::Failing, Notice::Warn));
}

#[test]
fn the_endpoint_state_is_per_host_and_shared_by_every_consumer() {
    let ctx = context(vec![]);
    let failure = http_failure();
    assert_eq!(ctx.note_endpoint("http://a.example", Some(&failure)), Notice::Warn);
    assert_eq!(ctx.note_endpoint("http://a.example", Some(&failure)), Notice::Debug);
    // Another host has its own episode.
    assert_eq!(ctx.note_endpoint("http://b.example", Some(&failure)), Notice::Warn);
    assert_eq!(ctx.note_endpoint("http://a.example", Some(&failure)), Notice::Debug);
    assert_eq!(ctx.note_endpoint("http://a.example", None), Notice::Info);
    assert_eq!(ctx.note_endpoint("http://a.example", None), Notice::Quiet);
    assert_eq!(ctx.note_endpoint("http://a.example", Some(&failure)), Notice::Warn);
    assert_eq!(ctx.note_endpoint("http://c.example", None), Notice::Quiet);
}

#[test]
fn a_ticker_conflict_warns_once_and_says_when_the_prices_agree_again() {
    let ctx = context(vec![]);
    let conflicting = || {
        combine(vec![
            source_table("a", &[("KMD", "0.00678", "coingecko", 10)]),
            source_table("b", &[("KMD", "0.00313", "coinpaprika", 20)]),
        ])
    };
    let agreeing = || {
        combine(vec![
            source_table("a", &[("KMD", "0.00678", "coingecko", 10)]),
            source_table("b", &[("KMD", "0.00679", "coingecko", 20)]),
        ])
    };
    let kmd = |notice| vec![("KMD".to_string(), notice)];
    assert_eq!(ctx.note_conflicts(&[], &conflicting()), kmd(Notice::Warn));
    assert_eq!(ctx.note_conflicts(&[], &conflicting()), kmd(Notice::Debug));
    assert_eq!(ctx.note_conflicts(&[], &agreeing()), kmd(Notice::Info));
    assert_eq!(ctx.note_conflicts(&[], &agreeing()), vec![]);
    assert_eq!(ctx.note_conflicts(&[], &conflicting()), kmd(Notice::Warn));
    // A ticker that vanished from both sources has not "agreed again".
    assert_eq!(ctx.note_conflicts(&[], &combine(vec![])), vec![]);
    assert_eq!(ctx.note_conflicts(&[], &conflicting()), kmd(Notice::Warn));
}

/// The bot's "no source answers" episode: an ERROR when it starts (the bot's), DEBUG while it lasts and an INFO when
/// a source answers again (CRD ch.62 D62.3).
#[test]
fn the_no_source_episode_follows_the_warn_once_rule() {
    let ctx = context(vec![]);
    let notices: Vec<Notice> = [false, true, true, true, false, false, true]
        .into_iter()
        .map(|failed| ctx.note_no_source(failed))
        .collect();
    assert_eq!(notices, [
        Notice::Quiet,
        Notice::Warn,
        Notice::Debug,
        Notice::Debug,
        Notice::Info,
        Notice::Quiet,
        Notice::Warn,
    ]);
}

/// Two consumers with different lists of sources keep their own conflict episodes: one's fetch must not end or
/// restart the other's.
#[test]
fn conflict_episodes_are_kept_per_list_of_sources() {
    let ctx = context(vec![]);
    let node_sources = vec![vec!["https://node.example/t".to_string()]];
    let bot_sources = vec![vec!["https://bot.example/t".to_string()]];
    let conflicting = || {
        combine(vec![
            source_table("a", &[("KMD", "0.00678", "coingecko", 10)]),
            source_table("b", &[("KMD", "0.00313", "coinpaprika", 20)]),
        ])
    };
    let agreeing = || {
        combine(vec![
            source_table("a", &[("KMD", "0.00678", "coingecko", 10)]),
            source_table("b", &[("KMD", "0.00679", "coingecko", 20)]),
        ])
    };
    let kmd = |notice| vec![("KMD".to_string(), notice)];
    assert_eq!(ctx.note_conflicts(&node_sources, &conflicting()), kmd(Notice::Warn));
    // The bot's list starts its own episode ...
    assert_eq!(ctx.note_conflicts(&bot_sources, &conflicting()), kmd(Notice::Warn));
    // ... and alternating does not warn again.
    assert_eq!(ctx.note_conflicts(&node_sources, &conflicting()), kmd(Notice::Debug));
    assert_eq!(ctx.note_conflicts(&bot_sources, &conflicting()), kmd(Notice::Debug));
    // One list's recovery does not end the other's episode.
    assert_eq!(ctx.note_conflicts(&bot_sources, &agreeing()), kmd(Notice::Info));
    assert_eq!(ctx.note_conflicts(&node_sources, &conflicting()), kmd(Notice::Debug));
}

// ---------------------------------------------------------------------------------------------------------------
// Fetching
// ---------------------------------------------------------------------------------------------------------------

fn document_of(entries: &[(&str, &str, &str, u64)]) -> String { price_document(entries) }

#[test]
fn a_source_tries_its_urls_in_order() {
    let (first, first_hits) = serve(SERVER_ERROR, "");
    let (second, second_hits) = serve(OK, document_of(&[("BTC", "100", "binance", 10)]));
    let sources = vec![vec![dead_url(), first, second]];
    let ctx = context(sources.clone());
    let table = block_on(fetch_combined(&ctx, &sources)).unwrap();
    assert_eq!(table["BTC"].infos.last_price, MmNumber::from("100"));
    assert_eq!(first_hits.load(Ordering::SeqCst), 1);
    assert_eq!(second_hits.load(Ordering::SeqCst), 1);
}

#[test]
fn a_non_200_answer_and_an_unparsable_body_are_failures_of_the_url() {
    let (unparsable, unparsable_hits) = serve(OK, "not json");
    let (incomplete, incomplete_hits) = serve(OK, r#"{"BTC":{"last_price":"1"}}"#);
    let (good, good_hits) = serve(OK, document_of(&[("BTC", "100", "binance", 10)]));
    let sources = vec![vec![unparsable, incomplete, good]];
    let ctx = context(sources.clone());
    let table = block_on(fetch_combined(&ctx, &sources)).unwrap();
    assert!(table.contains_key("BTC"));
    for hits in [unparsable_hits, incomplete_hits, good_hits] {
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }
}

/// There is no rotation: a later URL of a source is not asked while the first answers, on any fetch.
#[test]
fn a_source_that_answers_is_asked_first_every_time() {
    let (first, first_hits) = serve(OK, document_of(&[("BTC", "100", "binance", 10)]));
    let (second, second_hits) = serve(OK, document_of(&[("BTC", "100", "binance", 10)]));
    let sources = vec![vec![first, second]];
    let ctx = context(sources.clone());
    for _ in 0..3 {
        block_on(fetch_combined(&ctx, &sources)).unwrap();
    }
    assert_eq!(first_hits.load(Ordering::SeqCst), 3);
    assert_eq!(second_hits.load(Ordering::SeqCst), 0);
}

#[test]
fn two_sources_are_both_fetched_and_combined() {
    let (first, first_hits) = serve(
        OK,
        document_of(&[("BTC", "100", "binance", 10), ("KMD", "0.25", "coingecko", 10)]),
    );
    let (second, second_hits) = serve(
        OK,
        document_of(&[("BTC", "101", "binance", 20), ("MON", "0.016", "coinpaprika", 20)]),
    );
    let sources = vec![vec![first], vec![second]];
    let ctx = context(sources.clone());
    let table = block_on(fetch_combined(&ctx, &sources)).unwrap();
    assert_eq!(first_hits.load(Ordering::SeqCst), 1);
    assert_eq!(second_hits.load(Ordering::SeqCst), 1);
    assert_eq!(table["BTC"].infos.last_price, MmNumber::from("101"));
    assert!(table.contains_key("KMD") && table.contains_key("MON"));
}

#[test]
fn a_conflicting_ticker_is_absent_from_the_fetched_table() {
    let (first, _) = serve(
        OK,
        document_of(&[("BTC", "100", "binance", 10), ("KMD", "0.00678", "coingecko", 10)]),
    );
    let (second, _) = serve(
        OK,
        document_of(&[("BTC", "100", "binance", 20), ("KMD", "0.00313", "coinpaprika", 20)]),
    );
    let sources = vec![vec![first], vec![second]];
    let ctx = context(sources.clone());
    let table = block_on(fetch_combined(&ctx, &sources)).unwrap();
    assert!(table.contains_key("BTC"));
    assert!(!table.contains_key("KMD"));
}

#[test]
fn with_one_source_down_the_other_is_used_alone() {
    let (second, _) = serve(OK, document_of(&[("BTC", "100", "binance", 10)]));
    let sources = vec![vec![dead_url()], vec![second]];
    let ctx = context(sources.clone());
    let table = block_on(fetch_combined(&ctx, &sources)).unwrap();
    assert_eq!(table.len(), 1);
}

#[test]
fn with_every_source_down_the_fetch_is_an_error() {
    let (first, first_hits) = serve(SERVER_ERROR, "");
    let sources = vec![vec![first], vec![dead_url(), dead_url()]];
    let ctx = context(sources.clone());
    assert!(block_on(fetch_combined(&ctx, &sources)).is_err());
    assert_eq!(first_hits.load(Ordering::SeqCst), 1);
    assert!(block_on(fetch_combined(&ctx, &[])).is_err());
    assert!(block_on(fetch_combined(&ctx, &[vec![]])).is_err());
}

const SECRET_PARTS: [&str; 2] = ["PATHSECRET", "QUERYSECRET"];

/// A server that answers 404 with a body echoing the request (path, query and the full URL) must not carry a secret
/// part of a configured URL into the error or into the line that logs it (CRD ch.62 R62.0).
#[test]
fn an_error_body_that_echoes_the_url_leaks_no_part_of_it() {
    let (port, _) = serve_echoing(NOT_FOUND);
    let url = format!("http://127.0.0.1:{port}/v2/PATHSECRET/tickers?apikey=QUERYSECRET");
    let host = format!("http://127.0.0.1:{port}");
    let err = block_on(request_price_table(&url)).unwrap_err();
    let shown = [format!("{:?}", err), failure_reason(err.get_inner())];
    for text in &shown {
        assert!(text.contains(&host), "the host is missing from the error text");
        for (index, secret) in SECRET_PARTS.iter().enumerate() {
            assert!(!text.contains(secret), "secret part {index} reached the error text");
        }
    }

    // The same through a source, as the node fetches it.
    let sources = vec![vec![url]];
    let ctx = context(sources.clone());
    let err = block_on(fetch_combined(&ctx, &sources)).unwrap_err();
    let shown = format!("{:?}", err);
    for (index, secret) in SECRET_PARTS.iter().enumerate() {
        assert!(!shown.contains(secret), "secret part {index} reached the fetch error");
    }
}
