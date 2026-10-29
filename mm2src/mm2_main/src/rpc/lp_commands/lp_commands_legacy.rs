/******************************************************************************
 * Copyright © 2014-2018 The SuperNET Developers.                             *
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
//  rpc_commands.rs
//  marketmaker
//

use coins::{disable_coin as disable_coin_impl, lp_coinfind, lp_coininit, CoinBalance, CoinsContext, MmCoinEnum};
use common::custom_futures::FutureTimerExt;
use common::executor::{spawn, Timer};
use common::log::{error, warn};
use common::mm_number::BigDecimal;
use common::{rpc_err_response, rpc_response, HyRes};
use futures::compat::Future01CompatExt;
use http::Response;
use mm2_core::mm_ctx::MmArc;
use mm2_metrics::MetricsOps;
use serde_json::{self as json, Value as Json};
use std::borrow::Cow;
use std::future::Future;

use crate::mm2::lp_dispatcher::{dispatch_lp_event, StopCtxEvent};
use crate::mm2::lp_network::{peer_connection_healthcheck as peer_connection_healthcheck_impl, subscribe_to_topic};
use crate::mm2::lp_ordermatch::{cancel_orders_by, CancelBy};
use crate::mm2::lp_swap::{active_swaps_using_coin, tx_helper_topic, watcher_topic};
use crate::mm2::MmVersionResult;

/// Attempts to disable the coin
pub async fn disable_coin(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let ticker = try_s!(req["coin"].as_str().ok_or("No 'coin' field")).to_owned();
    let _coin = match lp_coinfind(&ctx, &ticker).await {
        Ok(Some(t)) => t,
        Ok(None) => return ERR!("No such coin: {}", ticker),
        Err(err) => return ERR!("!lp_coinfind({}): ", err),
    };
    let swaps = try_s!(active_swaps_using_coin(&ctx, &ticker));
    if !swaps.is_empty() {
        let err = json!({
            "error": fomat! ("There're active swaps using " (ticker)),
            "swaps": swaps,
        });
        return Response::builder()
            .status(500)
            .body(json::to_vec(&err).unwrap())
            .map_err(|e| ERRL!("{}", e));
    }
    let (cancelled, still_matching) = try_s!(cancel_orders_by(&ctx, CancelBy::Coin { ticker: ticker.clone() }).await);
    if !still_matching.is_empty() {
        let err = json!({
            "error": fomat! ("There're currently matching orders using " (ticker)),
            "orders": {
                "matching": still_matching,
                "cancelled": cancelled,
            }
        });
        return Response::builder()
            .status(500)
            .body(json::to_vec(&err).unwrap())
            .map_err(|e| ERRL!("{}", e));
    }

    try_s!(disable_coin_impl(&ctx, &ticker).await);
    let res = json!({
        "result": {
            "coin": ticker,
            "cancelled_orders": cancelled,
        }
    });
    Response::builder()
        .body(json::to_vec(&res).unwrap())
        .map_err(|e| ERRL!("{}", e))
}

// Docs vs reference (CRD ch.55 R55.4.5, public-docs erratum): some published examples of the legacy `enable`
// and `electrum` answers show a `locked_by_swaps` field, or omit `coin` / `unspendable_balance`. The field
// table and the reference release emit exactly the fields below, and so does Reloaded, so a client written
// from either source sees the fields it can rely on.
#[derive(Serialize)]
struct CoinInitResponse<'a> {
    result: &'a str,
    address: String,
    balance: BigDecimal,
    unspendable_balance: BigDecimal,
    coin: &'a str,
    required_confirmations: u64,
    requires_notarization: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    mature_confirmations: Option<u32>,
}

/// How long `electrum` waits for the balance of a freshly registered coin (CRD ch.55 R55.4.6).
const ELECTRUM_BALANCE_READ_TIMEOUT_SECS: f64 = 20.;

/// The `MM2.json` key of the compatibility switch for a failed balance read in `enable`.
const ENABLE_KEEPS_COIN_ON_BALANCE_FAILURE_KEY: &str = "legacy_enable_keeps_coin_on_balance_failure";

/// Whether `enable` leaves its coin registered when the balance read fails (CRD ch.55 R55.4.6).
///
/// Docs vs reference: the public reference does not say what `enable` does when its balance read fails.
/// The reference release deregisters the coin in `electrum` but leaves it registered in `enable`, so a
/// retry of a failed `enable` then fails as "already initialized" although the first call reported an
/// error. Reloaded deliberately deregisters in both methods (maintainer decision 2026-10-11), so that a
/// retry succeeds; no wire field changes. Setting this switch to `true` restores the reference behavior
/// for `enable` only (`electrum` is not affected). It exists to make the reference's inconsistency
/// reproducible and is not recommended.
fn enable_keeps_coin_on_balance_failure(ctx: &MmArc) -> bool {
    ctx.conf[ENABLE_KEEPS_COIN_ON_BALANCE_FAILURE_KEY].as_bool() == Some(true)
}

/// Reads the balance of a coin that this call has just registered.
///
/// When the read fails, or does not finish within `bound_secs`, the coin is deregistered before the error
/// is returned, so that a retry can succeed, unless `keep_coin_on_failure` is set (CRD ch.55 R55.4.6).
/// Only a coin that this call registered is ever deregistered, because the caller reaches here only after
/// its own registration succeeded; a call that lost the registration race fails earlier and never gets
/// here (R55.4.9).
async fn balance_of_registered_coin<Deregister, DeregisterFut>(
    read_balance: impl Future<Output = Result<CoinBalance, String>>,
    bound_secs: Option<f64>,
    keep_coin_on_failure: bool,
    deregister: Deregister,
) -> Result<CoinBalance, String>
where
    Deregister: FnOnce() -> DeregisterFut,
    DeregisterFut: Future<Output = ()>,
{
    let read_balance = Box::pin(read_balance);
    let result = match bound_secs {
        Some(secs) => match read_balance.timeout_secs(secs).await {
            Ok(result) => result,
            Err(_) => Err(format!("The balance was not received within {} seconds", secs)),
        },
        None => read_balance.await,
    };
    if result.is_err() && !keep_coin_on_failure {
        deregister().await;
    }
    result
}

async fn deregister_coin_after_failed_balance_read(ctx: &MmArc, ticker: &str) {
    if let Err(e) = disable_coin_impl(ctx, ticker).await {
        warn!("Cannot deregister '{}' after its balance read failed: {}", ticker, e);
    }
}

async fn legacy_activation_response(
    ctx: &MmArc,
    coin: &MmCoinEnum,
    bound_secs: Option<f64>,
    keep_coin_on_failure: bool,
) -> Result<Response<Vec<u8>>, String> {
    let ticker = coin.ticker().to_owned();
    let balance = balance_of_registered_coin(
        async { coin.my_balance().compat().await.map_err(|e| e.to_string()) },
        bound_secs,
        keep_coin_on_failure,
        || deregister_coin_after_failed_balance_read(ctx, &ticker),
    )
    .await?;
    let res = CoinInitResponse {
        result: "success",
        address: try_s!(coin.my_address()),
        balance: balance.spendable,
        unspendable_balance: balance.unspendable,
        coin: coin.ticker(),
        required_confirmations: coin.required_confirmations(),
        requires_notarization: coin.requires_notarization(),
        mature_confirmations: coin.mature_confirmations(),
    };
    let res = try_s!(json::to_vec(&res));
    Ok(try_s!(Response::builder().body(res)))
}

/// Enable a coin in the Electrum mode.
pub async fn electrum(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let ticker = try_s!(req["coin"].as_str().ok_or("No 'coin' field")).to_owned();
    // CRD ch.55 R55.1.12: the activation holds the ticker's lock through registration and the balance read, and
    // `lp_coininit` checks again whether the ticker is registered once the lock is ours.
    let _activation_guard = try_s!(try_s!(CoinsContext::from_ctx(&ctx)).lock_activation(&[&ticker]).await);
    let coin: MmCoinEnum = try_s!(lp_coininit(&ctx, &ticker, &req).await);
    // The balance read is bounded, and the coin is deregistered when it fails (CRD ch.55 R55.4.6).
    legacy_activation_response(&ctx, &coin, Some(ELECTRUM_BALANCE_READ_TIMEOUT_SECS), false).await
}

/// Enable a coin in the local wallet mode.
pub async fn enable(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let ticker = try_s!(req["coin"].as_str().ok_or("No 'coin' field")).to_owned();
    // CRD ch.55 R55.1.12: see `electrum`.
    let _activation_guard = try_s!(try_s!(CoinsContext::from_ctx(&ctx)).lock_activation(&[&ticker]).await);
    let coin: MmCoinEnum = try_s!(lp_coininit(&ctx, &ticker, &req).await);
    // `enable` does not bound its balance read, but like `electrum` it deregisters its coin when the read
    // fails, unless the compatibility switch asks for the reference behavior (CRD ch.55 R55.4.6).
    let res = legacy_activation_response(&ctx, &coin, None, enable_keeps_coin_on_balance_failure(&ctx)).await?;

    if coin.is_utxo_in_native_mode() {
        subscribe_to_topic(&ctx, tx_helper_topic(coin.ticker()));
    }

    // Subscribe to watcher messages for coins that support watchers
    if coin.is_supported_by_watchers() {
        subscribe_to_topic(&ctx, watcher_topic(coin.ticker()));
    }

    Ok(res)
}

#[cfg(target_arch = "wasm32")]
pub fn help() -> HyRes {
    rpc_response(
        500,
        json!({
            "error":"'help' is only supported in native mode"
        })
        .to_string(),
    )
}

#[cfg(not(target_arch = "wasm32"))]
pub fn help() -> HyRes {
    rpc_response(
        200,
        "
        buy(base, rel, price, relvolume, timeout=10, duration=3600)
        electrum(coin, urls)
        enable(coin, urls, swap_contract_address)
        myprice(base, rel)
        my_balance(coin)
        my_swap_status(params/uuid)
        orderbook(base, rel, duration=3600)
        sell(base, rel, price, basevolume, timeout=10, duration=3600)
        send_raw_transaction(coin, tx_hex)
        setprice(base, rel, price, broadcast=1)
        stop()
        version
        withdraw(coin, amount, to)
    ",
    )
}

/// Get MarketMaker session metrics
pub fn metrics(ctx: MmArc) -> HyRes {
    match ctx.metrics.collect_json().map(|value| value.to_string()) {
        Ok(response) => rpc_response(200, response),
        Err(err) => rpc_err_response(500, &err),
    }
}

/// Get my_balance of a coin
pub async fn my_balance(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let ticker = try_s!(req["coin"].as_str().ok_or("No 'coin' field")).to_owned();
    let coin = match lp_coinfind(&ctx, &ticker).await {
        Ok(Some(t)) => t,
        Ok(None) => return ERR!("No such coin: {}", ticker),
        Err(err) => return ERR!("!lp_coinfind({}): {}", ticker, err),
    };
    let my_balance = try_s!(coin.my_balance().compat().await);
    let res = json!({
        "coin": ticker,
        "balance": my_balance.spendable,
        "unspendable_balance": my_balance.unspendable,
        "address": try_s!(coin.my_address()),
    });
    let res = try_s!(json::to_vec(&res));
    Ok(try_s!(Response::builder().body(res)))
}

pub async fn stop(ctx: MmArc) -> Result<Response<Vec<u8>>, String> {
    dispatch_lp_event(ctx.clone(), StopCtxEvent.into()).await;
    // Should delay the shutdown a bit in order not to trip the "stop" RPC call in unit tests.
    // Stopping immediately leads to the "stop" RPC call failing with the "errno 10054" sometimes.
    spawn(async move {
        Timer::sleep(0.05).await;
        if let Err(e) = ctx.stop() {
            error!("Error stopping MmCtx: {}", e);
        }
    });
    let res = json!({
        "result": "success"
    });
    let res = try_s!(json::to_vec(&res));
    Ok(try_s!(Response::builder().body(res)))
}

pub async fn sim_panic(req: Json) -> Result<Response<Vec<u8>>, String> {
    #[derive(Deserialize)]
    struct Req {
        #[serde(default)]
        mode: String,
    }
    let req: Req = try_s!(json::from_value(req));

    #[derive(Serialize)]
    struct Ret<'a> {
        /// Supported panic modes.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        modes: Vec<Cow<'a, str>>,
    }
    let ret: Ret;

    if req.mode.is_empty() {
        ret = Ret {
            modes: vec!["simple".into()],
        }
    } else if req.mode == "simple" {
        panic!("sim_panic: simple")
    } else {
        return ERR!("No such mode: {}", req.mode);
    }

    let js = try_s!(json::to_vec(&ret));
    Ok(try_s!(Response::builder().body(js)))
}

pub fn version() -> HyRes { rpc_response(200, MmVersionResult::new().to_json().to_string()) }

pub async fn get_peers_info(ctx: MmArc) -> Result<Response<Vec<u8>>, String> {
    use crate::mm2::lp_network::P2PContext;
    use mm2_p2p::atomicdex_behaviour::get_peers_info;
    let ctx = P2PContext::fetch_from_mm_arc(&ctx);
    let cmd_tx = ctx.cmd_tx.lock().clone();
    let result = get_peers_info(cmd_tx).await;
    let result = json!({
        "result": result,
    });
    let res = try_s!(json::to_vec(&result));
    Ok(try_s!(Response::builder().body(res)))
}

pub async fn get_gossip_mesh(ctx: MmArc) -> Result<Response<Vec<u8>>, String> {
    use crate::mm2::lp_network::P2PContext;
    use mm2_p2p::atomicdex_behaviour::get_gossip_mesh;
    let ctx = P2PContext::fetch_from_mm_arc(&ctx);
    let cmd_tx = ctx.cmd_tx.lock().clone();
    let result = get_gossip_mesh(cmd_tx).await;
    let result = json!({
        "result": result,
    });
    let res = try_s!(json::to_vec(&result));
    Ok(try_s!(Response::builder().body(res)))
}

pub async fn get_gossip_peer_topics(ctx: MmArc) -> Result<Response<Vec<u8>>, String> {
    use crate::mm2::lp_network::P2PContext;
    use mm2_p2p::atomicdex_behaviour::get_gossip_peer_topics;
    let ctx = P2PContext::fetch_from_mm_arc(&ctx);
    let cmd_tx = ctx.cmd_tx.lock().clone();
    let result = get_gossip_peer_topics(cmd_tx).await;
    let result = json!({
        "result": result,
    });
    let res = try_s!(json::to_vec(&result));
    Ok(try_s!(Response::builder().body(res)))
}

pub async fn get_gossip_topic_peers(ctx: MmArc) -> Result<Response<Vec<u8>>, String> {
    use crate::mm2::lp_network::P2PContext;
    use mm2_p2p::atomicdex_behaviour::get_gossip_topic_peers;
    let ctx = P2PContext::fetch_from_mm_arc(&ctx);
    let cmd_tx = ctx.cmd_tx.lock().clone();
    let result = get_gossip_topic_peers(cmd_tx).await;
    let result = json!({
        "result": result,
    });
    let res = try_s!(json::to_vec(&result));
    Ok(try_s!(Response::builder().body(res)))
}

pub async fn get_relay_mesh(ctx: MmArc) -> Result<Response<Vec<u8>>, String> {
    use crate::mm2::lp_network::P2PContext;
    use mm2_p2p::atomicdex_behaviour::get_relay_mesh;
    let ctx = P2PContext::fetch_from_mm_arc(&ctx);
    let cmd_tx = ctx.cmd_tx.lock().clone();
    let result = get_relay_mesh(cmd_tx).await;
    let result = json!({
        "result": result,
    });
    let res = try_s!(json::to_vec(&result));
    Ok(try_s!(Response::builder().body(res)))
}

pub async fn get_my_peer_id(ctx: MmArc) -> Result<Response<Vec<u8>>, String> {
    let peer_id = try_s!(ctx.peer_id.ok_or("Peer ID is not initialized"));
    let result = json!({
        "result": peer_id,
    });
    let res = try_s!(json::to_vec(&result));
    Ok(try_s!(Response::builder().body(res)))
}

#[derive(Deserialize)]
struct PeerConnectionHealthcheckRequest {
    peer_address: String,
}

pub async fn peer_connection_healthcheck(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let req: PeerConnectionHealthcheckRequest = try_s!(json::from_value(req));
    let result = match peer_connection_healthcheck_impl(ctx, req.peer_address).await {
        Ok(result) => result,
        Err(e) => return ERR!("{}", e),
    };
    let res = try_s!(json::to_vec(&json!({ "result": result })));
    Ok(try_s!(Response::builder().body(res)))
}

construct_detailed!(DetailedMinTradingVol, min_trading_vol);

#[derive(Serialize)]
struct MinTradingVolResponse<'a> {
    coin: &'a str,
    #[serde(flatten)]
    volume: DetailedMinTradingVol,
}

/// Get min_trading_vol of a coin
pub async fn min_trading_vol(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let ticker = try_s!(req["coin"].as_str().ok_or("No 'coin' field")).to_owned();
    let coin = match lp_coinfind(&ctx, &ticker).await {
        Ok(Some(t)) => t,
        Ok(None) => return ERR!("No such coin: {}", ticker),
        Err(err) => return ERR!("!lp_coinfind({}): {}", ticker, err),
    };
    let min_trading_vol = coin.min_trading_vol();
    let response = MinTradingVolResponse {
        coin: &ticker,
        volume: min_trading_vol.into(),
    };
    let res = json!({
        "result": response,
    });
    let res = try_s!(json::to_vec(&res));
    Ok(try_s!(Response::builder().body(res)))
}

// AP: Inventory is not documented and not used as of now, commented out
/*
pub fn inventory (ctx: MmArc, req: Json) -> HyRes {
    let ticker = match req["coin"].as_str() {Some (s) => s, None => return rpc_err_response (500, "No 'coin' argument in request")};
    let coin = match lp_coinfind (&ctx, ticker) {
        Ok (Some (t)) => t,
        Ok (None) => return rpc_err_response (500, &fomat! ("No such coin: " (ticker))),
        Err (err) => return rpc_err_response (500, &fomat! ("!lp_coinfind(" (ticker) "): " (err)))
    };
    let ii = coin.iguana_info();

    unsafe {lp::LP_address (ii, (*ii).smartaddr.as_mut_ptr())};
    if unsafe {nonz (lp::G.LP_privkey.bytes)} {
        unsafe {lp::LP_privkey_init (-1, ii, lp::G.LP_privkey, lp::G.LP_mypub25519)};
    } else {
        log! ("inventory] no LP_privkey");
    }
    let retjson = json! ({
        "result": "success",
        "coin": ticker,
        "timestamp": now_ms() / 1000,
        "alice": []  // LP_inventory(coin)
        // "bob": LP_inventory(coin,1)
    });
    //LP_smartutxos_push(ptr);
    rpc_response (200, try_h! (json::to_string (&retjson)))
}
*/

