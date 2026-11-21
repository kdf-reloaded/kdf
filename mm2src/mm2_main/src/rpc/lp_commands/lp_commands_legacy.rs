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

use coins::utxo::rpc_clients::with_call_budget;
use coins::{disable_coin as disable_coin_impl, disable_coin_of_instance, disable_platform_with_dependents,
            lp_coinfind, lp_coinfind_registered, lp_coininit, CoinBalance, CoinsContext, DisableCoinError, MmCoinEnum,
            MyBalanceRpcError};
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
#[cfg(feature = "sim-panic-rpc")] use std::borrow::Cow;
use std::collections::HashMap;
use std::future::Future;

use crate::mm2::lp_dispatcher::{dispatch_lp_event, StopCtxEvent};
use crate::mm2::lp_network::{peer_connection_healthcheck as peer_connection_healthcheck_impl, subscribe_to_topic};
use crate::mm2::lp_ordermatch::{cancel_orders_by, CancelBy};
use crate::mm2::lp_swap::{active_swaps_using_coin, tx_helper_topic, watcher_topic};
use crate::mm2::rpc::daemon_rpc_errors::{GetMyPeerIdError, InfallibleRpcError, MetricsError};
use crate::mm2::rpc::legacy_adapter::{legacy_bare, legacy_error, legacy_result, legacy_result_body};
use crate::mm2::MmVersionResult;
use mm2_err_handle::prelude::*;
use uuid::Uuid;

/// The request of the versioned `disable_coin` (legacy members, unchanged). The versioned form does not read
/// `force_disable` (the maintainer's decision of 2026-11-18; CRD ch.43 §43.1E, second status note).
#[derive(Deserialize)]
pub struct DisableCoinRequest {
    pub coin: String,
}

/// The success of the versioned `disable_coin`: the content that the legacy body carries under `result`, without
/// `passivized`, which only the legacy form answers.
#[derive(Serialize)]
pub struct DisableCoinResponse {
    pub coin: String,
    pub cancelled_orders: Vec<Uuid>,
}

/// The success of the legacy `disable_coin`, the three members of CRD ch.43 R43.1E.2.
#[derive(Serialize)]
pub struct LegacyDisableCoinResponse {
    pub coin: String,
    pub cancelled_orders: Vec<Uuid>,
    /// True when the coin was made passive instead of being removed (R43.1E.5 item 3).
    pub passivized: bool,
}

/// What a `disable_coin` call does to the requested coin once its checks have passed.
enum DisableCoinPlan {
    /// Remove the requested coin only: a token, a coin without dependents, or the versioned form.
    RemoveOne,
    /// Make the platform passive; nothing is removed (R43.1E.5 item 3).
    Passivize,
    /// Remove the platform and every dependent in one step (R43.1E.5 item 4).
    RemoveWithDependents,
}

/// The swaps that use any of `tickers`, each once.
fn active_swaps_using_any_coin(ctx: &MmArc, tickers: &[String]) -> Result<Vec<Uuid>, MmError<DisableCoinError>> {
    let mut swaps = Vec::new();
    for ticker in tickers {
        for swap in active_swaps_using_coin(ctx, ticker).map_to_mm(DisableCoinError::InternalError)? {
            if !swaps.contains(&swap) {
                swaps.push(swap);
            }
        }
    }
    Ok(swaps)
}

