use super::*;

/// https://github.com/artemii235/SuperNET/issues/241
#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn alice_can_see_the_active_order_after_connection() {
    let coins = json!([
        {"coin":"DOC","asset":"DOC","rpcport":62415,"txversion":4,"overwintered":1,"protocol":{"type":"UTXO"}},
        {"coin":"MARTY","asset":"MARTY","rpcport":52592,"txversion":4,"overwintered":1,"protocol":{"type":"UTXO"}}
    ]);

    // start bob and immediately place the order
    let mm_bob = MarketMakerIt::start(
        json! ({
            "gui": "nogui",
            "netid": 9998,
            "myipaddr": env::var ("BOB_TRADE_IP") .ok(),
            "rpcip": env::var ("BOB_TRADE_IP") .ok(),
            "canbind": env::var ("BOB_TRADE_PORT") .ok().map (|s| s.parse::<i64>().unwrap()),
            "passphrase": "bob passphrase",
            "coins": coins,
            "rpc_password": "pass",
            "i_am_seed": true,
        }),
        "pass".into(),
        local_start!("bob"),
    )
    .unwrap();
    let (_bob_dump_log, _bob_dump_dashboard) = mm_bob.mm_dump();
    log!({"Bob log path: {}", mm_bob.log_path.display()});
    // Enable coins on Bob side. Print the replies in case we need the "address".
    log!({ "enable_coins (bob): {:?}", block_on(enable_coins_doc_marty_electrum(&mm_bob)) });
    // issue sell request on Bob side by setting base/rel price
    log!("Issue bob sell request");
    let rc = block_on(mm_bob.rpc(&json! ({
        "userpass": mm_bob.userpass,
        "method": "setprice",
        "base": "DOC",
        "rel": "MARTY",
        "price": 0.9,
        "volume": "0.9",
    })))
    .unwrap();
    assert!(rc.0.is_success(), "!setprice: {}", rc.1);
    // Bob orderbook must show the new order
    log!("Get DOC/MARTY orderbook on Bob side");
    let rc = block_on(mm_bob.rpc(&json! ({
        "userpass": mm_bob.userpass,
        "method": "orderbook",
        "base": "DOC",
        "rel": "MARTY",
    })))
    .unwrap();
    assert!(rc.0.is_success(), "!orderbook: {}", rc.1);

    let bob_orderbook: OrderbookResponse = json::from_str(&rc.1).unwrap();
    log!("Bob orderbook "[bob_orderbook]);
    assert!(!bob_orderbook.asks.is_empty(), "Bob DOC/MARTY asks are empty");
    assert_eq!(BigDecimal::from_str("0.9").unwrap(), bob_orderbook.asks[0].max_volume);

    // start eve and immediately place the order
    let mm_eve = MarketMakerIt::start(
        json! ({
            "gui": "nogui",
            "netid": 9998,
            "myipaddr": env::var ("BOB_TRADE_IP") .ok(),
            "rpcip": env::var ("BOB_TRADE_IP") .ok(),
            "canbind": env::var ("BOB_TRADE_PORT") .ok().map (|s| s.parse::<i64>().unwrap()),
            "passphrase": "eve passphrase",
            "coins": coins,
            "rpc_password": "pass",
            "seednodes": [fomat!((mm_bob.ip))],
        }),
        "pass".into(),
        local_start!("bob"),
    )
    .unwrap();
    let (_eve_dump_log, _eve_dump_dashboard) = mm_eve.mm_dump();
    log!({ "Eve log path: {}", mm_eve.log_path.display() });
    // Enable coins on Eve side. Print the replies in case we need the "address".
    log!({ "enable_coins (eve): {:?}", block_on(enable_coins_doc_marty_electrum(&mm_eve)) });
    // issue sell request on Eve side by setting base/rel price
    log!("Issue eve sell request");
    let rc = block_on(mm_eve.rpc(&json! ({
        "userpass": mm_eve.userpass,
        "method": "setprice",
        "base": "DOC",
        "rel": "MARTY",
        "price": "1",
        "volume": "0.9",
    })))
    .unwrap();
    assert!(rc.0.is_success(), "!setprice: {}", rc.1);
    // issue sell request on Eve side by setting base/rel price
    log!("Issue eve sell request");
    let rc = block_on(mm_eve.rpc(&json! ({
        "userpass": mm_eve.userpass,
        "method": "setprice",
        "base": "MARTY",
        "rel": "DOC",
        "price": "1",
        "volume": "0.9",
    })))
    .unwrap();
    assert!(rc.0.is_success(), "!setprice: {}", rc.1);

    log!("Get DOC/MARTY orderbook on Eve side");
    let rc = block_on(mm_eve.rpc(&json! ({
        "userpass": mm_eve.userpass,
        "method": "orderbook",
        "base": "DOC",
        "rel": "MARTY",
    })))
    .unwrap();
    assert!(rc.0.is_success(), "!orderbook: {}", rc.1);

    let eve_orderbook: OrderbookResponse = json::from_str(&rc.1).unwrap();
    log!("Eve orderbook "[eve_orderbook]);
    assert_eq!(
        eve_orderbook.asks.len(),
        2,
        "Eve DOC/MARTY orderbook must have exactly 2 asks"
    );
    assert_eq!(
        eve_orderbook.bids.len(),
        1,
        "Eve DOC/MARTY orderbook must have exactly 1 bid"
    );

    log!("Give Bob 2 seconds to import Eve order");
    thread::sleep(Duration::from_secs(2));
    log!("Get DOC/MARTY orderbook on Bob side");
    let rc = block_on(mm_bob.rpc(&json! ({
        "userpass": mm_bob.userpass,
        "method": "orderbook",
        "base": "DOC",
        "rel": "MARTY",
    })))
    .unwrap();
    assert!(rc.0.is_success(), "!orderbook: {}", rc.1);

    let bob_orderbook: OrderbookResponse = json::from_str(&rc.1).unwrap();
    log!("Bob orderbook "[bob_orderbook]);
    assert_eq!(
        bob_orderbook.asks.len(),
        2,
        "Bob DOC/MARTY orderbook must have exactly 2 asks"
    );
    assert_eq!(
        bob_orderbook.bids.len(),
        1,
        "Bob DOC/MARTY orderbook must have exactly 1 bid"
    );

    let mm_alice = MarketMakerIt::start(
        json! ({
            "gui": "nogui",
            "netid": 9998,
            "myipaddr": env::var ("ALICE_TRADE_IP") .ok(),
            "rpcip": env::var ("ALICE_TRADE_IP") .ok(),
            "passphrase": "alice passphrase",
            "coins": coins,
            "seednodes": [fomat!((mm_bob.ip))],
            "rpc_password": "pass",
        }),
        "pass".into(),
        local_start!("alice"),
    )
    .unwrap();

    let (_alice_dump_log, _alice_dump_dashboard) = mm_alice.mm_dump();
    log!({ "Alice log path: {}", mm_alice.log_path.display() });

    // Enable coins on Alice side. Print the replies in case we need the "address".
    log!({ "enable_coins (alice): {:?}", block_on(enable_coins_doc_marty_electrum(&mm_alice)) });

    log!("Get DOC/MARTY orderbook on Alice side");
    let rc = block_on(mm_alice.rpc(&json! ({
        "userpass": mm_alice.userpass,
        "method": "orderbook",
        "base": "DOC",
        "rel": "MARTY",
    })))
    .unwrap();
    assert!(rc.0.is_success(), "!orderbook: {}", rc.1);

    let alice_orderbook: OrderbookResponse = json::from_str(&rc.1).unwrap();
    log!("Alice orderbook "[alice_orderbook]);
    assert_eq!(
        alice_orderbook.asks.len(),
        2,
        "Alice DOC/MARTY orderbook must have exactly 2 asks"
    );
    assert_eq!(
        alice_orderbook.bids.len(),
        1,
        "Alice DOC/MARTY orderbook must have exactly 1 bid"
    );

    block_on(mm_bob.stop()).unwrap();
    block_on(mm_alice.stop()).unwrap();
    block_on(mm_eve.stop()).unwrap();
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn alice_can_see_the_active_order_after_orderbook_sync_segwit() {
    let bob_passphrase = get_passphrase(&".env.seed", "BOB_PASSPHRASE").unwrap();

    let bob_coins_config = json!([
        {"coin":"DOC","asset":"DOC","rpcport":62415,"txversion":4,"overwintered":1,"protocol":{"type":"UTXO"}},
        {"coin":"tBTC","name":"tbitcoin","fname":"tBitcoin","rpcport":18332,"pubtype":111,"p2shtype":196,"wiftype":239,"segwit":true,"bech32_hrp":"tb","txfee":0,"estimate_fee_mode":"ECONOMICAL","mm2":1,"required_confirmations":0,"protocol":{"type":"UTXO"},"address_format":{"format":"segwit"}}
    ]);

    let alice_coins_config = json!([
        {"coin":"DOC","asset":"DOC","rpcport":62415,"txversion":4,"overwintered":1,"protocol":{"type":"UTXO"}},
        {"coin":"tBTC","name":"tbitcoin","fname":"tBitcoin","rpcport":18332,"pubtype":111,"p2shtype":196,"wiftype":239,"segwit":true,"bech32_hrp":"tb","txfee":0,"estimate_fee_mode":"ECONOMICAL","mm2":1,"required_confirmations":0,"protocol":{"type":"UTXO"},"address_format":{"format":"segwit"}}
    ]);

    let mut mm_bob = MarketMakerIt::start(
        json! ({
            "gui": "nogui",
            "netid": 9998,
            "myipaddr": env::var ("BOB_TRADE_IP") .ok(),
            "rpcip": env::var ("BOB_TRADE_IP") .ok(),
            "canbind": env::var ("BOB_TRADE_PORT") .ok().map (|s| s.parse::<i64>().unwrap()),
            "passphrase": bob_passphrase,
            "coins": bob_coins_config,
            "rpc_password": "pass",
            "i_am_seed": true,
        }),
        "pass".into(),
        local_start!("bob"),
    )
    .unwrap();
    let (_bob_dump_log, _bob_dump_dashboard) = mm_bob.mm_dump();
    log!({"Bob log path: {}", mm_bob.log_path.display()});

    // Enable coins on Bob side. Print the replies in case we need the "address".
    let electrum = block_on(mm_bob.rpc(&json!({
        "userpass": "pass",
        "method": "electrum",
        "coin": "tBTC",
        "servers": [{"url":"blockstream.info:143"},{"url":"blackie.c3-soft.com:57005"},{"url":"testnet.qtornado.com:51001"}],
        "address_format":{"format":"segwit"},
        "mm2": 1,
    }))).unwrap();
    assert_eq!(
        electrum.0,
        StatusCode::OK,
        "RPC «electrum» failed with {} {}",
        electrum.0,
        electrum.1
    );
    log!({ "enable tBTC: {:?}", electrum });
    let enable_tbtc_res: EnableElectrumResponse = json::from_str(&electrum.1).unwrap();
    let tbtc_segwit_address = enable_tbtc_res.address;

    let electrum = block_on(mm_bob.rpc(&json!({
        "userpass": "pass",
        "method": "electrum",
        "coin": "DOC",
        "servers": [{"url":"doc.electrum1.cipig.net:10020"},{"url":"doc.electrum2.cipig.net:10020"},{"url":"doc.electrum3.cipig.net:10020"}],
        "mm2": 1,
    }))).unwrap();
    assert_eq!(
        electrum.0,
        StatusCode::OK,
        "RPC «electrum» failed with {} {}",
        electrum.0,
        electrum.1
    );
    log!({ "enable DOC: {:?}", electrum });
    let enable_doc_res: Json = json::from_str(&electrum.1).unwrap();
    let doc_address = enable_doc_res["address"].as_str().unwrap();

    // issue sell request on Bob side by setting base/rel price
    log!("Issue bob sell requests");

    let bob_orders = [
        // (base, rel, price, volume, min_volume)
        ("tBTC", "DOC", "0.7", "0.0002", Some("0.00015")),
        ("DOC", "tBTC", "0.7", "0.0002", Some("0.00015")),
    ];
    for (base, rel, price, volume, min_volume) in bob_orders.iter() {
        let rc = block_on(mm_bob.rpc(&json! ({
            "userpass": mm_bob.userpass,
            "method": "setprice",
            "base": base,
            "rel": rel,
            "price": price,
            "volume": volume,
            "min_volume": min_volume.unwrap_or("0.00777"),
            "cancel_previous": false,
        })))
        .unwrap();
        assert!(rc.0.is_success(), "!setprice: {}", rc.1);
    }

    let rc = block_on(mm_bob.rpc(&json! ({
        "userpass": mm_bob.userpass,
        "mmrpc": "2.0",
        "method": "get_public_key",
        "params": {},
    })))
    .unwrap();
    assert!(rc.0.is_success(), "!get_public_key: {}", rc.1);
    let get_public_key_res: RpcV2Response<GetPublicKeyResult> = serde_json::from_str(&rc.1).unwrap();
    let bob_pubkey = get_public_key_res.result.public_key;

    let mut mm_alice = MarketMakerIt::start(
        json! ({
            "gui": "nogui",
            "netid": 9998,
            "myipaddr": env::var ("ALICE_TRADE_IP") .ok(),
            "rpcip": env::var ("ALICE_TRADE_IP") .ok(),
            "passphrase": "alice passphrase",
            "coins": alice_coins_config,
            "seednodes": [fomat!((mm_bob.ip))],
            "rpc_password": "pass",
        }),
        "pass".into(),
        local_start!("alice"),
    )
    .unwrap();

    let (_alice_dump_log, _alice_dump_dashboard) = mm_alice.mm_dump();
    log!({ "Alice log path: {}", mm_alice.log_path.display() });

    block_on(mm_bob.wait_for_log(22., |log| {
        log.contains("DEBUG Handling IncludedTorelaysMesh message for peer")
    }))
    .unwrap();

    let electrum = block_on(mm_alice.rpc(&json!({
        "userpass": "pass",
        "method": "electrum",
        "coin": "tBTC",
        "servers": [{"url":"blockstream.info:143"},{"url":"blackie.c3-soft.com:57005"},{"url":"testnet.qtornado.com:51001"}],
        "address_format":{"format":"segwit"},
        "mm2": 1,
    }))).unwrap();
    assert_eq!(
        electrum.0,
        StatusCode::OK,
        "RPC «electrum» failed with {} {}",
        electrum.0,
        electrum.1
    );
    log!({ "enable Alice tBTC: {:?}", electrum });

    let electrum = block_on(mm_alice.rpc(&json!({
        "userpass": "pass",
        "method": "electrum",
        "coin": "DOC",
        "servers": [{"url":"doc.electrum1.cipig.net:10020"},{"url":"doc.electrum2.cipig.net:10020"},{"url":"doc.electrum3.cipig.net:10020"}],
        "mm2": 1,
    }))).unwrap();
    assert_eq!(
        electrum.0,
        StatusCode::OK,
        "RPC «electrum» failed with {} {}",
        electrum.0,
        electrum.1
    );
    log!({ "enable Alice DOC: {:?}", electrum });

    // setting the price will trigger Alice's subscription to the orderbook topic
    // but won't request the actual orderbook
    let rc = block_on(mm_alice.rpc(&json! ({
        "userpass": mm_alice.userpass,
        "method": "setprice",
        "base": "DOC",
        "rel": "tBTC",
        "price": "1",
        "volume": "0.1",
        "cancel_previous": false,
    })))
    .unwrap();
    assert!(rc.0.is_success(), "!setprice: {}", rc.1);

    block_on(
        mm_alice.wait_for_log((MIN_ORDER_KEEP_ALIVE_INTERVAL * 2) as f64, |log| {
            log.contains(&format!("Inserting order OrderbookItem {{ pubkey: \"{}\"", bob_pubkey))
        }),
    )
    .unwrap();

    // checking orderbook on alice side
    let rc = block_on(mm_alice.rpc(&json! ({
        "userpass": mm_alice.userpass,
        "method": "orderbook",
        "base": "tBTC",
        "rel": "DOC",
    })))
    .unwrap();
    assert!(rc.0.is_success(), "!orderbook: {}", rc.1);
    let response: OrderbookResponse = json::from_str(&rc.1).unwrap();
    assert_eq!(response.asks[0].address, tbtc_segwit_address);
    assert_eq!(response.bids[0].address, doc_address);

    block_on(mm_bob.stop()).unwrap();
    block_on(mm_alice.stop()).unwrap();
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn test_orderbook_segwit() {
    let bob_passphrase = get_passphrase(&".env.seed", "BOB_PASSPHRASE").unwrap();

    let bob_coins_config = json!([
        {"coin":"DOC","asset":"DOC","rpcport":62415,"txversion":4,"overwintered":1,"protocol":{"type":"UTXO"}},
        {"coin":"tBTC","name":"tbitcoin","fname":"tBitcoin","rpcport":18332,"pubtype":111,"p2shtype":196,"wiftype":239,"segwit":true,"bech32_hrp":"tb","txfee":0,"estimate_fee_mode":"ECONOMICAL","mm2":1,"required_confirmations":0,"protocol":{"type":"UTXO"},"address_format":{"format":"segwit"}}
    ]);

    let alice_coins_config = json!([
        {"coin":"DOC","asset":"DOC","rpcport":62415,"txversion":4,"overwintered":1,"protocol":{"type":"UTXO"}},
        {"coin":"tBTC","name":"tbitcoin","fname":"tBitcoin","rpcport":18332,"pubtype":111,"p2shtype":196,"wiftype":239,"segwit":true,"bech32_hrp":"tb","txfee":0,"estimate_fee_mode":"ECONOMICAL","mm2":1,"required_confirmations":0,"protocol":{"type":"UTXO"}}
    ]);

    let mut mm_bob = MarketMakerIt::start(
        json! ({
            "gui": "nogui",
            "netid": 9998,
            "myipaddr": env::var ("BOB_TRADE_IP") .ok(),
            "rpcip": env::var ("BOB_TRADE_IP") .ok(),
            "canbind": env::var ("BOB_TRADE_PORT") .ok().map (|s| s.parse::<i64>().unwrap()),
            "passphrase": bob_passphrase,
            "coins": bob_coins_config,
            "rpc_password": "pass",
            "i_am_seed": true,
        }),
        "pass".into(),
        local_start!("bob"),
    )
    .unwrap();
    let (_bob_dump_log, _bob_dump_dashboard) = mm_bob.mm_dump();
    log!({"Bob log path: {}", mm_bob.log_path.display()});

    // Enable coins on Bob side. Print the replies in case we need the "address".
    let electrum = block_on(mm_bob.rpc(&json!({
        "userpass": "pass",
        "method": "electrum",
        "coin": "tBTC",
        "servers": [{"url":"blockstream.info:143"},{"url":"blackie.c3-soft.com:57005"},{"url":"testnet.qtornado.com:51001"}],
        "address_format":{"format":"segwit"},
        "mm2": 1,
    }))).unwrap();
    assert_eq!(
        electrum.0,
        StatusCode::OK,
        "RPC «electrum» failed with {} {}",
        electrum.0,
        electrum.1
    );
    log!({ "enable tBTC: {:?}", electrum });
    let enable_tbtc_res: EnableElectrumResponse = json::from_str(&electrum.1).unwrap();
    let tbtc_segwit_address = enable_tbtc_res.address;

    let electrum = block_on(mm_bob.rpc(&json!({
        "userpass": "pass",
        "method": "electrum",
        "coin": "DOC",
        "servers": [{"url":"doc.electrum1.cipig.net:10020"},{"url":"doc.electrum2.cipig.net:10020"},{"url":"doc.electrum3.cipig.net:10020"}],
        "mm2": 1,
    }))).unwrap();
    assert_eq!(
        electrum.0,
        StatusCode::OK,
        "RPC «electrum» failed with {} {}",
        electrum.0,
        electrum.1
    );
    log!({ "enable DOC: {:?}", electrum });
    let enable_doc_res: Json = json::from_str(&electrum.1).unwrap();
    let doc_address = enable_doc_res["address"].as_str().unwrap();

    // issue sell request on Bob side by setting base/rel price
    log!("Issue bob sell requests");

    let bob_orders = [
        // (base, rel, price, volume, min_volume)
        ("tBTC", "DOC", "0.7", "0.0002", Some("0.00015")),
        ("DOC", "tBTC", "0.7", "0.0002", Some("0.00015")),
    ];
    for (base, rel, price, volume, min_volume) in bob_orders.iter() {
        let rc = block_on(mm_bob.rpc(&json! ({
            "userpass": mm_bob.userpass,
            "method": "setprice",
            "base": base,
            "rel": rel,
            "price": price,
            "volume": volume,
            "min_volume": min_volume.unwrap_or("0.00777"),
            "cancel_previous": false,
        })))
        .unwrap();
        assert!(rc.0.is_success(), "!setprice: {}", rc.1);
    }

    let mm_alice = MarketMakerIt::start(
        json! ({
            "gui": "nogui",
            "netid": 9998,
            "myipaddr": env::var ("ALICE_TRADE_IP") .ok(),
            "rpcip": env::var ("ALICE_TRADE_IP") .ok(),
            "passphrase": "alice passphrase",
            "coins": alice_coins_config,
            "seednodes": [fomat!((mm_bob.ip))],
            "rpc_password": "pass",
        }),
        "pass".into(),
        local_start!("alice"),
    )
    .unwrap();

    let (_alice_dump_log, _alice_dump_dashboard) = mm_alice.mm_dump();
    log!({ "Alice log path: {}", mm_alice.log_path.display() });

    block_on(mm_bob.wait_for_log(22., |log| {
        log.contains("DEBUG Handling IncludedTorelaysMesh message for peer")
    }))
    .unwrap();

    // checking orderbook on alice side
    let rc = block_on(mm_alice.rpc(&json! ({
        "userpass": mm_alice.userpass,
        "method": "orderbook",
        "base": "tBTC",
        "rel": "DOC",
    })))
    .unwrap();
    assert!(rc.0.is_success(), "!orderbook: {}", rc.1);
    let response: OrderbookResponse = json::from_str(&rc.1).unwrap();
    assert_eq!(response.asks[0].address, tbtc_segwit_address);
    assert_eq!(response.bids[0].address, doc_address);

    block_on(mm_bob.stop()).unwrap();
    block_on(mm_alice.stop()).unwrap();
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
// https://github.com/KomodoPlatform/atomicDEX-API/issues/859
fn orderbook_extended_data() {
    let coins = json!([
        {"coin":"DOC","asset":"DOC","protocol":{"type":"UTXO"}},
        {"coin":"MARTY","asset":"MARTY","protocol":{"type":"UTXO"}},
    ]);

    let mm = MarketMakerIt::start(
        json! ({
            "gui": "nogui",
            "netid": 9998,
            "myipaddr": env::var ("BOB_TRADE_IP") .ok(),
            "rpcip": env::var ("BOB_TRADE_IP") .ok(),
            "canbind": env::var ("BOB_TRADE_PORT") .ok().map (|s| s.parse::<i64>().unwrap()),
            "passphrase": "bob passphrase",
            "coins": coins,
            "rpc_password": "pass",
            "i_am_seed": true,
        }),
        "pass".into(),
        match var("LOCAL_THREAD_MM") {
            Ok(ref e) if e == "bob" => Some(local_start()),
            _ => None,
        },
    )
    .unwrap();
    let (_dump_log, _dump_dashboard) = &mm.mm_dump();
    log!({"Log path: {}", mm.log_path.display()});
    block_on(enable_electrum(&mm, "DOC", false, &[
        "doc.electrum3.cipig.net:10020",
        "doc.electrum2.cipig.net:10020",
        "doc.electrum1.cipig.net:10020",
    ]));
    block_on(enable_electrum(&mm, "MARTY", false, &[
        "marty.electrum3.cipig.net:10021",
        "marty.electrum2.cipig.net:10021",
        "marty.electrum1.cipig.net:10021",
    ]));

    let bob_orders = &[
        // (base, rel, price, volume)
        ("DOC", "MARTY", "0.9", "0.9"),
        ("DOC", "MARTY", "0.8", "0.9"),
        ("DOC", "MARTY", "0.7", "0.9"),
        ("MARTY", "DOC", "0.8", "0.9"),
        ("MARTY", "DOC", "1", "0.9"),
    ];

    for (base, rel, price, volume) in bob_orders {
        let rc = block_on(mm.rpc(&json!({
            "userpass": mm.userpass,
            "method": "setprice",
            "base": base,
            "rel": rel,
            "price": price,
            "volume": volume,
            "cancel_previous": false,
        })))
        .unwrap();
        assert!(rc.0.is_success(), "!setprice: {}", rc.1);
    }

    thread::sleep(Duration::from_secs(1));
    log!("Get DOC/MARTY orderbook");
    let rc = block_on(mm.rpc(&json! ({
        "userpass": mm.userpass,
        "method": "orderbook",
        "base": "DOC",
        "rel": "MARTY",
    })))
    .unwrap();
    assert!(rc.0.is_success(), "!orderbook: {}", rc.1);

    let orderbook: OrderbookResponse = json::from_str(&rc.1).unwrap();
    log!("orderbook "[rc.1]);
    let expected_total_asks_base_vol = MmNumber::from("2.7");
    assert_eq!(expected_total_asks_base_vol.to_decimal(), orderbook.total_asks_base_vol);

    let expected_total_bids_base_vol = MmNumber::from("1.62");
    assert_eq!(expected_total_bids_base_vol.to_decimal(), orderbook.total_bids_base_vol);

    let expected_total_asks_rel_vol = MmNumber::from("2.16");
    assert_eq!(expected_total_asks_rel_vol.to_decimal(), orderbook.total_asks_rel_vol);

    let expected_total_bids_rel_vol = MmNumber::from("1.8");
    assert_eq!(expected_total_bids_rel_vol.to_decimal(), orderbook.total_bids_rel_vol);

    fn check_price_and_vol_aggr(
        order: &OrderbookEntryAggregate,
        price: &'static str,
        base_aggr: &'static str,
        rel_aggr: &'static str,
    ) {
        let price = MmNumber::from(price);
        assert_eq!(price.to_decimal(), order.price);

        let base_aggr = MmNumber::from(base_aggr);
        assert_eq!(base_aggr.to_decimal(), order.base_max_volume_aggr);

        let rel_aggr = MmNumber::from(rel_aggr);
        assert_eq!(rel_aggr.to_decimal(), order.rel_max_volume_aggr);
    }

    check_price_and_vol_aggr(&orderbook.asks[0], "0.9", "2.7", "2.16");
    check_price_and_vol_aggr(&orderbook.asks[1], "0.8", "1.8", "1.35");
    check_price_and_vol_aggr(&orderbook.asks[2], "0.7", "0.9", "0.63");

    check_price_and_vol_aggr(&orderbook.bids[0], "1.25", "0.72", "0.9");
    check_price_and_vol_aggr(&orderbook.bids[1], "1", "1.62", "1.8");
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
fn orderbook_should_display_base_rel_volumes() {
    let coins = json!([
        {"coin":"DOC","asset":"DOC","protocol":{"type":"UTXO"}},
        {"coin":"MARTY","asset":"MARTY","protocol":{"type":"UTXO"}},
    ]);

    let mm = MarketMakerIt::start(
        json! ({
            "gui": "nogui",
            "netid": 9998,
            "myipaddr": env::var ("BOB_TRADE_IP") .ok(),
            "rpcip": env::var ("BOB_TRADE_IP") .ok(),
            "canbind": env::var ("BOB_TRADE_PORT") .ok().map (|s| s.parse::<i64>().unwrap()),
            "passphrase": "bob passphrase",
            "coins": coins,
            "rpc_password": "pass",
            "i_am_seed": true,
        }),
        "pass".into(),
        match var("LOCAL_THREAD_MM") {
            Ok(ref e) if e == "bob" => Some(local_start()),
            _ => None,
        },
    )
    .unwrap();
    let (_dump_log, _dump_dashboard) = &mm.mm_dump();
    log!({"Log path: {}", mm.log_path.display()});
    block_on(enable_electrum(&mm, "DOC", false, &[
        "doc.electrum3.cipig.net:10020",
        "doc.electrum2.cipig.net:10020",
        "doc.electrum1.cipig.net:10020",
    ]));
    block_on(enable_electrum(&mm, "MARTY", false, &[
        "marty.electrum3.cipig.net:10021",
        "marty.electrum2.cipig.net:10021",
        "marty.electrum1.cipig.net:10021",
    ]));

    let price = BigRational::new(2.into(), 1.into());
    let volume = BigRational::new(1.into(), 1.into());

    // create order with rational amount and price
    let rc = block_on(mm.rpc(&json! ({
        "userpass": mm.userpass,
        "method": "setprice",
        "base": "DOC",
        "rel": "MARTY",
        "price": price,
        "volume": volume,
        "cancel_previous": false,
    })))
    .unwrap();
    assert!(rc.0.is_success(), "!setprice: {}", rc.1);

    thread::sleep(Duration::from_secs(1));
    log!("Get DOC/MARTY orderbook");
    let rc = block_on(mm.rpc(&json! ({
        "userpass": mm.userpass,
        "method": "orderbook",
        "base": "DOC",
        "rel": "MARTY",
    })))
    .unwrap();
    assert!(rc.0.is_success(), "!orderbook: {}", rc.1);

    let orderbook: OrderbookResponse = json::from_str(&rc.1).unwrap();
    log!("orderbook "[orderbook]);
    assert_eq!(orderbook.asks.len(), 1, "DOC/MARTY orderbook must have exactly 1 ask");
    let min_volume = BigRational::new(1.into(), 10000.into());
    assert_eq!(volume, orderbook.asks[0].base_max_volume_rat);
    assert_eq!(min_volume, orderbook.asks[0].base_min_volume_rat);

    assert_eq!(&volume * &price, orderbook.asks[0].rel_max_volume_rat);
    assert_eq!(&min_volume * &price, orderbook.asks[0].rel_min_volume_rat);

    log!("Get MARTY/DOC orderbook");
    let rc = block_on(mm.rpc(&json! ({
        "userpass": mm.userpass,
        "method": "orderbook",
        "base": "MARTY",
        "rel": "DOC",
    })))
    .unwrap();
    assert!(rc.0.is_success(), "!orderbook: {}", rc.1);

    let orderbook: OrderbookResponse = json::from_str(&rc.1).unwrap();
    log!("orderbook "[orderbook]);
    assert_eq!(orderbook.bids.len(), 1, "MARTY/DOC orderbook must have exactly 1 bid");
    let min_volume = BigRational::new(1.into(), 10000.into());
    assert_eq!(volume, orderbook.bids[0].rel_max_volume_rat);
    assert_eq!(min_volume, orderbook.bids[0].rel_min_volume_rat);

    assert_eq!(&volume * &price, orderbook.bids[0].base_max_volume_rat);
    assert_eq!(&min_volume * &price, orderbook.bids[0].base_min_volume_rat);
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
// https://github.com/KomodoPlatform/atomicDEX-API/issues/670
fn orderbook_should_work_without_coins_activation() {
    let bob_passphrase = get_passphrase(&".env.seed", "BOB_PASSPHRASE").unwrap();

    let coins = json! ([
        {"coin":"DOC","asset":"DOC","rpcport":62415,"txversion":4,"overwintered":1,"protocol":{"type":"UTXO"}},
        {"coin":"MARTY","asset":"MARTY","rpcport":52592,"txversion":4,"overwintered":1,"protocol":{"type":"UTXO"}},
        {"coin":"ETH","name":"ethereum","protocol":{"type":"ETH"}},
        {"coin":"JST","name":"jst","protocol":{"type":"ERC20","protocol_data":{"platform":"ETH","contract_address":"0x2b294F029Fde858b2c62184e8390591755521d8E"}}}
    ]);

    let mm_bob = MarketMakerIt::start(
        json! ({
            "gui": "nogui",
            "netid": 9998,
            "myipaddr": env::var ("BOB_TRADE_IP") .ok(),
            "rpcip": env::var ("BOB_TRADE_IP") .ok(),
            "canbind": env::var ("BOB_TRADE_PORT") .ok().map (|s| s.parse::<i64>().unwrap()),
            "passphrase": bob_passphrase,
            "coins": coins,
            "rpc_password": "pass",
            "i_am_seed": true,
        }),
        "pass".into(),
        match var("LOCAL_THREAD_MM") {
            Ok(ref e) if e == "bob" => Some(local_start()),
            _ => None,
        },
    )
    .unwrap();

    let (_dump_log, _dump_dashboard) = mm_bob.mm_dump();
    log!({"Bob log path: {}", mm_bob.log_path.display()});

    let mm_alice = MarketMakerIt::start(
        json! ({
            "gui": "nogui",
            "netid": 9998,
            "dht": "on",  // Enable DHT without delay.
            "myipaddr": env::var ("ALICE_TRADE_IP") .ok(),
            "rpcip": env::var ("ALICE_TRADE_IP") .ok(),
            "passphrase": "alice passphrase",
            "coins": coins,
            "seednodes": [fomat!((mm_bob.ip))],
            "rpc_password": "pass",
        }),
        "pass".into(),
        match var("LOCAL_THREAD_MM") {
            Ok(ref e) if e == "alice" => Some(local_start()),
            _ => None,
        },
    )
    .unwrap();

    let (_alice_dump_log, _alice_dump_dashboard) = mm_alice.mm_dump();
    log!({"Alice log path: {}", mm_alice.log_path.display()});

    log! ({"enable_coins (bob): {:?}", block_on (enable_coins_eth_electrum_doc_marty (&mm_bob, &["http://195.201.0.6:8565"]))});

    let rc = block_on(mm_bob.rpc(&json! ({
        "userpass": mm_bob.userpass,
        "method": "setprice",
        "base": "ETH",
        "rel": "JST",
        "price": "1",
        "volume": "10",
        "min_volume": "1",
    })))
    .unwrap();
    assert!(rc.0.is_success(), "!setprice: {}", rc.1);

    log!("Get ETH/JST orderbook on Alice side");
    let rc = block_on(mm_alice.rpc(&json! ({
        "userpass": mm_alice.userpass,
        "method": "orderbook",
        "base": "ETH",
        "rel": "JST",
    })))
    .unwrap();
    assert!(rc.0.is_success(), "!orderbook: {}", rc.1);

    let orderbook: Json = json::from_str(&rc.1).unwrap();
    log!("orderbook "[orderbook]);
    let asks = orderbook["asks"].as_array().unwrap();
    assert_eq!(asks.len(), 1, "Alice ETH/JST orderbook must have exactly 1 ask");
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
// https://github.com/KomodoPlatform/atomicDEX-API/issues/511
fn test_all_orders_per_pair_per_node_must_be_displayed_in_orderbook() {
    let coins = json!([
        {"coin":"DOC","asset":"DOC","protocol":{"type":"UTXO"}},
        {"coin":"MARTY","asset":"MARTY","protocol":{"type":"UTXO"}},
    ]);

    let mm = MarketMakerIt::start(
        json! ({
            "gui": "nogui",
            "netid": 9998,
            "myipaddr": env::var ("BOB_TRADE_IP") .ok(),
            "rpcip": env::var ("BOB_TRADE_IP") .ok(),
            "canbind": env::var ("BOB_TRADE_PORT") .ok().map (|s| s.parse::<i64>().unwrap()),
            "passphrase": "bob passphrase",
            "coins": coins,
            "rpc_password": "pass",
            "i_am_seed": true,
        }),
        "pass".into(),
        match var("LOCAL_THREAD_MM") {
            Ok(ref e) if e == "bob" => Some(local_start()),
            _ => None,
        },
    )
    .unwrap();
    let (_dump_log, _dump_dashboard) = mm.mm_dump();
    log!({"Log path: {}", mm.log_path.display()});
    block_on(enable_electrum(&mm, "DOC", false, &[
        "doc.electrum3.cipig.net:10020",
        "doc.electrum2.cipig.net:10020",
        "doc.electrum1.cipig.net:10020",
    ]));
    block_on(enable_electrum(&mm, "MARTY", false, &[
        "marty.electrum3.cipig.net:10021",
        "marty.electrum2.cipig.net:10021",
        "marty.electrum1.cipig.net:10021",
    ]));

    // set 2 orders with different prices
    let rc = block_on(mm.rpc(&json! ({
        "userpass": mm.userpass,
        "method": "setprice",
        "base": "DOC",
        "rel": "MARTY",
        "price": 0.9,
        "volume": "0.9",
        "cancel_previous": false,
    })))
    .unwrap();
    assert!(rc.0.is_success(), "!setprice: {}", rc.1);

    let rc = block_on(mm.rpc(&json! ({
        "userpass": mm.userpass,
        "method": "setprice",
        "base": "DOC",
        "rel": "MARTY",
        "price": 1,
        "volume": "0.9",
        "cancel_previous": false,
    })))
    .unwrap();
    assert!(rc.0.is_success(), "!setprice: {}", rc.1);

    thread::sleep(Duration::from_secs(2));

    log!("Get DOC/MARTY orderbook");
    let rc = block_on(mm.rpc(&json! ({
        "userpass": mm.userpass,
        "method": "orderbook",
        "base": "DOC",
        "rel": "MARTY",
    })))
    .unwrap();
    assert!(rc.0.is_success(), "!orderbook: {}", rc.1);

    let orderbook: Json = json::from_str(&rc.1).unwrap();
    log!("orderbook "[orderbook]);
    let asks = orderbook["asks"].as_array().unwrap();
    assert_eq!(asks.len(), 2, "DOC/MARTY orderbook must have exactly 2 asks");
}

#[test]
#[cfg_attr(not(feature = "live-network-tests"), ignore = "needs public servers")]
// https://github.com/KomodoPlatform/atomicDEX-API/issues/473
fn setprice_min_volume_should_be_displayed_in_orderbook() {
    let bob_passphrase = get_passphrase(&".env.seed", "BOB_PASSPHRASE").unwrap();

    let coins = json! ([
        {"coin":"DOC","asset":"DOC","rpcport":62415,"txversion":4,"overwintered":1,"protocol":{"type":"UTXO"}},
        {"coin":"MARTY","asset":"MARTY","rpcport":52592,"txversion":4,"overwintered":1,"protocol":{"type":"UTXO"}},
        {"coin":"ETH","name":"ethereum","protocol":{"type":"ETH"}},
        {"coin":"JST","name":"jst","protocol":{"type":"ERC20","protocol_data":{"platform":"ETH","contract_address":"0x2b294F029Fde858b2c62184e8390591755521d8E"}}}
    ]);

    let mm_bob = MarketMakerIt::start(
        json! ({
            "gui": "nogui",
            "netid": 9998,
            "myipaddr": env::var ("BOB_TRADE_IP") .ok(),
            "rpcip": env::var ("BOB_TRADE_IP") .ok(),
            "canbind": env::var ("BOB_TRADE_PORT") .ok().map (|s| s.parse::<i64>().unwrap()),
            "passphrase": bob_passphrase,
            "coins": coins,
            "rpc_password": "pass",
            "i_am_seed": true,
        }),
        "pass".into(),
        match var("LOCAL_THREAD_MM") {
            Ok(ref e) if e == "bob" => Some(local_start()),
            _ => None,
        },
    )
    .unwrap();

    let (_dump_log, _dump_dashboard) = mm_bob.mm_dump();
    log!({"Bob log path: {}", mm_bob.log_path.display()});

    let mm_alice = MarketMakerIt::start(
        json! ({
            "gui": "nogui",
            "netid": 9998,
            "dht": "on",  // Enable DHT without delay.
            "myipaddr": env::var ("ALICE_TRADE_IP") .ok(),
            "rpcip": env::var ("ALICE_TRADE_IP") .ok(),
            "passphrase": "alice passphrase",
            "coins": coins,
            "seednodes": [fomat!((mm_bob.ip))],
            "rpc_password": "pass",
        }),
        "pass".into(),
        match var("LOCAL_THREAD_MM") {
            Ok(ref e) if e == "alice" => Some(local_start()),
            _ => None,
        },
    )
    .unwrap();

    let (_alice_dump_log, _alice_dump_dashboard) = mm_alice.mm_dump();
    log!({"Alice log path: {}", mm_alice.log_path.display()});

    log! ({"enable_coins (bob): {:?}", block_on (enable_coins_eth_electrum_doc_marty (&mm_bob, &["http://195.201.0.6:8565"]))});
    log! ({"enable_coins (alice): {:?}", block_on (enable_coins_eth_electrum_doc_marty (&mm_alice, &["http://195.201.0.6:8565"]))});

    // issue orderbook call on Alice side to trigger subscription to a topic
    block_on(mm_alice.rpc(&json! ({
        "userpass": mm_alice.userpass,
        "method": "orderbook",
        "base": "ETH",
        "rel": "JST",
    })))
    .unwrap();

    let rc = block_on(mm_bob.rpc(&json! ({
        "userpass": mm_bob.userpass,
        "method": "setprice",
        "base": "ETH",
        "rel": "JST",
        "price": "1",
        "volume": "10",
        "min_volume": "1",
    })))
    .unwrap();
    assert!(rc.0.is_success(), "!setprice: {}", rc.1);

    thread::sleep(Duration::from_secs(2));
    log!("Get ETH/JST orderbook on Bob side");
    let rc = block_on(mm_bob.rpc(&json! ({
        "userpass": mm_bob.userpass,
        "method": "orderbook",
        "base": "ETH",
        "rel": "JST",
    })))
    .unwrap();
    assert!(rc.0.is_success(), "!orderbook: {}", rc.1);

    let orderbook: Json = json::from_str(&rc.1).unwrap();
    log!("orderbook "[orderbook]);
    let asks = orderbook["asks"].as_array().unwrap();
    assert_eq!(asks.len(), 1, "Bob ETH/JST orderbook must have exactly 1 ask");

    let min_volume = asks[0]["min_volume"].as_str().unwrap();
    assert_eq!(min_volume, "1", "Bob ETH/JST ask must display correct min_volume");

    log!("Get ETH/JST orderbook on Alice side");
    let rc = block_on(mm_alice.rpc(&json! ({
        "userpass": mm_alice.userpass,
        "method": "orderbook",
        "base": "ETH",
        "rel": "JST",
    })))
    .unwrap();
    assert!(rc.0.is_success(), "!orderbook: {}", rc.1);

    let orderbook: Json = json::from_str(&rc.1).unwrap();
    log!("orderbook "[orderbook]);
    let asks = orderbook["asks"].as_array().unwrap();
    assert_eq!(asks.len(), 1, "Alice ETH/JST orderbook must have exactly 1 ask");

    let min_volume = asks[0]["min_volume"].as_str().unwrap();
    assert_eq!(min_volume, "1", "Alice ETH/JST ask must display correct min_volume");
}