#[cfg(test)]
mod tests {
    use super::*;
    use coins::{lp_register_coin, RegisterCoinParams, TestCoin};
    use common::block_on;
    use mm2_core::mm_ctx::MmCtxBuilder;
    use std::cell::Cell;

    fn ok_balance() -> Result<CoinBalance, String> { Ok(CoinBalance::default()) }

    fn failed_balance() -> Result<CoinBalance, String> { Err("balance backend is down".to_owned()) }

    /// Runs `balance_of_registered_coin` and tells whether it deregistered the coin.
    fn run(
        read: impl Future<Output = Result<CoinBalance, String>>,
        bound_secs: Option<f64>,
        keep_coin_on_failure: bool,
    ) -> (Result<CoinBalance, String>, bool) {
        let deregistered = Cell::new(false);
        let result = block_on(balance_of_registered_coin(
            read,
            bound_secs,
            keep_coin_on_failure,
            || async { deregistered.set(true) },
        ));
        (result, deregistered.get())
    }

    #[test]
    fn a_successful_balance_read_keeps_the_coin() {
        let (result, deregistered) = run(async { ok_balance() }, Some(20.), false);
        assert!(result.is_ok());
        assert!(!deregistered);
    }

    #[test]
    fn a_failed_balance_read_deregisters_the_coin_by_default() {
        // R55.4.6: both `electrum` (bounded) and `enable` (unbounded) deregister.
        for bound in [Some(20.), None] {
            let (result, deregistered) = run(async { failed_balance() }, bound, false);
            assert_eq!(result.unwrap_err(), "balance backend is down");
            assert!(deregistered, "bound {bound:?}");
        }
    }