/// The checks and the order cancellation of a `disable_coin` call over `tickers`, in the order of CRD ch.43
/// R43.1E.3 and R43.1E.4: a running swap of any of them refuses the call before anything changes; then the orders of
/// all of them are cancelled, and an order that is being matched refuses the call after the cancellation (the
/// documented cancel-first behavior of D43.1E.1 (c)). `requested` names the ticker in the refusals. Returns the
/// identifiers of the orders cancelled.
async fn check_and_cancel_orders(
    ctx: &MmArc,
    requested: &str,
    tickers: &[String],
) -> Result<Vec<Uuid>, MmError<DisableCoinError>> {
    let swaps = active_swaps_using_any_coin(ctx, tickers)?;
    if !swaps.is_empty() {
        return MmError::err(DisableCoinError::ActiveSwapsBlockDeactivation {
            coin: requested.to_owned(),
            swaps: swaps.iter().map(Uuid::to_string).collect(),
        });
    }
    let mut cancelled: Vec<Uuid> = Vec::new();
    let mut still_matching: Vec<Uuid> = Vec::new();
    for ticker in tickers {
        let (cancelled_now, matching_now) = cancel_orders_by(ctx, CancelBy::Coin { ticker: ticker.clone() })
            .await
            .map_to_mm(DisableCoinError::InternalError)?;
        for uuid in cancelled_now {
            if !cancelled.contains(&uuid) {
                cancelled.push(uuid);
            }
        }
        for uuid in matching_now {
            if !still_matching.contains(&uuid) {
                still_matching.push(uuid);
            }
        }
    }
    if !still_matching.is_empty() {
        return MmError::err(DisableCoinError::MatchingOrdersBlockDeactivation {
            coin: requested.to_owned(),
            matching: still_matching.iter().map(Uuid::to_string).collect(),
            cancelled: cancelled.iter().map(Uuid::to_string).collect(),
        });
    }
    Ok(cancelled)
}

/// The versioned `disable_coin` (CRD ch.43 §43.1R; the maintainer's decision of 2026-11-18): cancels the orders of
/// the coin and removes the coin, unless a running swap uses the coin or one of its orders is being matched. It
/// neither passivizes a platform nor reads `force_disable`: a platform with dependents is removed, its dependents
/// stay registered, and its dependent set is dropped. A passive platform is not an active coin here and answers
/// `NoSuchCoin`.
///
/// The call takes the activation lock of the requested ticker (CRD ch.55 R55.1.12, ch.43 D43.1F.2 (b)). The removal
/// ends the removed instance's background work and its event streams, through `disable_coin_impl` (CRD ch.43
/// R43.1E.5 item 4).
///
/// # Errors
///
/// A [`DisableCoinError`]: `NoSuchCoin`, the two deactivation conflicts (`ActiveSwapsBlockDeactivation`,
/// `MatchingOrdersBlockDeactivation`) and `InternalError`.
pub async fn disable_coin_core(
    ctx: MmArc,
    req: DisableCoinRequest,
) -> Result<DisableCoinResponse, MmError<DisableCoinError>> {
    let ticker = req.coin;
    let coins_ctx = CoinsContext::from_ctx(&ctx).map_to_mm(DisableCoinError::InternalError)?;
    let _activation_guard = coins_ctx
        .lock_activation(&[&ticker])
        .await
        .map_to_mm(DisableCoinError::InternalError)?;
    match lp_coinfind(&ctx, &ticker).await {
        Ok(Some(_)) => (),
        Ok(None) => return MmError::err(DisableCoinError::NoSuchCoin { coin: ticker }),
        // The text is the one of the dev handler, which did not name the ticker.
        Err(err) => return MmError::err(DisableCoinError::InternalError(format!("!lp_coinfind({}): ", err))),
    };
    let cancelled = check_and_cancel_orders(&ctx, &ticker, std::slice::from_ref(&ticker)).await?;
    disable_coin_impl(&ctx, &ticker)
        .await
        .map_to_mm(DisableCoinError::InternalError)?;
    Ok(DisableCoinResponse {
        coin: ticker,
        cancelled_orders: cancelled,
    })
}

