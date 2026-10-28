use super::*;
use crate::mm2::lp_network::P2PContext;
use crate::mm2::lp_ordermatch::new_protocol::{MakerOrderUpdated, PubkeyKeepAlive};
use crate::mm2::lp_swap::swap_versioning::SwapVersion;
use coins::{MmCoin, TestCoin};
use common::{block_on, executor::spawn};
use crypto::privkey::key_pair_from_seed;
use db_common::sqlite::rusqlite::Connection;
use futures::{channel::mpsc, lock::Mutex as AsyncMutex, StreamExt};
use mm2_core::mm_ctx::{MmArc, MmCtx, MmCtxBuilder};
use mm2_p2p::atomicdex_behaviour::AdexBehaviourCmd;
use mm2_p2p::{decode_message, PeerId};
use mocktopus::mocking::*;
use rand::{seq::SliceRandom, thread_rng, Rng};
use std::collections::HashSet;
use std::iter::{self, FromIterator};
use std::sync::Mutex;

#[test]
fn test_match_maker_order_and_taker_request() {
    let maker = MakerOrder {
        base: "BASE".into(),
        rel: "REL".into(),
        created_at: now_ms(),
        updated_at: Some(now_ms()),
        max_base_vol: 10.into(),
        min_base_vol: 0.into(),
        price: 1.into(),
        matches: HashMap::new(),
        started_swaps: Vec::new(),
        uuid: Uuid::new_v4(),
        conf_settings: None,
        changes_history: None,
        save_in_history: false,
        base_orderbook_ticker: None,
        rel_orderbook_ticker: None,
        p2p_privkey: None,
        timeout_in_minutes: None,
        swap_version: SwapVersion::default(),
    };

    let request = TakerRequest {
        base: "BASE".into(),
        rel: "REL".into(),
        uuid: Uuid::new_v4(),
        dest_pub_key: H256Json::default(),
        sender_pubkey: H256Json::default(),
        base_amount: 10.into(),
        rel_amount: 20.into(),
        action: TakerAction::Buy,
        match_by: MatchBy::Any,
        conf_settings: None,
        base_protocol_info: None,
        rel_protocol_info: None,
        swap_version: SwapVersion::default(),
    };

    let actual = maker.match_with_request(&request);
    let expected = OrderMatchResult::Matched((10.into(), 10.into()));
    assert_eq!(expected, actual);

    let maker = MakerOrder {
        base: "BASE".into(),
        rel: "REL".into(),
        created_at: now_ms(),
        updated_at: Some(now_ms()),
        max_base_vol: 10.into(),
        min_base_vol: 0.into(),
        price: "0.5".into(),
        matches: HashMap::new(),
        started_swaps: Vec::new(),
        uuid: Uuid::new_v4(),
        conf_settings: None,
        changes_history: None,
        save_in_history: false,
        base_orderbook_ticker: None,
        rel_orderbook_ticker: None,
        p2p_privkey: None,
        timeout_in_minutes: None,
        swap_version: SwapVersion::default(),
    };

    let request = TakerRequest {
        base: "BASE".into(),
        rel: "REL".into(),
        uuid: Uuid::new_v4(),
        dest_pub_key: H256Json::default(),
        sender_pubkey: H256Json::default(),
        base_amount: 10.into(),
        rel_amount: 20.into(),
        action: TakerAction::Buy,
        match_by: MatchBy::Any,
        conf_settings: None,
        base_protocol_info: None,
        rel_protocol_info: None,
        swap_version: SwapVersion::default(),
    };

    let actual = maker.match_with_request(&request);
    let expected = OrderMatchResult::Matched((10.into(), 5.into()));
    assert_eq!(expected, actual);

    let maker = MakerOrder {
        base: "BASE".into(),
        rel: "REL".into(),
        created_at: now_ms(),
        updated_at: Some(now_ms()),
        max_base_vol: 10.into(),
        min_base_vol: 0.into(),
        price: "0.5".into(),
        matches: HashMap::new(),
        started_swaps: Vec::new(),
        uuid: Uuid::new_v4(),
        conf_settings: None,
        changes_history: None,
        save_in_history: false,
        base_orderbook_ticker: None,
        rel_orderbook_ticker: None,
        p2p_privkey: None,
        timeout_in_minutes: None,
        swap_version: SwapVersion::default(),
    };

    let request = TakerRequest {
        base: "BASE".into(),
        rel: "REL".into(),
        uuid: Uuid::new_v4(),
        dest_pub_key: H256Json::default(),
        sender_pubkey: H256Json::default(),
        base_amount: 10.into(),
        rel_amount: 2.into(),
        action: TakerAction::Buy,
        match_by: MatchBy::Any,
        conf_settings: None,
        base_protocol_info: None,
        rel_protocol_info: None,
        swap_version: SwapVersion::default(),
    };

    let actual = maker.match_with_request(&request);
    let expected = OrderMatchResult::NotMatched;
    assert_eq!(expected, actual);

    let maker = MakerOrder {
        base: "BASE".into(),
        rel: "REL".into(),
        created_at: now_ms(),
        updated_at: Some(now_ms()),
        max_base_vol: 10.into(),
        min_base_vol: 0.into(),
        price: "0.5".into(),
        matches: HashMap::new(),
        started_swaps: Vec::new(),
        uuid: Uuid::new_v4(),
        conf_settings: None,
        changes_history: None,
        save_in_history: false,
        base_orderbook_ticker: None,
        rel_orderbook_ticker: None,
        p2p_privkey: None,
        timeout_in_minutes: None,
        swap_version: SwapVersion::default(),
    };

    let request = TakerRequest {
        base: "REL".into(),
        rel: "BASE".into(),
        uuid: Uuid::new_v4(),
        dest_pub_key: H256Json::default(),
        sender_pubkey: H256Json::default(),
        base_amount: 5.into(),
        rel_amount: 10.into(),
        action: TakerAction::Sell,
        match_by: MatchBy::Any,
        conf_settings: None,
        base_protocol_info: None,
        rel_protocol_info: None,
        swap_version: SwapVersion::default(),
    };

    let actual = maker.match_with_request(&request);
    let expected = OrderMatchResult::Matched((10.into(), 5.into()));
    assert_eq!(expected, actual);

    let maker = MakerOrder {
        base: "BASE".into(),
        rel: "REL".into(),
        created_at: now_ms(),
        updated_at: Some(now_ms()),
        max_base_vol: 20.into(),
        min_base_vol: 0.into(),
        price: "0.5".into(),
        matches: HashMap::new(),
        started_swaps: Vec::new(),
        uuid: Uuid::new_v4(),
        conf_settings: None,
        changes_history: None,
        save_in_history: false,
        base_orderbook_ticker: None,
        rel_orderbook_ticker: None,
        p2p_privkey: None,
        timeout_in_minutes: None,
        swap_version: SwapVersion::default(),
    };

    let request = TakerRequest {
        base: "REL".into(),
        rel: "BASE".into(),
        uuid: Uuid::new_v4(),
        dest_pub_key: H256Json::default(),
        sender_pubkey: H256Json::default(),
        base_amount: 10.into(),
        rel_amount: 10.into(),
        action: TakerAction::Sell,
        match_by: MatchBy::Any,
        conf_settings: None,
        base_protocol_info: None,
        rel_protocol_info: None,
        swap_version: SwapVersion::default(),
    };

    let actual = maker.match_with_request(&request);
    let expected = OrderMatchResult::Matched((20.into(), 10.into()));
    assert_eq!(expected, actual);

    let maker = MakerOrder {
        base: "BASE".into(),
        rel: "REL".into(),
        created_at: now_ms(),
        updated_at: Some(now_ms()),
        max_base_vol: 1.into(),
        min_base_vol: 0.into(),
        price: "1".into(),
        matches: HashMap::new(),
        started_swaps: Vec::new(),
        uuid: Uuid::new_v4(),
        conf_settings: None,
        changes_history: None,
        save_in_history: false,
        base_orderbook_ticker: None,
        rel_orderbook_ticker: None,
        p2p_privkey: None,
        timeout_in_minutes: None,
        swap_version: SwapVersion::default(),
    };

    let request = TakerRequest {
        base: "REL".into(),
        rel: "BASE".into(),
        uuid: Uuid::new_v4(),
        dest_pub_key: H256Json::default(),
        sender_pubkey: H256Json::default(),
        base_amount: 1.into(),
        rel_amount: "0.9".into(),
        action: TakerAction::Sell,
        match_by: MatchBy::Any,
        conf_settings: None,
        base_protocol_info: None,
        rel_protocol_info: None,
        swap_version: SwapVersion::default(),
    };

    let actual = maker.match_with_request(&request);
    let expected = OrderMatchResult::Matched((1.into(), 1.into()));
    assert_eq!(expected, actual);

    // The following Taker request has not to be matched since the resulted base amount it greater than `max_base_vol`.
    // https://github.com/KomodoPlatform/atomicDEX-API/issues/1041#issuecomment-901863864
    let maker = MakerOrder {
        max_base_vol: "0.2928826881884105".into(),
        min_base_vol: 0.into(),
        price: "2643.01935664".into(),
        created_at: now_ms(),
        updated_at: None,
        base: "ETH-BEP20".to_owned(),
        rel: "KMD".to_owned(),
        matches: HashMap::new(),
        started_swaps: vec![],
        uuid: Uuid::new_v4(),
        conf_settings: None,
        changes_history: None,
        save_in_history: false,
        base_orderbook_ticker: None,
        rel_orderbook_ticker: None,
        p2p_privkey: None,
        timeout_in_minutes: None,
        swap_version: SwapVersion::default(),
    };
    let request = TakerRequest {
        base: "KMD".to_owned(),
        rel: "ETH-BEP20".to_owned(),
        base_amount: "774.205645538427044180416545".into(),
        rel_amount: "0.2928826881884105".into(),
        action: TakerAction::Sell,
        uuid: Uuid::new_v4(),
        sender_pubkey: H256Json::default(),
        dest_pub_key: H256Json::default(),
        match_by: MatchBy::Any,
        conf_settings: None,
        base_protocol_info: None,
        rel_protocol_info: None,
        swap_version: SwapVersion::default(),
    };
    let actual = maker.match_with_request(&request);
    assert_eq!(actual, OrderMatchResult::NotMatched);

    // Though the Taker's rel amount is less than the Makers' min base volume '2',
    // the Maker's price is chosen to calculate the result amounts, so we have:
    // `base_amount = taker_base_amount/maker_price = 30/10 = 3`
    // `rel_amount = taker_base_amount = 30`.
    // The order should be matched.
    let maker = MakerOrder {
        max_base_vol: "3".into(),
        min_base_vol: "2".into(),
        price: 10.into(),
        created_at: now_ms(),
        updated_at: None,
        base: "BASE".to_owned(),
        rel: "REL".to_owned(),
        matches: HashMap::new(),
        started_swaps: vec![],
        uuid: Uuid::new_v4(),
        conf_settings: None,
        changes_history: None,
        save_in_history: false,
        base_orderbook_ticker: None,
        rel_orderbook_ticker: None,
        p2p_privkey: None,
        timeout_in_minutes: None,
        swap_version: SwapVersion::default(),
    };
    let request = TakerRequest {
        base: "REL".to_owned(),
        rel: "BASE".to_owned(),
        base_amount: "30".into(),
        rel_amount: "1.5".into(),
        action: TakerAction::Sell,
        uuid: Uuid::new_v4(),
        sender_pubkey: H256Json::default(),
        dest_pub_key: H256Json::default(),
        match_by: MatchBy::Any,
        conf_settings: None,
        base_protocol_info: None,
        rel_protocol_info: None,
        swap_version: SwapVersion::default(),
    };
    let actual = maker.match_with_request(&request);
    let expected_base_amount = MmNumber::from(3);
    let expected_rel_amount = MmNumber::from(30);
    let expected = OrderMatchResult::Matched((expected_base_amount, expected_rel_amount));
    assert_eq!(actual, expected);
}

// https://github.com/KomodoPlatform/atomicDEX-API/pull/739#discussion_r517275495
#[test]
fn maker_order_match_with_request_zero_volumes() {
    let coin = MmCoinEnum::Test(TestCoin::default());

    let maker_order = MakerOrderBuilder::new(&coin, &coin)
        .with_max_base_vol(1.into())
        .with_price(1.into())
        .build_unchecked();

    // default taker order has empty coins and zero amounts so it should pass to the price calculation stage (division)
    let taker_order = TakerOrderBuilder::new(&coin, &coin)
        .with_rel_amount(1.into())
        .build_unchecked();

    let expected = OrderMatchResult::NotMatched;
    let actual = maker_order.match_with_request(&taker_order.request);
    assert_eq!(expected, actual);

    // default taker order has empty coins and zero amounts so it should pass to the price calculation stage (division)
    let taker_request = TakerOrderBuilder::new(&coin, &coin)
        .with_base_amount(1.into())
        .with_action(TakerAction::Sell)
        .build_unchecked();

    let expected = OrderMatchResult::NotMatched;
    let actual = maker_order.match_with_request(&taker_request.request);
    assert_eq!(expected, actual);
}

#[test]
fn test_maker_order_available_amount() {
    let mut maker = MakerOrder {
        base: "BASE".into(),
        rel: "REL".into(),
        created_at: now_ms(),
        updated_at: Some(now_ms()),
        max_base_vol: 10.into(),
        min_base_vol: 0.into(),
        price: 1.into(),
        matches: HashMap::new(),
        started_swaps: Vec::new(),
        uuid: Uuid::new_v4(),
        conf_settings: None,
        changes_history: None,
        save_in_history: false,
        base_orderbook_ticker: None,
        rel_orderbook_ticker: None,
        p2p_privkey: None,
        timeout_in_minutes: None,
        swap_version: SwapVersion::default(),
    };
    maker.matches.insert(Uuid::new_v4(), MakerMatch {
        request: TakerRequest {
            uuid: Uuid::new_v4(),
            base: "BASE".into(),
            rel: "REL".into(),
            base_amount: 5.into(),
            rel_amount: 5.into(),
            sender_pubkey: H256Json::default(),
            dest_pub_key: H256Json::default(),
            action: TakerAction::Buy,
            match_by: MatchBy::Any,
            conf_settings: None,
            base_protocol_info: None,
            rel_protocol_info: None,
            swap_version: SwapVersion::default(),
        },
        reserved: MakerReserved {
            base: "BASE".into(),
            rel: "REL".into(),
            base_amount: 5.into(),
            rel_amount: 5.into(),
            sender_pubkey: H256Json::default(),
            dest_pub_key: H256Json::default(),
            maker_order_uuid: Uuid::new_v4(),
            taker_order_uuid: Uuid::new_v4(),
            conf_settings: None,
            base_protocol_info: None,
            rel_protocol_info: None,
            swap_version: SwapVersion::default(),
        },
        connect: None,
        connected: None,
        last_updated: now_ms(),
    });
    maker.matches.insert(Uuid::new_v4(), MakerMatch {
        request: TakerRequest {
            uuid: Uuid::new_v4(),
            base: "BASE".into(),
            rel: "REL".into(),
            base_amount: 1.into(),
            rel_amount: 1.into(),
            sender_pubkey: H256Json::default(),
            dest_pub_key: H256Json::default(),
            action: TakerAction::Buy,
            match_by: MatchBy::Any,
            conf_settings: None,
            base_protocol_info: None,
            rel_protocol_info: None,
            swap_version: SwapVersion::default(),
        },
        reserved: MakerReserved {
            base: "BASE".into(),
            rel: "REL".into(),
            base_amount: 1.into(),
            rel_amount: 1.into(),
            sender_pubkey: H256Json::default(),
            dest_pub_key: H256Json::default(),
            maker_order_uuid: Uuid::new_v4(),
            taker_order_uuid: Uuid::new_v4(),
            conf_settings: None,
            base_protocol_info: None,
            rel_protocol_info: None,
            swap_version: SwapVersion::default(),
        },
        connect: None,
        connected: None,
        last_updated: now_ms(),
    });

    let expected = BigRational::from_integer(4.into());
    let actual = maker.available_amount();
    assert_eq!(MmNumber::from(expected), actual);
}

#[test]
fn test_taker_match_reserved() {
    let uuid = Uuid::new_v4();

    let request = TakerRequest {
        base: "BASE".into(),
        rel: "REL".into(),
        uuid,
        dest_pub_key: H256Json::default(),
        sender_pubkey: H256Json::default(),
        base_amount: 10.into(),
        rel_amount: 10.into(),
        action: TakerAction::Buy,
        match_by: MatchBy::Any,
        conf_settings: None,
        base_protocol_info: None,
        rel_protocol_info: None,
        swap_version: SwapVersion::default(),
    };

    let order = TakerOrder {
        request,
        matches: HashMap::new(),
        created_at: now_ms(),
        order_type: OrderType::GoodTillCancelled,
        min_volume: 0.into(),
        timeout: 30,
        save_in_history: false,
        base_orderbook_ticker: None,
        rel_orderbook_ticker: None,
        p2p_privkey: None,
    };

    let reserved = MakerReserved {
        base: "BASE".into(),
        rel: "REL".into(),
        base_amount: 10.into(),
        rel_amount: 10.into(),
        sender_pubkey: H256Json::default(),
        dest_pub_key: H256Json::default(),
        maker_order_uuid: Uuid::new_v4(),
        taker_order_uuid: uuid,
        conf_settings: None,
        base_protocol_info: None,
        rel_protocol_info: None,
        swap_version: SwapVersion::default(),
    };

    assert_eq!(MatchReservedResult::Matched, order.match_reserved(&reserved));

    let request = TakerRequest {
        base: "BASE".into(),
        rel: "REL".into(),
        uuid,
        dest_pub_key: H256Json::default(),
        sender_pubkey: H256Json::default(),
        base_amount: 10.into(),
        rel_amount: 10.into(),
        action: TakerAction::Sell,
        match_by: MatchBy::Any,
        conf_settings: None,
        base_protocol_info: None,
        rel_protocol_info: None,
        swap_version: SwapVersion::default(),
    };

    let order = TakerOrder {
        request,
        matches: HashMap::new(),
        created_at: now_ms(),
        order_type: OrderType::GoodTillCancelled,
        min_volume: 0.into(),
        timeout: 30,
        save_in_history: false,
        base_orderbook_ticker: None,
        rel_orderbook_ticker: None,
        p2p_privkey: None,
    };

    let reserved = MakerReserved {
        base: "REL".into(),
        rel: "BASE".into(),
        base_amount: 10.into(),
        rel_amount: 10.into(),
        sender_pubkey: H256Json::default(),
        dest_pub_key: H256Json::default(),
        maker_order_uuid: Uuid::new_v4(),
        taker_order_uuid: uuid,
        conf_settings: None,
        base_protocol_info: None,
        rel_protocol_info: None,
        swap_version: SwapVersion::default(),
    };

    assert_eq!(MatchReservedResult::Matched, order.match_reserved(&reserved));

    let request = TakerRequest {
        base: "BASE".into(),
        rel: "REL".into(),
        uuid,
        dest_pub_key: H256Json::default(),
        sender_pubkey: H256Json::default(),
        base_amount: 1.into(),
        rel_amount: "0.9".into(),
        action: TakerAction::Sell,
        match_by: MatchBy::Any,
        conf_settings: None,
        base_protocol_info: None,
        rel_protocol_info: None,
        swap_version: SwapVersion::default(),
    };

    let order = TakerOrder {
        request,
        matches: HashMap::new(),
        created_at: now_ms(),
        order_type: OrderType::GoodTillCancelled,
        min_volume: 0.into(),
        timeout: 30,
        save_in_history: false,
        base_orderbook_ticker: None,
        rel_orderbook_ticker: None,
        p2p_privkey: None,
    };

    let reserved = MakerReserved {
        base: "REL".into(),
        rel: "BASE".into(),
        base_amount: 1.into(),
        rel_amount: 1.into(),
        sender_pubkey: H256Json::default(),
        dest_pub_key: H256Json::default(),
        maker_order_uuid: Uuid::new_v4(),
        taker_order_uuid: uuid,
        conf_settings: None,
        base_protocol_info: None,
        rel_protocol_info: None,
        swap_version: SwapVersion::default(),
    };

    assert_eq!(MatchReservedResult::Matched, order.match_reserved(&reserved));

    let request = TakerRequest {
        base: "BASE".into(),
        rel: "REL".into(),
        uuid,
        dest_pub_key: H256Json::default(),
        sender_pubkey: H256Json::default(),
        base_amount: 1.into(),
        rel_amount: "0.9".into(),
        action: TakerAction::Sell,
        match_by: MatchBy::Any,
        conf_settings: None,
        base_protocol_info: None,
        rel_protocol_info: None,
        swap_version: SwapVersion::default(),
    };

    let order = TakerOrder {
        request,
        matches: HashMap::new(),
        created_at: now_ms(),
        order_type: OrderType::GoodTillCancelled,
        min_volume: 0.into(),
        timeout: 30,
        save_in_history: false,
        base_orderbook_ticker: None,
        rel_orderbook_ticker: None,
        p2p_privkey: None,
    };

    let reserved = MakerReserved {
        base: "REL".into(),
        rel: "BASE".into(),
        base_amount: "0.8".into(),
        rel_amount: 1.into(),
        sender_pubkey: H256Json::default(),
        dest_pub_key: H256Json::default(),
        maker_order_uuid: Uuid::new_v4(),
        taker_order_uuid: uuid,
        conf_settings: None,
        base_protocol_info: None,
        rel_protocol_info: None,
        swap_version: SwapVersion::default(),
    };

    assert_eq!(MatchReservedResult::NotMatched, order.match_reserved(&reserved));

    let request = TakerRequest {
        base: "BASE".into(),
        rel: "REL".into(),
        uuid,
        dest_pub_key: H256Json::default(),
        sender_pubkey: H256Json::default(),
        base_amount: 1.into(),
        rel_amount: 2.into(),
        action: TakerAction::Buy,
        match_by: MatchBy::Any,
        conf_settings: None,
        base_protocol_info: None,
        rel_protocol_info: None,
        swap_version: SwapVersion::default(),
    };

    let order = TakerOrder {
        request,
        matches: HashMap::new(),
        created_at: now_ms(),
        order_type: OrderType::GoodTillCancelled,
        min_volume: 0.into(),
        timeout: 30,
        save_in_history: false,
        base_orderbook_ticker: None,
        rel_orderbook_ticker: None,
        p2p_privkey: None,
    };

    let reserved = MakerReserved {
        base: "BASE".into(),
        rel: "REL".into(),
        base_amount: 1.into(),
        rel_amount: 1.into(),
        sender_pubkey: H256Json::default(),
        dest_pub_key: H256Json::default(),
        maker_order_uuid: Uuid::new_v4(),
        taker_order_uuid: uuid,
        conf_settings: None,
        base_protocol_info: None,
        rel_protocol_info: None,
        swap_version: SwapVersion::default(),
    };

    assert_eq!(MatchReservedResult::Matched, order.match_reserved(&reserved));

    let request = TakerRequest {
        base: "BASE".into(),
        rel: "REL".into(),
        uuid,
        dest_pub_key: H256Json::default(),
        sender_pubkey: H256Json::default(),
        base_amount: 1.into(),
        rel_amount: 2.into(),
        action: TakerAction::Buy,
        match_by: MatchBy::Any,
        conf_settings: None,
        base_protocol_info: None,
        rel_protocol_info: None,
        swap_version: SwapVersion::default(),
    };

    let order = TakerOrder {
        request,
        matches: HashMap::new(),
        created_at: now_ms(),
        order_type: OrderType::GoodTillCancelled,
        min_volume: 0.into(),
        timeout: 30,
        save_in_history: false,
        base_orderbook_ticker: None,
        rel_orderbook_ticker: None,
        p2p_privkey: None,
    };

    let reserved = MakerReserved {
        base: "BASE".into(),
        rel: "REL".into(),
        base_amount: 1.into(),
        rel_amount: 1.into(),
        sender_pubkey: H256Json::default(),
        dest_pub_key: H256Json::default(),
        maker_order_uuid: Uuid::new_v4(),
        taker_order_uuid: uuid,
        conf_settings: None,
        base_protocol_info: None,
        rel_protocol_info: None,
        swap_version: SwapVersion::default(),
    };

    assert_eq!(MatchReservedResult::Matched, order.match_reserved(&reserved));

    let request = TakerRequest {
        base: "BASE".into(),
        rel: "REL".into(),
        uuid,
        dest_pub_key: H256Json::default(),
        sender_pubkey: H256Json::default(),
        base_amount: 1.into(),
        rel_amount: 2.into(),
        action: TakerAction::Buy,
        match_by: MatchBy::Any,
        conf_settings: None,
        base_protocol_info: None,
        rel_protocol_info: None,
        swap_version: SwapVersion::default(),
    };

    let order = TakerOrder {
        request,
        matches: HashMap::new(),
        created_at: now_ms(),
        order_type: OrderType::GoodTillCancelled,
        min_volume: 0.into(),
        timeout: 30,
        save_in_history: false,
        base_orderbook_ticker: None,
        rel_orderbook_ticker: None,
        p2p_privkey: None,
    };

    let reserved = MakerReserved {
        base: "BASE".into(),
        rel: "REL".into(),
        base_amount: 1.into(),
        rel_amount: 1.into(),
        sender_pubkey: H256Json::default(),
        dest_pub_key: H256Json::default(),
        maker_order_uuid: Uuid::new_v4(),
        taker_order_uuid: uuid,
        conf_settings: None,
        base_protocol_info: None,
        rel_protocol_info: None,
        swap_version: SwapVersion::default(),
    };

    assert_eq!(MatchReservedResult::Matched, order.match_reserved(&reserved));

    let request = TakerRequest {
        base: "BASE".into(),
        rel: "REL".into(),
        uuid,
        dest_pub_key: H256Json::default(),
        sender_pubkey: H256Json::default(),
        base_amount: 1.into(),
        rel_amount: 2.into(),
        action: TakerAction::Buy,
        match_by: MatchBy::Any,
        conf_settings: None,
        base_protocol_info: None,
        rel_protocol_info: None,
        swap_version: SwapVersion::default(),
    };

    let order = TakerOrder {
        request,
        matches: HashMap::new(),
        created_at: now_ms(),
        order_type: OrderType::GoodTillCancelled,
        min_volume: 0.into(),
        timeout: 30,
        save_in_history: false,
        base_orderbook_ticker: None,
        rel_orderbook_ticker: None,
        p2p_privkey: None,
    };

    let reserved = MakerReserved {
        base: "BASE".into(),
        rel: "REL".into(),
        base_amount: 1.into(),
        rel_amount: 3.into(),
        sender_pubkey: H256Json::default(),
        dest_pub_key: H256Json::default(),
        maker_order_uuid: Uuid::new_v4(),
        taker_order_uuid: uuid,
        conf_settings: None,
        base_protocol_info: None,
        rel_protocol_info: None,
        swap_version: SwapVersion::default(),
    };

    assert_eq!(MatchReservedResult::NotMatched, order.match_reserved(&reserved));

    let order = TakerOrder {
        created_at: 1568358064115,
        request: TakerRequest {
            base: "DOC".into(),
            rel: "MARTY".into(),
            base_amount:
                "0.3333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333"
                    .into(),
            rel_amount: 1.into(),
            action: TakerAction::Buy,
            uuid,
            sender_pubkey: H256Json::default(),
            dest_pub_key: H256Json::default(),
            match_by: MatchBy::Any,
            conf_settings: None,
            base_protocol_info: None,
            rel_protocol_info: None,
            swap_version: SwapVersion::default(),
        },
        matches: HashMap::new(),
        order_type: OrderType::GoodTillCancelled,
        min_volume: 0.into(),
        timeout: 30,
        save_in_history: false,
        base_orderbook_ticker: None,
        rel_orderbook_ticker: None,
        p2p_privkey: None,
    };

    let reserved = MakerReserved {
        base: "DOC".into(),
        rel: "MARTY".into(),
        base_amount: "0.3333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333".into(),
        rel_amount: "0.777777776666666666666666666666666666666666666666666666666666666666666666666666666666666666666666666588888889".into(),
        taker_order_uuid: uuid,
        maker_order_uuid: uuid,
        sender_pubkey: H256Json::default(),
        dest_pub_key: H256Json::default(),
        conf_settings: None,
        base_protocol_info: None,
        rel_protocol_info: None,
            swap_version: SwapVersion::default(),
    };

    assert_eq!(MatchReservedResult::Matched, order.match_reserved(&reserved));
}

