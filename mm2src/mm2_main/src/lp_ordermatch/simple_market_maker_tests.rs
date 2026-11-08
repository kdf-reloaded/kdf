use crate::mm2::{lp_ordermatch::lp_bot::simple_market_maker_bot::vwap,
                 lp_ordermatch::lp_bot::Provider,
                 lp_ordermatch::lp_bot::SimpleCoinMarketMakerCfg,
                 lp_ordermatch::lp_bot::TickerInfos,
                 lp_ordermatch::lp_bot::TickerInfosRegistry,
                 lp_swap::{MakerSavedSwap, MyRecentSwapsResponse, SavedSwap}};
use common::{block_on, log::UnifiedLoggerBuilder, mm_number::MmNumber};

use std::num::NonZeroUsize;

fn generate_swaps_from_values(swaps_value: Vec<(MmNumber, MmNumber)>) -> MyRecentSwapsResponse {
    let mut swaps: Vec<SavedSwap> = Vec::with_capacity(swaps_value.len());
    for (base_amount, other_amount) in swaps_value.iter() {
        swaps.push(SavedSwap::Maker(MakerSavedSwap::new(base_amount, other_amount)));
    }
    MyRecentSwapsResponse {
        from_uuid: None,
        limit: 0,
        skipped: 0,
        total: 0,
        found_records: 0,
        page_number: NonZeroUsize::new(1).unwrap(),
        total_pages: 0,
        swaps,
    }
}

fn generate_cfg_from_params(base: String, rel: String, spread: MmNumber) -> SimpleCoinMarketMakerCfg {
    SimpleCoinMarketMakerCfg {
        base,
        rel,
        min_volume: None,
        max_volume: None,
        spread,
        base_confs: None,
        base_nota: None,
        rel_confs: None,
        rel_nota: None,
        enable: true,
        price_elapsed_validity: None,
        check_last_bidirectional_trade_thresh_hold: Some(true),
        max: Some(true),
        min_base_price: None,
        min_rel_price: None,
        min_pair_price: None,
    }
}