/// The typed core of the legacy `disable_coin` (CRD ch.43 §43.1E): the versioned form's checks, plus the passive
/// state and `force_disable`.
///
/// The call takes the activation locks of the requested ticker and, when forced, of every dependent of it, in one
/// `lock_activation` call (R55.1.12, D43.1F.2 (b)); the state is read after the locks are held. Then:
///
/// 1. a ticker that is not registered, or a passive platform that is not forced, is `NoSuchCoin`;
/// 2. an unforced active platform with at least one dependent is made passive after the swap check and the order
///    cancellation of its own ticker (R43.1E.3, R43.1E.4, R43.1E.5 item 3), and the answer is `passivized: true`;
/// 3. a forced platform with at least one dependent, active or passive, is removed with all its dependents after
///    the swap check and the order cancellation have covered the platform and every dependent, and the whole call
///    is refused if any of them is in use (D43.1E.2 (b), R43.1E.5 item 4);
/// 4. anything else (a token, a coin without dependents) is removed as before (R43.1E.5 items 2 and 5).
///
/// The removal ends the background work of each removed instance and its event streams, through
/// `disable_platform_with_dependents` and `disable_coin_impl` (CRD ch.43 R43.1E.5 item 4).
///
/// # Errors
///
/// A [`DisableCoinError`], as [`disable_coin_core`].
pub async fn legacy_disable_coin_core(
    ctx: MmArc,
    ticker: String,
    force_disable: bool,
) -> Result<LegacyDisableCoinResponse, MmError<DisableCoinError>> {
    let coins_ctx = CoinsContext::from_ctx(&ctx).map_to_mm(DisableCoinError::InternalError)?;

    // A forced call locks the dependents too. A dependent can join or leave between the read that names the
    // locks and the moment the locks are held, so the set is read again, and the locks are taken again when it
    // changed. A token activation of this platform holds the platform's lock, so once ours is held the set can
    // only shrink.
    let (_activation_guard, dependents) = loop {
        let named = if force_disable {
            coins_ctx.dependents_of(&ticker).await
        } else {
            Default::default()
        };
        let mut tickers: Vec<&str> = vec![ticker.as_str()];
        tickers.extend(named.iter().map(String::as_str));
        let guard = coins_ctx
            .lock_activation(&tickers)
            .await
            .map_to_mm(DisableCoinError::InternalError)?;
        let dependents = coins_ctx.dependents_of(&ticker).await;
        if !force_disable || dependents == named {
            break (guard, dependents);
        }
    };

    match lp_coinfind_registered(&ctx, &ticker).await {
        Ok(Some(_)) => (),
        Ok(None) => return MmError::err(DisableCoinError::NoSuchCoin { coin: ticker }),
        // The text is the one of the dev handler, which did not name the ticker.
        Err(err) => return MmError::err(DisableCoinError::InternalError(format!("!lp_coinfind({}): ", err))),
    };
    let passive = coins_ctx.is_passive(&ticker).await;
    if passive && !force_disable {
        return MmError::err(DisableCoinError::NoSuchCoin { coin: ticker });
    }

    let plan = match (dependents.is_empty(), force_disable) {
        (false, true) => DisableCoinPlan::RemoveWithDependents,
        (false, false) => DisableCoinPlan::Passivize,
        (true, _) => DisableCoinPlan::RemoveOne,
    };
    let checked: Vec<String> = match plan {
        DisableCoinPlan::RemoveWithDependents => std::iter::once(ticker.clone()).chain(dependents).collect(),
        DisableCoinPlan::RemoveOne | DisableCoinPlan::Passivize => vec![ticker.clone()],
    };
    let cancelled = check_and_cancel_orders(&ctx, &ticker, &checked).await?;

    let passivized = match plan {
        // The set can have emptied since it was read: a dependent token is disabled under its own lock only. The
        // platform then has no dependent, and is removed like any coin without one.
        DisableCoinPlan::Passivize => coins_ctx.make_passive(&ticker).await,
        DisableCoinPlan::RemoveOne | DisableCoinPlan::RemoveWithDependents => false,
    };
    if !passivized {
        match plan {
            DisableCoinPlan::RemoveWithDependents => {
                disable_platform_with_dependents(&ctx, &ticker)
                    .await
                    .map_to_mm(DisableCoinError::InternalError)?;
            },
            DisableCoinPlan::RemoveOne | DisableCoinPlan::Passivize => {
                disable_coin_impl(&ctx, &ticker)
                    .await
                    .map_to_mm(DisableCoinError::InternalError)?;
            },
        }
    }
    Ok(LegacyDisableCoinResponse {
        coin: ticker,
        cancelled_orders: cancelled,
        passivized,
    })
}