#[test]
fn test_taker_order_cancellable() {
    let request = TakerRequest {
        base: "BASE".into(),
        rel: "REL".into(),
        uuid: Uuid::new_v4(),
        dest_pub_key: H256Json::default(),
        sender_pubkey: H256Json::default(),
        base_amount: 1.into(),
        rel_amount: 2.into(),
        action: TakerAction::Buy,
        match_by: MatchBy::Any,
        conf_settings: None,
        base_protocol_info: None,
        rel_protocol_info: None,
        swap_version: SwapVersion::default(),
    };

    let order = TakerOrder {
        request,
        matches: HashMap::new(),
        created_at: now_ms(),
        order_type: OrderType::GoodTillCancelled,
        min_volume: 0.into(),
        timeout: 30,
        save_in_history: false,
        base_orderbook_ticker: None,
        rel_orderbook_ticker: None,
        p2p_privkey: None,
    };

    assert!(order.is_cancellable());

    let request = TakerRequest {
        base: "BASE".into(),
        rel: "REL".into(),
        uuid: Uuid::new_v4(),
        dest_pub_key: H256Json::default(),
        sender_pubkey: H256Json::default(),
        base_amount: 1.into(),
        rel_amount: 2.into(),
        action: TakerAction::Buy,
        match_by: MatchBy::Any,
        conf_settings: None,
        base_protocol_info: None,
        rel_protocol_info: None,
        swap_version: SwapVersion::default(),
    };

    let mut order = TakerOrder {
        request,
        matches: HashMap::new(),
        created_at: now_ms(),
        order_type: OrderType::GoodTillCancelled,
        min_volume: 0.into(),
        timeout: 30,
        save_in_history: false,
        base_orderbook_ticker: None,
        rel_orderbook_ticker: None,
        p2p_privkey: None,
    };

    order.matches.insert(Uuid::new_v4(), TakerMatch {
        last_updated: now_ms(),
        reserved: MakerReserved {
            base: "BASE".into(),
            rel: "REL".into(),
            base_amount: 1.into(),
            rel_amount: 3.into(),
            sender_pubkey: H256Json::default(),
            dest_pub_key: H256Json::default(),
            maker_order_uuid: Uuid::new_v4(),
            taker_order_uuid: Uuid::new_v4(),
            conf_settings: None,
            base_protocol_info: None,
            rel_protocol_info: None,
            swap_version: SwapVersion::default(),
        },
        connect: TakerConnect {
            sender_pubkey: H256Json::default(),
            dest_pub_key: H256Json::default(),
            maker_order_uuid: Uuid::new_v4(),
            taker_order_uuid: Uuid::new_v4(),
        },
        connected: None,
    });

    assert!(!order.is_cancellable());
}

fn prepare_for_cancel_by(ctx: &MmArc) -> mpsc::Receiver<AdexBehaviourCmd> {
    let (tx, rx) = mpsc::channel(10);
    let p2p_ctx = P2PContext::new(tx);
    p2p_ctx.store_to_mm_arc(ctx);

    let ordermatch_ctx = OrdermatchContext::from_ctx(ctx).unwrap();
    let mut maker_orders = ordermatch_ctx.maker_orders_ctx.lock();
    let mut taker_orders = block_on(ordermatch_ctx.my_taker_orders.lock());

    maker_orders.insert_raw(
        Uuid::from_bytes([0; 16]),
        "DOC".into(),
        Arc::new(AsyncMutex::new(MakerOrder {
            uuid: Uuid::from_bytes([0; 16]),
            base: "DOC".into(),
            rel: "MARTY".into(),
            created_at: now_ms(),
            updated_at: Some(now_ms()),
            matches: HashMap::new(),
            max_base_vol: 0.into(),
            min_base_vol: 0.into(),
            price: 0.into(),
            started_swaps: vec![],
            conf_settings: None,
            changes_history: None,
            save_in_history: false,
            base_orderbook_ticker: None,
            rel_orderbook_ticker: None,
            p2p_privkey: None,
            timeout_in_minutes: None,
            swap_version: SwapVersion::default(),
        })),
        None,
    );
    maker_orders.insert_raw(
        Uuid::from_bytes([1; 16]),
        "MARTY".into(),
        Arc::new(AsyncMutex::new(MakerOrder {
            uuid: Uuid::from_bytes([1; 16]),
            base: "MARTY".into(),
            rel: "DOC".into(),
            created_at: now_ms(),
            updated_at: Some(now_ms()),
            matches: HashMap::new(),
            max_base_vol: 0.into(),
            min_base_vol: 0.into(),
            price: 0.into(),
            started_swaps: vec![],
            conf_settings: None,
            changes_history: None,
            save_in_history: false,
            base_orderbook_ticker: None,
            rel_orderbook_ticker: None,
            p2p_privkey: None,
            timeout_in_minutes: None,
            swap_version: SwapVersion::default(),
        })),
        None,
    );
    maker_orders.insert_raw(
        Uuid::from_bytes([2; 16]),
        "MARTY".into(),
        Arc::new(AsyncMutex::new(MakerOrder {
            uuid: Uuid::from_bytes([2; 16]),
            base: "MARTY".into(),
            rel: "ETH".into(),
            created_at: now_ms(),
            updated_at: Some(now_ms()),
            matches: HashMap::new(),
            max_base_vol: 0.into(),
            min_base_vol: 0.into(),
            price: 0.into(),
            started_swaps: vec![],
            conf_settings: None,
            changes_history: None,
            save_in_history: false,
            base_orderbook_ticker: None,
            rel_orderbook_ticker: None,
            p2p_privkey: None,
            timeout_in_minutes: None,
            swap_version: SwapVersion::default(),
        })),
        None,
    );
    taker_orders.insert(Uuid::from_bytes([3; 16]), TakerOrder {
        matches: HashMap::new(),
        created_at: now_ms(),
        request: TakerRequest {
            base: "DOC".into(),
            rel: "MARTY".into(),
            uuid: Uuid::from_bytes([3; 16]),
            action: TakerAction::Buy,
            base_amount: 0.into(),
            rel_amount: 0.into(),
            dest_pub_key: H256Json::default(),
            sender_pubkey: H256Json::default(),
            match_by: MatchBy::Any,
            conf_settings: None,
            base_protocol_info: None,
            rel_protocol_info: None,
            swap_version: SwapVersion::default(),
        },
        order_type: OrderType::GoodTillCancelled,
        min_volume: 0.into(),
        timeout: 30,
        save_in_history: false,
        base_orderbook_ticker: None,
        rel_orderbook_ticker: None,
        p2p_privkey: None,
    });
    rx
}

#[test]
fn test_cancel_by_single_coin() {
    let ctx = MmCtxBuilder::default()
        .with_secp256k1_key_pair(key_pair_from_seed("123").unwrap())
        .into_mm_arc();
    let rx = prepare_for_cancel_by(&ctx);

    let connection = Connection::open_in_memory().unwrap();
    let _ = ctx.sqlite_connection.pin(Arc::new(Mutex::new(connection)));

    delete_my_maker_order.mock_safe(|_, _, _| MockResult::Return(Box::new(futures01::future::ok(()))));
    delete_my_taker_order.mock_safe(|_, _, _| MockResult::Return(Box::new(futures01::future::ok(()))));

    let (cancelled, _) = block_on(cancel_orders_by(&ctx, CancelBy::Coin { ticker: "DOC".into() })).unwrap();
    block_on(rx.take(2).collect::<Vec<_>>());
    assert!(cancelled.contains(&Uuid::from_bytes([0; 16])));
    assert!(cancelled.contains(&Uuid::from_bytes([1; 16])));
    assert!(!cancelled.contains(&Uuid::from_bytes([2; 16])));
    assert!(cancelled.contains(&Uuid::from_bytes([3; 16])));
}

#[test]
fn test_cancel_by_pair() {
    let ctx = MmCtxBuilder::default()
        .with_secp256k1_key_pair(key_pair_from_seed("123").unwrap())
        .into_mm_arc();
    let rx = prepare_for_cancel_by(&ctx);

    let connection = Connection::open_in_memory().unwrap();
    let _ = ctx.sqlite_connection.pin(Arc::new(Mutex::new(connection)));

    delete_my_maker_order.mock_safe(|_, _, _| MockResult::Return(Box::new(futures01::future::ok(()))));
    delete_my_taker_order.mock_safe(|_, _, _| MockResult::Return(Box::new(futures01::future::ok(()))));

    let (cancelled, _) = block_on(cancel_orders_by(&ctx, CancelBy::Pair {
        base: "DOC".into(),
        rel: "MARTY".into(),
    }))
    .unwrap();
    block_on(rx.take(1).collect::<Vec<_>>());
    assert!(cancelled.contains(&Uuid::from_bytes([0; 16])));
    assert!(!cancelled.contains(&Uuid::from_bytes([1; 16])));
    assert!(!cancelled.contains(&Uuid::from_bytes([2; 16])));
    assert!(cancelled.contains(&Uuid::from_bytes([3; 16])));
}

#[test]
fn test_cancel_by_all() {
    let ctx = MmCtxBuilder::default()
        .with_secp256k1_key_pair(key_pair_from_seed("123").unwrap())
        .into_mm_arc();
    let rx = prepare_for_cancel_by(&ctx);

    let connection = Connection::open_in_memory().unwrap();
    let _ = ctx.sqlite_connection.pin(Arc::new(Mutex::new(connection)));

    delete_my_maker_order.mock_safe(|_, _, _| MockResult::Return(Box::new(futures01::future::ok(()))));
    delete_my_taker_order.mock_safe(|_, _, _| MockResult::Return(Box::new(futures01::future::ok(()))));

    let (cancelled, _) = block_on(cancel_orders_by(&ctx, CancelBy::All)).unwrap();
    block_on(rx.take(3).collect::<Vec<_>>());
    assert!(cancelled.contains(&Uuid::from_bytes([0; 16])));
    assert!(cancelled.contains(&Uuid::from_bytes([1; 16])));
    assert!(cancelled.contains(&Uuid::from_bytes([2; 16])));
    assert!(cancelled.contains(&Uuid::from_bytes([3; 16])));
}

#[test]
// https://github.com/KomodoPlatform/atomicDEX-API/issues/607
fn test_taker_order_match_by() {
    let uuid = Uuid::new_v4();

    let mut not_matching_uuids = HashSet::new();
    not_matching_uuids.insert(Uuid::new_v4());
    let request = TakerRequest {
        base: "BASE".into(),
        rel: "REL".into(),
        uuid,
        dest_pub_key: H256Json::default(),
        sender_pubkey: H256Json::default(),
        base_amount: 10.into(),
        rel_amount: 10.into(),
        action: TakerAction::Buy,
        match_by: MatchBy::Orders(not_matching_uuids),
        conf_settings: None,
        base_protocol_info: None,
        rel_protocol_info: None,
        swap_version: SwapVersion::default(),
    };

    let mut order = TakerOrder {
        request,
        matches: HashMap::new(),
        created_at: now_ms(),
        order_type: OrderType::GoodTillCancelled,
        min_volume: 0.into(),
        timeout: 30,
        save_in_history: false,
        base_orderbook_ticker: None,
        rel_orderbook_ticker: None,
        p2p_privkey: None,
    };

    let reserved = MakerReserved {
        base: "BASE".into(),
        rel: "REL".into(),
        base_amount: 10.into(),
        rel_amount: 10.into(),
        sender_pubkey: H256Json::default(),
        dest_pub_key: H256Json::default(),
        maker_order_uuid: Uuid::new_v4(),
        taker_order_uuid: uuid,
        conf_settings: None,
        base_protocol_info: None,
        rel_protocol_info: None,
        swap_version: SwapVersion::default(),
    };

    assert_eq!(MatchReservedResult::NotMatched, order.match_reserved(&reserved));

    let mut matching_uuids = HashSet::new();
    matching_uuids.insert(reserved.maker_order_uuid);
    order.request.match_by = MatchBy::Orders(matching_uuids);
    assert_eq!(MatchReservedResult::Matched, order.match_reserved(&reserved));

    let mut not_matching_pubkeys = HashSet::new();
    not_matching_pubkeys.insert([1; 32].into());
    order.request.match_by = MatchBy::Pubkeys(not_matching_pubkeys);
    assert_eq!(MatchReservedResult::NotMatched, order.match_reserved(&reserved));

    let mut matching_pubkeys = HashSet::new();
    matching_pubkeys.insert(H256Json::default());
    order.request.match_by = MatchBy::Pubkeys(matching_pubkeys);
    assert_eq!(MatchReservedResult::Matched, order.match_reserved(&reserved));
}

#[test]
fn test_maker_order_was_updated() {
    let created_at = now_ms();
    let mut maker_order = MakerOrder {
        base: "BASE".into(),
        rel: "REL".into(),
        created_at,
        updated_at: Some(created_at),
        max_base_vol: 10.into(),
        min_base_vol: 0.into(),
        price: 1.into(),
        matches: HashMap::new(),
        started_swaps: Vec::new(),
        uuid: Uuid::new_v4(),
        conf_settings: None,
        changes_history: None,
        save_in_history: false,
        base_orderbook_ticker: None,
        rel_orderbook_ticker: None,
        p2p_privkey: None,
        timeout_in_minutes: None,
        swap_version: SwapVersion::default(),
    };
    let mut update_msg = MakerOrderUpdated::new(maker_order.uuid);
    update_msg.with_new_price(BigRational::from_integer(2.into()));

    std::thread::sleep(Duration::from_secs(1));

    maker_order.apply_updated(&update_msg);
    assert!(maker_order.was_updated());
}