    #[test]
    fn a_balance_read_past_the_bound_fails_and_deregisters_the_coin() {
        let (result, deregistered) = run(futures::future::pending(), Some(0.05), false);
        assert!(result.unwrap_err().contains("not received"));
        assert!(deregistered);
    }

    #[test]
    fn the_compatibility_switch_keeps_the_coin_registered() {
        let (result, deregistered) = run(async { failed_balance() }, None, true);
        assert!(result.is_err());
        assert!(!deregistered);
    }

    #[test]
    fn the_enable_switch_defaults_to_false_and_needs_a_boolean_true() {
        let ctx = |conf: Json| MmCtxBuilder::new().with_conf(conf).into_mm_arc();
        assert!(!enable_keeps_coin_on_balance_failure(&ctx(json!({}))));
        assert!(!enable_keeps_coin_on_balance_failure(&ctx(
            json!({ "legacy_enable_keeps_coin_on_balance_failure": false })
        )));
        assert!(!enable_keeps_coin_on_balance_failure(&ctx(
            json!({ "legacy_enable_keeps_coin_on_balance_failure": "true" })
        )));
        assert!(enable_keeps_coin_on_balance_failure(&ctx(
            json!({ "legacy_enable_keeps_coin_on_balance_failure": true })
        )));
    }