/// The orders member of the extended refusal body.
#[derive(Serialize)]
struct LegacyDisableCoinOrders<'a> {
    matching: &'a [String],
    cancelled: &'a [String],
}

/// The extended body of a refused `disable_coin` (CRD ch.43 R43.1E.3 with D43.1E.1 option (c)): four members, all always
/// present, in this order. `active_swaps` and `swaps` hold the same list.
#[derive(Serialize)]
struct LegacyDisableCoinRefusalBody<'a> {
    error: String,
    orders: LegacyDisableCoinOrders<'a>,
    active_swaps: &'a [String],
    swaps: &'a [String],
}

impl<'a> LegacyDisableCoinRefusalBody<'a> {
    fn new(error: String, matching: &'a [String], cancelled: &'a [String], swaps: &'a [String]) -> Self {
        LegacyDisableCoinRefusalBody {
            error,
            orders: LegacyDisableCoinOrders { matching, cancelled },
            active_swaps: swaps,
            swaps,
        }
    }
}

/// The legacy answer of a refused `disable_coin`: HTTP 500 and the extended body of CRD ch.43 R43.1E.3 with D43.1E.1
/// option (c), `{"error", "orders": {"matching", "cancelled"}, "active_swaps", "swaps"}`, for the two deactivation
/// conflicts and for a coin that is not active (empty lists); every other failure is the legacy error text.
#[track_caller]
pub(crate) fn legacy_disable_coin_refusal(error: MmError<DisableCoinError>) -> Result<Response<Vec<u8>>, String> {
    let text = error.get_inner().to_string();
    let body = match error.get_inner() {
        DisableCoinError::ActiveSwapsBlockDeactivation { swaps, .. } => {
            LegacyDisableCoinRefusalBody::new(text, &[], &[], swaps)
        },
        DisableCoinError::MatchingOrdersBlockDeactivation {
            matching, cancelled, ..
        } => LegacyDisableCoinRefusalBody::new(text, matching, cancelled, &[]),
        DisableCoinError::NoSuchCoin { .. } => LegacyDisableCoinRefusalBody::new(text, &[], &[], &[]),
        DisableCoinError::InternalError(_) => return Err(legacy_error(error)),
    };
    let body = try_s!(json::to_vec(&body));
    Response::builder().status(500).body(body).map_err(|e| ERRL!("{}", e))
}