#[test]
fn lp_connect_start_bob_should_not_be_invoked_if_order_match_already_connected() {
    let order_json = r#"{"max_base_vol":"1","max_base_vol_rat":[[1,[1]],[1,[1]]],"min_base_vol":"0","min_base_vol_rat":[[0,[]],[1,[1]]],"price":"1","price_rat":[[1,[1]],[1,[1]]],"created_at":1589265312093,"updated_at":1589265312093,"base":"ETH","rel":"JST","matches":{"2f9afe84-7a89-4194-8947-45fba563118f":{"request":{"base":"ETH","rel":"JST","base_amount":"0.1","base_amount_rat":[[1,[1]],[1,[10]]],"rel_amount":"0.2","rel_amount_rat":[[1,[1]],[1,[5]]],"action":"Buy","uuid":"2f9afe84-7a89-4194-8947-45fba563118f","method":"request","sender_pubkey":"031d4256c4bc9f99ac88bf3dba21773132281f65f9bf23a59928bce08961e2f3","dest_pub_key":"0000000000000000000000000000000000000000000000000000000000000000","match_by":{"type":"Any"}},"reserved":{"base":"ETH","rel":"JST","base_amount":"0.1","base_amount_rat":[[1,[1]],[1,[10]]],"rel_amount":"0.1","rel_amount_rat":[[1,[1]],[1,[10]]],"taker_order_uuid":"2f9afe84-7a89-4194-8947-45fba563118f","maker_order_uuid":"5f6516ea-ccaa-453a-9e37-e1c2c0d527e3","method":"reserved","sender_pubkey":"c6a78589e18b482aea046975e6d0acbdea7bf7dbf04d9d5bd67fda917815e3ed","dest_pub_key":"031d4256c4bc9f99ac88bf3dba21773132281f65f9bf23a59928bce08961e2f3"},"connect":{"taker_order_uuid":"2f9afe84-7a89-4194-8947-45fba563118f","maker_order_uuid":"5f6516ea-ccaa-453a-9e37-e1c2c0d527e3","method":"connect","sender_pubkey":"031d4256c4bc9f99ac88bf3dba21773132281f65f9bf23a59928bce08961e2f3","dest_pub_key":"c6a78589e18b482aea046975e6d0acbdea7bf7dbf04d9d5bd67fda917815e3ed"},"connected":{"taker_order_uuid":"2f9afe84-7a89-4194-8947-45fba563118f","maker_order_uuid":"5f6516ea-ccaa-453a-9e37-e1c2c0d527e3","method":"connected","sender_pubkey":"c6a78589e18b482aea046975e6d0acbdea7bf7dbf04d9d5bd67fda917815e3ed","dest_pub_key":"031d4256c4bc9f99ac88bf3dba21773132281f65f9bf23a59928bce08961e2f3"},"last_updated":1589265314408}},"started_swaps":["2f9afe84-7a89-4194-8947-45fba563118f"],"uuid":"5f6516ea-ccaa-453a-9e37-e1c2c0d527e3"}"#;
    let maker_order: MakerOrder = json::from_str(order_json).unwrap();
    let ctx = MmCtxBuilder::default()
        .with_secp256k1_key_pair(
            key_pair_from_seed("also shoot benefit prefer juice shell elder veteran woman mimic image kidney").unwrap(),
        )
        .into_mm_arc();
    let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
    {
        let order_arc = Arc::new(AsyncMutex::new(maker_order.clone()));
        ordermatch_ctx
            .maker_orders_ctx
            .lock()
            .add_order(&maker_order, order_arc);
    }

    static mut CONNECT_START_CALLED: bool = false;
    lp_connect_start_bob.mock_safe(|_, _, _| {
        let _: () = unsafe { CONNECT_START_CALLED = true };
        MockResult::Return(())
    });

    let connect: TakerConnect = json::from_str(r#"{"taker_order_uuid":"2f9afe84-7a89-4194-8947-45fba563118f","maker_order_uuid":"5f6516ea-ccaa-453a-9e37-e1c2c0d527e3","method":"connect","sender_pubkey":"031d4256c4bc9f99ac88bf3dba21773132281f65f9bf23a59928bce08961e2f3","dest_pub_key":"c6a78589e18b482aea046975e6d0acbdea7bf7dbf04d9d5bd67fda917815e3ed"}"#).unwrap();
    block_on(process_taker_connect(ctx, connect.sender_pubkey, connect));
    assert!(unsafe { !CONNECT_START_CALLED });
}

#[test]
fn should_process_request_only_once() {
    let order_json = r#"{"max_base_vol":"1","max_base_vol_rat":[[1,[1]],[1,[1]]],"min_base_vol":"0","min_base_vol_rat":[[0,[]],[1,[1]]],"price":"1","price_rat":[[1,[1]],[1,[1]]],"created_at":1589265312093,"updated_at":1589265312093,"base":"ETH","rel":"JST","matches":{"2f9afe84-7a89-4194-8947-45fba563118f":{"request":{"base":"ETH","rel":"JST","base_amount":"0.1","base_amount_rat":[[1,[1]],[1,[10]]],"rel_amount":"0.2","rel_amount_rat":[[1,[1]],[1,[5]]],"action":"Buy","uuid":"2f9afe84-7a89-4194-8947-45fba563118f","method":"request","sender_pubkey":"031d4256c4bc9f99ac88bf3dba21773132281f65f9bf23a59928bce08961e2f3","dest_pub_key":"0000000000000000000000000000000000000000000000000000000000000000","match_by":{"type":"Any"}},"reserved":{"base":"ETH","rel":"JST","base_amount":"0.1","base_amount_rat":[[1,[1]],[1,[10]]],"rel_amount":"0.1","rel_amount_rat":[[1,[1]],[1,[10]]],"taker_order_uuid":"2f9afe84-7a89-4194-8947-45fba563118f","maker_order_uuid":"5f6516ea-ccaa-453a-9e37-e1c2c0d527e3","method":"reserved","sender_pubkey":"c6a78589e18b482aea046975e6d0acbdea7bf7dbf04d9d5bd67fda917815e3ed","dest_pub_key":"031d4256c4bc9f99ac88bf3dba21773132281f65f9bf23a59928bce08961e2f3"},"connect":{"taker_order_uuid":"2f9afe84-7a89-4194-8947-45fba563118f","maker_order_uuid":"5f6516ea-ccaa-453a-9e37-e1c2c0d527e3","method":"connect","sender_pubkey":"031d4256c4bc9f99ac88bf3dba21773132281f65f9bf23a59928bce08961e2f3","dest_pub_key":"c6a78589e18b482aea046975e6d0acbdea7bf7dbf04d9d5bd67fda917815e3ed"},"connected":{"taker_order_uuid":"2f9afe84-7a89-4194-8947-45fba563118f","maker_order_uuid":"5f6516ea-ccaa-453a-9e37-e1c2c0d527e3","method":"connected","sender_pubkey":"c6a78589e18b482aea046975e6d0acbdea7bf7dbf04d9d5bd67fda917815e3ed","dest_pub_key":"031d4256c4bc9f99ac88bf3dba21773132281f65f9bf23a59928bce08961e2f3"},"last_updated":1589265314408}},"started_swaps":["2f9afe84-7a89-4194-8947-45fba563118f"],"uuid":"5f6516ea-ccaa-453a-9e37-e1c2c0d527e3"}"#;
    let maker_order: MakerOrder = json::from_str(order_json).unwrap();
    let uuid = maker_order.uuid;
    let ctx = MmCtxBuilder::default()
        .with_secp256k1_key_pair(
            key_pair_from_seed("also shoot benefit prefer juice shell elder veteran woman mimic image kidney").unwrap(),
        )
        .into_mm_arc();
    let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
    {
        let order_arc = Arc::new(AsyncMutex::new(maker_order.clone()));
        ordermatch_ctx
            .maker_orders_ctx
            .lock()
            .add_order(&maker_order, order_arc);
    }
    let request: TakerRequest = json::from_str(
        r#"{"base":"ETH","rel":"JST","base_amount":"0.1","base_amount_rat":[[1,[1]],[1,[10]]],"rel_amount":"0.2","rel_amount_rat":[[1,[1]],[1,[5]]],"action":"Buy","uuid":"2f9afe84-7a89-4194-8947-45fba563118f","method":"request","sender_pubkey":"031d4256c4bc9f99ac88bf3dba21773132281f65f9bf23a59928bce08961e2f3","dest_pub_key":"0000000000000000000000000000000000000000000000000000000000000000","match_by":{"type":"Any"}}"#,
    ).unwrap();
    block_on(process_taker_request(ctx, Default::default(), request));
    let maker_orders = ordermatch_ctx.maker_orders_ctx.lock();
    let order_mutex = maker_orders.get_order(&uuid).unwrap();
    let order = block_on(order_mutex.lock());
    // when new request is processed match is replaced with new instance resetting
    // connect and connected to None so by checking is_some we check that request message is ignored
    assert!(order
        .matches
        .get(&"2f9afe84-7a89-4194-8947-45fba563118f".parse().unwrap())
        .unwrap()
        .connect
        .is_some());
    assert!(order
        .matches
        .get(&"2f9afe84-7a89-4194-8947-45fba563118f".parse().unwrap())
        .unwrap()
        .connected
        .is_some());
}

#[test]
fn test_choose_maker_confs_settings() {
    let coin = TestCoin::default().into();
    // no confs set
    let taker_order = TakerOrderBuilder::new(&coin, &coin).build_unchecked();
    TestCoin::requires_notarization.mock_safe(|_| MockResult::Return(true));
    TestCoin::required_confirmations.mock_safe(|_| MockResult::Return(8));
    let settings = choose_maker_confs_and_notas(None, &taker_order.request, &coin, &coin);
    // should pick settings from coin configuration
    assert!(settings.maker_coin_nota);
    assert_eq!(settings.maker_coin_confs, 8);
    assert!(settings.taker_coin_nota);
    assert_eq!(settings.taker_coin_confs, 8);

    let maker_conf_settings = OrderConfirmationsSettings {
        base_confs: 1,
        base_nota: false,
        rel_confs: 1,
        rel_nota: false,
    };
    // no confs set
    let taker_order = TakerOrderBuilder::new(&coin, &coin).build_unchecked();
    let settings = choose_maker_confs_and_notas(Some(maker_conf_settings), &taker_order.request, &coin, &coin);
    // should pick settings from maker order
    assert!(!settings.maker_coin_nota);
    assert_eq!(settings.maker_coin_confs, 1);
    assert!(!settings.taker_coin_nota);
    assert_eq!(settings.taker_coin_confs, 1);

    let maker_conf_settings = OrderConfirmationsSettings {
        base_confs: 10,
        base_nota: true,
        rel_confs: 1,
        rel_nota: false,
    };
    let taker_conf_settings = OrderConfirmationsSettings {
        base_confs: 5,
        base_nota: false,
        rel_confs: 5,
        rel_nota: false,
    };
    let taker_order = TakerOrderBuilder::new(&coin, &coin)
        .with_conf_settings(taker_conf_settings)
        .build_unchecked();
    let settings = choose_maker_confs_and_notas(Some(maker_conf_settings), &taker_order.request, &coin, &coin);
    // should pick settings from taker request because taker will wait less time for our
    // payment confirmation
    assert!(!settings.maker_coin_nota);
    assert_eq!(settings.maker_coin_confs, 5);
    assert!(!settings.taker_coin_nota);
    assert_eq!(settings.taker_coin_confs, 1);

    let maker_conf_settings = OrderConfirmationsSettings {
        base_confs: 10,
        base_nota: false,
        rel_confs: 1,
        rel_nota: false,
    };
    let taker_conf_settings = OrderConfirmationsSettings {
        base_confs: 1000,
        base_nota: true,
        rel_confs: 1000,
        rel_nota: true,
    };
    let taker_order = TakerOrderBuilder::new(&coin, &coin)
        .with_conf_settings(taker_conf_settings)
        .build_unchecked();
    let settings = choose_maker_confs_and_notas(Some(maker_conf_settings), &taker_order.request, &coin, &coin);
    // keep using our settings allowing taker to wait for our payment conf as much as he likes
    assert!(!settings.maker_coin_nota);
    assert_eq!(settings.maker_coin_confs, 10);
    assert!(!settings.taker_coin_nota);
    assert_eq!(settings.taker_coin_confs, 1);

    let maker_conf_settings = OrderConfirmationsSettings {
        base_confs: 10,
        base_nota: false,
        rel_confs: 2,
        rel_nota: true,
    };

    let taker_conf_settings = OrderConfirmationsSettings {
        rel_confs: 1,
        rel_nota: false,
        base_confs: 1,
        base_nota: false,
    };
    let taker_order = TakerOrderBuilder::new(&coin, &coin)
        .with_conf_settings(taker_conf_settings)
        .build_unchecked();
    let settings = choose_maker_confs_and_notas(Some(maker_conf_settings), &taker_order.request, &coin, &coin);

    // Taker conf settings should not have any effect on maker conf requirements for taker payment
    assert!(settings.taker_coin_nota);
    assert_eq!(settings.taker_coin_confs, 2);

    let maker_conf_settings = OrderConfirmationsSettings {
        base_confs: 10,
        base_nota: true,
        rel_confs: 1,
        rel_nota: false,
    };
    // Pair is reversed for TakerAction::Sell
    let taker_conf_settings = OrderConfirmationsSettings {
        rel_confs: 5,
        rel_nota: false,
        base_confs: 5,
        base_nota: false,
    };
    let taker_order = TakerOrderBuilder::new(&coin, &coin)
        .with_conf_settings(taker_conf_settings)
        .with_action(TakerAction::Sell)
        .build_unchecked();
    let settings = choose_maker_confs_and_notas(Some(maker_conf_settings), &taker_order.request, &coin, &coin);
    // should pick settings from taker request because taker will wait less time for our
    // payment confirmation
    assert!(!settings.maker_coin_nota);
    assert_eq!(settings.maker_coin_confs, 5);
    assert!(!settings.taker_coin_nota);
    assert_eq!(settings.taker_coin_confs, 1);
}

#[test]
fn test_choose_taker_confs_settings_buy_action() {
    let coin = TestCoin::default().into();

    // no confs and notas set
    let taker_order = TakerOrderBuilder::new(&coin, &coin).build_unchecked();
    // no confs and notas set
    let maker_reserved = MakerReserved::default();
    TestCoin::requires_notarization.mock_safe(|_| MockResult::Return(true));
    TestCoin::required_confirmations.mock_safe(|_| MockResult::Return(8));
    let settings = choose_taker_confs_and_notas(&taker_order.request, &maker_reserved, &coin, &coin);
    // should pick settings from coins
    assert!(settings.taker_coin_nota);
    assert_eq!(settings.taker_coin_confs, 8);
    assert!(settings.maker_coin_nota);
    assert_eq!(settings.maker_coin_confs, 8);

    let taker_conf_settings = OrderConfirmationsSettings {
        base_confs: 5,
        base_nota: true,
        rel_confs: 4,
        rel_nota: false,
    };
    let taker_order = TakerOrderBuilder::new(&coin, &coin)
        .with_conf_settings(taker_conf_settings)
        .build_unchecked();
    // no confs and notas set
    let maker_reserved = MakerReserved::default();
    let settings = choose_taker_confs_and_notas(&taker_order.request, &maker_reserved, &coin, &coin);
    // should pick settings from taker request
    // as action is buy my_coin is rel and other coin is base
    assert!(!settings.taker_coin_nota);
    assert_eq!(settings.taker_coin_confs, 4);
    assert!(settings.maker_coin_nota);
    assert_eq!(settings.maker_coin_confs, 5);

    let taker_conf_settings = OrderConfirmationsSettings {
        base_confs: 2,
        base_nota: true,
        rel_confs: 2,
        rel_nota: true,
    };
    let taker_order = TakerOrderBuilder::new(&coin, &coin)
        .with_conf_settings(taker_conf_settings)
        .build_unchecked();
    let mut maker_reserved = MakerReserved::default();
    let maker_conf_settings = OrderConfirmationsSettings {
        rel_confs: 1,
        rel_nota: false,
        base_confs: 2,
        base_nota: true,
    };
    maker_reserved.conf_settings = Some(maker_conf_settings);
    let settings = choose_taker_confs_and_notas(&taker_order.request, &maker_reserved, &coin, &coin);
    // should pick settings from maker reserved if he requires less confs
    // as action is buy my_coin is rel and other coin is base in request
    assert!(!settings.taker_coin_nota);
    assert_eq!(settings.taker_coin_confs, 1);
    assert!(settings.maker_coin_nota);
    assert_eq!(settings.maker_coin_confs, 2);

    let taker_conf_settings = OrderConfirmationsSettings {
        base_confs: 2,
        base_nota: true,
        rel_confs: 1,
        rel_nota: false,
    };
    let taker_order = TakerOrderBuilder::new(&coin, &coin)
        .with_conf_settings(taker_conf_settings)
        .build_unchecked();
    let mut maker_reserved = MakerReserved::default();
    let maker_conf_settings = OrderConfirmationsSettings {
        rel_confs: 2,
        rel_nota: true,
        base_confs: 2,
        base_nota: true,
    };
    maker_reserved.conf_settings = Some(maker_conf_settings);
    let settings = choose_taker_confs_and_notas(&taker_order.request, &maker_reserved, &coin, &coin);
    // should allow maker to use more confirmations than we require, but it shouldn't affect our settings
    // as action is buy my_coin is rel and other coin is base in request
    assert!(!settings.taker_coin_nota);
    assert_eq!(settings.taker_coin_confs, 1);
    assert!(settings.maker_coin_nota);
    assert_eq!(settings.maker_coin_confs, 2);

    let taker_conf_settings = OrderConfirmationsSettings {
        base_confs: 2,
        base_nota: true,
        rel_confs: 1,
        rel_nota: false,
    };
    let taker_order = TakerOrderBuilder::new(&coin, &coin)
        .with_conf_settings(taker_conf_settings)
        .build_unchecked();
    let mut maker_reserved = MakerReserved::default();
    let maker_conf_settings = OrderConfirmationsSettings {
        base_confs: 1,
        base_nota: false,
        rel_confs: 2,
        rel_nota: true,
    };
    maker_reserved.conf_settings = Some(maker_conf_settings);
    let settings = choose_taker_confs_and_notas(&taker_order.request, &maker_reserved, &coin, &coin);
    // maker settings should have no effect on other_coin_confs and other_coin_nota
    // as action is buy my_coin is rel and other coin is base in request
    assert!(!settings.taker_coin_nota);
    assert_eq!(settings.taker_coin_confs, 1);
    assert!(settings.maker_coin_nota);
    assert_eq!(settings.maker_coin_confs, 2);
}

#[test]
fn test_choose_taker_confs_settings_sell_action() {
    let coin = TestCoin::default().into();

    // no confs and notas set
    let taker_order = TakerOrderBuilder::new(&coin, &coin)
        .with_action(TakerAction::Sell)
        .build_unchecked();
    // no confs and notas set
    let maker_reserved = MakerReserved::default();
    TestCoin::requires_notarization.mock_safe(|_| MockResult::Return(true));
    TestCoin::required_confirmations.mock_safe(|_| MockResult::Return(8));
    let settings = choose_taker_confs_and_notas(&taker_order.request, &maker_reserved, &coin, &coin);
    // should pick settings from coins
    assert!(settings.taker_coin_nota);
    assert_eq!(settings.taker_coin_confs, 8);
    assert!(settings.maker_coin_nota);
    assert_eq!(settings.maker_coin_confs, 8);

    let taker_conf_settings = OrderConfirmationsSettings {
        base_confs: 4,
        base_nota: false,
        rel_confs: 5,
        rel_nota: true,
    };
    let taker_order = TakerOrderBuilder::new(&coin, &coin)
        .with_action(TakerAction::Sell)
        .with_conf_settings(taker_conf_settings)
        .build_unchecked();
    // no confs and notas set
    let maker_reserved = MakerReserved::default();
    let settings = choose_taker_confs_and_notas(&taker_order.request, &maker_reserved, &coin, &coin);
    // should pick settings from taker request
    // as action is sell my_coin is base and other coin is rel in request
    assert!(!settings.taker_coin_nota);
    assert_eq!(settings.taker_coin_confs, 4);
    assert!(settings.maker_coin_nota);
    assert_eq!(settings.maker_coin_confs, 5);

    let taker_conf_settings = OrderConfirmationsSettings {
        base_confs: 2,
        base_nota: true,
        rel_confs: 2,
        rel_nota: true,
    };
    let taker_order = TakerOrderBuilder::new(&coin, &coin)
        .with_action(TakerAction::Sell)
        .with_conf_settings(taker_conf_settings)
        .build_unchecked();
    let mut maker_reserved = MakerReserved::default();
    let maker_conf_settings = OrderConfirmationsSettings {
        base_confs: 2,
        base_nota: true,
        rel_confs: 1,
        rel_nota: false,
    };
    maker_reserved.conf_settings = Some(maker_conf_settings);
    let settings = choose_taker_confs_and_notas(&taker_order.request, &maker_reserved, &coin, &coin);
    // should pick settings from maker reserved if he requires less confs
    // as action is sell my_coin is base and other coin is rel in request
    assert!(!settings.taker_coin_nota);
    assert_eq!(settings.taker_coin_confs, 1);
    assert!(settings.maker_coin_nota);
    assert_eq!(settings.maker_coin_confs, 2);

    let taker_conf_settings = OrderConfirmationsSettings {
        base_confs: 1,
        base_nota: false,
        rel_confs: 2,
        rel_nota: true,
    };
    let taker_order = TakerOrderBuilder::new(&coin, &coin)
        .with_action(TakerAction::Sell)
        .with_conf_settings(taker_conf_settings)
        .build_unchecked();
    let mut maker_reserved = MakerReserved::default();
    let maker_conf_settings = OrderConfirmationsSettings {
        rel_confs: 2,
        rel_nota: true,
        base_confs: 1,
        base_nota: false,
    };
    maker_reserved.conf_settings = Some(maker_conf_settings);
    let settings = choose_taker_confs_and_notas(&taker_order.request, &maker_reserved, &coin, &coin);
    // should allow maker to use more confirmations than we require, but it shouldn't affect our settings
    // as action is sell my_coin is base and other coin is rel in request
    assert!(!settings.taker_coin_nota);
    assert_eq!(settings.taker_coin_confs, 1);
    assert!(settings.maker_coin_nota);
    assert_eq!(settings.maker_coin_confs, 2);

    let taker_conf_settings = OrderConfirmationsSettings {
        base_confs: 1,
        base_nota: false,
        rel_confs: 2,
        rel_nota: true,
    };
    let taker_order = TakerOrderBuilder::new(&coin, &coin)
        .with_action(TakerAction::Sell)
        .with_conf_settings(taker_conf_settings)
        .build_unchecked();
    let mut maker_reserved = MakerReserved::default();
    let maker_conf_settings = OrderConfirmationsSettings {
        rel_confs: 2,
        rel_nota: true,
        base_confs: 1,
        base_nota: false,
    };
    maker_reserved.conf_settings = Some(maker_conf_settings);
    let settings = choose_taker_confs_and_notas(&taker_order.request, &maker_reserved, &coin, &coin);
    // maker settings should have no effect on other_coin_confs and other_coin_nota
    // as action is sell my_coin is base and other coin is rel in request
    assert!(!settings.taker_coin_nota);
    assert_eq!(settings.taker_coin_confs, 1);
    assert!(settings.maker_coin_nota);
    assert_eq!(settings.maker_coin_confs, 2);
}

fn make_ctx_for_tests() -> (MmArc, String, [u8; 32]) {
    let ctx = MmArc::new(MmCtx::default());
    ctx.init_metrics().unwrap();
    ctx.secp256k1_key_pair
        .pin(key_pair_from_seed("passphrase").unwrap())
        .unwrap();
    let secret = *ctx.secp256k1_key_pair().private().secret;
    let pubkey = hex::encode(&**ctx.secp256k1_key_pair().public());
    (ctx, pubkey, secret)
}

#[test]
fn test_orderbook_address_handles_non_utxo_protocols_without_panic() {
    let (ctx, pubkey, _secret) = make_ctx_for_tests();
    let tendermint_conf = json::json!({
        "protocol": {
            "type": "TENDERMINT",
            "protocol_data": {
                "denom": "uiris",
                "decimals": 6,
                "account_prefix": "iaa",
                "chain_id": "irishub-1"
            }
        }
    });
    let address = orderbook_address(&ctx, "IRIS", &tendermint_conf, &pubkey, UtxoAddressFormat::Standard)
        .expect("Tendermint address must derive from order pubkey");
    match address {
        OrderbookAddress::Transparent(addr) => assert!(addr.starts_with("iaa")),
        OrderbookAddress::Shielded => panic!("Tendermint orderbook address must be transparent"),
    }

    // Sia's display address can't be derived from the order's P2P
    // secp256k1 identity (it signs with ed25519), so -- like ZHTLC -- it
    // takes the shielded outcome rather than being dropped from the
    // orderbook as unsupported (CRD ch.32 R-F6b; see also the
    // orderbook_address_tests module in ordermatch_orderbook.rs).
    let sia_conf = json::json!({ "protocol": { "type": "SIA" } });
    let address = orderbook_address(&ctx, "SC", &sia_conf, &pubkey, UtxoAddressFormat::Standard)
        .expect("Sia orderbook address must resolve to the shielded outcome, not an error");
    assert!(matches!(address, OrderbookAddress::Shielded));
}

pub(super) fn make_random_orders(
    pubkey: String,
    _secret: &[u8; 32],
    base: String,
    rel: String,
    n: usize,
) -> Vec<OrderbookItem> {
    let mut rng = rand::thread_rng();
    let mut orders = Vec::with_capacity(n);
    for _i in 0..n {
        let numer: u64 = rng.gen_range(2000..10000000);
        let order = new_protocol::MakerOrderCreated {
            uuid: Uuid::new_v4().into(),
            base: base.clone(),
            rel: rel.clone(),
            price: BigRational::new(numer.into(), 1000000.into()),
            max_volume: BigRational::from_integer(1.into()),
            min_volume: BigRational::from_integer(0.into()),
            conf_settings: OrderConfirmationsSettings::default(),
            created_at: now_ms() / 1000,
            timestamp: now_ms() / 1000,
            pair_trie_root: H64::default(),
            base_protocol_info: vec![],
            rel_protocol_info: vec![],
        };

        orders.push((order, pubkey.clone()).into());
    }

    orders
}

fn pubkey_and_secret_for_test(passphrase: &str) -> (String, [u8; 32]) {
    let key_pair = key_pair_from_seed(passphrase).unwrap();
    let pubkey = hex::encode(&**key_pair.public());
    let secret = *key_pair.private().secret;
    (pubkey, secret)
}

fn p2p_context_mock() -> (mpsc::Sender<AdexBehaviourCmd>, mpsc::Receiver<AdexBehaviourCmd>) {
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let cmd_sender = cmd_tx.clone();
    P2PContext::fetch_from_mm_arc.mock_safe(move |_| MockResult::Return(Arc::new(P2PContext::new(cmd_sender.clone()))));
    (cmd_tx, cmd_rx)
}

#[test]
fn test_process_get_orderbook_request() {
    const ORDERS_NUMBER: usize = 10;

    let (ctx, _pubkey, _secret) = make_ctx_for_tests();
    let (pubkey1, secret1) = pubkey_and_secret_for_test("passphrase-1");
    let (pubkey2, secret2) = pubkey_and_secret_for_test("passphrase-2");
    let (pubkey3, secret3) = pubkey_and_secret_for_test("passphrase-3");

    let mut pubkey1_orders = make_random_orders(pubkey1.clone(), &secret1, "DOC".into(), "MARTY".into(), ORDERS_NUMBER);
    let mut pubkey2_orders = make_random_orders(pubkey2.clone(), &secret2, "MARTY".into(), "DOC".into(), ORDERS_NUMBER);
    let mut pubkey3_orders = make_random_orders(pubkey3.clone(), &secret3, "DOC".into(), "MARTY".into(), ORDERS_NUMBER);
    pubkey3_orders.extend_from_slice(&make_random_orders(
        pubkey3.clone(),
        &secret3,
        "MARTY".into(),
        "DOC".into(),
        ORDERS_NUMBER,
    ));

    pubkey1_orders.sort_unstable_by_key(|x| x.uuid);
    pubkey2_orders.sort_unstable_by_key(|x| x.uuid);
    pubkey3_orders.sort_unstable_by_key(|x| x.uuid);

    let mut orders_by_pubkeys = HashMap::new();
    orders_by_pubkeys.insert(pubkey1, pubkey1_orders);
    orders_by_pubkeys.insert(pubkey2, pubkey2_orders);
    orders_by_pubkeys.insert(pubkey3, pubkey3_orders);

    let ordermatch_ctx = Arc::new(OrdermatchContext::default());
    let ordermatch_ctx_clone = ordermatch_ctx.clone();
    OrdermatchContext::from_ctx.mock_safe(move |_| MockResult::Return(Ok(ordermatch_ctx_clone.clone())));

    {
        let mut orderbook = ordermatch_ctx.orderbook.lock();
        for order in orders_by_pubkeys.values().flatten() {
            let ops = orderbook.index_insert_or_update(order.clone());
            orderbook.queue_trie_ops(&ordermatch_ctx.trie_ops_tx, ops);
        }
    }
    // Flush the trie worker so process_get_orderbook_request can see pubkeys_state
    ordermatch_ctx.wait_trie_ops_flushed();

    let encoded = process_get_orderbook_request(ctx.clone(), "DOC".into(), "MARTY".into())
        .unwrap()
        .unwrap();

    let orderbook = decode_message::<GetOrderbookRes>(&encoded).unwrap();
    for (pubkey, item) in orderbook.pubkey_orders {
        let expected = orders_by_pubkeys
            .get(&pubkey)
            .unwrap_or_else(|| panic!("!best_orders_by_pubkeys is expected to contain {:?}", pubkey));

        let mut actual: Vec<OrderbookItem> = item
            .orders
            .iter()
            .map(|(_uuid, order): &(Uuid, OrderbookP2PItem)| {
                OrderbookItem::from_p2p_and_info(
                    order.clone(),
                    BaseRelProtocolInfo::default(),
                    Some(OrderConfirmationsSettings::default()),
                )
            })
            .collect();
        actual.sort_unstable_by_key(|x| x.uuid);
        log!([pubkey]"-"[actual.len()]);
        assert_eq!(actual, *expected);
    }
}

#[test]
fn test_process_get_orderbook_request_limit() {
    let (ctx, pubkey, secret) = make_ctx_for_tests();

    let ordermatch_ctx = Arc::new(OrdermatchContext::default());
    let ordermatch_ctx_clone = ordermatch_ctx.clone();
    OrdermatchContext::from_ctx.mock_safe(move |_| MockResult::Return(Ok(ordermatch_ctx_clone.clone())));

    let orders = make_random_orders(
        pubkey,
        &secret,
        "DOC".into(),
        "MARTY".into(),
        MAX_ORDERS_NUMBER_IN_ORDERBOOK_RESPONSE + 1,
    );

    {
        let mut orderbook = ordermatch_ctx.orderbook.lock();
        for order in orders {
            let ops = orderbook.index_insert_or_update(order);
            orderbook.queue_trie_ops(&ordermatch_ctx.trie_ops_tx, ops);
        }
    }
    ordermatch_ctx.wait_trie_ops_flushed();

    let err = process_get_orderbook_request(ctx.clone(), "DOC".into(), "MARTY".into()).expect_err("Expected an error");

    log!("error: "(err));
    assert!(err.contains("Orderbook too large"));
}

#[test]
fn test_request_and_fill_orderbook() {
    const PUBKEYS_NUMBER: usize = 3;
    const ORDERS_NUMBER: usize = 10;

    let (ctx, _pubkey, _secret) = make_ctx_for_tests();
    let (_, mut cmd_rx) = p2p_context_mock();

    let other_pubkeys: Vec<(String, [u8; 32])> = (0..PUBKEYS_NUMBER)
        .map(|idx| {
            let passphrase = format!("passphrase-{}", idx);
            pubkey_and_secret_for_test(&passphrase)
        })
        .collect();
    let expected_orders: HashMap<String, Vec<(Uuid, OrderbookItem)>> = other_pubkeys
        .iter()
        .map(|(pubkey, secret)| {
            let orders: Vec<_> =
                make_random_orders(pubkey.clone(), secret, "DOC".into(), "MARTY".into(), ORDERS_NUMBER)
                    .into_iter()
                    .map(|order| (order.uuid, order))
                    .collect();
            (pubkey.clone(), orders)
        })
        .collect();

    // insert extra (DOC, MARTY) orders that must be removed from our trie before the orderbook is filled
    {
        let (pubkey, secret) = &other_pubkeys[0];
        for extra_order in make_random_orders(pubkey.clone(), secret, "DOC".into(), "MARTY".into(), 2) {
            insert_or_update_order(&ctx, extra_order);
        }
    }

    let expected_request = P2PRequest::Ordermatch(OrdermatchRequest::GetOrderbook {
        base: "DOC".into(),
        rel: "MARTY".into(),
    });

    let orders = expected_orders.clone();
    spawn(async move {
        let cmd = cmd_rx.next().await.unwrap();
        let (req, response_tx) = if let AdexBehaviourCmd::RequestAnyRelay { req, response_tx } = cmd {
            (req, response_tx)
        } else {
            panic!("Unexpected cmd");
        };

        // check if the received request is expected
        let actual = decode_message::<P2PRequest>(&req).unwrap();
        assert_eq!(actual, expected_request);

        let mut conf_infos = HashMap::new();
        let result = orders
            .into_iter()
            .map(|(pubkey, orders)| {
                let orders = orders
                    .into_iter()
                    .map(|(uuid, order)| {
                        if let Some(conf_settings) = order.conf_settings {
                            conf_infos.insert(uuid, conf_settings);
                        }
                        (uuid, order.into())
                    })
                    .collect();
                let item = GetOrderbookPubkeyItem {
                    orders,
                    last_keep_alive: now_ms() / 1000,
                    last_signed_pubkey_payload: vec![],
                };
                (pubkey, item)
            })
            .collect();
        let orderbook = GetOrderbookRes {
            pubkey_orders: result,
            protocol_infos: HashMap::new(),
            conf_infos,
        };
        let encoded = encode_message(&orderbook).unwrap();

        // send the response through the response channel
        response_tx.send(Some((PeerId::random(), encoded))).unwrap();
    });

    block_on(request_and_fill_orderbook(&ctx, "DOC", "MARTY")).unwrap();
    flush_trie(&ctx);

    // check if the best asks and bids are in the orderbook
    let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
    let orderbook = ordermatch_ctx.orderbook.lock();
    let trie_store = ordermatch_ctx.trie_store.lock();

    let expected = expected_orders.values().flat_map(|orders| orders.clone()).collect();
    assert_eq!(orderbook.order_set, expected);

    let expected = expected_orders.values().flatten().map(|(uuid, _order)| *uuid).collect();
    let unordered = orderbook
        .unordered
        .get(&("DOC".to_owned(), "MARTY".to_owned()))
        .expect("No (DOC, MARTY) in unordered container");
    assert_eq!(*unordered, expected);

    let expected = expected_orders
        .values()
        .flatten()
        .map(|(uuid, order)| OrderedByPriceOrder {
            uuid: *uuid,
            price: order.price.clone().into(),
        })
        .collect();
    let ordered = orderbook
        .ordered
        .get(&("DOC".to_owned(), "MARTY".to_owned()))
        .expect("No (DOC, MARTY) in unordered container");
    assert_eq!(*ordered, expected);

    let doc_marty_pair = alb_ordered_pair("DOC", "MARTY");
    for (pubkey, orders) in expected_orders {
        let pubkey_state = trie_store
            .pubkeys_state
            .get(&pubkey)
            .unwrap_or_else(|| panic!("!pubkey_state.get() {} pubkey", pubkey));

        let expected = orders
            .iter()
            .map(|(uuid, _order)| (*uuid, doc_marty_pair.clone()))
            .collect();
        assert_eq!(pubkey_state.orders_uuids, expected);

        let root = pubkey_state
            .trie_roots
            .get(&doc_marty_pair)
            .unwrap_or_else(|| panic!("!pubkey_state.trie_roots.get() {}", doc_marty_pair));

        // check if the root contains only expected orders
        let trie = TrieDB::<Layout>::new(&trie_store.memory_db, root).expect("!TrieDB::new()");
        let mut in_trie: Vec<(Uuid, OrderbookItem)> = trie
            .iter()
            .expect("!TrieDB::iter()")
            .map(|key_value| {
                let (key, _) = key_value.expect("Iterator returned an error");
                let key = TryFromBytes::try_from_bytes(key).expect("!try_from_bytes() key");
                let value = orderbook.order_set.get(&key).cloned().unwrap();
                (key, value)
            })
            .collect();

        in_trie.sort_by_key(|x| x.0);
        let mut expected = orders;
        expected.sort_by_key(|x| x.0);
        assert_eq!(in_trie, expected);
    }
}

/*
#[test]
fn test_process_order_keep_alive_requested_from_peer() {
    let ordermatch_ctx = Arc::new(OrdermatchContext::default());
    let ordermatch_ctx_clone = ordermatch_ctx.clone();
    OrdermatchContext::from_ctx.mock_safe(move |_| MockResult::Return(Ok(ordermatch_ctx_clone.clone())));
    let (_, mut cmd_rx) = p2p_context_mock();

    let (ctx, pubkey, secret) = make_ctx_for_tests();
    let uuid = Uuid::new_v4();
    let peer = PeerId::random().to_string();

    let order = new_protocol::MakerOrderCreated {
        uuid: uuid.clone().into(),
        base: "DOC".into(),
        rel: "MARTY".into(),
        price: BigRational::from_integer(1000000.into()),
        max_volume: BigRational::from_integer(2000000.into()),
        min_volume: BigRational::from_integer(2000000.into()),
        conf_settings: OrderConfirmationsSettings::default(),
    };

    // create an initial_message and encode it with the secret
    let initial_order_message = encode_and_sign(
        &new_protocol::OrdermatchMessage::MakerOrderCreated(order.clone()),
        &secret,
    )
    .unwrap();

    let expected_request = P2PRequest::Ordermatch(OrdermatchRequest::GetOrders {
        pairs: vec![("DOC".into(), "MARTY".into())],
        from_pubkey: pubkey.clone(),
    });
    let from_peer = peer.clone();
    let initial_message = initial_order_message.clone();
    spawn(async move {
        let cmd = cmd_rx.next().await.unwrap();
        let (req, response_tx) = if let AdexBehaviourCmd::RequestPeers { req, response_tx, .. } = cmd {
            (req, response_tx)
        } else {
            panic!("Unexpected cmd");
        };

        // check if the received request is expected
        let actual = decode_message::<P2PRequest>(&req).unwrap();
        assert_eq!(actual, expected_request);

        // create a response with the initial_message and random from_peer
        let response = vec![new_protocol::OrderInitialMessage {
            initial_message,
            from_peer: from_peer.clone(),
            update_messages: Vec::new(),
        }];

        let response = AdexResponse::Ok {
            response: encode_message(&response).unwrap(),
        };
        response_tx.send(vec![(PeerId::random(), response)]).unwrap();
    });

    let keep_alive = new_protocol::MakerOrdersKeepAlive {
        timestamp: now_ms(),
        num_orders: HashMap::from_iter(iter::once((("DOC".into(), "MARTY".into()), 1))),
    };

    // process_order_keep_alive() should return true because an order was successfully requested from a peer.
    assert!(block_on(process_orders_keep_alive(
        ctx,
        peer.clone(),
        pubkey.clone(),
        keep_alive
    )));

    let mut orderbook = block_on(ordermatch_ctx.orderbook.lock());
    // try to find the order within OrdermatchContext::orderbook and check if this order equals to the expected
    let actual = orderbook.find_order_by_uuid_and_pubkey(&uuid, &pubkey).unwrap();
    let expected: OrderbookItem = (order, pubkey).into();

    assert_eq!(actual, &expected);
}

#[test]
fn test_process_get_order_request() {
    let (ctx, pubkey, secret) = make_ctx_for_tests();
    let ordermatch_ctx = Arc::new(OrdermatchContext::default());
    let ordermatch_ctx_clone = ordermatch_ctx.clone();
    OrdermatchContext::from_ctx.mock_safe(move |_| MockResult::Return(Ok(ordermatch_ctx_clone.clone())));

    let mut orderbook = block_on(ordermatch_ctx.orderbook.lock());

    let order = new_protocol::MakerOrderCreated {
        uuid: Uuid::new_v4().into(),
        base: "DOC".into(),
        rel: "MARTY".into(),
        price: BigRational::from_integer(1000000.into()),
        max_volume: BigRational::from_integer(2000000.into()),
        min_volume: BigRational::from_integer(2000000.into()),
        conf_settings: OrderConfirmationsSettings::default(),
    };
    // create an initial_message and encode it with the secret
    let initial_message = encode_and_sign(
        &new_protocol::OrdermatchMessage::MakerOrderCreated(order.clone()),
        &secret,
    )
    .unwrap();
    let price_ping_request: OrderbookItem = (order, pubkey.clone()).into();
    orderbook.insert_or_update_order(price_ping_request.clone());

    // avoid dead lock on orderbook as process_get_orderbook_request also acquires it
    drop(orderbook);

    let encoded = block_on(process_get_order_request(
        ctx.clone(),
        price_ping_request.uuid,
        pubkey.clone(),
    ))
    .unwrap()
    .unwrap();

    let order = decode_message::<new_protocol::OrderInitialMessage>(&encoded).unwrap();
    let actual_price_ping_request = OrderbookItem::from_initial_msg(order.initial_message, order.from_peer).unwrap();
    assert_eq!(actual_price_ping_request, price_ping_request);
}

#[test]
fn test_subscribe_to_ordermatch_topic_not_subscribed() {
    let (ctx, _pubkey, _secret) = make_ctx_for_tests();
    let (_, mut cmd_rx) = p2p_context_mock();

    spawn(async move {
        match cmd_rx.next().await.unwrap() {
            AdexBehaviourCmd::Subscribe { .. } => (),
            _ => panic!("AdexBehaviourCmd::Subscribe expected first"),
        }

        let (req, response_tx) = match cmd_rx.next().await.unwrap() {
            AdexBehaviourCmd::RequestRelays { req, response_tx } => (req, response_tx),
            _ => panic!("AdexBehaviourCmd::RequestRelays expected"),
        };

        let request = decode_message::<P2PRequest>(&req).unwrap();
        match request {
            P2PRequest::Ordermatch(OrdermatchRequest::GetOrderbook { .. }) => (),
            _ => panic!(),
        }

        let response = new_protocol::Orderbook {
            asks: Vec::new(),
            bids: Vec::new(),
        };
        let encoded = encode_message(&response).unwrap();
        let response = vec![(PeerId::random(), AdexResponse::Ok { response: encoded })];
        response_tx.send(response).unwrap();
    });

    block_on(subscribe_to_orderbook_topic(&ctx, "DOC", "MARTY", true)).unwrap();

    let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
    let orderbook = block_on(ordermatch_ctx.orderbook.lock());

    let actual = orderbook
        .topics_subscribed_to
        .get(&orderbook_topic("DOC", "MARTY"))
        .cloned();
    let expected = Some(OrderbookRequestingState::Requested);
    assert_eq!(actual, expected);
}

#[test]
fn test_subscribe_to_ordermatch_topic_subscribed_not_filled() {
    let (ctx, _pubkey, _secret) = make_ctx_for_tests();
    let (_, mut cmd_rx) = p2p_context_mock();

    {
        let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
        let mut orderbook = block_on(ordermatch_ctx.orderbook.lock());
        // not enough time has passed for the orderbook to be filled
        let subscribed_at = now_ms() / 1000 - ORDERBOOK_REQUESTING_TIMEOUT + 1;
        orderbook.topics_subscribed_to.insert(
            orderbook_topic("DOC", "MARTY"),
            OrderbookRequestingState::NotRequested { subscribed_at },
        );
    }

    spawn(async move {
        let (req, response_tx) = match cmd_rx.next().await.unwrap() {
            AdexBehaviourCmd::RequestRelays { req, response_tx } => (req, response_tx),
            _ => panic!("AdexBehaviourCmd::RequestRelays expected"),
        };

        let request = decode_message::<P2PRequest>(&req).unwrap();
        match request {
            P2PRequest::Ordermatch(OrdermatchRequest::GetOrderbook { .. }) => (),
            _ => panic!(),
        }

        let response = new_protocol::Orderbook {
            asks: Vec::new(),
            bids: Vec::new(),
        };
        let encoded = encode_message(&response).unwrap();
        let response = vec![(PeerId::random(), AdexResponse::Ok { response: encoded })];
        response_tx.send(response).unwrap();
    });

    block_on(subscribe_to_orderbook_topic(&ctx, "DOC", "MARTY", true)).unwrap();

    let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
    let orderbook = block_on(ordermatch_ctx.orderbook.lock());

    let actual = orderbook
        .topics_subscribed_to
        .get(&orderbook_topic("DOC", "MARTY"))
        .cloned();
    let expected = Some(OrderbookRequestingState::Requested);
    assert_eq!(actual, expected);

    // orderbook.topics_subscribed_to.insert(orderbook_topic("DOC", "MARTY"), OrderbookSubscriptionState::NotRequested {subscribed_at: now_ms() - 41});
}

#[test]
fn test_subscribe_to_ordermatch_topic_subscribed_filled() {
    let (ctx, _pubkey, _secret) = make_ctx_for_tests();
    let (_, mut cmd_rx) = p2p_context_mock();

    // enough time has passed for the orderbook to be filled
    let subscribed_at = now_ms() / 1000 - ORDERBOOK_REQUESTING_TIMEOUT - 1;
    {
        let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
        let mut orderbook = block_on(ordermatch_ctx.orderbook.lock());
        orderbook.topics_subscribed_to.insert(
            orderbook_topic("DOC", "MARTY"),
            OrderbookRequestingState::NotRequested { subscribed_at },
        );
    }

    spawn(async move {
        assert!(cmd_rx.next().await.is_none(), "No commands expected");
    });

    block_on(subscribe_to_orderbook_topic(&ctx, "DOC", "MARTY", true)).unwrap();

    let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
    let orderbook = block_on(ordermatch_ctx.orderbook.lock());

    let actual = orderbook
        .topics_subscribed_to
        .get(&orderbook_topic("DOC", "MARTY"))
        .cloned();
    let expected = Some(OrderbookRequestingState::NotRequested { subscribed_at });
    assert_eq!(actual, expected);
}
*/
#[test]
fn test_taker_request_can_match_with_maker_pubkey() {
    let coin = TestCoin::default().into();

    let maker_pubkey = H256Json::default();

    // default has MatchBy::Any
    let mut order = TakerOrderBuilder::new(&coin, &coin).build_unchecked();
    assert!(order.request.can_match_with_maker_pubkey(&maker_pubkey));

    // the uuids of orders is checked in another method
    order.request.match_by = MatchBy::Orders(HashSet::new());
    assert!(order.request.can_match_with_maker_pubkey(&maker_pubkey));

    let mut set = HashSet::new();
    set.insert(maker_pubkey);
    order.request.match_by = MatchBy::Pubkeys(set);
    assert!(order.request.can_match_with_maker_pubkey(&maker_pubkey));

    order.request.match_by = MatchBy::Pubkeys(HashSet::new());
    assert!(!order.request.can_match_with_maker_pubkey(&maker_pubkey));
}

#[test]
fn test_taker_request_can_match_with_uuid() {
    let uuid = Uuid::new_v4();
    let coin = MmCoinEnum::Test(TestCoin::default());

    // default has MatchBy::Any
    let mut order = TakerOrderBuilder::new(&coin, &coin).build_unchecked();
    assert!(order.request.can_match_with_uuid(&uuid));

    // the uuids of orders is checked in another method
    order.request.match_by = MatchBy::Pubkeys(HashSet::new());
    assert!(order.request.can_match_with_uuid(&uuid));

    let mut set = HashSet::new();
    set.insert(uuid);
    order.request.match_by = MatchBy::Orders(set);
    assert!(order.request.can_match_with_uuid(&uuid));

    order.request.match_by = MatchBy::Orders(HashSet::new());
    assert!(!order.request.can_match_with_uuid(&uuid));
}

#[test]
fn test_orderbook_insert_or_update_order() {
    let (ctx, pubkey, secret) = make_ctx_for_tests();
    let order = make_random_orders(pubkey, &secret, "C1".into(), "C2".into(), 1).remove(0);
    insert_or_update_order(&ctx, order);
    flush_trie(&ctx);
}

/// If a cancel message arrives before the create message, the create should be silently ignored.
#[test]
fn test_recently_cancelled_blocks_insert() {
    let (_, pubkey, secret) = make_ctx_for_tests();
    let mut orderbook = Orderbook::default();
    let order = make_random_orders(pubkey.clone(), &secret, "C1".into(), "C2".into(), 1).remove(0);
    let uuid = order.uuid;

    // Simulate cancel arriving first
    orderbook.recently_cancelled.insert(uuid, pubkey.clone());

    // Now try to insert — should be silently dropped
    orderbook.index_insert_or_update(order);
    assert!(
        !orderbook.order_set.contains_key(&uuid),
        "order should NOT have been inserted after recent cancellation"
    );
}

/// An entry older than the time-to-live must no longer suppress an insert, even though it was not
/// reclaimed yet (reclaiming happens on writes only).
#[test]
fn test_recently_cancelled_expired_entry_allows_insert() {
    let (_, pubkey, secret) = make_ctx_for_tests();
    let mut orderbook = Orderbook {
        recently_cancelled: TimeCache::new(Duration::from_millis(50)),
        ..Default::default()
    };
    let order = make_random_orders(pubkey.clone(), &secret, "C1".into(), "C2".into(), 1).remove(0);
    let uuid = order.uuid;

    orderbook.recently_cancelled.insert(uuid, pubkey.clone());
    orderbook.index_insert_or_update(order.clone());
    assert!(
        !orderbook.order_set.contains_key(&uuid),
        "insert within the window must be suppressed"
    );

    std::thread::sleep(Duration::from_millis(150));
    orderbook.index_insert_or_update(order);
    assert!(
        orderbook.order_set.contains_key(&uuid),
        "insert after the entry exceeded the time-to-live must go through"
    );
}

/// If a cancel is recorded for pubkey A but the insert comes from pubkey B (different sender),
/// it should go through (the pubkeys don't match the recently_cancelled entry).
#[test]
fn test_recently_cancelled_allows_different_pubkey() {
    let (_, pubkey_a, _) = make_ctx_for_tests();
    let (pubkey_b, secret_b) = pubkey_and_secret_for_test("different passphrase");
    let mut orderbook = Orderbook::default();
    let order = make_random_orders(pubkey_b.clone(), &secret_b, "C1".into(), "C2".into(), 1).remove(0);
    let uuid = order.uuid;

    // Cancel recorded for pubkey_a — not the order's pubkey
    orderbook.recently_cancelled.insert(uuid, pubkey_a);

    // Insert from pubkey_b should succeed
    orderbook.index_insert_or_update(order);
    assert!(
        orderbook.order_set.contains_key(&uuid),
        "order from a different pubkey should be inserted"
    );
}

fn pair_trie_root_by_pub(ctx: &MmArc, pubkey: &str, pair: &str) -> H64 {
    let ordermatch_ctx = OrdermatchContext::from_ctx(ctx).unwrap();
    let trie_store = ordermatch_ctx.trie_store.lock();
    *trie_store
        .pubkeys_state
        .get(pubkey)
        .unwrap()
        .trie_roots
        .get(pair)
        .unwrap()
}

fn clone_orderbook_memory_db(ctx: &MmArc) -> MemoryDB<Blake2Hasher64> {
    let ordermatch_ctx = OrdermatchContext::from_ctx(ctx).unwrap();
    let trie_store = ordermatch_ctx.trie_store.lock();
    trie_store.memory_db.clone()
}

fn remove_order(ctx: &MmArc, uuid: Uuid) {
    let ordermatch_ctx = OrdermatchContext::from_ctx(ctx).unwrap();
    let queued = {
        let mut orderbook = ordermatch_ctx.orderbook.lock();
        match orderbook.index_remove(uuid) {
            Some((_removed, op)) => {
                orderbook.queue_trie_ops(&ordermatch_ctx.trie_ops_tx, vec![op]);
                true
            },
            None => false,
        }
    };
    if queued {
        ordermatch_ctx.wait_trie_ops_flushed();
    }
}

/// Wait until the background trie worker has applied all pending ops.
fn flush_trie(ctx: &MmArc) {
    let ordermatch_ctx = OrdermatchContext::from_ctx(ctx).unwrap();
    ordermatch_ctx.wait_trie_ops_flushed();
}

#[test]
fn test_process_sync_pubkey_orderbook_state_after_new_orders_added() {
    let (ctx, pubkey, secret) = make_ctx_for_tests();
    let orders = make_random_orders(pubkey.clone(), &secret, "C1".into(), "C2".into(), 100);

    for order in orders {
        insert_or_update_order(&ctx, order);
    }
    flush_trie(&ctx);

    let alb_ordered_pair = alb_ordered_pair("C1", "C2");
    let pair_trie_root = pair_trie_root_by_pub(&ctx, &pubkey, &alb_ordered_pair);

    let prev_pairs_state = HashMap::from_iter(iter::once((alb_ordered_pair.clone(), pair_trie_root)));

    let mut old_mem_db = clone_orderbook_memory_db(&ctx);

    let new_orders = make_random_orders(pubkey.clone(), &secret, "C1".into(), "C2".into(), 100);
    for order in new_orders {
        insert_or_update_order(&ctx, order.clone());
    }
    flush_trie(&ctx);

    let mut result = process_sync_pubkey_orderbook_state(ctx.clone(), pubkey.clone(), prev_pairs_state)
        .unwrap()
        .unwrap();

    // check pair trie root
    let expected_root_hash = pair_trie_root_by_pub(&ctx, &pubkey, &alb_ordered_pair);

    let delta = match result.pair_orders_diff.remove(&alb_ordered_pair).unwrap() {
        DeltaOrFullTrie::Delta(delta) => delta,
        DeltaOrFullTrie::FullTrie(_) => panic!("Must be DeltaOrFullTrie::Delta"),
    };

    let actual_root_hash = delta_trie_root::<Layout, _, _, _, _, _>(
        &mut old_mem_db,
        pair_trie_root,
        delta.into_iter().map(|(uuid, order)| {
            (
                *uuid.as_bytes(),
                order.map(|o| {
                    let o = OrderbookItem::from_p2p_and_info(o, BaseRelProtocolInfo::default(), None);
                    o.trie_state_bytes()
                }),
            )
        }),
    )
    .unwrap();
    assert_eq!(expected_root_hash, actual_root_hash);
}

#[test]
fn test_diff_should_not_be_written_if_hash_not_changed_on_insert() {
    let (ctx, pubkey, secret) = make_ctx_for_tests();
    let orders = make_random_orders(pubkey.clone(), &secret, "C1".into(), "C2".into(), 100);

    for order in orders.clone() {
        insert_or_update_order(&ctx, order);
    }
    flush_trie(&ctx);

    let alb_ordered_pair = alb_ordered_pair("C1", "C2");
    let pair_trie_root = pair_trie_root_by_pub(&ctx, &pubkey, &alb_ordered_pair);
    for order in orders.clone() {
        insert_or_update_order(&ctx, order);
    }
    flush_trie(&ctx);

    let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
    let trie_store = ordermatch_ctx.trie_store.lock();
    let pubkey_state = trie_store.pubkeys_state.get(&pubkey).unwrap();
    assert!(!pubkey_state
        .order_pairs_trie_state_history
        .get(&alb_ordered_pair)
        .expect("Must contain C1:C2 pair")
        .contains_key(&pair_trie_root));
}

#[test]
fn test_process_sync_pubkey_orderbook_state_after_orders_removed() {
    let (ctx, pubkey, secret) = make_ctx_for_tests();
    let orders = make_random_orders(pubkey.clone(), &secret, "C1".into(), "C2".into(), 100);

    for order in orders.clone() {
        insert_or_update_order(&ctx, order);
    }
    flush_trie(&ctx);

    let alb_ordered_pair = alb_ordered_pair("C1", "C2");
    let pair_trie_root = pair_trie_root_by_pub(&ctx, &pubkey, &alb_ordered_pair);

    let prev_pairs_state = HashMap::from_iter(iter::once((alb_ordered_pair.clone(), pair_trie_root)));

    let mut old_mem_db = clone_orderbook_memory_db(&ctx);

    // Remove deterministic non-tail orders. Removing the latest inserted order can
    // legitimately return the trie to a previous root and make sync fall back to
    // FullTrie instead of the delta path this test is asserting.
    let to_remove = orders.iter().take(10);
    for order in to_remove {
        remove_order(&ctx, order.uuid);
    }

    let mut result = process_sync_pubkey_orderbook_state(ctx.clone(), pubkey.clone(), prev_pairs_state)
        .unwrap()
        .unwrap();

    // check pair trie root
    let expected_root_hash = pair_trie_root_by_pub(&ctx, &pubkey, &alb_ordered_pair);

    let delta = match result.pair_orders_diff.remove(&alb_ordered_pair).unwrap() {
        DeltaOrFullTrie::Delta(delta) => delta,
        DeltaOrFullTrie::FullTrie(_) => panic!("Must be DeltaOrFullTrie::Delta"),
    };

    let actual_root_hash = delta_trie_root::<Layout, _, _, _, _, _>(
        &mut old_mem_db,
        pair_trie_root,
        delta
            .into_iter()
            .map(|(uuid, order)| (*uuid.as_bytes(), order.map(|o| encode_message(&o).unwrap()))),
    )
    .unwrap();
    assert_eq!(expected_root_hash, actual_root_hash);
}

#[test]
fn test_diff_should_not_be_written_if_hash_not_changed_on_remove() {
    let (ctx, pubkey, secret) = make_ctx_for_tests();
    let orders = make_random_orders(pubkey.clone(), &secret, "C1".into(), "C2".into(), 100);

    for order in orders.clone() {
        insert_or_update_order(&ctx, order);
    }
    flush_trie(&ctx);

    let to_remove: Vec<_> = orders
        .choose_multiple(&mut thread_rng(), 10)
        .map(|order| order.uuid)
        .collect();
    for uuid in &to_remove {
        remove_order(&ctx, *uuid);
    }
    for uuid in &to_remove {
        remove_order(&ctx, *uuid);
    }

    let alb_ordered_pair = alb_ordered_pair("C1", "C2");
    let pair_trie_root = pair_trie_root_by_pub(&ctx, &pubkey, &alb_ordered_pair);

    let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
    let trie_store = ordermatch_ctx.trie_store.lock();
    let pubkey_state = trie_store.pubkeys_state.get(&pubkey).unwrap();
    assert!(!pubkey_state
        .order_pairs_trie_state_history
        .get(&alb_ordered_pair)
        .expect("Must contain C1:C2 pair")
        .contains_key(&pair_trie_root));
}

#[test]
fn test_orderbook_pubkey_sync_request() {
    let (ctx, ..) = make_ctx_for_tests();
    let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
    {
        let mut subs = ordermatch_ctx.orderbook_subscriptions.write();
        subs.insert(
            orderbook_topic_from_base_rel("C1", "C2"),
            OrderbookRequestingState::Requested,
        );
    }
    let pubkey = "pubkey";

    let mut trie_roots = HashMap::new();
    trie_roots.insert("C1:C2".to_owned(), [1; 8]);
    trie_roots.insert("C2:C3".to_owned(), [1; 8]);

    let message = PubkeyKeepAlive {
        trie_roots,
        timestamp: now_ms() / 1000,
    };

    let subscribed_topics: HashSet<String> = {
        let subs = ordermatch_ctx.orderbook_subscriptions.read();
        subs.keys().cloned().collect()
    };
    let mut trie_store = ordermatch_ctx.trie_store.lock();
    let request = trie_store
        .prepare_sync_request_for_keep_alive(pubkey, message, false, |topic| subscribed_topics.contains(topic))
        .unwrap();
    match request {
        OrdermatchRequest::SyncPubkeyOrderbookState {
            trie_roots: pairs_trie_roots,
            ..
        } => {
            assert!(pairs_trie_roots.contains_key("C1:C2"));
            assert!(!pairs_trie_roots.contains_key("C2:C3"));
        },
        _ => panic!("Invalid request {:?}", request),
    }
}

/// CRD ch.32 R-P3: a keep-alive records the receiver's own receipt time, never the sender's timestamp.
#[test]
fn test_keep_alive_records_local_receipt_time_not_sender_timestamp() {
    let (ctx, ..) = make_ctx_for_tests();
    let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
    let pubkey = "pubkey";
    let before = now_ms() / 1000;
    let far_future = before + 10 * 365 * 24 * 3600;

    let message = PubkeyKeepAlive {
        trie_roots: HashMap::new(),
        timestamp: far_future,
    };
    let mut trie_store = ordermatch_ctx.trie_store.lock();
    trie_store.prepare_sync_request_for_keep_alive(pubkey, message, false, |_| false);
    let after = now_ms() / 1000;

    let stored = trie_store.pubkeys_state.get(pubkey).unwrap().last_keep_alive;
    assert!(
        (before..=after).contains(&stored),
        "stored {stored} must be local receipt time in {before}..={after}"
    );
}

/// Zero and empty-trie roots must not be advertised in outgoing keep-alives.
#[test]
fn test_keep_alive_skips_zero_and_null_trie_roots() {
    let mut all_roots = HashMap::new();
    all_roots.insert("A:B".to_owned(), H64::default());
    all_roots.insert("C:D".to_owned(), hashed_null_node::<Layout>());
    all_roots.insert("E:F".to_owned(), [1; 8]);

    let (roots, topics) = keep_alive_roots_and_topics(&all_roots);
    assert_eq!(roots.len(), 1);
    assert_eq!(roots.get("E:F"), Some(&[1; 8]));
    assert_eq!(topics.len(), 1);
}

#[test]
fn test_orderbook_pubkey_sync_request_relay() {
    let (ctx, ..) = make_ctx_for_tests();
    let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
    {
        let mut subs = ordermatch_ctx.orderbook_subscriptions.write();
        subs.insert(
            orderbook_topic_from_base_rel("C1", "C2"),
            OrderbookRequestingState::Requested,
        );
    }
    let pubkey = "pubkey";

    let mut trie_roots = HashMap::new();
    trie_roots.insert("C1:C2".to_owned(), [1; 8]);
    trie_roots.insert("C2:C3".to_owned(), [1; 8]);

    let message = PubkeyKeepAlive {
        trie_roots,
        timestamp: now_ms() / 1000,
    };

    let subscribed_topics: HashSet<String> = {
        let subs = ordermatch_ctx.orderbook_subscriptions.read();
        subs.keys().cloned().collect()
    };
    let mut trie_store = ordermatch_ctx.trie_store.lock();
    let request = trie_store
        .prepare_sync_request_for_keep_alive(pubkey, message, true, |topic| subscribed_topics.contains(topic))
        .unwrap();
    match request {
        OrdermatchRequest::SyncPubkeyOrderbookState {
            trie_roots: pairs_trie_roots,
            ..
        } => {
            assert!(pairs_trie_roots.contains_key("C1:C2"));
            assert!(pairs_trie_roots.contains_key("C2:C3"));
        },
        _ => panic!("Invalid request {:?}", request),
    }
}

#[test]
fn test_trie_diff_avoid_cycle_on_insertion() {
    let mut history = TrieDiffHistory::<String, String> {
        inner: TimeCache::new(Duration::from_secs(3600)),
    };
    history.insert_new_diff([1; 8], TrieDiff {
        delta: vec![],
        next_root: [2; 8],
    });
    history.insert_new_diff([2; 8], TrieDiff {
        delta: vec![],
        next_root: [3; 8],
    });
    history.insert_new_diff([3; 8], TrieDiff {
        delta: vec![],
        next_root: [4; 8],
    });
    history.insert_new_diff([4; 8], TrieDiff {
        delta: vec![],
        next_root: [5; 8],
    });
    history.insert_new_diff([5; 8], TrieDiff {
        delta: vec![],
        next_root: [2; 8],
    });

    let expected = HashMap::from_iter(iter::once(([1u8; 8], TrieDiff {
        delta: vec![],
        next_root: [2; 8],
    })));

    assert_eq!(expected, history.inner.as_hash_map());
}

#[test]
fn test_process_sync_pubkey_orderbook_state_points_to_not_uptodate_trie_root() {
    let (ctx, pubkey, secret) = make_ctx_for_tests();
    let orders = make_random_orders(pubkey.clone(), &secret, "DOC".into(), "MARTY".into(), 10);
    let new_order = make_random_orders(pubkey.clone(), &secret, "DOC".into(), "MARTY".into(), 1)
        .pop()
        .expect("Expected one order");

    for order in orders.iter() {
        insert_or_update_order(&ctx, order.clone());
    }
    flush_trie(&ctx);

    let alb_pair = alb_ordered_pair("DOC", "MARTY");

    // Update trie root by adding a new order directly (bypassing history)
    let (old_root, _new_root) = {
        let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
        let mut trie_store = ordermatch_ctx.trie_store.lock();
        let mut orderbook = ordermatch_ctx.orderbook.lock();

        let old_root = *trie_store
            .pubkeys_state
            .get_mut(&pubkey)
            .expect("!pubkeys_state")
            .trie_roots
            .get(&alb_pair)
            .expect("MARTY:DOC must be in trie_roots");

        let order_bytes = new_order.trie_state_bytes();
        let mut new_root = old_root;
        let mut trie = get_trie_mut(&mut trie_store.memory_db, &mut new_root).expect("!get_trie_mut");
        trie.insert(new_order.uuid.as_bytes(), &order_bytes)
            .expect("Error on order insertion");
        drop(trie);

        // update root in trie_store trie_roots
        trie_store
            .pubkeys_state
            .get_mut(&pubkey)
            .expect("!pubkeys_state")
            .trie_roots
            .insert(alb_pair.clone(), new_root);

        orderbook.order_set.insert(new_order.uuid, new_order.clone());
        (old_root, new_root)
    };

    let mut roots = HashMap::new();
    roots.insert(alb_pair.clone(), old_root);

    let SyncPubkeyOrderbookStateRes {
        mut pair_orders_diff, ..
    } = process_sync_pubkey_orderbook_state(ctx, pubkey, roots)
        .expect("!process_sync_pubkey_orderbook_state")
        .expect("Expected MARTY:DOC delta, returned None");

    let delta = pair_orders_diff.remove(&alb_pair).expect("Expected MARTY:DOC delta");
    let mut full_trie = match delta {
        DeltaOrFullTrie::Delta(_) => panic!("Expected FullTrie, found Delta"),
        DeltaOrFullTrie::FullTrie(full_trie) => full_trie,
    };

    let mut expected: Vec<(Uuid, OrderbookP2PItem)> =
        orders.into_iter().map(|order| (order.uuid, order.into())).collect();
    expected.push((new_order.uuid, new_order.into()));
    full_trie.sort_by_key(|x| x.0);
    expected.sort_by_key(|x| x.0);
    assert_eq!(full_trie, expected);
}

fn check_if_orderbook_contains_only(ctx: &MmArc, pubkey: &str, orders: &[OrderbookItem]) {
    let ordermatch_ctx = OrdermatchContext::from_ctx(ctx).unwrap();
    let orderbook = ordermatch_ctx.orderbook.lock();
    let trie_store = ordermatch_ctx.trie_store.lock();
    let pubkey_state = trie_store.pubkeys_state.get(pubkey).expect("!pubkeys_state");

    // order_set
    let expected_set: HashMap<_, _> = orders.iter().map(|order| (order.uuid, order.clone())).collect();
    assert_eq!(orderbook.order_set, expected_set);

    // ordered
    let mut expected_ordered = HashMap::new();
    for order in orders.iter() {
        let item = OrderedByPriceOrder {
            uuid: order.uuid,
            price: order.price.clone().into(),
        };
        let set = expected_ordered
            .entry((order.base.clone(), order.rel.clone()))
            .or_insert_with(BTreeSet::default);
        set.insert(item);
    }
    assert_eq!(orderbook.ordered, expected_ordered);

    // unordered
    let mut expected_unordered = HashMap::new();
    for order in orders.iter() {
        let set = expected_unordered
            .entry((order.base.clone(), order.rel.clone()))
            .or_insert_with(HashSet::default);
        set.insert(order.uuid);
    }
    assert_eq!(orderbook.unordered, expected_unordered);

    // history
    let actual_keys: HashSet<_> = pubkey_state.order_pairs_trie_state_history.keys().cloned().collect();
    let expected_keys: HashSet<_> = orders
        .iter()
        .map(|order| alb_ordered_pair(&order.base, &order.rel))
        .collect();
    assert_eq!(actual_keys, expected_keys);

    // orders_uuids
    let expected_uuids: HashSet<_> = orders
        .iter()
        .map(|order| (order.uuid, alb_ordered_pair(&order.base, &order.rel)))
        .collect();
    assert_eq!(pubkey_state.orders_uuids, expected_uuids);

    // trie_roots
    let actual_trie_orders: HashMap<_, _> = pubkey_state
        .trie_roots
        .iter()
        .map(|(alb_pair, trie_root)| {
            let trie = TrieDB::<Layout>::new(&trie_store.memory_db, trie_root).expect("!TrieDB::new");
            let mut trie: Vec<(Uuid, OrderbookItem)> = trie
                .iter()
                .expect("!TrieDB::iter")
                .map(|key_value| {
                    let (key, _) = key_value.expect("Iterator returned an error");
                    let key = TryFromBytes::try_from_bytes(key).expect("!try_from_bytes() key");
                    let value = orderbook.order_set.get(&key).cloned().unwrap();
                    (key, value)
                })
                .collect();
            trie.sort_by_key(|(uuid_x, _)| *uuid_x);
            (alb_pair.clone(), trie)
        })
        .collect();
    let mut expected_trie_orders = HashMap::new();
    for order in orders.iter() {
        let trie = expected_trie_orders
            .entry(alb_ordered_pair(&order.base, &order.rel))
            .or_insert_with(Vec::default);
        trie.push((order.uuid, order.clone()));
    }
    for trie in expected_trie_orders.values_mut() {
        trie.sort_by_key(|(uuid_x, _)| *uuid_x);
    }
    assert_eq!(actual_trie_orders, expected_trie_orders);
}

#[test]
fn test_remove_and_purge_pubkey_pair_orders() {
    let (ctx, pubkey, secret) = make_ctx_for_tests();
    let doc_marty_orders = make_random_orders(pubkey.clone(), &secret, "DOC".into(), "MARTY".into(), 10);
    let doc_kmd_orders = make_random_orders(pubkey.clone(), &secret, "DOC".into(), "KMD".into(), 10);

    for order in doc_marty_orders.iter().chain(doc_kmd_orders.iter()) {
        insert_or_update_order(&ctx, order.clone());
    }
    flush_trie(&ctx);

    let doc_marty_pair = alb_ordered_pair("DOC", "MARTY");

    let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
    {
        let mut orderbook = ordermatch_ctx.orderbook.lock();
        orderbook.index_remove_pubkey_pair_orders(&pubkey, &doc_marty_pair);
    }
    {
        let mut trie_store = ordermatch_ctx.trie_store.lock();
        trie_store.apply_ops(vec![TrieOp::ClearPair {
            pubkey: pubkey.clone(),
            alb_pair: doc_marty_pair.clone(),
        }]);
    }
    check_if_orderbook_contains_only(&ctx, &pubkey, &doc_kmd_orders);
}

// CRD ch.32 R-T3a tests: a batch of trie ops takes effect exactly as the same ops applied one at a time.
fn trie_op_item(pubkey: &str, base: &str, rel: &str, uuid: Uuid, price_numer: u64) -> OrderbookItem {
    OrderbookItem {
        pubkey: pubkey.to_owned(),
        base: base.to_owned(),
        rel: rel.to_owned(),
        price: BigRational::new(price_numer.into(), 1000.into()),
        max_volume: BigRational::from_integer(1.into()),
        min_volume: BigRational::from_integer(0.into()),
        uuid,
        created_at: 1_700_000_000,
        base_protocol_info: vec![],
        rel_protocol_info: vec![],
        conf_settings: None,
    }
}

fn trie_op_uuid(n: u64) -> Uuid { Uuid::from_fields(0, 0, 0, &n.to_be_bytes()).unwrap() }

fn trie_op_insert(pubkey: &str, uuid: Uuid, price_numer: u64) -> TrieOp {
    TrieOp::Insert {
        pubkey: pubkey.to_owned(),
        alb_pair: alb_ordered_pair("DOC", "MARTY"),
        uuid,
        order: trie_op_item(pubkey, "DOC", "MARTY", uuid, price_numer),
    }
}

fn trie_op_remove(pubkey: &str, uuid: Uuid) -> TrieOp {
    TrieOp::Remove {
        pubkey: pubkey.to_owned(),
        alb_pair: alb_ordered_pair("DOC", "MARTY"),
        uuid,
    }
}

fn trie_op_clear(pubkey: &str) -> TrieOp {
    TrieOp::ClearPair {
        pubkey: pubkey.to_owned(),
        alb_pair: alb_ordered_pair("DOC", "MARTY"),
    }
}

fn assert_trie_stores_equal(a: &TrieStore, b: &TrieStore, what: &str) {
    let keys = |s: &TrieStore| s.pubkeys_state.keys().cloned().collect::<HashSet<_>>();
    assert_eq!(keys(a), keys(b), "{what}: pubkey sets differ");
    for (pubkey, sa) in a.pubkeys_state.iter() {
        let sb = &b.pubkeys_state[pubkey];
        assert_eq!(sa.trie_roots, sb.trie_roots, "{what}: trie_roots differ for {pubkey}");
        assert_eq!(
            sa.orders_uuids, sb.orders_uuids,
            "{what}: orders_uuids differ for {pubkey}"
        );
    }
}

/// T-T1: apply `batch` after `prelude`, and compare with a store that only ever saw `survivors`.
fn check_batch_order_case(name: &str, prelude: Vec<TrieOp>, batch: Vec<TrieOp>, survivors: Vec<TrieOp>) {
    let pair = alb_ordered_pair("DOC", "MARTY");
    let mut store = TrieStore::default();
    store.apply_ops(prelude);
    store.apply_ops(batch);

    let mut reference = TrieStore::default();
    reference.apply_ops(survivors);

    let pk = "02aaaa";
    let (state, ref_state) = (store.pubkeys_state.get(pk), reference.pubkeys_state.get(pk));
    assert_eq!(
        state.and_then(|s| s.trie_roots.get(&pair)),
        ref_state.and_then(|s| s.trie_roots.get(&pair)),
        "{name}: pair root differs from survivors-only trie"
    );
    assert_eq!(
        state.map(|s| s.orders_uuids.clone()).unwrap_or_default(),
        ref_state.map(|s| s.orders_uuids.clone()).unwrap_or_default(),
        "{name}: order identifiers differ"
    );
    // value presence
    if let (Some(state), Some(ref_state)) = (state, ref_state) {
        if let (Some(root), Some(ref_root)) = (state.trie_roots.get(&pair), ref_state.trie_roots.get(&pair)) {
            let trie = TrieDB::<Layout>::new(&store.memory_db, root).expect("!TrieDB::new");
            let ref_trie = TrieDB::<Layout>::new(&reference.memory_db, ref_root).expect("!TrieDB::new");
            for (_, uuid) in ref_state.orders_uuids.iter() {
                assert_eq!(
                    trie.get(uuid.as_bytes()).unwrap(),
                    ref_trie.get(uuid.as_bytes()).unwrap(),
                    "{name}: value differs"
                );
            }
        }
    }
}

#[test]
fn test_trie_apply_ops_batch_order_t_t1() {
    let pk = "02aaaa";
    let u = trie_op_uuid(1);
    let x = trie_op_uuid(2);
    let other_pair_pubkey_op = |price| trie_op_insert("02bbbb", trie_op_uuid(9), price);

    // remove then insert -> present with the inserted value
    check_batch_order_case(
        "remove->insert",
        vec![trie_op_insert(pk, x, 5), trie_op_insert(pk, u, 10)],
        vec![trie_op_remove(pk, u), trie_op_insert(pk, u, 20)],
        vec![trie_op_insert(pk, x, 5), trie_op_insert(pk, u, 20)],
    );
    // insert then remove -> absent (u previously present, and previously absent)
    check_batch_order_case(
        "insert->remove (present before)",
        vec![trie_op_insert(pk, x, 5), trie_op_insert(pk, u, 10)],
        vec![trie_op_insert(pk, u, 20), trie_op_remove(pk, u)],
        vec![trie_op_insert(pk, x, 5)],
    );
    check_batch_order_case(
        "insert->remove (absent before)",
        vec![trie_op_insert(pk, x, 5)],
        vec![trie_op_insert(pk, u, 20), trie_op_remove(pk, u)],
        vec![trie_op_insert(pk, x, 5)],
    );
    // insert A then insert B -> B
    check_batch_order_case(
        "insert A->insert B",
        vec![trie_op_insert(pk, x, 5)],
        vec![trie_op_insert(pk, u, 11), trie_op_insert(pk, u, 22)],
        vec![trie_op_insert(pk, x, 5), trie_op_insert(pk, u, 22)],
    );
    // clear then insert -> only the insert
    check_batch_order_case(
        "clear->insert",
        vec![trie_op_insert(pk, x, 5), trie_op_insert(pk, u, 10)],
        vec![trie_op_clear(pk), trie_op_insert(pk, u, 30)],
        vec![trie_op_insert(pk, u, 30)],
    );
    // insert then clear -> empty slice
    check_batch_order_case(
        "insert->clear",
        vec![trie_op_insert(pk, x, 5)],
        vec![trie_op_insert(pk, u, 30), trie_op_clear(pk)],
        vec![],
    );
    // drop pubkey then insert -> only the insert; another pubkey is untouched
    check_batch_order_case(
        "drop pubkey->insert",
        vec![
            trie_op_insert(pk, x, 5),
            trie_op_insert(pk, u, 10),
            other_pair_pubkey_op(1),
        ],
        vec![
            TrieOp::RemovePubkey { pubkey: pk.to_owned() },
            trie_op_insert(pk, u, 40),
            other_pair_pubkey_op(1),
        ],
        vec![trie_op_insert(pk, u, 40)],
    );
}

fn trie_ops_lcg(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    *state >> 33
}

fn random_trie_ops(seed: u64, n: usize) -> Vec<TrieOp> {
    let mut st = seed;
    let pubkeys = ["02aaaa", "02bbbb"];
    let pairs = [("DOC", "MARTY"), ("DOC", "KMD")];
    (0..n)
        .map(|_| {
            let pubkey = pubkeys[(trie_ops_lcg(&mut st) % 2) as usize];
            let (base, rel) = pairs[(trie_ops_lcg(&mut st) % 2) as usize];
            let alb_pair = alb_ordered_pair(base, rel);
            let uuid = trie_op_uuid(trie_ops_lcg(&mut st) % 6 + 1);
            match trie_ops_lcg(&mut st) % 20 {
                0..=9 => TrieOp::Insert {
                    pubkey: pubkey.to_owned(),
                    alb_pair,
                    uuid,
                    order: trie_op_item(pubkey, base, rel, uuid, trie_ops_lcg(&mut st) % 1000 + 1),
                },
                10..=15 => TrieOp::Remove {
                    pubkey: pubkey.to_owned(),
                    alb_pair,
                    uuid,
                },
                16..=18 => TrieOp::ClearPair {
                    pubkey: pubkey.to_owned(),
                    alb_pair,
                },
                _ => TrieOp::RemovePubkey {
                    pubkey: pubkey.to_owned(),
                },
            }
        })
        .collect()
}

#[test]
fn test_trie_apply_ops_batch_equals_sequence_t_t2() {
    for seed in 1..=30u64 {
        let n = 200 + (seed as usize % 7) * 20;
        let mut batch_store = TrieStore::default();
        batch_store.apply_ops(random_trie_ops(seed, n));

        let mut seq_store = TrieStore::default();
        for op in random_trie_ops(seed, n) {
            seq_store.apply_ops(vec![op]);
        }
        assert_trie_stores_equal(&batch_store, &seq_store, &format!("seed {seed}"));
    }
}

#[test]
fn test_orderbook_sync_trie_diff_time_cache() {
    let (ctx_bob, pubkey_bob, secret_bob) = make_ctx_for_tests();
    let doc_marty_orders = make_random_orders(pubkey_bob.clone(), &secret_bob, "DOC".into(), "MARTY".into(), 15);

    let doc_marty_pair = alb_ordered_pair("DOC", "MARTY");

    for order in &doc_marty_orders[..5] {
        insert_or_update_order(&ctx_bob, order.clone());
    }
    flush_trie(&ctx_bob);

    std::thread::sleep(Duration::from_secs(3));

    for order in &doc_marty_orders[5..10] {
        insert_or_update_order(&ctx_bob, order.clone());
    }
    flush_trie(&ctx_bob);

    let ordermatch_ctx_bob = OrdermatchContext::from_ctx(&ctx_bob).unwrap();
    let trie_store_bob = ordermatch_ctx_bob.trie_store.lock();
    let bob_state = trie_store_bob.pubkeys_state.get(&pubkey_bob).unwrap();
    let doc_marty_history_bob = bob_state.order_pairs_trie_state_history.get(&doc_marty_pair).unwrap();
    assert_eq!(doc_marty_history_bob.len(), 5);

    // alice has an outdated state, for which bob doesn't have history anymore as it's expired
    let (ctx_alice, ..) = make_ctx_for_tests();

    for order in &doc_marty_orders[..3] {
        insert_or_update_order(&ctx_alice, order.clone());
    }
    flush_trie(&ctx_alice);

    let ordermatch_ctx_alice = OrdermatchContext::from_ctx(&ctx_alice).unwrap();
    let mut trie_store_alice = ordermatch_ctx_alice.trie_store.lock();
    let bob_state_on_alice_side = trie_store_alice.pubkeys_state.get(&pubkey_bob).unwrap();

    let alice_root = bob_state_on_alice_side.trie_roots.get(&doc_marty_pair).unwrap();
    let bob_root = bob_state.trie_roots.get(&doc_marty_pair).unwrap();

    let bob_history_on_sync = {
        let orderbook_bob = ordermatch_ctx_bob.orderbook.lock();
        DeltaOrFullTrie::from_history(
            doc_marty_history_bob,
            *alice_root,
            *bob_root,
            &trie_store_bob.memory_db,
            |uuid: &Uuid| orderbook_bob.order_set.get(uuid).cloned(),
        )
        .unwrap()
    };

    let full_trie = match bob_history_on_sync {
        DeltaOrFullTrie::FullTrie(trie) => trie,
        _ => panic!("Expected DeltaOrFullTrie::FullTrie"),
    };

    let params = ProcessTrieParams {
        pubkey: &pubkey_bob,
        alb_pair: &doc_marty_pair,
        protocol_infos: &HashMap::new(),
        conf_infos: &HashMap::new(),
    };

    // Phase 1: index update under orderbook lock, collect trie ops
    let ops = {
        let mut orderbook_alice = ordermatch_ctx_alice.orderbook.lock();
        process_pubkey_full_trie(
            &mut orderbook_alice,
            full_trie
                .into_iter()
                .map(|(uuid, order)| (uuid, order.into()))
                .collect(),
            params,
        )
    };
    // Phase 2: apply trie ops directly (we already hold trie_store_alice)
    if !ops.is_empty() {
        trie_store_alice.apply_ops(ops);
    }

    let new_alice_root = *trie_store_alice
        .pubkeys_state
        .get(&pubkey_bob)
        .unwrap()
        .trie_roots
        .get(&doc_marty_pair)
        .unwrap();
    assert_eq!(new_alice_root, *bob_root);

    drop(trie_store_bob);
    drop(trie_store_alice);

    for order in &doc_marty_orders[10..] {
        insert_or_update_order(&ctx_bob, order.clone());
    }
    flush_trie(&ctx_bob);

    remove_order(&ctx_bob, doc_marty_orders[12].uuid);

    let trie_store_bob = ordermatch_ctx_bob.trie_store.lock();
    let bob_state = trie_store_bob.pubkeys_state.get(&pubkey_bob).unwrap();
    let doc_marty_history_bob = bob_state.order_pairs_trie_state_history.get(&doc_marty_pair).unwrap();

    let mut trie_store_alice = ordermatch_ctx_alice.trie_store.lock();
    let bob_state_on_alice_side = trie_store_alice.pubkeys_state.get(&pubkey_bob).unwrap();

    let alice_root = bob_state_on_alice_side.trie_roots.get(&doc_marty_pair).unwrap();
    let bob_root = bob_state.trie_roots.get(&doc_marty_pair).unwrap();

    let bob_history_on_sync = {
        let orderbook_bob = ordermatch_ctx_bob.orderbook.lock();
        DeltaOrFullTrie::from_history(
            doc_marty_history_bob,
            *alice_root,
            *bob_root,
            &trie_store_bob.memory_db,
            |uuid: &Uuid| orderbook_bob.order_set.get(uuid).cloned(),
        )
        .unwrap()
    };

    // Check that alice gets orders from history this time
    let trie_delta = match bob_history_on_sync {
        DeltaOrFullTrie::Delta(delta) => delta,
        _ => panic!("Expected DeltaOrFullTrie::Delta"),
    };

    let params = ProcessTrieParams {
        pubkey: &pubkey_bob,
        alb_pair: &doc_marty_pair,
        protocol_infos: &HashMap::new(),
        conf_infos: &HashMap::new(),
    };
    // Phase 1: index update
    let ops = {
        let mut orderbook_alice = ordermatch_ctx_alice.orderbook.lock();
        process_trie_delta(
            &mut orderbook_alice,
            trie_delta
                .into_iter()
                .map(|(uuid, order)| (uuid, order.map(From::from)))
                .collect(),
            params,
        )
    };
    // Phase 2: apply trie ops
    if !ops.is_empty() {
        trie_store_alice.apply_ops(ops);
    }

    let new_alice_root = *trie_store_alice
        .pubkeys_state
        .get(&pubkey_bob)
        .unwrap()
        .trie_roots
        .get(&doc_marty_pair)
        .unwrap();
    assert_eq!(new_alice_root, *bob_root);
}

#[test]
fn test_orderbook_order_pairs_trie_state_history_updates_expiration_on_insert() {
    let (ctx_bob, pubkey_bob, secret_bob) = make_ctx_for_tests();
    let doc_marty_orders = make_random_orders(pubkey_bob.clone(), &secret_bob, "DOC".into(), "MARTY".into(), 15);

    let doc_marty_pair = alb_ordered_pair("DOC", "MARTY");

    for order in &doc_marty_orders[..5] {
        insert_or_update_order(&ctx_bob, order.clone());
    }
    flush_trie(&ctx_bob);

    // After 3 seconds DOC:MARTY pair trie state history will time out and will be empty
    std::thread::sleep(Duration::from_secs(3));

    // Insert some more orders to remove expired timecache DOC:MARTY key
    for order in &doc_marty_orders[5..10] {
        insert_or_update_order(&ctx_bob, order.clone());
    }
    flush_trie(&ctx_bob);

    let ordermatch_ctx_bob = OrdermatchContext::from_ctx(&ctx_bob).unwrap();
    let trie_store_bob = ordermatch_ctx_bob.trie_store.lock();
    let bob_state = trie_store_bob.pubkeys_state.get(&pubkey_bob).unwrap();

    // Only the last inserted 5 orders are found
    assert_eq!(
        bob_state
            .order_pairs_trie_state_history
            .get(&doc_marty_pair)
            .unwrap()
            .len(),
        5
    );

    drop(trie_store_bob);

    std::thread::sleep(Duration::from_secs(2));

    // On inserting 5 more orders expiration for DOC:MARTY pair trie state history will be reset
    for order in &doc_marty_orders[10..] {
        insert_or_update_order(&ctx_bob, order.clone());
    }
    flush_trie(&ctx_bob);

    let ordermatch_ctx_bob = OrdermatchContext::from_ctx(&ctx_bob).unwrap();
    let trie_store_bob = ordermatch_ctx_bob.trie_store.lock();
    let bob_state = trie_store_bob.pubkeys_state.get(&pubkey_bob).unwrap();

    assert_eq!(
        bob_state
            .order_pairs_trie_state_history
            .get(&doc_marty_pair)
            .unwrap()
            .len(),
        10
    );

    drop(trie_store_bob);

    std::thread::sleep(Duration::from_secs(1));

    let ordermatch_ctx_bob = OrdermatchContext::from_ctx(&ctx_bob).unwrap();
    let trie_store_bob = ordermatch_ctx_bob.trie_store.lock();
    let bob_state = trie_store_bob.pubkeys_state.get(&pubkey_bob).unwrap();

    // After 3 seconds from inserting orders number 6-10 these orders have not expired due to updated expiration on inserting orders 11-15
    assert_eq!(
        bob_state
            .order_pairs_trie_state_history
            .get(&doc_marty_pair)
            .unwrap()
            .len(),
        10
    );
}

#[test]
fn test_trie_state_bytes() {
    let pubkey = "037310a8fb9fd8f198a1a21db830252ad681fccda580ed4101f3f6bfb98b34fab5";
    let base = "DOC";
    let rel = "MARTY";
    let price = BigRational::from_integer(1.into());
    let max_volume = BigRational::from_integer(u64::MAX.into());
    let min_volume = BigRational::from_integer(1.into());
    let uuid = Uuid::new_v4();
    let created_at = now_ms() / 1000;

    #[derive(Serialize)]
    struct OrderbookItemV1 {
        pubkey: String,
        base: String,
        rel: String,
        price: BigRational,
        max_volume: BigRational,
        min_volume: BigRational,
        uuid: Uuid,
        created_at: u64,
    }

    let old = OrderbookItemV1 {
        pubkey: pubkey.to_owned(),
        base: base.to_owned(),
        rel: rel.to_owned(),
        price: price.clone(),
        max_volume: max_volume.clone(),
        min_volume: min_volume.clone(),
        uuid,
        created_at,
    };

    let old_bytes = rmp_serde::to_vec(&old).unwrap();

    let new = OrderbookItem {
        pubkey: pubkey.to_owned(),
        base: base.to_owned(),
        rel: rel.to_owned(),
        price,
        max_volume,
        min_volume,
        uuid,
        created_at,
        base_protocol_info: vec![1, 2, 3],
        rel_protocol_info: vec![4, 5, 6],
        conf_settings: Some(OrderConfirmationsSettings {
            base_confs: 6,
            base_nota: false,
            rel_confs: 3,
            rel_nota: true,
        }),
    };

    let new_bytes = new.trie_state_bytes();

    assert_eq!(old_bytes, new_bytes);
}

#[test]
fn check_get_orderbook_p2p_res_serde() {
    #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
    struct OrderbookItemV1 {
        pubkey: String,
        base: String,
        rel: String,
        price: BigRational,
        max_volume: BigRational,
        min_volume: BigRational,
        uuid: Uuid,
        created_at: u64,
    }

    type PubkeyOrdersV1 = Vec<(Uuid, OrderbookItemV1)>;

    impl From<OrderbookItem> for OrderbookItemV1 {
        fn from(o: OrderbookItem) -> Self {
            OrderbookItemV1 {
                pubkey: o.pubkey,
                base: o.base,
                rel: o.rel,
                price: o.price,
                max_volume: o.max_volume,
                min_volume: o.min_volume,
                uuid: o.uuid,
                created_at: o.created_at,
            }
        }
    }

    #[derive(Debug, Deserialize, PartialEq, Serialize)]
    struct GetOrderbookPubkeyItemV1 {
        /// Timestamp of the latest keep alive message received.
        last_keep_alive: u64,
        /// last signed OrdermatchMessage payload
        last_signed_pubkey_payload: Vec<u8>,
        /// Requested orders.
        orders: PubkeyOrdersV1,
    }

    #[derive(Debug, Deserialize, PartialEq, Serialize)]
    struct GetOrderbookResV1 {
        /// Asks and bids grouped by pubkey.
        pubkey_orders: HashMap<String, GetOrderbookPubkeyItemV1>,
    }

    let orders = make_random_orders("".into(), &[1; 32], "DOC".into(), "MARTY".into(), 10);
    let item = GetOrderbookPubkeyItemV1 {
        last_keep_alive: 100,
        last_signed_pubkey_payload: vec![1, 2, 3],
        orders: orders
            .clone()
            .into_iter()
            .map(|order| (order.uuid, order.into()))
            .collect(),
    };

    let v1 = GetOrderbookResV1 {
        pubkey_orders: HashMap::from_iter(std::iter::once(("pubkey".into(), item))),
    };

    let v1_serialized = rmp_serde::to_vec(&v1).unwrap();

    let mut new: GetOrderbookRes = rmp_serde::from_read_ref(&v1_serialized).unwrap();
    new.protocol_infos.insert(Uuid::new_v4(), BaseRelProtocolInfo {
        base: vec![1],
        rel: vec![2],
    });
    new.conf_infos.insert(Uuid::new_v4(), OrderConfirmationsSettings {
        base_confs: 6,
        base_nota: false,
        rel_confs: 3,
        rel_nota: true,
    });

    let new_serialized = rmp_serde::to_vec(&new).unwrap();

    let v1_from_new: GetOrderbookResV1 = rmp_serde::from_read_ref(&new_serialized).unwrap();
    assert_eq!(v1, v1_from_new);

    #[derive(Debug, Deserialize, PartialEq, Serialize)]
    struct GetOrderbookResV2 {
        /// Asks and bids grouped by pubkey.
        pubkey_orders: HashMap<String, GetOrderbookPubkeyItem>,
        #[serde(default)]
        protocol_infos: HashMap<Uuid, BaseRelProtocolInfo>,
    }

    let item = GetOrderbookPubkeyItem {
        last_keep_alive: 100,
        last_signed_pubkey_payload: vec![1, 2, 3],
        orders: orders.into_iter().map(|order| (order.uuid, order.into())).collect(),
    };

    let v2 = GetOrderbookResV2 {
        pubkey_orders: HashMap::from_iter(std::iter::once(("pubkey".into(), item))),
        protocol_infos: HashMap::from_iter(std::iter::once((Uuid::new_v4(), BaseRelProtocolInfo {
            base: vec![1],
            rel: vec![2],
        }))),
    };

    let v2_serialized = rmp_serde::to_vec(&v2).unwrap();

    let mut new: GetOrderbookRes = rmp_serde::from_read_ref(&v2_serialized).unwrap();
    new.conf_infos.insert(Uuid::new_v4(), OrderConfirmationsSettings {
        base_confs: 6,
        base_nota: false,
        rel_confs: 3,
        rel_nota: true,
    });

    let new_serialized = rmp_serde::to_vec(&new).unwrap();

    let v2_from_new: GetOrderbookResV2 = rmp_serde::from_read_ref(&new_serialized).unwrap();
    assert_eq!(v2, v2_from_new);
}

#[test]
fn check_sync_pubkey_state_p2p_res_serde() {
    #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
    struct OrderbookItemV1 {
        pubkey: String,
        base: String,
        rel: String,
        price: BigRational,
        max_volume: BigRational,
        min_volume: BigRational,
        uuid: Uuid,
        created_at: u64,
    }

    impl From<OrderbookItem> for OrderbookItemV1 {
        fn from(o: OrderbookItem) -> Self {
            OrderbookItemV1 {
                pubkey: o.pubkey,
                base: o.base,
                rel: o.rel,
                price: o.price,
                max_volume: o.max_volume,
                min_volume: o.min_volume,
                uuid: o.uuid,
                created_at: o.created_at,
            }
        }
    }

    #[derive(Debug, Deserialize, Serialize)]
    struct SyncPubkeyOrderbookStateResV1 {
        /// last signed OrdermatchMessage payload from pubkey
        last_signed_pubkey_payload: Vec<u8>,
        pair_orders_diff: HashMap<AlbOrderedOrderbookPair, DeltaOrFullTrie<Uuid, OrderbookItemV1>>,
    }

    let orders = make_random_orders("".into(), &[1; 32], "DOC".into(), "MARTY".into(), 10);

    let v1 = SyncPubkeyOrderbookStateResV1 {
        last_signed_pubkey_payload: vec![1, 2, 3, 4],
        pair_orders_diff: HashMap::from_iter(iter::once((
            alb_ordered_pair("DOC", "MARTY"),
            DeltaOrFullTrie::FullTrie(
                orders
                    .clone()
                    .into_iter()
                    .map(|order| (order.uuid, order.into()))
                    .collect(),
            ),
        ))),
    };

    let v1_serialized = rmp_serde::to_vec(&v1).unwrap();

    let mut new: SyncPubkeyOrderbookStateRes = rmp_serde::from_read_ref(&v1_serialized).unwrap();
    new.protocol_infos.insert(Uuid::new_v4(), BaseRelProtocolInfo {
        base: vec![1],
        rel: vec![2],
    });
    new.conf_infos.insert(Uuid::new_v4(), OrderConfirmationsSettings {
        base_confs: 6,
        base_nota: false,
        rel_confs: 3,
        rel_nota: true,
    });

    let new_serialized = rmp_serde::to_vec(&new).unwrap();

    let _v1_from_new: SyncPubkeyOrderbookStateResV1 = rmp_serde::from_read_ref(&new_serialized).unwrap();

    #[derive(Debug, Deserialize, Serialize)]
    struct SyncPubkeyOrderbookStateResV2 {
        /// last signed OrdermatchMessage payload from pubkey
        last_signed_pubkey_payload: Vec<u8>,
        pair_orders_diff: HashMap<AlbOrderedOrderbookPair, DeltaOrFullTrie<Uuid, OrderbookP2PItem>>,
        #[serde(default)]
        protocol_infos: HashMap<Uuid, BaseRelProtocolInfo>,
    }

    let v2 = SyncPubkeyOrderbookStateResV2 {
        last_signed_pubkey_payload: vec![1, 2, 3, 4],
        pair_orders_diff: HashMap::from_iter(iter::once((
            alb_ordered_pair("DOC", "MARTY"),
            DeltaOrFullTrie::FullTrie(orders.into_iter().map(|order| (order.uuid, order.into())).collect()),
        ))),
        protocol_infos: HashMap::from_iter(std::iter::once((Uuid::new_v4(), BaseRelProtocolInfo {
            base: vec![1],
            rel: vec![2],
        }))),
    };

    let v2_serialized = rmp_serde::to_vec(&v2).unwrap();

    let mut new: SyncPubkeyOrderbookStateRes = rmp_serde::from_read_ref(&v2_serialized).unwrap();
    new.conf_infos.insert(Uuid::new_v4(), OrderConfirmationsSettings {
        base_confs: 6,
        base_nota: false,
        rel_confs: 3,
        rel_nota: true,
    });

    let new_serialized = rmp_serde::to_vec(&new).unwrap();

    let _v2_from_new: SyncPubkeyOrderbookStateResV2 = rmp_serde::from_read_ref(&new_serialized).unwrap();
}

#[test]
fn check_order_serde() {
    let order_json = include_str!("for_tests/check_order_serde_payload.json");
    let _order: Order = json::from_str(order_json).unwrap();
}

// ---------------------------------------------------------------------------
// Pure function tests — no MmArc, no coins required
// ---------------------------------------------------------------------------

#[test]
fn test_alb_ordered_pair_reversed() {
    // Same pair regardless of argument order
    assert_eq!(alb_ordered_pair("A", "B"), alb_ordered_pair("B", "A"));
}

#[test]
fn test_alb_ordered_pair_same_coin() {
    assert_eq!("BTC:BTC", alb_ordered_pair("BTC", "BTC"));
}

#[test]
fn test_alb_ordered_pair_case_sensitive() {
    // Uppercase sorts before lowercase in ASCII
    let pair = alb_ordered_pair("abc", "ABC");
    assert!(pair.starts_with("ABC:"));
}

#[test]
fn test_parse_orderbook_pair_empty_topic() {
    assert_eq!(None, parse_orderbook_pair_from_topic(""));
}

#[test]
fn test_parse_orderbook_pair_wrong_prefix() {
    assert_eq!(None, parse_orderbook_pair_from_topic("swap/BTC:KMD"));
}

#[test]
fn test_parse_orderbook_pair_no_colon() {
    assert_eq!(None, parse_orderbook_pair_from_topic("orbk/BTCKMD"));
}

#[test]
fn test_parse_orderbook_pair_only_prefix() {
    assert_eq!(None, parse_orderbook_pair_from_topic("orbk/"));
}

#[test]
fn test_parse_orderbook_pair_trailing_colon() {
    // "orbk/BTC:" — second part is empty
    assert_eq!(None, parse_orderbook_pair_from_topic("orbk/BTC:"));
}

#[test]
fn test_parse_orderbook_pair_valid_roundtrip() {
    let topic = orderbook_topic_from_base_rel("ETH", "BTC");
    let parsed = parse_orderbook_pair_from_topic(&topic);
    assert!(parsed.is_some());
    let (a, b) = parsed.unwrap();
    // The topic is built from alb_ordered_pair, so order is alphabetical
    assert_eq!(a, "BTC");
    assert_eq!(b, "ETH");
}

// ---------------------------------------------------------------------------
// OrderConfirmationsSettings
// ---------------------------------------------------------------------------

#[test]
fn test_order_conf_settings_reversed() {
    let s = OrderConfirmationsSettings {
        base_confs: 3,
        base_nota: true,
        rel_confs: 1,
        rel_nota: false,
    };
    let r = s.reversed();
    assert_eq!(r.base_confs, 1);
    assert!(!r.base_nota);
    assert_eq!(r.rel_confs, 3);
    assert!(r.rel_nota);
}

#[test]
fn test_order_conf_settings_double_reverse_identity() {
    let s = OrderConfirmationsSettings {
        base_confs: 5,
        base_nota: true,
        rel_confs: 2,
        rel_nota: false,
    };
    let double = s.reversed().reversed();
    assert_eq!(double.base_confs, s.base_confs);
    assert_eq!(double.base_nota, s.base_nota);
    assert_eq!(double.rel_confs, s.rel_confs);
    assert_eq!(double.rel_nota, s.rel_nota);
}

#[test]
fn test_order_conf_settings_serde_roundtrip() {
    let s = OrderConfirmationsSettings {
        base_confs: 3,
        base_nota: true,
        rel_confs: 1,
        rel_nota: false,
    };
    let json = serde_json::to_string(&s).unwrap();
    let restored: OrderConfirmationsSettings = serde_json::from_str(&json).unwrap();
    assert_eq!(s, restored);
}

#[test]
fn test_order_conf_settings_default() {
    let s = OrderConfirmationsSettings::default();
    assert_eq!(s.base_confs, 0);
    assert_eq!(s.rel_confs, 0);
    assert!(!s.base_nota);
    assert!(!s.rel_nota);
}

// ---------------------------------------------------------------------------
// TakerOrderBuilder validation
// ---------------------------------------------------------------------------

fn make_test_coin_pair() -> (MmCoinEnum, MmCoinEnum) {
    let base = MmCoinEnum::Test(TestCoin::new("BASE"));
    let rel = MmCoinEnum::Test(TestCoin::new("REL"));
    (base, rel)
}

fn default_conf_settings() -> OrderConfirmationsSettings {
    OrderConfirmationsSettings {
        base_confs: 1,
        base_nota: false,
        rel_confs: 1,
        rel_nota: false,
    }
}

fn nonzero_pubkey() -> H256Json { H256Json::from([1u8; 32]) }

#[test]
fn test_taker_builder_base_equal_rel() {
    let coin = MmCoinEnum::Test(TestCoin::new("SAME"));
    let err = TakerOrderBuilder::new(&coin, &coin)
        .with_base_amount(10.into())
        .with_rel_amount(10.into())
        .with_sender_pubkey(nonzero_pubkey())
        .with_conf_settings(default_conf_settings())
        .build()
        .unwrap_err();
    assert!(matches!(err, TakerOrderBuildError::BaseEqualRel));
}

#[test]
fn test_taker_builder_zero_pubkey() {
    let (base, rel) = make_test_coin_pair();
    let err = TakerOrderBuilder::new(&base, &rel)
        .with_base_amount(10.into())
        .with_rel_amount(10.into())
        .with_conf_settings(default_conf_settings())
        // sender_pubkey defaults to zero
        .build()
        .unwrap_err();
    assert!(matches!(err, TakerOrderBuildError::SenderPubkeyIsZero));
}

#[test]
fn test_taker_builder_no_conf_settings() {
    let (base, rel) = make_test_coin_pair();
    let err = TakerOrderBuilder::new(&base, &rel)
        .with_base_amount(10.into())
        .with_rel_amount(10.into())
        .with_sender_pubkey(nonzero_pubkey())
        // conf_settings not set
        .build()
        .unwrap_err();
    assert!(matches!(err, TakerOrderBuildError::ConfsSettingsNotSet));
}

#[test]
fn test_taker_builder_success() {
    let (base, rel) = make_test_coin_pair();
    let result = TakerOrderBuilder::new(&base, &rel)
        .with_base_amount(10.into())
        .with_rel_amount(20.into())
        .with_sender_pubkey(nonzero_pubkey())
        .with_conf_settings(default_conf_settings())
        .build();
    let order = match result {
        Ok(o) => o,
        Err(e) => panic!("build failed: {}", e),
    };
    assert_eq!(order.request.base, "BASE");
    assert_eq!(order.request.rel, "REL");
}

// ---------------------------------------------------------------------------
// MakerOrderBuilder validation
// ---------------------------------------------------------------------------

#[test]
fn test_maker_builder_base_equal_rel() {
    let coin = MmCoinEnum::Test(TestCoin::new("SAME"));
    let err = MakerOrderBuilder::new(&coin, &coin)
        .with_max_base_vol(10.into())
        .with_price(1.into())
        .with_conf_settings(default_conf_settings())
        .build()
        .unwrap_err();
    assert!(matches!(err, MakerOrderBuildError::BaseEqualRel));
}

#[test]
fn test_maker_builder_no_conf_settings() {
    let (base, rel) = make_test_coin_pair();
    let err = MakerOrderBuilder::new(&base, &rel)
        .with_max_base_vol(10.into())
        .with_price(1.into())
        .build()
        .unwrap_err();
    assert!(matches!(err, MakerOrderBuildError::ConfSettingsNotSet));
}

#[test]
fn test_maker_builder_price_too_low() {
    let (base, rel) = make_test_coin_pair();
    let err = MakerOrderBuilder::new(&base, &rel)
        .with_max_base_vol(10.into())
        .with_price(MmNumber::from(0))
        .with_conf_settings(default_conf_settings())
        .build()
        .unwrap_err();
    assert!(matches!(err, MakerOrderBuildError::PriceTooLow { .. }));
}

#[test]
fn test_maker_builder_success() {
    let (base, rel) = make_test_coin_pair();
    let result = MakerOrderBuilder::new(&base, &rel)
        .with_max_base_vol(10.into())
        .with_price(1.into())
        .with_conf_settings(default_conf_settings())
        .build();
    let order = match result {
        Ok(o) => o,
        Err(e) => panic!("build failed: {}", e),
    };
    assert_eq!(order.base, "BASE");
    assert_eq!(order.rel, "REL");
}

// ---------------------------------------------------------------------------
// TakerOrderBuildError — Display
// ---------------------------------------------------------------------------

#[test]
fn test_taker_order_build_error_display() {
    let e = TakerOrderBuildError::BaseEqualRel;
    assert!(e.to_string().contains("same as base"));

    let e = TakerOrderBuildError::SenderPubkeyIsZero;
    assert!(e.to_string().contains("zero"));

    let e = TakerOrderBuildError::ConfsSettingsNotSet;
    assert!(e.to_string().contains("settings"));

    let e = TakerOrderBuildError::BaseAmountTooLow {
        actual: 1.into(),
        threshold: 10.into(),
    };
    assert!(e.to_string().contains("too low"));
}

// ---------------------------------------------------------------------------
// MakerOrderBuildError — Display
// ---------------------------------------------------------------------------

#[test]
fn test_maker_order_build_error_display() {
    let e = MakerOrderBuildError::BaseEqualRel;
    assert!(e.to_string().contains("same as base"));

    let e = MakerOrderBuildError::ConfSettingsNotSet;
    assert!(e.to_string().contains("settings"));

    let e = MakerOrderBuildError::PriceTooLow {
        actual: MmNumber::from(0),
        threshold: 1.into(),
    };
    assert!(e.to_string().contains("too low"));
}

// ---------------------------------------------------------------------------
// Pairs that cannot trade (CRD ch.60 R60.2.3, R60.6.5)
// ---------------------------------------------------------------------------

#[test]
fn test_trading_not_supported_errors() {
    let e = TakerOrderBuildError::TradingNotSupported {
        coin: "tBTC-lightning".into(),
    };
    assert_eq!(e.to_string(), "Trading is not supported for coin tBTC-lightning");
    let e = MakerOrderBuildError::TradingNotSupported {
        coin: "tBTC-lightning".into(),
    };
    assert_eq!(e.to_string(), "Trading is not supported for coin tBTC-lightning");
}

#[test]
fn test_ordinary_pair_is_tradable() {
    let a = MmCoinEnum::Test(TestCoin::new("RICK"));
    let b = MmCoinEnum::Test(TestCoin::new("MORTY"));
    assert_eq!(untradable_coin_ticker(&a, &b), None);
}

// ---------------------------------------------------------------------------
// TakerRequest — serde roundtrip
// ---------------------------------------------------------------------------

#[test]
fn test_taker_request_serde_roundtrip() {
    let req = TakerRequest {
        base: "BTC".into(),
        rel: "KMD".into(),
        base_amount: 1.into(),
        rel_amount: 100.into(),
        action: TakerAction::Buy,
        uuid: Uuid::new_v4(),
        sender_pubkey: H256Json::default(),
        dest_pub_key: H256Json::default(),
        match_by: MatchBy::Any,
        conf_settings: Some(OrderConfirmationsSettings {
            base_confs: 3,
            base_nota: true,
            rel_confs: 1,
            rel_nota: false,
        }),
        base_protocol_info: None,
        rel_protocol_info: None,
        swap_version: SwapVersion::default(),
    };
    let json = serde_json::to_string(&req).unwrap();
    let restored: TakerRequest = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.base, "BTC");
    assert_eq!(restored.rel, "KMD");
    assert_eq!(restored.action, TakerAction::Buy);
}