    #[test]
    fn the_real_deregistration_removes_only_the_named_coin() {
        let ctx = MmCtxBuilder::new().with_conf(json!({})).into_mm_arc();
        for ticker in ["AAA", "BBB"] {
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

        // A failed read of AAA removes AAA, so a retry can register it again, and leaves BBB alone.
        let result = block_on(balance_of_registered_coin(
            async { failed_balance() },
            None,
            false,
            || deregister_coin_after_failed_balance_read(&ctx, "AAA"),
        ));
        assert!(result.is_err());
        assert!(block_on(lp_coinfind(&ctx, "AAA")).unwrap().is_none());
        assert!(block_on(lp_coinfind(&ctx, "BBB")).unwrap().is_some());
        block_on(lp_register_coin(
            &ctx,
            TestCoin::new("AAA").into(),
            RegisterCoinParams {
                ticker: "AAA".to_owned(),
                tx_history: false,
            },
        ))
        .expect("the retry registers the coin again");

        // With the switch the coin stays registered, as in the reference.
        let result = block_on(balance_of_registered_coin(
            async { failed_balance() },
            None,
            true,
            || deregister_coin_after_failed_balance_read(&ctx, "AAA"),
        ));
        assert!(result.is_err());
        assert!(block_on(lp_coinfind(&ctx, "AAA")).unwrap().is_some());
    }

    /// A Sia-protocol coin goes through the legacy `enable` leg (CRD ch.46 R46.6.3): its Sia members are read
    /// from the top level of the request, and a walletd that cannot be reached fails the call through the
    /// legacy error envelope, leaving no coin registered (T46.8.5). The error is not the "protocol is not
    /// supported" refusal the legacy path used to give.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn legacy_enable_of_a_sia_coin_with_an_unreachable_walletd_fails_and_registers_nothing() {
        use crypto::CryptoCtx;

        let ctx = MmCtxBuilder::new()
            .with_conf(json!({
                "coins": [{ "coin": "SC", "mm2": 1, "required_confirmations": 1, "protocol": { "type": "SIA" } }]
            }))
            .into_mm_arc();
        CryptoCtx::init_with_iguana_passphrase(ctx.clone(), "legacy sia enable test passphrase").unwrap();

        let request = json!({
            "method": "enable",
            "coin": "SC",
            // Nothing listens on port 1 of the loopback interface.
            "client_conf": { "server_url": "http://127.0.0.1:1/", "timeout": 2 },
            "tx_history": false,
        });
        let err = block_on(enable(ctx.clone(), request)).unwrap_err();
        assert!(!err.contains("not supported by lp_coininit"), "{err}");
        assert!(block_on(lp_coinfind(&ctx, "SC")).unwrap().is_none());

        // A missing `client_conf` is reported through the same envelope.
        let err = block_on(enable(ctx.clone(), json!({ "method": "enable", "coin": "SC" }))).unwrap_err();
        assert!(!err.contains("not supported by lp_coininit"), "{err}");
        assert!(block_on(lp_coinfind(&ctx, "SC")).unwrap().is_none());
    }