/// Attempts to disable the coin
pub async fn disable_coin(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let coin = try_s!(req["coin"].as_str().ok_or("No 'coin' field")).to_owned();
    // Only the JSON value `true` forces; absent, `false` and any other value do not (CRD ch.43 R43.1E.1).
    let force_disable = req["force_disable"].as_bool() == Some(true);
    match legacy_disable_coin_core(ctx, coin, force_disable).await {
        Ok(response) => legacy_result_body(&response),
        Err(error) => legacy_disable_coin_refusal(error),
    }
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

/// Removes the coin whose balance read failed, if the registered instance of its ticker is `instance` (CRD ch.55
/// R55.4.9). Another instance of the ticker stays registered. `None` removes whatever is registered.
async fn deregister_coin_after_failed_balance_read(ctx: &MmArc, ticker: &str, instance: Option<u64>) {
    if let Err(e) = disable_coin_of_instance(ctx, ticker, instance).await {
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
    // The instance the activation registered: the cleanup of a failed read removes this one only (CRD ch.55 R55.4.9).
    // The activation lock of the ticker is held, so the registered instance is the one that was just built.
    let instance = try_s!(CoinsContext::from_ctx(ctx)).instance_id(&ticker).await;
    // The caller's bound is also the budget of the Electrum calls of the read, so that an Electrum client fails over
    // from a server that does not answer inside that bound (CRD ch.38 R38.6.4j item 1). Other clients ignore it.
    let call_budget = bound_secs.map_or(std::time::Duration::MAX, std::time::Duration::from_secs_f64);
    let balance = balance_of_registered_coin(
        with_call_budget(call_budget, async {
            coin.my_balance().compat().await.map_err(|e| e.to_string())
        }),
        bound_secs,
        keep_coin_on_failure,
        || deregister_coin_after_failed_balance_read(ctx, &ticker, instance),
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

/// The request of a method that takes no members (`version`, `stop`, `metrics`, `get_my_peer_id` and the peer and
/// gossip queries). Any object (members are ignored) and, through the versioned dispatcher, an absent or `null`
/// `params` decode to it.
#[derive(Deserialize)]
pub struct NoParamsRequest {}

/// The typed core of `metrics`: the session metrics, as the JSON the legacy body is made of.
///
/// # Errors
///
/// A [`MetricsError`].
pub async fn metrics_core(ctx: MmArc, _req: NoParamsRequest) -> Result<Json, MmError<MetricsError>> {
    ctx.metrics.collect_json().map_to_mm(MetricsError::Internal)
}

/// The legacy `metrics`: the bare metrics JSON as the whole body (HTTP 200, `application/json`); a failure is
/// `{"error": <text>}` with HTTP 500, logged, as before.
pub async fn metrics(ctx: MmArc) -> Result<Response<Vec<u8>>, String> {
    match metrics_core(ctx, NoParamsRequest {}).await {
        Ok(metrics) => rpc_response(200, metrics.to_string()).compat().await,
        Err(error) => rpc_err_response(500, &error.get_inner().to_string()).compat().await,
    }
}

/// The request of `my_balance` (legacy members, unchanged).
#[derive(Deserialize)]
pub struct MyBalanceRequest {
    pub coin: String,
}

/// The success of `my_balance`: the whole legacy body, a bare object. The members keep the order of the legacy body.
#[derive(Serialize)]
pub struct MyBalanceRpcResponse {
    pub coin: String,
    pub balance: BigDecimal,
    pub unspendable_balance: BigDecimal,
    pub address: String,
    /// The TRON GasFree custody address of `address` (CRD ch.63 §63.3.3): present for a TRON coin whose platform has
    /// a GasFree provider, omitted for every other coin, which leaves the legacy body unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gasfree_address: Option<String>,
}

/// The typed core of `my_balance`: the spendable and the unspendable balance and the address of a coin.
///
/// # Errors
///
/// A [`MyBalanceRpcError`].
pub async fn my_balance_core(
    ctx: MmArc,
    req: MyBalanceRequest,
) -> Result<MyBalanceRpcResponse, MmError<MyBalanceRpcError>> {
    let ticker = req.coin;
    let coin = match lp_coinfind(&ctx, &ticker).await {
        Ok(Some(t)) => t,
        Ok(None) => return MmError::err(MyBalanceRpcError::NoSuchCoin { coin: ticker }),
        Err(err) => {
            return MmError::err(MyBalanceRpcError::InternalError(format!(
                "!lp_coinfind({}): {}",
                ticker, err
            )))
        },
    };
    let balance = coin.my_balance().compat().await.mm_err(MyBalanceRpcError::from)?;
    let address = coin.my_address().map_to_mm(MyBalanceRpcError::InternalError)?;
    // The EVM family alone knows the field; its one report rule decides the value (CRD ch.63 §63.3.3, R18).
    let gasfree_address = match &coin {
        MmCoinEnum::EthCoin(eth_coin) => eth_coin.enabled_address_gasfree().await,
        _ => None,
    };
    Ok(MyBalanceRpcResponse {
        coin: ticker,
        balance: balance.spendable,
        unspendable_balance: balance.unspendable,
        address,
        gasfree_address,
    })
}

/// Get my_balance of a coin
pub async fn my_balance(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let coin = try_s!(req["coin"].as_str().ok_or("No 'coin' field")).to_owned();
    legacy_bare(my_balance_core(ctx, MyBalanceRequest { coin }).await)
}

/// The typed core of `stop`: raises the stop event and schedules the shutdown, which starts shortly after the answer
/// leaves, so the caller of the versioned form gets its answer as the legacy caller does. The success is the text that
/// the legacy body carries under `result`.
pub async fn stop_core(ctx: MmArc, _req: NoParamsRequest) -> Result<&'static str, MmError<InfallibleRpcError>> {
    dispatch_lp_event(ctx.clone(), StopCtxEvent.into()).await;
    // Should delay the shutdown a bit in order not to trip the "stop" RPC call in unit tests.
    // Stopping immediately leads to the "stop" RPC call failing with the "errno 10054" sometimes.
    spawn(async move {
        Timer::sleep(0.05).await;
        if let Err(e) = ctx.stop() {
            error!("Error stopping MmCtx: {}", e);
        }
    });
    Ok("success")
}

/// The legacy `stop`: calls [`stop_core`] and answers `{"result": "success"}`.
pub async fn stop(ctx: MmArc) -> Result<Response<Vec<u8>>, String> {
    legacy_result(stop_core(ctx, NoParamsRequest {}).await)
}

#[cfg(feature = "sim-panic-rpc")]
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

/// The typed core of `version`: the version object, whole, as the legacy body is (`result`, `datetime`).
pub async fn version_core(_ctx: MmArc, _req: NoParamsRequest) -> Result<MmVersionResult, MmError<InfallibleRpcError>> {
    Ok(MmVersionResult::new())
}

/// The legacy `version`: the version object as the whole body (HTTP 200, `application/json`).
pub async fn version(ctx: MmArc) -> Result<Response<Vec<u8>>, String> {
    match version_core(ctx, NoParamsRequest {}).await {
        Ok(version) => rpc_response(200, version.to_json().to_string()).compat().await,
        Err(error) => match error.into_inner() {},
    }
}

/// The typed core of `get_directly_connected_peers`: the peers this node is connected to, each with its addresses.
pub async fn get_directly_connected_peers_core(
    ctx: MmArc,
    _req: NoParamsRequest,
) -> Result<HashMap<String, Vec<String>>, MmError<InfallibleRpcError>> {
    use crate::mm2::lp_network::P2PContext;
    use mm2_p2p::atomicdex_behaviour::get_peers_info;
    let ctx = P2PContext::fetch_from_mm_arc(&ctx);
    let cmd_tx = ctx.cmd_tx.lock().clone();
    Ok(get_peers_info(cmd_tx).await)
}

/// The legacy `get_directly_connected_peers`: calls [`get_directly_connected_peers_core`] and answers
/// `{"result": {<peer>: [<address>]}}`.
pub async fn get_peers_info(ctx: MmArc) -> Result<Response<Vec<u8>>, String> {
    legacy_result(get_directly_connected_peers_core(ctx, NoParamsRequest {}).await)
}

/// The typed core of `get_gossip_mesh`: the current gossipsub mesh state.
pub async fn get_gossip_mesh_core(
    ctx: MmArc,
    _req: NoParamsRequest,
) -> Result<HashMap<String, Vec<String>>, MmError<InfallibleRpcError>> {
    use crate::mm2::lp_network::P2PContext;
    use mm2_p2p::atomicdex_behaviour::get_gossip_mesh;
    let ctx = P2PContext::fetch_from_mm_arc(&ctx);
    let cmd_tx = ctx.cmd_tx.lock().clone();
    Ok(get_gossip_mesh(cmd_tx).await)
}

/// The legacy `get_gossip_mesh`: calls [`get_gossip_mesh_core`] and answers `{"result": {...}}`.
pub async fn get_gossip_mesh(ctx: MmArc) -> Result<Response<Vec<u8>>, String> {
    legacy_result(get_gossip_mesh_core(ctx, NoParamsRequest {}).await)
}

/// The typed core of `get_gossip_peer_topics`: the topics of each gossip peer.
pub async fn get_gossip_peer_topics_core(
    ctx: MmArc,
    _req: NoParamsRequest,
) -> Result<HashMap<String, Vec<String>>, MmError<InfallibleRpcError>> {
    use crate::mm2::lp_network::P2PContext;
    use mm2_p2p::atomicdex_behaviour::get_gossip_peer_topics;
    let ctx = P2PContext::fetch_from_mm_arc(&ctx);
    let cmd_tx = ctx.cmd_tx.lock().clone();
    Ok(get_gossip_peer_topics(cmd_tx).await)
}

/// The legacy `get_gossip_peer_topics`: calls [`get_gossip_peer_topics_core`] and answers `{"result": {...}}`.
pub async fn get_gossip_peer_topics(ctx: MmArc) -> Result<Response<Vec<u8>>, String> {
    legacy_result(get_gossip_peer_topics_core(ctx, NoParamsRequest {}).await)
}

/// The typed core of `get_gossip_topic_peers`: the peers of each gossip topic.
pub async fn get_gossip_topic_peers_core(
    ctx: MmArc,
    _req: NoParamsRequest,
) -> Result<HashMap<String, Vec<String>>, MmError<InfallibleRpcError>> {
    use crate::mm2::lp_network::P2PContext;
    use mm2_p2p::atomicdex_behaviour::get_gossip_topic_peers;
    let ctx = P2PContext::fetch_from_mm_arc(&ctx);
    let cmd_tx = ctx.cmd_tx.lock().clone();
    Ok(get_gossip_topic_peers(cmd_tx).await)
}

/// The legacy `get_gossip_topic_peers`: calls [`get_gossip_topic_peers_core`] and answers `{"result": {...}}`.
pub async fn get_gossip_topic_peers(ctx: MmArc) -> Result<Response<Vec<u8>>, String> {
    legacy_result(get_gossip_topic_peers_core(ctx, NoParamsRequest {}).await)
}

/// The typed core of `get_relay_mesh`: the relays of the relay mesh.
pub async fn get_relay_mesh_core(
    ctx: MmArc,
    _req: NoParamsRequest,
) -> Result<Vec<String>, MmError<InfallibleRpcError>> {
    use crate::mm2::lp_network::P2PContext;
    use mm2_p2p::atomicdex_behaviour::get_relay_mesh;
    let ctx = P2PContext::fetch_from_mm_arc(&ctx);
    let cmd_tx = ctx.cmd_tx.lock().clone();
    Ok(get_relay_mesh(cmd_tx).await)
}

/// The legacy `get_relay_mesh`: calls [`get_relay_mesh_core`] and answers `{"result": [...]}`.
pub async fn get_relay_mesh(ctx: MmArc) -> Result<Response<Vec<u8>>, String> {
    legacy_result(get_relay_mesh_core(ctx, NoParamsRequest {}).await)
}

/// The typed core of `get_my_peer_id`: the peer identifier of this node.
///
/// # Errors
///
/// A [`GetMyPeerIdError`] when the node has no peer identifier yet.
pub async fn get_my_peer_id_core(ctx: MmArc, _req: NoParamsRequest) -> Result<String, MmError<GetMyPeerIdError>> {
    let peer_id = ctx
        .peer_id
        .ok_or("Peer ID is not initialized")
        .map_to_mm(|e| GetMyPeerIdError::Internal(e.to_string()))?;
    Ok(peer_id.clone())
}

/// The legacy `get_my_peer_id`: calls [`get_my_peer_id_core`] and answers `{"result": "<peer id>"}`.
pub async fn get_my_peer_id(ctx: MmArc) -> Result<Response<Vec<u8>>, String> {
    legacy_result(get_my_peer_id_core(ctx, NoParamsRequest {}).await)
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
            || deregister_coin_after_failed_balance_read(&ctx, "AAA", None),
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
            || deregister_coin_after_failed_balance_read(&ctx, "AAA", None),
        ));
        assert!(result.is_err());
        assert!(block_on(lp_coinfind(&ctx, "AAA")).unwrap().is_some());
    }

    /// CRD ch.55 R55.4.9: the cleanup of a failed activation removes the instance that activation registered, and
    /// leaves a different instance of the same ticker registered.
    #[test]
    fn the_cleanup_of_a_failed_read_leaves_another_instance_of_the_ticker_registered() {
        let ctx = MmCtxBuilder::new().with_conf(json!({})).into_mm_arc();
        let register = |ctx: &MmArc| {
            block_on(lp_register_coin(ctx, TestCoin::new("AAA").into(), RegisterCoinParams {
                ticker: "AAA".to_owned(),
                tx_history: false,
            }))
            .expect("AAA is registered")
        };
        let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();

        register(&ctx);
        let failed_activation = block_on(coins_ctx.instance_id("AAA")).expect("the first instance is registered");
        // The first instance is removed by someone else, and a second instance registers under the same ticker.
        block_on(disable_coin_impl(&ctx, "AAA")).unwrap();
        register(&ctx);
        let second = block_on(coins_ctx.instance_id("AAA")).expect("the second instance is registered");
        assert_ne!(failed_activation, second);

        block_on(deregister_coin_after_failed_balance_read(
            &ctx,
            "AAA",
            Some(failed_activation),
        ));
        assert!(
            block_on(lp_coinfind(&ctx, "AAA")).unwrap().is_some(),
            "the other instance stays"
        );
        assert_eq!(block_on(coins_ctx.instance_id("AAA")), Some(second));

        block_on(deregister_coin_after_failed_balance_read(&ctx, "AAA", Some(second)));
        assert!(
            block_on(lp_coinfind(&ctx, "AAA")).unwrap().is_none(),
            "its own instance is removed"
        );
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

    /// Solana is postponed for this beta: the legacy `enable` and `electrum` refuse every coin of the family with
    /// the "not available in this build" text and register nothing. The node keeps running. (TRON is switched on:
    /// `tron_activation_tests` has its legacy routes.)
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn legacy_enable_and_electrum_refuse_solana_coins() {
        use coins::SOLANA_NOT_AVAILABLE;
        use crypto::CryptoCtx;

        let coins = vec![
            ("SOL", json!({ "type": "SOLANA" }), SOLANA_NOT_AVAILABLE),
            (
                "USDC-SOL",
                json!({ "type": "SPLTOKEN", "protocol_data": {
                    "platform": "SOL",
                    "token_contract_address": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
                    "decimals": 6 } }),
                SOLANA_NOT_AVAILABLE,
            ),
            // The reference configuration forms (CRD ch.40 R40.1.2, R40.1.3) are refused the same way.
            (
                "SOL-REF",
                json!({ "type": "SOLANA", "protocol_data": {} }),
                SOLANA_NOT_AVAILABLE,
            ),
            (
                "USDC-SOL-REF",
                json!({ "type": "SOLANATOKEN", "protocol_data": {
                    "platform": "SOL-REF",
                    "decimals": 6,
                    "mint_address": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
                    "program_id": "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb" } }),
                SOLANA_NOT_AVAILABLE,
            ),
        ];
        let conf_coins: Vec<Json> = coins
            .iter()
            .map(|(ticker, protocol, _)| {
                json!({ "coin": ticker, "mm2": 1, "decimals": 6, "required_confirmations": 1, "protocol": protocol })
            })
            .collect();
        let ctx = MmCtxBuilder::new()
            .with_conf(json!({ "coins": conf_coins }))
            .into_mm_arc();
        CryptoCtx::init_with_iguana_passphrase(ctx.clone(), "legacy solana refusal test passphrase").unwrap();

        for (ticker, _, message) in &coins {
            let request = |method: &str| {
                json!({ "method": method, "coin": ticker, "urls": ["http://127.0.0.1:1"],
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