// ---------------------------------------------------------------------------
// MatchBy variants serde
// ---------------------------------------------------------------------------

#[test]
fn test_match_by_any_serde() {
    let mb = MatchBy::Any;
    let json = serde_json::to_string(&mb).unwrap();
    let restored: MatchBy = serde_json::from_str(&json).unwrap();
    assert!(matches!(restored, MatchBy::Any));
}

#[test]
fn test_match_by_orders_serde() {
    let uuids: HashSet<Uuid> = iter::once(Uuid::new_v4()).collect();
    let mb = MatchBy::Orders(uuids.clone());
    let json = serde_json::to_string(&mb).unwrap();
    let restored: MatchBy = serde_json::from_str(&json).unwrap();
    match restored {
        MatchBy::Orders(u) => assert_eq!(u, uuids),
        _ => panic!("Expected MatchBy::Orders"),
    }
}

#[test]
fn test_match_by_pubkeys_serde() {
    let keys: HashSet<H256Json> = iter::once(H256Json::from([1u8; 32])).collect();
    let mb = MatchBy::Pubkeys(keys);
    let json = serde_json::to_string(&mb).unwrap();
    let restored: MatchBy = serde_json::from_str(&json).unwrap();
    assert!(matches!(restored, MatchBy::Pubkeys(_)));
}