    /// CRD ch.55 T55.21: the legacy `enable` and `electrum` take part in the serialization of same-ticker
    /// activations. A Sia coin whose walletd is unreachable stands in for a coin the legacy path builds: if the
    /// call did not wait for the lock, it would fail at the walletd, not with the "already initialized" answer.
    #[cfg(not(target_arch = "wasm32"))]
    mod serialized_activation {
        use super::*;
        use crypto::CryptoCtx;
        use futures::future::{abortable, AbortHandle, Aborted};
        use std::sync::mpsc;
        use std::time::Duration;

        type LegacyAnswer = Result<Response<Vec<u8>>, String>;

        fn sia_ctx(tickers: &[&str]) -> MmArc {
            let coins: Vec<Json> = tickers
                .iter()
                .map(|ticker| {
                    json!({ "coin": ticker, "mm2": 1, "required_confirmations": 1, "protocol": { "type": "SIA" } })
                })
                .collect();
            let ctx = MmCtxBuilder::new().with_conf(json!({ "coins": coins })).into_mm_arc();
            CryptoCtx::init_with_iguana_passphrase(ctx.clone(), "legacy serialized activation test passphrase")
                .unwrap();
            ctx
        }

        fn request(method: &str, ticker: &str) -> Json {
            // Nothing listens on port 1 of the loopback interface.
            json!({
                "method": method,
                "coin": ticker,
                "client_conf": { "server_url": "http://127.0.0.1:1/", "timeout": 2 },
                "tx_history": false,
            })
        }