mod tests {
    use super::*;
    use std::time::SystemTime;

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn test_vwap_empty_base_rel() {
        let base_swaps = generate_swaps_from_values(vec![]);
        let rel_swaps = generate_swaps_from_values(vec![]);
        let mut calculated_price = MmNumber::from("7.14455729");
        let cfg = generate_cfg_from_params("FIRO".to_string(), "KMD".to_string(), MmNumber::from("1.015"));
        calculated_price = block_on(vwap(base_swaps, rel_swaps, calculated_price.clone(), &cfg));
        assert_eq!(calculated_price, MmNumber::from("7.14455729"));
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn test_vwap_single_base_side() {
        UnifiedLoggerBuilder::default().try_init().unwrap_or(());
        let base_swaps =
            generate_swaps_from_values(vec![(MmNumber::from("29.99997438"), MmNumber::from("222.76277576"))]);
        let rel_swaps = generate_swaps_from_values(vec![]);
        let mut calculated_price = MmNumber::from("7.6");
        let cfg = generate_cfg_from_params("FIRO".to_string(), "KMD".to_string(), MmNumber::from("1.015"));
        calculated_price = block_on(vwap(base_swaps, rel_swaps, calculated_price.clone(), &cfg));
        assert_eq!(calculated_price.to_decimal(), MmNumber::from("7.6").to_decimal());
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn test_vwap_single_base_side_forced_price() {
        UnifiedLoggerBuilder::default().try_init().unwrap_or(());
        let base_swaps =
            generate_swaps_from_values(vec![(MmNumber::from("29.99997438"), MmNumber::from("222.76277576"))]);
        let rel_swaps = generate_swaps_from_values(vec![]);
        let mut calculated_price = MmNumber::from("7.14455729");
        let cfg = generate_cfg_from_params("FIRO".to_string(), "KMD".to_string(), MmNumber::from("1.015"));
        calculated_price = block_on(vwap(base_swaps, rel_swaps, calculated_price.clone(), &cfg));
        let expected_price = MmNumber::from(
            "7.425432199985765454510364818518221681227982435363666467237829687773220024996568013735750396997505703",
        );
        assert_eq!(calculated_price.to_decimal(), expected_price.to_decimal());
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn test_vwap_multiple_base_side() {
        UnifiedLoggerBuilder::default().try_init().unwrap_or(());
        let base_swaps = generate_swaps_from_values(vec![
            (MmNumber::from("29.99997438"), MmNumber::from("222.76277576")),
            (MmNumber::from("14.99998719"), MmNumber::from("105.38138788")),
        ]);
        let rel_swaps = generate_swaps_from_values(vec![]);
        let mut calculated_price = MmNumber::from("7.14455729");
        let cfg = generate_cfg_from_params("FIRO".to_string(), "KMD".to_string(), MmNumber::from("1.015"));
        calculated_price = block_on(vwap(base_swaps, rel_swaps, calculated_price.clone(), &cfg));
        let expected_price = MmNumber::from(
            "7.292098752785668212293986632397917401154793919527340609682214001944090993587041856667078927018737007",
        );
        assert_eq!(calculated_price.to_decimal(), expected_price.to_decimal());
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn test_vwap_multiple_base_side_forced_price() {
        UnifiedLoggerBuilder::default().try_init().unwrap_or(());
        let base_swaps = generate_swaps_from_values(vec![
            (MmNumber::from("29.99997438"), MmNumber::from("222.76277576")),
            (MmNumber::from("29.99997438"), MmNumber::from("190.76277576")),
        ]);
        let rel_swaps = generate_swaps_from_values(vec![]);
        let mut calculated_price = MmNumber::from("7.14455729");
        let cfg = generate_cfg_from_params("FIRO".to_string(), "KMD".to_string(), MmNumber::from("1.015"));
        calculated_price = block_on(vwap(base_swaps, rel_swaps, calculated_price.clone(), &cfg));
        let expected_price = MmNumber::from("7.14455729");
        assert_eq!(calculated_price.to_decimal(), expected_price.to_decimal());
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn test_vwap_single_reversed_side() {
        UnifiedLoggerBuilder::default().try_init().unwrap_or(());
        let base_swaps = generate_swaps_from_values(vec![]);
        let rel_swaps = generate_swaps_from_values(vec![(MmNumber::from("219.4709"), MmNumber::from("29.99999"))]);
        let mut calculated_price = MmNumber::from("7.14455729");
        let cfg = generate_cfg_from_params("FIRO".to_string(), "KMD".to_string(), MmNumber::from("1.015"));
        calculated_price = block_on(vwap(base_swaps, rel_swaps, calculated_price.clone(), &cfg));
        let expected_price = MmNumber::from(
            "7.3156991052330350776783592261197420399140133046711015570338523446174482058160686053562017854005951340",
        );
        assert_eq!(calculated_price.to_decimal(), expected_price.to_decimal());
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn test_vwap_single_reversed_side_forced_price() {
        let base_swaps = generate_swaps_from_values(vec![]);
        let rel_swaps = generate_swaps_from_values(vec![(MmNumber::from("219.4709"), MmNumber::from("29.99999"))]);
        let mut calculated_price = MmNumber::from("7.6");
        let cfg = generate_cfg_from_params("FIRO".to_string(), "KMD".to_string(), MmNumber::from("1.015"));
        calculated_price = block_on(vwap(base_swaps, rel_swaps, calculated_price.clone(), &cfg));
        assert_eq!(calculated_price, MmNumber::from("7.6"));
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn test_vwap_multiple_reversed_side() {
        let base_swaps = generate_swaps_from_values(vec![]);
        let rel_swaps = generate_swaps_from_values(vec![
            (MmNumber::from("219.4709"), MmNumber::from("29.99999")),
            (MmNumber::from("222.762"), MmNumber::from("29.99999")),
        ]);
        let mut calculated_price = MmNumber::from("7.14455729");
        let cfg = generate_cfg_from_params("FIRO".to_string(), "KMD".to_string(), MmNumber::from("1.015"));
        calculated_price = block_on(vwap(base_swaps, rel_swaps, calculated_price.clone(), &cfg));
        let expected_price = MmNumber::from(
            "7.370550790183596727865575955191985063995021331673777224592408197469399156466385488795162931720977240",
        );
        assert_eq!(calculated_price.to_decimal(), expected_price.to_decimal());
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn test_vwap_multiple_reversed_side_forced_price() {
        let base_swaps = generate_swaps_from_values(vec![]);
        let rel_swaps = generate_swaps_from_values(vec![
            (MmNumber::from("219.4709"), MmNumber::from("29.99999")),
            (MmNumber::from("222.762"), MmNumber::from("29.99999")),
        ]);
        let mut calculated_price = MmNumber::from("7.54455729");
        let cfg = generate_cfg_from_params("FIRO".to_string(), "KMD".to_string(), MmNumber::from("1.015"));
        calculated_price = block_on(vwap(base_swaps, rel_swaps, calculated_price.clone(), &cfg));
        assert_eq!(calculated_price, MmNumber::from("7.54455729"));
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn test_vwap_multiple_trade_both_side() {
        let base_swaps = generate_swaps_from_values(vec![
            (MmNumber::from("4.102174"), MmNumber::from("752.8892")),
            (MmNumber::from("1.719676"), MmNumber::from("316.4945")),
            (MmNumber::from("0.052971"), MmNumber::from("10.24148")),
            (MmNumber::from("0.133952"), MmNumber::from("25.98481")),
        ]);
        let rel_swaps = generate_swaps_from_values(vec![
            (MmNumber::from("27.18649"), MmNumber::from("0.161065")),
            (MmNumber::from("13.17659"), MmNumber::from("0.077728")),
            (MmNumber::from("37.00451"), MmNumber::from("0.208161")),
            (MmNumber::from("743.0625"), MmNumber::from("4.266546")),
            (MmNumber::from("2819.370"), MmNumber::from("15.68015")),
            (MmNumber::from("2633.345"), MmNumber::from("14.61151")),
            (MmNumber::from("559.7940"), MmNumber::from("3.006166")),
        ]);
        let mut calculated_price = MmNumber::from("174.1375");
        let cfg = generate_cfg_from_params("LTC".to_string(), "KMD".to_string(), MmNumber::from("1.013"));
        calculated_price = block_on(vwap(base_swaps, rel_swaps, calculated_price.clone(), &cfg));
        let expected_price = MmNumber::from(
            "181.8799512443925929123148242562683065552235106947031231789891984094078454989954152150057794418761367",
        );
        assert_eq!(calculated_price.to_decimal(), expected_price.to_decimal());
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn test_get_cex_rates() {
        let mut registry = TickerInfosRegistry::default();
        let rates = registry
            .get_cex_rates("KMD".to_string(), "LTC".to_string())
            .unwrap_or_default();
        assert_eq!(rates.base_provider, Provider::Unknown);
        assert_eq!(rates.rel_provider, Provider::Unknown);

        registry.0.insert("KMD".to_string(), TickerInfos {
            ticker: "KMD".to_string(),
            last_price: MmNumber::from("10"),
            last_updated: "".to_string(),
            last_updated_timestamp: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            volume24_h: MmNumber::from("25000"),
            price_provider: Provider::Binance,
            volume_provider: Provider::Coinpaprika,
            sparkline_7_d: None,
            sparkline_provider: Default::default(),
            change_24_h: MmNumber::default(),
            change_24_h_provider: Default::default(),
        });

        registry.0.insert("LTC".to_string(), TickerInfos {
            ticker: "LTC".to_string(),
            last_price: MmNumber::from("500.0"),
            last_updated: "".to_string(),
            last_updated_timestamp: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            volume24_h: MmNumber::from("25000"),
            price_provider: Provider::Coingecko,
            volume_provider: Provider::Binance,
            sparkline_7_d: None,
            sparkline_provider: Default::default(),
            change_24_h: MmNumber::default(),
            change_24_h_provider: Default::default(),
        });

        registry.0.insert("USDT".to_string(), TickerInfos {
            ticker: "USDT".to_string(),
            last_price: MmNumber::from("1"),
            last_updated: "".to_string(),
            last_updated_timestamp: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            volume24_h: MmNumber::from("25000"),
            price_provider: Provider::Coingecko,
            volume_provider: Provider::Binance,
            sparkline_7_d: None,
            sparkline_provider: Default::default(),
            change_24_h: MmNumber::default(),
            change_24_h_provider: Default::default(),
        });

        let rates = registry
            .get_cex_rates("KMD".to_string(), "LTC".to_string())
            .unwrap_or_default();
        assert_eq!(rates.base_provider, Provider::Binance);
        assert_eq!(rates.rel_provider, Provider::Coingecko);
        assert_eq!(rates.price, MmNumber::from("0.02"));

        let usdt_infos = registry.get_infos("USDT-PLG20");
        assert!(usdt_infos.is_some());
        assert_eq!(usdt_infos.unwrap().last_price, MmNumber::from(1));

        let usdt_infos = registry.get_infos("USDT");
        assert!(usdt_infos.is_some());
        assert_eq!(usdt_infos.unwrap().last_price, MmNumber::from(1));
    }
}

/// The price sources of a bot start (CRD ch.62 D62.6) and the bot's view of the combined prices.
#[cfg(not(target_arch = "wasm32"))]
mod price_source_tests {
    use super::*;
    use crate::mm2::lp_ordermatch::lp_bot::price_service::stub_server::price_document;
    use crate::mm2::lp_ordermatch::lp_bot::price_service::{combine_sources, default_price_sources,
                                                           parse_price_document, read_price_providers_conf,
                                                           registry_from_table, PriceProvidersConf, ProviderSet,
                                                           SourceTable};
    use crate::mm2::lp_ordermatch::lp_bot::simple_market_maker_bot::{resolve_bot_price_sources,
                                                                     StartSimpleMakerBotError};
    use serde_json::Value as Json;

    fn node_sources() -> Vec<Vec<String>> { vec![vec!["https://node.example/tickers".to_string()]] }

    fn refused(price_url: Option<&str>, price_urls: Option<Json>) -> StartSimpleMakerBotError {
        resolve_bot_price_sources(price_url.map(str::to_string), price_urls, &node_sources())
            .unwrap_err()
            .into_inner()
    }

    #[test]
    fn without_request_keys_the_node_sources_are_used() {
        assert_eq!(
            resolve_bot_price_sources(None, None, &node_sources()).unwrap(),
            node_sources()
        );
        assert_eq!(
            resolve_bot_price_sources(None, None, &default_price_sources()).unwrap(),
            default_price_sources()
        );
    }

    #[test]
    fn a_request_price_url_is_one_source_tried_in_order() {
        let sources = resolve_bot_price_sources(
            Some("https://one.example/t, https://two.example/t".to_string()),
            None,
            &node_sources(),
        )
        .unwrap();
        assert_eq!(sources, vec![vec![
            "https://one.example/t".to_string(),
            "https://two.example/t".to_string()
        ]]);
    }

    #[test]
    fn a_request_price_urls_array_is_independent_sources() {
        let sources = resolve_bot_price_sources(
            None,
            Some(json!(["https://one.example/t", [
                "https://two.example/t",
                "https://three.example/t"
            ]])),
            &node_sources(),
        )
        .unwrap();
        assert_eq!(sources, vec![vec!["https://one.example/t".to_string()], vec![
            "https://two.example/t".to_string(),
            "https://three.example/t".to_string()
        ],]);
    }

    #[test]
    fn a_request_with_both_keys_is_refused() {
        assert!(matches!(
            refused(Some("https://one.example/t"), Some(json!(["https://two.example/t"]))),
            StartSimpleMakerBotError::BothPriceUrlKeys
        ));
    }

    #[test]
    fn a_malformed_request_value_is_refused_not_defaulted() {
        let malformed_values = [
            json!("https://one.example/t"),
            json!([]),
            json!([5]),
            json!([[]]),
            json!([["a", 5]]),
            json!({}),
        ];
        for malformed in malformed_values {
            match refused(None, Some(malformed.clone())) {
                StartSimpleMakerBotError::InvalidPriceUrl(key) => assert_eq!(key, "price_urls", "{malformed}"),
                other => panic!("{malformed}: unexpected {other:?}"),
            }
        }
        for malformed in ["", " , "] {
            match refused(Some(malformed), None) {
                StartSimpleMakerBotError::InvalidPriceUrl(key) => assert_eq!(key, "price_url", "{malformed:?}"),
                other => panic!("{malformed:?}: unexpected {other:?}"),
            }
        }
    }

    /// RIN is priced only by livecoinwatch: with that provider recognized the bot gets a rate for it (CRD ch.62
    /// R62.5.2), and the bot's enum names it.
    #[test]
    fn livecoinwatch_prices_are_accepted_and_the_bot_gets_a_rate() {
        let document = include_str!("../for_tests/price_defistats_tickers_v2.json");
        let source = SourceTable {
            host: "test".to_string(),
            records: parse_price_document(document).unwrap(),
        };
        let table = combine_sources(vec![source], &ProviderSet::default_set()).table;
        let registry = registry_from_table(table);
        let rin = registry.get_infos("RIN").unwrap();
        assert_eq!(rin.price_provider, Provider::Livecoinwatch);
        let rates = registry
            .get_cex_rates("RIN".to_string(), "KMD".to_string())
            .expect("a rate for RIN against KMD");
        assert_eq!(rates.base_provider, Provider::Livecoinwatch);
        assert_eq!(rates.rel_provider, Provider::Coingecko);
        let rates = registry
            .get_cex_rates("RIN".to_string(), "MATIC".to_string())
            .expect("a rate for RIN against MATIC");
        assert_eq!(rates.rel_provider, Provider::Livecoinwatch);
    }

    /// A provider the operator lists in `price_providers` that the bot's enum does not name is still accepted by the
    /// bot; a record from a provider that is not listed never reaches it.
    #[test]
    fn a_listed_provider_without_an_enum_name_is_quoted() {
        let document = price_document(&[
            ("RIN", "0.02", "gleeccex", 1_794_066_000),
            ("KMD", "0.25", "coingecko", 1_794_066_000),
        ]);
        let source = || SourceTable {
            host: "test".to_string(),
            records: parse_price_document(&document).unwrap(),
        };
        let default_set = combine_sources(vec![source()], &ProviderSet::default_set()).table;
        assert!(registry_from_table(default_set)
            .get_cex_rates("RIN".to_string(), "KMD".to_string())
            .is_none());

        let conf = json!({"price_providers": ["gleeccex", "coingecko"]});
        let PriceProvidersConf::Valid { providers, .. } = read_price_providers_conf(&conf) else {
            panic!("the list is well-formed");
        };
        let table = combine_sources(vec![source()], &providers).table;
        let rates = registry_from_table(table)
            .get_cex_rates("RIN".to_string(), "KMD".to_string())
            .expect("gleeccex is listed");
        assert_eq!(rates.base_provider, Provider::Configured);
    }
}