/// CRD ch.35 R35.2.7 / kdf-reloaded#16: the order paths (`setprice`, `sell`, `buy`) reject a coin whose
/// `MmCoin::wallet_only` is true with the wallet-only error. A custom ERC-20 token reports `wallet_only` true
/// without a coins-config entry (proved against the real activation in `coins_activation`); this test drives the
/// real order-placement functions with a registered coin whose `wallet_only` is true. A real custom `EthCoin`
/// cannot be built offline from `mm2_main`, so the coin here is a `TestCoin` made wallet-only by its config entry.
#[test]
fn order_placement_rejects_a_wallet_only_coin() {
    use coins::{lp_register_coin, RegisterCoinParams};

    let ctx = MmCtxBuilder::new()
        .with_conf(json::json!({ "coins": [{ "coin": "WALLET", "wallet_only": true }, { "coin": "TRADE" }] }))
        .into_mm_arc();
    for ticker in ["WALLET", "TRADE"] {
        block_on(lp_register_coin(
            &ctx,
            TestCoin::new(ticker).into(),
            RegisterCoinParams {
                ticker: ticker.to_owned(),
                tx_history: false,
            },
        ))
        .unwrap_or_else(|e| panic!("cannot register {ticker}: {e}"));
    }

    for (base, rel, wallet_coin) in [("WALLET", "TRADE", "Base"), ("TRADE", "WALLET", "Rel")] {
        let expected = format!("{wallet_coin} coin WALLET is wallet only");

        let set_price: SetPriceReq =
            json::from_value(json::json!({ "base": base, "rel": rel, "price": 1, "volume": 1 })).unwrap();
        let err = block_on(create_maker_order(&ctx, set_price)).unwrap_err();
        assert!(err.ends_with(&expected), "{err}");

        let order = |method: &str| json::json!({ "method": method, "base": base, "rel": rel, "price": 1, "volume": 1 });
        let err = block_on(sell(ctx.clone(), order("sell"))).unwrap_err();
        assert!(err.ends_with(&expected), "{err}");
        let err = block_on(buy(ctx.clone(), order("buy"))).unwrap_err();
        assert!(err.ends_with(&expected), "{err}");
    }
}