        struct Call {
            answer: mpsc::Receiver<Result<LegacyAnswer, Aborted>>,
            abort: AbortHandle,
        }

        impl Call {
            fn start(ctx: &MmArc, method: &'static str, ticker: &str) -> Call {
                let (ctx, request) = (ctx.clone(), request(method, ticker));
                let (call, abort) = abortable(async move {
                    match method {
                        "enable" => enable(ctx, request).await,
                        _ => electrum(ctx, request).await,
                    }
                });
                let (tx, answer) = mpsc::channel();
                std::thread::spawn(move || {
                    let _ = tx.send(block_on(call));
                });
                Call { answer, abort }
            }

            /// The answer, or `None` when the call has not finished within the settle time.
            fn answer_within(&self, wait: Duration) -> Option<Result<LegacyAnswer, Aborted>> {
                self.answer.recv_timeout(wait).ok()
            }
        }

        const SETTLE: Duration = Duration::from_millis(300);
        const LONG: Duration = Duration::from_secs(30);

        fn register(ctx: &MmArc, ticker: &str) {
            block_on(lp_register_coin(
                ctx,
                TestCoin::new(ticker).into(),
                RegisterCoinParams {
                    ticker: ticker.to_owned(),
                    tx_history: false,
                },
            ))
            .unwrap_or_else(|e| panic!("cannot register {ticker}: {e}"));
        }

