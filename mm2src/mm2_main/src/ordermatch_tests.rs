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
use mm2_p2p::{decode_message, decode_signed, PeerId};
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
    orderbook.recently_cancelled.insert((uuid, pubkey.clone()), ());

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

    orderbook.recently_cancelled.insert((uuid, pubkey.clone()), ());
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
    orderbook.recently_cancelled.insert((uuid, pubkey_a), ());

    // Insert from pubkey_b should succeed
    orderbook.index_insert_or_update(order);
    assert!(
        orderbook.order_set.contains_key(&uuid),
        "order from a different pubkey should be inserted"
    );
}

/// CRD ch.32 R-K1b, T-K8: a cancel this node received is final for the final-cancel window.
mod t32_k1b_final_cancel_tests {
    use super::sync_answer_deferral_tests::assert_tables_empty;
    use super::*;
    use crate::mm2::lp_ordermatch::ordermatch_orderbook::{cancel_final_max_entries, cancel_final_window_secs};
    use crate::mm2::lp_ordermatch::sync_log::{AltReason, Attempt, RequestClass};
    use mm2_p2p::atomicdex_behaviour::AdexResponse;
    use std::time::Instant;

    const PAIR: &str = "C1:C2";
    /// Older than the 120 s of R-K1 and younger than any default window.
    const OLD: u64 = 200;
    const FORWARDER: &str = "forwarding-peer";

    fn node(conf: Json) -> (MmArc, mpsc::Receiver<AdexBehaviourCmd>) {
        let ctx = MmCtxBuilder::new().with_conf(conf).into_mm_arc();
        ctx.init_metrics().unwrap();
        let (tx, rx) = mpsc::channel(1024);
        P2PContext::new(tx).store_to_mm_arc(&ctx);
        (ctx, rx)
    }

    fn next_cmd(rx: &mut mpsc::Receiver<AdexBehaviourCmd>) -> AdexBehaviourCmd {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match rx.try_recv() {
                Ok(cmd) => return cmd,
                Err(_) => {
                    assert!(Instant::now() < deadline, "no command was sent");
                    std::thread::sleep(Duration::from_millis(1));
                },
            }
        }
    }

    fn octx(ctx: &MmArc) -> Arc<OrdermatchContext> { OrdermatchContext::from_ctx(ctx).unwrap() }

    fn conf_ctx(conf: Json) -> MmArc { MmCtxBuilder::new().with_conf(conf).into_mm_arc() }

    fn created(secret: &[u8; 32], order: &OrderbookItem) -> Vec<u8> {
        let msg = new_protocol::MakerOrderCreated {
            uuid: order.uuid.into(),
            base: order.base.clone(),
            rel: order.rel.clone(),
            price: order.price.clone(),
            max_volume: order.max_volume.clone(),
            min_volume: order.min_volume.clone(),
            conf_settings: OrderConfirmationsSettings::default(),
            created_at: now_ms() / 1000,
            timestamp: now_ms() / 1000,
            pair_trie_root: H64::default(),
            base_protocol_info: vec![],
            rel_protocol_info: vec![],
        };
        encode_and_sign(&new_protocol::OrdermatchMessage::MakerOrderCreated(msg), secret).unwrap()
    }

    fn cancelled(secret: &[u8; 32], uuid: Uuid) -> Vec<u8> {
        let msg = new_protocol::MakerOrderCancelled {
            uuid: uuid.into(),
            timestamp: now_ms() / 1000,
            pair_trie_root: H64::default(),
        };
        encode_and_sign(&new_protocol::OrdermatchMessage::MakerOrderCancelled(msg), secret).unwrap()
    }

    fn receive(ctx: &MmArc, msg: &[u8]) -> bool {
        let accepted = block_on(process_msg(ctx.clone(), vec![], "peer".to_owned(), msg, false));
        flush_trie(ctx);
        accepted
    }

    fn book(ctx: &MmArc) -> HashSet<Uuid> { octx(ctx).orderbook.lock().order_set.keys().copied().collect() }

    /// The guard entry of `uuid`, as if its cancel had arrived `secs` ago.
    fn set_guard_age(ctx: &MmArc, uuid: Uuid, pubkey: &str, secs: u64) {
        let octx = octx(ctx);
        let mut orderbook = octx.orderbook.lock();
        orderbook.recently_cancelled.remove((uuid, pubkey.to_owned()));
        assert!(orderbook
            .recently_cancelled
            .insert_aged((uuid, pubkey.to_owned()), (), Duration::from_secs(secs)));
    }

    fn guarded(ctx: &MmArc, uuid: Uuid, pubkey: &str) -> bool {
        octx(ctx)
            .orderbook
            .lock()
            .recently_cancelled
            .get_unexpired(&(uuid, pubkey.to_owned()))
            .is_some()
    }

    fn ttl(ctx: &MmArc) -> Duration { octx(ctx).orderbook.lock().recently_cancelled.ttl() }

    /// What the node's own synchronization answer and order-book answer list for the maker.
    fn own_answers(ctx: &MmArc, pubkey: &str) -> (HashSet<Uuid>, HashSet<Uuid>) {
        flush_trie(ctx);
        let roots = HashMap::from([(PAIR.to_owned(), H64::default())]);
        let mut sync = process_sync_pubkey_orderbook_state(ctx.clone(), pubkey.to_owned(), roots)
            .unwrap()
            .unwrap();
        let in_sync = match sync.pair_orders_diff.remove(PAIR).expect("the pair is answered") {
            DeltaOrFullTrie::FullTrie(orders) => orders.into_iter().map(|(uuid, _)| uuid).collect(),
            DeltaOrFullTrie::Delta(_) => panic!("expected a full snapshot"),
        };
        let encoded = process_get_orderbook_request(ctx.clone(), "C1".into(), "C2".into())
            .unwrap()
            .unwrap();
        let mut orderbook = decode_message::<GetOrderbookRes>(&encoded).unwrap();
        let in_orderbook = orderbook
            .pubkey_orders
            .remove(pubkey)
            .map(|item| item.orders.into_iter().map(|(uuid, _)| uuid).collect())
            .unwrap_or_default();
        (in_sync, in_orderbook)
    }

    /// Three orders of one maker; `cancelled` is the one whose guard entry the node holds.
    struct Maker {
        pubkey: String,
        secret: [u8; 32],
        cancelled: OrderbookItem,
        others: Vec<OrderbookItem>,
    }

    impl Maker {
        fn new(name: &str) -> Maker {
            let (pubkey, secret) = pubkey_and_secret_for_test(name);
            let mut orders = make_random_orders(pubkey.clone(), &secret, "C1".into(), "C2".into(), 3);
            let cancelled = orders.remove(0);
            Maker {
                pubkey,
                secret,
                cancelled,
                others: orders,
            }
        }

        fn others(&self) -> HashSet<Uuid> { self.others.iter().map(|o| o.uuid).collect() }

        fn all(&self) -> Vec<OrderbookItem> {
            let mut all = vec![self.cancelled.clone()];
            all.extend(self.others.clone());
            all
        }
    }

    // (a)

    /// Whether a created message for the cancelled order is inserted `age_secs` after the cancel.
    fn created_after(conf: Json, age_secs: u64) -> bool {
        let (ctx, _rx) = node(conf);
        let maker = Maker::new("final-cancel-a");
        let order = &maker.cancelled;
        assert!(receive(&ctx, &cancelled(&maker.secret, order.uuid)));
        assert!(guarded(&ctx, order.uuid, &maker.pubkey), "the cancel is recorded");
        set_guard_age(&ctx, order.uuid, &maker.pubkey, age_secs);
        receive(&ctx, &created(&maker.secret, order));
        book(&ctx).contains(&order.uuid)
    }

    #[test]
    fn t_k8_a_default_window() {
        assert!(!created_after(json!({}), 121), "121 s after the cancel");
        assert!(!created_after(json!({}), 3599), "3,599 s after the cancel");
        assert!(created_after(json!({}), 3601), "3,601 s after the cancel");
        let (ctx, _rx) = node(json!({}));
        assert_eq!(ttl(&ctx), Duration::from_secs(3600));
    }

    #[test]
    fn t_k8_a_window_of_120_is_the_reference_behavior() {
        let conf = json!({ "cancel_final_window_secs": 120 });
        assert!(!created_after(conf.clone(), 100), "100 s after the cancel");
        assert!(created_after(conf.clone(), 121), "121 s after the cancel");
        let (ctx, _rx) = node(conf);
        assert_eq!(ttl(&ctx), Duration::from_secs(120));
    }

    #[test]
    fn t_k8_a_the_window_comes_from_the_configuration() {
        let conf = json!({ "cancel_final_window_secs": 7200 });
        let (ctx, _rx) = node(conf.clone());
        assert_eq!(ttl(&ctx), Duration::from_secs(7200));
        assert!(!created_after(conf.clone(), 3601));
        assert!(created_after(conf, 7201));
    }

    // (b)

    /// A node whose guard holds the cancel of `maker.cancelled`, aged past the 120 s of R-K1.
    fn guarding_node(conf: Json, maker: &Maker) -> (MmArc, mpsc::Receiver<AdexBehaviourCmd>) {
        let (ctx, rx) = node(conf);
        set_guard_age(&ctx, maker.cancelled.uuid, &maker.pubkey, OLD);
        (ctx, rx)
    }

    fn assert_cancelled_kept_out(ctx: &MmArc, maker: &Maker) {
        flush_trie(ctx);
        assert_eq!(
            book(ctx),
            maker.others(),
            "the other two orders are inserted, not the cancelled one"
        );
        assert!(guarded(ctx, maker.cancelled.uuid, &maker.pubkey));
        let (in_sync, in_orderbook) = own_answers(ctx, &maker.pubkey);
        assert_eq!(in_sync, maker.others(), "the synchronization answer omits it");
        assert_eq!(in_orderbook, maker.others(), "the order-book answer omits it");
        let octx = octx(ctx);
        let orderbook = octx.orderbook.lock();
        assert!(!orderbook.order_set.contains_key(&maker.cancelled.uuid));
        assert!(orderbook
            .unordered
            .get(&("C1".to_owned(), "C2".to_owned()))
            .is_none_or(|uuids| !uuids.contains(&maker.cancelled.uuid)));
    }

    #[test]
    fn t_k8_b_order_message() {
        let maker = Maker::new("final-cancel-b-message");
        let (ctx, _rx) = guarding_node(json!({}), &maker);
        for order in maker.all() {
            receive(&ctx, &created(&maker.secret, &order));
        }
        assert_cancelled_kept_out(&ctx, &maker);
    }

    /// An update of a held order is applied as before, and the cancelled order stays out.
    #[test]
    fn t_k8_b_update_of_another_order_is_applied() {
        let maker = Maker::new("final-cancel-b-update");
        let (ctx, _rx) = guarding_node(json!({}), &maker);
        for order in maker.all() {
            receive(&ctx, &created(&maker.secret, &order));
        }
        let held = maker.others[0].clone();
        let mut msg = MakerOrderUpdated::new(held.uuid);
        msg.with_new_price(BigRational::new(3.into(), 2.into()));
        let signed = encode_and_sign(&new_protocol::OrdermatchMessage::MakerOrderUpdated(msg), &maker.secret).unwrap();
        assert!(receive(&ctx, &signed));
        assert_eq!(
            octx(&ctx).orderbook.lock().order_set[&held.uuid].price,
            BigRational::new(3.into(), 2.into())
        );
        assert_cancelled_kept_out(&ctx, &maker);
    }

    /// `process_pubkey_full_trie` and `process_trie_delta`, as the synchronization applier calls them.
    fn apply(ctx: &MmArc, maker: &Maker, diff: DeltaOrFullTrie<Uuid, OrderbookP2PItem>) {
        let octx = octx(ctx);
        let protocol_infos = HashMap::new();
        let conf_infos = HashMap::new();
        let params = ProcessTrieParams {
            pubkey: &maker.pubkey,
            alb_pair: PAIR,
            protocol_infos: &protocol_infos,
            conf_infos: &conf_infos,
        };
        let mut orderbook = octx.orderbook.lock();
        let ops = match diff {
            DeltaOrFullTrie::Delta(delta) => process_trie_delta(&mut orderbook, delta, params),
            DeltaOrFullTrie::FullTrie(values) => process_pubkey_full_trie(&mut orderbook, values, params),
        };
        orderbook.queue_trie_ops(&octx.trie_ops_tx, ops);
    }

    #[test]
    fn t_k8_b_full_snapshot_applier() {
        let maker = Maker::new("final-cancel-b-full");
        let (ctx, _rx) = guarding_node(json!({}), &maker);
        let values = maker.all().into_iter().map(|o| (o.uuid, o.into())).collect();
        apply(&ctx, &maker, DeltaOrFullTrie::FullTrie(values));
        assert_cancelled_kept_out(&ctx, &maker);
    }

    #[test]
    fn t_k8_b_delta_applier() {
        let maker = Maker::new("final-cancel-b-delta");
        let (ctx, _rx) = guarding_node(json!({}), &maker);
        let delta = maker.all().into_iter().map(|o| (o.uuid, Some(o.into()))).collect();
        apply(&ctx, &maker, DeltaOrFullTrie::Delta(delta));
        assert_cancelled_kept_out(&ctx, &maker);
    }

    /// The keep-alive of the maker `pubkey` as `from` advertises it now.
    fn keep_alive_of(from: &MmArc, pubkey: &str) -> PubkeyKeepAlive {
        let roots = octx(from)
            .trie_store
            .lock()
            .pubkeys_state
            .get(pubkey)
            .map(|s| s.trie_roots.clone())
            .unwrap_or_default();
        PubkeyKeepAlive {
            trie_roots: advertisable_roots(&roots),
            timestamp: now_ms() / 1000,
        }
    }

    /// `relay` handles the keep-alive of `maker_node`, and the request it sends is answered by `maker_node`.
    /// Returns whether the answer was a delta.
    fn sync_with(
        relay: &MmArc,
        relay_rx: &mut mpsc::Receiver<AdexBehaviourCmd>,
        maker_node: &MmArc,
        pubkey: &str,
    ) -> bool {
        let keep_alive = keep_alive_of(maker_node, pubkey);
        let relay2 = relay.clone();
        let pubkey2 = pubkey.to_owned();
        let task = std::thread::spawn(move || {
            block_on(process_orders_keep_alive(
                relay2,
                FORWARDER.to_owned(),
                pubkey2,
                keep_alive,
                true,
            ))
        });
        let (req, response_tx) = match next_cmd(relay_rx) {
            AdexBehaviourCmd::RequestPeers { req, response_tx, .. } => (req, response_tx),
            other => panic!("unexpected command {:?}", other),
        };
        let (pubkey, trie_roots) = match decode_message::<P2PRequest>(&req).unwrap() {
            P2PRequest::Ordermatch(OrdermatchRequest::SyncPubkeyOrderbookState { pubkey, trie_roots }) => {
                (pubkey, trie_roots)
            },
            other => panic!("unexpected request {:?}", other),
        };
        let answer = process_sync_pubkey_orderbook_state(maker_node.clone(), pubkey, trie_roots)
            .unwrap()
            .unwrap();
        let is_delta = answer
            .pair_orders_diff
            .values()
            .all(|diff| matches!(diff, DeltaOrFullTrie::Delta(_)));
        let res = AdexResponse::Ok {
            response: encode_message(&answer).unwrap(),
        };
        response_tx.send(vec![(PeerId::random(), res)]).unwrap();
        assert!(task.join().unwrap(), "the keep-alive was handled");
        is_delta
    }

    fn maker_node(orders: &[OrderbookItem]) -> (MmArc, mpsc::Receiver<AdexBehaviourCmd>) {
        let (maker_ctx, rx) = node(json!({}));
        for order in orders {
            insert_or_update_order(&maker_ctx, order.clone());
        }
        flush_trie(&maker_ctx);
        (maker_ctx, rx)
    }

    /// The whole keep-alive path, answered with a snapshot that lists the cancelled order.
    #[test]
    fn t_k8_b_keep_alive_full_snapshot() {
        let maker = Maker::new("final-cancel-b-ka-full");
        let (maker_ctx, _maker_rx) = maker_node(&maker.all());
        let (relay, mut relay_rx) = guarding_node(json!({}), &maker);
        assert!(
            !sync_with(&relay, &mut relay_rx, &maker_ctx, &maker.pubkey),
            "a snapshot is answered"
        );
        assert_cancelled_kept_out(&relay, &maker);
    }

    /// The whole keep-alive path, answered with a delta that lists the cancelled order.
    #[test]
    fn t_k8_b_keep_alive_delta() {
        let maker = Maker::new("final-cancel-b-ka-delta");
        let (first, second) = (maker.others[0].clone(), maker.others[1].clone());
        let (maker_ctx, _maker_rx) = maker_node(std::slice::from_ref(&first));
        let (relay, mut relay_rx) = node(json!({}));
        assert!(!sync_with(&relay, &mut relay_rx, &maker_ctx, &maker.pubkey));
        assert_eq!(book(&relay), HashSet::from([first.uuid]));

        // The maker adds two orders, one of them the cancelled one, whose cancel the relay has recorded.
        for order in [second, maker.cancelled.clone()] {
            insert_or_update_order(&maker_ctx, order);
        }
        flush_trie(&maker_ctx);
        set_guard_age(&relay, maker.cancelled.uuid, &maker.pubkey, OLD);
        assert!(
            sync_with(&relay, &mut relay_rx, &maker_ctx, &maker.pubkey),
            "a delta is answered"
        );
        assert_cancelled_kept_out(&relay, &maker);
    }

    /// The answer of the request to one other relay (R-Q5d), through the whole keep-alive path: the
    /// forwarder's request fails, the relay list is asked, and the answer of the other relay lists the
    /// cancelled order with two others.
    #[test]
    fn t_k8_b_alternate_relay_answer() {
        const OTHER_RELAY: &str = "relay-g";
        let maker = Maker::new("final-cancel-b-alternate");
        let (maker_ctx, _maker_rx) = maker_node(&maker.all());
        let (relay, mut relay_rx) = guarding_node(json!({ "sync_request_retry_ms": 0 }), &maker);

        let keep_alive = keep_alive_of(&maker_ctx, &maker.pubkey);
        let (relay2, pubkey2) = (relay.clone(), maker.pubkey.clone());
        let task = std::thread::spawn(move || {
            block_on(process_orders_keep_alive(
                relay2,
                FORWARDER.to_owned(),
                pubkey2,
                keep_alive,
                true,
            ))
        });
        // The forwarder's request ends without an answer (R-Q5d trigger (a)).
        match next_cmd(&mut relay_rx) {
            AdexBehaviourCmd::RequestPeers { peers, response_tx, .. } => {
                assert_eq!(peers, vec![FORWARDER.to_owned()]);
                drop(response_tx);
            },
            other => panic!("unexpected command {:?}", other),
        }
        assert!(!task.join().unwrap(), "the forwarder's request failed");
        match next_cmd(&mut relay_rx) {
            AdexBehaviourCmd::GetConnectedRelays { result_tx } => result_tx
                .send(vec![FORWARDER.to_owned(), OTHER_RELAY.to_owned()])
                .unwrap(),
            other => panic!("unexpected command {:?}", other),
        }
        let (req, response_tx) = match next_cmd(&mut relay_rx) {
            AdexBehaviourCmd::RequestPeers {
                req,
                peers,
                response_tx,
            } => {
                assert_eq!(peers, vec![OTHER_RELAY.to_owned()]);
                (req, response_tx)
            },
            other => panic!("unexpected command {:?}", other),
        };
        let (pubkey, trie_roots) = match decode_message::<P2PRequest>(&req).unwrap() {
            P2PRequest::Ordermatch(OrdermatchRequest::SyncPubkeyOrderbookState { pubkey, trie_roots }) => {
                (pubkey, trie_roots)
            },
            other => panic!("unexpected request {:?}", other),
        };
        let answer = process_sync_pubkey_orderbook_state(maker_ctx.clone(), pubkey, trie_roots)
            .unwrap()
            .unwrap();
        assert!(
            answer
                .pair_orders_diff
                .values()
                .all(|diff| matches!(diff, DeltaOrFullTrie::FullTrie(orders) if orders.len() == 3)),
            "the answer lists all three orders"
        );
        let res = AdexResponse::Ok {
            response: encode_message(&answer).unwrap(),
        };
        response_tx.send(vec![(PeerId::random(), res)]).unwrap();

        // The answer is applied on a task of its own; it is counted once it has been applied.
        let deadline = Instant::now() + Duration::from_secs(5);
        while octx(&relay).sync_log.snapshot().alt_ok != 1 {
            assert!(
                Instant::now() < deadline,
                "the answer of the other relay was not applied"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(octx(&relay).sync_log.requests(), vec![
            (Attempt::First, RequestClass::Transport),
            (Attempt::Alternate(AltReason::Failure), RequestClass::Ok)
        ]);
        assert_cancelled_kept_out(&relay, &maker);
        assert_tables_empty(&relay);
    }

    /// The order-book request-and-fill path.
    #[test]
    fn t_k8_b_order_book_answer() {
        let maker = Maker::new("final-cancel-b-orderbook");
        let (ctx, mut rx) = guarding_node(json!({}), &maker);
        let task_ctx = ctx.clone();
        let task = std::thread::spawn(move || block_on(request_and_fill_orderbook(&task_ctx, "C1", "C2")));
        let response_tx = match next_cmd(&mut rx) {
            AdexBehaviourCmd::RequestAnyRelay { response_tx, .. } => response_tx,
            other => panic!("unexpected command {:?}", other),
        };
        let mut conf_infos = HashMap::new();
        let item = GetOrderbookPubkeyItem {
            orders: maker
                .all()
                .into_iter()
                .map(|order| {
                    if let Some(conf_settings) = order.conf_settings {
                        conf_infos.insert(order.uuid, conf_settings);
                    }
                    (order.uuid, order.into())
                })
                .collect(),
            last_keep_alive: now_ms() / 1000,
            last_signed_pubkey_payload: vec![],
        };
        let answer = GetOrderbookRes {
            pubkey_orders: HashMap::from([(maker.pubkey.clone(), item)]),
            protocol_infos: HashMap::new(),
            conf_infos,
        };
        response_tx
            .send(Some((PeerId::random(), encode_message(&answer).unwrap())))
            .unwrap();
        task.join().unwrap().unwrap();
        assert_cancelled_kept_out(&ctx, &maker);
    }

    // (c)

    #[test]
    fn t_k8_c_same_identifier_from_another_publisher_is_inserted() {
        let maker = Maker::new("final-cancel-c-maker");
        let other = Maker::new("final-cancel-c-other");
        let (ctx, _rx) = guarding_node(json!({}), &maker);
        let mut same_uuid = other.cancelled.clone();
        same_uuid.uuid = maker.cancelled.uuid;
        assert!(receive(&ctx, &created(&other.secret, &same_uuid)));
        let octx = octx(&ctx);
        let orderbook = octx.orderbook.lock();
        assert_eq!(orderbook.order_set[&maker.cancelled.uuid].pubkey, other.pubkey);
    }

    // (d)

    fn guard_of(cap: usize) -> Orderbook {
        Orderbook {
            cancel_guard_max_entries: cap,
            recently_cancelled: TimeCache::new(Duration::from_secs(3600)),
            ..Default::default()
        }
    }

    fn held_for(orderbook: &Orderbook, uuids: &[Uuid], pubkey: &str) -> Vec<bool> {
        uuids
            .iter()
            .map(|uuid| {
                orderbook
                    .recently_cancelled
                    .get_unexpired(&(*uuid, pubkey.to_owned()))
                    .is_some()
            })
            .collect()
    }

    fn held(orderbook: &Orderbook, uuids: &[Uuid]) -> Vec<bool> { held_for(orderbook, uuids, "pk") }

    #[test]
    fn t_k8_d_cap_keeps_the_newest_of_the_older_entries() {
        let mut orderbook = guard_of(3);
        let uuids: Vec<Uuid> = (0..5).map(|_| Uuid::new_v4()).collect();
        // Oldest first: 1,000 s, 900 s, ... 600 s ago; each insertion applies the cap, as a cancel does.
        for (i, uuid) in uuids.iter().enumerate() {
            let age = Duration::from_secs(1000 - 100 * i as u64);
            assert!(orderbook
                .recently_cancelled
                .insert_aged((*uuid, "pk".to_owned()), (), age));
            orderbook.enforce_cancel_guard_cap();
        }
        assert_eq!(held(&orderbook, &uuids), vec![false, false, true, true, true]);
        assert_eq!(orderbook.recently_cancelled.len(), 3);
    }

    #[test]
    fn t_k8_d_entries_younger_than_120_s_are_never_dropped_for_the_cap() {
        let mut orderbook = guard_of(3);
        let old: Vec<Uuid> = (0..3).map(|_| Uuid::new_v4()).collect();
        for (i, uuid) in old.iter().enumerate() {
            let age = Duration::from_secs(500 - 100 * i as u64);
            orderbook
                .recently_cancelled
                .insert_aged((*uuid, "pk".to_owned()), (), age);
        }
        orderbook.enforce_cancel_guard_cap();
        assert_eq!(held(&orderbook, &old), vec![true, true, true]);

        // Three cancels, as `delete_order` records them: all guard, and only older entries are dropped.
        let fresh: Vec<Uuid> = (0..3).map(|_| Uuid::new_v4()).collect();
        for uuid in &fresh {
            orderbook.record_cancel(*uuid, "pk");
        }
        assert_eq!(held(&orderbook, &old), vec![false, false, false]);
        assert_eq!(held(&orderbook, &fresh), vec![true, true, true]);
        // A fourth one takes the guard above the cap with nothing older left to drop.
        let fourth = Uuid::new_v4();
        orderbook.record_cancel(fourth, "pk");
        assert_eq!(held(&orderbook, &fresh), vec![true, true, true]);
        assert_eq!(held(&orderbook, &[fourth]), vec![true]);
        assert_eq!(orderbook.recently_cancelled.len(), 4);
    }

    #[test]
    fn t_k8_d_cap_is_applied_by_a_cancel_message() {
        let (ctx, _rx) = node(json!({ "cancel_final_max_entries": 1 }));
        let old: Vec<Uuid> = (0..2).map(|_| Uuid::new_v4()).collect();
        {
            let octx = octx(&ctx);
            let mut orderbook = octx.orderbook.lock();
            for (i, uuid) in old.iter().enumerate() {
                let age = Duration::from_secs(500 - 100 * i as u64);
                orderbook
                    .recently_cancelled
                    .insert_aged((*uuid, "pk".to_owned()), (), age);
            }
        }
        let maker = Maker::new("final-cancel-d-message");
        assert!(receive(&ctx, &cancelled(&maker.secret, maker.cancelled.uuid)));
        let octx = octx(&ctx);
        let orderbook = octx.orderbook.lock();
        assert_eq!(held(&orderbook, &old), vec![false, false]);
        assert_eq!(held_for(&orderbook, &[maker.cancelled.uuid], &maker.pubkey), vec![true]);
        assert_eq!(orderbook.cancel_guard_max_entries, 1);
    }

    // (e)

    #[test]
    fn t_k8_e_window_reader() {
        let window = |conf: Json| cancel_final_window_secs(&conf_ctx(conf));
        assert_eq!(window(json!({})), 3600);
        assert_eq!(window(json!({ "cancel_final_window_secs": 60 })), 120);
        assert_eq!(window(json!({ "cancel_final_window_secs": 100000 })), 86400);
        assert_eq!(window(json!({ "cancel_final_window_secs": "x" })), 3600);
        assert_eq!(window(json!({ "cancel_final_window_secs": 120 })), 120);
        assert_eq!(window(json!({ "cancel_final_window_secs": 86400 })), 86400);
        assert_eq!(window(json!({ "cancel_final_window_secs": 7200 })), 7200);
        assert_eq!(window(json!({ "cancel_final_window_secs": 0 })), 120);
        assert_eq!(window(json!({ "cancel_final_window_secs": -5 })), 120);
        assert_eq!(window(json!({ "cancel_final_window_secs": 7200.5 })), 3600);
        assert_eq!(window(json!({ "cancel_final_window_secs": null })), 3600);
    }

    #[test]
    fn t_k8_e_cap_reader() {
        let cap = |conf: Json| cancel_final_max_entries(&conf_ctx(conf));
        assert_eq!(cap(json!({})), 100_000);
        assert_eq!(cap(json!({ "cancel_final_max_entries": 0 })), 100_000);
        assert_eq!(cap(json!({ "cancel_final_max_entries": "x" })), 100_000);
        assert_eq!(cap(json!({ "cancel_final_max_entries": 2_000_000 })), 1_000_000);
        assert_eq!(cap(json!({ "cancel_final_max_entries": 1_000_000 })), 1_000_000);
        assert_eq!(cap(json!({ "cancel_final_max_entries": 3 })), 3);
        assert_eq!(cap(json!({ "cancel_final_max_entries": -3 })), 100_000);
    }

    #[test]
    fn t_k8_e_restore_switch_reader() {
        let on = |conf: Json| maker_restore_skips_cancelled(&conf_ctx(conf));
        assert!(on(json!({})));
        assert!(on(json!({ "maker_restore_skips_cancelled": true })));
        assert!(on(json!({ "maker_restore_skips_cancelled": "false" })));
        assert!(on(json!({ "maker_restore_skips_cancelled": 0 })));
        assert!(on(json!({ "maker_restore_skips_cancelled": null })));
        assert!(!on(json!({ "maker_restore_skips_cancelled": false })));
    }

    // (f)

    // (g)

    /// The cancel of the order `U` of the maker by `X`, a publisher that is not the maker, and then by the maker.
    fn cancelled_by_other_and_by_maker(conf: Json) -> (MmArc, Maker, Maker) {
        let (ctx, _rx) = node(conf);
        let maker = Maker::new("final-cancel-g-maker");
        let other = Maker::new("final-cancel-g-other");
        let uuid = maker.cancelled.uuid;
        assert!(receive(&ctx, &cancelled(&other.secret, uuid)));
        assert!(receive(&ctx, &cancelled(&maker.secret, uuid)));
        assert!(guarded(&ctx, uuid, &other.pubkey));
        assert!(guarded(&ctx, uuid, &maker.pubkey));
        assert_eq!(octx(&ctx).orderbook.lock().recently_cancelled.len(), 2, "two entries");
        (ctx, maker, other)
    }

    #[test]
    fn t_k8_g_a_cancel_is_kept_per_publisher() {
        let (ctx, maker, other) = cancelled_by_other_and_by_maker(json!({}));
        let uuid = maker.cancelled.uuid;
        let from_maker = created(&maker.secret, &maker.cancelled);
        let from_other = created(&other.secret, &maker.cancelled);

        // The maker's window counts from the maker's cancel; the other publisher's entry is still young.
        set_guard_age(&ctx, uuid, &maker.pubkey, 3599);
        receive(&ctx, &from_maker);
        assert!(book(&ctx).is_empty(), "kept out just below the maker's window");
        set_guard_age(&ctx, uuid, &maker.pubkey, 3601);
        receive(&ctx, &from_maker);
        assert!(book(&ctx).contains(&uuid), "inserted just above the maker's window");
        assert_eq!(octx(&ctx).orderbook.lock().order_set[&uuid].pubkey, maker.pubkey);

        // The other publisher's copy is kept out for the other publisher's own window only.
        receive(&ctx, &from_other);
        assert_eq!(octx(&ctx).orderbook.lock().order_set[&uuid].pubkey, maker.pubkey);
        set_guard_age(&ctx, uuid, &other.pubkey, 3601);
        receive(&ctx, &from_other);
        assert_eq!(octx(&ctx).orderbook.lock().order_set[&uuid].pubkey, other.pubkey);
    }

    /// Neither cancel refreshes the other's entry: the entry of the first, aged to just under the window, is gone
    /// a moment after the second cancel arrived.
    #[test]
    fn t_k8_g_neither_cancel_refreshes_the_other() {
        for other_first in [true, false] {
            let (ctx, _rx) = node(json!({ "cancel_final_window_secs": 120 }));
            let maker = Maker::new("final-cancel-g-maker");
            let other = Maker::new("final-cancel-g-other");
            let uuid = maker.cancelled.uuid;
            let (first, second) = if other_first {
                (&other, &maker)
            } else {
                (&maker, &other)
            };
            assert!(receive(&ctx, &cancelled(&first.secret, uuid)));
            octx(&ctx)
                .orderbook
                .lock()
                .recently_cancelled
                .remove((uuid, first.pubkey.clone()));
            octx(&ctx).orderbook.lock().recently_cancelled.insert_aged(
                (uuid, first.pubkey.clone()),
                (),
                Duration::from_millis(119_700),
            );
            assert!(receive(&ctx, &cancelled(&second.secret, uuid)));
            assert!(guarded(&ctx, uuid, &second.pubkey));
            std::thread::sleep(Duration::from_millis(600));
            assert!(
                !guarded(&ctx, uuid, &first.pubkey),
                "the first entry ran out on its own"
            );
            assert!(guarded(&ctx, uuid, &second.pubkey), "the second entry is still young");
            receive(&ctx, &created(&first.secret, &maker.cancelled));
            assert_eq!(
                book(&ctx),
                HashSet::from([uuid]),
                "the first publisher's copy is inserted"
            );
        }
    }

    #[test]
    fn t_k8_f_after_a_restart_nothing_is_guarded() {
        let maker = Maker::new("final-cancel-f");
        let (ctx, _rx) = node(json!({}));
        assert!(receive(&ctx, &cancelled(&maker.secret, maker.cancelled.uuid)));
        assert!(guarded(&ctx, maker.cancelled.uuid, &maker.pubkey));
        // The restart: a new context starts with an empty guard, so the late copy is inserted.
        let (restarted, _rx) = node(json!({}));
        assert_eq!(octx(&restarted).orderbook.lock().recently_cancelled.len(), 0);
        assert!(receive(&restarted, &created(&maker.secret, &maker.cancelled)));
        assert!(book(&restarted).contains(&maker.cancelled.uuid));
    }

    /// CRD ch.32 §32.9C.2 R-GH14, T-GH14: per-pair liveness on the receiver's clock. The passage of time is
    /// simulated the way the other liveness tests do it: the liveness instant of a pair is moved into the past
    /// (`age`), and the sweep is driven through `prune_stale_pairs`, which takes the clock reading as an
    /// argument. The maker's record keeps a fresh instant of its own (it still sends keep-alives for its other
    /// pairs), so only the pair instants age.
    mod t_gh14_pair_liveness {
        use super::*;
        use crate::mm2::lp_ordermatch::ordermatch_orderbook::apply_sync_answer;

        const X: &str = "C1:C2";
        const Y: &str = "C1:C3";
        const WINDOW: u64 = 90;
        const OWN_PERSISTENT: &str = "own-persistent-pubkey";
        const OWN_PER_ORDER: &str = "own-per-order-pubkey";

        /// A maker with two orders on X and two on Y.
        struct Poster {
            pubkey: String,
            secret: [u8; 32],
            x: Vec<OrderbookItem>,
            y: Vec<OrderbookItem>,
        }

        impl Poster {
            fn new(name: &str) -> Poster {
                let (pubkey, secret) = pubkey_and_secret_for_test(name);
                let x = make_random_orders(pubkey.clone(), &secret, "C1".into(), "C2".into(), 2);
                let y = make_random_orders(pubkey.clone(), &secret, "C1".into(), "C3".into(), 2);
                Poster { pubkey, secret, x, y }
            }

            fn all(&self) -> Vec<OrderbookItem> { [self.x.clone(), self.y.clone()].concat() }
        }

        fn uuids(orders: &[OrderbookItem]) -> HashSet<Uuid> { orders.iter().map(|o| o.uuid).collect() }

        /// A node: a keep-alive that requests sends exactly one command (no second request, no alternate relay).
        fn node_with(conf: Json) -> (MmArc, mpsc::Receiver<AdexBehaviourCmd>) {
            let mut conf = conf;
            let members = conf.as_object_mut().unwrap();
            members.entry("sync_request_retry_ms").or_insert(json!(0));
            members.entry("sync_request_alternate_relay").or_insert(json!(false));
            node(conf)
        }

        /// A node that holds the orders of the poster on both pairs, received as created messages.
        fn known_node(conf: Json) -> (MmArc, mpsc::Receiver<AdexBehaviourCmd>, Poster) {
            let (ctx, rx) = node_with(conf);
            let poster = Poster::new("gh14-poster");
            for order in poster.all() {
                assert!(receive(&ctx, &created(&poster.secret, &order)));
            }
            (ctx, rx, poster)
        }

        fn updated(secret: &[u8; 32], uuid: Uuid) -> Vec<u8> {
            let mut msg = MakerOrderUpdated::new(uuid);
            msg.with_new_price(BigRational::new(3.into(), 2.into()));
            encode_and_sign(&new_protocol::OrdermatchMessage::MakerOrderUpdated(msg), secret).unwrap()
        }

        /// The liveness instant of (`pubkey`, `pair`).
        fn seen(ctx: &MmArc, pubkey: &str, pair: &str) -> Option<u64> { octx(ctx).pair_liveness.get(pubkey, pair) }

        /// Simulates `secs` passing since the pair's last liveness instant.
        fn age(ctx: &MmArc, pubkey: &str, pair: &str, secs: u64) {
            assert!(seen(ctx, pubkey, pair).is_some(), "the pair has an instant");
            octx(ctx).pair_liveness.age(pubkey, pair, secs);
        }

        /// One sweep pass at the current reading of the clock, with the window `timeout`.
        fn prune_with(ctx: &MmArc, timeout: u64) -> Vec<PrunedPair> {
            let pruned = prune_stale_pairs(ctx, &octx(ctx), OWN_PERSISTENT, timeout, now_ms() / 1000);
            flush_trie(ctx);
            pruned
        }

        fn prune(ctx: &MmArc) -> Vec<PrunedPair> { prune_with(ctx, WINDOW) }

        fn pruned_pairs(pruned: Vec<PrunedPair>) -> Vec<String> {
            let mut pairs: Vec<String> = pruned.into_iter().map(|p| p.alb_pair).collect();
            pairs.sort();
            pairs
        }

        /// The orders of `pubkey` on `pair` in the read index.
        fn on_book(ctx: &MmArc, pubkey: &str, pair: &str) -> HashSet<Uuid> {
            octx(ctx)
                .orderbook
                .lock()
                .pubkey_pair_order_uuids(pubkey, pair)
                .into_iter()
                .collect()
        }

        fn root(ctx: &MmArc, pubkey: &str, pair: &str) -> Option<H64> {
            let octx = octx(ctx);
            let trie_store = octx.trie_store.lock();
            trie_store
                .pubkeys_state
                .get(pubkey)
                .and_then(|state| state.trie_roots.get(pair).copied())
        }

        fn has_history(ctx: &MmArc, pubkey: &str, pair: &str) -> bool {
            let octx = octx(ctx);
            let trie_store = octx.trie_store.lock();
            trie_store
                .pubkeys_state
                .get(pubkey)
                .is_some_and(|state| state.order_pairs_trie_state_history.get(&pair.to_owned()).is_some())
        }

        /// The keep-alive from `pubkey` naming `roots`, handled by `ctx` as a relay or as a non-relay. Returns the
        /// roots of the synchronization request it sent, if any; that request is left unanswered.
        fn keep_alive(
            ctx: &MmArc,
            rx: &mut mpsc::Receiver<AdexBehaviourCmd>,
            pubkey: &str,
            roots: HashMap<String, H64>,
            relay: bool,
        ) -> Option<HashMap<String, H64>> {
            let keep_alive = PubkeyKeepAlive {
                trie_roots: roots,
                timestamp: now_ms() / 1000,
            };
            let (ctx2, pubkey2) = (ctx.clone(), pubkey.to_owned());
            let task = std::thread::spawn(move || {
                block_on(process_orders_keep_alive(
                    ctx2,
                    FORWARDER.to_owned(),
                    pubkey2,
                    keep_alive,
                    relay,
                ))
            });
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match rx.try_recv() {
                    Ok(AdexBehaviourCmd::RequestPeers { req, response_tx, .. }) => {
                        let trie_roots = match decode_message::<P2PRequest>(&req).unwrap() {
                            P2PRequest::Ordermatch(OrdermatchRequest::SyncPubkeyOrderbookState {
                                trie_roots, ..
                            }) => trie_roots,
                            other => panic!("unexpected request {:?}", other),
                        };
                        drop(response_tx);
                        assert!(!task.join().unwrap(), "the unanswered request failed");
                        return Some(trie_roots);
                    },
                    Ok(other) => panic!("unexpected command {:?}", other),
                    Err(_) if task.is_finished() => {
                        assert!(task.join().unwrap(), "the keep-alive was handled");
                        return None;
                    },
                    Err(_) => {
                        assert!(Instant::now() < deadline, "the keep-alive was not handled");
                        std::thread::sleep(Duration::from_millis(1));
                    },
                }
            }
        }

        /// The roots a keep-alive naming the pairs `pairs` carries when it advertises what the node holds.
        fn held_roots(ctx: &MmArc, pubkey: &str, pairs: &[&str]) -> HashMap<String, H64> {
            pairs
                .iter()
                .map(|pair| (pair.to_string(), root(ctx, pubkey, pair).unwrap()))
                .collect()
        }

        fn answer_of(pair: &str, diff: DeltaOrFullTrie<Uuid, OrderbookP2PItem>) -> SyncPubkeyOrderbookStateRes {
            SyncPubkeyOrderbookStateRes {
                last_signed_pubkey_payload: vec![],
                pair_orders_diff: HashMap::from([(pair.to_owned(), diff)]),
                protocol_infos: HashMap::new(),
                conf_infos: HashMap::new(),
            }
        }

        fn apply_answer(ctx: &MmArc, pubkey: &str, answer: SyncPubkeyOrderbookStateRes) {
            apply_sync_answer(&octx(ctx), pubkey, answer);
            flush_trie(ctx);
        }

        // (a)

        #[test]
        fn t_gh14_a_emptied_pair_leaves_other_pairs_stay() {
            let (ctx, _rx, p) = known_node(json!({}));
            assert!(
                seen(&ctx, &p.pubkey, X).is_some(),
                "created messages give the pair an instant"
            );
            assert!(seen(&ctx, &p.pubkey, Y).is_some());
            assert!(has_history(&ctx, &p.pubkey, X));

            // P's only orders on X were cancelled, the node missed it, and P advertises Y only: X's last
            // naming is past the window.
            age(&ctx, &p.pubkey, X, WINDOW + 5);
            let pruned = prune(&ctx);
            assert_eq!(pruned, vec![PrunedPair {
                pubkey: p.pubkey.clone(),
                alb_pair: X.to_owned(),
                removed_orders: 2,
            }]);

            assert!(on_book(&ctx, &p.pubkey, X).is_empty());
            assert_eq!(on_book(&ctx, &p.pubkey, Y), uuids(&p.y), "the other pair stays");
            assert_eq!(book(&ctx), uuids(&p.y));
            assert_eq!(root(&ctx, &p.pubkey, X), None, "the held root is dropped");
            assert!(!has_history(&ctx, &p.pubkey, X), "the delta history is dropped");
            assert_eq!(seen(&ctx, &p.pubkey, X), None);
            assert!(root(&ctx, &p.pubkey, Y).is_some());
            assert!(seen(&ctx, &p.pubkey, Y).is_some());
            assert!(
                octx(&ctx).trie_store.lock().pubkeys_state.contains_key(&p.pubkey),
                "the record stays"
            );
            assert!(prune(&ctx).is_empty(), "nothing is pruned twice");
        }

        #[test]
        fn t_gh14_a_pair_is_pruned_at_the_window_and_not_before() {
            let (ctx, _rx, p) = known_node(json!({}));
            age(&ctx, &p.pubkey, X, WINDOW - 5);
            assert!(prune(&ctx).is_empty(), "within the window");
            age(&ctx, &p.pubkey, X, 10);
            assert_eq!(pruned_pairs(prune(&ctx)), vec![X.to_owned()], "past the window");
        }

        #[test]
        fn t_gh14_a_switch_off_keeps_the_pair_until_the_record_is_evicted() {
            let (ctx, _rx, p) = known_node(json!({ "pair_liveness": false }));
            assert!(octx(&ctx).pair_liveness.is_empty(), "no instant is kept");
            // Time far past the window: no pair of the record is pruned.
            let far = now_ms() / 1000 + 10 * WINDOW;
            assert!(prune_stale_pairs(&ctx, &octx(&ctx), OWN_PERSISTENT, WINDOW, far).is_empty());
            assert_eq!(book(&ctx), uuids(&p.all()));

            // The entries leave with the record (T-GH1, T-GH3).
            {
                let octx = octx(&ctx);
                octx.trie_store
                    .lock()
                    .pubkeys_state
                    .get_mut(&p.pubkey)
                    .unwrap()
                    .last_keep_alive -= WINDOW + 5;
            }
            let octx = octx(&ctx);
            let my_p2p_pubkeys = octx.orderbook.lock().my_p2p_pubkeys.clone();
            let trie_store = octx.trie_store.lock();
            let (evicted_orders, evicted_pubkeys) = stale_pubkeys(
                &trie_store.pubkeys_state,
                OWN_PERSISTENT,
                &my_p2p_pubkeys,
                WINDOW,
                now_ms() / 1000,
            );
            assert_eq!(evicted_pubkeys, vec![p.pubkey.clone()]);
            assert_eq!(evicted_orders.into_iter().collect::<HashSet<_>>(), uuids(&p.all()));
        }

        #[test]
        fn t_gh14_a_switch_off_records_no_instant_for_keep_alives_and_answers() {
            let (ctx, mut rx, p) = known_node(json!({ "pair_liveness": false }));
            let roots = HashMap::from([(X.to_owned(), [7; 8])]);
            assert!(keep_alive(&ctx, &mut rx, &p.pubkey, roots, true).is_some());
            let values = p.x.iter().map(|o| (o.uuid, o.clone().into())).collect();
            apply_answer(&ctx, &p.pubkey, answer_of(X, DeltaOrFullTrie::FullTrie(values)));
            assert!(octx(&ctx).pair_liveness.is_empty());
        }

        // (b)

        #[test]
        fn t_gh14_b_lost_keep_alives_prune_then_the_next_keep_alive_refills() {
            let p = Poster::new("gh14-b");
            let (maker_ctx, _maker_rx) = maker_node(&p.all());
            let (relay, mut relay_rx) = node_with(json!({}));
            assert!(
                !sync_with(&relay, &mut relay_rx, &maker_ctx, &p.pubkey),
                "a snapshot is answered"
            );
            // The answer is applied to the index at once, to the trie by the worker: a full snapshot clears the
            // pair and its instant there and creates them again. Aging the instant before that would age nothing.
            flush_trie(&relay);
            assert_eq!(book(&relay), uuids(&p.all()));
            assert!(
                seen(&relay, &p.pubkey, X).is_some(),
                "a keep-alive gives the pair an instant"
            );

            // The keep-alives for X are withheld for longer than the window; Y's keep alive on.
            age(&relay, &p.pubkey, X, WINDOW + 5);
            assert_eq!(pruned_pairs(prune(&relay)), vec![X.to_owned()]);
            assert_eq!(book(&relay), uuids(&p.y));
            assert_eq!(root(&relay, &p.pubkey, X), None);

            // The next keep-alive naming X and Y requests X as a new pair, and only X: the held root
            // is gone, so the request names the all-zero root (R-Q4, R-Q4a).
            let roots = keep_alive_of(&maker_ctx, &p.pubkey).trie_roots;
            assert_eq!(roots.len(), 2);
            let request = keep_alive(&relay, &mut relay_rx, &p.pubkey, roots, true).expect("a request is sent");
            assert_eq!(request, HashMap::from([(X.to_owned(), H64::default())]));
            assert!(
                seen(&relay, &p.pubkey, X).is_some(),
                "the placeholder has its creation instant"
            );

            // The answer restores the orders: one request, answered with a full snapshot.
            assert!(!sync_with(&relay, &mut relay_rx, &maker_ctx, &p.pubkey));
            assert!(relay_rx.try_recv().is_err(), "one request per keep-alive");
            assert_eq!(book(&relay), uuids(&p.all()));
            flush_trie(&relay);
            assert_eq!(root(&relay, &p.pubkey, X), root(&maker_ctx, &p.pubkey, X));
            assert!(prune(&relay).is_empty());
        }

        // (c)

        /// The pairs pruned when X is aged by 80 s, `refresh` happens, and X is aged by 80 s again.
        fn pruned_after(refresh: impl FnOnce(&MmArc, &mut mpsc::Receiver<AdexBehaviourCmd>, &Poster)) -> Vec<String> {
            let (ctx, mut rx, p) = known_node(json!({}));
            age(&ctx, &p.pubkey, X, WINDOW - 10);
            refresh(&ctx, &mut rx, &p);
            age(&ctx, &p.pubkey, X, WINDOW - 10);
            pruned_pairs(prune(&ctx))
        }

        #[test]
        fn t_gh14_c_without_a_refresh_the_pair_is_pruned() {
            assert_eq!(pruned_after(|_, _, _| {}), vec![X.to_owned()]);
        }

        #[test]
        fn t_gh14_c_keep_alive_with_a_differing_root_refreshes_and_requests() {
            let pruned = pruned_after(|ctx, rx, p| {
                let roots = HashMap::from([(X.to_owned(), [7; 8])]);
                let held = root(ctx, &p.pubkey, X).unwrap();
                let request = keep_alive(ctx, rx, &p.pubkey, roots, true).expect("the pair is requested");
                assert_eq!(request, HashMap::from([(X.to_owned(), held)]));
            });
            assert!(pruned.is_empty());
        }

        #[test]
        fn t_gh14_c_keep_alive_with_the_held_root_refreshes() {
            let pruned = pruned_after(|ctx, rx, p| {
                let roots = held_roots(ctx, &p.pubkey, &[X]);
                assert_eq!(keep_alive(ctx, rx, &p.pubkey, roots, true), None);
            });
            assert!(pruned.is_empty());
        }

        #[test]
        fn t_gh14_c_created_message_refreshes() {
            let pruned = pruned_after(|ctx, _rx, p| {
                let more = make_random_orders(p.pubkey.clone(), &p.secret, "C1".into(), "C2".into(), 1);
                assert!(receive(ctx, &created(&p.secret, &more[0])));
            });
            assert!(pruned.is_empty());
        }

        #[test]
        fn t_gh14_c_updated_message_refreshes() {
            let pruned = pruned_after(|ctx, _rx, p| {
                assert!(receive(ctx, &updated(&p.secret, p.x[0].uuid)));
            });
            assert!(pruned.is_empty());
        }

        #[test]
        fn t_gh14_c_updated_message_for_an_unknown_order_refreshes_nothing() {
            let pruned = pruned_after(|ctx, _rx, p| {
                assert!(!receive(ctx, &updated(&p.secret, Uuid::new_v4())));
            });
            assert_eq!(pruned, vec![X.to_owned()]);
        }

        #[test]
        fn t_gh14_c_a_created_message_the_guard_drops_refreshes_nothing() {
            let pruned = pruned_after(|ctx, _rx, p| {
                let order = make_random_orders(p.pubkey.clone(), &p.secret, "C1".into(), "C2".into(), 1).remove(0);
                assert!(receive(ctx, &cancelled(&p.secret, order.uuid)));
                receive(ctx, &created(&p.secret, &order));
                assert!(!book(ctx).contains(&order.uuid));
            });
            assert_eq!(pruned, vec![X.to_owned()]);
        }

        #[test]
        fn t_gh14_c_applied_full_snapshot_refreshes() {
            let pruned = pruned_after(|ctx, _rx, p| {
                let values = p.x.iter().map(|o| (o.uuid, o.clone().into())).collect();
                apply_answer(ctx, &p.pubkey, answer_of(X, DeltaOrFullTrie::FullTrie(values)));
            });
            assert!(pruned.is_empty());
        }

        #[test]
        fn t_gh14_c_applied_delta_that_inserts_refreshes() {
            let pruned = pruned_after(|ctx, _rx, p| {
                let more = make_random_orders(p.pubkey.clone(), &p.secret, "C1".into(), "C2".into(), 1).remove(0);
                let delta = HashMap::from([(more.uuid, Some(more.into()))]);
                apply_answer(ctx, &p.pubkey, answer_of(X, DeltaOrFullTrie::Delta(delta)));
            });
            assert!(pruned.is_empty());
        }

        #[test]
        fn t_gh14_c_applied_delta_that_removes_and_leaves_an_order_refreshes() {
            let pruned = pruned_after(|ctx, _rx, p| {
                let delta = HashMap::from([(p.x[0].uuid, None)]);
                apply_answer(ctx, &p.pubkey, answer_of(X, DeltaOrFullTrie::Delta(delta)));
                assert_eq!(on_book(ctx, &p.pubkey, X), HashSet::from([p.x[1].uuid]));
            });
            assert!(pruned.is_empty());
        }

        #[test]
        fn t_gh14_c_applied_answer_that_leaves_no_order_refreshes_nothing() {
            let pruned = pruned_after(|ctx, _rx, p| {
                let delta = p.x.iter().map(|o| (o.uuid, None)).collect();
                apply_answer(ctx, &p.pubkey, answer_of(X, DeltaOrFullTrie::Delta(delta)));
                assert!(on_book(ctx, &p.pubkey, X).is_empty());
            });
            assert_eq!(pruned, vec![X.to_owned()], "the emptied pair is dropped by the sweep");

            // An empty snapshot clears the pair, its root and its instant at once; nothing is left to prune.
            let (ctx, _rx, p) = known_node(json!({}));
            age(&ctx, &p.pubkey, X, WINDOW + 5);
            apply_answer(&ctx, &p.pubkey, answer_of(X, DeltaOrFullTrie::FullTrie(vec![])));
            assert!(on_book(&ctx, &p.pubkey, X).is_empty());
            assert_eq!((root(&ctx, &p.pubkey, X), seen(&ctx, &p.pubkey, X)), (None, None));
            assert!(prune(&ctx).is_empty());
        }

        #[test]
        fn t_gh14_c_applied_order_book_answer_refreshes() {
            let pruned = pruned_after(|ctx, rx, p| {
                let task_ctx = ctx.clone();
                let task = std::thread::spawn(move || block_on(request_and_fill_orderbook(&task_ctx, "C1", "C2")));
                let response_tx = match next_cmd(rx) {
                    AdexBehaviourCmd::RequestAnyRelay { response_tx, .. } => response_tx,
                    other => panic!("unexpected command {:?}", other),
                };
                let item = GetOrderbookPubkeyItem {
                    orders: p.x.iter().map(|order| (order.uuid, order.clone().into())).collect(),
                    last_keep_alive: now_ms() / 1000,
                    last_signed_pubkey_payload: vec![],
                };
                let answer = GetOrderbookRes {
                    pubkey_orders: HashMap::from([(p.pubkey.clone(), item)]),
                    protocol_infos: HashMap::new(),
                    conf_infos: p.x.iter().filter_map(|o| Some((o.uuid, o.conf_settings?))).collect(),
                };
                response_tx
                    .send(Some((PeerId::random(), encode_message(&answer).unwrap())))
                    .unwrap();
                task.join().unwrap().unwrap();
                flush_trie(ctx);
            });
            assert!(pruned.is_empty());
        }

        #[test]
        fn t_gh14_c_keep_alive_with_a_zero_or_empty_root_refreshes_nothing() {
            for advertised in [H64::default(), hashed_null_node::<Layout>()] {
                let pruned = pruned_after(|ctx, rx, p| {
                    let roots = HashMap::from([(X.to_owned(), advertised)]);
                    assert_eq!(keep_alive(ctx, rx, &p.pubkey, roots, true), None, "no request either");
                });
                assert_eq!(pruned, vec![X.to_owned()]);
            }
        }

        #[test]
        fn t_gh14_c_keep_alive_that_creates_a_pair_gives_it_the_instant_of_its_creation() {
            let p = Poster::new("gh14-placeholder");
            let (ctx, mut rx) = node_with(json!({}));
            let before = now_ms() / 1000;
            let request = keep_alive(&ctx, &mut rx, &p.pubkey, HashMap::from([(X.to_owned(), [3; 8])]), true);
            assert_eq!(request, Some(HashMap::from([(X.to_owned(), H64::default())])));
            assert_eq!(root(&ctx, &p.pubkey, X), Some(H64::default()), "the placeholder");
            assert!(seen(&ctx, &p.pubkey, X).unwrap() >= before);
            // The maker never answers and never names the pair again: the placeholder goes after the window.
            age(&ctx, &p.pubkey, X, WINDOW + 5);
            let pruned = prune(&ctx);
            assert_eq!(pruned.len(), 1);
            assert_eq!(pruned[0].removed_orders, 0);
            assert_eq!(root(&ctx, &p.pubkey, X), None);
        }

        // (d)

        fn subscribe(ctx: &MmArc, pair: &str) {
            octx(ctx).orderbook_subscriptions.write().insert(
                orderbook_topic_from_ordered_pair(pair),
                OrderbookRequestingState::Requested,
            );
        }

        /// Pairs pruned after two keep-alives, each within the window of the last, that name X and Y with the
        /// held roots.
        fn pruned_after_keep_alives_naming_both(relay: bool) -> Vec<String> {
            let (ctx, mut rx, p) = known_node(json!({}));
            subscribe(&ctx, Y);
            for _ in 0..2 {
                age(&ctx, &p.pubkey, X, WINDOW - 10);
                age(&ctx, &p.pubkey, Y, WINDOW - 10);
                let roots = held_roots(&ctx, &p.pubkey, &[X, Y]);
                assert_eq!(keep_alive(&ctx, &mut rx, &p.pubkey, roots, relay), None);
            }
            age(&ctx, &p.pubkey, X, WINDOW - 10);
            age(&ctx, &p.pubkey, Y, WINDOW - 10);
            pruned_pairs(prune(&ctx))
        }

        #[test]
        fn t_gh14_d_non_relay_prunes_a_pair_it_is_not_subscribed_to() {
            assert_eq!(pruned_after_keep_alives_naming_both(false), vec![X.to_owned()]);
        }

        #[test]
        fn t_gh14_d_relay_refreshes_every_named_pair() {
            assert!(pruned_after_keep_alives_naming_both(true).is_empty());
        }

        // (e)

        #[test]
        fn t_gh14_e_own_pubkeys_are_never_pruned() {
            let (ctx, _rx, p) = known_node(json!({}));
            let own = make_random_orders(OWN_PERSISTENT.into(), &[0; 32], "C1".into(), "C2".into(), 2);
            let per_order = make_random_orders(OWN_PER_ORDER.into(), &[0; 32], "C1".into(), "C3".into(), 2);
            for order in own.iter().chain(per_order.iter()) {
                insert_or_update_order(&ctx, order.clone());
            }
            octx(&ctx)
                .orderbook
                .lock()
                .my_p2p_pubkeys
                .insert(OWN_PER_ORDER.to_owned());
            flush_trie(&ctx);

            // Nothing has named any of these pairs for far longer than the window.
            for (pubkey, pair) in [(&p.pubkey, X), (&p.pubkey, Y)] {
                age(&ctx, pubkey, pair, 10 * WINDOW);
            }
            age(&ctx, OWN_PERSISTENT, X, 10 * WINDOW);
            age(&ctx, OWN_PER_ORDER, Y, 10 * WINDOW);

            let pruned = prune(&ctx);
            let mut pruned: Vec<(String, String)> = pruned.into_iter().map(|p| (p.pubkey, p.alb_pair)).collect();
            pruned.sort();
            let mut expected = vec![(p.pubkey.clone(), X.to_owned()), (p.pubkey.clone(), Y.to_owned())];
            expected.sort();
            assert_eq!(pruned, expected, "only the other maker's pairs");
            assert_eq!(on_book(&ctx, OWN_PERSISTENT, X), uuids(&own));
            assert_eq!(on_book(&ctx, OWN_PER_ORDER, Y), uuids(&per_order));
            assert!(root(&ctx, OWN_PERSISTENT, X).is_some());
            assert!(root(&ctx, OWN_PER_ORDER, Y).is_some());
        }

        // (f)

        #[test]
        fn t_gh14_f_window_follows_maker_order_timeout() {
            let (ctx, _rx, p) = known_node(json!({ "maker_order_timeout": 300 }));
            age(&ctx, &p.pubkey, X, 200);
            assert!(prune_with(&ctx, 300).is_empty(), "200 s is within a window of 300 s");
            age(&ctx, &p.pubkey, X, 105);
            assert_eq!(
                pruned_pairs(prune_with(&ctx, 300)),
                vec![X.to_owned()],
                "305 s is past it"
            );

            let (ctx, _rx, p) = known_node(json!({}));
            age(&ctx, &p.pubkey, X, 200);
            assert_eq!(
                pruned_pairs(prune_with(&ctx, WINDOW)),
                vec![X.to_owned()],
                "200 s is past the default window"
            );
        }

        // (g)

        #[test]
        fn t_gh14_g_cancelled_order_stays_out_after_prune_and_refill() {
            let p = Poster::new("gh14-g");
            // The maker's own view lists both orders on X, as a stale responder's would.
            let (maker_ctx, _maker_rx) = maker_node(&p.x);
            let (relay, mut relay_rx) = node_with(json!({}));
            for order in &p.x {
                assert!(receive(&relay, &created(&p.secret, order)));
            }
            let cancelled_uuid = p.x[0].uuid;
            assert!(receive(&relay, &cancelled(&p.secret, cancelled_uuid)));
            assert!(guarded(&relay, cancelled_uuid, &p.pubkey));
            assert_eq!(book(&relay), HashSet::from([p.x[1].uuid]));

            age(&relay, &p.pubkey, X, WINDOW + 5);
            let pruned = prune(&relay);
            assert_eq!(pruned.len(), 1);
            assert_eq!(pruned[0].removed_orders, 1);
            assert!(book(&relay).is_empty());

            // The refill: the answer lists the cancelled order, which the final-cancel guard keeps out.
            assert!(
                !sync_with(&relay, &mut relay_rx, &maker_ctx, &p.pubkey),
                "a snapshot is answered"
            );
            assert_eq!(book(&relay), HashSet::from([p.x[1].uuid]));
            assert!(guarded(&relay, cancelled_uuid, &p.pubkey));
        }

        // (h)

        #[test]
        fn t_gh14_h_switch_reader() {
            let on = |conf: Json| pair_liveness(&conf_ctx(conf));
            assert!(on(json!({})));
            assert!(on(json!({ "pair_liveness": true })));
            assert!(on(json!({ "pair_liveness": "false" })));
            assert!(on(json!({ "pair_liveness": 0 })));
            assert!(on(json!({ "pair_liveness": null })));
            assert!(!on(json!({ "pair_liveness": false })));
        }
    }
}

/// CRD ch.12 R7c, T9: a stored active record that outlived its order's removal is not restored, and the deletion
/// of the record at removal is retried.
mod t12_7c_restore_guard_tests {
    use super::*;
    use crate::mm2::database::my_orders::CREATE_MY_ORDERS_TABLE;
    use crate::mm2::lp_ordermatch::my_orders_storage::{delete_active_maker_order_attempt, MyOrdersError};
    use common::log::log_crate::{set_logger, set_max_level, Level, LevelFilter, Log, Metadata, Record};
    use futures::compat::Future01CompatExt;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::time::Instant;

    static LINES: Mutex<Vec<String>> = Mutex::new(Vec::new());

    struct Capture;

    impl Log for Capture {
        fn enabled(&self, metadata: &Metadata) -> bool { metadata.level() <= Level::Warn }

        fn log(&self, record: &Record) {
            if self.enabled(record.metadata()) {
                LINES
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(format!("{} {}", record.level(), record.args()));
            }
        }

        fn flush(&self) {}
    }

    static CAPTURE: Capture = Capture;

    /// Installs the capture of the warn lines. `false` when another logger already owns the process, in which case a
    /// test must not assert that a line is present.
    fn capture_installed() -> bool {
        let installed = set_logger(&CAPTURE).is_ok();
        if installed {
            set_max_level(LevelFilter::Warn);
        }
        // `set_logger` fails the second time too: the capture is ours when a line can be seen.
        log::warn!("t9 capture probe");
        installed || LINES.lock().unwrap().iter().any(|l| l.contains("t9 capture probe"))
    }

    /// The warn lines that name the order.
    fn lines_for(uuid: Uuid) -> Vec<String> {
        let uuid = uuid.to_string();
        LINES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|l| l.contains(&uuid))
            .cloned()
            .collect()
    }

    struct Node {
        ctx: MmArc,
        dbdir: std::path::PathBuf,
        p2p_rx: mpsc::Receiver<AdexBehaviourCmd>,
    }

    impl Drop for Node {
        fn drop(&mut self) { std::fs::remove_dir_all(&self.dbdir).ok(); }
    }

    impl Node {
        /// Real order files in a temporary directory, an in-memory history database, and `conf` merged in.
        fn new(conf: Json) -> Node {
            let dbdir = std::env::temp_dir().join(format!("kdf-restore-guard-{}", Uuid::new_v4()));
            let mut full_conf = json!({ "dbdir": dbdir.to_string_lossy(), "coins": [] });
            for (key, value) in conf.as_object().unwrap() {
                full_conf[key] = value.clone();
            }
            let ctx = MmCtxBuilder::new().with_conf(full_conf).into_mm_arc();
            std::fs::create_dir_all(my_maker_orders_dir(&ctx)).unwrap();
            std::fs::create_dir_all(my_taker_orders_dir(&ctx)).unwrap();
            std::fs::create_dir_all(my_orders_history_dir(&ctx)).unwrap();
            let connection = Connection::open_in_memory().unwrap();
            connection.execute(CREATE_MY_ORDERS_TABLE, []).unwrap();
            let _ = ctx.sqlite_connection.pin(Arc::new(Mutex::new(connection)));
            let (tx, p2p_rx) = mpsc::channel(1000);
            P2PContext::new(tx).store_to_mm_arc(&ctx);
            Node { ctx, dbdir, p2p_rx }
        }

        fn storage(&self) -> MyOrdersStorage { MyOrdersStorage::new(self.ctx.clone()) }

        fn active_file(&self, uuid: &Uuid) -> bool { my_maker_order_file_path(&self.ctx, uuid).exists() }

        fn history_bytes(&self, uuid: &Uuid) -> Option<Vec<u8>> {
            std::fs::read(my_order_history_file_path(&self.ctx, uuid)).ok()
        }

        /// The status and the last-updated time of the filtering-history row.
        fn row(&self, uuid: &Uuid) -> Option<(String, i64)> {
            self.ctx
                .sqlite_connection()
                .query_row(
                    "SELECT status, last_updated FROM my_orders WHERE uuid = ?1",
                    [uuid.to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .ok()
        }

        fn restored_makers(&self) -> HashSet<Uuid> {
            let ordermatch_ctx = OrdermatchContext::from_ctx(&self.ctx).unwrap();
            let uuids = ordermatch_ctx
                .maker_orders_ctx
                .lock()
                .clone_orders()
                .keys()
                .copied()
                .collect();
            uuids
        }

        fn restore(&self) { block_on(orders_kick_start(&self.ctx)).unwrap(); }

        /// The order is saved as `setprice` saves it, and then removed; the record is written back, as a record that
        /// outlived the removal.
        fn saved_removed_and_written_back(&self, order: &MakerOrder, reason: MakerOrderCancellationReason) {
            block_on(save_my_new_maker_order(self.ctx.clone(), order)).unwrap();
            block_on(delete_my_maker_order(self.ctx.clone(), order.clone(), reason).compat()).unwrap();
            block_on(self.storage().save_new_active_maker_order(order)).unwrap();
        }

        fn assert_nothing_published(&mut self) {
            assert!(self.p2p_rx.try_recv().is_err(), "a message was published");
        }
    }

    fn maker(save_in_history: bool) -> MakerOrder {
        MakerOrder {
            base: "DOC".into(),
            rel: "MARTY".into(),
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
            save_in_history,
            base_orderbook_ticker: None,
            rel_orderbook_ticker: None,
            p2p_privkey: None,
            timeout_in_minutes: None,
            swap_version: SwapVersion::default(),
        }
    }

    fn taker(save_in_history: bool) -> TakerOrder {
        TakerOrder {
            request: TakerRequest {
                base: "DOC".into(),
                rel: "MARTY".into(),
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
            save_in_history,
            base_orderbook_ticker: None,
            rel_orderbook_ticker: None,
            p2p_privkey: None,
        }
    }

    // (a)

    #[test]
    fn t9_a_a_record_whose_removal_the_history_records_is_not_restored() {
        let logs = capture_installed();
        let mut node = Node::new(json!({}));
        let stale = maker(true);
        let live = maker(true);
        node.saved_removed_and_written_back(&stale, MakerOrderCancellationReason::Cancelled);
        block_on(save_my_new_maker_order(node.ctx.clone(), &live)).unwrap();
        let history_before = node
            .history_bytes(&stale.uuid)
            .expect("the removal wrote a history record");
        let row_before = node.row(&stale.uuid).unwrap();
        assert_eq!(row_before.0, "Cancelled");
        assert!(node.active_file(&stale.uuid), "the record outlived the removal");

        node.restore();

        assert_eq!(node.restored_makers(), HashSet::from([live.uuid]));
        assert!(!node.active_file(&stale.uuid), "the stale record is deleted");
        assert!(node.active_file(&live.uuid));
        node.assert_nothing_published();
        assert_eq!(
            node.history_bytes(&stale.uuid),
            Some(history_before),
            "no new history entry"
        );
        assert_eq!(node.row(&stale.uuid), Some(row_before), "no new status");
        if logs {
            let lines = lines_for(stale.uuid);
            assert_eq!(lines.len(), 1, "{:?}", lines);
            assert!(
                lines[0].starts_with("WARN") && lines[0].contains("Cancelled"),
                "{:?}",
                lines
            );
        }
        assert!(lines_for(live.uuid).is_empty());
    }

    #[test]
    fn t9_a_every_removal_reason_counts() {
        let mut node = Node::new(json!({}));
        let reasons = [
            MakerOrderCancellationReason::Fulfilled,
            MakerOrderCancellationReason::InsufficientBalance,
            MakerOrderCancellationReason::Cancelled,
            MakerOrderCancellationReason::Expired,
        ];
        let stale: Vec<MakerOrder> = reasons.iter().map(|_| maker(true)).collect();
        for (order, reason) in stale.iter().zip(reasons) {
            node.saved_removed_and_written_back(order, reason);
        }
        node.restore();
        assert!(node.restored_makers().is_empty());
        for order in &stale {
            assert!(!node.active_file(&order.uuid));
        }
        node.assert_nothing_published();
    }

    #[test]
    fn t9_a_a_removal_status_without_a_history_record_or_a_history_record_without_a_status() {
        let node = Node::new(json!({}));
        let status_only = maker(true);
        let record_only = maker(true);
        let in_progress = maker(true);
        let updated = maker(true);
        for order in [&status_only, &record_only, &in_progress, &updated] {
            block_on(save_my_new_maker_order(node.ctx.clone(), order)).unwrap();
        }
        let storage = node.storage();
        // The history record write failed, the status was written.
        block_on(storage.update_order_status_in_filtering_history(status_only.uuid, "Fulfilled".to_owned())).unwrap();
        // The status write failed, the history record was written.
        block_on(storage.save_order_in_history(&Order::Maker(record_only.clone()))).unwrap();
        // An update leaves the status `Updated`, which an active order carries.
        block_on(storage.update_maker_order_in_filtering_history(&updated)).unwrap();
        assert_eq!(node.row(&updated.uuid).unwrap().0, "Updated");
        assert_eq!(node.row(&in_progress.uuid).unwrap().0, "Created");

        node.restore();

        assert_eq!(node.restored_makers(), HashSet::from([in_progress.uuid, updated.uuid]));
        assert!(!node.active_file(&status_only.uuid));
        assert!(!node.active_file(&record_only.uuid));
    }

    // (b)

    #[test]
    fn t9_b_an_order_not_kept_in_the_history_is_restored() {
        let node = Node::new(json!({}));
        let order = maker(false);
        // Its removal wrote no history entry, so its record is restored as before.
        node.saved_removed_and_written_back(&order, MakerOrderCancellationReason::Cancelled);
        assert!(node.history_bytes(&order.uuid).is_none());
        assert!(node.row(&order.uuid).is_none());
        node.restore();
        assert_eq!(node.restored_makers(), HashSet::from([order.uuid]));
        assert!(node.active_file(&order.uuid));
    }

    // (c)

    #[test]
    fn t9_c_the_switch_turns_the_guard_off() {
        let node = Node::new(json!({ "maker_restore_skips_cancelled": false }));
        let order = maker(true);
        node.saved_removed_and_written_back(&order, MakerOrderCancellationReason::Cancelled);
        node.restore();
        assert_eq!(node.restored_makers(), HashSet::from([order.uuid]));
        assert!(node.active_file(&order.uuid));
    }

    // (d)

    /// Mocks the attempt: it fails `failures` times and then succeeds. Returns the number of calls.
    fn mock_attempts(failures: usize) -> Arc<AtomicUsize> {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        delete_active_maker_order_attempt.mock_safe(move |_ctx, _uuid| {
            let call = counted.fetch_add(1, AtomicOrdering::SeqCst);
            let result = if call < failures {
                MmError::err(MyOrdersError::ErrorSaving("disk full".to_owned()))
            } else {
                Ok(())
            };
            MockResult::Return(Box::new(futures01::future::result(result)))
        });
        calls
    }

    fn remove(node: &Node, order: &MakerOrder) -> Duration {
        let started = Instant::now();
        block_on(
            delete_my_maker_order(node.ctx.clone(), order.clone(), MakerOrderCancellationReason::Cancelled).compat(),
        )
        .unwrap();
        started.elapsed()
    }

    #[test]
    fn t9_d_a_deletion_that_fails_twice_and_then_succeeds() {
        let logs = capture_installed();
        let node = Node::new(json!({}));
        let order = maker(false);
        let calls = mock_attempts(2);
        let elapsed = remove(&node, &order);
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 3);
        assert!(elapsed >= Duration::from_millis(400), "{:?}", elapsed);
        if logs {
            let lines = lines_for(order.uuid);
            assert_eq!(lines.len(), 2, "{:?}", lines);
            assert!(lines.iter().all(|l| l.starts_with("WARN") && !l.contains("Giving up")));
        }
    }

    #[test]
    fn t9_d_a_deletion_that_fails_three_times_gives_up_with_the_identifier() {
        let logs = capture_installed();
        let node = Node::new(json!({}));
        let order = maker(false);
        let calls = mock_attempts(3);
        let elapsed = remove(&node, &order);
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 3, "no fourth attempt");
        assert!(elapsed >= Duration::from_millis(400), "{:?}", elapsed);
        if logs {
            let lines = lines_for(order.uuid);
            assert_eq!(lines.len(), 4, "{:?}", lines);
            assert_eq!(lines.iter().filter(|l| l.contains("Giving up")).count(), 1);
            assert!(lines[3].contains("Giving up"));
        }
    }

    #[test]
    fn t9_d_the_history_writes_follow_the_failed_deletion() {
        let node = Node::new(json!({}));
        let order = maker(true);
        block_on(save_my_new_maker_order(node.ctx.clone(), &order)).unwrap();
        let _calls = mock_attempts(3);
        remove(&node, &order);
        assert!(node.history_bytes(&order.uuid).is_some());
        assert_eq!(node.row(&order.uuid).unwrap().0, "Cancelled");
    }

    #[test]
    fn t9_d_a_missing_record_is_not_retried() {
        let logs = capture_installed();
        let node = Node::new(json!({}));
        let order = maker(false);
        assert!(!node.active_file(&order.uuid));
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        // Counts the attempts, and lets the real one run.
        delete_active_maker_order_attempt.mock_safe(move |ctx, uuid| {
            counted.fetch_add(1, AtomicOrdering::SeqCst);
            MockResult::Continue((ctx, uuid))
        });
        remove(&node, &order);
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 1, "one attempt, no retry");
        if logs {
            assert!(lines_for(order.uuid).is_empty());
        }
    }

    #[test]
    fn t9_d_an_existing_record_is_deleted_at_the_first_attempt() {
        let node = Node::new(json!({}));
        let order = maker(false);
        block_on(save_my_new_maker_order(node.ctx.clone(), &order)).unwrap();
        assert!(node.active_file(&order.uuid));
        remove(&node, &order);
        assert!(!node.active_file(&order.uuid));
    }

    #[test]
    fn t9_d_the_switch_turns_the_retry_off() {
        let node = Node::new(json!({ "maker_restore_skips_cancelled": false }));
        let order = maker(false);
        let calls = mock_attempts(3);
        remove(&node, &order);
        assert_eq!(
            calls.load(AtomicOrdering::SeqCst),
            0,
            "the reference deletes once, directly"
        );
    }

    // (e)

    #[test]
    fn t9_e_a_stored_taker_record_is_restored() {
        let node = Node::new(json!({}));
        let order = taker(true);
        block_on(save_my_new_taker_order(node.ctx.clone(), &order)).unwrap();
        // Its removal is recorded in the history, and its record is written back.
        block_on(
            delete_my_taker_order(node.ctx.clone(), order.clone(), TakerOrderCancellationReason::Cancelled).compat(),
        )
        .unwrap();
        block_on(node.storage().save_new_active_taker_order(&order)).unwrap();
        assert!(node.history_bytes(&order.request.uuid).is_some());

        node.restore();

        let ordermatch_ctx = OrdermatchContext::from_ctx(&node.ctx).unwrap();
        let taker_orders = block_on(ordermatch_ctx.my_taker_orders.lock());
        assert!(taker_orders.contains_key(&order.request.uuid));
    }
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
        .prepare_sync_request_for_keep_alive(pubkey, message, false, SyncRequestRoot::Held, |topic| {
            subscribed_topics.contains(topic)
        })
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
    trie_store.prepare_sync_request_for_keep_alive(pubkey, message, false, SyncRequestRoot::Held, |_| false);
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

    let roots = advertisable_roots(&all_roots);
    assert_eq!(roots.len(), 1);
    assert_eq!(roots.get("E:F"), Some(&[1; 8]));
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
        .prepare_sync_request_for_keep_alive(pubkey, message, true, SyncRequestRoot::Held, |topic| {
            subscribed_topics.contains(topic)
        })
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
            MissingValue::Fail,
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
            MissingValue::Fail,
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
            // R60.6.7 Amendment (Decision AC): every update is re-checked; this one also changes the volume.
            volume_delta: Some(MmNumber::from("0.5")),
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

    /// CRD ch.60 R60.6.7 Amendment (Decision AC): every update re-checks the order, whatever it changes. The mock
    /// counts the safe-amount reads and reports a safe amount no order can meet, so a check that happens shows as
    /// the typed refusal.
    mod update_maker_order_check_scope {
        use super::*;
        use std::sync::atomic::{AtomicUsize, Ordering};

        fn update_request(uuid: Uuid, change: impl FnOnce(&mut MakerOrderUpdateReq)) -> MakerOrderUpdateReq {
            let mut req = MakerOrderUpdateReq {
                uuid,
                new_price: None,
                max: None,
                volume_delta: None,
                min_volume: None,
                base_confs: None,
                base_nota: None,
                rel_confs: None,
                rel_nota: None,
            };
            change(&mut req);
            req
        }

        /// Runs the update against an order of `DYN`/`KMD` whose coins always report an unmeetable safe amount.
        /// Returns the error text of the update, `None` if it succeeded, and the number of safe-amount reads.
        fn run(change: impl FnOnce(&mut MakerOrderUpdateReq)) -> (Option<String>, usize) {
            TestCoin::min_trading_vol.mock_safe(|_| MockResult::Return(units_8(10_000)));
            // The mocks are per thread, and so is this counter: tests run in parallel.
            let safe_amount_reads = Arc::new(AtomicUsize::new(0));
            let counted = safe_amount_reads.clone();
            TestCoin::spend_safe_amount.mock_safe(move |_| {
                counted.fetch_add(1, Ordering::SeqCst);
                MockResult::Return(Box::pin(async { Ok(Some(MmNumber::from(1_000))) }))
            });
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
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                block_on(update_maker_order(&ctx, update_request(order.uuid, change)))
            }));
            let reads = safe_amount_reads.load(Ordering::SeqCst);
            // Whatever a later stage of the update does without a database and a network (it may fail or
            // panic), it is not the check under test; only whether the check ran is.
            let error = match outcome {
                Ok(Ok(_)) => None,
                Ok(Err(e)) => Some(e),
                Err(_) => None,
            };
            (error, reads)
        }

        fn refused_by_the_check(error: &Option<String>) -> bool {
            error
                .as_deref()
                .is_some_and(|e| e.contains("is too low, required at least"))
        }

        #[test]
        fn price_only_update_is_rechecked() {
            let (error, reads) = run(|req| req.new_price = Some(MmNumber::from("2")));
            assert!(reads > 0, "{error:?}");
            assert!(refused_by_the_check(&error), "{error:?}");
        }

        #[test]
        fn min_volume_only_update_is_rechecked() {
            let (error, reads) = run(|req| req.min_volume = Some(units_8(12_000)));
            assert!(reads > 0, "{error:?}");
            assert!(refused_by_the_check(&error), "{error:?}");
        }

        #[test]
        fn confirmations_only_update_is_rechecked() {
            let (error, reads) = run(|req| {
                req.base_confs = Some(3);
                req.rel_nota = Some(true);
            });
            assert!(reads > 0, "{error:?}");
            assert!(refused_by_the_check(&error), "{error:?}");
        }

        #[test]
        fn volume_delta_update_is_rechecked() {
            let (error, reads) = run(|req| req.volume_delta = Some(MmNumber::from("0.5")));
            assert!(reads > 0);
            assert!(refused_by_the_check(&error), "{error:?}");
        }

        #[test]
        fn max_update_is_rechecked() {
            let (error, reads) = run(|req| req.max = Some(true));
            assert!(reads > 0);
            assert!(refused_by_the_check(&error), "{error:?}");
        }

        #[test]
        fn max_false_update_is_rechecked() {
            let (error, reads) = run(|req| {
                req.max = Some(false);
                req.new_price = Some(MmNumber::from("2"));
            });
            assert!(reads > 0, "{error:?}");
            assert!(refused_by_the_check(&error), "{error:?}");
        }

        /// Two price-only updates of the order, each refused under the conditions of (a). The safe amount of a
        /// coin is read through the request scope like the coin layer does, from a dynamic-fee estimate that the
        /// mock counts. Returns the refusals and the number of estimate fetches.
        fn two_price_only_updates(conf: Json) -> (Vec<String>, usize) {
            use coins::request_fee_scope::{memoize, FeeReadKind};
            TestCoin::min_trading_vol.mock_safe(|_| MockResult::Return(units_8(10_000)));
            let fetches = Arc::new(AtomicUsize::new(0));
            let counted = fetches.clone();
            TestCoin::spend_safe_amount.mock_safe(move |coin| {
                let (fetches, ticker) = (counted.clone(), coin.ticker().to_owned());
                MockResult::Return(Box::pin(async move {
                    let counted = fetches.clone();
                    memoize::<u64, (), _>(FeeReadKind::UtxoFeeEstimate, &ticker, async move {
                        counted.fetch_add(1, Ordering::SeqCst);
                        Ok(7)
                    })
                    .await
                    .unwrap();
                    Ok(Some(MmNumber::from(1_000)))
                }))
            });
            let ctx = ctx_with(conf, &["DYN", "KMD"]);
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
            let refusals = ["2", "3"]
                .iter()
                .map(|price| {
                    let req = update_request(order.uuid, |req| req.new_price = Some(MmNumber::from(*price)));
                    match block_on(update_maker_order(&ctx, req)) {
                        Err(e) => e,
                        Ok(_) => panic!("the update must be refused"),
                    }
                })
                .collect();
            (refusals, fetches.load(Ordering::SeqCst))
        }

        /// T60.6 (g): a price-only update is re-checked and refused under the conditions of (a), and while the
        /// fee reading is young enough it makes no fee request: the two updates cost one estimate fetch.
        #[test]
        fn t60_6_g_a_price_only_update_is_rechecked_and_served_from_the_cache() {
            let (refusals, fetches) = two_price_only_updates(json!({}));
            for refusal in &refusals {
                assert!(refusal.contains("is too low, required at least"), "{refusal}");
            }
            // The check refuses at the first leg it finds short (the base coin), so it reads that coin's
            // estimate once; the second update reads nothing.
            assert_eq!(fetches, 1, "one fetch for the two updates");
        }

        /// With the cache off, each of the same two updates reads the estimate.
        #[test]
        fn t60_6_g_with_the_cache_off_every_update_reads_the_estimate() {
            let (refusals, fetches) = two_price_only_updates(json!({"fee_estimate_cache_secs": 0}));
            assert!(refusals.iter().all(|r| r.contains("is too low, required at least")));
            assert_eq!(fetches, 2);
        }
    }

    /// CRD ch.38 R38.6.6j item 1, ch.57 R57.2.6, ch.60 R60.6.7 item 8: every order-posting entry point puts the
    /// request fee scope in place before its first coin call, so that all of its consumers share one fee reading.
    /// A probe in place of a coin method records whether a scope was active when the entry point reached it.
    mod request_fee_scope_entry {
        use super::*;
        use crate::mm2::lp_swap::{max_maker_vol, max_taker_vol, max_taker_vol_v2, trade_preimage_rpc};
        use coins::request_fee_scope::is_active;
        use coins::{BalanceError, BalanceTradeFeeUpdatedHandler, TradePreimageError};
        use std::sync::atomic::{AtomicUsize, Ordering};

        /// Counters of the probe: how often it was reached, and how often with a scope active.
        #[derive(Default)]
        struct Probe {
            reached: AtomicUsize,
            in_scope: AtomicUsize,
        }

        impl Probe {
            fn hit(&self) {
                self.reached.fetch_add(1, Ordering::SeqCst);
                if is_active() {
                    self.in_scope.fetch_add(1, Ordering::SeqCst);
                }
            }

            fn assert_reached_inside_a_scope(&self, entry: &str) {
                assert!(
                    self.reached.load(Ordering::SeqCst) > 0,
                    "{entry}: the probe was never reached"
                );
                assert_eq!(
                    self.in_scope.load(Ordering::SeqCst),
                    self.reached.load(Ordering::SeqCst),
                    "{entry}: reached outside a request fee scope"
                );
            }
        }

        /// `spend_safe_amount` and the balance of a `TestCoin` report to `probe` and fail, which ends the entry
        /// point right after the first coin call it makes.
        fn probe_coins(probe: &Arc<Probe>) {
            TestCoin::min_trading_vol.mock_safe(|_| MockResult::Return(units_8(10_000)));
            TestCoin::min_tx_amount.mock_safe(|_| MockResult::Return(BigDecimal::from(0)));
            let on_spend = probe.clone();
            TestCoin::spend_safe_amount.mock_safe(move |_| {
                let probe = on_spend.clone();
                MockResult::Return(Box::pin(async move {
                    probe.hit();
                    MmError::err(SpendSafeAmountError::EstimateUnavailable("probe".to_owned()))
                }))
            });
            let on_balance = probe.clone();
            TestCoin::my_balance.mock_safe(move |_| {
                on_balance.hit();
                MockResult::Return(Box::new(futures01::future::err(MmError::new(BalanceError::Internal(
                    "probe".to_owned(),
                )))))
            });
            let on_taker_fee = probe.clone();
            TestCoin::get_fee_to_send_taker_fee.mock_safe(move |_, _, _| {
                let probe = on_taker_fee.clone();
                MockResult::Return(Box::pin(async move {
                    probe.hit();
                    MmError::err(TradePreimageError::InternalError("probe".to_owned()))
                }))
            });
            let on_fee = probe.clone();
            TestCoin::get_sender_trade_fee.mock_safe(move |_, _, _| {
                let probe = on_fee.clone();
                MockResult::Return(Box::pin(async move {
                    probe.hit();
                    MmError::err(TradePreimageError::InternalError("probe".to_owned()))
                }))
            });
        }

        fn setup() -> (MmArc, Arc<Probe>) {
            let probe = Arc::new(Probe::default());
            probe_coins(&probe);
            let conf = json!({ "netid": mm2_net_config::SUPPORTED_NETIDS[0] });
            (ctx_with(conf, &["DYN", "KMD"]), probe)
        }

        #[test]
        fn setprice_enters_the_scope() {
            let (ctx, probe) = setup();
            let req: SetPriceReq =
                json::from_value(json!({"base": "DYN", "rel": "KMD", "price": "1", "volume": "1"})).unwrap();
            assert!(block_on(create_maker_order(&ctx, req)).is_err());
            probe.assert_reached_inside_a_scope("setprice");
        }

        #[test]
        fn update_maker_order_enters_the_scope() {
            let (ctx, probe) = setup();
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
                volume_delta: Some(MmNumber::from("0.5")),
                min_volume: None,
                base_confs: None,
                base_nota: None,
                rel_confs: None,
                rel_nota: None,
            };
            assert!(block_on(update_maker_order(&ctx, req)).is_err());
            probe.assert_reached_inside_a_scope("update_maker_order");
        }

        #[test]
        fn buy_and_sell_enter_the_scope() {
            for method in ["buy", "sell"] {
                let (ctx, probe) = setup();
                let body = json!({"method": method, "base": "DYN", "rel": "KMD", "price": "1", "volume": "1"});
                let result = if method == "buy" {
                    block_on(buy(ctx, body))
                } else {
                    block_on(sell(ctx, body))
                };
                assert!(result.is_err());
                probe.assert_reached_inside_a_scope(method);
            }
        }

        #[test]
        fn trade_preimage_enters_the_scope() {
            for method in ["setprice", "buy", "sell"] {
                let (ctx, probe) = setup();
                let req = json::from_value(
                    json!({"base": "DYN", "rel": "KMD", "swap_method": method, "price": "1", "volume": "1"}),
                )
                .unwrap();
                assert!(block_on(trade_preimage_rpc(ctx, req)).is_err());
                probe.assert_reached_inside_a_scope(&format!("trade_preimage {method}"));
            }
        }

        #[test]
        fn max_maker_vol_enters_the_scope() {
            let (ctx, probe) = setup();
            let req = json::from_value(json!({"coin": "DYN"})).unwrap();
            assert!(block_on(max_maker_vol(ctx, req)).is_err());
            probe.assert_reached_inside_a_scope("max_maker_vol");
        }

        #[test]
        fn max_taker_vol_enters_the_scope() {
            let (ctx, probe) = setup();
            assert!(block_on(max_taker_vol(ctx, json!({"coin": "DYN"}))).is_err());
            probe.assert_reached_inside_a_scope("max_taker_vol");

            let (ctx, probe) = setup();
            let req = json::from_value(json!({"coin": "DYN"})).unwrap();
            assert!(block_on(max_taker_vol_v2(ctx, req)).is_err());
            probe.assert_reached_inside_a_scope("max_taker_vol v2");
        }

        #[test]
        fn maker_balance_update_enters_the_scope() {
            let (ctx, probe) = setup();
            let handler = BalanceUpdateOrdermatchHandler::new(ctx.clone());
            let coin = MmCoinEnum::Test(TestCoin::new("DYN"));
            block_on(handler.balance_updated(&coin, &BigDecimal::from(1)));
            probe.assert_reached_inside_a_scope("balance update");
        }

        /// One `setprice`: the spend check, the sender trade fee of the base coin and the receiver trade fee of the
        /// rel coin all take their coin's reading through the request scope, as the coin layer does. Each coin is
        /// read once, whatever the number of consumers; a consumer outside any request reads afresh.
        #[test]
        fn setprice_takes_each_coin_reading_once() {
            use coins::request_fee_scope::{memoize, FeeReadKind};
            use coins::{CoinBalance, TradeFee};
            use std::collections::HashMap;
            use std::sync::Mutex;

            for kind in [
                FeeReadKind::UtxoFeeEstimate,
                FeeReadKind::UtxoRelayFee,
                FeeReadKind::EvmGasPrice,
            ] {
                let reads: Arc<Mutex<HashMap<String, usize>>> = Arc::default();
                let read = move |reads: Arc<Mutex<HashMap<String, usize>>>, ticker: String| async move {
                    let fetched = reads.clone();
                    let fetch_ticker = ticker.clone();
                    memoize::<u64, (), _>(kind, &ticker, async move {
                        *fetched.lock().unwrap().entry(fetch_ticker).or_default() += 1;
                        Ok(7)
                    })
                    .await
                    .unwrap();
                };

                TestCoin::min_trading_vol.mock_safe(|_| MockResult::Return(units_8(10_000)));
                TestCoin::min_tx_amount.mock_safe(|_| MockResult::Return(BigDecimal::from(0)));
                let r = reads.clone();
                TestCoin::spend_safe_amount.mock_safe(move |coin| {
                    let (reads, ticker) = (r.clone(), coin.ticker().to_owned());
                    MockResult::Return(Box::pin(async move {
                        read(reads, ticker).await;
                        Ok(None)
                    }))
                });
                let r = reads.clone();
                TestCoin::get_sender_trade_fee.mock_safe(move |coin, _, _| {
                    let (reads, ticker) = (r.clone(), coin.ticker().to_owned());
                    MockResult::Return(Box::pin(async move {
                        read(reads, ticker.clone()).await;
                        Ok(TradeFee {
                            coin: ticker,
                            amount: MmNumber::from(0),
                            paid_from_trading_vol: false,
                        })
                    }))
                });
                let r = reads.clone();
                TestCoin::get_receiver_trade_fee.mock_safe(move |coin, _| {
                    use futures::{FutureExt, TryFutureExt};
                    let (reads, ticker) = (r.clone(), coin.ticker().to_owned());
                    let fut = async move {
                        read(reads, ticker.clone()).await;
                        Ok(TradeFee {
                            coin: ticker,
                            amount: MmNumber::from(0),
                            paid_from_trading_vol: true,
                        })
                    };
                    MockResult::Return(Box::new(fut.boxed().compat()))
                });
                TestCoin::my_balance.mock_safe(|_| {
                    MockResult::Return(Box::new(futures01::future::ok(CoinBalance {
                        spendable: BigDecimal::from(100),
                        unspendable: BigDecimal::from(0),
                    })))
                });

                let ctx = ctx_with(json!({ "netid": mm2_net_config::SUPPORTED_NETIDS[0] }), &["DYN", "KMD"]);
                let req: SetPriceReq =
                    json::from_value(json!({"base": "DYN", "rel": "KMD", "price": "1", "volume": "1"})).unwrap();
                // The order is not stored (there is no database here); only the readings it took matter.
                let _ =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| block_on(create_maker_order(&ctx, req))));
                let taken = reads.lock().unwrap().clone();
                assert_eq!(taken.get("DYN"), Some(&1), "{kind:?}: {taken:?}");
                assert_eq!(taken.get("KMD"), Some(&1), "{kind:?}: {taken:?}");

                // Swap execution is outside any request: the same consumers read afresh.
                reads.lock().unwrap().clear();
                block_on(async {
                    read(reads.clone(), "DYN".to_owned()).await;
                    read(reads.clone(), "DYN".to_owned()).await;
                });
                assert_eq!(reads.lock().unwrap().get("DYN"), Some(&2), "{kind:?}");
            }
        }

        /// The scope ends with the request: nothing leaks into code that runs afterwards.
        #[test]
        fn no_scope_is_left_behind() {
            let (ctx, _probe) = setup();
            let req: SetPriceReq =
                json::from_value(json!({"base": "DYN", "rel": "KMD", "price": "1", "volume": "1"})).unwrap();
            let _ = block_on(create_maker_order(&ctx, req));
            assert!(!is_active());
        }
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
    let trie_store = TrieStore::default();
    let pair_liveness = trie_store.pair_liveness.clone();
    let mismatch_watch = trie_store.mismatch_watch.clone();
    let ctx = OrdermatchContext {
        maker_orders_ctx: Default::default(),
        my_taker_orders: Default::default(),
        orderbook: Default::default(),
        trie_store: Arc::new(parking_lot::Mutex::new(trie_store)),
        pair_liveness,
        mismatch_watch,
        trie_ops_tx,
        orderbook_subscriptions: Default::default(),
        sync_answer_deferral: Default::default(),
        sync_log: Default::default(),
        pending_maker_reserved: Default::default(),
        maker_outbox: Default::default(),
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
    assert_eq!(payload.unwrap().unwrap().get(&pair), Some(&old_root));
    assert!(sync_answer(&ctx, &pubkey, &pair, old_root).is_ok());

    // Cancel in the index while the worker is held back: no keep-alive, but the sync answer is
    // served from the store as it stands, without waiting (R-T3b items 2 and 3). The full snapshot
    // (request at the current trie root) leaves the cancelled order out; the delta (request at the
    // older root) ends at the trie's current root.
    index_remove_and_queue(&octx, cancelled);
    assert!(block_on(keep_alive_payload_for_pub(&octx, &pubkey, SHORT_WAIT_MS)).is_err());
    for root in [old_root, older_root] {
        let started = std::time::Instant::now();
        let mut res = sync_answer(&ctx, &pubkey, &pair, root)
            .expect("the answer never refuses")
            .unwrap();
        assert!(started.elapsed() < Duration::from_millis(SYNC_NO_WAIT_LIMIT_MS));
        let listed = sync_answer_order(&mut res, &pair, cancelled);
        assert!(listed.is_none(), "{:?}", listed);
    }

    // The worker catches up.
    drain_trie_queue(&octx, &mut rx);
    let roots = block_on(keep_alive_payload_for_pub(&octx, &pubkey, SHORT_WAIT_MS))
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
    // The full snapshot lists the changed order with the price the index holds (R-T3b item 2).
    let mut res = sync_answer(&ctx, &pubkey, &pair, old_root)
        .expect("the answer never refuses")
        .unwrap();
    assert_eq!(
        sync_answer_order(&mut res, &pair, changed.uuid),
        Some(Some(new_price.clone()))
    );

    drain_trie_queue(&octx, &mut rx);
    let roots = block_on(keep_alive_payload_for_pub(&octx, &pubkey, SHORT_WAIT_MS))
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
    let roots = block_on(keep_alive_payload_for_pub(&octx, &pubkey, TRIE_CATCH_UP_BOUND_MS))
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
        trie_store.prepare_sync_request_for_keep_alive(
            "other-pubkey",
            keep_alive_for(pair),
            i_am_relay,
            SyncRequestRoot::Held,
            |t| subscribed.contains(t),
        )
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

/// CRD ch.32 R-K2a, T-K1 to T-K4: one keep-alive per advertisable pair, carrying that pair's root
/// alone, on that pair's topic only.
mod keep_alive_shape_tests {
    use super::*;

    /// A context whose published messages land in the returned receiver.
    fn ctx_with_p2p() -> (MmArc, String, mpsc::Receiver<AdexBehaviourCmd>) {
        let (ctx, pubkey, _secret) = make_ctx_for_tests();
        let (tx, rx) = mpsc::channel(100);
        P2PContext::new(tx).store_to_mm_arc(&ctx);
        (ctx, pubkey, rx)
    }

    fn pairs(k: usize) -> Vec<(String, String)> { (0..k).map(|i| (format!("B{}", i), format!("R{}", i))).collect() }

    fn insert_orders(ctx: &MmArc, pubkey: &str, pairs: &[(String, String)]) -> Vec<OrderbookItem> {
        let mut all = Vec::new();
        for (base, rel) in pairs {
            for order in make_random_orders(pubkey.to_owned(), &[0; 32], base.clone(), rel.clone(), 2) {
                insert_or_update_order(ctx, order.clone());
                all.push(order);
            }
        }
        flush_trie(ctx);
        all
    }

    /// Every published keep-alive as (topic, signer, the single entry), checking the shape on the way.
    fn published_keep_alives(rx: &mut mpsc::Receiver<AdexBehaviourCmd>) -> Vec<(String, String, (String, H64))> {
        let mut out = Vec::new();
        while let Ok(cmd) = rx.try_recv() {
            let (topics, msg) = match cmd {
                AdexBehaviourCmd::PublishMsg { topics, msg, .. } => (topics, msg),
                AdexBehaviourCmd::PublishMsgFrom { topics, msg, .. } => (topics, msg),
                _ => continue,
            };
            let (message, _sig, signer) = decode_signed::<new_protocol::OrdermatchMessage>(&msg).unwrap();
            let keep_alive = match message {
                new_protocol::OrdermatchMessage::PubkeyKeepAlive(keep_alive) => keep_alive,
                other => panic!("unexpected message {:?}", other),
            };
            assert_eq!(topics.len(), 1, "one topic per keep-alive");
            assert_eq!(keep_alive.trie_roots.len(), 1, "one entry per keep-alive");
            let entry = keep_alive.trie_roots.into_iter().next().unwrap();
            assert_eq!(
                topics[0],
                orderbook_topic_from_ordered_pair(&entry.0),
                "published on its pair's topic"
            );
            out.push((topics[0].clone(), signer.to_hex(), entry));
        }
        out
    }

    fn held_roots(ctx: &MmArc, pubkey: &str) -> HashMap<AlbOrderedOrderbookPair, H64> {
        let octx = OrdermatchContext::from_ctx(ctx).unwrap();
        let trie_store = octx.trie_store.lock();
        trie_store.pubkeys_state[pubkey].trie_roots.clone()
    }

    /// T-K1, T-K2.
    #[test]
    fn one_single_entry_message_per_pair_on_its_own_topic() {
        let (ctx, pubkey, mut rx) = ctx_with_p2p();
        let octx = OrdermatchContext::from_ctx(&ctx).unwrap();
        insert_orders(&ctx, &pubkey, &pairs(3));

        block_on(broadcast_keep_alive_for_pub(&ctx, &octx, &pubkey, None));
        let published = published_keep_alives(&mut rx);
        assert_eq!(published.len(), 3);
        let held = held_roots(&ctx, &pubkey);
        let advertised: HashMap<_, _> = published
            .iter()
            .map(|(_, signer, (pair, root))| {
                assert_eq!(signer, &pubkey);
                (pair.clone(), *root)
            })
            .collect();
        assert_eq!(advertised, held);

        // A per-order pubkey publishes the same pattern, signed by its own key.
        let per_order = key_pair_from_seed("per-order key").unwrap();
        let per_order_pub = hex::encode(per_order.public_slice());
        insert_orders(&ctx, &per_order_pub, &pairs(2));
        block_on(broadcast_keep_alive_for_pub(
            &ctx,
            &octx,
            &per_order_pub,
            Some(&per_order),
        ));
        let published = published_keep_alives(&mut rx);
        assert_eq!(published.len(), 2);
        assert!(published.iter().all(|(_, signer, _)| signer == &per_order_pub));
    }

    /// T-K3.
    #[test]
    fn zero_and_empty_roots_are_not_advertised() {
        let (ctx, pubkey, mut rx) = ctx_with_p2p();
        let octx = OrdermatchContext::from_ctx(&ctx).unwrap();
        let orders = insert_orders(&ctx, &pubkey, &pairs(3));
        for order in orders.iter().filter(|o| o.base == "B0") {
            delete_order(&ctx, &pubkey, order.uuid);
        }
        flush_trie(&ctx);

        block_on(broadcast_keep_alive_for_pub(&ctx, &octx, &pubkey, None));
        let published = published_keep_alives(&mut rx);
        assert_eq!(published.len(), 2);
        assert!(published.iter().all(|(_, _, (pair, _))| pair != "B0:R0"));

        block_on(broadcast_keep_alive_for_pub(&ctx, &octx, "unknown-pubkey", None));
        assert!(published_keep_alives(&mut rx).is_empty());
    }

    /// T-K4: a receiver fed the per-pair messages ends where it ends when fed the aggregated form,
    /// with at most one single-pair request per message.
    #[test]
    fn receiver_handles_single_entry_messages_like_the_aggregated_form() {
        const MAKER: &str = "maker";
        let (m, ..) = make_ctx_for_tests();
        insert_orders(&m, MAKER, &pairs(3));
        let advertised = advertisable_roots(&held_roots(&m, MAKER));

        let receive = |messages: Vec<HashMap<AlbOrderedOrderbookPair, H64>>| {
            let (p, ..) = make_ctx_for_tests();
            let octx = OrdermatchContext::from_ctx(&p).unwrap();
            let mut requests = Vec::new();
            for trie_roots in messages {
                let request = octx.trie_store.lock().prepare_sync_request_for_keep_alive(
                    MAKER,
                    PubkeyKeepAlive {
                        trie_roots,
                        timestamp: 0,
                    },
                    true,
                    SyncRequestRoot::Held,
                    |_| true,
                );
                if let Some(OrdermatchRequest::SyncPubkeyOrderbookState { trie_roots, .. }) = request {
                    requests.push(trie_roots);
                }
            }
            for request in &requests {
                let answer = process_sync_pubkey_orderbook_state(m.clone(), MAKER.to_owned(), request.clone())
                    .unwrap()
                    .unwrap();
                let mut orderbook = octx.orderbook.lock();
                let mut ops = Vec::new();
                for (pair, diff) in answer.pair_orders_diff {
                    let params = ProcessTrieParams {
                        pubkey: MAKER,
                        alb_pair: &pair,
                        protocol_infos: &answer.protocol_infos,
                        conf_infos: &answer.conf_infos,
                    };
                    ops.extend(match diff {
                        DeltaOrFullTrie::Delta(delta) => process_trie_delta(&mut orderbook, delta, params),
                        DeltaOrFullTrie::FullTrie(values) => process_pubkey_full_trie(&mut orderbook, values, params),
                    });
                }
                orderbook.queue_trie_ops(&octx.trie_ops_tx, ops);
            }
            flush_trie(&p);
            let book: HashSet<Uuid> = octx.orderbook.lock().order_set.keys().copied().collect();
            (requests, held_roots(&p, MAKER), book)
        };

        let per_pair: Vec<_> = advertised
            .iter()
            .map(|(p, r)| HashMap::from([(p.clone(), *r)]))
            .collect();
        let (requests, roots, book) = receive(per_pair);
        assert_eq!(requests.len(), 3);
        assert!(requests.iter().all(|r| r.len() == 1));

        let (aggregated_requests, aggregated_roots, aggregated_book) = receive(vec![advertised.clone()]);
        assert_eq!(aggregated_requests.len(), 1);
        assert_eq!(roots, aggregated_roots);
        assert_eq!(roots, advertised);
        assert_eq!(book, aggregated_book);
        assert_eq!(book.len(), 6);
    }
}

/// CRD ch.32 R-T3b item 2 and R-T5: once the worker has caught up, an order the trie holds but the
/// index does not is a divergence and fails the answer; while its removal is still queued it is
/// left out.
#[test]
fn test_sync_answer_missing_value_fails_only_without_queued_events() {
    let (ctx, pubkey, secret) = make_ctx_for_tests();
    let (octx, mut rx) = ordermatch_ctx_without_worker();
    let octx2 = octx.clone();
    OrdermatchContext::from_ctx.mock_safe(move |_| MockResult::Return(Ok(octx2.clone())));
    let pair = alb_ordered_pair("C1", "C2");
    let orders = make_random_orders(pubkey.clone(), &secret, "C1".into(), "C2".into(), 2);
    let gone = orders[0].uuid;
    for o in orders {
        index_insert_and_queue(&octx, o);
    }
    drain_trie_queue(&octx, &mut rx);
    let root = pair_trie_root_by_pub_ctx(&octx, &pubkey, &pair);

    // Removed from the index with nothing queued: the stores have diverged.
    octx.orderbook.lock().index_remove(gone);
    let err = sync_answer(&ctx, &pubkey, &pair, root).expect_err("a divergence fails the answer");
    assert!(err.contains("GetterNoneForKeyFromTrie"), "{}", err);

    // The same state with an event still queued: the order is left out.
    let other = make_random_orders(pubkey.clone(), &secret, "C3".into(), "C4".into(), 1).remove(0);
    index_insert_and_queue(&octx, other);
    let mut res = sync_answer(&ctx, &pubkey, &pair, root).unwrap().unwrap();
    assert_eq!(sync_answer_order(&mut res, &pair, gone), None);
}

/// CRD ch.32 R-T3b item 4, T-T5: the keep-alive receiver records its receipt time, then compares with
/// a caught-up trie store, waiting for the worker within the bound; past the bound it compares with
/// the store as it stands.
mod keep_alive_receiver_catch_up_tests {
    use super::*;

    const PK: &str = "pubkey";

    fn keep_alive_for(pair: &str, root: H64) -> PubkeyKeepAlive {
        PubkeyKeepAlive {
            trie_roots: HashMap::from([(pair.to_owned(), root)]),
            timestamp: 0,
        }
    }

    /// A worker-less context holding `orders`, all applied to the trie, and then `extra` in the index
    /// only (its trie event queued).
    fn lagging_node(
        orders: &[OrderbookItem],
        extra: &OrderbookItem,
    ) -> (Arc<OrdermatchContext>, mpsc::UnboundedReceiver<Vec<TrieOp>>) {
        let (octx, mut rx) = ordermatch_ctx_without_worker();
        for o in orders.iter().cloned() {
            index_insert_and_queue(&octx, o);
        }
        drain_trie_queue(&octx, &mut rx);
        index_insert_and_queue(&octx, extra.clone());
        (octx, rx)
    }

    #[test]
    fn comparison_waits_for_the_trie_worker() {
        let orders = make_random_orders(PK.to_owned(), &[0; 32], "C1".into(), "C2".into(), 3);
        let extra = make_random_orders(PK.to_owned(), &[0; 32], "C1".into(), "C2".into(), 1).remove(0);
        // The maker's root, with the extra order applied.
        let (maker, mut maker_rx) = lagging_node(&orders, &extra);
        drain_trie_queue(&maker, &mut maker_rx);
        let advertised = pair_trie_root_by_pub_ctx(&maker, PK, "C1:C2");

        // The worker catches up within the bound: no request.
        let (peer, mut rx) = lagging_node(&orders, &extra);
        let worker_peer = peer.clone();
        let worker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            drain_trie_queue(&worker_peer, &mut rx);
        });
        let before = now_ms() / 1000;
        let request = block_on(keep_alive_sync_request(
            &peer,
            PK,
            keep_alive_for("C1:C2", advertised),
            true,
            SyncRequestRoot::Held,
            TRIE_CATCH_UP_BOUND_MS,
            0,
        ));
        worker.join().unwrap();
        assert!(request.is_none(), "the caught-up store matches the advertised root");
        assert!(peer.trie_store.lock().pubkeys_state[PK].last_keep_alive >= before);

        // The worker stays behind past the bound: the store as it stands is compared and a request
        // results.
        let (peer, _rx) = lagging_node(&orders, &extra);
        let request = block_on(keep_alive_sync_request(
            &peer,
            PK,
            keep_alive_for("C1:C2", advertised),
            true,
            SyncRequestRoot::Held,
            SHORT_WAIT_MS,
            0,
        ));
        assert!(matches!(
            request,
            Some((OrdermatchRequest::SyncPubkeyOrderbookState { .. }, None, _))
        ));
    }

    /// The receipt time is recorded before the wait, for a pubkey the node did not know yet: it is
    /// visible well before the call returns, not only when the roots are compared at its end.
    #[test]
    fn receipt_time_is_recorded_before_the_wait() {
        let orders = make_random_orders("other".to_owned(), &[0; 32], "C1".into(), "C2".into(), 1);
        let extra = make_random_orders("other".to_owned(), &[0; 32], "C1".into(), "C2".into(), 1).remove(0);
        let (peer, _rx) = lagging_node(&orders, &extra);
        let observer = peer.clone();
        let first_seen = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while std::time::Instant::now() < deadline {
                if observer.trie_store.lock().pubkeys_state.contains_key(PK) {
                    return Some(std::time::Instant::now());
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            None
        });
        let _ = block_on(keep_alive_sync_request(
            &peer,
            PK,
            keep_alive_for("C1:C2", [7; 8]),
            true,
            SyncRequestRoot::Held,
            TRIE_CATCH_UP_BOUND_MS,
            0,
        ));
        let returned = std::time::Instant::now();
        let seen = first_seen.join().unwrap().expect("the receipt is recorded");
        assert!(
            returned.duration_since(seen) >= Duration::from_millis(TRIE_CATCH_UP_BOUND_MS / 2),
            "recorded only {:?} before the call returned",
            returned.duration_since(seen)
        );
    }
}

/// CRD ch.59 R59.4.6, T11: an own maker-order update that changes nothing is not broadcast.
mod unchanged_order_update_tests {
    use super::*;
    use coins::{CoinsContext, MarketCoinOps, MmCoinEnum};

    fn maker_order() -> MakerOrder {
        let base = MmCoinEnum::Test(TestCoin::new("DYN"));
        let rel = MmCoinEnum::Test(TestCoin::new("KMD"));
        MakerOrderBuilder::new(&base, &rel)
            .with_max_base_vol(MmNumber::from("10"))
            .with_min_base_vol(Some(MmNumber::from("1")))
            .with_price(MmNumber::from("0.5"))
            .with_conf_settings(OrderConfirmationsSettings::default())
            .with_save_in_history(false)
            .build_unchecked()
    }

    #[test]
    fn every_carried_field_counts_and_amounts_compare_as_numbers() {
        let before = maker_order();

        let mut same = before.clone();
        same.price = MmNumber::from(BigRational::new_raw(2.into(), 4.into()));
        same.updated_at = Some(now_ms());
        assert!(!maker_order_update_changes_order(&before, &same));

        let mut price = before.clone();
        price.price = MmNumber::from("0.51");
        assert!(maker_order_update_changes_order(&before, &price));

        let mut max = before.clone();
        max.max_base_vol = MmNumber::from("11");
        assert!(maker_order_update_changes_order(&before, &max));

        let mut min = before.clone();
        min.min_base_vol = MmNumber::from("2");
        assert!(maker_order_update_changes_order(&before, &min));

        let mut confs = before.clone();
        confs.conf_settings = Some(OrderConfirmationsSettings {
            base_confs: 5,
            ..OrderConfirmationsSettings::default()
        });
        assert!(maker_order_update_changes_order(&before, &confs));
    }

    /// A node with the two test coins, the given extra configuration, an order directory in a fresh
    /// temporary directory, one own maker order and a channel that receives what it publishes.
    fn node_with_order(conf: Json) -> (MmArc, MakerOrder, mpsc::Receiver<AdexBehaviourCmd>, std::path::PathBuf) {
        TestCoin::min_trading_vol.mock_safe(|_| MockResult::Return(MmNumber::from("0.0001")));
        let dbdir = std::env::temp_dir().join(format!("kdf-noop-update-{}", Uuid::new_v4()));
        let mut conf = conf;
        conf["dbdir"] = json!(dbdir.to_string_lossy());
        conf["pre_trade_spend_check"] = json!(false);
        let ctx = MmCtxBuilder::new().with_conf(conf).into_mm_arc();
        ctx.secp256k1_key_pair
            .pin(key_pair_from_seed("passphrase").unwrap())
            .unwrap();
        let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();
        for ticker in ["DYN", "KMD"] {
            block_on(coins_ctx.add_coin(MmCoinEnum::Test(TestCoin::new(ticker)))).unwrap();
        }
        std::fs::create_dir_all(my_maker_orders_dir(&ctx)).unwrap();
        let (tx, rx) = mpsc::channel(100);
        P2PContext::new(tx).store_to_mm_arc(&ctx);

        let order = maker_order();
        OrdermatchContext::from_ctx(&ctx)
            .unwrap()
            .maker_orders_ctx
            .lock()
            .add_order(&order, Arc::new(AsyncMutex::new(order.clone())));
        (ctx, order, rx, dbdir)
    }

    fn update_price(ctx: &MmArc, uuid: Uuid, price: &'static str) {
        let req = MakerOrderUpdateReq {
            uuid,
            new_price: Some(MmNumber::from(price)),
            max: None,
            volume_delta: None,
            min_volume: None,
            base_confs: None,
            base_nota: None,
            rel_confs: None,
            rel_nota: None,
        };
        block_on(update_maker_order(ctx, req)).unwrap();
    }

    fn published_updates(rx: &mut mpsc::Receiver<AdexBehaviourCmd>) -> Vec<String> {
        let mut topics = Vec::new();
        while let Ok(cmd) = rx.try_recv() {
            if let AdexBehaviourCmd::PublishMsg { topics: t, msg, .. }
            | AdexBehaviourCmd::PublishMsgFrom { topics: t, msg, .. } = cmd
            {
                if let Ok((new_protocol::OrdermatchMessage::MakerOrderUpdated(_), ..)) =
                    decode_signed::<new_protocol::OrdermatchMessage>(&msg)
                {
                    topics.extend(t);
                }
            }
        }
        topics
    }

    #[test]
    fn t11_unchanged_update_is_saved_but_not_broadcast() {
        let (ctx, order, mut rx, dbdir) = node_with_order(json!({}));

        update_price(&ctx, order.uuid, "0.50");
        assert!(
            published_updates(&mut rx).is_empty(),
            "an unchanged update is not broadcast"
        );
        assert!(
            my_maker_order_file_path(&ctx, &order.uuid).exists(),
            "the order is still saved"
        );

        update_price(&ctx, order.uuid, "0.6");
        assert_eq!(published_updates(&mut rx), vec![order.orderbook_topic()]);

        std::fs::remove_dir_all(dbdir).ok();
    }

    #[test]
    fn t11_switch_off_broadcasts_every_update() {
        let (ctx, order, mut rx, dbdir) = node_with_order(json!({ "suppress_unchanged_order_updates": false }));

        update_price(&ctx, order.uuid, "0.5");
        assert_eq!(published_updates(&mut rx), vec![order.orderbook_topic()]);

        std::fs::remove_dir_all(dbdir).ok();
    }
}

/// Measurements of the keep-alive synchronization exchange between two of our own nodes (CRD ch.32
/// R-Q4, R-Q4a). `M` is a responder (the maker, or a relay that is fully
/// up to date with it), `P` a peer that received a keep-alive from `M`. Our receiver and responder
/// functions run unchanged; only the network hop is replaced by a direct call.
mod keep_alive_sync_measurement {
    use super::*;
    use std::collections::BTreeMap;

    const MAKER: &str = "021111111111111111111111111111111111111111111111111111111111111111";

    /// Deterministic xorshift generator, so every run measures the same sequence.
    struct Rng64(u64);

    impl Rng64 {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, n: u64) -> u64 { self.next() % n }
    }

    fn new_node() -> MmArc { make_ctx_for_tests().0 }

    fn pairs(k: usize) -> Vec<(String, String)> { (0..k).map(|i| (format!("B{}", i), format!("R{}", i))).collect() }

    fn seed_orders(k: usize, per_pair: usize) -> Vec<OrderbookItem> {
        pairs(k)
            .into_iter()
            .flat_map(|(base, rel)| make_random_orders(MAKER.to_owned(), &[0; 32], base, rel, per_pair))
            .collect()
    }

    fn insert_all(ctx: &MmArc, orders: &[OrderbookItem]) {
        for order in orders {
            insert_or_update_order(ctx, order.clone());
        }
        flush_trie(ctx);
    }

    fn price_update(rng: &mut Rng64, uuid: Uuid) -> MakerOrderUpdated {
        let mut msg = MakerOrderUpdated::new(uuid);
        msg.with_new_price(BigRational::new(
            (2000 + rng.below(10_000_000)).into(),
            1_000_000.into(),
        ));
        msg
    }

    /// The maker's order set as `P` should see it: uuid → (price, max, min).
    fn book(ctx: &MmArc) -> BTreeMap<Uuid, (BigRational, BigRational, BigRational)> {
        let octx = OrdermatchContext::from_ctx(ctx).unwrap();
        let orderbook = octx.orderbook.lock();
        orderbook
            .order_set
            .values()
            .filter(|o| o.pubkey == MAKER)
            .map(|o| (o.uuid, (o.price.clone(), o.max_volume.clone(), o.min_volume.clone())))
            .collect()
    }

    fn roots(ctx: &MmArc) -> HashMap<AlbOrderedOrderbookPair, H64> {
        flush_trie(ctx);
        let octx = OrdermatchContext::from_ctx(ctx).unwrap();
        let trie_store = octx.trie_store.lock();
        trie_store
            .pubkeys_state
            .get(MAKER)
            .map(|s| s.trie_roots.clone())
            .unwrap_or_default()
    }

    /// What `M` would advertise in its keep-alive right now.
    fn keep_alive(ctx: &MmArc) -> PubkeyKeepAlive {
        PubkeyKeepAlive {
            trie_roots: advertisable_roots(&roots(ctx)),
            timestamp: now_ms() / 1000,
        }
    }

    /// `P` processes one keep-alive copy and returns the request it would send, naming the root
    /// `request_root` selects. `P`'s worker is flushed first (R-T3b item 4).
    fn prepare_with(
        ctx: &MmArc,
        msg: PubkeyKeepAlive,
        request_root: SyncRequestRoot,
    ) -> Option<HashMap<AlbOrderedOrderbookPair, H64>> {
        flush_trie(ctx);
        let octx = OrdermatchContext::from_ctx(ctx).unwrap();
        let mut trie_store = octx.trie_store.lock();
        match trie_store.prepare_sync_request_for_keep_alive(MAKER, msg, true, request_root, |_| true)? {
            OrdermatchRequest::SyncPubkeyOrderbookState { trie_roots, .. } => Some(trie_roots),
            other => panic!("unexpected request {:?}", other),
        }
    }

    fn root_form(use_held_root: bool) -> SyncRequestRoot {
        if use_held_root {
            SyncRequestRoot::Held
        } else {
            SyncRequestRoot::Advertised
        }
    }

    /// `P` applies `M`'s answer exactly as `process_orders_keep_alive` does after the network hop.
    fn apply_answer(ctx: &MmArc, response: SyncPubkeyOrderbookStateRes) {
        let octx = OrdermatchContext::from_ctx(ctx).unwrap();
        {
            let mut orderbook = octx.orderbook.lock();
            let mut ops = Vec::new();
            for (pair, diff) in response.pair_orders_diff {
                let params = ProcessTrieParams {
                    pubkey: MAKER,
                    alb_pair: &pair,
                    protocol_infos: &response.protocol_infos,
                    conf_infos: &response.conf_infos,
                };
                let mut pair_ops = match diff {
                    DeltaOrFullTrie::Delta(delta) => process_trie_delta(&mut orderbook, delta, params),
                    DeltaOrFullTrie::FullTrie(values) => process_pubkey_full_trie(&mut orderbook, values, params),
                };
                ops.append(&mut pair_ops);
            }
            orderbook.queue_trie_ops(&octx.trie_ops_tx, ops);
        }
        flush_trie(ctx);
    }

    fn sync_once(m: &MmArc, p: &MmArc, request: HashMap<AlbOrderedOrderbookPair, H64>) {
        flush_trie(m);
        let response = process_sync_pubkey_orderbook_state(m.clone(), MAKER.to_owned(), request)
            .unwrap()
            .unwrap();
        apply_answer(p, response);
    }

    /// A peer that sees the maker for the first time (fresh start, new subscription, restarted seed)
    /// while the maker keeps updating: the keep-alive it received is already behind `M` when its
    /// request arrives.
    fn fresh_peer_round(use_held_root: bool) -> (bool, usize, usize) {
        let mut rng = Rng64(0x5eed_0001);
        let (m, p) = (new_node(), new_node());
        let orders = seed_orders(4, 5);
        insert_all(&m, &orders);

        let msg = keep_alive(&m);
        // Updates made by the maker while the keep-alive and the request travel.
        for _ in 0..3 {
            let uuid = orders[rng.below(orders.len() as u64) as usize].uuid;
            process_maker_order_updated(m.clone(), MAKER.to_owned(), price_update(&mut rng, uuid));
        }

        let request = prepare_with(&p, msg, root_form(use_held_root)).expect("a fresh peer must request");
        sync_once(&m, &p, request);
        (book(&p) == book(&m), book(&p).len(), book(&m).len())
    }

    #[test]
    fn fresh_peer_with_advertised_root_ends_with_a_partial_book() {
        let (converged, p_len, m_len) = fresh_peer_round(false);
        println!("advertised root: peer holds {} of {} orders", p_len, m_len);
        assert!(
            !converged,
            "the delta from the advertised root must not rebuild a fresh peer"
        );
        assert!(p_len < m_len);
    }

    #[test]
    fn fresh_peer_with_held_root_gets_the_full_book() {
        let (converged, p_len, m_len) = fresh_peer_round(true);
        println!("held root: peer holds {} of {} orders", p_len, m_len);
        assert!(converged);
    }

    /// The `[0;8]` path: a fresh relay asks with the advertised root, the maker has since cancelled
    /// one order, the delta only removes an order the relay never had, so the relay's root for the
    /// pair stays the zero placeholder while the exchange counts as a success (the keep-alive is
    /// forwarded). The next peer that asks the relay gets `InvalidStateRoot([0, 0, 0, 0, 0, 0, 0, 0])`.
    #[test]
    fn zero_root_survives_a_successful_sync_and_fails_the_next_requester() {
        let (m, p) = (new_node(), new_node());
        let orders = seed_orders(1, 3);
        insert_all(&m, &orders);
        let msg = keep_alive(&m);
        let pair = msg.trie_roots.keys().next().unwrap().clone();
        let advertised = msg.trie_roots[&pair];

        delete_order(&m, MAKER, orders[0].uuid);
        let request = prepare_with(&p, msg, SyncRequestRoot::Advertised).expect("a fresh relay must request");
        sync_once(&m, &p, request);

        assert_eq!(roots(&p).get(&pair), Some(&H64::default()), "zero placeholder survives");
        let next = HashMap::from_iter(iter::once((pair.clone(), advertised)));
        let err = process_sync_pubkey_orderbook_state(p.clone(), MAKER.to_owned(), next).unwrap_err();
        println!("downstream requester gets: {}", err);
        assert!(err.contains("InvalidStateRoot([0, 0, 0, 0, 0, 0, 0, 0])"), "{}", err);
    }

    /// The same exchange with the request this node sends: it names the zero root the relay holds,
    /// which is never a history key, so the answer is the full set and the relay ends at the maker's
    /// root and answers the next requester.
    #[test]
    fn held_root_request_leaves_no_zero_root_behind() {
        let (m, p) = (new_node(), new_node());
        let orders = seed_orders(1, 3);
        insert_all(&m, &orders);
        let msg = keep_alive(&m);
        let pair = msg.trie_roots.keys().next().unwrap().clone();

        delete_order(&m, MAKER, orders[0].uuid);
        let request = prepare_with(&p, msg, SyncRequestRoot::Held).expect("a fresh relay must request");
        assert_eq!(
            request.get(&pair),
            Some(&H64::default()),
            "the request names the held root"
        );
        sync_once(&m, &p, request);

        assert_eq!(roots(&p).get(&pair), roots(&m).get(&pair));
        assert_eq!(book(&p), book(&m));
        let next = HashMap::from_iter(iter::once((pair, H64::default())));
        assert!(process_sync_pubkey_orderbook_state(p.clone(), MAKER.to_owned(), next).is_ok());
    }

    /// The aggregated keep-alive shape Reloaded used to publish (one payload with every root, once per
    /// pair topic), measured: copies are processed concurrently (one task per gossip message), so each
    /// sees the same divergence (CRD ch.32 R-K2a divergence note).
    #[test]
    fn k_topic_copies_yield_k_requests_each_for_every_diverging_pair() {
        const K: usize = 6;
        let mut rng = Rng64(0x5eed_0003);
        let (m, p) = (new_node(), new_node());
        let orders = seed_orders(K, 3);
        insert_all(&m, &orders);
        insert_all(&p, &orders);
        // One price change per pair that `P` has not seen yet.
        for (base, rel) in pairs(K) {
            let uuid = orders
                .iter()
                .find(|o| o.base == base && o.rel == rel)
                .map(|o| o.uuid)
                .unwrap();
            process_maker_order_updated(m.clone(), MAKER.to_owned(), price_update(&mut rng, uuid));
        }

        let advertised = keep_alive(&m).trie_roots;
        let requests: Vec<_> = (0..K)
            .filter_map(|_| {
                prepare_with(
                    &p,
                    PubkeyKeepAlive {
                        trie_roots: advertised.clone(),
                        timestamp: 0,
                    },
                    SyncRequestRoot::Held,
                )
            })
            .collect();
        let pair_entries: usize = requests.iter().map(|r| r.len()).sum();
        println!(
            "K={} copies -> {} requests, {} pair entries in total",
            K,
            requests.len(),
            pair_entries
        );
        assert_eq!(requests.len(), K);
        assert_eq!(pair_entries, K * K);
    }

    /// Steady state over many keep-alive rounds. Per round the maker updates `per_round` orders; the
    /// last `reorder` of them reach `P` only after the keep-alive (they travel on another topic's
    /// mesh), the maker makes `in_flight` further updates between its keep-alive and `P`'s request,
    /// and `P` misses about one in `loss_one_in` of the updates made before the keep-alive for good
    /// (the divergence the keep-alive exists to repair; updates after it are always delivered). Returns the rounds in which `P`
    /// sent requests, the requests sent, and the rounds that ended with `P` still diverged.
    fn steady_state(
        use_held_root: bool,
        per_round: usize,
        reorder: usize,
        in_flight: usize,
        loss_one_in: u64,
    ) -> (usize, usize, usize) {
        const ROUNDS: usize = 20;
        const K: usize = 4;
        let mut rng = Rng64(0x5eed_0004);
        let (m, p) = (new_node(), new_node());
        let orders = seed_orders(K, 5);
        insert_all(&m, &orders);
        insert_all(&p, &orders);

        // The maker applies an update; it returns the message unless `P` loses it for good.
        let update = |rng: &mut Rng64, may_be_lost: bool| {
            let uuid = orders[rng.below(orders.len() as u64) as usize].uuid;
            let msg = price_update(rng, uuid);
            process_maker_order_updated(m.clone(), MAKER.to_owned(), msg.clone());
            if may_be_lost && rng.below(loss_one_in) == 0 {
                None
            } else {
                Some(msg)
            }
        };
        let deliver = |msg: MakerOrderUpdated| {
            process_maker_order_updated(p.clone(), MAKER.to_owned(), msg);
        };

        let (mut rounds_with_requests, mut requests_sent, mut rounds_diverged) = (0, 0, 0);
        for _ in 0..ROUNDS {
            let mut late = Vec::new();
            for i in 0..per_round {
                if let Some(msg) = update(&mut rng, true) {
                    if i + reorder >= per_round {
                        late.push(msg);
                    } else {
                        deliver(msg);
                    }
                }
            }
            let msg = keep_alive(&m);
            late.extend((0..in_flight).filter_map(|_| update(&mut rng, false)));

            let copies: Vec<_> = (0..K)
                .filter_map(|_| {
                    prepare_with(
                        &p,
                        PubkeyKeepAlive {
                            trie_roots: msg.trie_roots.clone(),
                            timestamp: 0,
                        },
                        root_form(use_held_root),
                    )
                })
                .collect();
            if !copies.is_empty() {
                rounds_with_requests += 1;
                requests_sent += copies.len();
            }
            for request in copies {
                sync_once(&m, &p, request);
            }
            late.into_iter().for_each(deliver);
            flush_trie(&p);
            if book(&p) != book(&m) {
                rounds_diverged += 1;
            }
        }
        (rounds_with_requests, requests_sent, rounds_diverged)
    }

    #[test]
    fn measure_steady_state_request_load() {
        const NONE: u64 = u64::MAX;
        println!("root       per_round reorder in_flight loss_1_in rounds_w_req requests diverged (20 rounds, K=4)");
        let mut results = HashMap::new();
        let cases = [
            (0, 0, 0, NONE),
            (5, 0, 0, NONE),
            (30, 5, 10, NONE),
            (30, 0, 10, 20),
            (30, 5, 10, 20),
        ];
        for &(per_round, reorder, in_flight, loss) in &cases {
            for &held in &[false, true] {
                let r = steady_state(held, per_round, reorder, in_flight, loss);
                println!(
                    "{:10} {:9} {:7} {:9} {:9} {:12} {:8} {:8}",
                    if held { "held" } else { "advertised" },
                    per_round,
                    reorder,
                    in_flight,
                    if loss == NONE { "-".to_owned() } else { loss.to_string() },
                    r.0,
                    r.1,
                    r.2
                );
                results.insert((held, per_round, reorder, in_flight, loss), r);
            }
        }
        // A quiet maker, and a maker whose updates all arrive before its keep-alive, cost nothing.
        assert_eq!(results[&(false, 0, 0, 0, NONE)], (0, 0, 0));
        assert_eq!(results[&(false, 5, 0, 0, NONE)], (0, 0, 0));
        // Updates that merely arrive after the keep-alive cost requests but no lasting divergence.
        assert_eq!(results[&(false, 30, 5, 10, NONE)].2, 0);
        // A lost update is repaired by the next keep-alive with the held root, but not with the
        // advertised root while the maker keeps moving.
        assert_eq!(results[&(true, 30, 0, 10, 20)].2, 0);
        assert_eq!(results[&(true, 30, 5, 10, 20)].2, 0);
        assert!(results[&(false, 30, 0, 10, 20)].2 > 0);
    }
}

/// CRD ch.32 R-Q4a, T-Q4a: the request names the held root by default, the advertised root with
/// `orderbook_sync_names_held_root` set to `false`.
mod sync_request_root_tests {
    use super::*;

    const PK: &str = "pubkey";

    fn request_roots(
        octx: &OrdermatchContext,
        request_root: SyncRequestRoot,
        entries: &[(&str, H64)],
    ) -> HashMap<AlbOrderedOrderbookPair, H64> {
        let keep_alive = PubkeyKeepAlive {
            trie_roots: entries.iter().map(|(pair, root)| ((*pair).to_owned(), *root)).collect(),
            timestamp: 0,
        };
        match octx
            .trie_store
            .lock()
            .prepare_sync_request_for_keep_alive(PK, keep_alive, true, request_root, |_| true)
        {
            Some(OrdermatchRequest::SyncPubkeyOrderbookState { trie_roots, .. }) => trie_roots,
            other => panic!("expected a request, got {:?}", other),
        }
    }

    #[test]
    fn held_root_by_default_and_zero_for_an_unsynchronized_pair() {
        let (octx, mut rx) = ordermatch_ctx_without_worker();
        for o in make_random_orders(PK.to_owned(), &[0; 32], "C1".into(), "C2".into(), 3) {
            index_insert_and_queue(&octx, o);
        }
        drain_trie_queue(&octx, &mut rx);
        let held = pair_trie_root_by_pub_ctx(&octx, PK, "C1:C2");

        let request = request_roots(&octx, SyncRequestRoot::Held, &[("C1:C2", [7; 8]), ("C3:C4", [9; 8])]);
        assert_eq!(request.get("C1:C2"), Some(&held));
        assert_eq!(request.get("C3:C4"), Some(&H64::default()));

        let request = request_roots(&octx, SyncRequestRoot::Advertised, &[
            ("C1:C2", [7; 8]),
            ("C3:C4", [9; 8]),
        ]);
        assert_eq!(request.get("C1:C2"), Some(&[7; 8]));
        assert_eq!(request.get("C3:C4"), Some(&[9; 8]));
    }

    #[test]
    fn the_switch_selects_the_root() {
        let ctx_with = |conf: Json| MmCtxBuilder::new().with_conf(conf).into_mm_arc();
        assert_eq!(SyncRequestRoot::configured(&ctx_with(json!({}))), SyncRequestRoot::Held);
        assert_eq!(
            SyncRequestRoot::configured(&ctx_with(json!({ "orderbook_sync_names_held_root": true }))),
            SyncRequestRoot::Held
        );
        assert_eq!(
            SyncRequestRoot::configured(&ctx_with(json!({ "orderbook_sync_names_held_root": "false" }))),
            SyncRequestRoot::Held
        );
        assert_eq!(
            SyncRequestRoot::configured(&ctx_with(json!({ "orderbook_sync_names_held_root": false }))),
            SyncRequestRoot::Advertised
        );
    }
}

/// The memory the trie store holds after a burst of order updates, a pair clear or a pubkey removal
/// is given back by the maintenance tick (`run_trie_store_maintenance`), without any further traffic.
mod trie_store_memory_retention {
    use super::*;
    use common::time_cache::SHRINK_FLOOR;
    use mm2_metrics::MetricsOps;

    const MAKER: &str = "021111111111111111111111111111111111111111111111111111111111111111";
    /// The test time to live of the histories (`TRIE_ORDER_HISTORY_TIMEOUT`, `TRIE_STATE_HISTORY_TIMEOUT`)
    /// plus a margin.
    const PAST_TTL: Duration = Duration::from_millis(3500);

    fn new_node() -> (MmArc, Arc<OrdermatchContext>) {
        let (ctx, _, _) = make_ctx_for_tests();
        let octx = OrdermatchContext::from_ctx(&ctx).unwrap();
        (ctx, octx)
    }

    /// The pair histories of `MAKER` live an hour instead of the test default, so that only the
    /// diffs of a pair history expire in the test.
    fn keep_pair_histories(octx: &OrdermatchContext) {
        octx.trie_store.lock().pubkeys_state.insert(
            MAKER.to_owned(),
            OrderbookPubkeyState::with_history_timeout(Duration::from_secs(3600)),
        );
    }

    fn seed(ctx: &MmArc, octx: &OrdermatchContext, n: usize) -> Vec<OrderbookItem> {
        let orders = make_random_orders(MAKER.to_owned(), &[0; 32], "RIN".into(), "KMD".into(), n);
        for order in &orders {
            insert_or_update_order(ctx, order.clone());
        }
        octx.wait_trie_ops_flushed();
        orders
    }

    /// `updates` price updates, round-robin over the orders, each one a different price.
    fn burst(ctx: &MmArc, octx: &OrdermatchContext, orders: &[OrderbookItem], updates: u64) {
        burst_from(ctx, octx, orders, 0, updates)
    }

    /// Like [`burst`], the prices start at `first_price_numer` (over one million).
    fn burst_from(
        ctx: &MmArc,
        octx: &OrdermatchContext,
        orders: &[OrderbookItem],
        first_price_numer: u64,
        updates: u64,
    ) {
        for i in 0..updates {
            let mut update = MakerOrderUpdated::new(orders[i as usize % orders.len()].uuid);
            update.with_new_price(BigRational::new(
                (2000 + first_price_numer + i).into(),
                1_000_000.into(),
            ));
            assert!(process_maker_order_updated(ctx.clone(), MAKER.to_owned(), update));
            if i % 500 == 499 {
                octx.wait_trie_ops_flushed();
            }
        }
        octx.wait_trie_ops_flushed();
    }

    fn pair() -> String { alb_ordered_pair("RIN", "KMD") }

    /// `(diffs in the pair history, its time-cache capacities, queued list length)`.
    fn inner_state(octx: &OrdermatchContext) -> (usize, (usize, usize), usize) {
        let trie_store = octx.trie_store.lock();
        let histories = &trie_store.pubkeys_state[MAKER].order_pairs_trie_state_history;
        let history = histories.get(&pair()).expect("the pair history exists");
        (
            history.inner.len(),
            history.inner.capacities(),
            history.inner.queued_len(),
        )
    }

    /// `(pair histories, their time-cache capacities, queued list length)` of the per-pubkey cache.
    fn outer_state(octx: &OrdermatchContext) -> (usize, (usize, usize), usize) {
        let trie_store = octx.trie_store.lock();
        let histories = &trie_store.pubkeys_state[MAKER].order_pairs_trie_state_history;
        (histories.len(), histories.capacities(), histories.queued_len())
    }

    fn assert_not_sparse(what: &str, len: usize, (map_capacity, list_capacity): (usize, usize)) {
        let bound = 2 * len + SHRINK_FLOOR;
        assert!(map_capacity <= bound, "{what}: map capacity {map_capacity} > {bound}");
        assert!(
            list_capacity <= bound,
            "{what}: list capacity {list_capacity} > {bound}"
        );
    }

    fn roots(octx: &OrdermatchContext) -> HashMap<AlbOrderedOrderbookPair, H64> {
        octx.wait_trie_ops_flushed();
        octx.trie_store.lock().pubkeys_state[MAKER].trie_roots.clone()
    }

    #[test]
    fn idle_pair_history_is_expired_and_shrunk_by_the_tick() {
        let (ctx, octx) = new_node();
        keep_pair_histories(&octx);
        let orders = seed(&ctx, &octx, 30);
        burst(&ctx, &octx, &orders, 20_000);

        let (diffs, ..) = inner_state(&octx);
        assert!(diffs > 2, "the burst left {diffs} diffs");
        let (pairs, _, queued) = outer_state(&octx);
        assert_eq!(
            (pairs, queued),
            (1, 1),
            "one list element per pair history after 20000 refreshes"
        );
        let bytes_before = octx.trie_store.lock().snapshot(true).memory_db_bytes.unwrap();

        std::thread::sleep(PAST_TTL);
        // Nothing wrote to the pair, so nothing has reclaimed the expired diffs yet.
        assert!(inner_state(&octx).0 > 2);

        run_trie_store_maintenance(&ctx, &octx, true);

        let (diffs, capacities, queued) = inner_state(&octx);
        assert!(diffs <= 2, "{diffs} diffs left");
        assert_eq!(queued, diffs, "no expired element is left in the list");
        assert_not_sparse("diffs", diffs, capacities);
        let (pairs, capacities, queued) = outer_state(&octx);
        assert_eq!((pairs, queued), (1, 1));
        assert_not_sparse("pair histories", pairs, capacities);

        // memory_db holds the live nodes of the 30 orders and nothing else: the same as a node that
        // only ever saw the final orders
        let (ref_ctx, ref_octx) = new_node();
        let final_orders: Vec<OrderbookItem> = octx.orderbook.lock().order_set.values().cloned().collect();
        assert_eq!(final_orders.len(), 30);
        for order in final_orders {
            insert_or_update_order(&ref_ctx, order);
        }
        ref_octx.wait_trie_ops_flushed();
        run_trie_store_maintenance(&ref_ctx, &ref_octx, true);
        assert_eq!(roots(&octx), roots(&ref_octx));

        let live = octx.trie_store.lock().snapshot(true);
        let reference = ref_octx.trie_store.lock().snapshot(true);
        let (live_bytes, reference_bytes) = (live.memory_db_bytes.unwrap(), reference.memory_db_bytes.unwrap());
        assert_eq!(live.memory_db_entries, reference.memory_db_entries);
        assert!(
            live_bytes <= 2 * reference_bytes,
            "memory_db holds {} B for {} live nodes, a fresh one {} B",
            live_bytes,
            live.memory_db_entries.unwrap(),
            reference_bytes
        );
        assert!(
            bytes_before > 2 * reference_bytes,
            "the burst did not fill memory_db ({bytes_before} B), the test shows nothing"
        );
    }

    #[test]
    fn idle_pair_history_cache_is_emptied_by_the_tick_when_its_own_time_is_up() {
        let (ctx, octx) = new_node();
        let orders = seed(&ctx, &octx, 30);
        burst(&ctx, &octx, &orders, 5_000);
        let (pairs, _, queued) = outer_state(&octx);
        assert_eq!((pairs, queued), (1, 1));

        std::thread::sleep(PAST_TTL);
        run_trie_store_maintenance(&ctx, &octx, false);

        let (pairs, capacities, queued) = outer_state(&octx);
        assert_eq!((pairs, queued), (0, 0));
        assert_not_sparse("pair histories", pairs, capacities);
        // the pair itself is still known
        assert!(octx.trie_store.lock().pubkeys_state[MAKER]
            .trie_roots
            .contains_key(&pair()));
    }

    /// A node that holds the answer to a sync request for `old_root` of the maker's pair.
    fn node_with_one_update() -> (MmArc, Arc<OrdermatchContext>, H64, Uuid) {
        let (ctx, octx) = new_node();
        keep_pair_histories(&octx);
        let orders = seed(&ctx, &octx, 5);
        let old_root = roots(&octx)[&pair()];
        burst(&ctx, &octx, &orders[..1], 1);
        assert_ne!(roots(&octx)[&pair()], old_root);
        (ctx, octx, old_root, orders[0].uuid)
    }

    fn answer(ctx: &MmArc, root: H64) -> DeltaOrFullTrie<Uuid, OrderbookP2PItem> {
        let request = HashMap::from([(pair(), root)]);
        let mut response = process_sync_pubkey_orderbook_state(ctx.clone(), MAKER.to_owned(), request)
            .unwrap()
            .unwrap();
        response.pair_orders_diff.remove(&pair()).unwrap()
    }

    #[test]
    fn the_tick_does_not_change_a_delta_answer_within_the_ttl_and_expired_roots_get_the_full_trie() {
        let (ctx, octx, old_root, updated) = node_with_one_update();
        // a second node whose expired diff is reclaimed lazily, by the next write to the pair
        let (lazy_ctx, lazy_octx, lazy_old_root, _) = node_with_one_update();

        let before = answer(&ctx, old_root);
        run_trie_store_maintenance(&ctx, &octx, true);
        let after = answer(&ctx, old_root);
        for delta in [before, after] {
            match delta {
                DeltaOrFullTrie::Delta(delta) => {
                    assert_eq!(delta.keys().collect::<Vec<_>>(), vec![&updated]);
                },
                DeltaOrFullTrie::FullTrie(_) => panic!("the diff is within its time to live"),
            }
        }

        std::thread::sleep(PAST_TTL);
        run_trie_store_maintenance(&ctx, &octx, true);
        match answer(&ctx, old_root) {
            DeltaOrFullTrie::FullTrie(orders) => assert_eq!(orders.len(), 5),
            DeltaOrFullTrie::Delta(_) => panic!("the diff expired and was reclaimed by the tick"),
        }

        // the same outcome as lazy expiry: the next write to the pair reclaims the expired diff
        let lazy_orders: Vec<OrderbookItem> = lazy_octx.orderbook.lock().order_set.values().cloned().collect();
        burst_from(&lazy_ctx, &lazy_octx, &lazy_orders[..1], 500_000, 1);
        match answer(&lazy_ctx, lazy_old_root) {
            DeltaOrFullTrie::FullTrie(orders) => assert_eq!(orders.len(), 5),
            DeltaOrFullTrie::Delta(_) => panic!("lazy expiry has no diff for the old root either"),
        }
    }

    fn gauges(ctx: &MmArc) -> HashMap<String, i64> {
        let json = ctx.metrics.collect_json().unwrap();
        json["metrics"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|metric| metric["type"] == "gauge")
            .map(|metric| {
                (
                    metric["key"].as_str().unwrap().to_owned(),
                    metric["value"].as_i64().unwrap(),
                )
            })
            .collect()
    }

    #[test]
    fn the_tick_reports_the_gauges() {
        let (ctx, octx) = new_node();
        keep_pair_histories(&octx);
        let orders = seed(&ctx, &octx, 10);
        // one update at a time: the worker merges the batches that wait for it (R-K5), which records
        // fewer deltas than the updates, and the test counts the deltas
        for i in 0..50 {
            burst_from(&ctx, &octx, &orders[i % 10..i % 10 + 1], i as u64, 1);
        }

        // A tick without purge reports the four per-tick gauges and leaves `memory_db` alone.
        run_trie_store_maintenance(&ctx, &octx, false);
        let reported = gauges(&ctx);
        assert_eq!(reported["orderbook.pubkeys"], 1);
        assert_eq!(reported["orderbook.history.pairs"], 1);
        // the 50 updates, and the 9 inserts after the first of the 10 orders (the first one has no previous root)
        assert_eq!(reported["orderbook.history.deltas"], 59);
        assert_eq!(reported["orderbook.trie_ops.pending"], 0);
        assert!(!reported.contains_key("orderbook.memory_db.entries"));
        assert!(!reported.contains_key("orderbook.memory_db"));
        let light = octx.trie_store.lock().snapshot(false);
        assert!(light.memory_db_entries.is_none());
        #[cfg(not(target_arch = "wasm32"))]
        assert!(light.memory_db_bytes.is_none());

        // A purge tick adds the two node-store gauges, measured after the purge.
        run_trie_store_maintenance(&ctx, &octx, true);
        let reported = gauges(&ctx);
        assert_eq!(reported["orderbook.pubkeys"], 1);
        assert_eq!(reported["orderbook.history.deltas"], 59);
        let snapshot = octx.trie_store.lock().snapshot(true);
        let entries = snapshot.memory_db_entries.unwrap();
        let bytes = snapshot.memory_db_bytes.unwrap();
        assert_eq!(reported["orderbook.memory_db.entries"], entries as i64);
        assert!(entries >= 10, "at least a leaf per order");
        assert_eq!(live_nodes(&mut octx.trie_store.lock()), entries);
        assert_eq!(reported["orderbook.memory_db"], bytes as i64);
        assert!(reported["orderbook.memory_db"] > 0);

        // batches queued and not applied yet
        collect_trie_store_metrics(&ctx, &snapshot, snapshot.applied_batches + 3);
        assert_eq!(gauges(&ctx)["orderbook.trie_ops.pending"], 3);

        // the sweep keeps reporting the length of the index
        collect_orderbook_metrics(&ctx, &octx.orderbook.lock());
        assert_eq!(gauges(&ctx)["orderbook.len"], 10);
    }

    // ---- ClearPair and RemovePubkey give the nodes of the pair trie back

    const PAIR: &str = "DOC:MARTY";

    fn item(item_pubkey: &str, uuid: Uuid, price_numer: u64) -> OrderbookItem {
        trie_op_item(item_pubkey, "DOC", "MARTY", uuid, price_numer)
    }

    fn insert_op(pubkey: &str, item_pubkey: &str, uuid: Uuid, price_numer: u64) -> TrieOp {
        TrieOp::Insert {
            pubkey: pubkey.to_owned(),
            alb_pair: PAIR.to_owned(),
            uuid,
            order: item(item_pubkey, uuid, price_numer),
        }
    }

    fn insert_all_ops(pubkey: &str, item_pubkey: &str, orders: u64, price_base: u64) -> Vec<TrieOp> {
        (0..orders)
            .map(|n| insert_op(pubkey, item_pubkey, trie_op_uuid(n), price_base + n))
            .collect()
    }

    /// Nodes some trie refers to; panics when a node has a negative count (a node removed twice).
    fn live_nodes(store: &mut TrieStore) -> usize {
        store.memory_db.purge();
        let nodes = store.memory_db.keys();
        assert!(
            nodes.values().all(|rc| *rc > 0),
            "a node has a negative reference count"
        );
        nodes.len()
    }

    fn read_back(store: &TrieStore, pubkey: &str) -> usize {
        let root = store.pubkeys_state[pubkey].trie_roots[PAIR];
        get_full_trie::<Uuid, OrderbookItem>(
            &root,
            &store.memory_db,
            |uuid| Some(item("X", *uuid, 1)),
            MissingValue::Fail,
        )
        .unwrap()
        .len()
    }

    #[test]
    fn full_resyncs_with_changed_data_leave_no_orphaned_nodes() {
        let mut store = TrieStore::default();
        store.apply_ops(insert_all_ops(MAKER, MAKER, 30, 1000));
        for round in 1..=20 {
            let mut ops = vec![trie_op_clear(MAKER)];
            ops.extend(insert_all_ops(MAKER, MAKER, 30, 1000 + round * 100));
            store.apply_ops(ops);
        }
        let held = live_nodes(&mut store);

        let mut fresh = TrieStore::default();
        fresh.apply_ops(insert_all_ops(MAKER, MAKER, 30, 1000 + 20 * 100));
        assert_eq!(
            store.pubkeys_state[MAKER].trie_roots,
            fresh.pubkeys_state[MAKER].trie_roots
        );
        assert_eq!(
            held,
            live_nodes(&mut fresh),
            "only the nodes of the last resync are held"
        );
        assert_eq!(read_back(&store, MAKER), 30);
    }

    #[test]
    fn clearing_a_pair_keeps_nodes_another_pubkey_shares() {
        // Both pubkeys carry the same orders, so their tries are the same nodes.
        let mut store = TrieStore::default();
        store.apply_ops(insert_all_ops("A", "X", 30, 1000));
        store.apply_ops(insert_all_ops("B", "X", 30, 1000));
        assert_eq!(store.pubkeys_state["A"].trie_roots, store.pubkeys_state["B"].trie_roots);
        let shared = live_nodes(&mut store);

        store.apply_ops(vec![TrieOp::ClearPair {
            pubkey: "A".to_owned(),
            alb_pair: PAIR.to_owned(),
        }]);
        assert_eq!(live_nodes(&mut store), shared, "B still holds every node");
        assert_eq!(read_back(&store, "B"), 30);

        // A comes back with the same orders: the tries are shared again
        store.apply_ops(insert_all_ops("A", "X", 30, 1000));
        assert_eq!(live_nodes(&mut store), shared);
        assert_eq!(read_back(&store, "A"), 30);

        store.apply_ops(vec![TrieOp::ClearPair {
            pubkey: "B".to_owned(),
            alb_pair: PAIR.to_owned(),
        }]);
        assert_eq!(live_nodes(&mut store), shared);
        assert_eq!(read_back(&store, "A"), 30);
        store.apply_ops(vec![TrieOp::ClearPair {
            pubkey: "A".to_owned(),
            alb_pair: PAIR.to_owned(),
        }]);
        assert_eq!(live_nodes(&mut store), 0, "nobody refers to a node any more");
    }

    #[test]
    fn removing_a_pubkey_gives_its_nodes_back_and_keeps_shared_ones() {
        let mut store = TrieStore::default();
        store.apply_ops(insert_all_ops("A", "X", 30, 1000));
        store.apply_ops(insert_all_ops("B", "X", 30, 1000));
        store.apply_ops(insert_all_ops("C", "Y", 12, 5000));
        let all = live_nodes(&mut store);

        // a bare RemovePubkey, without the removal of the orders first
        store.apply_ops(vec![TrieOp::RemovePubkey { pubkey: "C".to_owned() }]);
        let without_c = live_nodes(&mut store);
        assert!(without_c < all);

        store.apply_ops(vec![TrieOp::RemovePubkey { pubkey: "A".to_owned() }]);
        assert_eq!(live_nodes(&mut store), without_c, "B holds the nodes A shared");
        assert_eq!(read_back(&store, "B"), 30);

        store.apply_ops(vec![TrieOp::RemovePubkey { pubkey: "B".to_owned() }]);
        assert_eq!(live_nodes(&mut store), 0);
    }

    #[test]
    fn removing_a_pubkey_after_its_orders_were_removed_still_works() {
        let mut store = TrieStore::default();
        store.apply_ops(insert_all_ops("A", "X", 30, 1000));
        let mut ops: Vec<TrieOp> = (0..30)
            .map(|n| TrieOp::Remove {
                pubkey: "A".to_owned(),
                alb_pair: PAIR.to_owned(),
                uuid: trie_op_uuid(n),
            })
            .collect();
        ops.push(TrieOp::RemovePubkey { pubkey: "A".to_owned() });
        store.apply_ops(ops);
        assert!(store.pubkeys_state.is_empty());
        assert_eq!(live_nodes(&mut store), 0);
    }

    // ---- CRD ch.32 R-K5 (bulk drain, early purge) and R-K6 (cap on a pair's delta history): T-K5 to T-K7

    fn counters(ctx: &MmArc) -> HashMap<String, i64> {
        let json = ctx.metrics.collect_json().unwrap();
        json["metrics"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|metric| metric["type"] == "counter")
            .map(|metric| {
                (
                    metric["key"].as_str().unwrap().to_owned(),
                    metric["value"].as_i64().unwrap(),
                )
            })
            .collect()
    }

    fn slice_root(store: &TrieStore, pubkey: &str, pair: &str) -> H64 { store.pubkeys_state[pubkey].trie_roots[pair] }

    fn remove_op(pubkey: &str, pair: &str, uuid: Uuid) -> TrieOp {
        TrieOp::Remove {
            pubkey: pubkey.to_owned(),
            alb_pair: pair.to_owned(),
            uuid,
        }
    }

    /// Insertions and removals on 40 orders of one slice, from a seed.
    fn event_stream(seed: u64, events: usize) -> Vec<TrieOp> {
        let mut st = seed;
        (0..events)
            .map(|_| {
                let uuid = trie_op_uuid(trie_ops_lcg(&mut st) % 40 + 1);
                if trie_ops_lcg(&mut st).is_multiple_of(4) {
                    remove_op(MAKER, PAIR, uuid)
                } else {
                    insert_op(MAKER, MAKER, uuid, trie_ops_lcg(&mut st) % 100_000 + 1)
                }
            })
            .collect()
    }

    /// The stream of `event_stream(seed, events)` cut into batches of 1 to `max_len` events.
    fn event_batches(seed: u64, events: usize, max_len: u64) -> Vec<Vec<TrieOp>> {
        let mut st = seed ^ 0x5555;
        let mut stream = event_stream(seed, events).into_iter().peekable();
        let mut batches = Vec::new();
        while stream.peek().is_some() {
            let len = trie_ops_lcg(&mut st) % max_len + 1;
            batches.push(stream.by_ref().take(len as usize).collect());
        }
        batches
    }

    /// The orders a slice holds after the batches, as the read index would hold them.
    fn orders_after(batches: &[Vec<TrieOp>]) -> HashMap<Uuid, OrderbookItem> {
        let mut orders = HashMap::new();
        for op in batches.iter().flatten() {
            match op {
                TrieOp::Insert { uuid, order, .. } => {
                    orders.insert(*uuid, order.clone());
                },
                TrieOp::Remove { uuid, .. } => {
                    orders.remove(uuid);
                },
                _ => unreachable!("the streams hold insertions and removals only"),
            }
        }
        orders
    }

    /// What the store answers a requester that names `from` as its root of `pair`.
    fn history_answer(
        store: &TrieStore,
        pair: &str,
        from: H64,
        latest: &HashMap<Uuid, OrderbookItem>,
    ) -> DeltaOrFullTrie<Uuid, OrderbookItem> {
        let state = &store.pubkeys_state[MAKER];
        let history = state
            .order_pairs_trie_state_history
            .get(&pair.to_owned())
            .expect("the pair has a history");
        DeltaOrFullTrie::from_history(
            history,
            from,
            state.trie_roots[pair],
            &store.memory_db,
            |uuid| latest.get(uuid).cloned(),
            MissingValue::Fail,
        )
        .unwrap()
    }

    /// Apply an answer to the requester's store the way the receiver of a sync answer does. Returns
    /// whether it was the full snapshot.
    fn apply_answer(requester: &mut TrieStore, pair: &str, answer: DeltaOrFullTrie<Uuid, OrderbookItem>) -> bool {
        match answer {
            DeltaOrFullTrie::Delta(delta) => {
                for (uuid, change) in delta {
                    match change {
                        Some(order) => requester.apply_insert(MAKER, pair, uuid, order),
                        None => requester.apply_remove(MAKER, pair, uuid),
                    }
                }
                false
            },
            DeltaOrFullTrie::FullTrie(values) => {
                requester.apply_clear_pair(MAKER, pair);
                for (uuid, order) in values {
                    requester.apply_insert(MAKER, pair, uuid, order);
                }
                true
            },
        }
    }

    /// A store that applied the prelude and then the first `upto` batches one event at a time.
    fn one_by_one(prelude: Vec<TrieOp>, batches: Vec<Vec<TrieOp>>, upto: usize) -> TrieStore {
        let mut store = TrieStore::default();
        store.apply_batches(vec![prelude]);
        for batch in batches.into_iter().take(upto) {
            for op in batch {
                store.apply_ops(vec![op]);
            }
            store.applied_batches += 1;
        }
        store
    }

    /// T-K5: the worker applies a queue as a few large sequences, the outcome is that of one-by-one
    /// application and a requester whose root fell inside a merged sequence gets the full snapshot.
    #[test]
    fn t_k5_bulk_drain_equals_one_by_one() {
        for (what, seed, events, max_len) in [("small batches", 7, 3000, 3), ("large batches", 11, 6000, 400)] {
            let prelude = || insert_all_ops(MAKER, MAKER, 10, 1000);
            let batches = || event_batches(seed, events, max_len);
            let batch_count = batches().len();
            let latest = orders_after(&[prelude(), batches().into_iter().flatten().collect()]);

            let reference = one_by_one(prelude(), batches(), batch_count);

            // the root after every batch of the one-by-one run, the requesters' roots
            let mut boundaries = Vec::new();
            {
                let mut store = TrieStore::default();
                store.apply_batches(vec![prelude()]);
                boundaries.push(slice_root(&store, MAKER, PAIR));
                for batch in batches() {
                    for op in batch {
                        store.apply_ops(vec![op]);
                    }
                    boundaries.push(slice_root(&store, MAKER, PAIR));
                }
            }

            // the worker, with the whole queue waiting when it wakes up
            let mut start = TrieStore::default();
            start.apply_batches(vec![prelude()]);
            let merged = Arc::new(PaMutex::new(start));
            let (tx, rx) = mpsc::unbounded::<Vec<TrieOp>>();
            for batch in batches() {
                tx.unbounded_send(batch).unwrap();
            }
            drop(tx);
            let (ctx, ..) = make_ctx_for_tests();
            block_on(run_trie_store_worker(rx, merged.clone(), ctx.metrics.weak()));
            let merged = merged.lock();

            assert_trie_stores_equal(&merged, &reference, what);
            assert_eq!(
                merged.applied_batches,
                1 + batch_count as u64,
                "{what}: applied batches"
            );
            assert_eq!(merged.applied_batches, reference.applied_batches, "{what}");
            let final_root = slice_root(&merged, MAKER, PAIR);
            assert_eq!(final_root, *boundaries.last().unwrap());

            let deltas = |store: &TrieStore| store.snapshot(false).history_deltas;
            assert!(
                deltas(&merged) < deltas(&reference),
                "{what}: merging recorded no fewer deltas ({} against {})",
                deltas(&merged),
                deltas(&reference)
            );

            // Every root a requester can hold at a batch boundary is answered with a delta that
            // reaches the final root, or with the full snapshot.
            let step = (boundaries.len() / 30).max(1);
            let (mut deltas_given, mut fulls_given) = (0, 0);
            for k in (0..boundaries.len()).step_by(step) {
                let mut requester = one_by_one(prelude(), batches(), k);
                assert_eq!(
                    slice_root(&requester, MAKER, PAIR),
                    boundaries[k],
                    "{what}: requester {k}"
                );
                let answer = history_answer(&merged, PAIR, boundaries[k], &latest);
                if apply_answer(&mut requester, PAIR, answer) {
                    fulls_given += 1;
                } else {
                    deltas_given += 1;
                }
                assert_eq!(
                    slice_root(&requester, MAKER, PAIR),
                    final_root,
                    "{what}: requester {k} did not reach the advertised root"
                );
            }
            // negative control: roots inside the merged sequence are really gone from the history,
            // and the snapshot they get is correct (checked above)
            assert!(fulls_given > 0, "{what}: no root fell inside a merged sequence");
            assert!(deltas_given + fulls_given > 2);
        }
    }

    /// T-K5: bounds of one sequence, and the batch that does not fit starts the next one.
    #[test]
    fn t_k5_the_drain_takes_at_most_1024_batches_or_2048_events() {
        let op = |n: u64| insert_op(MAKER, MAKER, trie_op_uuid(n), 1000 + n);
        let take = |rx: &mut mpsc::UnboundedReceiver<Vec<TrieOp>>, carry: &mut Option<Vec<TrieOp>>| {
            let first = carry.take().unwrap_or_else(|| rx.try_recv().unwrap());
            take_waiting_batches(rx, first, carry)
        };

        // batches bound
        let (tx, mut rx) = mpsc::unbounded::<Vec<TrieOp>>();
        for n in 0..1030 {
            tx.unbounded_send(vec![op(n)]).unwrap();
        }
        let mut carry = None;
        let (batches, closed) = take(&mut rx, &mut carry);
        assert_eq!((batches.len(), closed, carry.is_none()), (1024, false, true));
        let (batches, closed) = take(&mut rx, &mut carry);
        assert_eq!((batches.len(), closed, carry.is_none()), (6, false, true));
        drop(tx);
        assert_eq!(rx.try_recv().err(), Some(TryRecvError::Closed));

        // events bound: 500 events a batch, 4 of them are 2,000 events, the 5th would pass 2,048
        assert_eq!((DRAIN_MAX_EVENTS, DRAIN_MAX_BATCHES), (2048, 1024));
        let (tx, mut rx) = mpsc::unbounded::<Vec<TrieOp>>();
        for b in 0..40 {
            tx.unbounded_send((0..500).map(|n| op(b * 500 + n)).collect()).unwrap();
        }
        let mut carry = None;
        let (batches, _) = take(&mut rx, &mut carry);
        assert_eq!(batches.len(), 4);
        assert!(carry.is_some(), "the 5th batch waits for the next sequence");
        let (batches, _) = take(&mut rx, &mut carry);
        assert_eq!(batches.len(), 4, "the carried batch is the first of the next sequence");
        assert!(carry.is_some());

        // a batch beyond the bound is applied alone
        let (tx2, mut rx2) = mpsc::unbounded::<Vec<TrieOp>>();
        tx2.unbounded_send((0..5_000).map(op).collect()).unwrap();
        tx2.unbounded_send(vec![op(1)]).unwrap();
        let mut carry = None;
        let (batches, _) = take(&mut rx2, &mut carry);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].len(), 5_000);
        let (batches, _) = take(&mut rx2, &mut carry);
        assert_eq!(batches.len(), 1);

        // the end of the queue is reported with the last sequence
        let (tx3, mut rx3) = mpsc::unbounded::<Vec<TrieOp>>();
        tx3.unbounded_send(vec![op(1)]).unwrap();
        tx3.unbounded_send(vec![op(2)]).unwrap();
        drop(tx3);
        let mut carry = None;
        let (batches, closed) = take(&mut rx3, &mut carry);
        assert_eq!((batches.len(), closed), (2, true));
        drop(tx);
    }

    /// T-K5 on a running node: the worker held back while the queue fills, then released; the book,
    /// the trie roots and the identifiers are those of one-by-one application, and "enqueued equals
    /// applied" means caught up.
    #[test]
    fn t_k5_a_released_worker_reaches_the_book_of_one_by_one_application() {
        let (ctx, octx) = new_node();
        let (ref_octx, mut ref_rx) = ordermatch_ctx_without_worker();

        let orders = make_random_orders(MAKER.to_owned(), &[0; 32], "RIN".into(), "KMD".into(), 30);
        let mut sequence = orders.clone();
        for i in 0..3000u64 {
            let mut order = orders[i as usize % orders.len()].clone();
            order.price = BigRational::new((2000 + i).into(), 1_000_000.into());
            sequence.push(order);
        }

        {
            // the worker waits for the lock while the queue fills
            let held = octx.trie_store.lock();
            for item in &sequence {
                insert_or_update_order(&ctx, item.clone());
            }
            assert!(octx.orderbook.lock().trie_ops_enqueued > held.applied_batches + 2000);
        }
        octx.wait_trie_ops_flushed();
        for item in &sequence {
            index_insert_and_queue(&ref_octx, item.clone());
        }
        drain_trie_queue(&ref_octx, &mut ref_rx);

        let (book, ref_book) = (octx.orderbook.lock(), ref_octx.orderbook.lock());
        assert_eq!(book.trie_ops_enqueued, octx.trie_store.lock().applied_batches);
        assert_eq!(book.order_set, ref_book.order_set);
        assert_eq!(book.trie_ops_enqueued, ref_book.trie_ops_enqueued);
        assert_trie_stores_equal(&octx.trie_store.lock(), &ref_octx.trie_store.lock(), "released worker");
        assert_eq!(
            octx.trie_store.lock().applied_batches,
            ref_octx.trie_store.lock().applied_batches
        );
    }

    /// The nodes `memory_db` holds, those nobody refers to any more included (they wait for a purge).
    fn stored_nodes(store: &TrieStore) -> usize { store.memory_db.clone().drain().len() }

    /// T-K6: the early purge after 100,000 leaf updates, at most every 5 s, never removing a live node.
    #[test]
    fn t_k6_early_purge_after_100000_leaf_updates() {
        const NOW: u64 = 1_000_000;
        let mut store = TrieStore::default();
        store.apply_batches(vec![insert_all_ops(MAKER, MAKER, 30, 1000)]);
        let mut model: HashMap<Uuid, u64> = (0..30).map(|n| (trie_op_uuid(n), 1000 + n)).collect();
        let mut updates = 0u64;
        // One update of one leaf, as the worker applies a queued batch of one event.
        fn update(store: &mut TrieStore, model: &mut HashMap<Uuid, u64>, updates: &mut u64, at_ms: u64) -> bool {
            let (uuid, price) = (trie_op_uuid(*updates % 30), 2000 + *updates);
            model.insert(uuid, price);
            *updates += 1;
            store.apply_batches(vec![vec![insert_op(MAKER, MAKER, uuid, price)]]);
            store.purge_if_loaded(at_ms)
        }

        let live = |store: &TrieStore| store.memory_db.keys().len();
        let mut purged = false;
        while !purged {
            purged = update(&mut store, &mut model, &mut updates, NOW);
            if !purged {
                assert_eq!(store.early_purges, 0);
            }
        }
        // 30 leaves were applied by the setup, the purge came with the 100,000th leaf update
        assert_eq!(updates, 100_000 - 30);
        assert_eq!(store.leaf_updates_since_purge, 0);
        assert_eq!(store.early_purges, 1);
        assert_eq!(
            stored_nodes(&store),
            live(&store),
            "only the live trie is held, not the replaced leaves"
        );
        let after_purge_bytes = parity_util_mem::malloc_size(&store.memory_db);
        store.memory_db.shrink_to_fit();
        assert!(
            parity_util_mem::malloc_size(&store.memory_db) < after_purge_bytes,
            "the early purge did not shrink the table"
        );

        // a second burst within 5 s: the threshold is reached again and no purge follows
        let held_by_the_live_trie = live(&store);
        for _ in 0..100_000 {
            assert!(!update(&mut store, &mut model, &mut updates, NOW + 4_999));
        }
        assert_eq!(store.early_purges, 1);
        assert!(store.leaf_updates_since_purge >= EARLY_PURGE_LEAF_UPDATES);
        assert!(
            stored_nodes(&store) > 10 * held_by_the_live_trie,
            "the burst left its replaced leaves in the store"
        );
        // 5 s after the last purge it is due
        assert!(!store.purge_if_loaded(NOW + 4_999));
        assert!(store.purge_if_loaded(NOW + 5_000));
        assert_eq!(store.early_purges, 2);
        assert_eq!(stored_nodes(&store), live(&store));

        // no node a live trie refers to was removed: the trie reads back in full and equals one
        // built from the final orders only
        let mut fresh = TrieStore::default();
        let final_orders: Vec<TrieOp> = model
            .iter()
            .map(|(uuid, price)| insert_op(MAKER, MAKER, *uuid, *price))
            .collect();
        fresh.apply_batches(vec![final_orders]);
        assert_eq!(slice_root(&store, MAKER, PAIR), slice_root(&fresh, MAKER, PAIR));
        assert_eq!(read_back(&store, MAKER), 30);
        assert_eq!(live(&store), live(&fresh));
    }

    /// The timer purge counts as a purge for the early one: its leaf updates are accounted for.
    #[test]
    fn t_k6_the_timer_purge_resets_the_early_purge_clock() {
        let mut store = TrieStore::default();
        store.apply_batches(vec![insert_all_ops(MAKER, MAKER, 30, 1000)]);
        assert_eq!(store.leaf_updates_since_purge, 30);
        store.purge_memory_db();
        assert_eq!(store.leaf_updates_since_purge, 0);
        store.leaf_updates_since_purge = EARLY_PURGE_LEAF_UPDATES;
        assert!(
            !store.purge_if_loaded(now_ms()),
            "the timer purge just ran, the early one has to wait"
        );
        assert!(store.purge_if_loaded(now_ms() + EARLY_PURGE_MIN_INTERVAL_MS));
        assert_eq!(store.early_purges, 1);
    }

    /// T-K6 and T-K7 through the worker: the counters.
    #[test]
    fn t_k6_t_k7_the_worker_reports_early_purges_and_evicted_deltas() {
        let (ctx, ..) = make_ctx_for_tests();
        let store = Arc::new(PaMutex::new(TrieStore::default()));
        let (tx, rx) = mpsc::unbounded::<Vec<TrieOp>>();
        let mut n = 0;
        // 110,000 new orders in 100 batches of 1,100: two of them pass the event bound, so a batch is a sequence
        for _ in 0..100 {
            let batch = (0..1100)
                .map(|_| {
                    n += 1;
                    insert_op(MAKER, MAKER, trie_op_uuid(n), 1000 + n)
                })
                .collect();
            tx.unbounded_send(batch).unwrap();
        }
        drop(tx);
        block_on(run_trie_store_worker(rx, store.clone(), ctx.metrics.weak()));

        let store = store.lock();
        assert_eq!(store.applied_batches, 100);
        assert_eq!(
            store.early_purges, 1,
            "the purge came with the 91st batch, 100,100 updates"
        );
        assert_eq!(counters(&ctx)["orderbook.trie_store.early_purges"], 1);
        assert!(
            store.history_evicted > 0,
            "110,000 new orders on one slice pass the cap"
        );
        assert_eq!(
            counters(&ctx)["orderbook.history.evicted"],
            store.history_evicted as i64
        );
        assert!(store.snapshot(false).history_deltas <= TRIE_DIFF_HISTORY_CAP);
    }

    /// T-K7: the cap on a pair's delta history.
    #[test]
    fn t_k7_history_cap() {
        const OTHER_PAIR: &str = "DOC:KMD";
        let other_insert = |uuid: Uuid, price: u64| TrieOp::Insert {
            pubkey: MAKER.to_owned(),
            alb_pair: OTHER_PAIR.to_owned(),
            uuid,
            order: trie_op_item(MAKER, "DOC", "KMD", uuid, price),
        };
        let one_hour = Duration::from_secs(3600);
        let prelude = || insert_all_ops(MAKER, MAKER, 5, 1000);
        let update = |i: u64| insert_op(MAKER, MAKER, trie_op_uuid(i % 5), 5000 + i);
        let build = |updates: u64| {
            let mut store = TrieStore::default();
            // the pair histories live an hour, and so do the deltas of the pairs: only the cap acts
            store
                .pubkeys_state
                .insert(MAKER.to_owned(), OrderbookPubkeyState::with_history_timeout(one_hour));
            store.apply_batches(vec![prelude()]);
            // one batch each: the second insert of the order would supersede the first in one batch
            store.apply_batches(vec![vec![other_insert(trie_op_uuid(0), 1)]]);
            store.apply_batches(vec![vec![other_insert(trie_op_uuid(0), 2)]]);
            for history in store
                .pubkeys_state
                .get_mut(MAKER)
                .unwrap()
                .order_pairs_trie_state_history
                .values_mut()
            {
                history.inner = TimeCache::new(one_hour);
            }
            store.history_evicted = 0;
            let mut roots = Vec::new();
            for i in 0..updates {
                roots.push(slice_root(&store, MAKER, PAIR));
                store.apply_batches(vec![vec![update(i)]]);
            }
            (store, roots)
        };
        let history_len = |store: &TrieStore, pair: &str| {
            store.pubkeys_state[MAKER]
                .order_pairs_trie_state_history
                .get(&pair.to_owned())
                .unwrap()
                .len()
        };

        let (mut store, roots) = build(3000);
        assert_eq!(history_len(&store, PAIR), TRIE_DIFF_HISTORY_CAP);
        assert_eq!(TRIE_DIFF_HISTORY_CAP, 2048);
        assert_eq!(store.history_evicted, 952);
        {
            let history = store.pubkeys_state[MAKER]
                .order_pairs_trie_state_history
                .get(&PAIR.to_owned())
                .unwrap();
            assert!(
                roots[..952].iter().all(|root| !history.contains_key(root)),
                "the oldest were dropped"
            );
            assert!(
                roots[952..].iter().all(|root| history.contains_key(root)),
                "the newest are kept"
            );
            // the memory held is that of 2,048 deltas: entries and room alike
            assert_eq!(history.inner.queued_len(), 2048);
            assert_not_sparse("history", history.inner.len(), history.inner.capacities());
        }
        let (exact, _) = build(2048);
        assert_eq!(exact.history_evicted, 0);
        assert_eq!(history_len(&exact, PAIR), 2048);

        // answers: the oldest root is gone, the others are served as deltas
        let final_root = slice_root(&store, MAKER, PAIR);
        let latest = orders_after(&[insert_all_ops(MAKER, MAKER, 5, 1000), (0..3000).map(update).collect()]);
        for (k, expect_full) in [(0, true), (951, true), (952, false), (1500, false), (2999, false)] {
            let mut requester = TrieStore::default();
            requester.apply_batches(vec![insert_all_ops(MAKER, MAKER, 5, 1000)]);
            requester.apply_batches((0..k).map(|i| vec![update(i)]).collect());
            assert_eq!(slice_root(&requester, MAKER, PAIR), roots[k as usize]);
            let answer = history_answer(&store, PAIR, roots[k as usize], &latest);
            assert_eq!(matches!(answer, DeltaOrFullTrie::FullTrie(_)), expect_full, "root {k}");
            apply_answer(&mut requester, PAIR, answer);
            assert_eq!(slice_root(&requester, MAKER, PAIR), final_root, "root {k}");
        }

        // another slice of the same pubkey is not touched by the cap of this one
        assert_eq!(history_len(&store, OTHER_PAIR), 0);
        for price in 3..=102 {
            store.apply_batches(vec![vec![other_insert(trie_op_uuid(0), price)]]);
        }
        assert_eq!(history_len(&store, OTHER_PAIR), 100);
        assert_eq!(store.history_evicted, 952);
        assert_eq!(history_len(&store, PAIR), 2048);
    }

    // ---- measurement harness of docs/plans/orderbook-queue-and-history-caps.md (ignored by default)
    //
    // cargo test --release -p mm2_main --lib --features counting-alloc measure_burst_retention -- --ignored --nocapture --test-threads=1
    //
    // The feature `counting-alloc` installs the counting allocator (`counting_alloc` below) as the
    // allocator of the whole test binary; without it the harness is not compiled and the tests run on
    // the system allocator.
    //
    // With KDF_HARNESS_LEGACY_WORKER set, the worker applies one queued batch at a time with no early
    // purge, the way it did before R-K5 (compile with the cap `TRIE_DIFF_HISTORY_CAP` raised to
    // `usize::MAX` for the full "before" picture of R-K6).

    /// The deltas live a minute instead of the production 300 s so that the run can wait them out.
    #[cfg(all(not(target_arch = "wasm32"), feature = "counting-alloc"))]
    const HARNESS_TTL: Duration = Duration::from_secs(60);

    #[cfg(all(not(target_arch = "wasm32"), feature = "counting-alloc"))]
    fn install_legacy_worker(ctx: &MmArc) {
        // The store is built first, so that `pair_liveness` is the one the store holds. The remaining fields come
        // from the test-only `Default`, so that a field added later cannot break this feature build again. That
        // `Default` also spawns a worker of its own on a channel of its own: the sender it made is dropped with the
        // fields this struct update does not take, which ends that worker at its first poll, and it never gets a
        // batch.
        let trie_store = TrieStore::default();
        let pair_liveness = trie_store.pair_liveness.clone();
        let trie_store = Arc::new(PaMutex::new(trie_store));
        let (trie_ops_tx, mut rx) = mpsc::unbounded::<Vec<TrieOp>>();
        let worker_store = trie_store.clone();
        spawn(async move {
            while let Some(ops) = rx.next().await {
                worker_store.lock().apply_batch(ops);
            }
        });
        let octx = OrdermatchContext {
            trie_store,
            pair_liveness,
            trie_ops_tx,
            ..Default::default()
        };
        from_ctx(&ctx.ordermatch_ctx, move || Ok(octx)).unwrap();
    }

    #[cfg(all(not(target_arch = "wasm32"), feature = "counting-alloc"))]
    fn mb(bytes: isize) -> f64 { bytes as f64 / 1_048_576. }

    #[cfg(all(not(target_arch = "wasm32"), feature = "counting-alloc"))]
    fn measure(legacy: bool, stalled: bool) {
        use super::counting_alloc;
        const UPDATES: u64 = 120_480;
        let (ctx, ..) = make_ctx_for_tests();
        if legacy {
            install_legacy_worker(&ctx);
        }
        let octx = OrdermatchContext::from_ctx(&ctx).unwrap();
        keep_pair_histories(&octx);
        let orders = seed(&ctx, &octx, 30);
        for history in octx
            .trie_store
            .lock()
            .pubkeys_state
            .get_mut(MAKER)
            .unwrap()
            .order_pairs_trie_state_history
            .values_mut()
        {
            history.inner = TimeCache::new(HARNESS_TTL);
        }

        let mut applied_hint = octx.trie_store.lock().applied_batches;
        let pending = |applied_hint: &mut u64| {
            let enqueued = octx.orderbook.lock().trie_ops_enqueued;
            if let Some(store) = octx.trie_store.try_lock() {
                *applied_hint = store.applied_batches;
            }
            enqueued.saturating_sub(*applied_hint)
        };

        let base = counting_alloc::start_peak();
        let started = std::time::Instant::now();
        let mut queue_peak = 0;
        let held = stalled.then(|| octx.trie_store.lock());
        for i in 0..UPDATES {
            let mut update = MakerOrderUpdated::new(orders[i as usize % orders.len()].uuid);
            update.with_new_price(BigRational::new((2000 + i).into(), 1_000_000.into()));
            assert!(process_maker_order_updated(ctx.clone(), MAKER.to_owned(), update));
            if i % 200 == 0 {
                queue_peak = queue_peak.max(pending(&mut applied_hint));
            }
        }
        let produced_in = started.elapsed();
        let queued_at_end = pending(&mut applied_hint);
        queue_peak = queue_peak.max(queued_at_end);
        let peak_producing = counting_alloc::peak() - base;
        drop(held);
        loop {
            let enqueued = octx.orderbook.lock().trie_ops_enqueued;
            if octx.trie_store.lock().applied_batches == enqueued {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let drained_in = started.elapsed();
        let peak_total = counting_alloc::peak() - base;
        let retained_after_drain = counting_alloc::current() - base;
        let (early_purges, evicted, deltas) = {
            let store = octx.trie_store.lock();
            (
                store.early_purges,
                store.history_evicted,
                store.snapshot(false).history_deltas,
            )
        };
        run_trie_store_maintenance(&ctx, &octx, true);
        let retained_after_tick = counting_alloc::current() - base;
        std::thread::sleep(HARNESS_TTL + Duration::from_secs(1));
        run_trie_store_maintenance(&ctx, &octx, true);
        let retained_after_ttl = counting_alloc::current() - base;
        println!(
            "HARNESS legacy_worker={legacy} worker_stalled_during_burst={stalled} updates={UPDATES} \
             produced_in={produced_in:?} drained_in={drained_in:?} queue_peak_batches={queue_peak} \
             queue_at_end_of_production={queued_at_end} peak_producing_MB={:.1} peak_total_MB={:.1} \
             retained_after_drain_MB={:.1} retained_after_tick_and_purge_MB={:.1} \
             retained_61s_later_after_tick_MB={:.1} early_purges={early_purges} history_evicted={evicted} \
             history_deltas_after_drain={deltas}",
            mb(peak_producing),
            mb(peak_total),
            mb(retained_after_drain),
            mb(retained_after_tick),
            mb(retained_after_ttl),
        );
    }

    #[test]
    #[cfg(all(not(target_arch = "wasm32"), feature = "counting-alloc"))]
    #[ignore = "measurement harness of docs/plans/orderbook-queue-and-history-caps.md, run alone"]
    fn measure_burst_retention() {
        let legacy = std::env::var("KDF_HARNESS_LEGACY_WORKER").is_ok();
        for stalled in [false, true] {
            measure(legacy, stalled);
        }
    }

    // ---- how long the worker holds the trie store lock (ignored harness)
    //
    // cargo test --release -p mm2_main --lib measure_lock_hold -- --ignored --nocapture --test-threads=1
    //
    // The readers that take the orderbook lock and then wait for the trie store lock wait as long as the
    // worker holds it, so the hold of one sequence (`apply_sequence`: apply, early purge if due, report)
    // is what the other users of the orderbook can be made to wait for.

    /// `events` insertions of new orders (distinct uuids from `first`), round-robin over `pubkeys` pubkeys and
    /// `pairs` pairs.
    fn new_orders(first: u64, events: u64, pubkeys: u64, pairs: u64) -> Vec<TrieOp> {
        (first..first + events)
            .map(|n| {
                let pubkey = format!("02{:064x}", n % pubkeys);
                let uuid = trie_op_uuid(n);
                TrieOp::Insert {
                    alb_pair: format!("P{}:KMD", (n / pubkeys) % pairs),
                    order: trie_op_item(&pubkey, "P", "KMD", uuid, 1000 + n),
                    pubkey,
                    uuid,
                }
            })
            .collect()
    }

    fn in_batches(ops: Vec<TrieOp>, batch_len: usize) -> Vec<Vec<TrieOp>> {
        let mut batches = Vec::new();
        let mut ops = ops.into_iter().peekable();
        while ops.peek().is_some() {
            batches.push(ops.by_ref().take(batch_len).collect());
        }
        batches
    }

    #[test]
    #[ignore = "measurement harness, run alone"]
    fn measure_lock_hold() {
        const PREFILL: u64 = 30_000;
        const PER_CONFIG: u64 = 65_536;
        let (ctx, ..) = make_ctx_for_tests();
        let metrics = ctx.metrics.weak();
        let ms = |since: std::time::Instant| since.elapsed().as_secs_f64() * 1000.;

        // (a) and (c): sequences of N events (batches of 16 events) on distinct new orders, over a store
        // that already holds 30,000 orders
        for (what, pubkeys, pairs) in [("1 pubkey x 1 pair", 1, 1), ("100 pubkeys x 25 pairs", 100, 25)] {
            for sequence_events in [16_384u64, 4_096, 2_048, 1_024] {
                let mut store = TrieStore::default();
                store.apply_batches(in_batches(new_orders(0, PREFILL, pubkeys, pairs), 16));
                store.leaf_updates_since_purge = 0;
                let mut holds = Vec::new();
                let mut reported = (0, 0);
                for k in 0..PER_CONFIG / sequence_events {
                    let first = PREFILL + k * sequence_events;
                    let batches = in_batches(new_orders(first, sequence_events, pubkeys, pairs), 16);
                    let started = std::time::Instant::now();
                    apply_sequence(&mut store, batches, &metrics, &mut reported);
                    holds.push(ms(started));
                }
                assert_eq!(store.early_purges, 0, "the purge would be part of the hold");
                holds.sort_by(|a, b| a.partial_cmp(b).unwrap());
                println!(
                    "LOCKHOLD apply_sequence {what}: events_per_sequence={sequence_events} sequences={} \
                     median_ms={:.1} p90_ms={:.1} max_ms={:.1}",
                    holds.len(),
                    holds[holds.len() / 2],
                    holds[holds.len() * 9 / 10],
                    holds[holds.len() - 1]
                );
            }
        }

        // (b): the early purge of a store holding 100,000 replaced leaves
        for live in [30u64, 30_000] {
            let mut store = TrieStore::default();
            store.apply_batches(in_batches(new_orders(0, live, 1, 1), 16));
            store.leaf_updates_since_purge = 0;
            for i in 0..EARLY_PURGE_LEAF_UPDATES {
                let uuid = trie_op_uuid(i % live);
                let op = TrieOp::Insert {
                    pubkey: format!("02{:064x}", 0),
                    alb_pair: "P0:KMD".to_owned(),
                    uuid,
                    order: trie_op_item(&format!("02{:064x}", 0), "P", "KMD", uuid, 10_000_000 + i),
                };
                store.apply_batches(vec![vec![op]]);
            }
            let stored_before = stored_nodes(&store);
            let started = std::time::Instant::now();
            assert!(store.purge_if_loaded(now_ms() + 10_000));
            println!(
                "LOCKHOLD early purge: live_orders={live} stored_nodes_before={stored_before} \
                 stored_nodes_after={} purge_ms={:.1}",
                stored_nodes(&store),
                ms(started)
            );
        }
    }
}

/// A counting `#[global_allocator]` for the ignored measurement harness of `trie_store_memory_retention`,
/// compiled only with the cargo feature `counting-alloc` so that normal test runs keep the system
/// allocator. It always counts the live bytes (one relaxed atomic add or subtract per call); the peak is followed
/// only while the harness asks for it.
#[cfg(all(not(target_arch = "wasm32"), feature = "counting-alloc"))]
mod counting_alloc {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

    static CURRENT: AtomicIsize = AtomicIsize::new(0);
    static PEAK: AtomicIsize = AtomicIsize::new(0);
    static FOLLOW_PEAK: AtomicBool = AtomicBool::new(false);

    struct CountingAlloc;

    fn add(n: usize) {
        let now = CURRENT.fetch_add(n as isize, Ordering::Relaxed) + n as isize;
        if FOLLOW_PEAK.load(Ordering::Relaxed) {
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
    }

    fn sub(n: usize) { CURRENT.fetch_sub(n as isize, Ordering::Relaxed); }

    unsafe impl GlobalAlloc for CountingAlloc {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let p = System.alloc(layout);
            if !p.is_null() {
                add(layout.size());
            }
            p
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            let p = System.alloc_zeroed(layout);
            if !p.is_null() {
                add(layout.size());
            }
            p
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            System.dealloc(ptr, layout);
            sub(layout.size());
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            let p = System.realloc(ptr, layout, new_size);
            if !p.is_null() {
                sub(layout.size());
                add(new_size);
            }
            p
        }
    }

    #[global_allocator]
    static ALLOCATOR: CountingAlloc = CountingAlloc;

    /// Live bytes now.
    pub(super) fn current() -> isize { CURRENT.load(Ordering::Relaxed) }

    /// Peak of the live bytes since [`start_peak`].
    pub(super) fn peak() -> isize { PEAK.load(Ordering::Relaxed) }

    /// Start following the peak from the live bytes now; returns them.
    pub(super) fn start_peak() -> isize {
        let now = current();
        PEAK.store(now, Ordering::Relaxed);
        FOLLOW_PEAK.store(true, Ordering::Relaxed);
        now
    }
}

/// CRD ch.60 T60.7 (R60.4.5, R60.4.6): the balance watch of the node's own maker orders. Time is driven by a
/// sleeper that the test passes to the watch task; it logs every pause and every balance read in one list, so
/// "sleep first, then one read per 30 s" is read off the order of the entries, and no test waits.
#[cfg(not(target_arch = "wasm32"))]
mod t60_7_maker_balance_watch_tests {
    use super::*;
    use crate::mm2::lp_ordermatch::maker_balance_watch::{run_maker_balance_watch, start_maker_balance_watch,
                                                         BalanceObservation, MAKER_BALANCE_WATCH_INTERVAL_SECS};
    use async_trait::async_trait;
    use coins::{register_balance_update_handler, BalanceError, CoinBalance, CoinsContext, MarketCoinOps, MmCoinEnum,
                TradeFee};
    use std::collections::VecDeque;
    use std::str::FromStr;

    fn bd(s: &str) -> BigDecimal { BigDecimal::from_str(s).unwrap() }

    /// The shared log and the scripted balance of the mocked coins.
    #[derive(Clone)]
    struct Probe {
        log: Arc<Mutex<Vec<String>>>,
        script: Arc<Mutex<VecDeque<Option<BigDecimal>>>>,
        steady: Arc<Mutex<BigDecimal>>,
    }

    impl Probe {
        /// Installs the coin mocks: `my_balance` answers the scripted readings (`None` is a failed read), then the
        /// steady balance; the fee mocks make `calc_max_maker_vol` a pure function of the balance.
        fn install(steady: &str) -> Probe {
            let probe = Probe {
                log: Arc::new(Mutex::new(Vec::new())),
                script: Arc::new(Mutex::new(VecDeque::new())),
                steady: Arc::new(Mutex::new(bd(steady))),
            };
            let reader = probe.clone();
            TestCoin::my_balance.mock_safe(move |coin| {
                let ticker = coin.ticker().to_owned();
                reader.log.lock().unwrap().push(format!("read {}", ticker));
                let reading = match reader.script.lock().unwrap().pop_front() {
                    Some(scripted) => scripted,
                    None => Some(reader.steady.lock().unwrap().clone()),
                };
                let result = match reading {
                    Some(spendable) => Ok(CoinBalance::new(spendable)),
                    None => MmError::err(BalanceError::Transport("down".to_owned())),
                };
                MockResult::Return(Box::new(futures01::future::result(result)))
            });
            TestCoin::get_sender_trade_fee.mock_safe(|coin, _value, _stage| {
                let fee = TradeFee {
                    coin: coin.ticker().to_owned(),
                    amount: MmNumber::from("0.1"),
                    paid_from_trading_vol: false,
                };
                MockResult::Return(Box::pin(async move { Ok(fee) }))
            });
            TestCoin::get_receiver_trade_fee.mock_safe(|coin, _stage| {
                let fee = TradeFee {
                    coin: coin.ticker().to_owned(),
                    amount: MmNumber::from("0.1"),
                    paid_from_trading_vol: false,
                };
                MockResult::Return(Box::new(futures01::future::ok(fee)))
            });
            TestCoin::min_tx_amount.mock_safe(|_| MockResult::Return(bd("0")));
            probe
        }

        fn script(&self, readings: &[Option<&str>]) {
            *self.script.lock().unwrap() = readings.iter().map(|r| r.map(bd)).collect();
        }

        fn entries(&self) -> Vec<String> { self.log.lock().unwrap().clone() }

        fn count(&self, entry: &str) -> usize { self.entries().iter().filter(|e| *e == entry).count() }
    }

    /// A balance handler that only records the recomputations it is asked for.
    struct Recorder(Probe);

    #[async_trait]
    impl coins::BalanceTradeFeeUpdatedHandler for Recorder {
        async fn balance_updated(&self, coin: &MmCoinEnum, new_balance: &BigDecimal) {
            self.0
                .log
                .lock()
                .unwrap()
                .push(format!("recompute {} {}", coin.ticker(), new_balance));
        }
    }

    fn ctx_with_coins(probe: &Probe) -> MmArc {
        // An unrelated configuration member must not change the interval (T60.7 (a)).
        let ctx = MmCtxBuilder::new()
            .with_conf(json!({
                "coins": [{ "coin": "BASE" }, { "coin": "REL" }, { "coin": "OTHER" }],
                "balance_watch_interval": 1,
                "balance_update_interval": 1
            }))
            .with_secp256k1_key_pair(key_pair_from_seed("passphrase").unwrap())
            .into_mm_arc();
        let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();
        for ticker in ["BASE", "REL", "OTHER"] {
            block_on(coins_ctx.add_coin(MmCoinEnum::Test(TestCoin::new(ticker)))).unwrap();
        }
        block_on(register_balance_update_handler(
            ctx.clone(),
            Box::new(Recorder(probe.clone())),
        ));
        ctx
    }

    fn own_order(base: &str, rel: &str, max: &str, min: &str) -> MakerOrder {
        MakerOrder {
            uuid: Uuid::new_v4(),
            base: base.into(),
            rel: rel.into(),
            created_at: now_ms(),
            updated_at: Some(now_ms()),
            matches: HashMap::new(),
            max_base_vol: MmNumber::from(bd(max)),
            min_base_vol: MmNumber::from(bd(min)),
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

    fn store(ctx: &MmArc, order: &MakerOrder) {
        let ordermatch_ctx = OrdermatchContext::from_ctx(ctx).unwrap();
        let order_arc = Arc::new(AsyncMutex::new(order.clone()));
        ordermatch_ctx.maker_orders_ctx.lock().add_order(order, order_arc);
    }

    fn remove(ctx: &MmArc, order: &MakerOrder) {
        let ordermatch_ctx = OrdermatchContext::from_ctx(ctx).unwrap();
        ordermatch_ctx.maker_orders_ctx.lock().remove_order(&order.uuid);
    }

    /// Registers the watch of `ticker` the way the order paths do, without spawning its task.
    fn begin(ctx: &MmArc, ticker: &str, seed: Option<&str>) -> Option<u64> {
        let ordermatch_ctx = OrdermatchContext::from_ctx(ctx).unwrap();
        let generation = ordermatch_ctx
            .maker_orders_ctx
            .lock()
            .begin_balance_watch(ticker, seed.map(bd));
        generation
    }

    /// Runs the watch task of `ticker` to its end. Every pause is logged and numbered from 1; `on_sleep` may
    /// change the world at that point (move the balance, remove an order).
    fn drive(ctx: &MmArc, probe: &Probe, ticker: &str, generation: u64, on_sleep: impl Fn(usize)) {
        let calls = std::cell::Cell::new(0usize);
        let log = probe.log.clone();
        let sleeper = |secs: f64| {
            calls.set(calls.get() + 1);
            log.lock().unwrap().push(format!("sleep {}", secs));
            on_sleep(calls.get());
            std::future::ready(())
        };
        block_on(run_maker_balance_watch(
            ctx.weak(),
            ticker.to_owned(),
            generation,
            sleeper,
        ));
    }

    /// (a) With one maker order on base coin BASE and a balance that does not change, exactly one balance read of
    /// BASE occurs per 30 s and none within the first 30 s after the watch starts; the interval is unaffected by
    /// any configuration member.
    #[test]
    fn t60_7_a_one_read_per_thirty_seconds_sleep_first() {
        let probe = Probe::install("10");
        let ctx = ctx_with_coins(&probe);
        let order = own_order("BASE", "REL", "5", "1");
        store(&ctx, &order);
        let generation = begin(&ctx, "BASE", Some("10")).expect("the first base order starts the watch");

        drive(&ctx, &probe, "BASE", generation, |n| {
            if n == 4 {
                remove(&ctx, &order);
            }
        });

        assert_eq!(MAKER_BALANCE_WATCH_INTERVAL_SECS, 30.);
        // Sleep first; one read after each of the first three pauses; the order is gone after the fourth pause, so
        // the poll that follows it reads nothing and the task ends. The balance never moved, so no recomputation.
        assert_eq!(probe.entries(), vec![
            "sleep 30",
            "read BASE",
            "sleep 30",
            "read BASE",
            "sleep 30",
            "read BASE",
            "sleep 30"
        ]);
    }

    /// (b) A watch seeded at creation with balance b performs no recomputation on a first poll reading b, and
    /// exactly one on a first poll reading b' != b.
    #[test]
    fn t60_7_b_seeded_watch_recomputes_only_when_the_balance_moved() {
        // First poll reads the seed.
        let probe = Probe::install("10");
        let ctx = ctx_with_coins(&probe);
        let order = own_order("BASE", "REL", "5", "1");
        store(&ctx, &order);
        let generation = begin(&ctx, "BASE", Some("10")).unwrap();
        drive(&ctx, &probe, "BASE", generation, |n| {
            if n == 2 {
                remove(&ctx, &order);
            }
        });
        assert_eq!(probe.entries(), vec!["sleep 30", "read BASE", "sleep 30"]);

        // First poll reads another value: exactly one recomputation, then none while it stays.
        let probe = Probe::install("11");
        let ctx = ctx_with_coins(&probe);
        let order = own_order("BASE", "REL", "5", "1");
        store(&ctx, &order);
        let generation = begin(&ctx, "BASE", Some("10")).unwrap();
        drive(&ctx, &probe, "BASE", generation, |n| {
            if n == 3 {
                remove(&ctx, &order);
            }
        });
        assert_eq!(probe.entries(), vec![
            "sleep 30",
            "read BASE",
            "recompute BASE 11",
            "sleep 30",
            "read BASE",
            "sleep 30"
        ]);
    }

    /// (c), the watch half: a restored order starts the watch without a seed and without a read; its first poll
    /// at +30 s runs one recomputation although the balance equals the maintenance pass's reading, and that
    /// recomputation (the real order-matching handler) changes no order and sends no message.
    #[test]
    fn t60_7_c_restored_order_unseeded_watch_recomputes_once_without_effect() {
        let dbdir = std::env::temp_dir().join(format!("kdf-balance-watch-restart-{}", Uuid::new_v4()));
        let probe = Probe::install("20");
        let ctx = MmCtxBuilder::new()
            .with_conf(json!({
                "coins": [{ "coin": "BASE" }, { "coin": "REL" }],
                "dbdir": dbdir.to_str().unwrap()
            }))
            .with_secp256k1_key_pair(key_pair_from_seed("passphrase").unwrap())
            .into_mm_arc();
        let (tx, mut p2p_rx) = mpsc::channel(100);
        P2PContext::new(tx).store_to_mm_arc(&ctx);
        std::fs::create_dir_all(my_maker_orders_dir(&ctx)).unwrap();
        std::fs::create_dir_all(my_taker_orders_dir(&ctx)).unwrap();
        let order = own_order("BASE", "REL", "10", "1");
        block_on(save_my_new_maker_order(ctx.clone(), &order)).unwrap();

        // The restart: the order is loaded, the watch exists without a seed, and no balance was read.
        block_on(orders_kick_start(&ctx)).unwrap();
        let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
        let generation = {
            let maker_orders = ordermatch_ctx.maker_orders_ctx.lock();
            assert!(!maker_orders.has_balance_watch("REL"));
            assert_eq!(maker_orders.balance_watch_last_seen("BASE"), None);
            maker_orders
                .balance_watch_generation("BASE")
                .expect("the restored order started the watch")
        };
        assert_eq!(probe.entries(), Vec::<String>::new());

        // The coins are activated and the first maintenance pass runs: one read, the order is re-announced.
        let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();
        for ticker in ["BASE", "REL"] {
            block_on(coins_ctx.add_coin(MmCoinEnum::Test(TestCoin::new(ticker)))).unwrap();
        }
        block_on(register_balance_update_handler(
            ctx.clone(),
            Box::new(Recorder(probe.clone())),
        ));
        block_on(register_balance_update_handler(
            ctx.clone(),
            Box::new(BalanceUpdateOrdermatchHandler::new(ctx.clone())),
        ));
        block_on(reconcile_missing_own_maker_orders(&ctx, &ordermatch_ctx));
        assert_eq!(probe.entries(), vec!["read BASE"]);
        let mut announced = 0;
        while let Ok(cmd) = p2p_rx.try_recv() {
            if matches!(
                cmd,
                AdexBehaviourCmd::PublishMsg { .. } | AdexBehaviourCmd::PublishMsgFrom { .. }
            ) {
                announced += 1;
            }
        }
        assert!(announced >= 1, "the restored order is re-announced");
        let max_after_pass = block_on(
            ordermatch_ctx
                .maker_orders_ctx
                .lock()
                .get_order(&order.uuid)
                .unwrap()
                .lock(),
        )
        .max_base_vol
        .clone();

        // The watch's first poll, 30 s after the start: the balance equals the pass's reading, yet one recomputation.
        drive(&ctx, &probe, "BASE", generation, |n| {
            if n == 2 {
                remove(&ctx, &order);
            }
        });
        assert_eq!(probe.entries(), vec![
            "read BASE",
            "sleep 30",
            "read BASE",
            "recompute BASE 20",
            "sleep 30"
        ]);
        // The real handler ran inside that recomputation and found nothing to lower: nothing was sent.
        assert!(
            p2p_rx.try_recv().is_err(),
            "the first poll's recomputation sends no message"
        );
        assert_eq!(max_after_pass, MmNumber::from("10"));
        std::fs::remove_dir_all(&dbdir).ok();
    }

    /// (d) A poll whose read fails leaves the last-seen value and every order untouched, and the next poll occurs
    /// 30 s later.
    #[test]
    fn t60_7_d_failed_read_changes_nothing() {
        let probe = Probe::install("12");
        let ctx = ctx_with_coins(&probe);
        let order = own_order("BASE", "REL", "5", "1");
        store(&ctx, &order);
        let generation = begin(&ctx, "BASE", Some("10")).unwrap();
        // Reads: 11 (moved), failure, failure, 11 (the kept last-seen value), then the steady 12.
        probe.script(&[Some("11"), None, None, Some("11")]);
        drive(&ctx, &probe, "BASE", generation, |n| {
            if n == 6 {
                remove(&ctx, &order);
            }
        });
        assert_eq!(probe.entries(), vec![
            "sleep 30",
            "read BASE",
            "recompute BASE 11",
            "sleep 30",
            "read BASE", // fails: nothing recomputed, last-seen stays 11
            "sleep 30",
            "read BASE", // fails again
            "sleep 30",
            "read BASE", // 11 equals the last-seen value: nothing recomputed
            "sleep 30",
            "read BASE",
            "recompute BASE 12",
            "sleep 30",
        ]);
    }

    /// (e) A restored order whose live balance supports less than its available amount is announced with its
    /// maximum volume lowered to the reserved amount plus the supported maximum; one whose supported maximum is
    /// below its minimum volume is cancelled with the insufficient-balance reason and not announced.
    #[test]
    fn t60_7_e_restored_order_is_reconciled_with_the_live_balance() {
        let probe = Probe::install("5");
        let ctx = ctx_with_coins(&probe);
        let (tx, mut p2p_rx) = mpsc::channel(100);
        P2PContext::new(tx).store_to_mm_arc(&ctx);
        let cancelled = Arc::new(Mutex::new(Vec::<String>::new()));
        let cancelled_log = cancelled.clone();
        delete_my_maker_order.mock_safe(move |_, order, reason| {
            cancelled_log.lock().unwrap().push(format!("{} {}", order.uuid, reason));
            MockResult::Return(Box::new(futures01::future::ok(())))
        });
        let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
        let published = |rx: &mut mpsc::Receiver<AdexBehaviourCmd>| {
            let mut n = 0;
            while let Ok(cmd) = rx.try_recv() {
                if matches!(
                    cmd,
                    AdexBehaviourCmd::PublishMsg { .. } | AdexBehaviourCmd::PublishMsgFrom { .. }
                ) {
                    n += 1;
                }
            }
            n
        };

        // Balance 5, fee 0.1: the supported maximum is 4.9, below the available 10 and above the minimum 1.
        let lowered = own_order("BASE", "REL", "10", "1");
        store(&ctx, &lowered);
        block_on(reconcile_missing_own_maker_orders(&ctx, &ordermatch_ctx));
        let max_vol = block_on(
            ordermatch_ctx
                .maker_orders_ctx
                .lock()
                .get_order(&lowered.uuid)
                .unwrap()
                .lock(),
        )
        .max_base_vol
        .clone();
        assert_eq!(max_vol, MmNumber::from("4.9"));
        assert!(published(&mut p2p_rx) >= 1, "the lowered order is announced");
        assert!(cancelled.lock().unwrap().is_empty());
        remove(&ctx, &lowered);

        // Balance 0.5: the supported maximum 0.4 is below the minimum 1: cancelled, not announced.
        probe.script(&[Some("0.5")]);
        let too_small = own_order("BASE", "REL", "10", "1");
        store(&ctx, &too_small);
        block_on(reconcile_missing_own_maker_orders(&ctx, &ordermatch_ctx));
        assert_eq!(published(&mut p2p_rx), 0, "a cancelled order is not announced");
        assert_eq!(*cancelled.lock().unwrap(), vec![format!(
            "{} {}",
            too_small.uuid,
            MakerOrderCancellationReason::InsufficientBalance
        )]);
        assert!(!ordermatch_ctx.maker_orders_ctx.lock().contains_key(&too_small.uuid));

        // A balance read that fails cancels the order as well.
        probe.script(&[None]);
        let unreadable = own_order("BASE", "REL", "10", "1");
        store(&ctx, &unreadable);
        block_on(reconcile_missing_own_maker_orders(&ctx, &ordermatch_ctx));
        assert!(!ordermatch_ctx.maker_orders_ctx.lock().contains_key(&unreadable.uuid));
        assert_eq!(published(&mut p2p_rx), 0);
    }

    /// A coin that is only activated, or only appears as rel, is never polled.
    #[test]
    fn a_coin_that_is_only_activated_or_only_rel_is_never_polled() {
        let probe = Probe::install("10");
        let ctx = ctx_with_coins(&probe);
        let order = own_order("BASE", "REL", "5", "1");
        store(&ctx, &order);

        assert_eq!(begin(&ctx, "REL", Some("10")), None, "rel only: no watch");
        assert_eq!(begin(&ctx, "OTHER", None), None, "activated only: no watch");
        // The entry point the order paths use spawns nothing for them either.
        let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
        start_maker_balance_watch(&ctx, &ordermatch_ctx, "REL", None);
        start_maker_balance_watch(&ctx, &ordermatch_ctx, "OTHER", None);
        {
            let maker_orders = ordermatch_ctx.maker_orders_ctx.lock();
            assert!(!maker_orders.has_balance_watch("REL"));
            assert!(!maker_orders.has_balance_watch("OTHER"));
        }

        let generation = begin(&ctx, "BASE", Some("10")).unwrap();
        drive(&ctx, &probe, "BASE", generation, |n| {
            if n == 3 {
                remove(&ctx, &order);
            }
        });
        assert_eq!(probe.count("read REL"), 0);
        assert_eq!(probe.count("read OTHER"), 0);
        assert_eq!(probe.count("read BASE"), 2);
    }

    /// The watch stops when the last base order goes, and a second order on the same base does not start a
    /// second watch.
    #[test]
    fn the_watch_stops_with_the_last_base_order_and_is_not_doubled() {
        let probe = Probe::install("10");
        let ctx = ctx_with_coins(&probe);
        let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
        let first = own_order("BASE", "REL", "5", "1");
        let second = own_order("BASE", "OTHER", "5", "1");
        store(&ctx, &first);
        let generation = begin(&ctx, "BASE", Some("10")).unwrap();
        store(&ctx, &second);
        assert_eq!(begin(&ctx, "BASE", Some("99")), None, "one watch per coin");
        assert_eq!(
            ordermatch_ctx.maker_orders_ctx.lock().balance_watch_last_seen("BASE"),
            Some(bd("10")),
            "the running watch keeps its own last-seen value"
        );

        remove(&ctx, &first);
        assert!(ordermatch_ctx.maker_orders_ctx.lock().has_balance_watch("BASE"));
        remove(&ctx, &second);
        assert!(!ordermatch_ctx.maker_orders_ctx.lock().has_balance_watch("BASE"));

        // The task of the ended watch finds it gone after its pause and reads nothing.
        drive(&ctx, &probe, "BASE", generation, |_| {});
        assert_eq!(probe.entries(), vec!["sleep 30"]);

        // A later order starts a new watch; the old one's generation is not live.
        let third = own_order("BASE", "REL", "5", "1");
        store(&ctx, &third);
        let renewed = begin(&ctx, "BASE", None).unwrap();
        assert_ne!(renewed, generation);
        let maker_orders = ordermatch_ctx.maker_orders_ctx.lock();
        assert!(maker_orders.balance_watch_is_live("BASE", renewed));
        assert!(!maker_orders.balance_watch_is_live("BASE", generation));
    }

    /// A change observed after the last order went does not recompute: the observation is stale.
    #[test]
    fn an_observation_without_a_base_order_is_stale() {
        let probe = Probe::install("10");
        let ctx = ctx_with_coins(&probe);
        let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
        let order = own_order("BASE", "REL", "5", "1");
        store(&ctx, &order);
        let generation = begin(&ctx, "BASE", Some("10")).unwrap();
        let mut maker_orders = ordermatch_ctx.maker_orders_ctx.lock();
        assert_eq!(
            maker_orders.observe_balance("BASE", generation, &bd("10")),
            BalanceObservation::Unchanged
        );
        assert_eq!(
            maker_orders.observe_balance("BASE", generation, &bd("11")),
            BalanceObservation::Changed
        );
        maker_orders.remove_order(&order.uuid);
        assert_eq!(
            maker_orders.observe_balance("BASE", generation, &bd("12")),
            BalanceObservation::Stale
        );
    }

    /// A taker order converted to a maker order (CRD ch.59 R59.4.1) starts an unseeded watch and reads no balance.
    #[test]
    fn a_converted_taker_order_starts_an_unseeded_watch() {
        let dbdir = std::env::temp_dir().join(format!("kdf-balance-watch-convert-{}", Uuid::new_v4()));
        let probe = Probe::install("10");
        let ctx = MmCtxBuilder::new()
            .with_conf(json!({
                "coins": [{ "coin": "BASE" }, { "coin": "REL" }],
                "dbdir": dbdir.to_str().unwrap()
            }))
            .with_secp256k1_key_pair(key_pair_from_seed("passphrase").unwrap())
            .into_mm_arc();
        let (tx, _p2p_rx) = mpsc::channel(100);
        P2PContext::new(tx).store_to_mm_arc(&ctx);
        std::fs::create_dir_all(my_maker_orders_dir(&ctx)).unwrap();
        std::fs::create_dir_all(my_taker_orders_dir(&ctx)).unwrap();
        let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();
        for ticker in ["BASE", "REL"] {
            block_on(coins_ctx.add_coin(MmCoinEnum::Test(TestCoin::new(ticker)))).unwrap();
        }
        delete_my_taker_order.mock_safe(|_, _, _| MockResult::Return(Box::new(futures01::future::ok(()))));

        let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
        let taker = TakerOrder {
            request: TakerRequest {
                base: "BASE".into(),
                rel: "REL".into(),
                uuid: Uuid::new_v4(),
                dest_pub_key: H256Json::default(),
                sender_pubkey: H256Json::default(),
                base_amount: 10.into(),
                rel_amount: 10.into(),
                action: TakerAction::Sell,
                match_by: MatchBy::Any,
                conf_settings: Some(OrderConfirmationsSettings::default()),
                base_protocol_info: None,
                rel_protocol_info: None,
                swap_version: SwapVersion::default(),
            },
            matches: HashMap::new(),
            created_at: now_ms() - 60_000,
            order_type: OrderType::GoodTillCancelled,
            min_volume: 0.into(),
            timeout: 30,
            save_in_history: false,
            base_orderbook_ticker: None,
            rel_orderbook_ticker: None,
            p2p_privkey: None,
        };
        let uuid = taker.request.uuid;
        block_on(ordermatch_ctx.my_taker_orders.lock()).insert(uuid, taker);

        block_on(handle_timed_out_taker_orders(ctx.clone(), &ordermatch_ctx));

        let maker_orders = ordermatch_ctx.maker_orders_ctx.lock();
        assert!(maker_orders.contains_key(&uuid), "the taker order became a maker order");
        assert!(maker_orders.has_balance_watch("BASE"));
        assert!(!maker_orders.has_balance_watch("REL"));
        assert_eq!(maker_orders.balance_watch_last_seen("BASE"), None);
        assert_eq!(probe.entries(), Vec::<String>::new(), "no balance read at insertion");
        std::fs::remove_dir_all(&dbdir).ok();
    }

    /// An order created by `setprice` seeds the watch with the balance the creation read, the quantity the poll
    /// compares, and the creation reads the base balance once.
    #[test]
    fn setprice_seeds_the_watch_with_the_balance_it_read() {
        let dbdir = std::env::temp_dir().join(format!("kdf-balance-watch-setprice-{}", Uuid::new_v4()));
        let probe = Probe::install("42");
        let ctx = MmCtxBuilder::new()
            .with_conf(json!({
                "coins": [{ "coin": "BASE" }, { "coin": "REL" }],
                "dbdir": dbdir.to_str().unwrap()
            }))
            .with_secp256k1_key_pair(key_pair_from_seed("passphrase").unwrap())
            .into_mm_arc();
        let (tx, _p2p_rx) = mpsc::channel(100);
        P2PContext::new(tx).store_to_mm_arc(&ctx);
        std::fs::create_dir_all(my_maker_orders_dir(&ctx)).unwrap();
        let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();
        for ticker in ["BASE", "REL"] {
            block_on(coins_ctx.add_coin(MmCoinEnum::Test(TestCoin::new(ticker)))).unwrap();
        }

        for max in [false, true] {
            let req: SetPriceReq = json::from_value(json!({
                "base": "BASE", "rel": "REL", "price": 1, "volume": 3, "max": max, "save_in_history": false
            }))
            .unwrap();
            let order = block_on(create_maker_order(&ctx, req)).unwrap();
            let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
            let maker_orders = ordermatch_ctx.maker_orders_ctx.lock();
            assert!(maker_orders.contains_key(&order.uuid));
            assert_eq!(maker_orders.balance_watch_last_seen("BASE"), Some(bd("42")));
            assert!(!maker_orders.has_balance_watch("REL"));
            drop(maker_orders);
            // One read of the base balance per creation: the one the checks made, which is the seed.
            assert_eq!(probe.count("read BASE"), 1);
            probe.log.lock().unwrap().clear();
        }
        std::fs::remove_dir_all(&dbdir).ok();
    }

    // ---- R60.4.6 item 5: the maintenance reads hold no order lock -------------------------------------------

    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    fn publishes(rx: &mut mpsc::Receiver<AdexBehaviourCmd>) -> usize {
        let mut n = 0;
        while let Ok(cmd) = rx.try_recv() {
            if matches!(
                cmd,
                AdexBehaviourCmd::PublishMsg { .. } | AdexBehaviourCmd::PublishMsgFrom { .. }
            ) {
                n += 1;
            }
        }
        n
    }

    fn order_mutex(ctx: &MmArc, order: &MakerOrder) -> Arc<AsyncMutex<MakerOrder>> {
        let ordermatch_ctx = OrdermatchContext::from_ctx(ctx).unwrap();
        let order_mutex = ordermatch_ctx
            .maker_orders_ctx
            .lock()
            .get_order(&order.uuid)
            .unwrap()
            .clone();
        order_mutex
    }

    /// The balance read and the fee read of the maintenance pass try to take the order's lock when they are
    /// reached. They succeed only if the pass does not hold the lock across its network reads. Returns the number
    /// of reads that found the lock taken.
    fn mock_reads_that_try_the_lock(
        order_mutex: Arc<AsyncMutex<MakerOrder>>,
        spendable: &'static str,
        on_balance_read: impl Fn() + 'static,
    ) -> Arc<AtomicUsize> {
        let blocked = Arc::new(AtomicUsize::new(0));
        let (balance_guard, fee_guard) = (blocked.clone(), blocked.clone());
        let balance_order = order_mutex.clone();
        TestCoin::my_balance.mock_safe(move |_| {
            if balance_order.try_lock().is_none() {
                balance_guard.fetch_add(1, AtomicOrdering::SeqCst);
            }
            on_balance_read();
            MockResult::Return(Box::new(futures01::future::ok(CoinBalance::new(bd(spendable)))))
        });
        TestCoin::get_sender_trade_fee.mock_safe(move |coin, _value, _stage| {
            if order_mutex.try_lock().is_none() {
                fee_guard.fetch_add(1, AtomicOrdering::SeqCst);
            }
            let fee = TradeFee {
                coin: coin.ticker().to_owned(),
                amount: MmNumber::from("0.1"),
                paid_from_trading_vol: false,
            };
            MockResult::Return(Box::pin(async move { Ok(fee) }))
        });
        blocked
    }

    /// The order's lock is free while the pass reads the base coin's balance and the fee, so a call that awaits
    /// the order (`my_orders`, `update_maker_order`, `cancel`) does not wait for those round trips. The pass
    /// still applies its result: the order is lowered and re-announced.
    #[test]
    fn t60_7_g_the_reconciliation_holds_no_order_lock_across_its_reads() {
        let probe = Probe::install("5");
        let ctx = ctx_with_coins(&probe);
        let (tx, mut p2p_rx) = mpsc::channel(100);
        P2PContext::new(tx).store_to_mm_arc(&ctx);
        let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
        let order = own_order("BASE", "REL", "10", "1");
        store(&ctx, &order);
        let mutex = order_mutex(&ctx, &order);
        let reads = Arc::new(AtomicUsize::new(0));
        let counted = reads.clone();
        let blocked = mock_reads_that_try_the_lock(mutex.clone(), "5", move || {
            counted.fetch_add(1, AtomicOrdering::SeqCst);
        });

        block_on(reconcile_missing_own_maker_orders(&ctx, &ordermatch_ctx));

        assert_eq!(reads.load(AtomicOrdering::SeqCst), 1, "the balance was read");
        assert_eq!(
            blocked.load(AtomicOrdering::SeqCst),
            0,
            "a read found the order's lock taken"
        );
        assert_eq!(block_on(mutex.lock()).max_base_vol, MmNumber::from("4.9"));
        assert!(publishes(&mut p2p_rx) >= 1, "and the order is re-announced");
    }

    /// Several missing orders on one base coin share one balance read per pass; another base coin reads its own.
    #[test]
    fn t60_7_g_missing_orders_on_one_base_coin_share_one_balance_read() {
        let probe = Probe::install("10");
        let ctx = ctx_with_coins(&probe);
        let (tx, mut p2p_rx) = mpsc::channel(100);
        P2PContext::new(tx).store_to_mm_arc(&ctx);
        let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
        for (base, rel) in [("BASE", "REL"), ("BASE", "OTHER"), ("BASE", "REL"), ("OTHER", "REL")] {
            store(&ctx, &own_order(base, rel, "5", "1"));
        }

        block_on(reconcile_missing_own_maker_orders(&ctx, &ordermatch_ctx));

        assert_eq!(probe.count("read BASE"), 1, "{:?}", probe.entries());
        assert_eq!(probe.count("read OTHER"), 1, "{:?}", probe.entries());
        assert_eq!(probe.entries().len(), 2);
        assert!(publishes(&mut p2p_rx) >= 4, "every order is re-announced");
    }

    /// An order that went away between the read and the apply is neither touched nor announced.
    #[test]
    fn t60_7_g_an_order_removed_between_the_read_and_the_apply_is_left_alone() {
        let probe = Probe::install("2");
        let ctx = ctx_with_coins(&probe);
        let (tx, mut p2p_rx) = mpsc::channel(100);
        P2PContext::new(tx).store_to_mm_arc(&ctx);
        let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
        let cancelled = Arc::new(AtomicUsize::new(0));
        let cancelled_count = cancelled.clone();
        delete_my_maker_order.mock_safe(move |_, _, _| {
            cancelled_count.fetch_add(1, AtomicOrdering::SeqCst);
            MockResult::Return(Box::new(futures01::future::ok(())))
        });
        let gone = own_order("BASE", "REL", "5", "1");
        let kept = own_order("OTHER", "REL", "5", "1");
        store(&ctx, &gone);
        store(&ctx, &kept);
        let gone_mutex = order_mutex(&ctx, &gone);
        // The balance of BASE is 2, so a lowering would show: the supported maximum 1.9 is below the available 5.
        let remover = ctx.clone();
        let removed = gone.clone();
        mock_reads_that_try_the_lock(gone_mutex.clone(), "2", move || remove(&remover, &removed));

        block_on(reconcile_missing_own_maker_orders(&ctx, &ordermatch_ctx));

        assert_eq!(
            block_on(gone_mutex.lock()).max_base_vol,
            MmNumber::from("5"),
            "not touched"
        );
        assert!(!ordermatch_ctx.maker_orders_ctx.lock().contains_key(&gone.uuid));
        assert_eq!(cancelled.load(AtomicOrdering::SeqCst), 0, "not cancelled either");
        // Only the order that stayed was announced (the mock is the same for both bases, so it was lowered too).
        assert_eq!(publishes(&mut p2p_rx), 1);
        assert!(ordermatch_ctx.maker_orders_ctx.lock().contains_key(&kept.uuid));
    }

    /// CRD ch.32 R-GH8a, T-GH8a: with `setprice_replace_after_checks` on, `setprice` with `cancel_previous`
    /// replaces the previous orders of the pair only after every check and the build of the new order passed.
    mod t_gh8a_replace_after_checks {
        use super::super::maker_publication_tests::drain;
        use super::*;
        use crate::mm2::lp_ordermatch::new_protocol::OrdermatchMessage;

        const PAIR: &str = "BASE:REL";

        /// A message of the maker order flow that left the node.
        #[derive(Debug, PartialEq)]
        enum Sent {
            Created(Uuid),
            Cancelled(Uuid),
        }

        /// A node with the coins `BASE` and `REL`, the scripted balance of `Probe` and a channel that receives what
        /// the node publishes.
        struct Maker {
            ctx: MmArc,
            dbdir: std::path::PathBuf,
            rx: mpsc::Receiver<AdexBehaviourCmd>,
            probe: Probe,
        }

        impl Drop for Maker {
            fn drop(&mut self) { std::fs::remove_dir_all(&self.dbdir).ok(); }
        }

        impl Maker {
            /// `extra` is merged into the top-level configuration.
            fn new(extra: Json) -> Maker {
                let probe = Probe::install("42");
                let dbdir = std::env::temp_dir().join(format!("kdf-gh8a-{}", Uuid::new_v4()));
                let mut conf = json!({
                    "coins": [{ "coin": "BASE" }, { "coin": "REL" }],
                    "dbdir": dbdir.to_str().unwrap()
                });
                for (key, value) in extra.as_object().unwrap() {
                    conf[key] = value.clone();
                }
                let ctx = MmCtxBuilder::new()
                    .with_conf(conf)
                    .with_secp256k1_key_pair(key_pair_from_seed("passphrase").unwrap())
                    .into_mm_arc();
                let (tx, rx) = mpsc::channel(1000);
                P2PContext::new(tx).store_to_mm_arc(&ctx);
                std::fs::create_dir_all(my_maker_orders_dir(&ctx)).unwrap();
                let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();
                for ticker in ["BASE", "REL"] {
                    block_on(coins_ctx.add_coin(MmCoinEnum::Test(TestCoin::new(ticker)))).unwrap();
                }
                Maker { ctx, dbdir, rx, probe }
            }

            fn ordermatch_ctx(&self) -> Arc<OrdermatchContext> { OrdermatchContext::from_ctx(&self.ctx).unwrap() }

            /// `setprice` of `volume` BASE at price 1, with an optional minimum volume.
            fn set_price(
                &self,
                cancel_previous: bool,
                volume: i64,
                min_volume: Option<i64>,
            ) -> Result<MakerOrder, String> {
                self.set_price_with_history(cancel_previous, volume, min_volume, false)
            }

            /// As `set_price`, with the choice of `save_in_history`: the history write is the one that can fail
            /// when the order is saved.
            fn set_price_with_history(
                &self,
                cancel_previous: bool,
                volume: i64,
                min_volume: Option<i64>,
                save_in_history: bool,
            ) -> Result<MakerOrder, String> {
                let req: SetPriceReq = json::from_value(json!({
                    "base": "BASE", "rel": "REL", "price": 1, "volume": volume, "min_volume": min_volume,
                    "cancel_previous": cancel_previous, "save_in_history": save_in_history
                }))
                .unwrap();
                block_on(create_maker_order(&self.ctx, req))
            }

            /// Two orders on (BASE, REL), created without `cancel_previous`; what was published meanwhile and the
            /// reads of the balance are forgotten.
            fn two_previous_orders(&mut self) -> HashSet<Uuid> {
                let previous: HashSet<Uuid> = (0..2).map(|_| placed(self.set_price(false, 3, None)).uuid).collect();
                flush_trie(&self.ctx);
                assert_eq!(self.sent().len(), 2);
                self.probe.log.lock().unwrap().clear();
                previous
            }

            /// The created and cancelled messages published since the last call, in publication order.
            fn sent(&mut self) -> Vec<Sent> {
                drain(&mut self.rx)
                    .into_iter()
                    .filter_map(|published| match published.message {
                        OrdermatchMessage::MakerOrderCreated(created) => Some(Sent::Created(created.uuid.into())),
                        OrdermatchMessage::MakerOrderCancelled(cancelled) => {
                            Some(Sent::Cancelled(cancelled.uuid.into()))
                        },
                        _ => None,
                    })
                    .collect()
            }

            /// The own orders of the maker.
            fn own_orders(&self) -> HashSet<Uuid> {
                self.ordermatch_ctx()
                    .maker_orders_ctx
                    .lock()
                    .clone_orders()
                    .keys()
                    .copied()
                    .collect()
            }

            fn in_book(&self, uuid: &Uuid) -> bool {
                flush_trie(&self.ctx);
                self.ordermatch_ctx()
                    .orderbook
                    .lock()
                    .find_order_by_uuid(uuid)
                    .is_some()
            }

            /// The root the trie holds for the pair of the maker's own orders.
            fn held_root(&self) -> H64 {
                flush_trie(&self.ctx);
                let pubkey = hex::encode(self.ctx.secp256k1_key_pair().public_slice());
                pair_trie_root_by_pub_ctx(&self.ordermatch_ctx(), &pubkey, PAIR)
            }

            /// Nothing happened to the pair: the same orders in the maker's set, in the book and under the held
            /// root, and nothing published.
            fn assert_pair_unchanged(&mut self, previous: &HashSet<Uuid>, root: H64) {
                assert_eq!(&self.own_orders(), previous);
                assert!(previous.iter().all(|uuid| self.in_book(uuid)));
                assert_eq!(self.held_root(), root);
                assert_eq!(self.sent(), Vec::<Sent>::new(), "no cancel and no created message");
            }
        }

        fn placed(result: Result<MakerOrder, String>) -> MakerOrder {
            match result {
                Ok(order) => order,
                Err(e) => panic!("the order must be placed: {e}"),
            }
        }

        fn refused(result: Result<MakerOrder, String>) -> String {
            match result {
                Ok(order) => panic!("the order {} must be refused", order.uuid),
                Err(e) => e,
            }
        }

        /// A balance check that fails once, then passes: the first read of the balance fails.
        fn fail_the_next_balance_check(maker: &Maker) { maker.probe.script(&[None]); }

        /// T-GH8a bullet 1: the balance check fails, the error is returned, both previous orders are in the
        /// maker's set and under the pair's held root, and no cancel message is published.
        #[test]
        fn t_gh8a_a_failed_balance_check_leaves_the_previous_orders() {
            let mut maker = Maker::new(json!({ "setprice_replace_after_checks": true }));
            let previous = maker.two_previous_orders();
            let root = maker.held_root();
            fail_the_next_balance_check(&maker);

            assert!(maker.set_price(true, 3, None).is_err());

            maker.assert_pair_unchanged(&previous, root);
        }

        /// T-GH8a bullet 2: the checks pass, the builder refuses the order (a volume below its minimum); the
        /// pair is left as in bullet 1. The balance was read, so the checks ran before the build.
        #[test]
        fn t_gh8a_b_a_refused_build_leaves_the_previous_orders() {
            let mut maker = Maker::new(json!({ "setprice_replace_after_checks": true }));
            let previous = maker.two_previous_orders();
            let root = maker.held_root();

            let err = refused(maker.set_price(true, 1, Some(2)));

            assert!(err.contains("below min base vol"), "{err}");
            assert_eq!(maker.probe.count("read BASE"), 1, "the balance check ran first");
            maker.assert_pair_unchanged(&previous, root);
        }

        /// T-GH8a, item 2 of R-GH8a: the save of the new order comes before the first cancel. The history database
        /// of the node has no orders table, so the history write of the new order (`save_in_history`) fails: the
        /// error is returned, both previous orders stay in the maker's set, in the book and under the held root,
        /// no cancel and no created message is published, and no new order exists.
        #[test]
        fn t_gh8a_f_a_failed_save_leaves_the_previous_orders() {
            let mut maker = Maker::new(json!({ "setprice_replace_after_checks": true }));
            let previous = maker.two_previous_orders();
            let root = maker.held_root();
            let connection = db_common::sqlite::rusqlite::Connection::open_in_memory().unwrap();
            let _ = maker
                .ctx
                .sqlite_connection
                .pin(Arc::new(std::sync::Mutex::new(connection)));

            let err = refused(maker.set_price_with_history(true, 3, None, true));

            assert!(err.contains("Error saving an order"), "{err}");
            assert_eq!(maker.probe.count("read BASE"), 1, "the balance check ran first");
            maker.assert_pair_unchanged(&previous, root);
        }

        /// T-GH8a bullet 3 with T-GH8 and T-GH9: after a failed re-pricing, the next one whose checks pass
        /// replaces both orders: one cancel per old order, all before the created message of the new order.
        #[test]
        fn t_gh8a_c_the_next_passing_repricing_replaces_both_orders() {
            let mut maker = Maker::new(json!({ "setprice_replace_after_checks": true }));
            let previous = maker.two_previous_orders();
            let root = maker.held_root();
            fail_the_next_balance_check(&maker);
            assert!(maker.set_price(true, 3, None).is_err());
            maker.assert_pair_unchanged(&previous, root);

            let new_order = placed(maker.set_price(true, 3, None));

            assert_eq!(maker.own_orders(), HashSet::from([new_order.uuid]));
            assert!(maker.in_book(&new_order.uuid));
            assert!(previous.iter().all(|uuid| !maker.in_book(uuid)));
            assert_ne!(maker.held_root(), root);
            let sent = maker.sent();
            assert_eq!(sent.len(), 3, "{sent:?}");
            let cancelled: HashSet<Uuid> = sent[..2]
                .iter()
                .map(|message| match message {
                    Sent::Cancelled(uuid) => *uuid,
                    other => panic!("a cancel message must come first, got {other:?}"),
                })
                .collect();
            assert_eq!(cancelled, previous, "one cancel per old order");
            assert_eq!(sent[2], Sent::Created(new_order.uuid));
        }

        /// T-GH8a bullet 4 (R-GH8): with the switch absent or `false`, the failed check cancels both previous
        /// orders and creates none. A non-boolean value is taken as the default too (bullet 5).
        #[test]
        fn t_gh8a_d_at_the_default_a_failed_check_cancels_both_orders() {
            for extra in [
                json!({}),
                json!({ "setprice_replace_after_checks": false }),
                json!({ "setprice_replace_after_checks": "true" }),
                json!({ "setprice_replace_after_checks": 1 }),
                json!({ "setprice_replace_after_checks": null }),
            ] {
                let mut maker = Maker::new(extra.clone());
                let previous = maker.two_previous_orders();
                fail_the_next_balance_check(&maker);

                assert!(maker.set_price(true, 3, None).is_err(), "{extra}");

                assert_eq!(maker.own_orders(), HashSet::new(), "{extra}");
                assert!(previous.iter().all(|uuid| !maker.in_book(uuid)), "{extra}");
                let sent = maker.sent();
                assert_eq!(sent.len(), 2, "{extra}: {sent:?}");
                let cancelled: HashSet<Uuid> = sent
                    .iter()
                    .map(|message| match message {
                        Sent::Cancelled(uuid) => *uuid,
                        other => panic!("{extra}: no created message expected, got {other:?}"),
                    })
                    .collect();
                assert_eq!(cancelled, previous, "{extra}");
            }
        }

        /// R-GH8 at the default: a build that fails after the cancel leaves the pair without an order.
        #[test]
        fn t_gh8a_d_at_the_default_a_refused_build_leaves_the_pair_empty() {
            let mut maker = Maker::new(json!({}));
            let previous = maker.two_previous_orders();

            assert!(maker.set_price(true, 1, Some(2)).is_err());

            assert_eq!(maker.own_orders(), HashSet::new());
            let sent = maker.sent();
            assert_eq!(sent.len(), 2, "{sent:?}");
            assert!(sent.iter().all(|message| match message {
                Sent::Cancelled(uuid) => previous.contains(uuid),
                Sent::Created(_) => false,
            }));
        }

        /// A request without `cancel_previous` never cancels, with the switch on or off.
        #[test]
        fn t_gh8a_without_cancel_previous_nothing_is_cancelled() {
            for extra in [json!({}), json!({ "setprice_replace_after_checks": true })] {
                let mut maker = Maker::new(extra.clone());
                let previous = maker.two_previous_orders();
                let root = maker.held_root();
                fail_the_next_balance_check(&maker);

                assert!(maker.set_price(false, 3, None).is_err(), "{extra}");

                maker.assert_pair_unchanged(&previous, root);
            }
        }

        /// T-GH8a bullet 5: only a JSON `true` turns the switch on.
        #[test]
        fn t_gh8a_e_only_a_json_true_turns_the_switch_on() {
            let switch = |conf: Json| {
                let ctx = MmCtxBuilder::new().with_conf(conf).into_mm_arc();
                setprice_replace_after_checks(&ctx)
            };
            assert!(!switch(json!({})));
            assert!(switch(json!({ "setprice_replace_after_checks": true })));
            for other in [
                json!(false),
                json!("true"),
                json!(1),
                json!(0),
                json!(null),
                json!([true]),
            ] {
                assert!(!switch(json!({ "setprice_replace_after_checks": other })), "{other}");
            }
        }
    }
}

/// CRD ch.12 R7a, R7b (maintainer Decision AM): a maker order's own lock is held for in-memory work only, and the
/// storage writes of one order keep the order of its changes. T7 and T8.
mod t12_7_order_lock_io_tests {
    use super::*;
    use crate::mm2::database::my_orders::CREATE_MY_ORDERS_TABLE;
    use crate::mm2::lp_ordermatch::my_orders_storage::{write_maker_order_state, MyOrdersError};
    use crate::mm2::lp_ordermatch::order_write_gate::PublishedState;
    use coins::{BalanceError, CoinsContext, MarketCoinOps, MmCoinEnum, TradeFee};
    use futures::future::poll_fn;
    use futures::{FutureExt, TryFutureExt};
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
    use std::task::{Context, Poll};

    const ORDER_JSON: &str = r#"{"max_base_vol":"1","max_base_vol_rat":[[1,[1]],[1,[1]]],"min_base_vol":"0","min_base_vol_rat":[[0,[]],[1,[1]]],"price":"1","price_rat":[[1,[1]],[1,[1]]],"created_at":1589265312093,"updated_at":1589265312093,"base":"ETH","rel":"JST","matches":{"2f9afe84-7a89-4194-8947-45fba563118f":{"request":{"base":"ETH","rel":"JST","base_amount":"0.1","base_amount_rat":[[1,[1]],[1,[10]]],"rel_amount":"0.2","rel_amount_rat":[[1,[1]],[1,[5]]],"action":"Buy","uuid":"2f9afe84-7a89-4194-8947-45fba563118f","method":"request","sender_pubkey":"031d4256c4bc9f99ac88bf3dba21773132281f65f9bf23a59928bce08961e2f3","dest_pub_key":"0000000000000000000000000000000000000000000000000000000000000000","match_by":{"type":"Any"}},"reserved":{"base":"ETH","rel":"JST","base_amount":"0.1","base_amount_rat":[[1,[1]],[1,[10]]],"rel_amount":"0.1","rel_amount_rat":[[1,[1]],[1,[10]]],"taker_order_uuid":"2f9afe84-7a89-4194-8947-45fba563118f","maker_order_uuid":"5f6516ea-ccaa-453a-9e37-e1c2c0d527e3","method":"reserved","sender_pubkey":"c6a78589e18b482aea046975e6d0acbdea7bf7dbf04d9d5bd67fda917815e3ed","dest_pub_key":"031d4256c4bc9f99ac88bf3dba21773132281f65f9bf23a59928bce08961e2f3"},"connect":{"taker_order_uuid":"2f9afe84-7a89-4194-8947-45fba563118f","maker_order_uuid":"5f6516ea-ccaa-453a-9e37-e1c2c0d527e3","method":"connect","sender_pubkey":"031d4256c4bc9f99ac88bf3dba21773132281f65f9bf23a59928bce08961e2f3","dest_pub_key":"c6a78589e18b482aea046975e6d0acbdea7bf7dbf04d9d5bd67fda917815e3ed"},"connected":{"taker_order_uuid":"2f9afe84-7a89-4194-8947-45fba563118f","maker_order_uuid":"5f6516ea-ccaa-453a-9e37-e1c2c0d527e3","method":"connected","sender_pubkey":"c6a78589e18b482aea046975e6d0acbdea7bf7dbf04d9d5bd67fda917815e3ed","dest_pub_key":"031d4256c4bc9f99ac88bf3dba21773132281f65f9bf23a59928bce08961e2f3"},"last_updated":1589265314408}},"started_swaps":["2f9afe84-7a89-4194-8947-45fba563118f"],"uuid":"5f6516ea-ccaa-453a-9e37-e1c2c0d527e3"}"#;

    fn bd(s: &str) -> BigDecimal { s.parse().unwrap() }

    /// A node with two test coins, real order files in a temporary directory and an in-memory history database.
    struct Node {
        ctx: MmArc,
        dbdir: std::path::PathBuf,
        p2p_rx: mpsc::Receiver<AdexBehaviourCmd>,
    }

    impl Drop for Node {
        fn drop(&mut self) { std::fs::remove_dir_all(&self.dbdir).ok(); }
    }

    impl Node {
        fn new() -> Node {
            TestCoin::min_trading_vol.mock_safe(|_| MockResult::Return(MmNumber::from("0.0001")));
            TestCoin::min_tx_amount.mock_safe(|_| MockResult::Return(bd("0")));
            TestCoin::get_sender_trade_fee.mock_safe(|coin, _value, _stage| {
                let fee = TradeFee {
                    coin: coin.ticker().to_owned(),
                    amount: MmNumber::from("0.1"),
                    paid_from_trading_vol: false,
                };
                MockResult::Return(Box::pin(async move { Ok(fee) }))
            });
            TestCoin::get_receiver_trade_fee.mock_safe(|coin, _stage| {
                let fee = TradeFee {
                    coin: coin.ticker().to_owned(),
                    amount: MmNumber::from("0.1"),
                    paid_from_trading_vol: false,
                };
                MockResult::Return(Box::new(futures01::future::ok(fee)))
            });
            let dbdir = std::env::temp_dir().join(format!("kdf-order-lock-io-{}", Uuid::new_v4()));
            let conf = json!({ "dbdir": dbdir.to_string_lossy(), "pre_trade_spend_check": false });
            let ctx = MmCtxBuilder::new().with_conf(conf).into_mm_arc();
            ctx.secp256k1_key_pair
                .pin(key_pair_from_seed("passphrase").unwrap())
                .unwrap();
            let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();
            for ticker in ["DYN", "KMD"] {
                block_on(coins_ctx.add_coin(MmCoinEnum::Test(TestCoin::new(ticker)))).unwrap();
            }
            std::fs::create_dir_all(my_maker_orders_dir(&ctx)).unwrap();
            std::fs::create_dir_all(my_orders_history_dir(&ctx)).unwrap();
            let connection = Connection::open_in_memory().unwrap();
            connection.execute(CREATE_MY_ORDERS_TABLE, []).unwrap();
            let _ = ctx.sqlite_connection.pin(Arc::new(Mutex::new(connection)));
            let (tx, p2p_rx) = mpsc::channel(1000);
            P2PContext::new(tx).store_to_mm_arc(&ctx);
            Node { ctx, dbdir, p2p_rx }
        }

        fn ordermatch_ctx(&self) -> Arc<OrdermatchContext> { OrdermatchContext::from_ctx(&self.ctx).unwrap() }

        /// Adds an own maker order DYN/KMD (volume 10, minimum 1, price 0.5) to the container. With `on_disk` it is
        /// also saved, as `setprice` saves a new order. It is old enough for an update to change `updated_at`.
        fn add_order(&self, on_disk: bool, save_in_history: bool) -> (MakerOrder, Arc<AsyncMutex<MakerOrder>>) {
            let base = MmCoinEnum::Test(TestCoin::new("DYN"));
            let rel = MmCoinEnum::Test(TestCoin::new("KMD"));
            let mut order = MakerOrderBuilder::new(&base, &rel)
                .with_max_base_vol(MmNumber::from("10"))
                .with_min_base_vol(Some(MmNumber::from("1")))
                .with_price(MmNumber::from("0.5"))
                .with_conf_settings(OrderConfirmationsSettings::default())
                .with_save_in_history(save_in_history)
                .build_unchecked();
            order.created_at -= 10_000;
            order.updated_at = Some(order.created_at);
            order.save_in_history = save_in_history;
            if on_disk {
                block_on(save_my_new_maker_order(self.ctx.clone(), &order)).unwrap();
            }
            let handle = Arc::new(AsyncMutex::new(order.clone()));
            self.ordermatch_ctx()
                .maker_orders_ctx
                .lock()
                .add_order(&order, handle.clone());
            (order, handle)
        }

        fn in_container(&self, uuid: &Uuid) -> bool { self.ordermatch_ctx().maker_orders_ctx.lock().contains_key(uuid) }

        fn active_record(&self, uuid: &Uuid) -> Option<MakerOrder> {
            let path = my_maker_order_file_path(&self.ctx, uuid);
            std::fs::read(path).ok().map(|bytes| json::from_slice(&bytes).unwrap())
        }

        fn history_entry(&self, uuid: Uuid) -> Option<MakerOrder> {
            match block_on(MyOrdersStorage::new(self.ctx.clone()).load_order_from_history(uuid)) {
                Ok(Order::Maker(order)) => Some(order),
                _ => None,
            }
        }
    }

    fn update_price_req(uuid: Uuid, price: &'static str) -> MakerOrderUpdateReq {
        MakerOrderUpdateReq {
            uuid,
            new_price: Some(MmNumber::from(price)),
            max: None,
            volume_delta: None,
            min_volume: None,
            base_confs: None,
            base_nota: None,
            rel_confs: None,
            rel_nota: None,
        }
    }

    /// An unfinished match that has been waiting longer than the match timeout.
    fn stale_match() -> MakerMatch {
        let order: MakerOrder = json::from_str(ORDER_JSON).unwrap();
        let mut stale = order.matches.into_values().next().unwrap();
        stale.connect = None;
        stale.connected = None;
        stale.last_updated = 1;
        stale
    }

    /// Pends once, so that an operation that uses it stays open across a poll.
    fn yield_once() -> impl Future<Output = ()> {
        let mut yielded = false;
        poll_fn(move |cx| {
            if yielded {
                Poll::Ready(())
            } else {
                yielded = true;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        })
    }

    /// What the mocked storage operations of one test saw.
    #[derive(Clone, Default)]
    struct StorageProbe {
        /// How many operations ran.
        calls: Arc<AtomicUsize>,
        /// How many times the order's lock was taken while an operation was open.
        blocked: Arc<AtomicUsize>,
    }

    impl StorageProbe {
        /// While a mocked storage operation is open, the test tries to take the order's lock, as `my_orders` and
        /// `order_status` would.
        async fn open(&self, order: Arc<AsyncMutex<MakerOrder>>) {
            self.calls.fetch_add(1, AtomicOrdering::SeqCst);
            for _ in 0..2 {
                if order.try_lock().is_none() {
                    self.blocked.fetch_add(1, AtomicOrdering::SeqCst);
                }
                yield_once().await;
            }
        }

        fn assert_ran_without_the_lock(&self, operations: usize) {
            assert_eq!(
                self.calls.load(AtomicOrdering::SeqCst),
                operations,
                "the storage operation ran"
            );
            assert_eq!(
                self.blocked.load(AtomicOrdering::SeqCst),
                0,
                "the order's lock was taken while a storage operation was open"
            );
        }
    }

    /// The deletion from storage is held open and finds out whether the order's lock is free.
    fn mock_delete_probing(order: Arc<AsyncMutex<MakerOrder>>) -> StorageProbe {
        let probe = StorageProbe::default();
        let seen = probe.clone();
        delete_my_maker_order.mock_safe(move |_, _, _| {
            let (seen, order) = (seen.clone(), order.clone());
            MockResult::Return(Box::new(
                async move {
                    seen.open(order).await;
                    Ok(())
                }
                .boxed()
                .compat(),
            ))
        });
        probe
    }

    /// The write of a changed order is held open and finds out whether the order's lock is free.
    fn mock_write_probing(order: Arc<AsyncMutex<MakerOrder>>) -> StorageProbe {
        let probe = StorageProbe::default();
        let seen = probe.clone();
        write_maker_order_state.mock_safe(move |_, _, _| {
            let (seen, order) = (seen.clone(), order.clone());
            MockResult::Return(Box::new(
                async move {
                    seen.open(order).await;
                    Ok(())
                }
                .boxed()
                .compat(),
            ))
        });
        probe
    }

    // ---- T7: no storage operation runs under the order's lock ------------------------------------------------

    #[test]
    fn t7_balance_update_handler_deletes_outside_the_lock() {
        let node = Node::new();
        let (order, handle) = node.add_order(false, false);
        let probe = mock_delete_probing(handle);

        // The balance 0.5 supports a volume below the order's minimum: the order is cancelled.
        let handler = BalanceUpdateOrdermatchHandler::new(node.ctx.clone());
        let coin = MmCoinEnum::Test(TestCoin::new("DYN"));
        block_on(handler.balance_updated(&coin, &bd("0.5")));

        probe.assert_ran_without_the_lock(1);
        assert!(!node.in_container(&order.uuid));
    }

    #[test]
    fn t7_balance_check_deletes_outside_the_lock() {
        let node = Node::new();
        let (order, handle) = node.add_order(false, false);
        block_on(handle.lock()).max_base_vol = MmNumber::from("0.5");
        let probe = mock_delete_probing(handle);

        block_on(check_balance_for_maker_orders(node.ctx.clone(), &node.ordermatch_ctx()));

        probe.assert_ran_without_the_lock(1);
        assert!(!node.in_container(&order.uuid));
    }

    #[test]
    fn t7_timed_out_matches_are_stored_outside_the_lock() {
        let node = Node::new();
        let (order, handle) = node.add_order(false, false);
        block_on(handle.lock()).matches.insert(Uuid::new_v4(), stale_match());
        let probe = mock_write_probing(handle.clone());

        block_on(handle_timed_out_maker_matches(node.ctx.clone(), &node.ordermatch_ctx()));

        probe.assert_ran_without_the_lock(1);
        assert!(block_on(handle.lock()).matches.is_empty());
        assert!(node.in_container(&order.uuid));
    }

    #[test]
    fn t7_expired_order_is_deleted_outside_the_lock() {
        let node = Node::new();
        let (order, handle) = node.add_order(false, false);
        let probe = mock_delete_probing(handle.clone());
        // The order's own timeout has elapsed (the container's clock is the real one, so a short one is used).
        let ordermatch_ctx = node.ordermatch_ctx();
        ordermatch_ctx
            .maker_orders_ctx
            .lock()
            .orders
            .insert_expirable_unchecked(order.uuid, handle, Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(50));

        block_on(cancel_expired_maker_orders(&node.ctx, &node.ordermatch_ctx()));

        probe.assert_ran_without_the_lock(1);
        assert!(!node.in_container(&order.uuid));
    }

    #[test]
    fn t7_cancel_order_deletes_outside_the_lock() {
        let node = Node::new();
        let (order, handle) = node.add_order(false, false);
        let probe = mock_delete_probing(handle);

        block_on(cancel_order(node.ctx.clone(), CancelOrderReq { uuid: order.uuid })).unwrap();

        probe.assert_ran_without_the_lock(1);
        assert!(!node.in_container(&order.uuid));
    }

    #[test]
    fn t7_cancel_order_rpc_deletes_outside_the_lock() {
        let node = Node::new();
        let (order, handle) = node.add_order(false, false);
        let probe = mock_delete_probing(handle);

        block_on(cancel_order_rpc(node.ctx.clone(), json!({ "uuid": order.uuid }))).unwrap();

        probe.assert_ran_without_the_lock(1);
        assert!(!node.in_container(&order.uuid));
    }

    #[test]
    fn t7_cancel_previous_orders_deletes_outside_the_lock() {
        let node = Node::new();
        let (order, handle) = node.add_order(false, false);
        let probe = mock_delete_probing(handle);

        block_on(cancel_previous_maker_orders(
            &node.ctx,
            &node.ordermatch_ctx(),
            "DYN",
            "KMD",
        ));

        probe.assert_ran_without_the_lock(1);
        assert!(!node.in_container(&order.uuid));
    }

    #[test]
    fn t7_cancel_all_orders_deletes_outside_the_lock() {
        let node = Node::new();
        let (order, handle) = node.add_order(false, false);
        let probe = mock_delete_probing(handle);

        let (cancelled, _) = block_on(cancel_orders_by(&node.ctx, CancelBy::All)).unwrap();

        probe.assert_ran_without_the_lock(1);
        assert_eq!(cancelled, vec![order.uuid]);
        assert!(!node.in_container(&order.uuid));
    }

    #[test]
    fn t7_update_maker_order_stores_outside_the_lock() {
        let node = Node::new();
        let (order, handle) = node.add_order(false, false);
        let probe = mock_write_probing(handle.clone());

        let updated = block_on(update_maker_order(&node.ctx, update_price_req(order.uuid, "0.6"))).unwrap();

        probe.assert_ran_without_the_lock(1);
        assert_eq!(updated.price, MmNumber::from("0.6"));
        assert_eq!(block_on(handle.lock()).price, MmNumber::from("0.6"));
    }

    /// The reconciliation pass cancels an order whose balance cannot be read (the precedent of this change).
    #[test]
    fn t7_reconciliation_cancel_deletes_outside_the_lock() {
        let node = Node::new();
        let (order, handle) = node.add_order(false, false);
        TestCoin::my_balance.mock_safe(|_| {
            MockResult::Return(Box::new(futures01::future::result(MmError::err(
                BalanceError::Transport("down".to_owned()),
            ))))
        });
        let probe = mock_delete_probing(handle);

        block_on(reconcile_missing_own_maker_orders(&node.ctx, &node.ordermatch_ctx()));

        probe.assert_ran_without_the_lock(1);
        assert!(!node.in_container(&order.uuid));
    }

    // ---- T8: the storage order of one order ------------------------------------------------------------------

    /// A mocked write that is held open until `release`, then writes for real (as the unmocked function does) and
    /// records the price it wrote.
    #[derive(Clone, Default)]
    struct HeldWrites {
        started: Arc<AtomicUsize>,
        released: Arc<AtomicBool>,
        fail: Arc<AtomicBool>,
        written: Arc<Mutex<Vec<MmNumber>>>,
    }

    impl HeldWrites {
        /// Every write waits for `release`, then fails if `fail` is set.
        fn install(node: &Node) -> HeldWrites {
            let held = HeldWrites::default();
            let seen = held.clone();
            let ctx = node.ctx.clone();
            write_maker_order_state.mock_safe(move |_, order, with_history| {
                let (seen, ctx) = (seen.clone(), ctx.clone());
                MockResult::Return(Box::new(
                    async move {
                        seen.started.fetch_add(1, AtomicOrdering::SeqCst);
                        poll_fn(|cx| {
                            if seen.released.load(AtomicOrdering::SeqCst) {
                                Poll::Ready(())
                            } else {
                                cx.waker().wake_by_ref();
                                Poll::Pending
                            }
                        })
                        .await;
                        if seen.fail.load(AtomicOrdering::SeqCst) {
                            return MmError::err(MyOrdersError::ErrorSaving("disk full".to_owned()));
                        }
                        let storage = MyOrdersStorage::new(ctx);
                        storage.update_active_maker_order(&order).await?;
                        if with_history && order.save_in_history {
                            storage.update_maker_order_in_filtering_history(&order).await?;
                        }
                        seen.written.lock().unwrap().push(order.price.clone());
                        Ok(())
                    }
                    .boxed()
                    .compat(),
                ))
            });
            held
        }

        fn release(&self) { self.released.store(true, AtomicOrdering::SeqCst); }

        fn started(&self) -> usize { self.started.load(AtomicOrdering::SeqCst) }
    }

    /// Polls `fut` until `until` holds, with a waker that does nothing. The future must not finish before.
    fn poll_until<F: Future + Unpin>(fut: &mut F, until: impl Fn() -> bool) {
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        for _ in 0..10_000 {
            if until() {
                return;
            }
            if Pin::new(&mut *fut).poll(&mut cx).is_ready() {
                panic!("the future finished before the awaited point");
            }
        }
        panic!("the awaited point was not reached");
    }

    /// Polls `fut` a number of times, with a waker that does nothing. The future must not finish.
    fn poll_pending<F: Future + Unpin>(fut: &mut F, times: usize) {
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        for _ in 0..times {
            assert!(
                Pin::new(&mut *fut).poll(&mut cx).is_pending(),
                "the future finished while it had to wait"
            );
        }
    }

    /// T8 (a), at the gate: two states are published in the order of two changes, and their writes are completed in
    /// the reverse order. Only the newer state is stored.
    #[test]
    fn t8_a_newer_state_wins_when_writes_complete_in_reverse_order() {
        let node = Node::new();
        let (order, handle) = node.add_order(false, false);
        let written = Arc::new(Mutex::new(Vec::new()));
        let record = written.clone();
        write_maker_order_state.mock_safe(move |_, order, _| {
            record.lock().unwrap().push(order.price.clone());
            MockResult::Return(Box::new(futures01::future::ok(())))
        });
        let ordermatch_ctx = node.ordermatch_ctx();

        let (first, second) = {
            let mut locked = block_on(handle.lock());
            locked.price = MmNumber::from("0.6");
            let first = PublishedState::publish(&ordermatch_ctx, &locked, true).unwrap();
            locked.price = MmNumber::from("0.7");
            let second = PublishedState::publish(&ordermatch_ctx, &locked, true).unwrap();
            (first, second)
        };
        // The newer one completes first; the older one must not write over it.
        block_on(second.complete(&node.ctx)).unwrap();
        block_on(first.complete(&node.ctx)).unwrap();

        assert_eq!(*written.lock().unwrap(), vec![MmNumber::from("0.7")]);
        assert!(node.in_container(&order.uuid));
    }

    /// T8 (a), through the RPC: a second update runs while the first one's write is open. The writes reach storage
    /// in the order of the changes, and the newer state is the one stored.
    #[test]
    fn t8_a_two_updates_write_in_the_order_of_their_changes() {
        let node = Node::new();
        let (order, _handle) = node.add_order(true, false);
        let held = HeldWrites::install(&node);

        let mut first = Box::pin(update_maker_order(&node.ctx, update_price_req(order.uuid, "0.6")));
        poll_until(&mut first, || held.started() == 1);
        let mut second = Box::pin(update_maker_order(&node.ctx, update_price_req(order.uuid, "0.7")));
        poll_pending(&mut second, 100);
        held.release();
        let (first, second) = block_on(async { futures::join!(first, second) });

        first.unwrap();
        second.unwrap();
        let written = held.written.lock().unwrap().clone();
        assert_eq!(written, vec![MmNumber::from("0.6"), MmNumber::from("0.7")]);
        assert_eq!(node.active_record(&order.uuid).unwrap().price, MmNumber::from("0.7"));
    }

    /// T8 (b): a cancellation that runs while an update's write is open leaves no active record, and its history
    /// entry carries the updated state.
    #[test]
    fn t8_b_cancel_during_an_update_write_leaves_no_active_record() {
        let node = Node::new();
        let (order, _handle) = node.add_order(true, true);
        let held = HeldWrites::install(&node);

        let mut update = Box::pin(update_maker_order(&node.ctx, update_price_req(order.uuid, "0.6")));
        poll_until(&mut update, || held.started() == 1);
        // The order's lock is free, so the cancellation goes through up to the deletion, which waits for the write.
        let mut cancel = Box::pin(cancel_order(node.ctx.clone(), CancelOrderReq { uuid: order.uuid }));
        poll_until(&mut cancel, || !node.in_container(&order.uuid));
        assert!(
            node.active_record(&order.uuid).is_some(),
            "the deletion waits for the write"
        );
        held.release();
        let (update, cancel) = block_on(async { futures::join!(update, cancel) });

        update.unwrap();
        cancel.unwrap();
        assert!(node.active_record(&order.uuid).is_none(), "no active record");
        assert_eq!(node.history_entry(order.uuid).unwrap().price, MmNumber::from("0.6"));
    }

    /// T8 (b), the write has been published but not started when the order is cancelled: the removal writes the
    /// unwritten state before it deletes, so the history carries it, and the late completion writes nothing.
    #[test]
    fn t8_b_cancel_before_a_published_write_starts_carries_the_updated_state() {
        let node = Node::new();
        let (order, handle) = node.add_order(true, true);
        let ordermatch_ctx = node.ordermatch_ctx();

        let published = {
            let mut locked = block_on(handle.lock());
            locked.price = MmNumber::from("0.6");
            locked.updated_at = Some(now_ms());
            PublishedState::publish(&ordermatch_ctx, &locked, true).unwrap()
        };
        block_on(cancel_order(node.ctx.clone(), CancelOrderReq { uuid: order.uuid })).unwrap();
        block_on(published.complete(&node.ctx)).unwrap();

        assert!(node.active_record(&order.uuid).is_none(), "no active record");
        assert_eq!(node.history_entry(order.uuid).unwrap().price, MmNumber::from("0.6"));
    }

    /// T8 (c): the removal of timed-out matches is being stored when the order is cancelled. The record is not
    /// recreated, and the history carries the order without the matches.
    #[test]
    fn t8_c_timed_out_match_save_during_a_cancel_does_not_resurrect() {
        let node = Node::new();
        let (order, handle) = node.add_order(true, true);
        block_on(handle.lock()).matches.insert(Uuid::new_v4(), stale_match());
        let held = HeldWrites::install(&node);
        let ordermatch_ctx = node.ordermatch_ctx();

        let mut timed_out = Box::pin(handle_timed_out_maker_matches(node.ctx.clone(), &ordermatch_ctx));
        poll_until(&mut timed_out, || held.started() == 1);
        let mut cancel = Box::pin(cancel_order(node.ctx.clone(), CancelOrderReq { uuid: order.uuid }));
        poll_until(&mut cancel, || !node.in_container(&order.uuid));
        held.release();
        let ((), cancel) = block_on(async { futures::join!(timed_out, cancel) });

        cancel.unwrap();
        assert!(node.active_record(&order.uuid).is_none(), "no active record");
        let history = node.history_entry(order.uuid).unwrap();
        assert!(history.matches.is_empty());
    }

    /// T8 (c), a stale handle: a path that took the order's handle before the cancellation and changes the order
    /// afterwards publishes nothing, so nothing is written for the removed order.
    #[test]
    fn t8_c_a_stale_handle_publishes_nothing_after_the_removal() {
        let node = Node::new();
        let (order, handle) = node.add_order(true, true);
        let writes = Arc::new(AtomicUsize::new(0));
        let counted = writes.clone();
        write_maker_order_state.mock_safe(move |_, _, _| {
            counted.fetch_add(1, AtomicOrdering::SeqCst);
            MockResult::Return(Box::new(futures01::future::ok(())))
        });
        block_on(cancel_order(node.ctx.clone(), CancelOrderReq { uuid: order.uuid })).unwrap();

        let published = {
            let mut locked = block_on(handle.lock());
            locked.matches.insert(Uuid::new_v4(), stale_match());
            PublishedState::publish(&node.ordermatch_ctx(), &locked, false)
        };

        assert!(published.is_none());
        assert_eq!(writes.load(AtomicOrdering::SeqCst), 0);
        assert!(node.active_record(&order.uuid).is_none());
    }

    /// T8 (d): the write of an update fails. The RPC fails and the order in memory returns to its state before
    /// the update.
    #[test]
    fn t8_d_a_failing_update_write_restores_the_order() {
        let node = Node::new();
        let (order, handle) = node.add_order(true, false);
        let held = HeldWrites::install(&node);
        held.fail.store(true, AtomicOrdering::SeqCst);
        held.release();

        let err = block_on(update_maker_order(&node.ctx, update_price_req(order.uuid, "0.6"))).unwrap_err();

        assert!(err.contains("Error on saving updated order state"), "{}", err);
        assert_eq!(*block_on(handle.lock()), order, "back to the state before the update");
        assert_eq!(node.active_record(&order.uuid).unwrap().price, order.price);
    }

    /// T8 (d): another path changes the order while the update's write is open, and then the write fails. The RPC
    /// fails, and the later change stays.
    #[test]
    fn t8_d_a_failing_update_write_leaves_a_later_change_alone() {
        let node = Node::new();
        let (order, handle) = node.add_order(true, false);
        let held = HeldWrites::install(&node);
        held.fail.store(true, AtomicOrdering::SeqCst);

        let mut update = Box::pin(update_maker_order(&node.ctx, update_price_req(order.uuid, "0.6")));
        poll_until(&mut update, || held.started() == 1);
        // The balance handler lowers the maximum volume, say.
        block_on(handle.lock()).max_base_vol = MmNumber::from("7");
        held.release();
        let err = block_on(update).unwrap_err();

        assert!(err.contains("Error on saving updated order state"), "{}", err);
        let after = block_on(handle.lock()).clone();
        assert_eq!(after.max_base_vol, MmNumber::from("7"), "the later change stays");
        assert_eq!(after.price, MmNumber::from("0.6"), "and so does what it was applied to");
    }

    /// What an order-updated announcement carries.
    #[derive(Debug, PartialEq)]
    struct Announced {
        price: Option<MmNumber>,
        max_volume: Option<MmNumber>,
        min_volume: Option<MmNumber>,
        conf_settings: Option<OrderConfirmationsSettings>,
    }

    impl Node {
        /// The order-updated announcements published since the last call, in order.
        fn announced_updates(&mut self) -> Vec<Announced> {
            let mut announced = Vec::new();
            while let Ok(cmd) = self.p2p_rx.try_recv() {
                if let AdexBehaviourCmd::PublishMsg { msg, .. } | AdexBehaviourCmd::PublishMsgFrom { msg, .. } = cmd {
                    if let Ok((new_protocol::OrdermatchMessage::MakerOrderUpdated(update), ..)) =
                        decode_signed::<new_protocol::OrdermatchMessage>(&msg)
                    {
                        announced.push(Announced {
                            price: update.new_price(),
                            max_volume: update.new_max_volume(),
                            min_volume: update.new_min_volume(),
                            conf_settings: update.new_conf_settings(),
                        });
                    }
                }
            }
            announced
        }
    }

    /// T8 (e): the balance-update handler lowers the order's volume while an update's write is open. The update's
    /// announcement, sent after the write, carries the lowered available volume (and the update's price), so the
    /// last announcement of the volume matches the order.
    #[test]
    fn t8_e_the_update_announcement_carries_the_volume_after_a_lowering_during_the_write() {
        let mut node = Node::new();
        let (order, handle) = node.add_order(true, false);
        let held = HeldWrites::install(&node);

        let mut update = Box::pin(update_maker_order(&node.ctx, update_price_req(order.uuid, "0.6")));
        poll_until(&mut update, || held.started() == 1);
        // The balance supports 4.9 (the fee is 0.1): the maximum volume 10 is lowered and announced.
        let handler = BalanceUpdateOrdermatchHandler::new(node.ctx.clone());
        block_on(handler.balance_updated(&MmCoinEnum::Test(TestCoin::new("DYN")), &bd("5")));
        assert_eq!(block_on(handle.lock()).max_base_vol, MmNumber::from("4.9"));
        held.release();
        block_on(update).unwrap();

        let announced = node.announced_updates();
        assert_eq!(announced.len(), 2, "{:?}", announced);
        assert_eq!(announced[0].max_volume, Some(MmNumber::from("4.9")), "the lowering");
        assert_eq!(
            announced[1].max_volume,
            Some(MmNumber::from("4.9")),
            "the update, after it"
        );
        assert_eq!(announced[1].price, Some(MmNumber::from("0.6")));
        assert_eq!(block_on(handle.lock()).price, MmNumber::from("0.6"));
    }

    /// T8 (e): with no change during the write the announcement is what the update always sent: the price it set,
    /// the available volume, the confirmation settings, and no minimum volume.
    #[test]
    fn t8_e_the_update_announcement_is_unchanged_without_a_change_during_the_write() {
        let mut node = Node::new();
        let (order, _handle) = node.add_order(true, false);

        block_on(update_maker_order(&node.ctx, update_price_req(order.uuid, "0.6"))).unwrap();
        assert_eq!(node.announced_updates(), vec![Announced {
            price: Some(MmNumber::from("0.6")),
            max_volume: Some(MmNumber::from("10")),
            min_volume: None,
            conf_settings: order.conf_settings,
        }]);

        // A minimum volume is carried when the update sets it, and the price is not when it does not.
        let mut req = update_price_req(order.uuid, "0.6");
        req.new_price = None;
        req.min_volume = Some(MmNumber::from("2"));
        block_on(update_maker_order(&node.ctx, req)).unwrap();
        assert_eq!(node.announced_updates(), vec![Announced {
            price: None,
            max_volume: Some(MmNumber::from("10")),
            min_volume: Some(MmNumber::from("2")),
            conf_settings: order.conf_settings,
        }]);
    }

    /// The gate of a removed order is dropped at a later add or remove, once no writer holds it; one that a writer
    /// holds stays until then, and keeps its mark.
    #[test]
    fn t12_7_gates_of_removed_orders_are_swept_when_no_writer_holds_them() {
        let node = Node::new();
        let ordermatch_ctx = node.ordermatch_ctx();
        let (kept, _) = node.add_order(false, false);
        let (held, _) = node.add_order(false, false);
        let (dropped, _) = node.add_order(false, false);
        let held_gate = ordermatch_ctx
            .maker_orders_ctx
            .lock()
            .live_write_gate(&held.uuid)
            .unwrap();

        ordermatch_ctx.maker_orders_ctx.lock().remove_order(&held.uuid);
        ordermatch_ctx.maker_orders_ctx.lock().remove_order(&dropped.uuid);
        assert!(held_gate.is_removed(), "marked in the step that removed the order");
        assert_eq!(ordermatch_ctx.maker_orders_ctx.lock().write_gates.len(), 3);

        // The next add sweeps: `dropped` is released, `held` is kept because a writer still holds its gate.
        node.add_order(false, false);
        {
            let orders = ordermatch_ctx.maker_orders_ctx.lock();
            assert_eq!(orders.write_gates.len(), 3);
            assert!(orders.write_gates.contains_key(&kept.uuid) && orders.write_gates.contains_key(&held.uuid));
            assert!(!orders.write_gates.contains_key(&dropped.uuid));
            assert_eq!(orders.removed_gates, vec![held.uuid]);
        }
        drop(held_gate);
        node.add_order(false, false);
        let orders = ordermatch_ctx.maker_orders_ctx.lock();
        assert!(!orders.write_gates.contains_key(&held.uuid));
        assert!(orders.removed_gates.is_empty());
    }
}

/// CRD ch.59 R59.6.1a (T12): maker-order messages count as liveness for a public key the node already
/// holds a record for. Messages go through `process_msg` signed with a real key, so the signature
/// verification and the key form the store uses are exercised; the passage of time is simulated by
/// moving `last_keep_alive` into the past and by the `now` argument of `stale_pubkeys`.
mod order_message_liveness {
    use super::*;

    const WINDOW: u64 = 90;

    fn node(conf: Json) -> (MmArc, String) {
        let ctx = MmCtxBuilder::new().with_conf(conf).into_mm_arc();
        ctx.secp256k1_key_pair
            .pin(key_pair_from_seed("passphrase").unwrap())
            .unwrap();
        let own_pubkey = hex::encode(&**ctx.secp256k1_key_pair().public());
        (ctx, own_pubkey)
    }

    fn created(secret: &[u8; 32], uuid: Uuid) -> Vec<u8> {
        let msg = new_protocol::MakerOrderCreated {
            uuid: uuid.into(),
            base: "DOC".into(),
            rel: "MARTY".into(),
            price: BigRational::from_integer(1.into()),
            max_volume: BigRational::from_integer(2.into()),
            min_volume: BigRational::from_integer(0.into()),
            conf_settings: OrderConfirmationsSettings::default(),
            created_at: now_ms() / 1000,
            timestamp: now_ms() / 1000,
            pair_trie_root: H64::default(),
            base_protocol_info: vec![],
            rel_protocol_info: vec![],
        };
        encode_and_sign(&new_protocol::OrdermatchMessage::MakerOrderCreated(msg), secret).unwrap()
    }

    fn updated(secret: &[u8; 32], uuid: Uuid) -> Vec<u8> {
        let mut msg = MakerOrderUpdated::new(uuid);
        msg.with_new_price(BigRational::new(3.into(), 2.into()));
        encode_and_sign(&new_protocol::OrdermatchMessage::MakerOrderUpdated(msg), secret).unwrap()
    }

    fn cancelled(secret: &[u8; 32], uuid: Uuid) -> Vec<u8> {
        let msg = new_protocol::MakerOrderCancelled {
            uuid: uuid.into(),
            timestamp: now_ms() / 1000,
            pair_trie_root: H64::default(),
        };
        encode_and_sign(&new_protocol::OrdermatchMessage::MakerOrderCancelled(msg), secret).unwrap()
    }

    fn receive(ctx: &MmArc, msg: &[u8]) -> bool {
        let accepted = block_on(process_msg(ctx.clone(), vec![], "peer".to_owned(), msg, false));
        flush_trie(ctx);
        accepted
    }

    fn last_keep_alive(ctx: &MmArc, pubkey: &str) -> Option<u64> {
        let ordermatch_ctx = OrdermatchContext::from_ctx(ctx).unwrap();
        let trie_store = ordermatch_ctx.trie_store.lock();
        trie_store.pubkeys_state.get(pubkey).map(|s| s.last_keep_alive)
    }

    /// Simulate `secs` passing since the last liveness instant of the record.
    fn age_record(ctx: &MmArc, pubkey: &str, secs: u64) {
        let ordermatch_ctx = OrdermatchContext::from_ctx(ctx).unwrap();
        let mut trie_store = ordermatch_ctx.trie_store.lock();
        let state = trie_store.pubkeys_state.get_mut(pubkey).unwrap();
        state.last_keep_alive -= secs;
    }

    fn swept(ctx: &MmArc, own_pubkey: &str, now: u64) -> Vec<String> {
        let ordermatch_ctx = OrdermatchContext::from_ctx(ctx).unwrap();
        let my_p2p_pubkeys = ordermatch_ctx.orderbook.lock().my_p2p_pubkeys.clone();
        let trie_store = ordermatch_ctx.trie_store.lock();
        stale_pubkeys(&trie_store.pubkeys_state, own_pubkey, &my_p2p_pubkeys, WINDOW, now).1
    }

    /// A node that holds a record and one order for the maker `P` (created through `process_msg`).
    fn node_knowing_maker(conf: Json) -> (MmArc, String, String, [u8; 32], Uuid) {
        let (ctx, own_pubkey) = node(conf);
        let (maker, secret) = pubkey_and_secret_for_test("order-message-liveness-maker");
        let uuid = Uuid::new_v4();
        assert!(receive(&ctx, &created(&secret, uuid)));
        assert!(last_keep_alive(&ctx, &maker).is_some());
        (ctx, own_pubkey, maker, secret, uuid)
    }

    #[test]
    fn t12_updated_message_refreshes_a_known_maker() {
        let (ctx, _own, maker, secret, uuid) = node_knowing_maker(json!({}));
        age_record(&ctx, &maker, 80);
        let before = now_ms() / 1000;
        assert!(receive(&ctx, &updated(&secret, uuid)));
        assert!(last_keep_alive(&ctx, &maker).unwrap() >= before);
    }

    #[test]
    fn t12_cancelled_message_refreshes_a_known_maker() {
        let (ctx, _own, maker, secret, uuid) = node_knowing_maker(json!({}));
        age_record(&ctx, &maker, 80);
        let before = now_ms() / 1000;
        assert!(receive(&ctx, &cancelled(&secret, uuid)));
        assert!(last_keep_alive(&ctx, &maker).unwrap() >= before);
    }

    #[test]
    fn t12_created_message_refreshes_a_known_maker() {
        let (ctx, _own, maker, secret, _uuid) = node_knowing_maker(json!({}));
        age_record(&ctx, &maker, 80);
        let before = now_ms() / 1000;
        assert!(receive(&ctx, &created(&secret, Uuid::new_v4())));
        assert!(last_keep_alive(&ctx, &maker).unwrap() >= before);
    }

    #[test]
    fn t12_updated_and_cancelled_messages_of_an_unknown_maker_create_no_record() {
        let (ctx, _own) = node(json!({}));
        let (maker, secret) = pubkey_and_secret_for_test("order-message-liveness-unknown-maker");
        let uuid = Uuid::new_v4();
        receive(&ctx, &updated(&secret, uuid));
        receive(&ctx, &cancelled(&secret, uuid));
        assert_eq!(last_keep_alive(&ctx, &maker), None);
        let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
        assert!(!ordermatch_ctx.trie_store.lock().pubkeys_state.contains_key(&maker));
    }

    #[test]
    fn t12_updated_message_for_an_unknown_order_is_ignored_and_does_not_refresh() {
        let (ctx, _own, maker, secret, _uuid) = node_knowing_maker(json!({}));
        age_record(&ctx, &maker, 80);
        let aged = last_keep_alive(&ctx, &maker).unwrap();
        // A validly signed update from the known maker, naming an order it never had.
        assert!(!receive(&ctx, &updated(&secret, Uuid::new_v4())));
        assert_eq!(last_keep_alive(&ctx, &maker), Some(aged));
    }

    #[test]
    fn t12_a_message_of_another_key_does_not_refresh_the_maker() {
        let (ctx, _own, maker, _secret, _uuid) = node_knowing_maker(json!({}));
        let (_other, other_secret) = pubkey_and_secret_for_test("order-message-liveness-other-maker");
        age_record(&ctx, &maker, 80);
        let aged = last_keep_alive(&ctx, &maker).unwrap();
        receive(&ctx, &updated(&other_secret, Uuid::new_v4()));
        assert_eq!(last_keep_alive(&ctx, &maker), Some(aged));
    }

    #[test]
    fn t12_updates_every_few_seconds_keep_the_maker_for_150_seconds_without_keep_alives() {
        let (ctx, own, maker, secret, uuid) = node_knowing_maker(json!({}));
        let update = updated(&secret, uuid);
        for _ in 0..30 {
            // Five seconds pass; the record is never older than the window at the sweep.
            age_record(&ctx, &maker, 5);
            let now = now_ms() / 1000;
            assert!(swept(&ctx, &own, now).is_empty());
            assert!(receive(&ctx, &update));
        }
        // 150 simulated seconds have passed with no keep-alive; the orders are still in the index.
        let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
        assert!(ordermatch_ctx.orderbook.lock().order_set.contains_key(&uuid));
        let last = last_keep_alive(&ctx, &maker).unwrap();
        assert!(swept(&ctx, &own, last + WINDOW - 1).is_empty());
    }

    #[test]
    fn t12_a_maker_silent_past_the_window_is_selected() {
        let (ctx, own, maker, secret, uuid) = node_knowing_maker(json!({}));
        assert!(receive(&ctx, &updated(&secret, uuid)));
        let last = last_keep_alive(&ctx, &maker).unwrap();
        assert!(swept(&ctx, &own, last + WINDOW - 1).is_empty());
        assert_eq!(swept(&ctx, &own, last + WINDOW), vec![maker]);
    }

    #[test]
    fn t12_switch_off_leaves_liveness_to_keep_alives() {
        let (ctx, own, maker, secret, uuid) = node_knowing_maker(json!({ "order_messages_refresh_liveness": false }));
        age_record(&ctx, &maker, 60);
        let aged = last_keep_alive(&ctx, &maker).unwrap();
        assert!(receive(&ctx, &updated(&secret, uuid)));
        assert!(receive(&ctx, &created(&secret, Uuid::new_v4())));
        assert!(receive(&ctx, &cancelled(&secret, uuid)));
        assert_eq!(last_keep_alive(&ctx, &maker), Some(aged));
        assert_eq!(swept(&ctx, &own, aged + WINDOW), vec![maker]);
    }

    #[test]
    fn t12_only_a_json_false_turns_the_switch_off() {
        for (conf, expected) in [
            (json!({}), true),
            (json!({ "order_messages_refresh_liveness": true }), true),
            (json!({ "order_messages_refresh_liveness": "false" }), true),
            (json!({ "order_messages_refresh_liveness": 0 }), true),
            (json!({ "order_messages_refresh_liveness": false }), false),
        ] {
            let ctx = MmCtxBuilder::new().with_conf(conf).into_mm_arc();
            assert_eq!(order_messages_refresh_liveness(&ctx), expected);
        }
    }
}

/// CRD ch.32 R-Q5b, T-Q5b: a request for a pair the node is still synchronizing is parked and answered
/// when the node's own synchronization lands.
mod sync_answer_deferral_tests {
    use super::*;
    use crate::mm2::lp_ordermatch::sync_answer_deferral::{answer_orderbook_request_within,
                                                          orderbook_answer_waits_for_worker,
                                                          sync_answer_deferral_before_placeholder,
                                                          sync_answer_deferral_max_per_peer,
                                                          sync_answer_deferral_max_total, CEILING_MAX_PARKED_PER_PEER,
                                                          CEILING_MAX_PARKED_TOTAL, DEFAULT_MAX_PARKED_PER_PEER,
                                                          DEFAULT_MAX_PARKED_TOTAL, DEFAULT_SYNC_ANSWER_DEFERRAL_MS,
                                                          DEFAULT_SYNC_REQUEST_RETRY_MS};
    use crate::mm2::lp_ordermatch::sync_log::{AltReason, Attempt, RequestClass};
    use futures::channel::oneshot;
    use mm2_p2p::atomicdex_behaviour::AdexResponse;
    use mm2_p2p::request_response::LocalFailure;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::mpsc as std_mpsc;
    use std::task::Context;
    use std::thread::JoinHandle;
    use std::time::Instant;

    const MAKER: &str = "02aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const PAIR: &str = "C1:C2";
    const OTHER_PAIR: &str = "C3:C4";
    const ORDERS: usize = 5;
    /// A bound no test is meant to reach, unless it is the one that tests the bound.
    const LONG_BOUND_MS: u64 = 5000;

    type OwnResponseTx = oneshot::Sender<Vec<(PeerId, AdexResponse)>>;
    type Answers = std_mpsc::Sender<(usize, AdexResponse)>;

    fn node(conf: Json) -> (MmArc, mpsc::Receiver<AdexBehaviourCmd>) {
        let ctx = MmCtxBuilder::new().with_conf(conf).into_mm_arc();
        ctx.init_metrics().unwrap();
        let (tx, rx) = mpsc::channel(1024);
        P2PContext::new(tx).store_to_mm_arc(&ctx);
        (ctx, rx)
    }

    fn next_cmd(rx: &mut mpsc::Receiver<AdexBehaviourCmd>) -> AdexBehaviourCmd {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match rx.try_recv() {
                Ok(cmd) => return cmd,
                Err(_) => {
                    assert!(Instant::now() < deadline, "no command was sent");
                    std::thread::sleep(Duration::from_millis(1));
                },
            }
        }
    }

    /// `conf` with the request to another relay (R-Q5d) off, unless it sets the switch itself.
    fn without_alternate_relay(mut conf: Json) -> Json {
        if let Some(conf) = conf.as_object_mut() {
            conf.entry("sync_request_alternate_relay").or_insert(json!(false));
        }
        conf
    }

    fn octx(ctx: &MmArc) -> Arc<OrdermatchContext> { OrdermatchContext::from_ctx(ctx).unwrap() }

    /// The tables of the node are empty (the in-flight record ends on a task of its own, so wait for it).
    pub(super) fn assert_tables_empty(ctx: &MmArc) {
        let deferral = octx(ctx).sync_answer_deferral.clone();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline
            && (deferral.parked_len() != 0 || deferral.in_flight_len() != 0 || deferral.registrations_len() != 0)
        {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(deferral.parked_len(), 0, "parked table");
        assert!(deferral.parked_counts_agree(), "per-peer count of the parked table");
        assert_eq!(deferral.in_flight_len(), 0, "in-flight table");
        assert_eq!(deferral.registrations_len(), 0, "registrations table");
        assert!(
            deferral.registration_counts_agree(),
            "per-peer count and index of the registrations table"
        );
        // The watch of the keep-alive requests (it ends with the check of trigger (b), on a task of its own).
        let watch = octx(ctx).mismatch_watch.clone();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && !watch.is_empty() {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(watch.is_empty(), "mismatch watch table");
    }

    /// The uuids of the maker's orders in the node's read index.
    fn maker_book(ctx: &MmArc) -> HashSet<Uuid> {
        octx(ctx)
            .orderbook
            .lock()
            .order_set
            .values()
            .filter(|o| o.pubkey == MAKER)
            .map(|o| o.uuid)
            .collect()
    }

    /// The all-zero placeholder entry that the keep-alive path creates, without any request behind it.
    fn add_placeholder(ctx: &MmArc, pubkey: &str, pair: &str) {
        let octx = octx(ctx);
        let mut trie_store = octx.trie_store.lock();
        let state = pubkey_state_mut(&mut trie_store.pubkeys_state, pubkey);
        order_pair_root_mut(&mut state.trie_roots, pair);
    }

    /// The eviction of `lp_ordermatch_loop` for the maker (after 90 s without a keep-alive).
    fn wipe_maker(ctx: &MmArc) {
        let octx = octx(ctx);
        octx.trie_store
            .lock()
            .pubkeys_state
            .get_mut(MAKER)
            .unwrap()
            .last_keep_alive = 0;
        let (uuids, pubkeys) = {
            let trie_store = octx.trie_store.lock();
            stale_pubkeys(&trie_store.pubkeys_state, "own", &HashSet::new(), 90, now_ms() / 1000)
        };
        assert_eq!(pubkeys, vec![MAKER.to_owned()]);
        let mut ops = Vec::new();
        let mut orderbook = octx.orderbook.lock();
        for uuid in uuids {
            ops.extend(orderbook.index_remove(uuid).map(|(_, op)| op));
        }
        ops.push(TrieOp::RemovePubkey {
            pubkey: MAKER.to_owned(),
        });
        orderbook.queue_trie_ops(&octx.trie_ops_tx, ops);
        drop(orderbook);
        flush_trie(ctx);
    }

    /// Ask `relay` for `pairs` of `pubkey` as the peer `peer` does; the answer lands in `answers` under `id`.
    fn ask(
        relay: &MmArc,
        peer: PeerId,
        id: usize,
        pubkey: &str,
        pairs: &[&str],
        answers: &Answers,
    ) -> Result<(), String> {
        let roots: HashMap<String, H64> = pairs.iter().map(|p| (p.to_string(), H64::default())).collect();
        let answers = answers.clone();
        let reply: SyncAnswerReply = Box::new(move |res| answers.send((id, res)).map_err(|e| e.to_string()));
        answer_sync_request(relay, peer, pubkey.to_owned(), roots, reply)
    }

    fn full_set_len(res: &AdexResponse) -> usize {
        let response = match res {
            AdexResponse::Ok { response } => response,
            other => panic!("expected an answer, got {:?}", other),
        };
        let mut answer: SyncPubkeyOrderbookStateRes = decode_message(response).unwrap();
        match answer.pair_orders_diff.remove(PAIR).expect("the pair is answered") {
            DeltaOrFullTrie::FullTrie(orders) => orders.len(),
            DeltaOrFullTrie::Delta(_) => panic!("expected a full snapshot"),
        }
    }

    fn assert_placeholder_error(res: &AdexResponse) {
        match res {
            AdexResponse::Err { error } => assert!(error.contains("InvalidStateRoot"), "{}", error),
            other => panic!("expected the placeholder error, got {:?}", other),
        }
    }

    /// The relay's own keep-alive-driven request, started and held: the test decides how it ends.
    struct OwnSync {
        keep_alive_task: JoinHandle<bool>,
        request: Vec<u8>,
        response_tx: Option<OwnResponseTx>,
    }

    impl OwnSync {
        /// The request is answered by `maker`.
        fn answer(&mut self, maker: &MmArc) {
            let (pubkey, trie_roots) = match decode_message::<P2PRequest>(&self.request).unwrap() {
                P2PRequest::Ordermatch(OrdermatchRequest::SyncPubkeyOrderbookState { pubkey, trie_roots }) => {
                    (pubkey, trie_roots)
                },
                other => panic!("unexpected request {:?}", other),
            };
            let answer = process_sync_pubkey_orderbook_state(maker.clone(), pubkey, trie_roots)
                .unwrap()
                .unwrap();
            let res = AdexResponse::Ok {
                response: encode_message(&answer).unwrap(),
            };
            self.response_tx
                .take()
                .unwrap()
                .send(vec![(PeerId::random(), res)])
                .unwrap();
        }

        /// The request fails (the transport drops the response channel).
        fn fail(&mut self) { drop(self.response_tx.take()); }

        /// The keep-alive task ends; whether it reported success (the forward of R-F3b).
        fn finish(self) -> bool { self.keep_alive_task.join().unwrap() }
    }

    struct Fixture {
        relay: MmArc,
        relay_cmds: mpsc::Receiver<AdexBehaviourCmd>,
        maker: MmArc,
        uuids: HashSet<Uuid>,
    }

    impl Fixture {
        /// A relay with the given config and a maker node holding `ORDERS` orders for `PAIR`.
        fn new(conf: Json) -> Self {
            // The tests of R-Q5b and R-Q5c run with no other relay connected (T-Q5c, T-Q6); the tests of
            // R-Q5d switch the rule on.
            let (relay, relay_cmds) = node(without_alternate_relay(conf));
            let (maker, _maker_cmds) = node(json!({}));
            let orders = make_random_orders(MAKER.to_owned(), &[0; 32], "C1".into(), "C2".into(), ORDERS);
            let uuids = orders.iter().map(|o| o.uuid).collect();
            for order in orders {
                insert_or_update_order(&maker, order);
            }
            flush_trie(&maker);
            Fixture {
                relay,
                relay_cmds,
                maker,
                uuids,
            }
        }

        /// What the maker advertises now.
        fn keep_alive(&self) -> PubkeyKeepAlive {
            let roots = octx(&self.maker)
                .trie_store
                .lock()
                .pubkeys_state
                .get(MAKER)
                .map(|s| s.trie_roots.clone())
                .unwrap_or_default();
            PubkeyKeepAlive {
                trie_roots: advertisable_roots(&roots),
                timestamp: now_ms() / 1000,
            }
        }

        /// The maker's keep-alive reaches the relay, which sends its own request and waits for the answer.
        fn start_own_sync(&mut self) -> OwnSync {
            let relay = self.relay.clone();
            let keep_alive = self.keep_alive();
            let keep_alive_task = std::thread::spawn(move || {
                block_on(process_orders_keep_alive(
                    relay,
                    "forwarding-peer".to_owned(),
                    MAKER.to_owned(),
                    keep_alive,
                    true,
                ))
            });
            match next_cmd(&mut self.relay_cmds) {
                AdexBehaviourCmd::RequestPeers { req, response_tx, .. } => OwnSync {
                    keep_alive_task,
                    request: req,
                    response_tx: Some(response_tx),
                },
                other => panic!("unexpected command {:?}", other),
            }
        }

        /// The second request of R-Q5c, which takes the place of the first in `own`.
        fn expect_second_request(&mut self, own: &mut OwnSync) {
            match next_cmd(&mut self.relay_cmds) {
                AdexBehaviourCmd::RequestPeers { req, response_tx, .. } => {
                    own.request = req;
                    own.response_tx = Some(response_tx);
                },
                other => panic!("unexpected command {:?}", other),
            }
        }

        /// Nothing more was asked (to be called once the keep-alive task has finished).
        fn assert_no_further_request(&mut self) {
            while let Ok(cmd) = self.relay_cmds.try_recv() {
                assert!(
                    !matches!(cmd, AdexBehaviourCmd::RequestPeers { .. }),
                    "a further request was sent"
                );
            }
        }
    }

    fn peer() -> PeerId { PeerId::random() }

    /// A relay config without the second request of R-Q5c, for the tests of R-Q5b alone.
    fn no_retry() -> Json { json!({ "sync_request_retry_ms": 0 }) }

    fn answered(answers: &std_mpsc::Receiver<(usize, AdexResponse)>, within_ms: u64) -> (usize, AdexResponse) {
        answers
            .recv_timeout(Duration::from_millis(within_ms))
            .expect("the request was not answered in time")
    }

    #[test]
    fn parked_request_is_answered_with_the_full_set_once_the_own_answer_is_applied() {
        let mut f = Fixture::new(json!({}));
        let (answers_tx, answers) = std_mpsc::channel();
        let mut own = f.start_own_sync();

        let started = std::time::Instant::now();
        ask(&f.relay, peer(), 1, MAKER, &[PAIR], &answers_tx).unwrap();
        assert!(answers.try_recv().is_err(), "the request is parked, not answered");
        assert_eq!(octx(&f.relay).sync_answer_deferral.parked_len(), 1);
        assert_eq!(octx(&f.relay).sync_answer_deferral.in_flight_len(), 1);

        own.answer(&f.maker);
        let (id, res) = answered(&answers, 2000);
        assert!(started.elapsed() < Duration::from_millis(LONG_BOUND_MS / 2));
        assert_eq!(id, 1);
        assert_eq!(full_set_len(&res), ORDERS);
        assert!(own.finish());
        assert_eq!(maker_book(&f.relay), f.uuids);
        assert_tables_empty(&f.relay);
    }

    #[test]
    fn parked_request_gets_the_placeholder_error_when_the_own_request_fails() {
        let mut f = Fixture::new(no_retry());
        let (answers_tx, answers) = std_mpsc::channel();
        let mut own = f.start_own_sync();
        ask(&f.relay, peer(), 1, MAKER, &[PAIR], &answers_tx).unwrap();
        assert!(answers.try_recv().is_err());

        let started = std::time::Instant::now();
        own.fail();
        let (_, res) = answered(&answers, 2000);
        assert!(started.elapsed() < Duration::from_millis(LONG_BOUND_MS / 2));
        assert_placeholder_error(&res);
        assert!(!own.finish(), "a failed request is not forwarded");
        assert_tables_empty(&f.relay);
    }

    #[test]
    fn parked_request_gets_the_placeholder_error_when_the_bound_expires() {
        const BOUND_MS: u64 = 300;
        let mut f = Fixture::new(json!({ "sync_answer_deferral_ms": BOUND_MS }));
        let (answers_tx, answers) = std_mpsc::channel();
        // The own request never answers during the test.
        let mut own = f.start_own_sync();

        let started = std::time::Instant::now();
        ask(&f.relay, peer(), 1, MAKER, &[PAIR], &answers_tx).unwrap();
        assert!(answers.try_recv().is_err());
        let (_, res) = answered(&answers, 5000);
        let waited = started.elapsed();
        assert!(
            waited >= Duration::from_millis(BOUND_MS - 50),
            "answered after {:?}",
            waited
        );
        assert!(
            waited < Duration::from_millis(BOUND_MS * 5),
            "answered after {:?}",
            waited
        );
        assert_placeholder_error(&res);
        // The parked slot is free; the own request is still in flight, as it has not ended.
        assert_eq!(octx(&f.relay).sync_answer_deferral.parked_len(), 0);
        assert_eq!(octx(&f.relay).sync_answer_deferral.in_flight_len(), 1);

        own.fail();
        assert!(!own.finish());
        assert_tables_empty(&f.relay);
    }

    #[test]
    fn requests_that_are_not_deferrable_are_answered_at_once() {
        let f = Fixture::new(json!({}));
        let (answers_tx, answers) = std_mpsc::channel();

        // A placeholder with no own request in flight.
        add_placeholder(&f.relay, MAKER, PAIR);
        ask(&f.relay, peer(), 1, MAKER, &[PAIR], &answers_tx).unwrap();
        assert_placeholder_error(&answers.try_recv().expect("answered at once").1);

        // A pair the relay does not track.
        ask(&f.relay, peer(), 2, MAKER, &["C9:C8"], &answers_tx).unwrap();
        match answers.try_recv().expect("answered at once").1 {
            AdexResponse::Err { error } => assert!(error.contains("No pair trie root for C9:C8"), "{}", error),
            other => panic!("unexpected answer {:?}", other),
        }

        // A pubkey the relay has no data for.
        ask(&f.relay, peer(), 3, "03unknown", &[PAIR], &answers_tx).unwrap();
        assert_eq!(answers.try_recv().expect("answered at once").1, AdexResponse::None);

        assert!(answers.try_recv().is_err());
        assert_tables_empty(&f.relay);
    }

    #[test]
    fn request_with_a_non_deferrable_pair_is_answered_at_once_with_the_placeholder_error() {
        let mut f = Fixture::new(json!({}));
        let (answers_tx, answers) = std_mpsc::channel();
        let mut own = f.start_own_sync();
        // The pair of the own request is waited for; this one has a placeholder and no own request.
        add_placeholder(&f.relay, MAKER, OTHER_PAIR);

        ask(&f.relay, peer(), 1, MAKER, &[PAIR, OTHER_PAIR], &answers_tx).unwrap();
        assert_placeholder_error(&answers.try_recv().expect("answered at once").1);
        // The same with a pair the relay does not track.
        ask(&f.relay, peer(), 2, MAKER, &[PAIR, "C9:C8"], &answers_tx).unwrap();
        assert!(matches!(
            answers.try_recv().expect("answered at once").1,
            AdexResponse::Err { .. }
        ));
        assert_eq!(octx(&f.relay).sync_answer_deferral.parked_len(), 0);

        // The own request is unaffected and a request for its pair alone is still parked.
        ask(&f.relay, peer(), 3, MAKER, &[PAIR], &answers_tx).unwrap();
        assert!(answers.try_recv().is_err());
        own.answer(&f.maker);
        assert_eq!(full_set_len(&answered(&answers, 2000).1), ORDERS);
        assert!(own.finish());
        assert_tables_empty(&f.relay);
    }

    /// The relay config with the limits of R-Q5b item 4 set small.
    fn small_limits(per_peer: usize, total: usize) -> Json {
        json!({ "sync_answer_deferral_max_per_peer": per_peer, "sync_answer_deferral_max_total": total })
    }

    #[test]
    fn a_request_past_the_per_peer_limit_is_answered_at_once() {
        let mut f = Fixture::new(small_limits(2, 3));
        let (answers_tx, answers) = std_mpsc::channel();
        let mut own = f.start_own_sync();

        let greedy = peer();
        for id in 0..2 {
            ask(&f.relay, greedy, id, MAKER, &[PAIR], &answers_tx).unwrap();
        }
        assert!(answers.try_recv().is_err());
        assert_eq!(octx(&f.relay).sync_answer_deferral.parked_len(), 2);

        // The third of that peer is answered at once; another peer is still served by parking.
        ask(&f.relay, greedy, 100, MAKER, &[PAIR], &answers_tx).unwrap();
        let (id, res) = answers.try_recv().expect("answered at once");
        assert_eq!(id, 100);
        assert_placeholder_error(&res);
        ask(&f.relay, peer(), 101, MAKER, &[PAIR], &answers_tx).unwrap();
        assert!(answers.try_recv().is_err());
        assert_eq!(octx(&f.relay).sync_answer_deferral.parked_len(), 3);

        own.answer(&f.maker);
        let mut ids: Vec<usize> = (0..3)
            .map(|_| {
                let (id, res) = answered(&answers, 3000);
                assert_eq!(full_set_len(&res), ORDERS);
                id
            })
            .collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![0, 1, 101]);
        assert!(own.finish());
        assert_tables_empty(&f.relay);
    }

    #[test]
    fn a_request_past_the_total_limit_is_answered_at_once() {
        let mut f = Fixture::new(small_limits(2, 3));
        let (answers_tx, answers) = std_mpsc::channel();
        let mut own = f.start_own_sync();

        for id in 0..3 {
            // One request per peer, so only the total limit can stop the last one.
            ask(&f.relay, peer(), id, MAKER, &[PAIR], &answers_tx).unwrap();
        }
        assert!(answers.try_recv().is_err());
        assert_eq!(octx(&f.relay).sync_answer_deferral.parked_len(), 3);

        ask(&f.relay, peer(), 1000, MAKER, &[PAIR], &answers_tx).unwrap();
        let (id, res) = answers.try_recv().expect("answered at once");
        assert_eq!(id, 1000);
        assert_placeholder_error(&res);
        assert_eq!(octx(&f.relay).sync_answer_deferral.parked_len(), 3);

        own.answer(&f.maker);
        for _ in 0..3 {
            assert_eq!(full_set_len(&answered(&answers, 5000).1), ORDERS);
        }
        assert!(own.finish());
        assert_tables_empty(&f.relay);
    }

    #[test]
    fn the_default_per_peer_limit_parks_32_requests_and_answers_the_33rd_at_once() {
        let mut f = Fixture::new(json!({}));
        let (answers_tx, answers) = std_mpsc::channel();
        let mut own = f.start_own_sync();

        let greedy = peer();
        for id in 0..DEFAULT_MAX_PARKED_PER_PEER {
            ask(&f.relay, greedy, id, MAKER, &[PAIR], &answers_tx).unwrap();
        }
        assert!(answers.try_recv().is_err());
        assert_eq!(
            octx(&f.relay).sync_answer_deferral.parked_len(),
            DEFAULT_MAX_PARKED_PER_PEER
        );

        ask(&f.relay, greedy, 100, MAKER, &[PAIR], &answers_tx).unwrap();
        let (id, res) = answers.try_recv().expect("answered at once");
        assert_eq!(id, 100);
        assert_placeholder_error(&res);

        own.answer(&f.maker);
        for _ in 0..DEFAULT_MAX_PARKED_PER_PEER {
            assert_eq!(full_set_len(&answered(&answers, 5000).1), ORDERS);
        }
        assert!(own.finish());
        assert_tables_empty(&f.relay);
    }

    #[test]
    fn the_per_peer_limit_is_the_same_for_a_peer_across_pairs_and_requests() {
        // A request for two tracked pairs counts once, whatever its size.
        let mut f = Fixture::new(json!({
            "sync_request_retry_ms": 0,
            "sync_answer_deferral_max_per_peer": 2,
        }));
        let (answers_tx, answers) = std_mpsc::channel();
        let mut own = f.start_own_sync();
        let p = peer();
        for id in 0..2 {
            ask(&f.relay, p, id, MAKER, &[PAIR, PAIR], &answers_tx).unwrap();
        }
        assert_eq!(octx(&f.relay).sync_answer_deferral.parked_len(), 2);
        ask(&f.relay, p, 100, MAKER, &[PAIR], &answers_tx).unwrap();
        assert_placeholder_error(&answers.try_recv().expect("answered at once").1);
        own.fail();
        for _ in 0..2 {
            assert_placeholder_error(&answered(&answers, 3000).1);
        }
        assert!(!own.finish());
        assert_tables_empty(&f.relay);
    }

    #[test]
    fn zero_turns_deferral_off() {
        let mut f = Fixture::new(json!({ "sync_answer_deferral_ms": 0 }));
        let (answers_tx, answers) = std_mpsc::channel();
        let mut own = f.start_own_sync();
        // Nothing is tracked while deferral is off.
        assert_eq!(octx(&f.relay).sync_answer_deferral.in_flight_len(), 0);

        ask(&f.relay, peer(), 1, MAKER, &[PAIR], &answers_tx).unwrap();
        assert_placeholder_error(&answers.try_recv().expect("answered at once").1);
        assert_eq!(octx(&f.relay).sync_answer_deferral.parked_len(), 0);

        own.answer(&f.maker);
        assert!(own.finish());
        flush_trie(&f.relay);
        ask(&f.relay, peer(), 2, MAKER, &[PAIR], &answers_tx).unwrap();
        assert_eq!(full_set_len(&answers.try_recv().expect("answered at once").1), ORDERS);
        assert_tables_empty(&f.relay);
    }

    #[test]
    fn only_a_non_negative_integer_sets_the_bound() {
        for (conf, expected) in [
            (json!({}), DEFAULT_SYNC_ANSWER_DEFERRAL_MS),
            (json!({ "sync_answer_deferral_ms": 0 }), 0),
            (json!({ "sync_answer_deferral_ms": 1200 }), 1200),
            (
                json!({ "sync_answer_deferral_ms": "0" }),
                DEFAULT_SYNC_ANSWER_DEFERRAL_MS,
            ),
            (
                json!({ "sync_answer_deferral_ms": false }),
                DEFAULT_SYNC_ANSWER_DEFERRAL_MS,
            ),
            (
                json!({ "sync_answer_deferral_ms": -1 }),
                DEFAULT_SYNC_ANSWER_DEFERRAL_MS,
            ),
            (
                json!({ "sync_answer_deferral_ms": 2.5 }),
                DEFAULT_SYNC_ANSWER_DEFERRAL_MS,
            ),
        ] {
            let ctx = MmCtxBuilder::new().with_conf(conf).into_mm_arc();
            assert_eq!(sync_answer_deferral_ms(&ctx), expected);
        }
        assert_eq!(DEFAULT_SYNC_ANSWER_DEFERRAL_MS, 5000);
        assert_eq!((DEFAULT_MAX_PARKED_PER_PEER, DEFAULT_MAX_PARKED_TOTAL), (32, 2048));
        assert_eq!((CEILING_MAX_PARKED_PER_PEER, CEILING_MAX_PARKED_TOTAL), (64, 8192));
    }

    #[test]
    fn only_a_positive_integer_sets_a_parking_limit_and_it_is_capped_at_the_ceiling() {
        type Reader = fn(&MmArc) -> usize;
        let readers: [(&str, Reader, usize, usize); 2] = [
            (
                "sync_answer_deferral_max_per_peer",
                sync_answer_deferral_max_per_peer,
                DEFAULT_MAX_PARKED_PER_PEER,
                CEILING_MAX_PARKED_PER_PEER,
            ),
            (
                "sync_answer_deferral_max_total",
                sync_answer_deferral_max_total,
                DEFAULT_MAX_PARKED_TOTAL,
                CEILING_MAX_PARKED_TOTAL,
            ),
        ];
        for (key, reader, default, ceiling) in readers {
            let read = |conf: Json| reader(&MmCtxBuilder::new().with_conf(conf).into_mm_arc());
            assert_eq!(read(json!({})), default, "{} absent", key);
            for value in [json!(0), json!("5"), json!(-1), json!(2.5), json!(false)] {
                assert_eq!(read(json!({ key: value.clone() })), default, "{} = {}", key, value);
            }
            assert_eq!(read(json!({ key: 5 })), 5, "{} = 5", key);
            assert_eq!(read(json!({ key: ceiling })), ceiling, "{} = ceiling", key);
            assert_eq!(read(json!({ key: ceiling + 1 })), ceiling, "{} above the ceiling", key);
        }
        let read = |conf: Json| MmCtxBuilder::new().with_conf(conf).into_mm_arc();
        assert_eq!(
            sync_answer_deferral_max_per_peer(&read(json!({ "sync_answer_deferral_max_per_peer": 100 }))),
            64
        );
        assert_eq!(
            sync_answer_deferral_max_total(&read(json!({ "sync_answer_deferral_max_total": 10000 }))),
            8192
        );
        assert_eq!((DEFAULT_MAX_PARKED_PER_PEER, DEFAULT_MAX_PARKED_TOTAL), (32, 2048));
    }

    /// The pause of R-Q5c in the tests of the second request.
    const RETRY_MS: u64 = 50;

    fn quick_retry() -> Json { json!({ "sync_request_retry_ms": RETRY_MS }) }

    /// The pairs and roots a request names.
    fn requested_roots(request: &[u8]) -> HashMap<String, H64> {
        match decode_message::<P2PRequest>(request).unwrap() {
            P2PRequest::Ordermatch(OrdermatchRequest::SyncPubkeyOrderbookState { trie_roots, .. }) => trie_roots,
            other => panic!("unexpected request {:?}", other),
        }
    }

    #[test]
    fn a_quick_failure_is_asked_once_more_and_the_parked_request_gets_the_full_set() {
        let mut f = Fixture::new(quick_retry());
        let (answers_tx, answers) = std_mpsc::channel();
        let mut own = f.start_own_sync();
        ask(&f.relay, peer(), 1, MAKER, &[PAIR], &answers_tx).unwrap();

        own.fail();
        f.expect_second_request(&mut own);
        assert_eq!(
            requested_roots(&own.request),
            HashMap::from([(PAIR.to_owned(), H64::default())]),
            "the second request names the pair still at the placeholder"
        );
        assert!(
            answers.try_recv().is_err(),
            "the parked request waits for the second answer"
        );
        assert_eq!(octx(&f.relay).sync_answer_deferral.in_flight_len(), 1);

        own.answer(&f.maker);
        let (id, res) = answered(&answers, 2000);
        assert_eq!(id, 1);
        assert_eq!(full_set_len(&res), ORDERS);
        assert!(own.finish(), "an answered second request counts as success (R-F3b)");
        assert_eq!(maker_book(&f.relay), f.uuids);
        f.assert_no_further_request();
        assert_tables_empty(&f.relay);
    }

    #[test]
    fn the_second_request_is_the_last() {
        let mut f = Fixture::new(quick_retry());
        let (answers_tx, answers) = std_mpsc::channel();
        let mut own = f.start_own_sync();
        ask(&f.relay, peer(), 1, MAKER, &[PAIR], &answers_tx).unwrap();

        own.fail();
        f.expect_second_request(&mut own);
        own.fail();
        assert_placeholder_error(&answered(&answers, 2000).1);
        assert!(!own.finish());
        f.assert_no_further_request();
        assert_tables_empty(&f.relay);
    }

    #[test]
    fn a_pair_that_left_the_placeholder_is_not_asked_for_again() {
        const PAUSE_MS: u64 = 300;
        let mut f = Fixture::new(json!({ "sync_request_retry_ms": PAUSE_MS }));
        let mut own = f.start_own_sync();
        own.fail();

        // During the pause the maker's orders reach the relay by gossip.
        let orders: Vec<OrderbookItem> = octx(&f.maker)
            .orderbook
            .lock()
            .order_set
            .values()
            .filter(|o| o.pubkey == MAKER)
            .cloned()
            .collect();
        for order in orders {
            insert_or_update_order(&f.relay, order);
        }
        flush_trie(&f.relay);

        assert!(!own.finish());
        f.assert_no_further_request();
        assert_eq!(maker_book(&f.relay), f.uuids);
        assert_tables_empty(&f.relay);
    }

    #[test]
    fn no_second_request_when_its_answer_could_not_arrive_within_the_bound() {
        // A pause as long as the bound.
        let mut f = Fixture::new(json!({ "sync_request_retry_ms": DEFAULT_SYNC_ANSWER_DEFERRAL_MS }));
        let mut own = f.start_own_sync();
        own.fail();
        assert!(!own.finish());
        f.assert_no_further_request();

        // A failure that comes late: 250 ms after the request, with a 100 ms pause and a 300 ms bound.
        let mut f = Fixture::new(json!({ "sync_answer_deferral_ms": 300, "sync_request_retry_ms": 100 }));
        let mut own = f.start_own_sync();
        std::thread::sleep(Duration::from_millis(250));
        own.fail();
        assert!(!own.finish());
        f.assert_no_further_request();
        assert_tables_empty(&f.relay);
    }

    #[test]
    fn zero_turns_the_second_request_off_and_so_does_a_zero_bound() {
        for conf in [no_retry(), json!({ "sync_answer_deferral_ms": 0 })] {
            let mut f = Fixture::new(conf);
            let mut own = f.start_own_sync();
            own.fail();
            assert!(!own.finish());
            f.assert_no_further_request();
            assert_tables_empty(&f.relay);
        }
    }

    #[test]
    fn only_a_non_negative_integer_sets_the_pause() {
        for (conf, expected) in [
            (json!({}), DEFAULT_SYNC_REQUEST_RETRY_MS),
            (json!({ "sync_request_retry_ms": 0 }), 0),
            (json!({ "sync_request_retry_ms": 700 }), 700),
            (json!({ "sync_request_retry_ms": "0" }), DEFAULT_SYNC_REQUEST_RETRY_MS),
            (json!({ "sync_request_retry_ms": -1 }), DEFAULT_SYNC_REQUEST_RETRY_MS),
            (json!({ "sync_request_retry_ms": 2.5 }), DEFAULT_SYNC_REQUEST_RETRY_MS),
        ] {
            let ctx = MmCtxBuilder::new().with_conf(conf).into_mm_arc();
            assert_eq!(sync_request_retry_ms(&ctx), expected);
        }
        assert_eq!(DEFAULT_SYNC_REQUEST_RETRY_MS, 1000);
    }

    #[test]
    fn the_inbound_handler_returns_without_waiting() {
        let mut f = Fixture::new(no_retry());
        let (answers_tx, answers) = std_mpsc::channel();
        let mut own = f.start_own_sync();

        let started = std::time::Instant::now();
        ask(&f.relay, peer(), 1, MAKER, &[PAIR], &answers_tx).unwrap();
        let handler_time = started.elapsed();
        assert!(
            handler_time < Duration::from_millis(LONG_BOUND_MS / 10),
            "the handler took {:?} with the request parked for up to {} ms",
            handler_time,
            LONG_BOUND_MS
        );
        assert!(answers.try_recv().is_err());

        own.fail();
        assert_placeholder_error(&answered(&answers, 2000).1);
        assert!(!own.finish());
        assert_tables_empty(&f.relay);
    }

    #[test]
    fn a_cancelled_keep_alive_task_does_not_leave_the_record_behind() {
        let f = Fixture::new(json!({}));
        let (answers_tx, answers) = std_mpsc::channel();
        let keep_alive = f.keep_alive();
        let relay = f.relay.clone();
        // The task is polled until it has sent its request, then dropped without ever finishing.
        let mut task = Box::pin(process_orders_keep_alive(
            relay,
            "forwarding-peer".to_owned(),
            MAKER.to_owned(),
            keep_alive,
            true,
        ));
        let mut cmds = f.relay_cmds;
        block_on(async {
            let sent = async { cmds.next().await };
            futures::pin_mut!(sent);
            match futures::future::select(task.as_mut(), sent).await {
                futures::future::Either::Right((Some(AdexBehaviourCmd::RequestPeers { .. }), _)) => (),
                _ => panic!("the request should have been sent first"),
            }
        });
        assert_eq!(octx(&f.relay).sync_answer_deferral.in_flight_len(), 1);
        ask(&f.relay, peer(), 1, MAKER, &[PAIR], &answers_tx).unwrap();
        assert_eq!(octx(&f.relay).sync_answer_deferral.parked_len(), 1);

        drop(task);
        assert_placeholder_error(&answered(&answers, 2000).1);
        assert_tables_empty(&f.relay);
    }

    /// S2 of the wipe-recovery plan: a relay wiped for a maker gets the first keep-alive after the stall;
    /// its subscriber, which received the same keep-alive from the relay's gossip-layer forward, asks
    /// the relay while the relay's own request is in flight.
    #[test]
    fn s2_a_wiped_relay_serves_its_subscriber_when_its_own_sync_lands() {
        let mut f = Fixture::new(json!({}));

        // The relay held the maker's book, and the sweep wiped it after the stall.
        let mut own = f.start_own_sync();
        own.answer(&f.maker);
        assert!(own.finish());
        flush_trie(&f.relay);
        assert_eq!(maker_book(&f.relay), f.uuids);
        wipe_maker(&f.relay);
        assert!(maker_book(&f.relay).is_empty());
        assert_tables_empty(&f.relay);

        // The first keep-alive after the stall reaches the relay and, forwarded at once, its subscriber.
        let (subscriber, mut subscriber_cmds) = node(without_alternate_relay(json!({})));
        let started = std::time::Instant::now();
        let mut relay_own = f.start_own_sync();
        let keep_alive = f.keep_alive();
        let subscriber_task = {
            let subscriber = subscriber.clone();
            std::thread::spawn(move || {
                block_on(process_orders_keep_alive(
                    subscriber,
                    "relay".to_owned(),
                    MAKER.to_owned(),
                    keep_alive,
                    true,
                ))
            })
        };

        // The subscriber's request travels to the relay, whose own request is still in flight.
        let (request, response_tx) = match next_cmd(&mut subscriber_cmds) {
            AdexBehaviourCmd::RequestPeers { req, response_tx, .. } => (req, response_tx),
            other => panic!("unexpected command {:?}", other),
        };
        let (pubkey, trie_roots) = match decode_message::<P2PRequest>(&request).unwrap() {
            P2PRequest::Ordermatch(OrdermatchRequest::SyncPubkeyOrderbookState { pubkey, trie_roots }) => {
                (pubkey, trie_roots)
            },
            other => panic!("unexpected request {:?}", other),
        };
        let relay_peer = peer();
        let reply: SyncAnswerReply = Box::new(move |res| {
            response_tx
                .send(vec![(relay_peer, res)])
                .map_err(|_| "the subscriber is gone".to_owned())
        });
        answer_sync_request(&f.relay, peer(), pubkey, trie_roots, reply).unwrap();
        assert_eq!(octx(&f.relay).sync_answer_deferral.parked_len(), 1);
        assert!(maker_book(&subscriber).is_empty());

        // The relay's own answer arrives and lands; the subscriber is answered right after it.
        relay_own.answer(&f.maker);
        assert!(subscriber_task.join().unwrap(), "the subscriber's sync succeeded");
        flush_trie(&subscriber);
        assert!(started.elapsed() < Duration::from_millis(LONG_BOUND_MS / 2));
        assert!(relay_own.finish());
        assert_eq!(maker_book(&f.relay), f.uuids);
        assert_eq!(maker_book(&subscriber), f.uuids);
        assert_tables_empty(&f.relay);
        assert_tables_empty(&subscriber);
    }

    // ---- The logging of the synchronization (left to the implementation, CRD ch.32 R36). The tests read the
    // counters and the recorded events of the node, not the log text.

    fn assert_wire_form(res: &AdexResponse, needle: &str) {
        match res {
            AdexResponse::Err { error } => {
                assert!(error.starts_with("ordermatch_orderbook:"), "{}", error);
                assert!(error.contains(needle), "{}", error);
            },
            other => panic!("expected an error answer, got {:?}", other),
        }
    }

    impl OwnSync {
        /// The request ends with `res` as the transport's answer.
        fn respond(&mut self, res: AdexResponse) {
            self.response_tx
                .take()
                .unwrap()
                .send(vec![(PeerId::random(), res)])
                .unwrap();
        }
    }

    #[test]
    fn a_placeholder_answered_at_once_without_an_own_request_is_logged_as_such() {
        let f = Fixture::new(json!({}));
        let (answers_tx, answers) = std_mpsc::channel();
        add_placeholder(&f.relay, MAKER, PAIR);

        ask(&f.relay, peer(), 1, MAKER, &[PAIR], &answers_tx).unwrap();
        // Wire guard: the answer keeps the location prefix and the reason.
        assert_wire_form(&answers.try_recv().unwrap().1, "InvalidStateRoot");

        let log = &octx(&f.relay).sync_log;
        assert_eq!(log.answers(), vec![(
            AnswerClass::Placeholder,
            AnswerPath::AtOnce(AtOnce::OwnRequestNotInFlight)
        )]);
        let counters = log.snapshot();
        assert_eq!(counters.answers_placeholder, 1);
        assert_eq!(counters.placeholder_own_not_in_flight, 1);
        assert_eq!(counters.answers_parked, 0);
    }

    #[test]
    fn a_placeholder_parked_until_the_bound_is_logged_with_the_path_parked_bound() {
        let mut f = Fixture::new(json!({ "sync_answer_deferral_ms": 200 }));
        let (answers_tx, answers) = std_mpsc::channel();
        let mut own = f.start_own_sync();
        ask(&f.relay, peer(), 1, MAKER, &[PAIR], &answers_tx).unwrap();
        assert_wire_form(&answered(&answers, 3000).1, "InvalidStateRoot");

        let log = &octx(&f.relay).sync_log;
        assert_eq!(log.answers(), vec![(AnswerClass::Placeholder, AnswerPath::ParkedBound)]);
        let counters = log.snapshot();
        assert_eq!((counters.placeholder_parked_bound, counters.answers_parked), (1, 1));

        own.fail();
        assert!(!own.finish());
    }

    #[test]
    fn a_parked_request_woken_by_a_failed_or_an_answered_own_request_is_logged_as_woken() {
        let mut f = Fixture::new(no_retry());
        let (answers_tx, answers) = std_mpsc::channel();

        // The own request fails: the parked request gets the placeholder error.
        let mut own = f.start_own_sync();
        ask(&f.relay, peer(), 1, MAKER, &[PAIR], &answers_tx).unwrap();
        own.fail();
        assert_wire_form(&answered(&answers, 2000).1, "InvalidStateRoot");
        assert!(!own.finish());
        assert_tables_empty(&f.relay);

        // The next own request is answered: the parked request gets the full set.
        let mut own = f.start_own_sync();
        ask(&f.relay, peer(), 2, MAKER, &[PAIR], &answers_tx).unwrap();
        own.answer(&f.maker);
        assert_eq!(full_set_len(&answered(&answers, 2000).1), ORDERS);
        assert!(own.finish());

        let log = &octx(&f.relay).sync_log;
        assert_eq!(log.answers(), vec![
            (AnswerClass::Placeholder, AnswerPath::ParkedWoken),
            (AnswerClass::OkFull, AnswerPath::ParkedWoken),
        ]);
        let counters = log.snapshot();
        assert_eq!(counters.placeholder_parked_woken, 1);
        assert_eq!(counters.answers_ok_full, 1);
        assert_eq!(counters.answers_parked, 2);
    }

    #[test]
    fn a_missing_pair_no_data_and_a_full_set_are_classified_and_the_wire_form_is_kept() {
        let f = Fixture::new(json!({}));
        let (answers_tx, answers) = std_mpsc::channel();

        ask(&f.maker, peer(), 1, MAKER, &["C9:C8"], &answers_tx).unwrap();
        assert_wire_form(&answers.try_recv().unwrap().1, "No pair trie root for C9:C8");
        ask(&f.maker, peer(), 2, "03unknown", &[PAIR], &answers_tx).unwrap();
        assert_eq!(answers.try_recv().unwrap().1, AdexResponse::None);
        ask(&f.maker, peer(), 3, MAKER, &[PAIR], &answers_tx).unwrap();
        assert_eq!(full_set_len(&answers.try_recv().unwrap().1), ORDERS);

        let not_deferrable = AnswerPath::AtOnce(AtOnce::NotDeferrable);
        assert_eq!(octx(&f.maker).sync_log.answers(), vec![
            (AnswerClass::MissingPair, not_deferrable),
            (AnswerClass::NoData, not_deferrable),
            (AnswerClass::OkFull, not_deferrable),
        ]);
        let counters = octx(&f.maker).sync_log.snapshot();
        assert_eq!(
            (
                counters.answers_missing_pair,
                counters.answers_no_data,
                counters.answers_ok_full
            ),
            (1, 1, 1)
        );
    }

    #[test]
    fn a_delta_is_logged_as_ok_delta() {
        let f = Fixture::new(json!({}));
        let (answers_tx, answers) = std_mpsc::channel();
        let old_root = pair_trie_root_by_pub(&f.maker, MAKER, PAIR);
        for order in make_random_orders(MAKER.to_owned(), &[0; 32], "C1".into(), "C2".into(), 2) {
            insert_or_update_order(&f.maker, order);
        }
        flush_trie(&f.maker);

        let roots = HashMap::from([(PAIR.to_owned(), old_root)]);
        let reply: SyncAnswerReply = Box::new(move |res| answers_tx.send((1, res)).map_err(|e| e.to_string()));
        answer_sync_request(&f.maker, peer(), MAKER.to_owned(), roots, reply).unwrap();
        assert!(matches!(answers.try_recv().unwrap().1, AdexResponse::Ok { .. }));

        assert_eq!(octx(&f.maker).sync_log.answers(), vec![(
            AnswerClass::OkDelta,
            AnswerPath::AtOnce(AtOnce::NotDeferrable)
        )]);
        assert_eq!(octx(&f.maker).sync_log.snapshot().answers_ok_delta, 1);
    }

    #[test]
    fn a_full_parked_table_and_a_zero_bound_are_logged_with_their_paths() {
        let mut f = Fixture::new(small_limits(2, 3));
        let (answers_tx, answers) = std_mpsc::channel();
        let mut own = f.start_own_sync();
        let greedy = peer();
        for id in 0..2 {
            ask(&f.relay, greedy, id, MAKER, &[PAIR], &answers_tx).unwrap();
        }
        ask(&f.relay, greedy, 100, MAKER, &[PAIR], &answers_tx).unwrap();
        assert_placeholder_error(&answers.try_recv().expect("answered at once").1);
        assert_eq!(octx(&f.relay).sync_log.answers(), vec![(
            AnswerClass::Placeholder,
            AnswerPath::AtOnce(AtOnce::CapsFull)
        )]);
        assert_eq!(octx(&f.relay).sync_log.snapshot().placeholder_caps, 1);
        own.answer(&f.maker);
        assert!(own.finish());

        let f = Fixture::new(json!({ "sync_answer_deferral_ms": 0 }));
        let (answers_tx, answers) = std_mpsc::channel();
        add_placeholder(&f.relay, MAKER, PAIR);
        ask(&f.relay, peer(), 1, MAKER, &[PAIR], &answers_tx).unwrap();
        assert_placeholder_error(&answers.try_recv().unwrap().1);
        assert_eq!(octx(&f.relay).sync_log.answers(), vec![(
            AnswerClass::Placeholder,
            AnswerPath::AtOnce(AtOnce::DeferralOff)
        )]);
        assert_eq!(octx(&f.relay).sync_log.snapshot().placeholder_at_once, 1);
    }

    #[test]
    fn a_placeholder_alongside_a_tracked_pair_is_not_deferrable_and_says_which_reason() {
        let mut f = Fixture::new(no_retry());
        let (answers_tx, answers) = std_mpsc::channel();
        let mut own = f.start_own_sync();
        add_placeholder(&f.relay, MAKER, OTHER_PAIR);
        ask(&f.relay, peer(), 1, MAKER, &[PAIR, OTHER_PAIR], &answers_tx).unwrap();
        assert_placeholder_error(&answers.try_recv().unwrap().1);
        // A pair with no entry at all decides the reason first, whatever the order of the pairs.
        ask(&f.relay, peer(), 2, MAKER, &[PAIR, OTHER_PAIR, "C9:C8"], &answers_tx).unwrap();
        assert!(matches!(answers.try_recv().unwrap().1, AdexResponse::Err { .. }));
        // The class of the second answer depends on which of its failing pairs the trie read meets first, so
        // only the path is fixed.
        let answers = octx(&f.relay).sync_log.answers();
        assert_eq!(
            answers[0],
            (
                AnswerClass::Placeholder,
                AnswerPath::AtOnce(AtOnce::OwnRequestNotInFlight)
            )
        );
        assert_eq!(answers[1].1, AnswerPath::AtOnce(AtOnce::NotDeferrable));
        assert!(matches!(
            answers[1].0,
            AnswerClass::Placeholder | AnswerClass::MissingPair
        ));
        own.fail();
        assert!(!own.finish());
    }

    /// The first request ends as the transport reports, with no re-ask.
    fn request_class_of(res: Option<AdexResponse>) -> Vec<(Attempt, RequestClass)> {
        let mut f = Fixture::new(no_retry());
        let mut own = f.start_own_sync();
        match res {
            Some(res) => own.respond(res),
            None => own.fail(),
        }
        assert!(!own.finish());
        f.assert_no_further_request();
        octx(&f.relay).sync_log.requests()
    }

    #[test]
    fn every_outcome_of_the_first_request_has_its_class() {
        let peer_error = AdexResponse::Err {
            error: "the peer says no".to_owned(),
        };
        let local = |kind| AdexResponse::LocalErr {
            kind,
            error: "ResponseError".to_owned(),
        };
        let garbage = AdexResponse::Ok { response: vec![0xc1] };
        for (res, class) in [
            (Some(AdexResponse::None), RequestClass::NoData),
            (Some(peer_error), RequestClass::PeerError),
            (Some(local(LocalFailure::Timeout)), RequestClass::Timeout),
            (Some(local(LocalFailure::Transport)), RequestClass::Transport),
            (Some(garbage), RequestClass::Decode),
            // The command is dropped without an answer.
            (None, RequestClass::Transport),
        ] {
            assert_eq!(request_class_of(res), vec![(Attempt::First, class)], "{:?}", class);
        }
    }

    #[test]
    fn an_answered_request_is_ok_and_the_re_ask_is_attempt_two() {
        // Answered at once.
        let mut f = Fixture::new(quick_retry());
        let mut own = f.start_own_sync();
        own.answer(&f.maker);
        assert!(own.finish());
        assert_eq!(octx(&f.relay).sync_log.requests(), vec![(
            Attempt::First,
            RequestClass::Ok
        )]);

        // A quick failure, then the answer to the second request.
        let mut f = Fixture::new(quick_retry());
        let mut own = f.start_own_sync();
        own.respond(AdexResponse::LocalErr {
            kind: LocalFailure::Timeout,
            error: "Timeout".to_owned(),
        });
        f.expect_second_request(&mut own);
        own.answer(&f.maker);
        assert!(own.finish());
        assert_eq!(octx(&f.relay).sync_log.requests(), vec![
            (Attempt::First, RequestClass::Timeout),
            (Attempt::Second, RequestClass::Ok)
        ]);
        assert_eq!(octx(&f.relay).sync_log.snapshot().requests_ok, 1);

        // Both fail, each in its own way.
        let mut f = Fixture::new(quick_retry());
        let mut own = f.start_own_sync();
        own.respond(AdexResponse::Err {
            error: "busy".to_owned(),
        });
        f.expect_second_request(&mut own);
        own.respond(AdexResponse::LocalErr {
            kind: LocalFailure::Transport,
            error: "ConnectionClosed".to_owned(),
        });
        assert!(!own.finish());
        assert_eq!(octx(&f.relay).sync_log.requests(), vec![
            (Attempt::First, RequestClass::PeerError),
            (Attempt::Second, RequestClass::Transport)
        ]);
        let counters = octx(&f.relay).sync_log.snapshot();
        assert_eq!((counters.requests_peer_error, counters.requests_transport), (1, 1));
        assert_tables_empty(&f.relay);
    }

    /// CRD ch.32 R-Q5d, T-Q5d: one other relay is asked once when the forwarder's answer fails or leaves the
    /// pair diverged.
    mod alternate_relay_tests {
        use super::*;

        const FORWARDER: &str = "forwarding-peer";
        const G: &str = "relay-g";

        /// The relay with the request to another relay switched on and no second request to the forwarder.
        fn alt_conf() -> Json { json!({ "sync_request_alternate_relay": true, "sync_request_retry_ms": 0 }) }

        /// A request that is held until the test decides how it ends.
        struct HeldRequest {
            request: Vec<u8>,
            response_tx: Option<OwnResponseTx>,
        }

        impl HeldRequest {
            fn roots(&self) -> HashMap<String, H64> { requested_roots(&self.request) }

            /// The request is answered by `node`.
            fn answer(&mut self, node: &MmArc) {
                let (pubkey, trie_roots) = match decode_message::<P2PRequest>(&self.request).unwrap() {
                    P2PRequest::Ordermatch(OrdermatchRequest::SyncPubkeyOrderbookState { pubkey, trie_roots }) => {
                        (pubkey, trie_roots)
                    },
                    other => panic!("unexpected request {:?}", other),
                };
                let answer = process_sync_pubkey_orderbook_state(node.clone(), pubkey, trie_roots)
                    .unwrap()
                    .unwrap();
                let res = AdexResponse::Ok {
                    response: encode_message(&answer).unwrap(),
                };
                self.response_tx
                    .take()
                    .unwrap()
                    .send(vec![(PeerId::random(), res)])
                    .unwrap();
            }
        }

        impl Fixture {
            /// The node asks for its relays; `relays` is the answer.
            fn answer_relay_list(&mut self, relays: &[&str]) {
                match next_cmd(&mut self.relay_cmds) {
                    AdexBehaviourCmd::GetConnectedRelays { result_tx } => {
                        result_tx.send(relays.iter().map(|r| r.to_string()).collect()).unwrap()
                    },
                    other => panic!("unexpected command {:?}", other),
                }
            }

            /// The node asks for its relays and the reply channel is dropped.
            fn drop_relay_list_reply(&mut self) {
                match next_cmd(&mut self.relay_cmds) {
                    AdexBehaviourCmd::GetConnectedRelays { result_tx } => drop(result_tx),
                    other => panic!("unexpected command {:?}", other),
                }
            }

            /// The request to one other relay, which must be addressed to `peer` alone.
            fn expect_alternate_request(&mut self, peer: &str) -> HeldRequest {
                match next_cmd(&mut self.relay_cmds) {
                    AdexBehaviourCmd::RequestPeers {
                        req,
                        peers,
                        response_tx,
                    } => {
                        assert_eq!(peers, vec![peer.to_owned()]);
                        HeldRequest {
                            request: req,
                            response_tx: Some(response_tx),
                        }
                    },
                    other => panic!("unexpected command {:?}", other),
                }
            }

            /// No command at all is waiting (to be called once the tasks of the keep-alive have settled).
            fn assert_no_command(&mut self) {
                if let Ok(cmd) = self.relay_cmds.try_recv() {
                    panic!("unexpected command {:?}", cmd);
                }
            }

            fn advertised(&self, pair: &str) -> H64 { self.keep_alive().trie_roots[pair] }
        }

        fn wait_until(what: &str, cond: impl Fn() -> bool) {
            // Generous: the late worker is noticed after 5 s.
            let deadline = Instant::now() + Duration::from_secs(15);
            while !cond() {
                assert!(Instant::now() < deadline, "timed out waiting for {}", what);
                std::thread::sleep(Duration::from_millis(2));
            }
        }

        /// The root the trie of `ctx` holds for the maker's `pair`.
        fn held(ctx: &MmArc, pair: &str) -> Option<H64> {
            octx(ctx)
                .trie_store
                .lock()
                .pubkeys_state
                .get(MAKER)
                .and_then(|s| s.trie_roots.get(pair).copied())
        }

        /// A node holding the maker's orders but those of `skipped`.
        fn node_without(f: &Fixture, skipped: &HashSet<Uuid>) -> MmArc {
            let (node, _cmds) = node(json!({}));
            let items: Vec<OrderbookItem> = octx(&f.maker).orderbook.lock().order_set.values().cloned().collect();
            for item in items.into_iter().filter(|i| !skipped.contains(&i.uuid)) {
                insert_or_update_order(&node, item);
            }
            flush_trie(&node);
            node
        }

        fn one_of(f: &Fixture, last: bool) -> HashSet<Uuid> {
            let mut uuids: Vec<Uuid> = f.uuids.iter().copied().collect();
            uuids.sort();
            HashSet::from([if last { uuids[uuids.len() - 1] } else { uuids[0] }])
        }

        fn alt_counters(ctx: &MmArc) -> [u32; 6] {
            let c = octx(ctx).sync_log.snapshot();
            [
                c.alt_failure,
                c.alt_mismatch,
                c.alt_no_relay,
                c.alt_worker_late,
                c.alt_sent,
                c.alt_ok,
            ]
        }

        #[test]
        fn a_timeout_at_the_forwarder_is_followed_by_one_request_to_another_relay() {
            let mut f = Fixture::new(alt_conf());
            let mut own = f.start_own_sync();
            let first_roots = requested_roots(&own.request);
            own.respond(AdexResponse::LocalErr {
                kind: LocalFailure::Timeout,
                error: "Timeout".to_owned(),
            });
            // The result of the keep-alive's handling does not wait for the request to the other relay.
            assert!(!own.finish());

            f.answer_relay_list(&[FORWARDER, G]);
            let mut alt = f.expect_alternate_request(G);
            assert_eq!(
                alt.roots(),
                first_roots,
                "the pairs of the request, with the held roots"
            );
            alt.answer(&f.maker);

            wait_until("the answer of the other relay", || maker_book(&f.relay) == f.uuids);
            assert_tables_empty(&f.relay);
            wait_until("the root", || held(&f.relay, PAIR) == Some(f.advertised(PAIR)));
            f.assert_no_command();
            assert_eq!(octx(&f.relay).sync_log.requests(), vec![
                (Attempt::First, RequestClass::Timeout),
                (Attempt::Alternate(AltReason::Failure), RequestClass::Ok)
            ]);
            assert_eq!(alt_counters(&f.relay), [1, 0, 0, 0, 1, 1]);
            // The request to the other relay is counted among the requests as any request is.
            assert_eq!(octx(&f.relay).sync_log.snapshot().requests_timeout, 1);
            assert_eq!(octx(&f.relay).sync_log.snapshot().requests_ok, 1);
        }

        /// With `orderbook_sync_names_held_root` off, the request names the advertised roots.
        #[test]
        fn the_request_to_the_other_relay_names_the_advertised_root_when_configured() {
            let mut f = Fixture::new(json!({
                "sync_request_alternate_relay": true,
                "sync_request_retry_ms": 0,
                "orderbook_sync_names_held_root": false
            }));
            let mut own = f.start_own_sync();
            let advertised = HashMap::from([(PAIR.to_owned(), f.advertised(PAIR))]);
            assert_eq!(requested_roots(&own.request), advertised);
            own.fail();
            assert!(!own.finish());
            f.answer_relay_list(&[G, FORWARDER]);
            let mut alt = f.expect_alternate_request(G);
            assert_eq!(alt.roots(), advertised);
            alt.answer(&f.maker);
            wait_until("the answer of the other relay", || maker_book(&f.relay) == f.uuids);
            assert_tables_empty(&f.relay);
        }

        #[test]
        fn the_other_relay_is_asked_after_the_second_failure_not_the_first() {
            let mut f =
                Fixture::new(json!({ "sync_request_alternate_relay": true, "sync_request_retry_ms": RETRY_MS }));
            let mut own = f.start_own_sync();
            let first_roots = requested_roots(&own.request);
            own.respond(AdexResponse::Err {
                error: "busy".to_owned(),
            });
            // The next command is the second request to the forwarder, not the relay list.
            f.expect_second_request(&mut own);
            own.respond(AdexResponse::LocalErr {
                kind: LocalFailure::Transport,
                error: "ConnectionClosed".to_owned(),
            });
            assert!(!own.finish());

            f.answer_relay_list(&[FORWARDER, G]);
            let mut alt = f.expect_alternate_request(G);
            assert_eq!(alt.roots(), first_roots);
            alt.answer(&f.maker);
            wait_until("the answer of the other relay", || maker_book(&f.relay) == f.uuids);
            assert_tables_empty(&f.relay);
            f.assert_no_command();
            assert_eq!(octx(&f.relay).sync_log.requests(), vec![
                (Attempt::First, RequestClass::PeerError),
                (Attempt::Second, RequestClass::Transport),
                (Attempt::Alternate(AltReason::Failure), RequestClass::Ok)
            ]);
        }

        #[test]
        fn a_diverged_pair_is_asked_from_another_relay_and_never_cleared() {
            let mut f = Fixture::new(alt_conf());
            let stale_f = node_without(&f, &one_of(&f, false));
            let stale_g = node_without(&f, &one_of(&f, true));
            let advertised = f.advertised(PAIR);
            let f_root = held(&stale_f, PAIR).unwrap();
            assert_ne!(f_root, advertised);
            assert_ne!(held(&stale_g, PAIR).unwrap(), advertised);

            let mut own = f.start_own_sync();
            own.answer(&stale_f);
            assert!(own.finish(), "F's answer was applied");
            wait_until("F's set", || maker_book(&f.relay) == maker_book(&stale_f));
            wait_until("F's root", || held(&f.relay, PAIR) == Some(f_root));

            f.answer_relay_list(&[G, FORWARDER]);
            let mut alt = f.expect_alternate_request(G);
            assert_eq!(
                alt.roots(),
                HashMap::from([(PAIR.to_owned(), f_root)]),
                "only the pair, at the held root"
            );
            // F's set stays until G's answer is applied.
            assert_eq!(maker_book(&f.relay), maker_book(&stale_f));
            assert_eq!(held(&f.relay, PAIR), Some(f_root));

            // G's answer is applied although the root it reaches differs from the advertised one too.
            alt.answer(&stale_g);
            wait_until("G's set", || maker_book(&f.relay) == maker_book(&stale_g));
            wait_until("G's root", || held(&f.relay, PAIR) == held(&stale_g, PAIR));
            assert_ne!(held(&f.relay, PAIR), Some(advertised));
            assert_tables_empty(&f.relay);
            std::thread::sleep(Duration::from_millis(100));
            f.assert_no_command();
            assert_eq!(octx(&f.relay).sync_log.requests(), vec![
                (Attempt::First, RequestClass::Ok),
                (Attempt::Alternate(AltReason::Mismatch), RequestClass::Ok)
            ]);
            assert_eq!(alt_counters(&f.relay), [0, 1, 0, 0, 1, 1]);
        }

        /// The DEBUG tail of the mismatch line (the watch table): a diverged pair with no candidate rule that
        /// would have skipped it. The request to the other relay is sent as without the watch.
        #[test]
        fn the_mismatch_line_counts_no_flag_for_a_plainly_diverged_pair() {
            let mut f = Fixture::new(alt_conf());
            octx(&f.relay).mismatch_watch.force_debug();
            let stale_f = node_without(&f, &one_of(&f, false));
            let f_root = held(&stale_f, PAIR).unwrap();

            let mut own = f.start_own_sync();
            assert_eq!(octx(&f.relay).mismatch_watch.len(), 1, "the watch of the request");
            own.answer(&stale_f);
            assert!(own.finish(), "F's answer was applied");
            wait_until("F's root", || held(&f.relay, PAIR) == Some(f_root));

            f.answer_relay_list(&[G, FORWARDER]);
            let mut alt = f.expect_alternate_request(G);
            assert_eq!(alt.roots(), HashMap::from([(PAIR.to_owned(), f_root)]));
            alt.answer(&f.maker);
            wait_until("G's set", || maker_book(&f.relay) == f.uuids);
            assert_tables_empty(&f.relay);
            f.assert_no_command();

            let mut skips = octx(&f.relay).sync_log.skips();
            assert_eq!(skips.len(), 1);
            let since_keep_alive_ms = std::mem::take(&mut skips[0].since_keep_alive_ms);
            assert!(since_keep_alive_ms < 15_000, "{}", since_keep_alive_ms);
            assert_eq!(skips, vec![SkipCounts {
                pairs: 1,
                ..Default::default()
            }]);
            assert_eq!(alt_counters(&f.relay), [0, 1, 0, 0, 1, 1]);
        }

        /// An order message of the maker for the pair, handled after the keep-alive and before the check, is
        /// candidate (b): the line says so, and the request to the other relay is still sent.
        #[test]
        fn the_mismatch_line_counts_an_order_message_handled_after_the_keep_alive() {
            let mut f = Fixture::new(alt_conf());
            octx(&f.relay).mismatch_watch.force_debug();
            let stale_f = node_without(&f, &one_of(&f, false));
            let f_root = held(&stale_f, PAIR).unwrap();

            let mut own = f.start_own_sync();
            // The maker's created message reaches the relay while the request is in flight; the answer, which is
            // no order message, then replaces the pair.
            let extra = make_random_orders(MAKER.to_owned(), &[0; 32], "C1".into(), "C2".into(), 1).remove(0);
            insert_or_update_order(&f.relay, extra);
            own.answer(&stale_f);
            assert!(own.finish(), "F's answer was applied");
            wait_until("F's set", || maker_book(&f.relay) == maker_book(&stale_f));
            wait_until("F's root", || held(&f.relay, PAIR) == Some(f_root));

            f.answer_relay_list(&[G, FORWARDER]);
            let mut alt = f.expect_alternate_request(G);
            assert_eq!(
                alt.roots(),
                HashMap::from([(PAIR.to_owned(), f_root)]),
                "the request is sent as without the watch"
            );
            alt.answer(&f.maker);
            wait_until("G's set", || maker_book(&f.relay) == f.uuids);
            assert_tables_empty(&f.relay);
            f.assert_no_command();

            let mut skips = octx(&f.relay).sync_log.skips();
            assert_eq!(skips.len(), 1);
            let since_keep_alive_ms = std::mem::take(&mut skips[0].since_keep_alive_ms);
            assert!(since_keep_alive_ms < 15_000, "{}", since_keep_alive_ms);
            assert_eq!(skips, vec![SkipCounts {
                pairs: 1,
                skip_b: 1,
                skip_ab: 1,
                ..Default::default()
            }]);
            assert_eq!(alt_counters(&f.relay), [0, 1, 0, 0, 1, 1]);
        }

        #[test]
        fn a_set_that_reaches_the_advertised_root_asks_no_other_relay() {
            let mut f = Fixture::new(alt_conf());
            let mut own = f.start_own_sync();
            own.answer(&f.maker);
            assert!(own.finish());
            let advertised = f.advertised(PAIR);
            wait_until("the root", || held(&f.relay, PAIR) == Some(advertised));
            assert_tables_empty(&f.relay);
            std::thread::sleep(Duration::from_millis(200));
            f.assert_no_command();
            assert_eq!(alt_counters(&f.relay), [0; 6]);
        }

        #[test]
        fn only_the_diverged_pair_of_two_is_asked_from_another_relay() {
            let mut f = Fixture::new(alt_conf());
            let other = make_random_orders(MAKER.to_owned(), &[0; 32], "C3".into(), "C4".into(), 3);
            let other_uuids: HashSet<Uuid> = other.iter().map(|o| o.uuid).collect();
            for order in other {
                insert_or_update_order(&f.maker, order);
            }
            flush_trie(&f.maker);
            // F misses one order of the second pair only.
            let skipped = HashSet::from([*other_uuids.iter().next().unwrap()]);
            let stale_f = node_without(&f, &skipped);
            let (advertised, other_advertised) = (f.advertised(PAIR), f.advertised(OTHER_PAIR));
            let f_other_root = held(&stale_f, OTHER_PAIR).unwrap();
            assert_ne!(f_other_root, other_advertised);
            assert_eq!(held(&stale_f, PAIR), Some(advertised));

            let mut own = f.start_own_sync();
            assert_eq!(requested_roots(&own.request).len(), 2);
            own.answer(&stale_f);
            assert!(own.finish());

            f.answer_relay_list(&[FORWARDER, G]);
            let mut alt = f.expect_alternate_request(G);
            assert_eq!(alt.roots(), HashMap::from([(OTHER_PAIR.to_owned(), f_other_root)]));
            alt.answer(&f.maker);
            let all_uuids: HashSet<Uuid> = f.uuids.union(&other_uuids).copied().collect();
            wait_until("G's set", || maker_book(&f.relay) == all_uuids);
            wait_until("G's root", || held(&f.relay, OTHER_PAIR) == Some(other_advertised));
            assert_tables_empty(&f.relay);
            f.assert_no_command();
        }

        /// The relay's worker never applies the answer: no request follows after 5 s.
        #[test]
        fn a_worker_that_is_late_by_five_seconds_asks_no_other_relay() {
            let mut f = Fixture::new(alt_conf());
            let (workerless, _held_back) = ordermatch_ctx_without_worker();
            let workerless = Arc::try_unwrap(workerless).ok().expect("the only reference");
            mm2_core::mm_ctx::from_ctx(&f.relay.ordermatch_ctx, move || Ok(workerless)).unwrap();

            let mut own = f.start_own_sync();
            own.answer(&f.maker);
            assert!(own.finish());
            assert!(
                maker_book(&f.relay) == f.uuids,
                "the index took the answer, the trie did not"
            );
            wait_until("the late worker to be noticed", || {
                octx(&f.relay).sync_log.snapshot().alt_worker_late == 1
            });
            assert_tables_empty(&f.relay);
            f.assert_no_command();
            assert_eq!(alt_counters(&f.relay), [0, 0, 0, 1, 0, 0]);
        }

        #[test]
        fn nothing_beyond_the_second_request_without_another_relay_or_with_the_switch_off() {
            // Only the forwarder is connected.
            let mut f = Fixture::new(alt_conf());
            let mut own = f.start_own_sync();
            own.fail();
            assert!(!own.finish());
            f.answer_relay_list(&[FORWARDER]);
            assert_tables_empty(&f.relay);
            f.assert_no_command();
            assert_eq!(alt_counters(&f.relay), [1, 0, 1, 0, 0, 0]);

            // The reply to the question is dropped: no relay is known.
            let mut f = Fixture::new(alt_conf());
            let mut own = f.start_own_sync();
            own.fail();
            assert!(!own.finish());
            f.drop_relay_list_reply();
            assert_tables_empty(&f.relay);
            f.assert_no_command();
            assert_eq!(alt_counters(&f.relay), [1, 0, 1, 0, 0, 0]);

            // The switch is off: the relays are not even asked for.
            let mut f = Fixture::new(json!({ "sync_request_alternate_relay": false, "sync_request_retry_ms": 0 }));
            let mut own = f.start_own_sync();
            own.fail();
            assert!(!own.finish());
            assert_tables_empty(&f.relay);
            f.assert_no_command();
            assert_eq!(alt_counters(&f.relay), [0; 6]);

            // A divergence with the switch off asks nobody either.
            let mut f = Fixture::new(json!({ "sync_request_alternate_relay": false }));
            let stale = node_without(&f, &one_of(&f, false));
            let mut own = f.start_own_sync();
            own.answer(&stale);
            assert!(own.finish());
            wait_until("the stale set", || maker_book(&f.relay) == maker_book(&stale));
            assert_tables_empty(&f.relay);
            std::thread::sleep(Duration::from_millis(100));
            f.assert_no_command();
        }

        #[test]
        fn a_relay_list_without_another_relay_after_a_divergence_sends_nothing() {
            let mut f = Fixture::new(alt_conf());
            let stale = node_without(&f, &one_of(&f, false));
            let mut own = f.start_own_sync();
            own.answer(&stale);
            assert!(own.finish());
            f.answer_relay_list(&[FORWARDER]);
            wait_until("the missing relay to be counted", || {
                octx(&f.relay).sync_log.snapshot().alt_no_relay == 1
            });
            assert_tables_empty(&f.relay);
            f.assert_no_command();
            assert_eq!(alt_counters(&f.relay), [0, 1, 1, 0, 0, 0]);
        }

        #[test]
        fn the_keep_alive_returns_before_the_request_to_the_other_relay_ends() {
            let mut f = Fixture::new(alt_conf());
            let mut own = f.start_own_sync();
            own.fail();
            f.answer_relay_list(&[FORWARDER, G]);
            let mut alt = f.expect_alternate_request(G);
            // The request to G is held open, and the keep-alive's task is done.
            wait_until("the keep-alive task to end", || own.keep_alive_task.is_finished());
            assert!(!own.finish());
            // The record of the own request lasts through the request to G.
            assert_eq!(octx(&f.relay).sync_answer_deferral.in_flight_len(), 1);
            alt.answer(&f.maker);
            wait_until("the answer of the other relay", || maker_book(&f.relay) == f.uuids);
            assert_tables_empty(&f.relay);
        }

        #[test]
        fn a_parked_request_is_answered_with_the_set_of_the_other_relay() {
            let mut f =
                Fixture::new(json!({ "sync_request_alternate_relay": true, "sync_request_retry_ms": RETRY_MS }));
            let (answers_tx, answers) = std_mpsc::channel();
            let mut own = f.start_own_sync();
            ask(&f.relay, peer(), 1, MAKER, &[PAIR], &answers_tx).unwrap();
            assert_eq!(octx(&f.relay).sync_answer_deferral.parked_len(), 1);

            own.respond(AdexResponse::Err {
                error: "busy".to_owned(),
            });
            f.expect_second_request(&mut own);
            own.respond(AdexResponse::Err {
                error: "busy".to_owned(),
            });
            assert!(!own.finish());
            // The parked request keeps waiting: the record moved to the request to the other relay.
            assert!(answers.try_recv().is_err());
            assert_eq!(octx(&f.relay).sync_answer_deferral.parked_len(), 1);
            assert_eq!(octx(&f.relay).sync_answer_deferral.in_flight_len(), 1);

            f.answer_relay_list(&[FORWARDER, G]);
            let mut alt = f.expect_alternate_request(G);
            alt.answer(&f.maker);
            let (id, res) = answered(&answers, 2000);
            assert_eq!(id, 1);
            assert_eq!(full_set_len(&res), ORDERS);
            assert_tables_empty(&f.relay);
        }
    }

    // ---- CRD ch.32 R-Q5b item 6 (T-Q5b6): a request that arrives before the placeholder exists.

    /// The trie worker lags: one batch counts as queued that the worker has not applied, so the handling of a
    /// keep-alive waits for it at its start (R-T3b item 4). Call it once per node, and release it with
    /// [`release_worker`].
    fn hold_worker(ctx: &MmArc) { octx(ctx).orderbook.lock().trie_ops_enqueued += 1; }

    /// The worker applies the batch that [`hold_worker`] counted.
    fn release_worker(ctx: &MmArc) { octx(ctx).trie_store.lock().applied_batches += 1; }

    /// A root that is neither the zero root nor the empty-trie root.
    fn some_root() -> H64 { H64::from([7; 8]) }

    fn keep_alive_naming(roots: &[(&str, H64)]) -> PubkeyKeepAlive {
        PubkeyKeepAlive {
            trie_roots: roots.iter().map(|(pair, root)| (pair.to_string(), *root)).collect(),
            timestamp: now_ms() / 1000,
        }
    }

    type Handling = Pin<Box<dyn Future<Output = bool> + Send>>;

    /// The handling of a keep-alive of `pubkey` received from `peer`, polled until it waits for the trie worker
    /// and then left there: the test decides when it ends, by dropping it.
    fn start_handling(
        relay: &MmArc,
        peer: &str,
        pubkey: &str,
        keep_alive: PubkeyKeepAlive,
        i_am_relay: bool,
    ) -> Handling {
        let mut task: Handling = Box::pin(process_orders_keep_alive(
            relay.clone(),
            peer.to_owned(),
            pubkey.to_owned(),
            keep_alive,
            i_am_relay,
        ));
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        assert!(
            task.as_mut().poll(&mut cx).is_pending(),
            "the handling should wait for the worker"
        );
        task
    }

    /// Lets a handling that was left waiting by [`start_handling`] run to its end on a thread of its own.
    fn finish_handling(task: Handling) -> JoinHandle<bool> { std::thread::spawn(move || block_on(task)) }

    fn assert_missing_pair(res: &AdexResponse) {
        match res {
            AdexResponse::Err { error } => assert!(error.contains("No pair trie root"), "{}", error),
            other => panic!("expected the missing-pair failure, got {:?}", other),
        }
    }

    /// The request is parked behind a registration, the worker is released, the relay sends its own request
    /// and the parked request is answered with the full set once that answer is applied.
    fn assert_parked_before_the_placeholder_then_served(f: &mut Fixture) {
        let (answers_tx, answers) = std_mpsc::channel();
        hold_worker(&f.relay);
        // Polled once on this thread: the registration exists and the handling waits for the worker. It cannot
        // go on before it is polled again, so nothing below depends on how fast this thread runs.
        let task = start_handling(&f.relay, "forwarding-peer", MAKER, f.keep_alive(), true);
        assert_eq!(octx(&f.relay).sync_answer_deferral.registrations_len(), 1);

        let started = Instant::now();
        ask(&f.relay, peer(), 1, MAKER, &[PAIR], &answers_tx).unwrap();
        assert!(
            started.elapsed() < Duration::from_millis(LONG_BOUND_MS / 10),
            "the inbound handler waited"
        );
        assert!(answers.try_recv().is_err(), "the request is parked, not answered");
        assert_eq!(octx(&f.relay).sync_answer_deferral.parked_len(), 1);
        assert_eq!(octx(&f.relay).sync_answer_deferral.in_flight_len(), 0);

        release_worker(&f.relay);
        let task = finish_handling(task);
        let mut own = match next_cmd(&mut f.relay_cmds) {
            AdexBehaviourCmd::RequestPeers { req, response_tx, .. } => OwnSync {
                keep_alive_task: task,
                request: req,
                response_tx: Some(response_tx),
            },
            other => panic!("unexpected command {:?}", other),
        };
        // The registration has ended and the own request is in flight: the request waits for that one.
        assert!(answers.recv_timeout(Duration::from_millis(100)).is_err());
        assert_eq!(octx(&f.relay).sync_answer_deferral.parked_len(), 1);
        assert_eq!(octx(&f.relay).sync_answer_deferral.in_flight_len(), 1);

        own.answer(&f.maker);
        let (id, res) = answered(&answers, 2000);
        assert_eq!(id, 1);
        assert_eq!(full_set_len(&res), ORDERS);
        assert!(own.finish());
        assert_eq!(maker_book(&f.relay), f.uuids);
        assert_tables_empty(&f.relay);

        let log = &octx(&f.relay).sync_log;
        assert_eq!(log.answers(), vec![(AnswerClass::OkFull, AnswerPath::PreParkedWoken)]);
        let counters = log.snapshot();
        assert_eq!(
            (
                counters.pre_parked_woken,
                counters.pre_parked_bound,
                counters.answers_parked,
                counters.registrations_capped
            ),
            (1, 0, 1, 0)
        );
    }

    #[test]
    fn a_request_for_a_pubkey_never_seen_is_parked_until_the_own_sync_lands() {
        let mut f = Fixture::new(json!({}));
        assert_parked_before_the_placeholder_then_served(&mut f);
    }

    #[test]
    fn a_request_for_a_known_pubkey_with_the_pair_missing_is_parked_until_the_own_sync_lands() {
        let mut f = Fixture::new(json!({}));
        add_placeholder(&f.relay, MAKER, OTHER_PAIR);
        assert_parked_before_the_placeholder_then_served(&mut f);
    }

    #[test]
    fn a_request_for_a_pair_held_at_the_advertised_root_is_answered_when_the_registration_ends() {
        let mut f = Fixture::new(json!({}));
        let mut own = f.start_own_sync();
        own.answer(&f.maker);
        assert!(own.finish());
        flush_trie(&f.relay);
        assert_tables_empty(&f.relay);

        // The same keep-alive again: the comparison finds the root held and sends no request.
        let (answers_tx, answers) = std_mpsc::channel();
        hold_worker(&f.relay);
        let task = start_handling(&f.relay, "forwarding-peer", MAKER, f.keep_alive(), true);
        assert_eq!(octx(&f.relay).sync_answer_deferral.registrations_len(), 1);
        ask(&f.relay, peer(), 1, MAKER, &[PAIR], &answers_tx).unwrap();
        assert!(answers.try_recv().is_err(), "parked behind the registration");
        assert_eq!(octx(&f.relay).sync_answer_deferral.parked_len(), 1);

        release_worker(&f.relay);
        let task = finish_handling(task);
        assert_eq!(full_set_len(&answered(&answers, 2000).1), ORDERS);
        assert!(task.join().unwrap(), "the keep-alive is forwarded");
        f.assert_no_further_request();
        assert_tables_empty(&f.relay);
        assert_eq!(octx(&f.relay).sync_log.answers(), vec![(
            AnswerClass::OkFull,
            AnswerPath::PreParkedWoken
        )]);
    }

    #[test]
    fn a_dropped_handling_answers_the_parked_request_at_once_and_a_later_request_too() {
        let f = Fixture::new(json!({}));
        let (answers_tx, answers) = std_mpsc::channel();
        hold_worker(&f.relay);
        let task = start_handling(&f.relay, "forwarding-peer", MAKER, f.keep_alive(), true);
        assert_eq!(octx(&f.relay).sync_answer_deferral.registrations_len(), 1);
        ask(&f.relay, peer(), 1, MAKER, &[PAIR], &answers_tx).unwrap();
        assert!(answers.try_recv().is_err());

        drop(task);
        let (_, res) = answered(&answers, 2000);
        assert_missing_pair(&res);
        ask(&f.relay, peer(), 2, MAKER, &[PAIR], &answers_tx).unwrap();
        assert_missing_pair(&answers.try_recv().expect("answered at once").1);

        release_worker(&f.relay);
        assert_tables_empty(&f.relay);
        assert_eq!(octx(&f.relay).sync_log.answers(), vec![
            (AnswerClass::MissingPair, AnswerPath::PreParkedWoken),
            (AnswerClass::MissingPair, AnswerPath::AtOnce(AtOnce::NotDeferrable)),
        ]);
    }

    #[test]
    fn a_handling_blocked_past_the_bound_does_not_hold_the_request_beyond_it() {
        const BOUND_MS: u64 = 1000;
        let f = Fixture::new(json!({ "sync_answer_deferral_ms": BOUND_MS }));
        let (answers_tx, answers) = std_mpsc::channel();
        hold_worker(&f.relay);
        let started = Instant::now();
        // Polled once and never again: the handling stays where it is, past any bound.
        let task = start_handling(&f.relay, "forwarding-peer", MAKER, f.keep_alive(), true);
        ask(&f.relay, peer(), 1, MAKER, &[PAIR], &answers_tx).unwrap();
        assert!(answers.try_recv().is_err());

        // Not answered before the registration expires, and not later than the request's own bound.
        assert!(answers.recv_timeout(Duration::from_millis(BOUND_MS - 400)).is_err());
        let (_, res) = answered(&answers, 3000);
        let waited = started.elapsed();
        assert!(
            waited >= Duration::from_millis(BOUND_MS - 50),
            "answered after {:?}",
            waited
        );
        assert!(
            waited < Duration::from_millis(BOUND_MS * 5),
            "answered after {:?}",
            waited
        );
        assert_missing_pair(&res);
        assert_eq!(octx(&f.relay).sync_answer_deferral.parked_len(), 0);

        // The handling has not ended, but its registration is older than the bound: it counts as ended.
        assert_eq!(octx(&f.relay).sync_answer_deferral.registrations_len(), 1);
        ask(&f.relay, peer(), 2, MAKER, &[PAIR], &answers_tx).unwrap();
        assert_missing_pair(&answers.try_recv().expect("answered at once").1);

        drop(task);
        release_worker(&f.relay);
        assert_tables_empty(&f.relay);
        let log = &octx(&f.relay).sync_log;
        assert_eq!(log.answers(), vec![
            (AnswerClass::MissingPair, AnswerPath::PreParkedBound),
            (AnswerClass::MissingPair, AnswerPath::AtOnce(AtOnce::NotDeferrable)),
        ]);
        assert_eq!(log.snapshot().pre_parked_bound, 1);
    }

    #[test]
    fn a_pair_named_with_the_zero_or_the_empty_trie_root_is_not_registered() {
        for root in [H64::default(), hashed_null_node::<Layout>()] {
            let f = Fixture::new(json!({}));
            let (answers_tx, answers) = std_mpsc::channel();
            hold_worker(&f.relay);
            let task = start_handling(
                &f.relay,
                "forwarding-peer",
                MAKER,
                keep_alive_naming(&[(PAIR, root)]),
                true,
            );
            assert_eq!(octx(&f.relay).sync_answer_deferral.registrations_len(), 0);
            ask(&f.relay, peer(), 1, MAKER, &[PAIR], &answers_tx).unwrap();
            assert_missing_pair(&answers.try_recv().expect("answered at once").1);
            assert_eq!(octx(&f.relay).sync_answer_deferral.parked_len(), 0);

            drop(task);
            release_worker(&f.relay);
            assert_tables_empty(&f.relay);
        }
    }

    #[test]
    fn a_pubkey_no_keep_alive_names_and_a_relay_s_own_pubkey_are_answered_at_once() {
        let f = Fixture::new(json!({}));
        let (answers_tx, answers) = std_mpsc::channel();
        hold_worker(&f.relay);
        let task = start_handling(&f.relay, "forwarding-peer", MAKER, f.keep_alive(), true);
        assert_eq!(octx(&f.relay).sync_answer_deferral.registrations_len(), 1);

        // Another pubkey, which no keep-alive being handled names.
        ask(&f.relay, peer(), 1, "03unknown", &[PAIR], &answers_tx).unwrap();
        assert_eq!(answers.try_recv().expect("answered at once").1, AdexResponse::None);
        assert_eq!(octx(&f.relay).sync_answer_deferral.parked_len(), 0);
        drop(task);
        release_worker(&f.relay);
        assert_tables_empty(&f.relay);

        // One of the relay's own pubkeys: nothing is registered for it.
        octx(&f.relay).orderbook.lock().my_p2p_pubkeys.insert(MAKER.to_owned());
        hold_worker(&f.relay);
        let task = start_handling(&f.relay, "forwarding-peer", MAKER, f.keep_alive(), true);
        assert_eq!(octx(&f.relay).sync_answer_deferral.registrations_len(), 0);
        ask(&f.relay, peer(), 2, MAKER, &[PAIR], &answers_tx).unwrap();
        assert_missing_pair(&answers.try_recv().expect("answered at once").1);
        drop(task);
        release_worker(&f.relay);
        assert_tables_empty(&f.relay);
    }

    #[test]
    fn a_registered_pair_together_with_an_unregistered_missing_pair_is_answered_at_once() {
        let f = Fixture::new(json!({}));
        let (answers_tx, answers) = std_mpsc::channel();
        hold_worker(&f.relay);
        let task = start_handling(&f.relay, "forwarding-peer", MAKER, f.keep_alive(), true);
        assert_eq!(octx(&f.relay).sync_answer_deferral.registrations_len(), 1);

        ask(&f.relay, peer(), 1, MAKER, &[PAIR, OTHER_PAIR], &answers_tx).unwrap();
        assert_missing_pair(&answers.try_recv().expect("answered at once").1);
        assert_eq!(octx(&f.relay).sync_answer_deferral.parked_len(), 0);
        assert_eq!(octx(&f.relay).sync_answer_deferral.registrations_len(), 1);

        drop(task);
        release_worker(&f.relay);
        assert_tables_empty(&f.relay);
    }

    #[test]
    fn registrations_are_limited_apart_from_the_parked_requests_and_a_parked_one_counts_against_the_latter() {
        let f = Fixture::new(small_limits(2, 3));
        let (answers_tx, answers) = std_mpsc::channel();
        hold_worker(&f.relay);
        // Two keep-alives from F register, the third from F is over the per-peer limit; G's registers, which
        // fills the total, and H's is over it.
        let keep_alives = [
            ("F", "03a0"),
            ("F", "03a1"),
            ("F", "03a2"),
            ("G", "03a3"),
            ("H", "03a4"),
        ];
        let tasks: Vec<Handling> = keep_alives
            .iter()
            .map(|(from, pubkey)| {
                start_handling(&f.relay, from, pubkey, keep_alive_naming(&[(PAIR, some_root())]), true)
            })
            .collect();
        let deferral = octx(&f.relay).sync_answer_deferral.clone();
        assert_eq!(deferral.registrations_len(), 3);
        assert!(deferral.registration_counts_agree());
        assert_eq!(octx(&f.relay).sync_log.snapshot().registrations_capped, 2);

        // A request for the pairs of the two that were handled without a registration is answered at once.
        for (id, pubkey) in [(1, "03a2"), (2, "03a4")] {
            ask(&f.relay, peer(), id, pubkey, &[PAIR], &answers_tx).unwrap();
            assert_missing_pair(&answers.try_recv().expect("answered at once").1);
        }

        // Requests for registered pairs are parked, and they count against the parked limits (2 and 3).
        let asking = peer();
        ask(&f.relay, asking, 10, "03a0", &[PAIR], &answers_tx).unwrap();
        ask(&f.relay, asking, 11, "03a1", &[PAIR], &answers_tx).unwrap();
        assert!(answers.try_recv().is_err());
        assert_eq!(deferral.parked_len(), 2);
        ask(&f.relay, asking, 12, "03a0", &[PAIR], &answers_tx).unwrap();
        assert_eq!(answers.try_recv().expect("over the per-peer limit").0, 12);
        ask(&f.relay, peer(), 13, "03a0", &[PAIR], &answers_tx).unwrap();
        assert!(answers.try_recv().is_err());
        assert_eq!(deferral.parked_len(), 3);
        ask(&f.relay, peer(), 14, "03a1", &[PAIR], &answers_tx).unwrap();
        assert_eq!(answers.try_recv().expect("over the total limit").0, 14);
        assert_eq!(deferral.parked_len(), 3);

        drop(tasks);
        let mut ids: Vec<usize> = (0..3)
            .map(|_| {
                let (id, res) = answered(&answers, 3000);
                assert_missing_pair(&res);
                id
            })
            .collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![10, 11, 13]);
        release_worker(&f.relay);
        assert_tables_empty(&f.relay);

        let log = &octx(&f.relay).sync_log;
        let answers_of_the_window = log.answers();
        assert_eq!(
            answers_of_the_window
                .iter()
                .filter(|(_, path)| *path == AnswerPath::AtOnce(AtOnce::CapsFull))
                .count(),
            2
        );
        assert_eq!(
            answers_of_the_window
                .iter()
                .filter(|(_, path)| *path == AnswerPath::PreParkedWoken)
                .count(),
            3
        );
        let counters = log.snapshot();
        assert_eq!(
            (
                counters.pre_parked_woken,
                counters.pre_parked_bound,
                counters.answers_parked,
                counters.registrations_capped
            ),
            (3, 0, 3, 2)
        );
        // The part of the summary line that item (f) adds; a capped registration is reported as a failure.
        assert!(counters
            .summary_line()
            .expect("a window with capped registrations is reported")
            .ends_with("; pre_placeholder parked_woken=3 parked_bound=0 registrations_capped=2"));
    }

    #[test]
    fn nothing_is_registered_on_a_non_relay_with_the_switch_off_or_with_a_zero_bound() {
        let cases = [
            (json!({}), false),
            (json!({ "sync_answer_deferral_before_placeholder": false }), true),
            (json!({ "sync_answer_deferral_ms": 0 }), true),
        ];
        for (conf, i_am_relay) in cases {
            let f = Fixture::new(conf);
            let (answers_tx, answers) = std_mpsc::channel();
            hold_worker(&f.relay);
            let task = start_handling(&f.relay, "forwarding-peer", MAKER, f.keep_alive(), i_am_relay);
            assert_eq!(octx(&f.relay).sync_answer_deferral.registrations_len(), 0);
            ask(&f.relay, peer(), 1, MAKER, &[PAIR], &answers_tx).unwrap();
            assert_missing_pair(&answers.try_recv().expect("answered at once").1);
            assert_eq!(octx(&f.relay).sync_answer_deferral.parked_len(), 0);

            drop(task);
            release_worker(&f.relay);
            assert_tables_empty(&f.relay);
            let counters = octx(&f.relay).sync_log.snapshot();
            assert_eq!(
                (
                    counters.pre_parked_woken,
                    counters.pre_parked_bound,
                    counters.registrations_capped
                ),
                (0, 0, 0)
            );
        }
    }

    /// Only a JSON `false` turns the parking before the placeholder exists off.
    #[test]
    fn the_before_placeholder_switch_is_off_only_for_a_json_false() {
        let switch = |conf: Json| {
            let ctx = MmCtxBuilder::new().with_conf(conf).into_mm_arc();
            sync_answer_deferral_before_placeholder(&ctx)
        };
        assert!(switch(json!({})));
        assert!(switch(json!({ "sync_answer_deferral_before_placeholder": true })));
        assert!(!switch(json!({ "sync_answer_deferral_before_placeholder": false })));
        assert!(switch(json!({ "sync_answer_deferral_before_placeholder": "false" })));
        assert!(switch(json!({ "sync_answer_deferral_before_placeholder": 0 })));
        assert!(switch(json!({ "sync_answer_deferral_before_placeholder": null })));
    }

    // ---- CRD ch.32 R-GH12a (T-GH12a): the order-book answer waits briefly for the trie worker.

    /// Puts `n` orders of `pubkey` for `C1:C2` into the read index of `ctx` and counts their insertion as queued,
    /// but does not hand it to the trie worker: no record of the pubkey exists until [`apply_held`] is called
    /// with the returned operations.
    fn index_without_record(ctx: &MmArc, pubkey: &str, n: usize) -> (Vec<Uuid>, Vec<TrieOp>) {
        let octx = octx(ctx);
        let mut orderbook = octx.orderbook.lock();
        let mut uuids = Vec::new();
        let mut ops = Vec::new();
        for order in make_random_orders(pubkey.to_owned(), &[0; 32], "C1".into(), "C2".into(), n) {
            uuids.push(order.uuid);
            ops.extend(orderbook.index_insert_or_update(order));
        }
        orderbook.trie_ops_enqueued += 1;
        (uuids, ops)
    }

    /// The worker applies a batch that [`index_without_record`] (or [`cancel_in_index`]) counted.
    fn apply_held(ctx: &MmArc, ops: Vec<TrieOp>) { octx(ctx).trie_store.lock().apply_batch(ops); }

    /// The orders are cancelled in the read index; their removal counts as queued as well. Returns the removal.
    fn cancel_in_index(ctx: &MmArc, uuids: &[Uuid]) -> Vec<TrieOp> {
        let octx = octx(ctx);
        let mut orderbook = octx.orderbook.lock();
        let ops: Vec<TrieOp> = uuids
            .iter()
            .filter_map(|uuid| orderbook.index_remove(*uuid).map(|(_, op)| op))
            .collect();
        orderbook.trie_ops_enqueued += 1;
        ops
    }

    fn ask_orderbook(relay: &MmArc, asking: PeerId, id: usize, answers: &Answers) -> Result<(), String> {
        let answers = answers.clone();
        let reply: SyncAnswerReply = Box::new(move |res| answers.send((id, res)).map_err(|e| e.to_string()));
        answer_orderbook_request(relay, asking, "C1".into(), "C2".into(), reply)
    }

    fn orderbook_answer(res: &AdexResponse) -> GetOrderbookRes {
        match res {
            AdexResponse::Ok { response } => decode_message(response).unwrap(),
            other => panic!("expected an answer, got {:?}", other),
        }
    }

    fn assert_missing_record_error(res: &AdexResponse) {
        assert_wire_form(res, "TrieStore::pubkeys_state is expected to contain");
    }

    #[test]
    fn the_orderbook_answer_waits_for_a_worker_that_catches_up_within_the_bound() {
        let (relay, _cmds) = node(json!({}));
        // A bound far longer than the real one: the outcome does not depend on how fast this thread runs.
        const LONG_WAIT_MS: u64 = 2000;
        let (uuids, ops) = index_without_record(&relay, MAKER, 1);
        let (answers_tx, answers) = std_mpsc::channel();

        let started = Instant::now();
        let reply: SyncAnswerReply = {
            let answers = answers_tx.clone();
            Box::new(move |res| answers.send((1, res)).map_err(|e| e.to_string()))
        };
        answer_orderbook_request_within(&relay, peer(), "C1".into(), "C2".into(), LONG_WAIT_MS, reply).unwrap();
        assert!(
            started.elapsed() < Duration::from_millis(LONG_WAIT_MS / 2),
            "the inbound handler waited"
        );
        assert!(
            answers.try_recv().is_err(),
            "the request waits, it is not answered at once"
        );
        assert_eq!(octx(&relay).sync_answer_deferral.parked_len(), 1);

        let applied_at_secs = now_ms() / 1000;
        apply_held(&relay, ops);
        let (id, res) = answered(&answers, 5000);
        assert_eq!(id, 1);
        // The slot is free by the time the answer arrives.
        assert_eq!(octx(&relay).sync_answer_deferral.parked_len(), 0);
        let mut answer = orderbook_answer(&res);
        let item = answer.pubkey_orders.remove(MAKER).expect("the maker is listed");
        assert_eq!(item.orders.len(), 1);
        assert_eq!(item.orders[0].0, uuids[0]);
        assert!(item.last_keep_alive >= applied_at_secs, "{}", item.last_keep_alive);
        assert!(answer.pubkey_orders.is_empty());
        assert_tables_empty(&relay);
    }

    #[test]
    fn the_orderbook_answer_fails_when_the_worker_is_held_past_the_bound_and_not_before() {
        let (relay, _cmds) = node(json!({}));
        let (_, ops) = index_without_record(&relay, MAKER, 1);
        let (answers_tx, answers) = std_mpsc::channel();

        let started = Instant::now();
        ask_orderbook(&relay, peer(), 1, &answers_tx).unwrap();
        assert!(answers
            .recv_timeout(Duration::from_millis(TRIE_CATCH_UP_BOUND_MS - 100))
            .is_err());
        let (_, res) = answered(&answers, 3000);
        let waited = started.elapsed();
        assert!(
            waited >= Duration::from_millis(TRIE_CATCH_UP_BOUND_MS - 5),
            "answered after {:?}",
            waited
        );
        assert!(
            waited < Duration::from_millis(TRIE_CATCH_UP_BOUND_MS * 8),
            "{:?}",
            waited
        );
        assert_missing_record_error(&res);
        assert_tables_empty(&relay);
        // Keep the node consistent for its worker.
        apply_held(&relay, ops);
    }

    #[test]
    fn an_order_cancelled_while_the_request_waits_is_left_out_and_the_request_does_not_fail() {
        let (relay, _cmds) = node(json!({}));
        let (uuids, insertion) = index_without_record(&relay, MAKER, 1);
        let (answers_tx, answers) = std_mpsc::channel();
        ask_orderbook(&relay, peer(), 1, &answers_tx).unwrap();
        assert!(answers.try_recv().is_err());

        // The cancellation reaches the index, and the worker then applies the insertion and the removal.
        let removal = cancel_in_index(&relay, &uuids);
        apply_held(&relay, insertion);
        apply_held(&relay, removal);
        let (_, res) = answered(&answers, 2000);
        assert!(orderbook_answer(&res).pubkey_orders.is_empty());
        assert_tables_empty(&relay);
    }

    #[test]
    fn a_cancelled_order_whose_pubkey_never_gets_a_record_is_answered_without_it_at_the_bound() {
        let (relay, _cmds) = node(json!({}));
        let (uuids, _insertion) = index_without_record(&relay, MAKER, 1);
        let (answers_tx, answers) = std_mpsc::channel();
        let started = Instant::now();
        ask_orderbook(&relay, peer(), 1, &answers_tx).unwrap();
        let _removal = cancel_in_index(&relay, &uuids);

        let (_, res) = answered(&answers, 3000);
        assert!(started.elapsed() >= Duration::from_millis(TRIE_CATCH_UP_BOUND_MS - 5));
        assert!(orderbook_answer(&res).pubkey_orders.is_empty());
        assert_tables_empty(&relay);
    }

    #[test]
    fn a_pair_with_more_than_1000_orders_fails_at_once() {
        let (relay, _cmds) = node(json!({}));
        index_without_record(&relay, MAKER, MAX_ORDERS_NUMBER_IN_ORDERBOOK_RESPONSE + 1);
        let (answers_tx, answers) = std_mpsc::channel();
        ask_orderbook(&relay, peer(), 1, &answers_tx).unwrap();
        assert_wire_form(&answers.try_recv().expect("answered at once").1, "Orderbook too large");
        assert_eq!(octx(&relay).sync_answer_deferral.parked_len(), 0);
        assert_tables_empty(&relay);
    }

    #[test]
    fn the_orderbook_answer_fails_at_once_with_the_switch_off() {
        let (relay, _cmds) = node(json!({ "orderbook_answer_waits_for_worker": false }));
        index_without_record(&relay, MAKER, 1);
        let (answers_tx, answers) = std_mpsc::channel();
        ask_orderbook(&relay, peer(), 1, &answers_tx).unwrap();
        assert_missing_record_error(&answers.try_recv().expect("answered at once").1);
        assert_eq!(octx(&relay).sync_answer_deferral.parked_len(), 0);
        assert_tables_empty(&relay);
    }

    #[test]
    fn a_waiting_orderbook_request_counts_against_the_parked_limits() {
        let mut f = Fixture::new(small_limits(2, 3));
        let (answers_tx, answers) = std_mpsc::channel();
        let mut own = f.start_own_sync();
        let asking = peer();
        for id in 0..2 {
            ask(&f.relay, asking, id, MAKER, &[PAIR], &answers_tx).unwrap();
        }
        assert_eq!(octx(&f.relay).sync_answer_deferral.parked_len(), 2);

        // A pubkey the index lists with no record, so that an order-book answer has to wait.
        let (_, ops) = index_without_record(&f.relay, "03other", 1);

        // The peer already has two parked requests: its order-book request fails at once.
        ask_orderbook(&f.relay, asking, 100, &answers_tx).unwrap();
        let (id, res) = answers.try_recv().expect("answered at once");
        assert_eq!(id, 100);
        assert_missing_record_error(&res);

        // Another peer's waits, which fills the total; a third peer's fails at once.
        ask_orderbook(&f.relay, peer(), 101, &answers_tx).unwrap();
        assert!(answers.try_recv().is_err());
        assert_eq!(octx(&f.relay).sync_answer_deferral.parked_len(), 3);
        ask_orderbook(&f.relay, peer(), 102, &answers_tx).unwrap();
        let (id, res) = answers.try_recv().expect("answered at once");
        assert_eq!(id, 102);
        assert_missing_record_error(&res);

        // The worker catches up: the waiting order-book request is answered, then the own request lands and
        // the parked synchronization requests are answered.
        apply_held(&f.relay, ops);
        let (id, res) = answered(&answers, 2000);
        assert_eq!(id, 101);
        assert!(orderbook_answer(&res).pubkey_orders.contains_key("03other"));
        own.answer(&f.maker);
        for _ in 0..2 {
            assert_eq!(full_set_len(&answered(&answers, 3000).1), ORDERS);
        }
        assert!(own.finish());
        assert_tables_empty(&f.relay);
    }

    /// Only a JSON `false` turns the wait of the order-book answer off.
    #[test]
    fn the_orderbook_wait_switch_is_off_only_for_a_json_false() {
        let switch = |conf: Json| {
            let ctx = MmCtxBuilder::new().with_conf(conf).into_mm_arc();
            orderbook_answer_waits_for_worker(&ctx)
        };
        assert!(switch(json!({})));
        assert!(switch(json!({ "orderbook_answer_waits_for_worker": true })));
        assert!(!switch(json!({ "orderbook_answer_waits_for_worker": false })));
        assert!(switch(json!({ "orderbook_answer_waits_for_worker": "false" })));
        assert!(switch(json!({ "orderbook_answer_waits_for_worker": 0 })));
        assert!(switch(json!({ "orderbook_answer_waits_for_worker": null })));
        // Independent of the switches of R-Q5b.
        assert!(switch(json!({ "sync_answer_deferral_ms": 0 })));
        assert!(switch(json!({ "sync_answer_deferral_before_placeholder": false })));
    }
}

/// `my_orders` speed work: the decimals of an order's numbers come from a content-addressed memo and the
/// answer is serialized straight from the typed response. The answer must stay identical, digit for digit,
/// to what the unmemoized conversion and the `json!` tree produced, so the reference path below keeps the
/// old code unchanged and every order's bytes are compared against it.
mod my_orders_speed_tests {
    use super::*;
    use rand::rngs::SmallRng;
    use rand::SeedableRng;
    use serde_json::value::RawValue;
    use std::collections::BTreeMap;
    use std::time::{Duration as StdDuration, Instant};

    #[derive(Clone, Copy, Debug)]
    enum Profile {
        /// Volumes and prices that are non-terminating rationals, as a price derived from a division gives.
        NonTerminating,
        /// Values born from 8-decimal strings, as a user types them into `setprice`.
        DecimalBorn,
    }

    fn decimal_born(int: u64, frac: u64) -> MmNumber {
        let d = <BigDecimal as std::str::FromStr>::from_str(&format!("{int}.{frac:08}")).unwrap();
        MmNumber::from(d)
    }

    fn new_maker_order(base: String, rel: String, profile: Profile, rng: &mut SmallRng) -> MakerOrder {
        let (max_base_vol, min_base_vol, price) = match profile {
            Profile::NonTerminating => (
                MmNumber::from((
                    rng.gen_range(100_000_000..9_999_999_999u64),
                    rng.gen_range(1_000..99_999u64),
                )),
                MmNumber::from((rng.gen_range(1_000..99_999u64), rng.gen_range(1_000_000..9_999_999u64))),
                MmNumber::from((
                    rng.gen_range(1_000_000..999_999_999u64),
                    rng.gen_range(1_000..999_999u64),
                )),
            ),
            Profile::DecimalBorn => (
                decimal_born(rng.gen_range(1..9_999), rng.gen_range(0..99_999_999)),
                decimal_born(0, rng.gen_range(1..99_999_999)),
                decimal_born(rng.gen_range(0..999), rng.gen_range(0..99_999_999)),
            ),
        };
        MakerOrder {
            base,
            rel,
            created_at: now_ms(),
            updated_at: Some(now_ms()),
            max_base_vol,
            min_base_vol,
            price,
            matches: HashMap::new(),
            started_swaps: Vec::new(),
            uuid: Uuid::new_v4(),
            conf_settings: None,
            changes_history: None,
            save_in_history: true,
            base_orderbook_ticker: None,
            rel_orderbook_ticker: None,
            p2p_privkey: None,
            timeout_in_minutes: None,
            swap_version: SwapVersion::default(),
        }
    }

    fn maker_match(order: &MakerOrder, reserved_base: MmNumber, connected: bool) -> MakerMatch {
        let taker_uuid = Uuid::new_v4();
        let rel_amount = &reserved_base * &order.price;
        let request = TakerRequest {
            uuid: taker_uuid,
            base: order.base.clone(),
            rel: order.rel.clone(),
            base_amount: reserved_base.clone(),
            rel_amount: rel_amount.clone(),
            sender_pubkey: H256Json::default(),
            dest_pub_key: H256Json::default(),
            action: TakerAction::Buy,
            match_by: MatchBy::Any,
            conf_settings: None,
            base_protocol_info: None,
            rel_protocol_info: None,
            swap_version: SwapVersion::default(),
        };
        MakerMatch {
            request,
            reserved: MakerReserved {
                base: order.base.clone(),
                rel: order.rel.clone(),
                base_amount: reserved_base,
                rel_amount,
                sender_pubkey: H256Json::default(),
                dest_pub_key: H256Json::default(),
                maker_order_uuid: order.uuid,
                taker_order_uuid: taker_uuid,
                conf_settings: None,
                base_protocol_info: None,
                rel_protocol_info: None,
                swap_version: SwapVersion::default(),
            },
            connect: Some(TakerConnect {
                taker_order_uuid: taker_uuid,
                maker_order_uuid: order.uuid,
                sender_pubkey: H256Json::default(),
                dest_pub_key: H256Json::default(),
            }),
            connected: if connected {
                Some(MakerConnected {
                    taker_order_uuid: taker_uuid,
                    maker_order_uuid: order.uuid,
                    method: "connected".into(),
                    sender_pubkey: H256Json::default(),
                    dest_pub_key: H256Json::default(),
                })
            } else {
                None
            },
            last_updated: now_ms(),
        }
    }

    fn new_taker_order(rng: &mut SmallRng) -> TakerOrder {
        let uuid = Uuid::new_v4();
        let request = TakerRequest {
            base: "TAKERBASE".into(),
            rel: "TAKERREL".into(),
            uuid,
            dest_pub_key: H256Json::default(),
            sender_pubkey: H256Json::default(),
            base_amount: MmNumber::from((rng.gen_range(100_000..9_999_999u64), 7)),
            rel_amount: MmNumber::from((rng.gen_range(100_000..9_999_999u64), 13)),
            action: TakerAction::Sell,
            match_by: MatchBy::Any,
            conf_settings: None,
            base_protocol_info: None,
            rel_protocol_info: None,
            swap_version: SwapVersion::default(),
        };
        let maker_uuid = Uuid::new_v4();
        let mut matches = HashMap::new();
        matches.insert(maker_uuid, TakerMatch {
            reserved: MakerReserved {
                base: "TAKERBASE".into(),
                rel: "TAKERREL".into(),
                base_amount: MmNumber::from((1, 3)),
                rel_amount: MmNumber::from((2, 7)),
                sender_pubkey: H256Json::default(),
                dest_pub_key: H256Json::default(),
                maker_order_uuid: maker_uuid,
                taker_order_uuid: uuid,
                conf_settings: None,
                base_protocol_info: None,
                rel_protocol_info: None,
                swap_version: SwapVersion::default(),
            },
            connect: TakerConnect {
                taker_order_uuid: uuid,
                maker_order_uuid: maker_uuid,
                sender_pubkey: H256Json::default(),
                dest_pub_key: H256Json::default(),
            },
            connected: None,
            last_updated: now_ms(),
        });
        TakerOrder {
            request,
            matches,
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

    /// `n_base * n_rel` pairs, a bid and an ask each, so `2 * n_base * n_rel` maker orders. Every
    /// `matched_every`-th order (when non-zero) carries matches, so its `available_amount` differs from its
    /// `max_base_vol`. One taker order is added when `with_taker`.
    fn seed_orders(
        ctx: &MmArc,
        profile: Profile,
        n_base: usize,
        n_rel: usize,
        matched_every: usize,
        with_taker: bool,
        seed: u64,
    ) -> usize {
        let octx = OrdermatchContext::from_ctx(ctx).unwrap();
        let mut rng = SmallRng::seed_from_u64(seed);
        let mut count = 0;
        let add = |mut order: MakerOrder, count: &mut usize| {
            if matched_every != 0 && count.is_multiple_of(matched_every) {
                let third = &order.max_base_vol / &MmNumber::from(3);
                let seventh = &order.max_base_vol / &MmNumber::from(7);
                let first = maker_match(&order, third, true);
                let second = maker_match(&order, seventh, false);
                order.matches.insert(first.request.uuid, first);
                order.matches.insert(second.request.uuid, second);
            }
            *count += 1;
            let arc = Arc::new(AsyncMutex::new(order.clone()));
            octx.maker_orders_ctx.lock().add_order(&order, arc);
        };
        for b in 0..n_base {
            for r in 0..n_rel {
                add(
                    new_maker_order(format!("BASE{b}"), format!("REL{r:03}"), profile, &mut rng),
                    &mut count,
                );
                add(
                    new_maker_order(format!("REL{r:03}"), format!("BASE{b}"), profile, &mut rng),
                    &mut count,
                );
            }
        }
        if with_taker {
            let taker = new_taker_order(&mut rng);
            block_on(octx.my_taker_orders.lock()).insert(taker.request.uuid, taker);
        }
        count
    }

    // ---- the reference path: the conversion and the `json!` tree as they were before the change ----

    /// `MakerOrderForRpc` as it was: the same fields in the same order, the numbers plain `BigDecimal`s.
    #[derive(Serialize)]
    struct ReferenceMakerOrderForRpc<'a> {
        base: &'a str,
        rel: &'a str,
        price: BigDecimal,
        price_rat: &'a MmNumber,
        max_base_vol: BigDecimal,
        max_base_vol_rat: &'a MmNumber,
        min_base_vol: BigDecimal,
        min_base_vol_rat: &'a MmNumber,
        created_at: u64,
        updated_at: Option<u64>,
        matches: HashMap<Uuid, MakerMatchForRpc<'a>>,
        started_swaps: &'a [Uuid],
        uuid: Uuid,
        conf_settings: &'a Option<OrderConfirmationsSettings>,
        #[serde(skip_serializing_if = "Option::is_none")]
        changes_history: &'a Option<Vec<HistoricalOrder>>,
        base_orderbook_ticker: &'a Option<String>,
        rel_orderbook_ticker: &'a Option<String>,
    }

    /// `MakerOrderForMyOrdersRpc` as it was.
    #[derive(Serialize)]
    struct ReferenceMakerOrderForMyOrdersRpc<'a> {
        #[serde(flatten)]
        order: ReferenceMakerOrderForRpc<'a>,
        cancellable: bool,
        available_amount: BigDecimal,
    }

    fn reference_maker_for_rpc(order: &MakerOrder) -> ReferenceMakerOrderForRpc<'_> {
        ReferenceMakerOrderForRpc {
            base: &order.base,
            rel: &order.rel,
            price: order.price.to_decimal(),
            price_rat: &order.price,
            max_base_vol: order.max_base_vol.to_decimal(),
            max_base_vol_rat: &order.max_base_vol,
            min_base_vol: order.min_base_vol.to_decimal(),
            min_base_vol_rat: &order.min_base_vol,
            created_at: order.created_at,
            updated_at: order.updated_at,
            matches: order
                .matches
                .iter()
                .map(|(uuid, order_match)| (*uuid, order_match.into()))
                .collect(),
            started_swaps: &order.started_swaps,
            uuid: order.uuid,
            conf_settings: &order.conf_settings,
            changes_history: &order.changes_history,
            base_orderbook_ticker: &order.base_orderbook_ticker,
            rel_orderbook_ticker: &order.rel_orderbook_ticker,
        }
    }

    fn reference_maker_for_my_orders(order: &MakerOrder) -> ReferenceMakerOrderForMyOrdersRpc<'_> {
        ReferenceMakerOrderForMyOrdersRpc {
            order: reference_maker_for_rpc(order),
            cancellable: order.is_cancellable(),
            available_amount: order.available_amount().into(),
        }
    }

    /// `my_orders` as it was: convert with the unmemoized function, build a `json!` tree, write it out.
    async fn reference_my_orders(ctx: MmArc) -> Vec<u8> {
        let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
        let my_maker_orders = ordermatch_ctx.maker_orders_ctx.lock().clone_orders();
        let mut maker_orders_map = HashMap::with_capacity(my_maker_orders.len());
        for (uuid, order_mutex) in my_maker_orders.iter() {
            let order = order_mutex.lock().await.clone();
            maker_orders_map.insert(uuid, order);
        }
        let maker_orders_for_rpc: HashMap<_, _> = maker_orders_map
            .iter()
            .map(|(uuid, order)| (uuid, reference_maker_for_my_orders(order)))
            .collect();

        let taker_orders = ordermatch_ctx.my_taker_orders.lock().await;
        let taker_orders_for_rpc: HashMap<_, _> = taker_orders
            .iter()
            .map(|(uuid, order)| (uuid, TakerOrderForRpc::from(order)))
            .collect();
        let res = json!({
            "result": {
                "maker_orders": maker_orders_for_rpc,
                "taker_orders": taker_orders_for_rpc,
            }
        });
        json::to_vec(&res).unwrap()
    }

    /// An answer split per order: each order's object as the exact bytes it was written with. The maps are
    /// sorted by uuid, so two answers whose hash maps iterate in different orders compare equal when every
    /// object is the same.
    #[derive(Deserialize)]
    struct SplitBody {
        result: SplitResult,
    }

    #[derive(Deserialize)]
    struct SplitResult {
        maker_orders: BTreeMap<String, Box<RawValue>>,
        taker_orders: BTreeMap<String, Box<RawValue>>,
    }

    fn split(body: &[u8]) -> SplitBody {
        assert!(
            body.starts_with(br#"{"result":{"maker_orders":{"#),
            "unexpected top-level structure"
        );
        json::from_slice(body).unwrap()
    }

    /// Panics at the first order whose bytes differ, naming the uuid and the first differing offset.
    fn assert_same_per_order(expected: &SplitBody, actual: &SplitBody) {
        for (kind, exp, act) in [
            (
                "maker_orders",
                &expected.result.maker_orders,
                &actual.result.maker_orders,
            ),
            (
                "taker_orders",
                &expected.result.taker_orders,
                &actual.result.taker_orders,
            ),
        ] {
            assert_eq!(
                exp.keys().collect::<Vec<_>>(),
                act.keys().collect::<Vec<_>>(),
                "{kind}: different uuids"
            );
            for (uuid, e) in exp {
                let a = &act[uuid];
                if e.get() == a.get() {
                    continue;
                }
                // `matches` is a hash map and iterates in a different order per map instance, in the
                // reference as much as in the new path. An order with two or more matches may therefore
                // differ in the order of the entries of `matches` only: same document, nothing else.
                let (ev, av): (Json, Json) = (json::from_str(e.get()).unwrap(), json::from_str(a.get()).unwrap());
                let several_matches = ev["matches"].as_object().is_some_and(|m| m.len() > 1);
                if several_matches && ev == av {
                    continue;
                }
                let at = e.get().bytes().zip(a.get().bytes()).position(|(x, y)| x != y);
                panic!(
                    "{kind} {uuid}: bytes differ at offset {at:?}\nexpected {}\nactual   {}",
                    e.get(),
                    a.get()
                );
            }
        }
    }

    fn run_my_orders(ctx: &MmArc) -> Vec<u8> { block_on(my_orders(ctx.clone())).unwrap().into_body() }

    fn byte_identity_for(profile: Profile) {
        let ctx = MmCtxBuilder::new().into_mm_arc();
        // 5 bases x 200 rels x 2 directions = 2,000 maker orders, every 10th matched, one taker order.
        let n = seed_orders(&ctx, profile, 5, 200, 10, true, 20261028);
        assert_eq!(n, 2000);
        let octx = OrdermatchContext::from_ctx(&ctx).unwrap();
        assert!(octx.maker_orders_ctx.lock().clone_orders().len() == 2000);
        assert_eq!(block_on(octx.my_taker_orders.lock()).len(), 1);

        let expected_body = block_on(reference_my_orders(ctx.clone()));
        let expected = split(&expected_body);
        assert_eq!(expected.result.maker_orders.len(), 2000);
        assert_eq!(expected.result.taker_orders.len(), 1);
        // The matched orders are really in the comparison: their available amount differs from the maximum.
        let matched_differs = expected.result.maker_orders.values().any(|raw| {
            let v: Json = json::from_str(raw.get()).unwrap();
            !v["matches"].as_object().unwrap().is_empty() && v["available_amount"] != v["max_base_vol"]
        });
        assert!(
            matched_differs,
            "no matched order with available_amount != max_base_vol"
        );

        // First call fills the memo from empty (every number a miss for these fresh values), the next ones
        // are served from it. Both must equal the reference.
        for call in 0..3 {
            let actual_body = run_my_orders(&ctx);
            assert_same_per_order(&expected, &split(&actual_body));
            // Same JSON document too, modulo the order of keys inside the hash maps.
            let e: Json = json::from_slice(&expected_body).unwrap();
            let a: Json = json::from_slice(&actual_body).unwrap();
            assert_eq!(e, a, "call {call}");
        }
    }

    #[test]
    fn my_orders_bytes_identical_non_terminating_values() { byte_identity_for(Profile::NonTerminating); }

    #[test]
    fn my_orders_bytes_identical_decimal_born_values() { byte_identity_for(Profile::DecimalBorn); }

    #[test]
    fn my_orders_empty_answer_is_unchanged() {
        let ctx = MmCtxBuilder::new().into_mm_arc();
        let body = run_my_orders(&ctx);
        assert_eq!(body, br#"{"result":{"maker_orders":{},"taker_orders":{}}}"#.to_vec());
        assert_eq!(body, block_on(reference_my_orders(ctx)));
    }

    /// The comparison itself: a single changed digit in one order is reported.
    #[test]
    #[should_panic(expected = "bytes differ")]
    fn per_order_comparison_catches_one_digit() {
        let ctx = MmCtxBuilder::new().into_mm_arc();
        seed_orders(&ctx, Profile::NonTerminating, 1, 3, 0, false, 7);
        let body = run_my_orders(&ctx);
        let expected = split(&body);
        let mut actual = split(&body);
        let (uuid, raw) = actual
            .result
            .maker_orders
            .iter()
            .next()
            .map(|(k, v)| (k.clone(), v.get().to_string()))
            .unwrap();
        let at = raw.find("\"price\":\"").unwrap() + "\"price\":\"".len();
        let digit = raw.as_bytes()[at];
        let replaced = if digit == b'9' { '1' } else { (digit + 1) as char };
        let changed = format!("{}{}{}", &raw[..at], replaced, &raw[at + 1..]);
        actual
            .result
            .maker_orders
            .insert(uuid, RawValue::from_string(changed).unwrap());
        assert_same_per_order(&expected, &actual);
    }

    /// Several tasks calling `my_orders` at once, the memo shared, all answers equal to the reference.
    #[test]
    fn my_orders_concurrent_calls_agree_with_reference() {
        let ctx = MmCtxBuilder::new().into_mm_arc();
        seed_orders(&ctx, Profile::NonTerminating, 2, 50, 7, true, 99);
        let expected_body = block_on(reference_my_orders(ctx.clone()));
        let expected = Arc::new(split(&expected_body));
        let handles: Vec<_> = (0..6)
            .map(|_| {
                let (ctx, expected) = (ctx.clone(), expected.clone());
                std::thread::spawn(move || {
                    for _ in 0..5 {
                        let actual = split(&run_my_orders(&ctx));
                        assert_same_per_order(&expected, &actual);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("a worker thread panicked");
        }
    }

    /// The rendering shared by `setprice`, `update_maker_order` and `order_status` is `MakerOrderForRpc`:
    /// it must carry the unmemoized decimals, for fresh values and for repeated ones.
    #[test]
    fn maker_order_for_rpc_matches_unmemoized_conversion() {
        let mut rng = SmallRng::seed_from_u64(5);
        for profile in [Profile::NonTerminating, Profile::DecimalBorn] {
            for _ in 0..200 {
                let order = new_maker_order("A".into(), "B".into(), profile, &mut rng);
                for _ in 0..2 {
                    let actual = json::to_vec(&MakerOrderForRpc::from(&order)).unwrap();
                    let expected = json::to_vec(&reference_maker_for_rpc(&order)).unwrap();
                    assert_eq!(String::from_utf8(actual).unwrap(), String::from_utf8(expected).unwrap());
                }
            }
        }
    }

    /// `available_amount_rendered` against the unmemoized `available_amount().to_decimal()`: without
    /// matches (memoized under `max_base_vol`, hit on the second call), with matches, and for a maximum
    /// stored as an unreduced fraction, which the subtraction reduces.
    #[test]
    fn available_amount_rendered_matches_unmemoized() {
        let mut rng = SmallRng::seed_from_u64(11);
        for profile in [Profile::NonTerminating, Profile::DecimalBorn] {
            for i in 0..100usize {
                let mut order = new_maker_order("A".into(), "B".into(), profile, &mut rng);
                if i.is_multiple_of(3) {
                    let numer = order.max_base_vol.numer() * common::mm_number::BigInt::from(6);
                    let denom = order.max_base_vol.denom() * common::mm_number::BigInt::from(6);
                    order.max_base_vol = MmNumber::from(BigRational::new_raw(numer, denom));
                }
                for _ in 0..2 {
                    assert_eq!(
                        order.available_amount_rendered().as_str(),
                        order.available_amount().to_decimal().to_string()
                    );
                }
                let third = &order.max_base_vol / &MmNumber::from(3);
                let reserved = maker_match(&order, third, false);
                order.matches.insert(reserved.request.uuid, reserved);
                for _ in 0..2 {
                    assert_eq!(
                        order.available_amount_rendered().as_str(),
                        order.available_amount().to_decimal().to_string()
                    );
                }
            }
        }
    }

    // ---- timing: old handler against new on the same orders ----

    fn median(v: &mut [f64]) -> f64 {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    }

    fn ms(d: StdDuration) -> f64 { d.as_secs_f64() * 1000.0 }

    fn describe(label: &str, mut v: Vec<f64>) -> f64 {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let (min, max, med) = (v[0], v[v.len() - 1], median(&mut v));
        println!(
            "TIMING {label}: n={} min={min:.2} median={med:.2} max={max:.2} (ms)",
            v.len()
        );
        med
    }

    /// Run with `cargo test --release -p mm2_main --lib my_orders_timing -- --ignored --nocapture`.
    /// Targets at 2,000 orders: decimal-born values under 10 ms median, non-terminating under 50 ms.
    #[test]
    #[ignore]
    fn my_orders_timing_old_vs_new() {
        println!("TIMING debug_assertions={}", cfg!(debug_assertions));
        for (profile, target_ms) in [(Profile::DecimalBorn, 10.0), (Profile::NonTerminating, 50.0)] {
            // Several independent runs, each on freshly generated orders: the first new call of a run
            // starts with a memo that has none of these values (cold); the following ones hit (warm).
            let (mut old_all, mut new_cold, mut new_warm) = (vec![], vec![], vec![]);
            for run in 0..7u64 {
                let ctx = MmCtxBuilder::new().into_mm_arc();
                seed_orders(&ctx, profile, 5, 200, 0, false, 1000 + run);
                for _ in 0..2 {
                    block_on(reference_my_orders(ctx.clone()));
                }
                let t = Instant::now();
                run_my_orders(&ctx);
                new_cold.push(ms(t.elapsed()));
                let mut old_run = vec![];
                let mut new_run = vec![];
                for _ in 0..9 {
                    let t = Instant::now();
                    block_on(reference_my_orders(ctx.clone()));
                    old_run.push(ms(t.elapsed()));
                    let t = Instant::now();
                    run_my_orders(&ctx);
                    new_run.push(ms(t.elapsed()));
                }
                println!(
                    "TIMING {profile:?} run {run}: old median {:.2} ms, new warm median {:.2} ms",
                    median(&mut old_run.clone()),
                    median(&mut new_run.clone())
                );
                old_all.extend(old_run);
                new_warm.extend(new_run);
            }
            println!("TIMING ===== {profile:?}, 2000 orders, target {target_ms} ms =====");
            describe("old handler", old_all);
            describe("new handler, cold memo (first call)", new_cold);
            let warm = describe("new handler, warm memo", new_warm);
            println!("TIMING {profile:?} warm median {warm:.2} ms vs target {target_ms} ms");
        }
    }
}

/// CRD ch.32 §32.9B (Reloaded-original, maker publication rate): the outbox, the priority marking, the
/// keep-alive ordering, the reconnect round, the metrics and the switch. T-MP1 and T-MP4 are the unit tests of
/// `maker_outbox.rs`.
mod maker_publication_tests {
    use super::*;
    use crate::mm2::lp_ordermatch::new_protocol::OrdermatchMessage;
    use mm2_metrics::MetricsOps;

    /// A node with a channel that receives what it publishes. The channel is large enough for every test.
    pub(super) fn node(conf: Json) -> (MmArc, mpsc::Receiver<AdexBehaviourCmd>) {
        let ctx = MmCtxBuilder::new().with_conf(conf).into_mm_arc();
        let _ = ctx.init_metrics();
        CryptoCtx::init_with_iguana_passphrase(ctx.clone(), "maker publication test passphrase").unwrap();
        let (tx, rx) = mpsc::channel(20_000);
        P2PContext::new(tx).store_to_mm_arc(&ctx);
        (ctx, rx)
    }

    pub(super) fn own_order(base: &str, rel: &str) -> MakerOrder {
        MakerOrder {
            uuid: Uuid::new_v4(),
            base: base.into(),
            rel: rel.into(),
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

    pub(super) fn pubkey_of(ctx: &MmArc) -> String { hex::encode(ctx.secp256k1_key_pair().public_slice()) }

    pub(super) struct Published {
        pub(super) priority: bool,
        pub(super) topics: Vec<String>,
        pub(super) bytes: Vec<u8>,
        pub(super) message: OrdermatchMessage,
    }

    pub(super) fn drain(rx: &mut mpsc::Receiver<AdexBehaviourCmd>) -> Vec<Published> {
        let mut published = Vec::new();
        while let Ok(cmd) = rx.try_recv() {
            let (topics, msg, priority) = match cmd {
                AdexBehaviourCmd::PublishMsg { topics, msg, priority }
                | AdexBehaviourCmd::PublishMsgFrom {
                    topics, msg, priority, ..
                } => (topics, msg, priority),
                _ => continue,
            };
            let (message, _sig, _pubkey) = decode_signed::<OrdermatchMessage>(&msg).unwrap();
            published.push(Published {
                priority,
                topics,
                bytes: msg,
                message,
            });
        }
        published
    }

    fn ratio(n: i64) -> BigRational { BigRational::from_integer(n.into()) }

    pub(super) fn update_price(ctx: &MmArc, order: &MakerOrder, price: i64) {
        let mut message = MakerOrderUpdated::new(order.uuid);
        message.with_new_price(ratio(price));
        maker_order_updated_p2p_notify(ctx.clone(), order.orderbook_topic(), message, None);
    }

    fn pending(ctx: &MmArc) -> usize {
        let octx = OrdermatchContext::from_ctx(ctx).unwrap();
        let pending = octx.maker_outbox.outboxes.lock().pending_of(&pubkey_of(ctx));
        pending
    }

    fn kind(message: &OrdermatchMessage) -> &'static str {
        match message {
            OrdermatchMessage::MakerOrderCreated(_) => "created",
            OrdermatchMessage::MakerOrderUpdated(_) => "updated",
            OrdermatchMessage::MakerOrderCancelled(_) => "cancelled",
            OrdermatchMessage::PubkeyKeepAlive(_) => "keep-alive",
            OrdermatchMessage::TakerRequest(_) => "taker request",
            OrdermatchMessage::MakerReserved(_) => "maker reserved",
            OrdermatchMessage::TakerConnect(_) => "taker connect",
            OrdermatchMessage::MakerConnected(_) => "maker connected",
        }
    }

    #[test]
    fn settings_are_read_like_the_existing_switches() {
        let ctx_with = |conf: Json| MmCtxBuilder::new().with_conf(conf).into_mm_arc();
        assert!(!maker_update_coalescing(&ctx_with(json!({}))));
        assert!(maker_update_coalescing(&ctx_with(
            json!({ "maker_update_coalescing": true })
        )));
        // Only a JSON `true` turns the outbox on.
        for other in [json!(false), json!("true"), json!(1), json!(null)] {
            assert!(!maker_update_coalescing(&ctx_with(
                json!({ "maker_update_coalescing": other })
            )));
        }
        assert_eq!(maker_update_rate_max(&ctx_with(json!({}))), 20.0);
        assert_eq!(
            maker_update_rate_max(&ctx_with(json!({ "maker_update_rate_max": 25 }))),
            25.0
        );
        assert_eq!(
            maker_update_rate_max(&ctx_with(json!({ "maker_update_rate_max": 0.5 }))),
            0.5
        );
        // Not a positive finite number: the default.
        for bad in [json!(0), json!(-3), json!("fast"), json!(null), json!(true)] {
            assert_eq!(
                maker_update_rate_max(&ctx_with(json!({ "maker_update_rate_max": bad }))),
                20.0
            );
        }
    }

    /// T-MP2: a cancellation leaves at once and with priority when the budget is exhausted, and discards the
    /// pending message of the order. The local state and the update are not delayed.
    #[test]
    fn t_mp2_cancellation_is_released_at_once_with_priority() {
        let (ctx, mut rx) = node(json!({ "maker_update_coalescing": true, "maker_update_rate_max": 0.1 }));
        let order = own_order("DOC", "MARTY");
        // The burst of the budget is one message: the creation uses it.
        maker_order_created_p2p_notify(ctx.clone(), &order, vec![], vec![]);
        for price in 2..6 {
            update_price(&ctx, &order, price);
        }
        let published = drain(&mut rx);
        assert_eq!(published.len(), 1, "the updates wait in the outbox");
        assert_eq!(kind(&published[0].message), "created");
        assert_eq!(pending(&ctx), 1, "four updates merged into one pending message");
        // The local order is current at once.
        flush_trie(&ctx);
        let octx = OrdermatchContext::from_ctx(&ctx).unwrap();
        assert_eq!(
            octx.orderbook.lock().find_order_by_uuid(&order.uuid).unwrap().price,
            ratio(5)
        );

        maker_order_cancelled_p2p_notify(ctx.clone(), &order);
        let published = drain(&mut rx);
        assert_eq!(published.len(), 1);
        assert_eq!(kind(&published[0].message), "cancelled");
        assert!(published[0].priority);
        assert_eq!(published[0].topics, vec![order.orderbook_topic()]);
        assert_eq!(pending(&ctx), 0, "the pending update was discarded");
    }

    /// T-MP3 and the second bullet of T-MP12: the four swap-negotiation messages are published at once, with
    /// priority, with the outbox full; a created, an updated and a keep-alive message are not priority.
    #[test]
    fn t_mp3_swap_messages_bypass_the_outbox_and_carry_priority() {
        let (ctx, mut rx) = node(json!({ "maker_update_coalescing": true, "maker_update_rate_max": 0.1 }));
        let (a, b) = (own_order("DOC", "MARTY"), own_order("DOC", "MARTY"));
        maker_order_created_p2p_notify(ctx.clone(), &a, vec![], vec![]);
        maker_order_created_p2p_notify(ctx.clone(), &b, vec![], vec![]);
        update_price(&ctx, &a, 3);
        flush_trie(&ctx);
        let published = drain(&mut rx);
        assert_eq!(published.len(), 1, "the budget is exhausted");
        assert!(!published[0].priority, "a created message is not priority");
        assert_eq!(pending(&ctx), 2);

        let (taker_uuid, maker_uuid) = (Uuid::new_v4().into(), Uuid::new_v4().into());
        let topic = a.orderbook_topic();
        let messages = vec![
            OrdermatchMessage::TakerRequest(new_protocol::TakerRequest {
                base: "DOC".into(),
                rel: "MARTY".into(),
                base_amount: ratio(1),
                rel_amount: ratio(1),
                action: TakerAction::Buy,
                uuid: taker_uuid,
                match_by: new_protocol::MatchBy::Any,
                conf_settings: OrderConfirmationsSettings::default(),
                base_protocol_info: None,
                rel_protocol_info: None,
                swap_version: SwapVersion::default(),
            }),
            OrdermatchMessage::MakerReserved(new_protocol::MakerReserved {
                base: "DOC".into(),
                rel: "MARTY".into(),
                base_amount: ratio(1),
                rel_amount: ratio(1),
                taker_order_uuid: taker_uuid,
                maker_order_uuid: maker_uuid,
                conf_settings: OrderConfirmationsSettings::default(),
                base_protocol_info: None,
                rel_protocol_info: None,
                swap_version: SwapVersion::default(),
            }),
            OrdermatchMessage::TakerConnect(new_protocol::TakerConnect {
                taker_order_uuid: taker_uuid,
                maker_order_uuid: maker_uuid,
            }),
            OrdermatchMessage::MakerConnected(new_protocol::MakerConnected {
                taker_order_uuid: taker_uuid,
                maker_order_uuid: maker_uuid,
            }),
        ];
        let expected: Vec<&str> = messages.iter().map(kind).collect();
        for message in messages {
            broadcast_ordermatch_message(&ctx, vec![topic.clone()], message, None);
        }
        let published = drain(&mut rx);
        assert_eq!(published.iter().map(|p| kind(&p.message)).collect::<Vec<_>>(), expected);
        assert!(
            published.iter().all(|p| p.priority),
            "every swap-negotiation message has priority"
        );
        assert_eq!(pending(&ctx), 2, "the outbox is untouched");

        // A keep-alive is not priority. The outbox is degraded, so the keep-alive does not wait for the pending messages.
        let octx = OrdermatchContext::from_ctx(&ctx).unwrap();
        octx.maker_outbox.outboxes.lock().signal(now_ms(), SignalKind::Close);
        block_on(broadcast_keep_alive_for_pub(&ctx, &octx, &pubkey_of(&ctx), None));
        let published = drain(&mut rx);
        assert!(published.iter().any(|p| kind(&p.message) == "keep-alive"));
        assert!(published
            .iter()
            .filter(|p| kind(&p.message) == "keep-alive")
            .all(|p| !p.priority));
    }

    /// T-MP9: below the ceiling every update is published as it is made; nothing is merged.
    #[test]
    fn t_mp9_healthy_relay_gets_every_update_at_once() {
        let (ctx, mut rx) = node(json!({ "maker_update_coalescing": true }));
        let order = own_order("DOC", "MARTY");
        maker_order_created_p2p_notify(ctx.clone(), &order, vec![], vec![]);
        for price in 2..32 {
            update_price(&ctx, &order, price);
        }
        let published = drain(&mut rx);
        assert_eq!(published.len(), 31);
        let prices: Vec<_> = published
            .iter()
            .skip(1)
            .map(|p| match &p.message {
                OrdermatchMessage::MakerOrderUpdated(u) => u.new_price().unwrap(),
                other => panic!("an update expected, got {}", kind(other)),
            })
            .collect();
        assert_eq!(prices, (2..32).map(|p| MmNumber::from(ratio(p))).collect::<Vec<_>>());
        assert!(published.iter().all(|p| !p.priority));
        assert_eq!(pending(&ctx), 0);
    }

    /// T-MP11: with no `maker_update_coalescing` setting, with `false` and with a value that is not a JSON boolean
    /// every message is published at once; with `true` the same sequence goes through the outbox and is held back by
    /// the budget.
    #[test]
    fn t_mp11_switch_publishes_every_message_at_once() {
        let run = |setting: Option<Json>| {
            let mut conf = json!({ "maker_update_rate_max": 0.1 });
            if let Some(setting) = setting {
                conf["maker_update_coalescing"] = setting;
            }
            let (ctx, mut rx) = node(conf);
            let order = own_order("DOC", "MARTY");
            maker_order_created_p2p_notify(ctx.clone(), &order, vec![], vec![]);
            for price in 2..7 {
                update_price(&ctx, &order, price);
            }
            (drain(&mut rx).len(), pending(&ctx))
        };
        for setting in [
            None,
            Some(json!(false)),
            Some(json!("true")),
            Some(json!(1)),
            Some(json!(null)),
        ] {
            assert_eq!(run(setting.clone()), (6, 0), "setting {setting:?}");
        }
        assert_eq!(run(Some(json!(true))), (1, 1));
    }

    /// T-MP12 (propagation): a relay propagates a cancellation with priority and a created message without.
    #[test]
    fn t_mp12_a_relay_propagates_a_cancellation_with_priority() {
        let (maker, _rx) = node(json!({}));
        let (relay, _relay_rx) = node(json!({}));
        let order = own_order("DOC", "MARTY");
        let secret = maker.secp256k1_key_pair().private_bytes();
        let created = new_protocol::MakerOrderCreated {
            uuid: order.uuid.into(),
            base: "DOC".into(),
            rel: "MARTY".into(),
            price: ratio(1),
            max_volume: ratio(10),
            min_volume: ratio(1),
            created_at: 1,
            conf_settings: OrderConfirmationsSettings::default(),
            timestamp: 1,
            pair_trie_root: H64::default(),
            base_protocol_info: vec![],
            rel_protocol_info: vec![],
        };
        let created = encode_and_sign(&OrdermatchMessage::MakerOrderCreated(created), &secret).unwrap();
        let cancelled = new_protocol::MakerOrderCancelled {
            uuid: order.uuid.into(),
            timestamp: 1,
            pair_trie_root: H64::default(),
        };
        let cancelled = encode_and_sign(&OrdermatchMessage::MakerOrderCancelled(cancelled), &secret).unwrap();

        let (propagate, priority) = block_on(process_msg_with_priority(
            relay.clone(),
            vec![],
            "peer".into(),
            &created,
            true,
        ));
        assert!(propagate && !priority);
        let (propagate, priority) = block_on(process_msg_with_priority(
            relay.clone(),
            vec![],
            "peer".into(),
            &cancelled,
            true,
        ));
        assert!(propagate && priority);
    }

    /// T-MP12 (relay) and T-F3d (CRD ch.32 R-F3b, D11): after processing a received message the network layer of a
    /// relay issues a propagate command with priority for a cancellation, a propagate command without priority for
    /// a message outside the orderbook topics, and for any other orderbook message only a command that validates
    /// the message in the cache (no second copy).
    #[test]
    fn t_mp12_the_propagate_command_of_a_relay_carries_the_priority() {
        use crate::mm2::lp_network::process_p2p_message;
        use libp2p::gossipsub::TopicHash;

        let (relay, mut rx) = node(json!({}));
        let order = own_order("DOC", "MARTY");
        let secret = relay.secp256k1_key_pair().private_bytes();
        let topic = order.orderbook_topic();
        let created = new_protocol::MakerOrderCreated {
            uuid: order.uuid.into(),
            base: "DOC".into(),
            rel: "MARTY".into(),
            price: ratio(1),
            max_volume: ratio(10),
            min_volume: ratio(1),
            created_at: 1,
            conf_settings: OrderConfirmationsSettings::default(),
            timestamp: 1,
            pair_trie_root: H64::default(),
            base_protocol_info: vec![],
            rel_protocol_info: vec![],
        };
        let cancelled = new_protocol::MakerOrderCancelled {
            uuid: order.uuid.into(),
            timestamp: 1,
            pair_trie_root: H64::default(),
        };
        let received = |data: Vec<u8>| mm2_p2p::GossipsubMessage {
            source: None,
            data,
            sequence_number: Some(1),
            topic: TopicHash::from_raw(topic.clone()),
        };
        let propagated = |rx: &mut mpsc::Receiver<AdexBehaviourCmd>| match rx.try_recv() {
            Ok(AdexBehaviourCmd::PropagateMessage { priority, .. }) => priority,
            other => panic!("a propagate command expected, got {:?}", other),
        };
        let validated = |rx: &mut mpsc::Receiver<AdexBehaviourCmd>, id: u8| match rx.try_recv() {
            Ok(AdexBehaviourCmd::ValidateMessage { message_id }) => {
                assert_eq!(message_id, mm2_p2p::MessageId::from(vec![id]))
            },
            other => panic!("a validate command expected, got {:?}", other),
        };

        let data = encode_and_sign(&OrdermatchMessage::MakerOrderCreated(created), &secret).unwrap();
        block_on(process_p2p_message(
            relay.clone(),
            PeerId::random(),
            mm2_p2p::MessageId::from(vec![1]),
            received(data),
            true,
        ));
        validated(&mut rx, 1);
        assert!(rx.try_recv().is_err(), "a created message has no second copy");

        let data = encode_and_sign(&OrdermatchMessage::MakerOrderCancelled(cancelled), &secret).unwrap();
        block_on(process_p2p_message(
            relay.clone(),
            PeerId::random(),
            mm2_p2p::MessageId::from(vec![2]),
            received(data),
            true,
        ));
        assert!(propagated(&mut rx), "a cancellation is propagated with priority");
        assert!(rx.try_recv().is_err(), "a cancellation sends one command");

        // A message outside the orderbook topics keeps the second copy, without priority.
        block_on(process_p2p_message(
            relay.clone(),
            PeerId::random(),
            mm2_p2p::MessageId::from(vec![3]),
            mm2_p2p::GossipsubMessage {
                source: None,
                data: vec![1, 2, 3],
                sequence_number: Some(1),
                topic: TopicHash::from_raw("swap/not-a-uuid"),
            },
            true,
        ));
        assert!(!propagated(&mut rx), "a swap message is propagated without priority");
        assert!(rx.try_recv().is_err(), "a swap message sends one command");
    }

    /// A receiver applies the given published messages in order and collects its synchronization requests.
    fn apply_to_a_receiver(maker: &MmArc, messages: Vec<&Published>) -> Vec<Option<OrdermatchRequest>> {
        let _ = maker;
        let (receiver, _rx) = node(json!({}));
        let recv_octx = OrdermatchContext::from_ctx(&receiver).unwrap();
        let mut requests = Vec::new();
        for p in messages {
            let (message, _sig, signer) = decode_signed::<OrdermatchMessage>(&p.bytes).unwrap();
            match message {
                OrdermatchMessage::PubkeyKeepAlive(keep_alive) => {
                    let request = block_on(keep_alive_sync_request(
                        &recv_octx,
                        &signer.to_hex(),
                        keep_alive,
                        true,
                        SyncRequestRoot::Held,
                        TRIE_CATCH_UP_BOUND_MS,
                        0,
                    ));
                    requests.push(request.map(|(request, _in_flight, _advertised)| request));
                },
                _ => {
                    block_on(process_msg(receiver.clone(), vec![], "peer".into(), &p.bytes, true));
                },
            }
        }
        requests
    }

    /// Exhausts the burst of `ctx` with orders on the pair `WARM:UP`, which then are published.
    fn exhaust_the_burst(ctx: &MmArc, rx: &mut mpsc::Receiver<AdexBehaviourCmd>, burst: usize) -> Vec<Published> {
        for _ in 0..burst {
            maker_order_created_p2p_notify(ctx.clone(), &own_order("WARM", "UP"), vec![], vec![]);
        }
        assert_eq!(pending(ctx), 0);
        let published = drain(rx);
        assert_eq!(published.len(), burst);
        published
    }

    /// T-MP6 (first part): with a budget that releases the pending messages within the wait, a pair's pending
    /// messages are published before its keep-alive, and a receiver that applies them in order holds the advertised
    /// root and sends no synchronization request.
    #[test]
    fn t_mp6_the_changes_of_a_pair_precede_its_keep_alive_within_the_wait() {
        // 2 messages per second, a burst of 20.
        let (maker, mut rx) = node(json!({ "maker_update_coalescing": true, "maker_update_rate_max": 2.0 }));
        let warm_up = exhaust_the_burst(&maker, &mut rx, 20);
        let orders = [
            own_order("DOC", "MARTY"),
            own_order("DOC", "MARTY"),
            own_order("C1", "C2"),
            own_order("C1", "C2"),
        ];
        for order in &orders {
            maker_order_created_p2p_notify(maker.clone(), order, vec![], vec![]);
        }
        update_price(&maker, &orders[1], 7);
        flush_trie(&maker);
        assert_eq!(pending(&maker), 4, "the budget is exhausted, all four wait");
        spawn(maker_outbox_loop(maker.clone()));
        let octx = OrdermatchContext::from_ctx(&maker).unwrap();
        let started = std::time::Instant::now();
        block_on(broadcast_keep_alive_for_pub(&maker, &octx, &pubkey_of(&maker), None));
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the pending messages left within the wait: {:?}",
            started.elapsed()
        );
        let published = drain(&mut rx);
        assert_eq!(pending(&maker), 0);

        // Per pair: both orders of the pair precede its keep-alive, and no change of the pair follows it.
        let mut keep_alives = 0;
        for (i, p) in published.iter().enumerate() {
            if kind(&p.message) != "keep-alive" {
                continue;
            }
            keep_alives += 1;
            if p.topics[0].contains("WARM") {
                continue;
            }
            let before_for_pair = published[..i].iter().filter(|q| q.topics == p.topics).count();
            assert_eq!(before_for_pair, 2, "both orders of the pair precede its keep-alive");
            assert!(
                published[i + 1..]
                    .iter()
                    .all(|q| q.topics != p.topics || kind(&q.message) == "keep-alive"),
                "no change of the pair follows its keep-alive"
            );
        }
        assert_eq!(keep_alives, 3, "one keep-alive per pair");

        // The receiver holds the advertised root of every pair: it knows the warm-up orders from earlier, so they
        // are published to it first.
        let all = apply_to_a_receiver(&maker, warm_up.iter().chain(published.iter()).collect());
        assert_eq!(all.len(), 3);
        assert!(
            all.iter().all(|r| r.is_none()),
            "the receiver holds the advertised roots: {:?}",
            all
        );
    }

    /// T-MP6 / R-MP3 item 3: a message the outbox released but has not yet handed to the swarm counts as pending, so the
    /// bounded wait does not end, and the keep-alive does not overtake it.
    #[test]
    fn t_mp6_a_released_message_still_in_flight_holds_the_keep_alive_back() {
        let (maker, _rx) = node(json!({ "maker_update_coalescing": true }));
        let octx = OrdermatchContext::from_ctx(&maker).unwrap();
        let topics = vec![own_order("DOC", "MARTY").orderbook_topic()];
        octx.maker_outbox
            .in_flight
            .store(1, std::sync::atomic::Ordering::SeqCst);
        let release = {
            let octx = octx.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(500));
                octx.maker_outbox
                    .in_flight
                    .store(0, std::sync::atomic::Ordering::SeqCst);
            })
        };
        let started = std::time::Instant::now();
        block_on(wait_for_pairs_to_drain(&octx, &pubkey_of(&maker), &topics, 10_000));
        let waited = started.elapsed();
        release.join().unwrap();
        assert!(
            waited >= Duration::from_millis(450) && waited < Duration::from_secs(3),
            "the wait lasted {waited:?}"
        );
    }

    /// T-MP6 (second part): with more pending than the budget releases in 10 s, the keep-alives leave 10 s after the
    /// tick, give or take a second, and no message leaves beyond the budget.
    #[test]
    fn t_mp6_with_more_pending_than_the_budget_releases_the_keep_alives_leave_after_ten_seconds() {
        // 0.5 messages per second, a burst of 5.
        let (maker, mut rx) = node(json!({ "maker_update_coalescing": true, "maker_update_rate_max": 0.5 }));
        let _ = exhaust_the_burst(&maker, &mut rx, 5);
        for _ in 0..20 {
            maker_order_created_p2p_notify(maker.clone(), &own_order("DOC", "MARTY"), vec![], vec![]);
        }
        flush_trie(&maker);
        assert_eq!(pending(&maker), 20);
        // The loop reports its exit, so the test can stop it and know that no step is half done (see below).
        let (loop_done_tx, loop_done_rx) = futures::channel::oneshot::channel::<()>();
        let outbox_loop_ctx = maker.clone();
        spawn(async move {
            maker_outbox_loop(outbox_loop_ctx).await;
            let _ = loop_done_tx.send(());
        });
        let octx = OrdermatchContext::from_ctx(&maker).unwrap();
        let started = std::time::Instant::now();
        block_on(broadcast_keep_alive_for_pub(&maker, &octx, &pubkey_of(&maker), None));
        let waited = started.elapsed();
        assert!(
            waited >= Duration::from_secs(9) && waited <= Duration::from_secs(11),
            "the keep-alives left after {:?}",
            waited
        );
        // A release leaves the pending set under the outbox lock, and reaches the channel a moment later, in the same
        // pump step. The running loop could thus release between the drain and the read of `pending`. So the loop is
        // stopped first, and its exit awaited: a pump step is synchronous, so no step is half done once the loop has
        // returned, and the two reads below see the same, final state.
        maker.stop().unwrap();
        block_on(loop_done_rx).expect("the outbox loop returned");
        let published = drain(&mut rx);
        let released = published.iter().filter(|p| kind(&p.message) == "created").count();
        // 0.5 per second for 10 s, plus the tokens that had refilled and the pump's step.
        assert!(
            (4..=7).contains(&released),
            "{released} messages left during the wait, the budget allows about 5"
        );
        assert_eq!(pending(&maker), 20 - released, "the rest stays pending");
        assert!(published.iter().any(|p| kind(&p.message) == "keep-alive"));
    }

    /// T-MP6 (third part): in degraded mode the keep-alives leave at the tick.
    #[test]
    fn t_mp6_in_degraded_mode_the_keep_alives_leave_at_the_tick() {
        let (maker, mut rx) = node(json!({ "maker_update_coalescing": true, "maker_update_rate_max": 0.5 }));
        let _ = exhaust_the_burst(&maker, &mut rx, 5);
        for _ in 0..4 {
            maker_order_created_p2p_notify(maker.clone(), &own_order("DOC", "MARTY"), vec![], vec![]);
        }
        flush_trie(&maker);
        let octx = OrdermatchContext::from_ctx(&maker).unwrap();
        octx.maker_outbox.outboxes.lock().signal(now_ms(), SignalKind::Queue);
        let started = std::time::Instant::now();
        block_on(broadcast_keep_alive_for_pub(&maker, &octx, &pubkey_of(&maker), None));
        assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
        let published = drain(&mut rx);
        assert_eq!(published.iter().filter(|p| kind(&p.message) == "keep-alive").count(), 2);
        assert_eq!(published.iter().filter(|p| kind(&p.message) == "created").count(), 0);
        assert_eq!(pending(&maker), 4, "nothing was released");
    }

    /// T-MP5 (close part): the close of the last connection to a relay is a signal only with a pending message or a
    /// release within 30 seconds; the outbox then drops to the floor in degraded mode.
    #[test]
    fn t_mp5_a_relay_close_is_a_signal_only_with_recent_maker_traffic() {
        let (ctx, mut rx) = node(json!({ "maker_update_coalescing": true, "maker_update_rate_max": 0.5 }));
        let octx = OrdermatchContext::from_ctx(&ctx).unwrap();
        // No traffic: no signal.
        relay_connection_closed(&ctx);
        assert_eq!(
            octx.maker_outbox
                .close_signals
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        assert!(!octx.maker_outbox.outboxes.lock().is_degraded(&pubkey_of(&ctx)));

        // A released message within the last 30 seconds: a signal.
        maker_order_created_p2p_notify(ctx.clone(), &own_order("DOC", "MARTY"), vec![], vec![]);
        drain(&mut rx);
        relay_connection_closed(&ctx);
        assert_eq!(
            octx.maker_outbox
                .close_signals
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        assert!(octx.maker_outbox.outboxes.lock().is_degraded(&pubkey_of(&ctx)));
        assert_eq!(gauge_after_pump(&ctx, "p2p.outbox.degraded"), Some(1));
        assert_eq!(gauge_after_pump(&ctx, "p2p.outbox.signals.close.count"), Some(1));
    }

    /// T-MP7: after a relay joined the relay mesh one keep-alive per pair leaves within 5 s, however many relays
    /// joined in the same moment; a later join starts another round.
    #[test]
    fn t_mp7_a_relay_joining_the_mesh_triggers_one_keep_alive_per_pair() {
        let (ctx, mut rx) = node(json!({ "maker_update_coalescing": true }));
        for (base, rel) in [("DOC", "MARTY"), ("C1", "C2"), ("C1", "C2")] {
            maker_order_created_p2p_notify(ctx.clone(), &own_order(base, rel), vec![], vec![]);
        }
        flush_trie(&ctx);
        drain(&mut rx);

        let started = std::time::Instant::now();
        schedule_reconnect_keep_alive_round(ctx.clone());
        schedule_reconnect_keep_alive_round(ctx.clone());
        schedule_reconnect_keep_alive_round(ctx.clone());
        let mut round = Vec::new();
        while started.elapsed() < Duration::from_secs(5) && round.len() < 2 {
            block_on(Timer::sleep(0.05));
            round.extend(drain(&mut rx));
        }
        assert!(started.elapsed() < Duration::from_secs(5), "the round took too long");
        // Give a duplicated round the chance to show up.
        block_on(Timer::sleep(1.5));
        round.extend(drain(&mut rx));
        assert_eq!(
            round.len(),
            2,
            "one keep-alive per pair, joins in the same moment share the round"
        );
        assert!(round.iter().all(|p| kind(&p.message) == "keep-alive"));
        assert_ne!(round[0].topics, round[1].topics);

        schedule_reconnect_keep_alive_round(ctx.clone());
        block_on(Timer::sleep(2.0));
        assert_eq!(drain(&mut rx).len(), 2, "a later join starts another round");
    }

    fn gauge_after_pump(ctx: &MmArc, key: &str) -> Option<i64> {
        pump_maker_outbox(ctx);
        gauge(ctx, key)
    }

    fn gauge(ctx: &MmArc, key: &str) -> Option<i64> {
        let json = ctx.metrics.collect_json().unwrap();
        json["metrics"]
            .as_array()?
            .iter()
            .find(|m| m["key"] == key)
            .and_then(|m| m["value"].as_i64())
    }

    /// T-MP10 (outbox part): the outbox gauges show the pending messages, the rate, the queue signal, the degraded
    /// mode and the signals, and carry no label (no peer identity, no content).
    #[test]
    fn t_mp10_outbox_gauges_follow_the_state() {
        let (ctx, _rx) = node(json!({ "maker_update_coalescing": true, "maker_update_rate_max": 0.1 }));
        let order = own_order("DOC", "MARTY");
        maker_order_created_p2p_notify(ctx.clone(), &order, vec![], vec![]);
        for price in 2..5 {
            update_price(&ctx, &order, price);
        }
        pump_maker_outbox(&ctx);
        assert_eq!(gauge(&ctx, "p2p.outbox.pending"), Some(1));
        // The rate is reported in thousandths of a message per second.
        assert_eq!(gauge(&ctx, "p2p.outbox.rate"), Some(100));
        assert_eq!(gauge(&ctx, "p2p.outbox.pressure"), Some(0));
        assert_eq!(gauge(&ctx, "p2p.outbox.degraded"), Some(0));
        assert_eq!(gauge(&ctx, "p2p.outbox.signals.queue.count"), Some(0));
        assert_eq!(gauge(&ctx, "p2p.outbox.signals.close.count"), Some(0));

        // The queue signal is raised (the swarm side counts it once).
        let signal = P2PContext::fetch_from_mm_arc(&ctx).relay_queue_signal.clone();
        signal.raised.store(true, std::sync::atomic::Ordering::Relaxed);
        signal.raised_count.store(1, std::sync::atomic::Ordering::Relaxed);
        pump_maker_outbox(&ctx);
        assert_eq!(gauge(&ctx, "p2p.outbox.pressure"), Some(1));
        assert_eq!(gauge(&ctx, "p2p.outbox.degraded"), Some(1));
        assert_eq!(gauge(&ctx, "p2p.outbox.signals.queue.count"), Some(1));
        // A close of the relay while a message is pending.
        relay_connection_closed(&ctx);
        pump_maker_outbox(&ctx);
        assert_eq!(gauge(&ctx, "p2p.outbox.signals.close.count"), Some(1));
        let json = ctx.metrics.collect_json().unwrap();
        for metric in json["metrics"].as_array().unwrap() {
            if metric["key"].as_str().is_some_and(|k| k.starts_with("p2p.outbox")) {
                assert!(metric["labels"].as_object().unwrap().is_empty(), "{metric}");
            }
        }
    }
}

/// CRD ch.59 T4 (R59.5.1, R59.5.2): a failed negotiation penalises the maker, the record names the swap
/// and the outcome, and a later `MakerReserved` from that maker is dropped at the dispatch boundary,
/// before it can reach the selection of §59.3. The other two penalising outcomes still penalise, and
/// an outcome outside R59.5.2 does not.
mod negotiation_failure_penalty {
    use super::*;
    use crate::mm2::lp_swap::{list_banned_pubkeys_rpc, penalise_maker_if_due, TakerSwapEvent};

    fn taker_node() -> MmArc {
        let ctx = MmCtxBuilder::new().into_mm_arc();
        ctx.secp256k1_key_pair
            .pin(key_pair_from_seed("negotiation-failure-penalty-taker").unwrap())
            .unwrap();
        ctx
    }

    /// The maker's signing secret and its key in the form the penalty map stores (without the prefix).
    fn maker(passphrase: &str) -> ([u8; 32], chain::hash::H256) {
        let (pubkey, secret) = pubkey_and_secret_for_test(passphrase);
        let bytes = hex::decode(pubkey).unwrap();
        (secret, chain::hash::H256::from(&bytes[1..]))
    }

    fn maker_reserved(secret: &[u8; 32], taker_order_uuid: Uuid) -> Vec<u8> {
        let msg = new_protocol::MakerReserved {
            base: "DOC".into(),
            rel: "MARTY".into(),
            base_amount: BigRational::from_integer(1.into()),
            rel_amount: BigRational::from_integer(1.into()),
            taker_order_uuid: taker_order_uuid.into(),
            maker_order_uuid: Uuid::new_v4().into(),
            conf_settings: OrderConfirmationsSettings::default(),
            base_protocol_info: None,
            rel_protocol_info: None,
            swap_version: SwapVersion::default(),
        };
        encode_and_sign(&new_protocol::OrdermatchMessage::MakerReserved(msg), secret).unwrap()
    }

    fn banned(ctx: &MmArc, maker: chain::hash::H256) -> bool { is_pubkey_banned(ctx, &maker.into()) }

    fn listed(ctx: &MmArc) -> Json {
        let response = block_on(list_banned_pubkeys_rpc(ctx.clone())).unwrap();
        json::from_slice::<Json>(response.body()).unwrap()["result"].clone()
    }

    #[test]
    fn t4_failed_negotiation_penalises_the_maker_and_its_reservation_is_dropped() {
        let ctx = taker_node();
        let (secret, maker_key) = maker("negotiation-failure-penalty-maker");
        let swap_uuid = Uuid::new_v4();

        let event = TakerSwapEvent::NegotiateFailed("maker sent negotiated = false".into());
        penalise_maker_if_due(&ctx, maker_key, &swap_uuid, &event);

        // The record exists and names the swap and the outcome (R59.5.2).
        assert!(banned(&ctx, maker_key));
        let listed = listed(&ctx).to_string();
        assert!(listed.contains(&swap_uuid.to_string()), "{}", listed);
        assert!(listed.contains("NegotiateFailed"), "{}", listed);

        // A reservation from the penalised maker for a fresh taker order is dropped (R59.5.1) ...
        let taker_order_uuid = Uuid::new_v4();
        let reserved = maker_reserved(&secret, taker_order_uuid);
        assert!(!block_on(process_msg(
            ctx.clone(),
            vec![],
            "peer".to_owned(),
            &reserved,
            false
        )));
        let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
        assert!(block_on(ordermatch_ctx.pending_maker_reserved.lock()).is_empty());

        // ... while the same message from a maker that is not penalised is accepted.
        let (other_secret, other_key) = maker("negotiation-failure-penalty-other-maker");
        assert!(!banned(&ctx, other_key));
        let reserved = maker_reserved(&other_secret, taker_order_uuid);
        assert!(block_on(process_msg(
            ctx.clone(),
            vec![],
            "peer".to_owned(),
            &reserved,
            false
        )));
    }

    #[test]
    fn t4_the_other_two_outcomes_still_penalise() {
        let ctx = taker_node();
        let (_, validate_key) = maker("negotiation-failure-penalty-validate");
        let (_, spend_key) = maker("negotiation-failure-penalty-spend");

        let event = TakerSwapEvent::MakerPaymentValidateFailed("invalid payment".into());
        penalise_maker_if_due(&ctx, validate_key, &Uuid::new_v4(), &event);
        let event = TakerSwapEvent::TakerPaymentWaitForSpendFailed("never spent".into());
        penalise_maker_if_due(&ctx, spend_key, &Uuid::new_v4(), &event);

        assert!(banned(&ctx, validate_key));
        assert!(banned(&ctx, spend_key));
    }

    #[test]
    fn t4_an_outcome_outside_the_rule_does_not_penalise() {
        let ctx = taker_node();
        let (_, maker_key) = maker("negotiation-failure-penalty-not-banned");

        let event = TakerSwapEvent::MakerPaymentWaitConfirmFailed("slow chain".into());
        penalise_maker_if_due(&ctx, maker_key, &Uuid::new_v4(), &event);

        assert!(!banned(&ctx, maker_key));
        assert_eq!(listed(&ctx), json!({}));
    }
}

/// CRD ch.59 §59.4A, T14: capability words in coin protocol information (carriage, trie, decisions).
mod t14_capability_word_tests {
    use super::*;
    use crate::mm2::lp_swap::{active_swaps, list_banned_pubkeys_rpc};
    use coins::coin_protocol_capability::{append_capability_word, declares_capability, CAPABILITY_SIGHASH_SUFFIX,
                                          CAPABILITY_TX_V6};
    use coins::{CoinsContext, MarketCoinOps, MmCoinEnum};
    use common::mm_number::BigRational;
    use std::sync::Arc as StdArc;

    /// The first value of a UTXO coin: its encoded address format.
    fn first_values() -> Vec<Vec<u8>> {
        [
            AddressFormat::Standard,
            AddressFormat::Segwit,
            AddressFormat::CashAddress {
                network: "bitcoincash".to_owned(),
                pub_addr_prefix: 0,
                p2sh_addr_prefix: 5,
            },
        ]
        .iter()
        .map(|format| rmp_serde::to_vec(format).unwrap())
        .collect()
    }

    fn with_word(first: &[u8]) -> Vec<u8> { append_capability_word(first.to_vec(), &[CAPABILITY_TX_V6]) }

    // --- (c), orderbook-reader half: the address format the order book reports (R59.4A.3) ---

    #[test]
    fn t14_c_orderbook_address_format_is_the_same_with_the_word() {
        for first in first_values() {
            let without = addr_format_from_protocol_info(&first);
            let with = addr_format_from_protocol_info(&with_word(&first));
            assert_eq!(without, with);
        }
        // The three encodings really differ, so the equality above is not vacuous.
        let formats: Vec<_> = first_values()
            .iter()
            .map(|f| addr_format_from_protocol_info(f))
            .collect();
        assert_eq!(formats[0], AddressFormat::Standard);
        assert_eq!(formats[1], AddressFormat::Segwit);
        assert!(matches!(formats[2], AddressFormat::CashAddress { .. }));
    }

    // --- (e) carriage ---

    fn maker_order_created(base_info: Vec<u8>, rel_info: Vec<u8>) -> new_protocol::MakerOrderCreated {
        new_protocol::MakerOrderCreated {
            uuid: Uuid::new_v4().into(),
            base: "BASE".into(),
            rel: "REL".into(),
            price: BigRational::from_integer(1.into()),
            max_volume: BigRational::from_integer(2.into()),
            min_volume: BigRational::from_integer(1.into()),
            created_at: 1,
            conf_settings: OrderConfirmationsSettings::default(),
            timestamp: 1,
            pair_trie_root: H64::default(),
            base_protocol_info: base_info,
            rel_protocol_info: rel_info,
        }
    }

    fn wire_taker_request(base_info: Option<Vec<u8>>, rel_info: Option<Vec<u8>>) -> new_protocol::TakerRequest {
        new_protocol::TakerRequest {
            base: "BASE".into(),
            rel: "REL".into(),
            base_amount: BigRational::from_integer(1.into()),
            rel_amount: BigRational::from_integer(1.into()),
            action: TakerAction::Buy,
            uuid: Uuid::new_v4().into(),
            match_by: new_protocol::MatchBy::Any,
            conf_settings: OrderConfirmationsSettings::default(),
            base_protocol_info: base_info,
            rel_protocol_info: rel_info,
            swap_version: SwapVersion::default(),
        }
    }

    fn wire_maker_reserved(base_info: Option<Vec<u8>>, rel_info: Option<Vec<u8>>) -> new_protocol::MakerReserved {
        new_protocol::MakerReserved {
            base: "BASE".into(),
            rel: "REL".into(),
            base_amount: BigRational::from_integer(1.into()),
            rel_amount: BigRational::from_integer(1.into()),
            taker_order_uuid: Uuid::new_v4().into(),
            maker_order_uuid: Uuid::new_v4().into(),
            conf_settings: OrderConfirmationsSettings::default(),
            base_protocol_info: base_info,
            rel_protocol_info: rel_info,
            swap_version: SwapVersion::default(),
        }
    }

    /// Signs, encodes, decodes and verifies a message exactly as the order-match layer does.
    fn through_the_wire(msg: new_protocol::OrdermatchMessage) -> new_protocol::OrdermatchMessage {
        let (_, secret) = pubkey_and_secret_for_test("t14-carriage");
        let encoded = encode_and_sign(&msg, &secret).unwrap();
        decode_signed::<new_protocol::OrdermatchMessage>(&encoded).unwrap().0
    }

    #[test]
    fn t14_e_maker_order_created_carries_the_bytes_unchanged() {
        let base = with_word(&first_values()[0]);
        let rel = first_values()[1].clone();
        let msg = maker_order_created(base.clone(), rel.clone());
        match through_the_wire(new_protocol::OrdermatchMessage::MakerOrderCreated(msg.clone())) {
            new_protocol::OrdermatchMessage::MakerOrderCreated(decoded) => {
                assert_eq!(decoded, msg);
                assert_eq!(decoded.base_protocol_info, base);
                assert_eq!(decoded.rel_protocol_info, rel);
                // And into the order book item the node keeps.
                let item: OrderbookItem = (decoded, "pubkey".to_owned()).into();
                assert_eq!(item.base_protocol_info, base);
                assert_eq!(item.rel_protocol_info, rel);
            },
            other => panic!("unexpected message {other:?}"),
        }
    }

    #[test]
    fn t14_e_taker_request_carries_the_bytes_unchanged() {
        let base = with_word(&first_values()[1]);
        let rel = with_word(&first_values()[2]);
        let msg = wire_taker_request(Some(base.clone()), Some(rel.clone()));
        match through_the_wire(new_protocol::OrdermatchMessage::TakerRequest(msg)) {
            new_protocol::OrdermatchMessage::TakerRequest(decoded) => {
                assert_eq!(decoded.base_protocol_info.as_ref(), Some(&base));
                assert_eq!(decoded.rel_protocol_info.as_ref(), Some(&rel));
                let request = TakerRequest::from_new_proto_and_pubkey(decoded, H256Json::default());
                assert_eq!(request.base_protocol_info.as_ref(), Some(&base));
                assert_eq!(request.rel_protocol_info.as_ref(), Some(&rel));
            },
            other => panic!("unexpected message {other:?}"),
        }
    }

    #[test]
    fn t14_e_maker_reserved_carries_the_bytes_unchanged() {
        let base = with_word(&first_values()[2]);
        let rel = first_values()[0].clone();
        let msg = wire_maker_reserved(Some(base.clone()), Some(rel.clone()));
        match through_the_wire(new_protocol::OrdermatchMessage::MakerReserved(msg)) {
            new_protocol::OrdermatchMessage::MakerReserved(decoded) => {
                assert_eq!(decoded.base_protocol_info.as_ref(), Some(&base));
                assert_eq!(decoded.rel_protocol_info.as_ref(), Some(&rel));
                let reserved = MakerReserved::from_new_proto_and_pubkey(decoded, H256Json::default());
                assert_eq!(reserved.base_protocol_info.as_ref(), Some(&base));
                assert_eq!(reserved.rel_protocol_info.as_ref(), Some(&rel));
                // And back out: the maker's own reservation puts the same bytes into the message.
                let mut outgoing = reserved;
                outgoing.conf_settings = Some(OrderConfirmationsSettings::default());
                match new_protocol::OrdermatchMessage::from(outgoing) {
                    new_protocol::OrdermatchMessage::MakerReserved(out) => {
                        assert_eq!(out.base_protocol_info.as_ref(), Some(&base));
                        assert_eq!(out.rel_protocol_info.as_ref(), Some(&rel));
                    },
                    other => panic!("unexpected message {other:?}"),
                }
            },
            other => panic!("unexpected message {other:?}"),
        }
    }

    #[test]
    fn t14_e_synchronization_answer_side_channel_carries_the_bytes_unchanged() {
        let uuid = Uuid::new_v4();
        let info = BaseRelProtocolInfo {
            base: with_word(&first_values()[0]),
            rel: with_word(&first_values()[1]),
        };
        let answer = SyncPubkeyOrderbookStateRes {
            last_signed_pubkey_payload: vec![1, 2, 3],
            pair_orders_diff: HashMap::new(),
            protocol_infos: HashMap::from_iter(iter::once((uuid, info.clone()))),
            conf_infos: HashMap::new(),
        };
        let encoded = encode_message(&answer).unwrap();
        let decoded: SyncPubkeyOrderbookStateRes = decode_message(&encoded).unwrap();
        assert_eq!(decoded.protocol_infos.get(&uuid), Some(&info));

        let full = GetOrderbookRes {
            pubkey_orders: HashMap::new(),
            protocol_infos: HashMap::from_iter(iter::once((uuid, info.clone()))),
            conf_infos: HashMap::new(),
        };
        let decoded: GetOrderbookRes = decode_message(&encode_message(&full).unwrap()).unwrap();
        assert_eq!(decoded.protocol_infos.get(&uuid), Some(&info));
    }

    #[test]
    fn t14_e_the_trie_root_and_the_stored_leaf_are_the_same_with_and_without_the_word() {
        let (ctx_a, pubkey, secret) = make_ctx_for_tests();
        let (ctx_b, ..) = make_ctx_for_tests();
        let mut order = make_random_orders(pubkey.clone(), &secret, "C1".into(), "C2".into(), 1).remove(0);
        order.base_protocol_info = first_values()[0].clone();
        order.rel_protocol_info = first_values()[1].clone();
        let mut order_with_word = order.clone();
        order_with_word.base_protocol_info = with_word(&order.base_protocol_info);
        order_with_word.rel_protocol_info = with_word(&order.rel_protocol_info);
        assert_ne!(order.base_protocol_info, order_with_word.base_protocol_info);

        insert_or_update_order(&ctx_a, order.clone());
        insert_or_update_order(&ctx_b, order_with_word.clone());
        flush_trie(&ctx_a);
        flush_trie(&ctx_b);

        let pair = alb_ordered_pair("C1", "C2");
        assert_eq!(
            pair_trie_root_by_pub(&ctx_a, &pubkey, &pair),
            pair_trie_root_by_pub(&ctx_b, &pubkey, &pair)
        );
        let octx_a = OrdermatchContext::from_ctx(&ctx_a).unwrap();
        let octx_b = OrdermatchContext::from_ctx(&ctx_b).unwrap();
        let leaf_a = trie_value_of(&octx_a, &pubkey, &pair, &order.uuid);
        assert!(leaf_a.is_some());
        assert_eq!(leaf_a, trie_value_of(&octx_b, &pubkey, &pair, &order.uuid));
    }

    // --- (f) decisions at the call sites ---

    /// A test coin that requires `tx_v6` in the counterparty's protocol information. Every consulted
    /// coin ticker is recorded, in order.
    fn require_capability() -> StdArc<Mutex<Vec<String>>> {
        let consulted = StdArc::new(Mutex::new(Vec::new()));
        let log = consulted.clone();
        TestCoin::is_coin_protocol_supported.mock_safe(move |coin, info| {
            log.lock().unwrap().push(coin.ticker().to_owned());
            MockResult::Return(declares_capability(info, CAPABILITY_TX_V6))
        });
        consulted
    }

    struct Node {
        ctx: MmArc,
        rx: mpsc::Receiver<AdexBehaviourCmd>,
        dbdir: std::path::PathBuf,
    }

    impl Node {
        fn new() -> Node {
            let dbdir = std::env::temp_dir().join(format!("kdf-t14-capability-{}", Uuid::new_v4()));
            let conf = json!({ "dbdir": dbdir.to_string_lossy(), "pre_trade_spend_check": false });
            let ctx = MmCtxBuilder::new().with_conf(conf).into_mm_arc();
            ctx.secp256k1_key_pair
                .pin(key_pair_from_seed("t14-capability-node").unwrap())
                .unwrap();
            let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();
            for ticker in ["BASE", "REL"] {
                block_on(coins_ctx.add_coin(MmCoinEnum::Test(TestCoin::new(ticker)))).unwrap();
            }
            std::fs::create_dir_all(my_taker_orders_dir(&ctx)).unwrap();
            std::fs::create_dir_all(my_maker_orders_dir(&ctx)).unwrap();
            let (tx, rx) = mpsc::channel(100);
            P2PContext::new(tx).store_to_mm_arc(&ctx);
            Node { ctx, rx, dbdir }
        }

        /// The ordermatch messages published since the last call.
        fn published(&mut self) -> Vec<new_protocol::OrdermatchMessage> {
            let mut out = Vec::new();
            while let Ok(cmd) = self.rx.try_recv() {
                if let AdexBehaviourCmd::PublishMsg { msg, .. } | AdexBehaviourCmd::PublishMsgFrom { msg, .. } = cmd {
                    if let Ok((message, ..)) = decode_signed::<new_protocol::OrdermatchMessage>(&msg) {
                        out.push(message);
                    }
                }
            }
            out
        }

        /// No swap is running and no pubkey carries a penalty record (R59.4A.5).
        fn assert_no_swap_and_no_penalty(&self) {
            assert!(active_swaps(&self.ctx).unwrap().is_empty());
            let response = block_on(list_banned_pubkeys_rpc(self.ctx.clone())).unwrap();
            let listed = json::from_slice::<Json>(response.body()).unwrap();
            assert_eq!(listed["result"], json!({}));
        }
    }

    impl Drop for Node {
        fn drop(&mut self) { std::fs::remove_dir_all(&self.dbdir).ok(); }
    }

    fn connects(published: &[new_protocol::OrdermatchMessage]) -> usize {
        published
            .iter()
            .filter(|m| matches!(m, new_protocol::OrdermatchMessage::TakerConnect(_)))
            .count()
    }

    fn reservations(published: &[new_protocol::OrdermatchMessage]) -> usize {
        published
            .iter()
            .filter(|m| matches!(m, new_protocol::OrdermatchMessage::MakerReserved(_)))
            .count()
    }

    fn taker_request(base_info: Option<Vec<u8>>, rel_info: Option<Vec<u8>>) -> TakerRequest {
        TakerRequest {
            base: "BASE".into(),
            rel: "REL".into(),
            uuid: Uuid::new_v4(),
            dest_pub_key: H256Json::default(),
            sender_pubkey: H256Json::default(),
            base_amount: 10.into(),
            rel_amount: 10.into(),
            action: TakerAction::Buy,
            match_by: MatchBy::Any,
            conf_settings: None,
            base_protocol_info: base_info,
            rel_protocol_info: rel_info,
            swap_version: SwapVersion::default(),
        }
    }

    fn reservation_for(
        taker: &TakerOrder,
        rel_amount: i32,
        base_info: Option<Vec<u8>>,
        rel_info: Option<Vec<u8>>,
    ) -> MakerReserved {
        MakerReserved {
            base: "BASE".into(),
            rel: "REL".into(),
            base_amount: 10.into(),
            rel_amount: rel_amount.into(),
            sender_pubkey: H256Json::from([rel_amount as u8; 32]),
            dest_pub_key: H256Json::default(),
            maker_order_uuid: Uuid::new_v4(),
            taker_order_uuid: taker.request.uuid,
            conf_settings: None,
            base_protocol_info: base_info,
            rel_protocol_info: rel_info,
            swap_version: SwapVersion::default(),
        }
    }

    fn add_taker_order(node: &Node) -> TakerOrder {
        let taker = TakerOrder {
            request: taker_request(None, None),
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
        let octx = OrdermatchContext::from_ctx(&node.ctx).unwrap();
        block_on(octx.my_taker_orders.lock()).insert(taker.request.uuid, taker.clone());
        taker
    }

    fn matches_of_taker_order(node: &Node, uuid: &Uuid) -> usize {
        let octx = OrdermatchContext::from_ctx(&node.ctx).unwrap();
        let orders = block_on(octx.my_taker_orders.lock());
        orders.get(uuid).unwrap().matches.len()
    }

    fn process_reservations(node: &Node, reservations: Vec<MakerReserved>) {
        let handlers = reservations
            .into_iter()
            .map(|msg| process_maker_reserved(node.ctx.clone(), H256Json::default(), msg));
        block_on(futures::future::join_all(handlers));
    }

    fn with_cap() -> Option<Vec<u8>> { Some(with_word(&first_values()[1])) }

    fn without_cap() -> Option<Vec<u8>> { Some(first_values()[1].clone()) }

    #[test]
    fn t14_f_reservation_without_the_capability_is_not_connected() {
        let _consulted = require_capability();
        let mut node = Node::new();
        let taker = add_taker_order(&node);
        let reserved = reservation_for(&taker, 10, without_cap(), with_cap());
        assert_eq!(taker.match_reserved(&reserved), MatchReservedResult::Matched);

        process_reservations(&node, vec![reserved]);

        assert_eq!(connects(&node.published()), 0);
        assert_eq!(matches_of_taker_order(&node, &taker.request.uuid), 0);
        node.assert_no_swap_and_no_penalty();
    }

    #[test]
    fn t14_f_reservation_declined_by_the_rel_coin_is_not_connected() {
        let _consulted = require_capability();
        let mut node = Node::new();
        let taker = add_taker_order(&node);

        process_reservations(&node, vec![reservation_for(&taker, 10, with_cap(), without_cap())]);

        assert_eq!(connects(&node.published()), 0);
        assert_eq!(matches_of_taker_order(&node, &taker.request.uuid), 0);
        node.assert_no_swap_and_no_penalty();
    }

    #[test]
    fn t14_f_reservation_with_the_capability_is_connected() {
        let _consulted = require_capability();
        let mut node = Node::new();
        let taker = add_taker_order(&node);

        process_reservations(&node, vec![reservation_for(&taker, 10, with_cap(), with_cap())]);

        assert_eq!(connects(&node.published()), 1);
        assert_eq!(matches_of_taker_order(&node, &taker.request.uuid), 1);
        node.assert_no_swap_and_no_penalty();
    }

    #[test]
    fn t14_f_declined_reservation_is_skipped_and_the_next_one_is_connected() {
        let _consulted = require_capability();
        let mut node = Node::new();
        let taker = add_taker_order(&node);
        // The declined one is the cheaper, so it is walked first.
        let declined = reservation_for(&taker, 9, without_cap(), with_cap());
        let accepted = reservation_for(&taker, 10, with_cap(), with_cap());
        let accepted_maker = accepted.maker_order_uuid;

        process_reservations(&node, vec![accepted, declined]);

        assert_eq!(connects(&node.published()), 1);
        let octx = OrdermatchContext::from_ctx(&node.ctx).unwrap();
        let orders = block_on(octx.my_taker_orders.lock());
        let matches = &orders.get(&taker.request.uuid).unwrap().matches;
        assert_eq!(matches.len(), 1);
        assert!(matches.contains_key(&accepted_maker));
        drop(orders);
        node.assert_no_swap_and_no_penalty();
    }

    #[test]
    fn t14_f_the_rel_check_is_skipped_when_the_base_coin_declines() {
        let consulted = require_capability();
        let node = Node::new();
        let taker = add_taker_order(&node);

        process_reservations(&node, vec![reservation_for(&taker, 10, without_cap(), with_cap())]);

        assert_eq!(*consulted.lock().unwrap(), vec!["BASE".to_owned()]);
    }

    #[test]
    fn t14_f_info_line_counts_the_declined_reservations() {
        // The info line is decided by `declined_reservations_summary` (the log output is not captured here).
        let uuid = Uuid::new_v4();
        assert_eq!(declined_reservations_summary(&uuid, 0), None);
        let one = declined_reservations_summary(&uuid, 1).unwrap();
        assert!(one.contains(&uuid.to_string()), "{one}");
        assert!(one.contains("1 reservation(s) declined"), "{one}");
        let three = declined_reservations_summary(&uuid, 3).unwrap();
        assert!(three.contains("3 reservation(s) declined"), "{three}");
    }

    #[test]
    fn t14_f_declining_coin_is_named_and_the_rel_coin_is_consulted_only_after_the_base_accepted() {
        let consulted = require_capability();
        let base = MmCoinEnum::Test(TestCoin::new("BASE"));
        let rel = MmCoinEnum::Test(TestCoin::new("REL"));
        let some = |info: Vec<u8>| Some(info);

        let base_declines = declining_coin_ticker(&base, &rel, &some(first_values()[0].clone()), &with_cap());
        assert_eq!(base_declines.as_deref(), Some("BASE"));
        let rel_declines = declining_coin_ticker(&base, &rel, &with_cap(), &None);
        assert_eq!(rel_declines.as_deref(), Some("REL"));
        assert_eq!(declining_coin_ticker(&base, &rel, &with_cap(), &with_cap()), None);
        assert_eq!(*consulted.lock().unwrap(), vec!["BASE", "BASE", "REL", "BASE", "REL"]);
    }

    fn add_maker_order(node: &Node) -> MakerOrder {
        let base = MmCoinEnum::Test(TestCoin::new("BASE"));
        let rel = MmCoinEnum::Test(TestCoin::new("REL"));
        let order = MakerOrderBuilder::new(&base, &rel)
            .with_max_base_vol(MmNumber::from("100"))
            .with_min_base_vol(Some(MmNumber::from("1")))
            .with_price(MmNumber::from("1"))
            .with_conf_settings(OrderConfirmationsSettings::default())
            .with_save_in_history(false)
            .build_unchecked();
        let octx = OrdermatchContext::from_ctx(&node.ctx).unwrap();
        octx.maker_orders_ctx
            .lock()
            .add_order(&order, StdArc::new(AsyncMutex::new(order.clone())));
        order
    }

    fn matches_of_maker_order(node: &Node, uuid: &Uuid) -> usize {
        let octx = OrdermatchContext::from_ctx(&node.ctx).unwrap();
        let orders = octx.maker_orders_ctx.lock();
        let order_mutex = orders.get_order(uuid).unwrap();
        let order = block_on(order_mutex.lock());
        order.matches.len()
    }

    fn process_request(node: &Node, request: TakerRequest) {
        block_on(process_taker_request(node.ctx.clone(), H256Json::default(), request));
    }

    #[test]
    fn t14_f_request_without_the_capability_is_not_reserved() {
        let _consulted = require_capability();
        let mut node = Node::new();
        let order = add_maker_order(&node);

        process_request(&node, taker_request(without_cap(), with_cap()));

        assert_eq!(reservations(&node.published()), 0);
        assert_eq!(matches_of_maker_order(&node, &order.uuid), 0);
        node.assert_no_swap_and_no_penalty();
    }

    #[test]
    fn t14_f_request_declined_by_the_rel_coin_is_not_reserved() {
        let _consulted = require_capability();
        let mut node = Node::new();
        let order = add_maker_order(&node);

        process_request(&node, taker_request(with_cap(), without_cap()));

        assert_eq!(reservations(&node.published()), 0);
        assert_eq!(matches_of_maker_order(&node, &order.uuid), 0);
        node.assert_no_swap_and_no_penalty();
    }

    #[test]
    fn t14_f_request_with_the_capability_is_reserved() {
        let _consulted = require_capability();
        let mut node = Node::new();
        let order = add_maker_order(&node);

        process_request(&node, taker_request(with_cap(), with_cap()));

        assert_eq!(reservations(&node.published()), 1);
        assert_eq!(matches_of_maker_order(&node, &order.uuid), 1);
        node.assert_no_swap_and_no_penalty();
    }

    #[test]
    fn t14_f_the_rel_check_of_a_request_is_skipped_when_the_base_coin_declines() {
        let consulted = require_capability();
        let node = Node::new();
        add_maker_order(&node);

        process_request(&node, taker_request(without_cap(), with_cap()));

        assert_eq!(*consulted.lock().unwrap(), vec!["BASE".to_owned()]);
    }

    #[test]
    fn t14_f_a_request_that_matched_before_is_not_checked_again() {
        let consulted = require_capability();
        let node = Node::new();
        let order = add_maker_order(&node);
        let request = taker_request(with_cap(), with_cap());

        process_request(&node, request.clone());
        let after_first = consulted.lock().unwrap().len();
        process_request(&node, request);

        assert_eq!(consulted.lock().unwrap().len(), after_first);
        assert_eq!(matches_of_maker_order(&node, &order.uuid), 1);
    }

    // --- CRD ch.61 R61.5A, T10 (reader half) and T12: BC2 swaps require a counterparty that signs with the suffix ---

    fn with_suffix_word(first: &[u8]) -> Vec<u8> {
        append_capability_word(first.to_vec(), &[CAPABILITY_SIGHASH_SUFFIX])
    }

    fn suffix_cap() -> Option<Vec<u8>> { Some(with_suffix_word(&first_values()[1])) }

    fn plain() -> Option<Vec<u8>> { Some(first_values()[1].clone()) }

    /// Test coins where the coin with the ticker `requiring` demands the `sighash_suffix` word of the counterparty
    /// and every other coin judges as a coin with a zero suffix does (it accepts the information). Every consulted
    /// coin ticker is recorded, in order.
    fn require_suffix_on(requiring: &'static str) -> StdArc<Mutex<Vec<String>>> {
        let consulted = StdArc::new(Mutex::new(Vec::new()));
        let log = consulted.clone();
        TestCoin::is_coin_protocol_supported.mock_safe(move |coin, info| {
            log.lock().unwrap().push(coin.ticker().to_owned());
            MockResult::Return(coin.ticker() != requiring || declares_capability(info, CAPABILITY_SIGHASH_SUFFIX))
        });
        consulted
    }

    #[test]
    fn t10_orderbook_address_format_is_the_same_with_the_suffix_word() {
        for first in first_values() {
            assert_eq!(
                addr_format_from_protocol_info(&first),
                addr_format_from_protocol_info(&with_suffix_word(&first))
            );
        }
    }

    #[test]
    fn t12_maker_declines_a_request_whose_base_information_lacks_the_word() {
        let _consulted = require_suffix_on("BASE");
        let mut node = Node::new();
        let order = add_maker_order(&node);

        process_request(&node, taker_request(plain(), plain()));

        assert_eq!(reservations(&node.published()), 0);
        assert_eq!(matches_of_maker_order(&node, &order.uuid), 0);
        node.assert_no_swap_and_no_penalty();
    }

    #[test]
    fn t12_maker_accepts_a_request_that_carries_the_word_and_the_other_coin_does_not_trigger() {
        let consulted = require_suffix_on("BASE");
        let mut node = Node::new();
        let order = add_maker_order(&node);

        // REL carries no word and BASE does: only BASE requires it.
        process_request(&node, taker_request(suffix_cap(), plain()));

        assert_eq!(reservations(&node.published()), 1);
        assert_eq!(matches_of_maker_order(&node, &order.uuid), 1);
        // Both coins were asked, and REL accepted information without the word.
        assert!(consulted.lock().unwrap().iter().any(|ticker| ticker == "REL"));
        node.assert_no_swap_and_no_penalty();
    }

    #[test]
    fn t12_maker_declines_when_the_rel_coin_requires_the_word_and_its_information_lacks_it() {
        let _consulted = require_suffix_on("REL");
        let mut node = Node::new();
        let order = add_maker_order(&node);

        // BASE carries no word and is not required to.
        process_request(&node, taker_request(plain(), plain()));
        assert_eq!(reservations(&node.published()), 0);
        assert_eq!(matches_of_maker_order(&node, &order.uuid), 0);

        process_request(&node, taker_request(plain(), suffix_cap()));
        assert_eq!(reservations(&node.published()), 1);
        assert_eq!(matches_of_maker_order(&node, &order.uuid), 1);
        node.assert_no_swap_and_no_penalty();
    }

    #[test]
    fn t12_taker_declines_a_reservation_whose_base_information_lacks_the_word() {
        let _consulted = require_suffix_on("BASE");
        let mut node = Node::new();
        let taker = add_taker_order(&node);

        process_reservations(&node, vec![reservation_for(&taker, 10, plain(), plain())]);

        assert_eq!(connects(&node.published()), 0);
        assert_eq!(matches_of_taker_order(&node, &taker.request.uuid), 0);
        node.assert_no_swap_and_no_penalty();
    }

    #[test]
    fn t12_taker_accepts_a_reservation_that_carries_the_word_and_the_other_coin_does_not_trigger() {
        let consulted = require_suffix_on("BASE");
        let mut node = Node::new();
        let taker = add_taker_order(&node);

        // REL carries no word and BASE does: only BASE requires it.
        process_reservations(&node, vec![reservation_for(&taker, 10, suffix_cap(), plain())]);

        assert_eq!(connects(&node.published()), 1);
        assert_eq!(matches_of_taker_order(&node, &taker.request.uuid), 1);
        // Both coins were asked, and REL accepted information without the word.
        assert!(consulted.lock().unwrap().iter().any(|ticker| ticker == "REL"));
        node.assert_no_swap_and_no_penalty();
    }

    #[test]
    fn t12_taker_declines_when_the_rel_coin_requires_the_word_and_its_information_lacks_it() {
        let _consulted = require_suffix_on("REL");
        let mut node = Node::new();
        let taker = add_taker_order(&node);

        process_reservations(&node, vec![reservation_for(&taker, 10, plain(), plain())]);
        assert_eq!(connects(&node.published()), 0);
        assert_eq!(matches_of_taker_order(&node, &taker.request.uuid), 0);

        process_reservations(&node, vec![reservation_for(&taker, 10, plain(), suffix_cap())]);
        assert_eq!(connects(&node.published()), 1);
        assert_eq!(matches_of_taker_order(&node, &taker.request.uuid), 1);
        node.assert_no_swap_and_no_penalty();
    }

    #[test]
    fn t12_the_declining_coin_is_named_for_the_log_lines_of_the_order_match_layer() {
        let base = MmCoinEnum::Test(TestCoin::new("BASE"));
        let rel = MmCoinEnum::Test(TestCoin::new("REL"));

        let _consulted = require_suffix_on("BASE");
        assert_eq!(
            declining_coin_ticker(&base, &rel, &plain(), &plain()).as_deref(),
            Some("BASE")
        );
        assert_eq!(declining_coin_ticker(&base, &rel, &suffix_cap(), &plain()), None);

        let _consulted = require_suffix_on("REL");
        assert_eq!(
            declining_coin_ticker(&base, &rel, &plain(), &plain()).as_deref(),
            Some("REL")
        );
        assert_eq!(declining_coin_ticker(&base, &rel, &plain(), &suffix_cap()), None);
    }
}

/// CRD ch.28 R16a, T4a: every ordermatch message the node sends passes the bounds check, and the order record
/// decoded from a peer's bytes is read under the bounds.
mod t4a_bounded_reading_tests {
    use super::*;
    use crate::mm2::lp_network::P2PRequest;
    use crate::mm2::lp_ordermatch::new_protocol::{MakerConnected, MakerOrderCancelled, MakerOrderCreated,
                                                  MakerReserved, MatchBy, OrdermatchMessage, TakerConnect,
                                                  TakerRequest as WireTakerRequest};
    use common::msgpack_bounds::{check_msgpack_bounds, dropped_peer_values, max_nesting_depth, MAX_PEER_NESTING};

    /// Half of the bound: the deepest value the node sends may reach it.
    const HALF_BOUND: usize = MAX_PEER_NESTING / 2;

    /// Asserts that `encoded` passes the check up to its last byte and nests at most half of the bound.
    fn assert_within_half_the_bound(name: &str, encoded: &[u8]) {
        assert_eq!(check_msgpack_bounds(encoded), Ok(encoded.len()), "{name}");
        let depth = max_nesting_depth(encoded);
        println!("t4a depth {name}: {depth}");
        assert!(depth <= HALF_BOUND, "{name}: depth {depth}");
    }

    /// The message as published: the payload, and the signed envelope around it.
    fn assert_ordermatch_message(name: &str, message: OrdermatchMessage) {
        let payload = encode_message(&message).unwrap();
        assert_within_half_the_bound(&format!("{name} payload"), &payload);
        let envelope = encode_and_sign(&message, &[3; 32]).unwrap();
        assert_within_half_the_bound(&format!("{name} envelope"), &envelope);
        decode_signed::<OrdermatchMessage>(&envelope).unwrap();
    }

    fn uuid() -> Uuid { Uuid::new_v4() }

    fn price() -> BigRational { BigRational::new(12345.into(), 1000.into()) }

    fn conf_settings() -> OrderConfirmationsSettings {
        OrderConfirmationsSettings {
            base_confs: 6,
            base_nota: false,
            rel_confs: 3,
            rel_nota: true,
        }
    }

    #[test]
    fn t4a_a_ordermatch_messages_nest_within_half_the_bound() {
        assert_ordermatch_message(
            "MakerOrderCreated",
            OrdermatchMessage::MakerOrderCreated(MakerOrderCreated {
                uuid: uuid().into(),
                base: "RIN".into(),
                rel: "KMD".into(),
                price: price(),
                max_volume: price(),
                min_volume: price(),
                created_at: 1,
                conf_settings: conf_settings(),
                timestamp: 2,
                pair_trie_root: H64::default(),
                base_protocol_info: vec![1, 2, 3],
                rel_protocol_info: vec![4, 5, 6],
            }),
        );

        let mut updated = new_protocol::MakerOrderUpdated::new(Uuid::new_v4());
        updated.with_new_price(price());
        updated.with_new_max_volume(price());
        updated.with_new_min_volume(price());
        updated.with_new_conf_settings(conf_settings());
        assert_ordermatch_message("MakerOrderUpdated", OrdermatchMessage::MakerOrderUpdated(updated));

        assert_ordermatch_message(
            "MakerOrderCancelled",
            OrdermatchMessage::MakerOrderCancelled(MakerOrderCancelled {
                uuid: uuid().into(),
                timestamp: 3,
                pair_trie_root: H64::default(),
            }),
        );

        let mut orders = HashSet::new();
        orders.insert(uuid().into());
        assert_ordermatch_message(
            "TakerRequest",
            OrdermatchMessage::TakerRequest(WireTakerRequest {
                base: "RIN".into(),
                rel: "KMD".into(),
                base_amount: price(),
                rel_amount: price(),
                action: TakerAction::Buy,
                uuid: uuid().into(),
                match_by: MatchBy::Orders(orders),
                conf_settings: conf_settings(),
                base_protocol_info: Some(vec![1, 2, 3]),
                rel_protocol_info: Some(vec![4, 5, 6]),
                swap_version: SwapVersion::default(),
            }),
        );

        assert_ordermatch_message(
            "MakerReserved",
            OrdermatchMessage::MakerReserved(MakerReserved {
                base: "RIN".into(),
                rel: "KMD".into(),
                base_amount: price(),
                rel_amount: price(),
                taker_order_uuid: uuid().into(),
                maker_order_uuid: uuid().into(),
                conf_settings: conf_settings(),
                base_protocol_info: Some(vec![1, 2, 3]),
                rel_protocol_info: Some(vec![4, 5, 6]),
                swap_version: SwapVersion::default(),
            }),
        );

        assert_ordermatch_message(
            "TakerConnect",
            OrdermatchMessage::TakerConnect(TakerConnect {
                taker_order_uuid: uuid().into(),
                maker_order_uuid: uuid().into(),
            }),
        );

        assert_ordermatch_message(
            "MakerConnected",
            OrdermatchMessage::MakerConnected(MakerConnected {
                taker_order_uuid: uuid().into(),
                maker_order_uuid: uuid().into(),
            }),
        );

        let mut trie_roots = HashMap::new();
        trie_roots.insert(alb_ordered_pair("RIN", "KMD"), H64::from([7; 8]));
        trie_roots.insert(alb_ordered_pair("RIN", "BTC"), H64::from([8; 8]));
        assert_ordermatch_message(
            "PubkeyKeepAlive",
            OrdermatchMessage::PubkeyKeepAlive(PubkeyKeepAlive {
                trie_roots,
                timestamp: 4,
            }),
        );
    }

    #[test]
    fn t4a_a_requests_and_sync_answers_nest_within_half_the_bound() {
        let mut trie_roots = HashMap::new();
        trie_roots.insert(alb_ordered_pair("RIN", "KMD"), H64::from([7; 8]));
        let requests = [
            P2PRequest::Ordermatch(OrdermatchRequest::GetOrderbook {
                base: "RIN".into(),
                rel: "KMD".into(),
            }),
            P2PRequest::Ordermatch(OrdermatchRequest::SyncPubkeyOrderbookState {
                pubkey: "02".repeat(33),
                trie_roots,
            }),
            P2PRequest::Ordermatch(OrdermatchRequest::BestOrders {
                coin: "RIN".into(),
                action: BestOrdersAction::Buy,
                volume: price(),
            }),
            P2PRequest::Ordermatch(OrdermatchRequest::OrderbookDepth {
                pairs: vec![("RIN".into(), "KMD".into())],
            }),
            P2PRequest::Ordermatch(OrdermatchRequest::BestOrdersByNumber {
                coin: "RIN".into(),
                action: BestOrdersAction::Sell,
                number: 10,
            }),
            P2PRequest::NetworkInfo(crate::mm2::lp_stats::NetworkInfoRequest::CurrentTimestamp),
        ];
        for (i, request) in requests.iter().enumerate() {
            let encoded = encode_message(request).unwrap();
            assert_within_half_the_bound(&format!("P2PRequest {i}"), &encoded);
            assert_eq!(&decode_message::<P2PRequest>(&encoded).unwrap(), request);
        }

        // A synchronization answer holding a full trie of order records, with side tables.
        let orders = make_random_orders("".into(), &[1; 32], "RIN".into(), "KMD".into(), 5);
        let mut answer = SyncPubkeyOrderbookStateRes {
            last_signed_pubkey_payload: vec![1, 2, 3, 4],
            pair_orders_diff: HashMap::from_iter(iter::once((
                alb_ordered_pair("RIN", "KMD"),
                DeltaOrFullTrie::FullTrie(orders.into_iter().map(|order| (order.uuid, order.into())).collect()),
            ))),
            protocol_infos: HashMap::new(),
            conf_infos: HashMap::new(),
        };
        answer.protocol_infos.insert(Uuid::new_v4(), BaseRelProtocolInfo {
            base: vec![1],
            rel: vec![2],
        });
        answer.conf_infos.insert(Uuid::new_v4(), conf_settings());
        let encoded = encode_message(&answer).unwrap();
        assert_within_half_the_bound("SyncPubkeyOrderbookStateRes", &encoded);
        decode_message::<SyncPubkeyOrderbookStateRes>(&encoded).unwrap();

        // The delta form.
        let mut delta = HashMap::new();
        delta.insert(Uuid::new_v4(), None);
        let answer = SyncPubkeyOrderbookStateRes {
            last_signed_pubkey_payload: vec![1],
            pair_orders_diff: HashMap::from_iter(iter::once((
                alb_ordered_pair("RIN", "KMD"),
                DeltaOrFullTrie::Delta(delta),
            ))),
            protocol_infos: HashMap::new(),
            conf_infos: HashMap::new(),
        };
        let encoded = encode_message(&answer).unwrap();
        assert_within_half_the_bound("SyncPubkeyOrderbookStateRes delta", &encoded);
    }

    fn order_record() -> OrderbookP2PItem {
        let order = make_random_orders("02".repeat(33), &[1; 32], "RIN".into(), "KMD".into(), 1).remove(0);
        order.into()
    }

    /// `bytes`, a map, with one more key whose value is `levels` one-element arrays around nil. The first byte
    /// of `bytes` is a fixmap header.
    fn map_with_extra_key(bytes: &[u8], levels: usize) -> Vec<u8> {
        let mut out = bytes.to_vec();
        assert_eq!(out[0] & 0xf0, 0x80, "a fixmap");
        out[0] += 1;
        out.extend_from_slice(&[0xa5, b'e', b'x', b't', b'r', b'a']);
        out.extend(vec![0x91; levels]);
        out.push(0xc0);
        out
    }

    #[test]
    fn t4a_a_order_record_nests_within_half_the_bound() {
        let encoded = rmp_serde::to_vec(&order_record()).unwrap();
        assert_within_half_the_bound("OrderbookP2PItem", &encoded);
    }

    #[test]
    fn t4a_f_order_record_beyond_the_bound_is_dropped() {
        let record = order_record();
        let encoded = rmp_serde::to_vec(&record).unwrap();
        assert_eq!(OrderbookP2PItem::try_from_bytes(encoded).unwrap(), record);

        // The same record as a map with one more key; the total nesting is 64 and 65 levels.
        let named = rmp_serde::to_vec_named(&record).unwrap();
        assert_eq!(
            OrderbookP2PItem::try_from_bytes(map_with_extra_key(&named, MAX_PEER_NESTING - 1)).unwrap(),
            record
        );
        let before = dropped_peer_values();
        assert!(OrderbookP2PItem::try_from_bytes(map_with_extra_key(&named, MAX_PEER_NESTING)).is_err());
        assert!(OrderbookP2PItem::try_from_bytes(map_with_extra_key(&named, 10_000)).is_err());
        assert!(dropped_peer_values() >= before + 2);

        // A declared length beyond the bytes present.
        let mut declared = vec![0x91, 0xdb];
        declared.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
        assert!(OrderbookP2PItem::try_from_bytes(declared).is_err());
    }

    #[test]
    fn t4a_f_address_format_of_protocol_info_falls_back_beyond_the_bound() {
        for format in [
            AddressFormat::Standard,
            AddressFormat::Segwit,
            AddressFormat::CashAddress {
                network: "bitcoincash".into(),
                pub_addr_prefix: 0,
                p2sh_addr_prefix: 5,
            },
        ] {
            let info = rmp_serde::to_vec(&format).unwrap();
            assert_within_half_the_bound("AddressFormat", &info);
            assert_eq!(addr_format_from_protocol_info(&info), format);

            // Bytes after the first value are ignored.
            let mut with_tail = info.clone();
            with_tail.extend_from_slice(&[0xc1, 0xdd, 0xff, 0xff]);
            assert_eq!(addr_format_from_protocol_info(&with_tail), format);
        }

        // A segwit format with one more key: at the bound it is read, beyond it the standard format is the fallback.
        let named = rmp_serde::to_vec_named(&AddressFormat::Segwit).unwrap();
        let at_bound = map_with_extra_key(&named, MAX_PEER_NESTING - 1);
        assert_eq!(addr_format_from_protocol_info(&at_bound), AddressFormat::Segwit);
        let before = dropped_peer_values();
        let beyond = map_with_extra_key(&named, MAX_PEER_NESTING);
        assert_eq!(addr_format_from_protocol_info(&beyond), AddressFormat::Standard);
        let far_beyond = map_with_extra_key(&named, 10_000);
        assert_eq!(addr_format_from_protocol_info(&far_beyond), AddressFormat::Standard);
        assert!(dropped_peer_values() >= before + 2);
        assert_eq!(
            addr_format_from_protocol_info(&[0xdd, 0xff, 0xff, 0xff, 0xff]),
            AddressFormat::Standard
        );
    }
}

/// CRD ch.05 §5.12B T15-T18: the refusal of a swap whose key is not the key its coin pays from, under a global-HD
/// login. What each coin family reports is pinned in the `coins` crate; here `TestCoin` reports it, so the sites
/// that ask the question run without any network.
mod t05_12b_swap_key_check_tests {
    use super::*;
    use coins::{CoinsContext, MarketCoinOps, MmCoinEnum, SwapKeyDisagreement, SwapOps};
    use crypto::CryptoCtx;

    const HD_MNEMONIC: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    const EVM_REASON: &str = r#"an EVM coin under an HD login ("enable_hd") pays from another key than the one its swaps announce; trade it under a login without "enable_hd""#;
    const SINGLE_KEY_REASON: &str = r#"it was enabled with a single key (legacy "enable" or "electrum") under an HD login, so it pays from another key than the one its swaps announce; enable it through its HD activation, or log in without "enable_hd""#;

    /// `ETH` is the EVM coin, `RIN` a UTXO coin enabled with a single key, `RINX` a UTXO coin activated at
    /// address 1, `RINH` a UTXO coin at its first address (no disagreement).
    fn mock_coins() {
        TestCoin::swap_key_disagreement.mock_safe(|coin, _node_pubkey| {
            MockResult::Return(match coin.ticker() {
                "ETH" => Some(SwapKeyDisagreement::EvmSigner),
                "RIN" => Some(SwapKeyDisagreement::SingleKey),
                "RINX" => Some(SwapKeyDisagreement::HdAddress {
                    activated_path: "m/44'/141'/0'/0/1".to_owned(),
                    paying_path: "m/44'/141'/0'/0/0".to_owned(),
                }),
                _ => None,
            })
        });
        TestCoin::min_trading_vol.mock_safe(|_| MockResult::Return(MmNumber::from("0.00000001")));
    }

    /// A node logged in with the HD mnemonic (`hd`) or the iguana passphrase, with the coins activated.
    fn ctx_with(conf: Json, hd: bool, tickers: &[&str]) -> MmArc {
        let ctx = MmCtxBuilder::new().with_conf(conf).into_mm_arc();
        if hd {
            CryptoCtx::init_with_global_hd_account(ctx.clone(), HD_MNEMONIC).unwrap();
        } else {
            CryptoCtx::init_with_iguana_passphrase(ctx.clone(), "an iguana passphrase of the test").unwrap();
        }
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

    fn refusal(coin: &str, reason: &str) -> String { format!("{coin} cannot be swapped under this login: {reason}") }

    fn mismatch_of(ctx: &MmArc, base: &str, rel: &str) -> Option<String> {
        let (base, rel) = pair(base, rel);
        swap_key_mismatch(ctx, &base, &rel).map(|mismatch| mismatch.to_string())
    }

    /// R38: the three reasons, word for word, and the coin they name: the first of the pair that fails.
    #[test]
    fn the_refusal_text_is_the_one_of_r38() {
        mock_coins();
        let ctx = ctx_with(json!({}), true, &[]);
        assert_eq!(mismatch_of(&ctx, "ETH", "KMD"), Some(refusal("ETH", EVM_REASON)));
        assert_eq!(mismatch_of(&ctx, "KMD", "ETH"), Some(refusal("ETH", EVM_REASON)));
        assert_eq!(mismatch_of(&ctx, "RIN", "KMD"), Some(refusal("RIN", SINGLE_KEY_REASON)));
        assert_eq!(
            mismatch_of(&ctx, "KMD", "RINX"),
            Some(refusal(
                "RINX",
                "it is activated at m/44'/141'/0'/0/1, but its swaps pay from m/44'/141'/0'/0/0; activate it at account 0, external chain, address 0"
            ))
        );
        // The base coin is named when both fail.
        assert_eq!(mismatch_of(&ctx, "RIN", "ETH"), Some(refusal("RIN", SINGLE_KEY_REASON)));
        // A pair whose coins agree passes.
        assert_eq!(mismatch_of(&ctx, "RINH", "KMD"), None);
    }

    /// T15: an iguana login passes this check without asking any coin.
    #[test]
    fn an_iguana_login_is_never_refused() {
        TestCoin::swap_key_disagreement.mock_safe(|_, _| panic!("no coin is asked under an iguana login"));
        let ctx = ctx_with(json!({}), false, &[]);
        assert_eq!(mismatch_of(&ctx, "ETH", "RIN"), None);
    }

    /// T18 and R40: only an explicit JSON `false` switches the check off.
    #[test]
    fn the_switch_false_disables_the_check() {
        mock_coins();
        for (conf, enabled) in [
            (json!({}), true),
            (json!({ "swap_key_check": true }), true),
            (json!({ "swap_key_check": null }), true),
            (json!({ "swap_key_check": "false" }), true),
            (json!({ "swap_key_check": false }), false),
        ] {
            let ctx = ctx_with(conf.clone(), true, &[]);
            assert_eq!(swap_key_check_enabled(&ctx), enabled, "{conf}");
            assert_eq!(mismatch_of(&ctx, "ETH", "RIN").is_some(), enabled, "{conf}");
        }
    }

    /// T18: a coin whose swaps are off (wallet only) is not refused by this rule.
    #[test]
    fn a_wallet_only_coin_is_exempt() {
        mock_coins();
        let ctx = ctx_with(
            json!({ "coins": [{ "coin": "ETH", "wallet_only": true }, { "coin": "RIN", "wallet_only": true }] }),
            true,
            &[],
        );
        assert_eq!(mismatch_of(&ctx, "ETH", "RIN"), None);
        assert_eq!(mismatch_of(&ctx, "ETH", "KMD"), None);
    }

    /// T15: `setprice`, `buy`, `sell` and `update_maker_order` refuse the pair before any balance check and
    /// answer the text of R38; an iguana login, and `swap_key_check` false, pass the check.
    #[test]
    fn t15_order_creation_and_update_are_refused() {
        mock_coins();
        let ctx = ctx_with(json!({}), true, &["ETH", "KMD", "RIN", "RINX"]);
        for (base, rel, expected) in [
            ("ETH", "KMD", refusal("ETH", EVM_REASON)),
            ("KMD", "RIN", refusal("RIN", SINGLE_KEY_REASON)),
        ] {
            let req: SetPriceReq =
                json::from_value(json!({ "base": base, "rel": rel, "price": "1", "volume": "1" })).unwrap();
            // A legacy error carries the location of its source, as every other legacy refusal does.
            let refused_with = |err: Option<String>, what: &str| match err {
                Some(err) if err.ends_with(expected.as_str()) => (),
                other => panic!("{what} {base}/{rel}: {other:?}"),
            };
            refused_with(block_on(create_maker_order(&ctx, req)).err(), "setprice");

            for method in ["buy", "sell"] {
                let body = json!({ "base": base, "rel": rel, "price": "1", "volume": "1", "method": method });
                let err = match method {
                    "buy" => block_on(buy(ctx.clone(), body)),
                    _ => block_on(sell(ctx.clone(), body)),
                }
                .err();
                refused_with(err, method);
            }
        }

        let (base, rel) = pair("ETH", "KMD");
        let order = MakerOrderBuilder::new(&base, &rel)
            .with_max_base_vol(1.into())
            .with_price(1.into())
            .with_conf_settings(OrderConfirmationsSettings::default())
            .build_unchecked();
        OrdermatchContext::from_ctx(&ctx)
            .unwrap()
            .maker_orders_ctx
            .lock()
            .add_order(&order, Arc::new(AsyncMutex::new(order.clone())));
        let req = MakerOrderUpdateReq {
            uuid: order.uuid,
            new_price: Some(2.into()),
            max: None,
            volume_delta: None,
            min_volume: None,
            base_confs: None,
            base_nota: None,
            rel_confs: None,
            rel_nota: None,
        };
        let err = block_on(update_maker_order(&ctx, req)).err().unwrap();
        assert!(err.ends_with(&refusal("ETH", EVM_REASON)), "{err}");
    }

    /// T15: the same requests under an iguana login, and with the switch off, are not refused by this check (they
    /// stop later, at the first thing `TestCoin` cannot answer).
    #[test]
    fn t15_the_same_requests_pass_this_check_when_the_keys_agree_or_the_check_is_off() {
        mock_coins();
        for (conf, hd) in [(json!({}), false), (json!({ "swap_key_check": false }), true)] {
            let ctx = ctx_with(conf, hd, &["ETH", "KMD"]);
            let req: SetPriceReq =
                json::from_value(json!({ "base": "ETH", "rel": "KMD", "price": "1", "volume": "1" })).unwrap();
            let outcome =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| block_on(create_maker_order(&ctx, req))));
            if let Ok(Err(err)) = outcome {
                assert!(!err.contains("cannot be swapped under this login"), "{err}");
            }
        }
    }

    fn matching_request(base: &str, rel: &str) -> TakerRequest {
        TakerRequest {
            base: base.into(),
            rel: rel.into(),
            uuid: Uuid::new_v4(),
            dest_pub_key: H256Json::default(),
            sender_pubkey: H256Json::default(),
            base_amount: MmNumber::from(1),
            rel_amount: MmNumber::from(1),
            action: TakerAction::Buy,
            match_by: MatchBy::Any,
            conf_settings: None,
            base_protocol_info: None,
            rel_protocol_info: None,
            swap_version: SwapVersion::default(),
        }
    }

    /// T17: a stored order of ours whose pair fails the rule gets no reservation when a request matches it. It is
    /// cancelled once, as an ordinary cancellation that peers see, and no swap starts.
    #[test]
    fn t17_an_own_order_that_fails_is_cancelled_at_the_match_check() {
        mock_coins();
        let ctx = ctx_with(json!({}), true, &["ETH", "KMD"]);
        let (tx, mut p2p_rx) = mpsc::channel(100);
        P2PContext::new(tx).store_to_mm_arc(&ctx);
        let deleted = Arc::new(Mutex::new(Vec::<String>::new()));
        let deleted_log = deleted.clone();
        delete_my_maker_order.mock_safe(move |_, order, reason| {
            deleted_log.lock().unwrap().push(format!("{} {}", order.uuid, reason));
            MockResult::Return(Box::new(futures01::future::ok(())))
        });
        lp_connect_start_bob.mock_safe(|_, _, _| panic!("no swap starts for a pair that fails the rule"));

        let (base, rel) = pair("ETH", "KMD");
        let order = MakerOrderBuilder::new(&base, &rel)
            .with_max_base_vol(1.into())
            .with_min_base_vol(Some(MmNumber::from("0.01")))
            .with_price(1.into())
            .with_conf_settings(OrderConfirmationsSettings::default())
            .build_unchecked();
        let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
        ordermatch_ctx
            .maker_orders_ctx
            .lock()
            .add_order(&order, Arc::new(AsyncMutex::new(order.clone())));

        let request = matching_request("ETH", "KMD");
        assert!(matches!(
            order.match_with_request(&request),
            OrderMatchResult::Matched(_)
        ));
        block_on(process_taker_request(ctx.clone(), H256Json::default(), request.clone()));
        // A second request finds no order: the cancellation is not repeated.
        block_on(process_taker_request(ctx.clone(), H256Json::default(), request));

        assert!(!ordermatch_ctx.maker_orders_ctx.lock().contains_key(&order.uuid));
        assert_eq!(*deleted.lock().unwrap(), vec![format!(
            "{} {}",
            order.uuid,
            MakerOrderCancellationReason::Cancelled
        )]);
        // The only message published is the cancellation of the order; no reservation is sent.
        let mut published = Vec::new();
        while let Ok(cmd) = p2p_rx.try_recv() {
            if let AdexBehaviourCmd::PublishMsg { msg, .. } | AdexBehaviourCmd::PublishMsgFrom { msg, .. } = cmd {
                let (message, ..) = decode_signed::<new_protocol::OrdermatchMessage>(&msg).unwrap();
                published.push(message);
            }
        }
        assert_eq!(published.len(), 1, "{published:?}");
        assert!(
            matches!(&published[0], new_protocol::OrdermatchMessage::MakerOrderCancelled(c) if c.uuid == order.uuid.into()),
            "{:?}",
            published[0]
        );
    }

    /// R37: no connect message is sent for a reservation whose pair fails the rule; with the keys in agreement
    /// the same reservation is answered with the connect message, so the setup does reach the send.
    #[test]
    fn a_connect_message_is_not_sent_for_a_pair_that_fails() {
        mock_coins();
        TestCoin::is_coin_protocol_supported.mock_safe(|_, _| MockResult::Return(true));
        TestCoin::coin_protocol_info.mock_safe(|_| MockResult::Return(Vec::new()));
        for (base_ticker, expects_connect) in [("ETH", false), ("KMD", true)] {
            let ctx = ctx_with(json!({}), true, &[base_ticker, "REL"]);
            let (tx, mut p2p_rx) = mpsc::channel(100);
            P2PContext::new(tx).store_to_mm_arc(&ctx);
            let (base, rel) = pair(base_ticker, "REL");
            let order = TakerOrderBuilder::new(&base, &rel)
                .with_base_amount(MmNumber::from(1))
                .with_rel_amount(MmNumber::from(1))
                .with_action(TakerAction::Buy)
                .with_match_by(MatchBy::Any)
                .with_conf_settings(OrderConfirmationsSettings::default())
                .with_sender_pubkey(H256Json::from([1u8; 32]))
                .build_unchecked();
            let uuid = order.request.uuid;
            let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
            block_on(ordermatch_ctx.my_taker_orders.lock()).insert(uuid, order);

            let reserved = MakerReserved {
                base: base_ticker.into(),
                rel: "REL".into(),
                base_amount: MmNumber::from(1),
                rel_amount: MmNumber::from(1),
                taker_order_uuid: uuid,
                maker_order_uuid: Uuid::new_v4(),
                sender_pubkey: H256Json::from([2u8; 32]),
                dest_pub_key: H256Json::default(),
                conf_settings: Some(OrderConfirmationsSettings::default()),
                base_protocol_info: Some(Vec::new()),
                rel_protocol_info: Some(Vec::new()),
                swap_version: SwapVersion::default(),
            };
            block_on(process_maker_reserved(ctx.clone(), reserved.sender_pubkey, reserved));

            let order = block_on(ordermatch_ctx.my_taker_orders.lock()).remove(&uuid).unwrap();
            assert_eq!(!order.matches.is_empty(), expects_connect, "{base_ticker}");
            let mut sent = 0;
            while let Ok(cmd) = p2p_rx.try_recv() {
                if matches!(
                    cmd,
                    AdexBehaviourCmd::PublishMsg { .. } | AdexBehaviourCmd::PublishMsgFrom { .. }
                ) {
                    sent += 1;
                }
            }
            assert_eq!(sent > 0, expects_connect, "{base_ticker}: {sent} messages published");
        }
    }

    /// R39: the WARN line.
    #[test]
    fn the_cancel_line_is_the_text_of_r39() {
        let uuid = Uuid::new_v4();
        let mismatch = SwapKeyMismatch {
            coin: "ETH".into(),
            reason: EVM_REASON.into(),
        };
        assert_eq!(
            swap_key_cancel_line(&uuid, &mismatch),
            format!("Maker order {uuid} cancelled: ETH cannot be swapped under this login: {EVM_REASON}")
        );
    }
}

/// CRD ch.05 §5.12D (R45, R46, R48; T13, T25, T26, T29, T30): the node key of a global-HD login is the internal
/// key, and the passphrase hash is only the separate legacy key.
mod hd_trading_identity_tests {
    use super::*;
    use crate::mm2::lp_native_dex::relay_p2p_identity_secret;
    use crypto::CryptoCtx;
    use kdf_crypto::sha256;

    /// A valid BIP39 mnemonic used as the passphrase of the HD login.
    const HD_MNEMONIC: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    fn hd_ctx() -> (MmArc, std::sync::Arc<CryptoCtx>, mpsc::Receiver<AdexBehaviourCmd>) {
        let ctx = MmCtxBuilder::new()
            .with_conf(json::json!({"netid": 8762}))
            .into_mm_arc();
        ctx.init_metrics().unwrap();
        let crypto_ctx = CryptoCtx::init_with_global_hd_account(ctx.clone(), HD_MNEMONIC).unwrap();
        let (tx, rx) = mpsc::channel(100);
        P2PContext::new(tx).store_to_mm_arc(&ctx);
        (ctx, crypto_ctx, rx)
    }

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

    /// T13, T25 (R45, R48): under a global-HD login the node key is the internal key, and the passphrase hash is
    /// another key; the order key (an order announced from storage), `sender_pubkey` and the persistent key all
    /// read the node key, which is the key `is_mine` and the keep-alive already used.
    #[test]
    fn t13_t25_the_node_key_is_the_internal_key() {
        let (ctx, crypto_ctx, mut rx) = hd_ctx();
        let internal = *crypto_ctx.mm2_internal_key_pair();
        let hash_key = key_pair_from_seed(HD_MNEMONIC).unwrap();
        assert_ne!(
            internal.public(),
            hash_key.public(),
            "the two keys of an HD login differ"
        );

        assert_eq!(ctx.secp256k1_key_pair().public(), internal.public());
        assert_eq!(ctx.secp256k1_key_pair().private_bytes(), internal.private_bytes());
        assert_eq!(ctx.legacy_passphrase_key_pair().public(), hash_key.public());
        assert_eq!(
            ctx.legacy_passphrase_key_pair().private_bytes(),
            hash_key.private_bytes()
        );
        // `sender_pubkey` of a taker request and the other ordermatch messages.
        assert_eq!(ctx.public_id().unwrap().bytes, internal.public()[1..33]);
        let internal_hex = hex::encode(internal.public_slice());
        assert_eq!(crypto_ctx.mm2_internal_pubkey_hex(), internal_hex);

        // An order announced from storage at start is announced and signed under the internal key.
        let order = own_order();
        reannounce_missing_own_order(&ctx, &order, vec![], vec![]);
        let mut signers = Vec::new();
        while let Ok(cmd) = rx.try_recv() {
            if let AdexBehaviourCmd::PublishMsg { msg, .. } | AdexBehaviourCmd::PublishMsgFrom { msg, .. } = cmd {
                let (_, _, signer) = decode_signed::<new_protocol::OrdermatchMessage>(&msg).unwrap();
                signers.push(signer.to_hex());
            }
        }
        assert_eq!(signers, vec![internal_hex.clone()]);
        let ordermatch_ctx = OrdermatchContext::from_ctx(&ctx).unwrap();
        let item = ordermatch_ctx
            .orderbook
            .lock()
            .order_set
            .get(&order.uuid)
            .cloned()
            .unwrap();
        assert_eq!(
            item.pubkey, internal_hex,
            "the orderbook entry carries the internal key"
        );
        // `is_mine`, and so the stale-order exemption, hold for it.
        let my_pub = mm2_internal_pubkey_hex(&ctx).unwrap();
        assert!(is_my_order(&item.pubkey, &my_pub, &HashSet::new()));
        assert_eq!(my_pub.as_deref(), Some(internal_hex.as_str()));
    }

    /// T13, T25: `rmd160` and the database paths already derive from the internal key and are unchanged.
    #[test]
    fn t13_rmd160_and_database_path_derive_from_the_internal_key() {
        let (ctx, crypto_ctx, _rx) = hd_ctx();
        let internal_rmd160 = crypto_ctx.mm2_internal_key_pair().public().address_hash();
        assert_eq!(*ctx.rmd160(), internal_rmd160);
        assert_eq!(
            ctx.dbdir(),
            std::path::Path::new("DB").join(hex::encode(*internal_rmd160))
        );
        let hash_rmd160 = key_pair_from_seed(HD_MNEMONIC).unwrap().public().address_hash();
        assert_ne!(*ctx.rmd160(), hash_rmd160);
    }

    /// T26 (R45 item 6): the relay's libp2p identity secret is the SHA-256 of the internal key under a global-HD
    /// login, and of the passphrase hash (as before) under an iguana login.
    #[test]
    fn t26_the_relay_identity_comes_from_the_node_key() {
        let (ctx, crypto_ctx, _rx) = hd_ctx();
        assert_eq!(
            relay_p2p_identity_secret(&ctx),
            sha256(&*crypto_ctx.mm2_internal_key_pair().private().secret).take()
        );
        assert_ne!(
            relay_p2p_identity_secret(&ctx),
            sha256(&*key_pair_from_seed(HD_MNEMONIC).unwrap().private().secret).take()
        );

        let iguana = MmCtxBuilder::new().into_mm_arc();
        CryptoCtx::init_with_iguana_passphrase(iguana.clone(), "passphrase").unwrap();
        assert_eq!(
            relay_p2p_identity_secret(&iguana),
            sha256(&*key_pair_from_seed("passphrase").unwrap().private().secret).take()
        );
    }

    /// T29 (R46): the legacy key an HD login keeps for the Lightning seed and the BCH-with-tokens and Tendermint
    /// activations is the passphrase hash, which was the context key before this section.
    #[test]
    fn t29_the_legacy_key_is_the_passphrase_hash() {
        let (ctx, ..) = hd_ctx();
        let before = key_pair_from_seed(HD_MNEMONIC).unwrap();
        assert_eq!(
            ctx.legacy_passphrase_key_pair().private().secret,
            before.private().secret
        );
    }

    /// T30 (R45 to R48): under an iguana login the node key, the legacy key, `rmd160` and the relay identity are
    /// the passphrase hash, as in the previous build.
    #[test]
    fn t30_an_iguana_login_is_unchanged() {
        let ctx = MmCtxBuilder::new().into_mm_arc();
        ctx.init_metrics().unwrap();
        CryptoCtx::init_with_iguana_passphrase(ctx.clone(), "passphrase").unwrap();
        let hash_key = key_pair_from_seed("passphrase").unwrap();
        assert_eq!(ctx.secp256k1_key_pair().private().secret, hash_key.private().secret);
        assert_eq!(
            ctx.legacy_passphrase_key_pair().private().secret,
            hash_key.private().secret
        );
        assert_eq!(*ctx.rmd160(), hash_key.public().address_hash());
    }
}