// ---------------------------------------------------------------------------
// ch.57 T11 -- the field report, end to end (R57.7A.1, R57.7A.4)
//
// A real eight-decimal ERC-20 cannot be built from this crate's lib tests without live endpoints, so
// `TestCoin::min_trading_vol` is mocked to the value R57.7A.1 yields for eight decimals (10^-8). The EVM
// value itself is pinned by `t10_evm_min_trading_vol_is_one_smallest_unit` in the `coins` crate. The
// preview and `min_trading_vol_v2` parts of T11 live in `lp_swap/taker_swap.rs` (`t11_*`), next to the
// private items they exercise.
// ---------------------------------------------------------------------------

mod t11_field_report {
    use super::*;
    use coins::{CoinsContext, MarketCoinOps, MmCoinEnum};

    fn one_smallest_unit_8() -> MmNumber { MmNumber::from(BigRational::new(1.into(), 100_000_000.into())) }

    fn field_report_volume() -> MmNumber { MmNumber::from("0.00072437") }

    fn half_smallest_unit_8() -> MmNumber { MmNumber::from(BigRational::new(1.into(), 200_000_000.into())) }

    fn mock_floor() { TestCoin::min_trading_vol.mock_safe(|_| MockResult::Return(one_smallest_unit_8())); }

    fn coins() -> (MmCoinEnum, MmCoinEnum) {
        (
            MmCoinEnum::Test(TestCoin::new("WBTC8")),
            MmCoinEnum::Test(TestCoin::new("REL")),
        )
    }

    #[test]
    fn taker_order_builder() {
        mock_floor();
        let (base, rel) = coins();
        let build = |base_amount: MmNumber, rel_amount: MmNumber| {
            TakerOrderBuilder::new(&base, &rel)
                .with_base_amount(base_amount)
                .with_rel_amount(rel_amount)
                .with_sender_pubkey(nonzero_pubkey())
                .with_conf_settings(default_conf_settings())
                .build()
        };

        if let Err(e) = build(field_report_volume(), field_report_volume()) {
            panic!("the field-report volume must be accepted: {}", e);
        }

        match build(half_smallest_unit_8(), MmNumber::from(1)) {
            Err(TakerOrderBuildError::BaseAmountTooLow { actual, threshold }) => {
                assert_eq!(actual, half_smallest_unit_8());
                assert_eq!(threshold, one_smallest_unit_8());
            },
            Err(e) => panic!("expected BaseAmountTooLow, got {}", e),
            Ok(_) => panic!("expected BaseAmountTooLow, got an order"),
        }
    }

    #[test]
    fn maker_order_builder() {
        mock_floor();
        let (base, rel) = coins();
        let build = |volume: MmNumber| {
            MakerOrderBuilder::new(&base, &rel)
                .with_max_base_vol(volume)
                .with_price(1.into())
                .with_conf_settings(default_conf_settings())
                .build()
        };

        let order = match build(field_report_volume()) {
            Ok(order) => order,
            Err(e) => panic!("the field-report volume must be accepted: {}", e),
        };
        assert_eq!(order.min_base_vol, one_smallest_unit_8());

        match build(half_smallest_unit_8()) {
            Err(MakerOrderBuildError::MaxBaseVolTooLow { actual, threshold }) => {
                assert_eq!(actual, half_smallest_unit_8());
                assert_eq!(threshold, one_smallest_unit_8());
            },
            Err(e) => panic!("expected MaxBaseVolTooLow, got {}", e),
            Ok(_) => panic!("expected MaxBaseVolTooLow, got an order"),
        }
    }