        fn hold(ctx: &MmArc, ticker: &str) -> coins::activation_locks::ActivationGuard {
            block_on(CoinsContext::from_ctx(ctx).unwrap().lock_activation(&[ticker])).unwrap()
        }

        /// (a) and (c): another activation of the ticker is running; the call waits, and once that activation
        /// has registered the coin the call ends with the legacy "already initialized" error, building nothing.
        #[test]
        fn t55_21_legacy_calls_wait_for_a_running_activation_and_end_already_initialized() {
            for method in ["enable", "electrum"] {
                let ctx = sia_ctx(&["SC"]);
                let running = hold(&ctx, "SC");
                let call = Call::start(&ctx, method, "SC");
                assert!(call.answer_within(SETTLE).is_none(), "{method} did not wait");

                register(&ctx, "SC");
                drop(running);
                let answer = call
                    .answer_within(LONG)
                    .expect("the call finishes")
                    .expect("not aborted");
                let err = answer.unwrap_err();
                assert!(err.contains("already initialized"), "{method}: {err}");
                assert!(block_on(lp_coinfind(&ctx, "SC")).unwrap().is_some());
            }
        }

        /// (b): the activation that held the lock failed and registered nothing; the call proceeds with its own
        /// activation (here it reaches the unreachable walletd, so it does not end "already initialized").
        #[test]
        fn t55_21_legacy_calls_proceed_when_the_running_activation_failed() {
            for method in ["enable", "electrum"] {
                let ctx = sia_ctx(&["SC"]);
                let running = hold(&ctx, "SC");
                let call = Call::start(&ctx, method, "SC");
                assert!(call.answer_within(SETTLE).is_none(), "{method} did not wait");

                drop(running);
                let answer = call
                    .answer_within(LONG)
                    .expect("the call finishes")
                    .expect("not aborted");
                let err = answer.unwrap_err();
                assert!(!err.contains("already initialized"), "{method}: {err}");
                assert!(block_on(lp_coinfind(&ctx, "SC")).unwrap().is_none());
            }
        }