    #[test]
    fn legacy_min_trading_vol_reports_the_floor() {
        mock_floor();
        let ctx = MmCtxBuilder::default().into_mm_arc();
        let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();
        block_on(coins_ctx.add_coin(MmCoinEnum::Test(TestCoin::new("WBTC8")))).unwrap();

        let legacy = block_on(crate::mm2::rpc::lp_commands_legacy::min_trading_vol(
            ctx,
            json!({"coin": "WBTC8"}),
        ))
        .unwrap();
        let legacy: Json = json::from_slice(legacy.body()).unwrap();
        assert_eq!(legacy["result"]["coin"], "WBTC8");
        assert_eq!(legacy["result"]["min_trading_vol"], "0.00000001");
        assert_eq!(legacy["result"]["min_trading_vol_fraction"]["numer"], "1");
        assert_eq!(legacy["result"]["min_trading_vol_fraction"]["denom"], "100000000");
    }
}

/// CRD ch.60 T60.6: the pre-trade spend-safety check (R60.6.7). The coin-side safe amounts are pinned in the
/// `coins` crate (a dynamic rate of 20,000 with dust 1,000 gives 0.0001092; the Sia fixed miner fee plus one
/// hasting); here `TestCoin` reports them, so the checks of the order paths run without any network.
#[cfg(not(target_arch = "wasm32"))]
mod t60_6_pre_trade_spend_check_tests {
    use super::*;
    use coins::{CoinsContext, MarketCoinOps, MmCoinEnum, SpendSafeAmountError};
    use common::mm_number::BigRational;
    use mm2_err_handle::prelude::MmError;

    fn units_8(units: i64) -> MmNumber { MmNumber::from(BigRational::new(units.into(), 100_000_000.into())) }

    /// Safe amounts by ticker: `DYN` is the dynamic-fee coin of T60.6 (a), `SIA` the Sia coin of (b), `KMD` the
    /// fixed-fee coin of (c), `PLAIN` a coin that is not covered. All of them have the floor 0.0001 except `SIA`.
    fn safe_amount_of(ticker: &str) -> Option<MmNumber> {
        match ticker {
            "DYN" => Some(units_8(10_920)),
            "KMD" => Some(units_8(1_496)),
            // The Sia safe amount is its fixed miner fee plus one hasting: 0.0200..01 here is stood in by 0.02.
            "SIA" => Some(MmNumber::from("0.02000000000000000000000001")),
            _ => None,
        }
    }

    fn mock_coins(estimate_fails: bool) {
        TestCoin::min_trading_vol.mock_safe(|coin| {
            let floor = if coin.ticker() == "SIA" {
                MmNumber::from("0.000000000000000000000001")
            } else {
                units_8(10_000)
            };
            MockResult::Return(floor)
        });
        TestCoin::spend_safe_amount.mock_safe(move |coin| {
            let ticker = coin.ticker().to_owned();
            MockResult::Return(Box::pin(async move {
                if estimate_fails && ticker == "DYN" {
                    return MmError::err(SpendSafeAmountError::EstimateUnavailable("no estimate".to_owned()));
                }
                Ok(safe_amount_of(&ticker))
            }))
        });
    }

    fn ctx_with(conf: Json, tickers: &[&str]) -> MmArc {
        let ctx = MmCtxBuilder::new().with_conf(conf).into_mm_arc();
        let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();
        for ticker in tickers {
            block_on(coins_ctx.add_coin(MmCoinEnum::Test(TestCoin::new(ticker)))).unwrap();
        }
        ctx
    }

    fn pair(base: &str, rel: &str) -> (MmCoinEnum, MmCoinEnum) {
        (
            MmCoinEnum::Test(TestCoin::new(base)),
            MmCoinEnum::Test(TestCoin::new(rel)),
        )
    }

    fn check(
        ctx: &MmArc,
        base: &str,
        rel: &str,
        min_volume: &'static str,
        price: &'static str,
    ) -> Result<(), SpendSafetyError> {
        let (base, rel) = pair(base, rel);
        block_on(check_pre_trade_spend_safety(
            ctx,
            &base,
            &rel,
            Some(MmNumber::from(min_volume)),
            &MmNumber::from(price),
        ))
    }

    fn assert_too_low(result: Result<(), SpendSafetyError>, expected_coin: &str, expected_threshold: &'static str) {
        match result {
            Err(SpendSafetyError::VolumeTooLow { coin, threshold, .. }) => {
                assert_eq!(coin, expected_coin);
                assert_eq!(MmNumber::from(threshold), MmNumber::from(expected_threshold));
            },
            other => panic!("expected the volume-too-low refusal, got {:?}", other),
        }
    }

    /// (a): the dynamic-fee coin with S = 0.0001092 refuses a minimum volume of 0.0001 and accepts 0.00011.
    #[test]
    fn t60_6_a_dynamic_fee_coin_minimum_volume_is_checked_against_the_safe_amount() {
        mock_coins(false);
        let ctx = ctx_with(json!({}), &[]);
        assert_too_low(check(&ctx, "DYN", "KMD", "0.0001", "1"), "DYN", "0.0001092");
        assert!(check(&ctx, "DYN", "KMD", "0.00011", "1").is_ok());
        // The rel leg is the minimum volume times the price of the rel coin.
        assert_too_low(check(&ctx, "KMD", "DYN", "0.0001", "1"), "DYN", "0.0001092");
        assert!(check(&ctx, "KMD", "DYN", "0.00011", "1").is_ok());
        assert_too_low(check(&ctx, "KMD", "DYN", "0.0002", "0.5"), "DYN", "0.0001092");
        assert!(check(&ctx, "KMD", "DYN", "0.0002", "0.6").is_ok());
    }

    /// An order with no operator-set minimum volume is refused at its default, never raised.
    #[test]
    fn t60_6_a_default_minimum_volume_is_refused_not_raised() {
        mock_coins(false);
        let ctx = ctx_with(json!({}), &[]);
        let (base, rel) = pair("DYN", "KMD");
        let result = block_on(check_pre_trade_spend_safety(
            &ctx,
            &base,
            &rel,
            None,
            &MmNumber::from("1"),
        ));
        assert_too_low(result, "DYN", "0.0001092");
    }

    /// (b): a Sia leg at the floor is refused with the miner fee plus one hasting as the threshold.
    #[test]
    fn t60_6_b_sia_leg_equal_to_the_miner_fee_is_refused() {
        mock_coins(false);
        let ctx = ctx_with(json!({}), &[]);
        assert_too_low(
            check(&ctx, "SIA", "KMD", "0.02", "100"),
            "SIA",
            "0.02000000000000000000000001",
        );
        assert!(check(&ctx, "SIA", "KMD", "0.03", "100").is_ok());
    }

    /// (c): KMD at its floor of 0.0001 (S = 496 + 1,000 base units) is accepted; so is a coin that is not covered.
    #[test]
    fn t60_6_c_kmd_at_its_floor_and_uncovered_coins_are_accepted() {
        mock_coins(false);
        let ctx = ctx_with(json!({}), &[]);
        assert!(check(&ctx, "KMD", "PLAIN", "0.0001", "1").is_ok());
        assert!(check(&ctx, "PLAIN", "PLAIN2", "0.0001", "1").is_ok());
    }

    /// A dynamic-fee estimate failure refuses with a typed error.
    #[test]
    fn t60_6_estimate_failure_is_a_typed_refusal() {
        mock_coins(true);
        let ctx = ctx_with(json!({}), &[]);
        match check(&ctx, "DYN", "KMD", "1", "1") {
            Err(SpendSafetyError::EstimateUnavailable { coin, .. }) => assert_eq!(coin, "DYN"),
            other => panic!("expected the estimate-unavailable refusal, got {:?}", other),
        }
    }

    /// (e): with `pre_trade_spend_check` false the order of (a) is accepted; only an explicit false switches it off.
    #[test]
    fn t60_6_e_switch_false_disables_the_check() {
        mock_coins(false);
        for (conf, enabled) in [
            (json!({}), true),
            (json!({ "pre_trade_spend_check": true }), true),
            (json!({ "pre_trade_spend_check": null }), true),
            (json!({ "pre_trade_spend_check": "false" }), true),
            (json!({ "pre_trade_spend_check": false }), false),
        ] {
            let ctx = ctx_with(conf.clone(), &[]);
            assert_eq!(pre_trade_spend_check_enabled(&ctx), enabled, "{conf}");
            assert_eq!(check(&ctx, "DYN", "KMD", "0.0001", "1").is_ok(), !enabled, "{conf}");
        }
    }

    /// (a) through the `setprice` path: refused with the typed volume-too-low text, and the operator-set minimum
    /// volume of an order is what is checked, whether or not one was given.
    #[test]
    fn t60_6_setprice_is_refused_before_anything_else_happens() {
        mock_coins(false);
        let ctx = ctx_with(json!({}), &["DYN", "KMD"]);
        for body in [
            json!({"base": "DYN", "rel": "KMD", "price": "1", "volume": "1", "min_volume": "0.0001"}),
            json!({"base": "DYN", "rel": "KMD", "price": "1", "volume": "1"}),
        ] {
            let req: SetPriceReq = json::from_value(body).unwrap();
            let err = match block_on(create_maker_order(&ctx, req)) {
                Err(e) => e,
                Ok(_) => panic!("setprice must be refused"),
            };
            assert!(
                err.contains("The volume 0.0001 of the DYN coin is too low, required at least 0.0001092"),
                "{err}"
            );
        }
    }

    /// `buy` and `sell` reach the same check through `lp_auto_buy`.
    #[test]
    fn t60_6_buy_is_refused() {
        mock_coins(false);
        let ctx = ctx_with(json!({}), &["DYN", "KMD"]);
        let (base, rel) = pair("DYN", "KMD");
        let input: AutoBuyInput = json::from_value(json!({
            "base": "DYN", "rel": "KMD", "price": "1", "volume": "1", "method": "buy", "min_volume": "0.0001"
        }))
        .unwrap();
        let err = match block_on(lp_auto_buy(&ctx, &base, &rel, input)) {
            Err(e) => e,
            Ok(_) => panic!("buy must be refused"),
        };
        assert!(
            err.contains("The volume 0.0001 of the DYN coin is too low, required at least 0.0001092"),
            "{err}"
        );
    }

    /// `update_maker_order` checks the order as updated.
    #[test]
    fn t60_6_update_maker_order_is_refused() {
        mock_coins(false);
        let ctx = ctx_with(json!({}), &["DYN", "KMD"]);
        let (base, rel) = pair("DYN", "KMD");
        let order = MakerOrderBuilder::new(&base, &rel)
            .with_max_base_vol(1.into())
            .with_min_base_vol(Some(units_8(11_000)))
            .with_price(1.into())
            .with_conf_settings(OrderConfirmationsSettings::default())
            .build_unchecked();
        let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
        ordermatch_ctx
            .maker_orders_ctx
            .lock()
            .add_order(&order, Arc::new(AsyncMutex::new(order.clone())));
        let req = MakerOrderUpdateReq {
            uuid: order.uuid,
            new_price: None,
            max: None,
            volume_delta: None,
            min_volume: Some(units_8(10_000)),
            base_confs: None,
            base_nota: None,
            rel_confs: None,
            rel_nota: None,
        };
        let err = match block_on(update_maker_order(&ctx, req)) {
            Err(e) => e,
            Ok(_) => panic!("the update must be refused"),
        };
        assert!(
            err.contains("The volume 0.0001 of the DYN coin is too low, required at least 0.0001092"),
            "{err}"
        );
    }

    /// (f): an order or request received from a peer is handled exactly as without the rule: matching never
    /// consults the safe amount, so a request below it is matched at the order's own minimum volume.
    #[test]
    fn t60_6_f_incoming_requests_are_unaffected() {
        TestCoin::spend_safe_amount.mock_safe(|_| panic!("the safe amount must not be consulted when matching"));
        let maker = MakerOrderBuilder::new(&pair("DYN", "KMD").0, &pair("DYN", "KMD").1)
            .with_max_base_vol(1.into())
            .with_min_base_vol(Some(units_8(10_000)))
            .with_price(1.into())
            .with_conf_settings(OrderConfirmationsSettings::default())
            .build_unchecked();
        let request = TakerRequest {
            base: "DYN".into(),
            rel: "KMD".into(),
            uuid: Uuid::new_v4(),
            dest_pub_key: H256Json::default(),
            sender_pubkey: H256Json::default(),
            base_amount: units_8(10_000),
            rel_amount: units_8(10_000),
            action: TakerAction::Buy,
            match_by: MatchBy::Any,
            conf_settings: None,
            base_protocol_info: None,
            rel_protocol_info: None,
            swap_version: SwapVersion::default(),
        };
        assert_eq!(
            maker.match_with_request(&request),
            OrderMatchResult::Matched((units_8(10_000), units_8(10_000)))
        );
    }
}

use super::orderbook_rpc::OrderbookReq;

// ---- Legacy `orderbook` address rendering (CRD ch.32 R-F6c..R-F6e, T-F6.1..T-F6.4; ch.39 R39.5.3) ----

const ZHTLC_COIN_CONF: &str = r#"{"coin":"ZOMBIE","asset":"ZOMBIE","mm2":1,"protocol":{"type":"ZHTLC","protocol_data":{"consensus_params":{"overwinter_activation_height":0,"sapling_activation_height":1,"blossom_activation_height":null,"heartwood_activation_height":null,"canopy_activation_height":null,"coin_type":133,"hrp_sapling_extended_spending_key":"secret-extended-key-main","hrp_sapling_extended_full_viewing_key":"zxviews","hrp_sapling_payment_address":"zs","b58_pubkey_address_prefix":[28,184],"b58_script_address_prefix":[28,189]},"z_derivation_path":"m/32'/133'"}}}"#;

pub(super) fn legacy_rendering_test_coins() -> Json {
    json::json!([
        json::from_str::<Json>(ZHTLC_COIN_CONF).unwrap(),
        {"coin":"DOC","asset":"DOC","pubtype":60,"p2shtype":85,"wiftype":188,"txfee":1000,"mm2":1,"protocol":{"type":"UTXO"}},
        {"coin":"MARTY","asset":"MARTY","pubtype":60,"p2shtype":85,"wiftype":188,"txfee":1000,"mm2":1,"protocol":{"type":"UTXO"}},
        {"coin":"SOL","asset":"SOL","mm2":1,"protocol":{"type":"SOLANA"}},
        {"coin":"IRIS","asset":"IRIS","mm2":1,"protocol":{"type":"TENDERMINT","protocol_data":{
            "decimals":6,"denom":"uiris","account_prefix":"iaa","chain_id":"irishub-1"}}},
    ])
}

/// A context carrying the coins of [`legacy_rendering_test_coins`] whose orderbooks for the
/// given pairs are marked as already requested, so the RPC handlers read the in-memory book
/// without any P2P round trip.
pub(super) fn ctx_for_legacy_rendering_tests(pairs: &[(&str, &str)]) -> (MmArc, String, [u8; 32]) {
    let ctx = MmCtxBuilder::new()
        .with_conf(json::json!({ "coins": legacy_rendering_test_coins() }))
        .into_mm_arc();
    ctx.init_metrics().unwrap();
    ctx.secp256k1_key_pair
        .pin(key_pair_from_seed("passphrase").unwrap())
        .unwrap();
    let secret = *ctx.secp256k1_key_pair().private().secret;
    let pubkey = hex::encode(&**ctx.secp256k1_key_pair().public());
    let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
    {
        let mut subs = ordermatch_ctx.orderbook_subscriptions.write();
        for (a, b) in pairs {
            subs.insert(orderbook_topic_from_base_rel(a, b), OrderbookRequestingState::Requested);
            subs.insert(orderbook_topic_from_base_rel(b, a), OrderbookRequestingState::Requested);
        }
    }
    (ctx, pubkey, secret)
}

fn legacy_orderbook(ctx: &MmArc, base: &str, rel: &str) -> Json {
    let response = block_on(orderbook_rpc(ctx.clone(), json::json!({ "base": base, "rel": rel })))
        .expect("legacy orderbook must succeed");
    json::from_slice(response.body()).unwrap()
}

fn v2_orderbook(ctx: &MmArc, base: &str, rel: &str) -> Json {
    let req: OrderbookReq = json::from_value(json::json!({ "base": base, "rel": rel })).unwrap();
    let response = block_on(orderbook_rpc_v2(ctx.clone(), req)).expect("mmrpc 2.0 orderbook must succeed");
    json::to_value(response).unwrap()
}

fn addresses(side: &Json) -> Vec<Json> { side.as_array().unwrap().iter().map(|e| e["address"].clone()).collect() }

/// T-F6.1: a shielded-coin order does not fail the legacy `orderbook`; it is an ask / a bid with
/// the address `Shielded`, and mmrpc 2.0 shows `{"address_type":"Shielded"}`.
#[test]
fn test_legacy_orderbook_shielded_coin_order_t_f6_1() {
    let (ctx, pubkey, secret) = ctx_for_legacy_rendering_tests(&[("ZOMBIE", "DOC")]);
    for order in make_random_orders(pubkey, &secret, "ZOMBIE".into(), "DOC".into(), 1) {
        insert_or_update_order(&ctx, order);
    }
    flush_trie(&ctx);

    // The shielded coin as base: the order is an ask.
    let legacy = legacy_orderbook(&ctx, "ZOMBIE", "DOC");
    assert_eq!(legacy["numasks"], 1);
    assert_eq!(legacy["numbids"], 0);
    assert_eq!(addresses(&legacy["asks"]), vec![json::json!("Shielded")]);
    let v2 = v2_orderbook(&ctx, "ZOMBIE", "DOC");
    assert_eq!(v2["num_asks"], 1);
    assert_eq!(addresses(&v2["asks"]), vec![json::json!({"address_type": "Shielded"})]);

    // The shielded coin as rel: the same order is a bid.
    let legacy = legacy_orderbook(&ctx, "DOC", "ZOMBIE");
    assert_eq!(legacy["numasks"], 0);
    assert_eq!(legacy["numbids"], 1);
    assert_eq!(addresses(&legacy["bids"]), vec![json::json!("Shielded")]);
    let v2 = v2_orderbook(&ctx, "DOC", "ZOMBIE");
    assert_eq!(v2["num_bids"], 1);
    assert_eq!(addresses(&v2["bids"]), vec![json::json!({"address_type": "Shielded"})]);
}

/// T-F6.2: a UTXO order has the transparent string in the legacy `address` and the same string in
/// the mmrpc 2.0 `address_data`; the two generations agree on the outcome.
#[test]
fn test_legacy_orderbook_utxo_order_address_agrees_with_v2_t_f6_2() {
    let (ctx, pubkey, secret) = ctx_for_legacy_rendering_tests(&[("DOC", "MARTY")]);
    for order in make_random_orders(pubkey, &secret, "DOC".into(), "MARTY".into(), 1) {
        insert_or_update_order(&ctx, order);
    }
    flush_trie(&ctx);

    for (base, rel, side) in [("DOC", "MARTY", "asks"), ("MARTY", "DOC", "bids")] {
        let legacy = addresses(&legacy_orderbook(&ctx, base, rel)[side]);
        let v2 = addresses(&v2_orderbook(&ctx, base, rel)[side]);
        assert_eq!(legacy.len(), 1);
        let legacy_addr = legacy[0].as_str().expect("legacy address is a plain string");
        assert_ne!(legacy_addr, "Shielded");
        assert!(legacy_addr.starts_with('R'), "unexpected address {legacy_addr}");
        assert_eq!(v2, vec![
            json::json!({"address_type": "Transparent", "address_data": legacy_addr})
        ]);
    }
}

/// T-F6.3: an order that cannot be resolved is omitted and the rest of the response is served.
/// Two flavours: a coin family with no derivation at all (Solana), and one order of a
/// derivable family whose pubkey does not resolve, next to a good one on the same side.
#[test]
fn test_legacy_orderbook_unresolvable_order_is_omitted_t_f6_3() {
    let (ctx, pubkey, secret) = ctx_for_legacy_rendering_tests(&[("SOL", "DOC"), ("DOC", "MARTY")]);

    // Solana pair: the Solana-listed side is empty, the UTXO-listed side is served.
    for order in make_random_orders(pubkey.clone(), &secret, "SOL".into(), "DOC".into(), 1) {
        insert_or_update_order(&ctx, order);
    }
    for order in make_random_orders(pubkey.clone(), &secret, "DOC".into(), "SOL".into(), 1) {
        insert_or_update_order(&ctx, order);
    }
    // Same side: one resolvable and one unresolvable (malformed pubkey) order.
    let good = make_random_orders(pubkey.clone(), &secret, "DOC".into(), "MARTY".into(), 1).remove(0);
    let good_uuid = good.uuid;
    insert_or_update_order(&ctx, good);
    let mut bad = make_random_orders(pubkey, &secret, "DOC".into(), "MARTY".into(), 1).remove(0);
    bad.pubkey = "not-a-pubkey".to_owned();
    insert_or_update_order(&ctx, bad);
    flush_trie(&ctx);

    let legacy = legacy_orderbook(&ctx, "SOL", "DOC");
    assert_eq!(legacy["numasks"], 0);
    assert_eq!(legacy["numbids"], 1);
    let v2 = v2_orderbook(&ctx, "SOL", "DOC");
    assert_eq!(v2["num_asks"], 0);
    assert_eq!(v2["num_bids"], 1);

    let legacy = legacy_orderbook(&ctx, "DOC", "MARTY");
    let v2 = v2_orderbook(&ctx, "DOC", "MARTY");
    assert_eq!(legacy["numasks"], 1);
    assert_eq!(legacy["asks"][0]["uuid"], good_uuid.to_string());
    assert_eq!(v2["num_asks"], 1);
    assert_eq!(v2["asks"][0]["uuid"], good_uuid.to_string());
}

/// Reloaded-only regression (not reference behavior): the old legacy resolver refused the
/// Tendermint family, so the legacy `orderbook` failed for such a pair. It now renders the
/// bech32 account address, as the mmrpc 2.0 path does.
#[test]
fn test_legacy_orderbook_tendermint_order_reloaded_only_regression() {
    let (ctx, pubkey, secret) = ctx_for_legacy_rendering_tests(&[("IRIS", "DOC")]);
    for order in make_random_orders(pubkey, &secret, "IRIS".into(), "DOC".into(), 1) {
        insert_or_update_order(&ctx, order);
    }
    flush_trie(&ctx);

    let legacy = addresses(&legacy_orderbook(&ctx, "IRIS", "DOC")["asks"]);
    let v2 = addresses(&v2_orderbook(&ctx, "IRIS", "DOC")["asks"]);
    assert_eq!(legacy.len(), 1);
    let legacy_addr = legacy[0].as_str().unwrap();
    assert!(legacy_addr.starts_with("iaa"), "unexpected address {legacy_addr}");
    assert_eq!(v2, vec![
        json::json!({"address_type": "Transparent", "address_data": legacy_addr})
    ]);
}

/// R-F6e: the legacy rendering of the shielded outcome (also reached by Sia and Lightning) is the
/// fixed string `Shielded`; a transparent outcome renders as its own string.
#[test]
fn test_orderbook_address_legacy_string_rendering() {
    assert_eq!(OrderbookAddress::Shielded.into_legacy_string(), "Shielded");
    assert_eq!(
        OrderbookAddress::Transparent("RAddr".to_owned()).into_legacy_string(),
        "RAddr"
    );

    let (ctx, pubkey, _secret) = make_ctx_for_tests();
    let sia_conf = json::json!({ "protocol": { "type": "SIA" } });
    let sia = orderbook_address(&ctx, "SC", &sia_conf, &pubkey, UtxoAddressFormat::Standard).unwrap();
    assert_eq!(sia.into_legacy_string(), "Shielded");

    let fee_and_confs = json::json!({"default_fee_per_kb": 1000, "n_blocks": 1});
    let lightning_conf = json::json!({"protocol": {"type": "LIGHTNING", "protocol_data": {
        "platform": "BTC",
        "network": "mainnet",
        "confirmations": {"background": fee_and_confs, "normal": fee_and_confs, "high_priority": fee_and_confs},
    }}});
    let lightning = orderbook_address(
        &ctx,
        "BTC-lightning",
        &lightning_conf,
        &pubkey,
        UtxoAddressFormat::Standard,
    )
    .unwrap();
    assert_eq!(lightning.into_legacy_string(), "Shielded");
}

fn pubkey_state_with_keep_alive(last_keep_alive: u64) -> OrderbookPubkeyState {
    let mut state = OrderbookPubkeyState::with_history_timeout(Duration::from_secs(60));
    state.last_keep_alive = last_keep_alive;
    state
}

/// CRD ch.59 R59.6.4/R59.6.5: the sweep keeps the persistent key and own per-order keys, evicts stale foreign keys.
#[test]
fn test_stale_pubkeys_exempts_own_persistent_and_per_order_keys() {
    const WINDOW: u64 = 90;
    let now = 10_000;
    let stale_at = now - WINDOW - 1;
    let uuid = Uuid::new_v4();

    let mut stale_foreign = pubkey_state_with_keep_alive(stale_at);
    stale_foreign
        .orders_uuids
        .insert((uuid, alb_ordered_pair("BASE", "REL")));

    let mut states = HashMap::new();
    states.insert("foreign_stale".to_owned(), stale_foreign);
    states.insert("persistent".to_owned(), pubkey_state_with_keep_alive(stale_at));
    states.insert("per_order".to_owned(), pubkey_state_with_keep_alive(stale_at));
    states.insert(
        "foreign_fresh".to_owned(),
        pubkey_state_with_keep_alive(now - WINDOW + 1),
    );

    // (a) the same stale state is evicted for a foreign key and kept for an own per-order key.
    // (b) the persistent key stays exempt; (c) a foreign key within the window stays.
    let own_per_order: HashSet<String> = HashSet::from_iter(["per_order".to_owned()]);
    let (uuids, pubkeys) = stale_pubkeys(&states, "persistent", &own_per_order, WINDOW, now);
    assert_eq!(pubkeys, vec!["foreign_stale".to_owned()]);
    assert_eq!(uuids, vec![uuid]);

    // Without the per-order exemption the per-order key would have been evicted too.
    let (_, mut pubkeys) = stale_pubkeys(&states, "persistent", &HashSet::new(), WINDOW, now);
    pubkeys.sort();
    assert_eq!(pubkeys, vec!["foreign_stale".to_owned(), "per_order".to_owned()]);

    // The boundary instant (exactly one window old) is already stale.
    let (_, pubkeys) = stale_pubkeys(&states, "persistent", &own_per_order, WINDOW, stale_at + WINDOW);
    assert!(pubkeys.contains(&"foreign_stale".to_owned()));
}

/// An own shielded-pair maker order made under a per-order key survives the sweep, and the key's
/// stale state becomes sweepable again once the order is cancelled.
#[test]
fn test_stale_pubkeys_keeps_real_per_order_state_until_order_cancelled() {
    let (ctx, persistent_pubkey, secret) = make_ctx_for_tests();
    let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
    let p2p_key = SerializableSecp256k1Keypair::random();
    let order_pubkey = hex::encode(p2p_key.public_slice());

    let item = make_random_orders(order_pubkey.clone(), &secret, "ZBASE".into(), "REL".into(), 1).remove(0);
    let uuid = item.uuid;
    let created_at = now_ms() / 1000;
    let maker = MakerOrder {
        base: "ZBASE".into(),
        rel: "REL".into(),
        created_at: now_ms(),
        updated_at: Some(now_ms()),
        max_base_vol: 1.into(),
        min_base_vol: 0.into(),
        price: 1.into(),
        matches: HashMap::new(),
        started_swaps: Vec::new(),
        uuid,
        conf_settings: None,
        changes_history: None,
        save_in_history: false,
        base_orderbook_ticker: None,
        rel_orderbook_ticker: None,
        p2p_privkey: Some(p2p_key),
        timeout_in_minutes: None,
        swap_version: SwapVersion::default(),
    };
    insert_or_update_my_order(&ctx, item, &maker);
    ordermatch_ctx.wait_trie_ops_flushed();

    let sweep = |now: u64| {
        let my_p2p_pubkeys = ordermatch_ctx.orderbook.lock().my_p2p_pubkeys.clone();
        let trie_store = ordermatch_ctx.trie_store.lock();
        stale_pubkeys(
            &trie_store.pubkeys_state,
            &persistent_pubkey,
            &my_p2p_pubkeys,
            MAKER_ORDER_TIMEOUT,
            now,
        )
    };

    // (d) 91 s after creation the own per-order key is not selected; order and state are intact.
    let (uuids, pubkeys) = sweep(created_at + MAKER_ORDER_TIMEOUT + 1);
    assert!(uuids.is_empty() && pubkeys.is_empty());
    assert!(ordermatch_ctx.orderbook.lock().order_set.contains_key(&uuid));
    assert!(ordermatch_ctx
        .trie_store
        .lock()
        .pubkeys_state
        .contains_key(&order_pubkey));

    // (e) after cancellation the key leaves `my_p2p_pubkeys`, so its stale state is selected: nothing leaks.
    delete_my_order(&ctx, uuid, maker.p2p_privkey.as_ref());
    ordermatch_ctx.wait_trie_ops_flushed();
    let (_, pubkeys) = sweep(created_at + MAKER_ORDER_TIMEOUT + 1);
    assert_eq!(pubkeys, vec![order_pubkey]);
}

// ---- Saved orders with a Tendermint coin are cancelled at startup while Tendermint swaps are off ----

#[cfg(not(target_arch = "wasm32"))]
mod tendermint_saved_orders {
    use super::*;
    use crate::mm2::lp_ordermatch::ordermatch_trading::tendermint_order_cancel_message;

    fn conf() -> Json {
        json::json!([
            {"coin":"DOC","asset":"DOC","mm2":1,"protocol":{"type":"UTXO"}},
            {"coin":"MARTY","asset":"MARTY","mm2":1,"protocol":{"type":"UTXO"}},
            {"coin":"IRIS","asset":"IRIS","mm2":1,"protocol":{"type":"TENDERMINT","protocol_data":{
                "decimals":6,"denom":"uiris","account_prefix":"iaa","chain_id":"irishub-1"}}},
            {"coin":"IRIS-ATOM","asset":"IRIS-ATOM","mm2":1,"protocol":{"type":"TENDERMINTTOKEN","protocol_data":{
                "platform":"IRIS","decimals":6,"denom":"ibc/ATOM"}}},
        ])
    }

    fn maker(base: &str, rel: &str) -> MakerOrder {
        MakerOrder {
            base: base.into(),
            rel: rel.into(),
            created_at: now_ms(),
            updated_at: Some(now_ms()),
            max_base_vol: 10.into(),
            min_base_vol: 0.into(),
            price: 1.into(),
            matches: HashMap::new(),
            started_swaps: Vec::new(),
            uuid: Uuid::new_v4(),
            conf_settings: None,
            changes_history: None,
            save_in_history: false,
            base_orderbook_ticker: None,
            rel_orderbook_ticker: None,
            p2p_privkey: None,
            timeout_in_minutes: None,
            swap_version: SwapVersion::default(),
        }
    }

    fn taker(base: &str, rel: &str) -> TakerOrder {
        TakerOrder {
            request: TakerRequest {
                base: base.into(),
                rel: rel.into(),
                uuid: Uuid::new_v4(),
                dest_pub_key: H256Json::default(),
                sender_pubkey: H256Json::default(),
                base_amount: 10.into(),
                rel_amount: 10.into(),
                action: TakerAction::Buy,
                match_by: MatchBy::Any,
                conf_settings: None,
                base_protocol_info: None,
                rel_protocol_info: None,
                swap_version: SwapVersion::default(),
            },
            matches: HashMap::new(),
            created_at: now_ms(),
            order_type: OrderType::GoodTillCancelled,
            min_volume: 0.into(),
            timeout: 30,
            save_in_history: false,
            base_orderbook_ticker: None,
            rel_orderbook_ticker: None,
            p2p_privkey: None,
        }
    }

    #[test]
    fn saved_orders_with_a_tendermint_coin_are_cancelled_at_startup() {
        let dbdir = std::env::temp_dir().join(format!("kdf-tendermint-saved-orders-{}", Uuid::new_v4()));
        let ctx = MmCtxBuilder::new()
            .with_conf(json::json!({ "coins": conf(), "dbdir": dbdir.to_str().unwrap() }))
            .into_mm_arc();
        std::fs::create_dir_all(my_maker_orders_dir(&ctx)).unwrap();
        std::fs::create_dir_all(my_taker_orders_dir(&ctx)).unwrap();

        let tendermint_base = maker("IRIS", "DOC");
        let tendermint_token_rel = maker("DOC", "IRIS-ATOM");
        let plain = maker("DOC", "MARTY");
        let tendermint_taker = taker("MARTY", "IRIS");
        let plain_taker = taker("DOC", "MARTY");
        for order in [&tendermint_base, &tendermint_token_rel, &plain] {
            block_on(save_my_new_maker_order(ctx.clone(), order)).unwrap();
        }
        for order in [&tendermint_taker, &plain_taker] {
            block_on(save_my_new_taker_order(ctx.clone(), order)).unwrap();
        }

        let coins = block_on(orders_kick_start(&ctx)).unwrap();

        // The coins needed for the kick-start come from the kept orders alone.
        assert_eq!(
            coins,
            ["DOC", "MARTY"].iter().map(|c| c.to_string()).collect::<HashSet<_>>()
        );
        let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
        let loaded = ordermatch_ctx.maker_orders_ctx.lock().clone_orders();
        assert_eq!(loaded.keys().copied().collect::<Vec<_>>(), vec![plain.uuid]);
        let taker_orders = block_on(ordermatch_ctx.my_taker_orders.lock());
        assert_eq!(taker_orders.keys().copied().collect::<Vec<_>>(), vec![
            plain_taker.request.uuid
        ]);

        // Nothing is left in the storage: only the kept orders are saved.
        let storage = MyOrdersStorage::new(ctx.clone());
        let saved: Vec<_> = block_on(storage.load_active_maker_orders())
            .unwrap()
            .into_iter()
            .map(|o| o.uuid)
            .collect();
        assert_eq!(saved, vec![plain.uuid]);
        let saved: Vec<_> = block_on(storage.load_active_taker_orders())
            .unwrap()
            .into_iter()
            .map(|o| o.request.uuid)
            .collect();
        assert_eq!(saved, vec![plain_taker.request.uuid]);

        // The log line names the order and the pair; a kept order produces none.
        assert_eq!(
            tendermint_order_cancel_message(&ctx, "maker", &tendermint_base.uuid, "IRIS", "DOC").unwrap(),
            format!(
                "Cancelled saved maker order {} (IRIS/DOC): Tendermint swaps are not available in this build",
                tendermint_base.uuid
            )
        );
        assert_eq!(
            tendermint_order_cancel_message(&ctx, "maker", &tendermint_token_rel.uuid, "DOC", "IRIS-ATOM").unwrap(),
            format!(
                "Cancelled saved maker order {} (DOC/IRIS-ATOM): Tendermint swaps are not available in this build",
                tendermint_token_rel.uuid
            )
        );
        assert!(tendermint_order_cancel_message(&ctx, "maker", &plain.uuid, "DOC", "MARTY").is_none());

        std::fs::remove_dir_all(&dbdir).ok();
    }
}

// CRD ch.32 R-T3b tests: queue order under concurrency (T-T4) and reads that never run ahead of
// the trie (T-T3).

/// An `OrdermatchContext` without a running trie worker; the test drains the queue by hand.
fn ordermatch_ctx_without_worker() -> (Arc<OrdermatchContext>, mpsc::UnboundedReceiver<Vec<TrieOp>>) {
    let (trie_ops_tx, rx) = mpsc::unbounded::<Vec<TrieOp>>();
    let ctx = OrdermatchContext {
        maker_orders_ctx: Default::default(),
        my_taker_orders: Default::default(),
        orderbook: Default::default(),
        trie_store: Arc::new(parking_lot::Mutex::new(TrieStore::default())),
        trie_ops_tx,
        orderbook_subscriptions: Default::default(),
        pending_maker_reserved: Default::default(),
        orderbook_tickers: Default::default(),
        original_tickers: Default::default(),
    };
    (Arc::new(ctx), rx)
}

/// Do what the worker does for every queued batch.
fn drain_trie_queue(ctx: &OrdermatchContext, rx: &mut mpsc::UnboundedReceiver<Vec<TrieOp>>) {
    while let Ok(ops) = rx.try_recv() {
        ctx.trie_store.lock().apply_batch(ops);
    }
}

/// An index insert/update with its trie events queued under the guard, as production does.
fn index_insert_and_queue(ctx: &OrdermatchContext, item: OrderbookItem) {
    let mut orderbook = ctx.orderbook.lock();
    let ops = orderbook.index_insert_or_update(item);
    orderbook.queue_trie_ops(&ctx.trie_ops_tx, ops);
}

fn index_remove_and_queue(ctx: &OrdermatchContext, uuid: Uuid) {
    let mut orderbook = ctx.orderbook.lock();
    if let Some((_item, op)) = orderbook.index_remove(uuid) {
        orderbook.queue_trie_ops(&ctx.trie_ops_tx, vec![op]);
    }
}

const SHORT_WAIT_MS: u64 = 10;
const SYNC_NO_WAIT_LIMIT_MS: u64 = 5 * TRIE_CATCH_UP_BOUND_MS / 25;

fn sync_answer(
    ctx: &MmArc,
    pubkey: &str,
    pair: &str,
    root: H64,
) -> Result<Option<SyncPubkeyOrderbookStateRes>, String> {
    process_sync_pubkey_orderbook_state(
        ctx.clone(),
        pubkey.to_owned(),
        HashMap::from_iter(iter::once((pair.to_owned(), root))),
    )
}

/// What a sync answer says about `uuid`: `Some(Some(price))` listed, `Some(None)` listed as removed,
/// `None` not mentioned.
fn sync_answer_order(res: &mut SyncPubkeyOrderbookStateRes, pair: &str, uuid: Uuid) -> Option<Option<BigRational>> {
    match res.pair_orders_diff.remove(pair).expect("pair in answer") {
        DeltaOrFullTrie::Delta(delta) => delta
            .into_iter()
            .find(|(u, _)| *u == uuid)
            .map(|(_, o)| o.map(|o| o.price)),
        DeltaOrFullTrie::FullTrie(full) => full.into_iter().find(|(u, _)| *u == uuid).map(|(_, o)| Some(o.price)),
    }
}

#[test]
fn test_reads_never_run_ahead_of_trie_cancel() {
    let (ctx, pubkey, secret) = make_ctx_for_tests();
    let (octx, mut rx) = ordermatch_ctx_without_worker();
    let octx2 = octx.clone();
    OrdermatchContext::from_ctx.mock_safe(move |_| MockResult::Return(Ok(octx2.clone())));
    let pair = alb_ordered_pair("C1", "C2");

    let mut orders = make_random_orders(pubkey.clone(), &secret, "C1".into(), "C2".into(), 3);
    let cancelled = orders[0].uuid;
    let third = orders.pop().unwrap();
    for o in orders {
        index_insert_and_queue(&octx, o);
    }
    drain_trie_queue(&octx, &mut rx);
    let older_root = pair_trie_root_by_pub_ctx(&octx, &pubkey, &pair);
    index_insert_and_queue(&octx, third);
    drain_trie_queue(&octx, &mut rx);
    let old_root = pair_trie_root_by_pub_ctx(&octx, &pubkey, &pair);
    assert_ne!(older_root, old_root);

    // Consistent: nothing withheld.
    let payload = block_on(keep_alive_payload_for_pub(&octx, &pubkey, SHORT_WAIT_MS));
    assert_eq!(payload.unwrap().unwrap().0.get(&pair), Some(&old_root));
    assert!(sync_answer(&ctx, &pubkey, &pair, old_root).is_ok());

    // Cancel in the index while the worker is held back.
    index_remove_and_queue(&octx, cancelled);
    assert!(block_on(keep_alive_payload_for_pub(&octx, &pubkey, SHORT_WAIT_MS)).is_err());
    for root in [old_root, older_root] {
        let started = std::time::Instant::now();
        let err = sync_answer(&ctx, &pubkey, &pair, root).expect_err("must be the retryable error");
        assert!(err.contains("retry"), "{}", err);
        // The inbound path never waits for the worker (R-T3b item 3).
        assert!(started.elapsed() < Duration::from_millis(SYNC_NO_WAIT_LIMIT_MS));
    }

    // The worker catches up.
    drain_trie_queue(&octx, &mut rx);
    let (roots, _topics) = block_on(keep_alive_payload_for_pub(&octx, &pubkey, SHORT_WAIT_MS))
        .unwrap()
        .unwrap();
    let new_root = roots[&pair];
    assert_ne!(new_root, old_root);
    for root in [old_root, older_root] {
        let mut res = sync_answer(&ctx, &pubkey, &pair, root).unwrap().unwrap();
        assert_eq!(sync_answer_order(&mut res, &pair, cancelled), Some(None));
    }
}

#[test]
fn test_reads_never_run_ahead_of_trie_price_change() {
    let (ctx, pubkey, secret) = make_ctx_for_tests();
    let (octx, mut rx) = ordermatch_ctx_without_worker();
    let octx2 = octx.clone();
    OrdermatchContext::from_ctx.mock_safe(move |_| MockResult::Return(Ok(octx2.clone())));
    let pair = alb_ordered_pair("C1", "C2");

    let orders = make_random_orders(pubkey.clone(), &secret, "C1".into(), "C2".into(), 2);
    let mut changed = orders[0].clone();
    for o in orders {
        index_insert_and_queue(&octx, o);
    }
    drain_trie_queue(&octx, &mut rx);
    let old_root = pair_trie_root_by_pub_ctx(&octx, &pubkey, &pair);

    changed.price = &changed.price + BigRational::from_integer(7.into());
    let new_price = changed.price.clone();
    index_insert_and_queue(&octx, changed.clone());
    assert!(block_on(keep_alive_payload_for_pub(&octx, &pubkey, SHORT_WAIT_MS)).is_err());
    assert!(sync_answer(&ctx, &pubkey, &pair, old_root)
        .expect_err("retryable")
        .contains("retry"));

    drain_trie_queue(&octx, &mut rx);
    let (roots, _) = block_on(keep_alive_payload_for_pub(&octx, &pubkey, SHORT_WAIT_MS))
        .unwrap()
        .unwrap();
    assert_ne!(roots[&pair], old_root);
    let mut res = sync_answer(&ctx, &pubkey, &pair, old_root).unwrap().unwrap();
    assert_eq!(sync_answer_order(&mut res, &pair, changed.uuid), Some(Some(new_price)));
}

#[test]
fn test_reads_not_withheld_with_running_worker() {
    let (ctx, pubkey, secret) = make_ctx_for_tests();
    let octx = OrdermatchContext::from_ctx(&ctx).unwrap();
    let pair = alb_ordered_pair("C1", "C2");
    let orders = make_random_orders(pubkey.clone(), &secret, "C1".into(), "C2".into(), 50);
    let first = orders[0].uuid;
    for o in orders {
        insert_or_update_order(&ctx, o);
    }
    delete_my_order(&ctx, first, None);
    // No flush: the keep-alive producer waits for the worker within the normal bound; the inbound
    // sync answer never waits, so it is asked after that.
    let (roots, _) = block_on(keep_alive_payload_for_pub(&octx, &pubkey, TRIE_CATCH_UP_BOUND_MS))
        .unwrap()
        .unwrap();
    assert!(roots.contains_key(&pair));
    let answer = process_sync_pubkey_orderbook_state(
        ctx.clone(),
        pubkey.clone(),
        HashMap::from_iter(iter::once((pair.clone(), H64::default()))),
    );
    assert!(answer.unwrap().is_some());
}

fn pair_trie_root_by_pub_ctx(octx: &OrdermatchContext, pubkey: &str, pair: &str) -> H64 {
    *octx
        .trie_store
        .lock()
        .pubkeys_state
        .get(pubkey)
        .unwrap()
        .trie_roots
        .get(pair)
        .unwrap()
}

/// The value the trie holds for `uuid` under (pubkey, pair), if any.
fn trie_value_of(octx: &OrdermatchContext, pubkey: &str, pair: &str, uuid: &Uuid) -> Option<Vec<u8>> {
    let store = octx.trie_store.lock();
    let root = store.pubkeys_state.get(pubkey)?.trie_roots.get(pair)?;
    if *root == H64::default() {
        return None;
    }
    let trie = TrieDB::<Layout>::new(&store.memory_db, root).unwrap();
    trie.get(uuid.as_bytes()).unwrap()
}

#[test]
fn test_trie_queue_order_under_concurrency() {
    const ROUNDS: usize = 300;
    let (ctx, pubkey, secret) = make_ctx_for_tests();
    let octx = OrdermatchContext::from_ctx(&ctx).unwrap();
    let pair = alb_ordered_pair("C1", "C2");
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let (round_tx, round_rx) = std::sync::mpsc::channel::<Option<OrderbookItem>>();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();

    // Thread A updates the price of the order each round, the main thread cancels it concurrently.
    let updater = {
        let (ctx, barrier) = (ctx.clone(), barrier.clone());
        std::thread::spawn(move || {
            while let Ok(Some(item)) = round_rx.recv() {
                barrier.wait();
                insert_or_update_order(&ctx, item);
                done_tx.send(()).unwrap();
            }
        })
    };

    for round in 0..ROUNDS {
        let mut item = make_random_orders(pubkey.clone(), &secret, "C1".into(), "C2".into(), 1).remove(0);
        insert_or_update_order(&ctx, item.clone());
        item.price = &item.price + BigRational::from_integer((round as u64 + 1).into());
        round_tx.send(Some(item.clone())).unwrap();
        barrier.wait();
        delete_my_order(&ctx, item.uuid, None);
        done_rx.recv().unwrap();
        octx.wait_trie_ops_flushed();

        let in_index = octx.orderbook.lock().order_set.get(&item.uuid).cloned();
        let in_trie = trie_value_of(&octx, &pubkey, &pair, &item.uuid);
        assert_eq!(
            in_trie,
            in_index.map(|o| o.trie_state_bytes()),
            "round {}: trie and read index disagree",
            round
        );
        // tidy up so rounds stay independent
        delete_my_order(&ctx, item.uuid, None);
    }
    round_tx.send(None).unwrap();
    updater.join().unwrap();
}

// CRD ch.59 R59.6.7 / T10: the re-announcement of an own maker order absent from the local orderbook
// records the pair subscription, subscribes on the mesh only without a prior record, and requests nothing.
mod reannounce_missing_own_order_tests {
    use super::*;

    fn own_order() -> MakerOrder {
        MakerOrder {
            uuid: Uuid::new_v4(),
            base: "DOC".into(),
            rel: "MARTY".into(),
            created_at: now_ms(),
            updated_at: Some(now_ms()),
            matches: HashMap::new(),
            max_base_vol: 10.into(),
            min_base_vol: 1.into(),
            price: 1.into(),
            started_swaps: vec![],
            conf_settings: Some(OrderConfirmationsSettings::default()),
            changes_history: None,
            save_in_history: false,
            base_orderbook_ticker: None,
            rel_orderbook_ticker: None,
            p2p_privkey: None,
            timeout_in_minutes: None,
            swap_version: SwapVersion::default(),
        }
    }

    fn ctx_with_channel() -> (MmArc, mpsc::Receiver<AdexBehaviourCmd>) {
        let (ctx, ..) = make_ctx_for_tests();
        let (tx, rx) = mpsc::channel(10);
        P2PContext::new(tx).store_to_mm_arc(&ctx);
        (ctx, rx)
    }

    fn drain(rx: &mut mpsc::Receiver<AdexBehaviourCmd>) -> Vec<AdexBehaviourCmd> {
        let mut cmds = Vec::new();
        while let Ok(cmd) = rx.try_recv() {
            cmds.push(cmd);
        }
        cmds
    }

    #[test]
    fn test_reannounce_records_subscription_without_request() {
        let (ctx, mut rx) = ctx_with_channel();
        let order = own_order();
        let topic = order.orderbook_topic();

        reannounce_missing_own_order(&ctx, &order, vec![], vec![]);

        let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
        match ordermatch_ctx.orderbook_subscriptions.read().get(&topic) {
            Some(OrderbookRequestingState::NotRequested { .. }) => (),
            other => panic!("NotRequested record expected, got {:?}", other),
        }

        let cmds = drain(&mut rx);
        assert_eq!(cmds.len(), 2, "Subscribe and PublishMsg expected only");
        match &cmds[0] {
            AdexBehaviourCmd::Subscribe { topic: t } => assert_eq!(*t, topic),
            _ => panic!("Subscribe must be sent first"),
        }
        match &cmds[1] {
            AdexBehaviourCmd::PublishMsg { topics, .. } => assert_eq!(topics, &vec![topic]),
            _ => panic!("PublishMsg must follow the Subscribe"),
        }
    }

    #[test]
    fn test_reannounce_keeps_existing_requested_record() {
        let (ctx, mut rx) = ctx_with_channel();
        let order = own_order();
        let topic = order.orderbook_topic();
        let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
        ordermatch_ctx
            .orderbook_subscriptions
            .write()
            .insert(topic.clone(), OrderbookRequestingState::Requested);

        reannounce_missing_own_order(&ctx, &order, vec![], vec![]);

        match ordermatch_ctx.orderbook_subscriptions.read().get(&topic) {
            Some(OrderbookRequestingState::Requested) => (),
            other => panic!("Requested record must be kept, got {:?}", other),
        }
        let cmds = drain(&mut rx);
        assert_eq!(cmds.len(), 1, "no second Subscribe, only the notification");
        assert!(matches!(cmds[0], AdexBehaviourCmd::PublishMsg { .. }));
    }

    #[test]
    fn test_reannounce_keeps_existing_not_requested_timestamp() {
        let (ctx, mut rx) = ctx_with_channel();
        let order = own_order();
        let topic = order.orderbook_topic();
        let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
        ordermatch_ctx
            .orderbook_subscriptions
            .write()
            .insert(topic.clone(), OrderbookRequestingState::NotRequested {
                subscribed_at: 7,
            });

        reannounce_missing_own_order(&ctx, &order, vec![], vec![]);

        match ordermatch_ctx.orderbook_subscriptions.read().get(&topic) {
            Some(OrderbookRequestingState::NotRequested { subscribed_at: 7 }) => (),
            other => panic!("record must be unchanged, got {:?}", other),
        }
        assert_eq!(drain(&mut rx).len(), 1);
    }

    fn keep_alive_for(pair: &str) -> PubkeyKeepAlive {
        let mut trie_roots = HashMap::new();
        trie_roots.insert(pair.to_owned(), [1; 8]);
        PubkeyKeepAlive {
            trie_roots,
            timestamp: now_ms() / 1000,
        }
    }

    fn keep_alive_request(ctx: &MmArc, pair: &str, i_am_relay: bool) -> Option<OrdermatchRequest> {
        let ordermatch_ctx = OrdermatchContext::from_ctx(ctx).unwrap();
        let subscribed: HashSet<String> = ordermatch_ctx.orderbook_subscriptions.read().keys().cloned().collect();
        let mut trie_store = ordermatch_ctx.trie_store.lock();
        trie_store.prepare_sync_request_for_keep_alive("other-pubkey", keep_alive_for(pair), i_am_relay, |t| {
            subscribed.contains(t)
        })
    }

    #[test]
    fn test_kickstart_keep_alive_requests_sync_on_non_relay_node() {
        let (ctx, _rx) = ctx_with_channel();
        let own_pair = alb_ordered_pair("DOC", "MARTY");
        let other_pair = alb_ordered_pair("C1", "C2");

        // Before any re-announcement a non-relay node does not request, a relay node does.
        assert!(keep_alive_request(&ctx, &own_pair, false).is_none());

        reannounce_missing_own_order(&ctx, &own_order(), vec![], vec![]);

        match keep_alive_request(&ctx, &own_pair, false) {
            Some(OrdermatchRequest::SyncPubkeyOrderbookState { pubkey, trie_roots }) => {
                assert_eq!(pubkey, "other-pubkey");
                assert_eq!(trie_roots.len(), 1);
                assert!(trie_roots.contains_key(&own_pair));
            },
            other => panic!("one SyncPubkeyOrderbookState expected, got {:?}", other),
        }
        // A pair with no own order and no record sends nothing on a non-relay node.
        assert!(keep_alive_request(&ctx, &other_pair, false).is_none());
        // A relay node requests in both cases.
        assert!(keep_alive_request(&ctx, &own_pair, true).is_some());
        assert!(keep_alive_request(&ctx, &other_pair, true).is_some());
    }
}