        /// (d): a different ticker does not wait for the held one.
        #[test]
        fn t55_21_legacy_calls_for_different_tickers_do_not_wait() {
            let ctx = sia_ctx(&["SC", "SC2"]);
            let _running = hold(&ctx, "SC");
            let call = Call::start(&ctx, "enable", "SC2");
            let answer = call
                .answer_within(LONG)
                .expect("the call does not wait")
                .expect("not aborted");
            let err = answer.unwrap_err();
            assert!(!err.contains("already initialized"), "{err}");
        }

        /// (e): cancelling a call that is still waiting registers nothing and deregisters nothing.
        #[test]
        fn t55_21_cancelling_a_waiting_legacy_call_registers_and_deregisters_nothing() {
            let ctx = sia_ctx(&["SC"]);
            let running = hold(&ctx, "SC");
            let call = Call::start(&ctx, "enable", "SC");
            assert!(call.answer_within(SETTLE).is_none());
            register(&ctx, "SC");
            call.abort.abort();
            assert!(matches!(call.answer_within(LONG), Some(Err(Aborted))));
            drop(running);
            assert!(
                block_on(lp_coinfind(&ctx, "SC")).unwrap().is_some(),
                "the coin of the holder was removed"
            );

            // The holder failed: the cancelled call must not have gone on to activate the coin.
            let ctx = sia_ctx(&["SC"]);
            let running = hold(&ctx, "SC");
            let call = Call::start(&ctx, "enable", "SC");
            assert!(call.answer_within(SETTLE).is_none());
            call.abort.abort();
            assert!(matches!(call.answer_within(LONG), Some(Err(Aborted))));
            drop(running);
            std::thread::sleep(SETTLE);
            assert!(block_on(lp_coinfind(&ctx, "SC")).unwrap().is_none());
        }
    }

    /// TRON and Solana are postponed for this beta: the legacy `enable` and `electrum` refuse every coin of
    /// those families with the "not available in this build" text and register nothing. The node keeps running.
    #[test]
    fn legacy_enable_and_electrum_refuse_tron_and_solana_coins() {
        use coins::{SOLANA_NOT_AVAILABLE, TRON_NOT_AVAILABLE};
        use crypto::CryptoCtx;

        let mut coins = vec![
            (
                "TRX",
                json!({ "type": "TRX", "protocol_data": { "network": "Mainnet" } }),
                TRON_NOT_AVAILABLE,
            ),
            (
                "USDT-TRC20",
                json!({ "type": "TRC20", "protocol_data": {
                    "platform": "TRX", "contract_address": "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t" } }),
                TRON_NOT_AVAILABLE,
            ),
        ];
        #[cfg(not(target_arch = "wasm32"))]
        {
            coins.push(("SOL", json!({ "type": "SOLANA" }), SOLANA_NOT_AVAILABLE));
            coins.push((
                "USDC-SOL",
                json!({ "type": "SPLTOKEN", "protocol_data": {
                    "platform": "SOL",
                    "token_contract_address": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
                    "decimals": 6 } }),
                SOLANA_NOT_AVAILABLE,
            ));
        }
        let conf_coins: Vec<Json> = coins
            .iter()
            .map(|(ticker, protocol, _)| {
                json!({ "coin": ticker, "mm2": 1, "decimals": 6, "required_confirmations": 1, "protocol": protocol })
            })
            .collect();
        let ctx = MmCtxBuilder::new()
            .with_conf(json!({ "coins": conf_coins }))
            .into_mm_arc();
        CryptoCtx::init_with_iguana_passphrase(ctx.clone(), "legacy tron solana refusal test passphrase").unwrap();

        for (ticker, _, message) in &coins {
            let request = |method: &str| {
                json!({ "method": method, "coin": ticker, "urls": ["https://api.trongrid.io"],
                        "servers": [{ "url": "127.0.0.1:1" }], "tx_history": false })
            };
            let err = block_on(enable(ctx.clone(), request("enable"))).unwrap_err();
            assert!(err.ends_with(message), "enable {ticker}: {err}");
            let err = block_on(electrum(ctx.clone(), request("electrum"))).unwrap_err();
            assert!(err.ends_with(message), "electrum {ticker}: {err}");
            assert!(
                block_on(lp_coinfind(&ctx, ticker)).unwrap().is_none(),
                "{ticker} registered"
            );
        }
    }
}
