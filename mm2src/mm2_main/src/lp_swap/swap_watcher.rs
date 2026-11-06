use super::swap_watcher_v2::{self, TakerSwapWatcherV2};
/// Swap watcher node — monitors ongoing swaps and can complete or refund them
/// if one party disappears mid-swap.
///
/// A watcher subscribes to the `swpwtchr/<taker_coin>` gossipsub topic. When
/// the taker broadcasts `TakerSwapWatcherData` (after sending taker payment),
/// any watcher node receiving the message can:
///   1. Validate the taker fee on-chain
///   2. Validate the taker payment on-chain
///   3. Wait for the taker payment to be spent (normal completion)
///   4. If spent → extract secret → broadcast maker payment spend preimage
///   5. If timeout → broadcast taker payment refund preimage
///
/// The watcher carries **precomputed** spend/refund transactions so it never
/// needs the taker's private key.
use crate::mm2::lp_swap::SwapsContext;
use coins::{lp_coinfind, MmCoinEnum};
use common::executor::{spawn, Timer};
use common::log::{debug, error, info, warn};
use common::now_ms;
use common::state_machine::prelude::*;
use futures::compat::Future01CompatExt;
use futures::FutureExt;
use mm2_core::mm_ctx::MmArc;
use mm2_p2p::TopicPrefix;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use uuid::Uuid;

// ── Constants ────────────────────────────────────────────────────────

/// Gossipsub topic prefix for watcher messages.
pub const WATCHER_PREFIX: TopicPrefix = "swpwtchr";

/// Interval between watcher message re-broadcasts (seconds).
pub const WATCHER_MSG_INTERVAL: f64 = 10.0;

/// Maximum number of retries when validating the taker fee.
const TAKER_FEE_VALIDATION_ATTEMPTS: u32 = 6;

/// Delay between taker fee validation retries (seconds).
const TAKER_FEE_VALIDATION_RETRY_SECS: f64 = 10.0;

/// Default seconds to wait for taker payment to appear on-chain.
const WAIT_TAKER_PAYMENT_DEFAULT_SECS: f64 = 60.0;

/// Default poll interval when searching for taker payment spend (seconds).
const SEARCH_INTERVAL_DEFAULT_SECS: f64 = 300.0;

/// Factor applied to lock_duration to compute refund deadline.
/// refund_start = swap_started_at + (lock_duration * REFUND_START_FACTOR)
const REFUND_START_FACTOR: f64 = 1.5;

/// Duration (seconds) that a taker swap watcher lock stays active.
const TAKER_SWAP_WATCHER_ENTRY_TIMEOUT_SECS: u64 = 21600; // 6 hours

/// Bound background jobs admitted from untrusted gossip before chain validation.
const MAX_CONCURRENT_WATCHERS: usize = 256;

// ── Wire Protocol ────────────────────────────────────────────────────

/// Message envelope for watcher-relevant data, transmitted via gossipsub.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum SwapWatcherMsg {
    TakerSwapWatcherMsg(Box<TakerSwapWatcherData>),
    TakerSwapWatcherMsgV2(Box<TakerSwapWatcherV2>),
}

/// All information a watcher needs to monitor and intervene in a taker swap.
///
/// The taker broadcasts this after sending the taker payment. Precomputed
/// spend/refund transaction preimages allow the watcher to act without any
/// private keys.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TakerSwapWatcherData {
    pub uuid: Uuid,
    pub secret_hash: Vec<u8>,
    /// Precomputed transaction that spends the maker payment (taker-side success path).
    pub maker_payment_spend_preimage: Vec<u8>,
    /// Precomputed transaction that refunds the taker payment (safety net on timeout).
    pub taker_payment_refund_preimage: Vec<u8>,
    pub swap_started_at: u64,
    pub lock_duration: u64,
    pub taker_coin: String,
    /// Hash of taker fee tx — used to locate fee on-chain.
    pub taker_fee_hash: Vec<u8>,
    /// Hash of taker payment tx.
    pub taker_payment_hash: Vec<u8>,
    pub taker_coin_start_block: u64,
    pub taker_payment_confirmations: u64,
    pub taker_payment_requires_nota: Option<bool>,
    pub maker_coin: String,
    /// Maker's HTLC public key (33 bytes, compressed secp256k1).
    pub maker_pub: Vec<u8>,
    /// Hash of maker payment tx.
    pub maker_payment_hash: Vec<u8>,
    pub maker_coin_start_block: u64,
}

// ── Watcher Configuration ────────────────────────────────────────────

/// Per-coin watcher timing overrides (from coin config JSON).
#[derive(Clone, Debug, Deserialize)]
pub struct WatcherConf {
    #[serde(default = "default_wait_taker_payment")]
    pub wait_taker_payment: f64,
    #[serde(default = "default_search_interval")]
    pub search_interval: f64,
    #[serde(default = "default_refund_start_factor")]
    pub refund_start_factor: f64,
}

impl Default for WatcherConf {
    fn default() -> Self {
        WatcherConf {
            wait_taker_payment: WAIT_TAKER_PAYMENT_DEFAULT_SECS,
            search_interval: SEARCH_INTERVAL_DEFAULT_SECS,
            refund_start_factor: REFUND_START_FACTOR,
        }
    }
}

fn default_wait_taker_payment() -> f64 { WAIT_TAKER_PAYMENT_DEFAULT_SECS }
fn default_search_interval() -> f64 { SEARCH_INTERVAL_DEFAULT_SECS }
fn default_refund_start_factor() -> f64 { REFUND_START_FACTOR }

// ── Watcher State Machine Context ────────────────────────────────────

/// Shared context accessible by all watcher states.
pub struct WatcherStateMachineCtx {
    /// MmArc context — used when watcher needs to query node state.
    #[allow(dead_code)]
    pub ctx: MmArc,
    pub data: TakerSwapWatcherData,
    pub conf: WatcherConf,
    pub taker_coin: MmCoinEnum,
    pub maker_coin: MmCoinEnum,
    /// Verified public key of the taker that signed the delegation.
    pub verified_pub: Vec<u8>,
    pub(super) v2: Option<TakerSwapWatcherV2>,
    taker_payment: Option<Vec<u8>>,
}

impl WatcherStateMachineCtx {
    fn taker_locktime(&self) -> u64 { self.data.swap_started_at + self.data.lock_duration }

    fn refund_start_time(&self) -> u64 {
        let factor = self.conf.refund_start_factor;
        self.data.swap_started_at + (factor * self.data.lock_duration as f64) as u64
    }

    async fn taker_spend(&self) -> Result<Option<coins::FoundSwapTxSpend>, String> {
        let payment = self
            .taker_payment
            .as_ref()
            .ok_or("the validated watcher payment is unavailable")?;
        let terms = self
            .v2
            .as_ref()
            .ok_or("the watcher spend search has no validated V2 context")?;
        self.taker_coin
            .watcher_search_for_swap_tx_spend(
                self.taker_locktime() as u32,
                &terms.taker_claim_pub,
                &terms.taker_refund_pub,
                &self.data.secret_hash,
                payment,
                self.data.taker_coin_start_block,
            )
            .await
    }
}

/// Terminal result of the watcher state machine run.
#[derive(Clone, Debug)]
pub enum WatcherResult {
    /// Watcher successfully spent maker payment after observing taker payment spend.
    MakerPaymentSpent,
    /// Watcher successfully refunded taker payment after timeout.
    TakerPaymentRefunded,
    /// Watcher determined the swap completed normally (taker payment already spent).
    CompletedNormally,
    /// Watcher stopped due to validation failure or an error.
    StoppedOnError(String),
}

// ── States ───────────────────────────────────────────────────────────

/// State 1: Validate the taker fee transaction on-chain.
pub struct ValidateTakerFee;

/// State 2: Wait for and validate the taker payment on-chain.
pub struct ValidateTakerPayment;

/// State 3: Wait for the taker payment to be spent or timeout.
pub struct WaitForTakerPaymentSpend;

/// State 4a: Taker payment was spent → extract secret → spend maker payment.
pub struct SpendMakerPayment {
    /// Secret extracted from the taker payment spend; never logged.
    pub secret: Vec<u8>,
}

/// State 4b: Timeout reached → refund taker payment using preimage.
pub struct RefundTakerPayment;

/// Terminal state: watcher is done.
pub struct Stopped {
    pub result: WatcherResult,
}

// ── Transitions ──────────────────────────────────────────────────────

impl TransitionFrom<ValidateTakerFee> for ValidateTakerPayment {}
impl TransitionFrom<ValidateTakerFee> for Stopped {}
impl TransitionFrom<ValidateTakerPayment> for WaitForTakerPaymentSpend {}
impl TransitionFrom<ValidateTakerPayment> for Stopped {}
impl TransitionFrom<WaitForTakerPaymentSpend> for SpendMakerPayment {}
impl TransitionFrom<WaitForTakerPaymentSpend> for RefundTakerPayment {}
impl TransitionFrom<WaitForTakerPaymentSpend> for Stopped {}
impl TransitionFrom<SpendMakerPayment> for Stopped {}
impl TransitionFrom<RefundTakerPayment> for Stopped {}
impl TransitionFrom<RefundTakerPayment> for SpendMakerPayment {}

// ── State Implementations ────────────────────────────────────────────

#[async_trait::async_trait]
impl State for ValidateTakerFee {
    type Ctx = WatcherStateMachineCtx;
    type Result = WatcherResult;

    async fn on_changed(self: Box<Self>, ctx: &mut Self::Ctx) -> StateResult<Self::Ctx, Self::Result> {
        let input = coins::WatcherValidateTakerFeeInput {
            taker_fee_hash: ctx.data.taker_fee_hash.clone(),
            sender_pubkey: ctx.verified_pub.clone(),
            min_block_number: ctx.data.taker_coin_start_block,
            fee_addr: mm2_net_config::net_config_or_panic(ctx.ctx.netid())
                .dex_fee_addr_raw_pubkey()
                .to_vec(),
            lock_duration: ctx.data.lock_duration,
        };

        for attempt in 1..=TAKER_FEE_VALIDATION_ATTEMPTS {
            let validation = match &ctx.v2 {
                Some(terms) => swap_watcher_v2::validate_fee(ctx, terms).await,
                None => ctx.taker_coin.watcher_validate_taker_fee(input.clone()).compat().await,
            };
            match validation {
                Ok(()) => {
                    info!("Watcher {}: taker fee validated (attempt {})", ctx.data.uuid, attempt);
                    return Self::change_state(ValidateTakerPayment);
                },
                Err(e) => {
                    warn!(
                        "Watcher {}: taker fee validation attempt {}/{} failed: {}",
                        ctx.data.uuid, attempt, TAKER_FEE_VALIDATION_ATTEMPTS, e
                    );
                    if attempt < TAKER_FEE_VALIDATION_ATTEMPTS {
                        Timer::sleep(TAKER_FEE_VALIDATION_RETRY_SECS).await;
                    }
                },
            }
        }

        Self::change_state(Stopped {
            result: WatcherResult::StoppedOnError(format!(
                "Watcher {}: taker fee validation failed after {} attempts",
                ctx.data.uuid, TAKER_FEE_VALIDATION_ATTEMPTS
            )),
        })
    }
}

#[async_trait::async_trait]
impl State for ValidateTakerPayment {
    type Ctx = WatcherStateMachineCtx;
    type Result = WatcherResult;

    async fn on_changed(self: Box<Self>, ctx: &mut Self::Ctx) -> StateResult<Self::Ctx, Self::Result> {
        let Some(terms) = &ctx.v2 else {
            // The legacy envelope has no expected amount or per-chain taker
            // keys. Do not fabricate these in a funds-moving validator.
            return Self::change_state(Stopped {
                result: WatcherResult::StoppedOnError(
                    "legacy watcher delegation lacks the amount and chain-key context required for safe execution"
                        .into(),
                ),
            });
        };
        // A normal block interval can exceed the per-RPC retry budget. Keep
        // the accepted job while its funding reaches the agreed confirmations.
        let deadline = match terms.maker_locktime() {
            Ok(lock) => u64::from(lock).max(now_ms() / 1000 + ctx.conf.wait_taker_payment as u64),
            Err(error) => {
                return Self::change_state(Stopped {
                    result: WatcherResult::StoppedOnError(error),
                })
            },
        };
        loop {
            let now = now_ms() / 1000;
            if now > deadline {
                return Self::change_state(Stopped {
                    result: WatcherResult::StoppedOnError(format!(
                        "Watcher {}: payment validation did not finish by {}",
                        ctx.data.uuid, deadline
                    )),
                });
            }

            match swap_watcher_v2::validate_payments(ctx, terms).await {
                Ok(()) => {
                    ctx.taker_payment = Some(terms.taker_payment.clone());
                    return Self::change_state(WaitForTakerPaymentSpend);
                },
                Err(error) => {
                    debug!("Watcher {}: payment validation is pending: {}", ctx.data.uuid, error);
                    Timer::sleep(10.0).await;
                },
            }
        }
    }
}

#[async_trait::async_trait]
impl State for WaitForTakerPaymentSpend {
    type Ctx = WatcherStateMachineCtx;
    type Result = WatcherResult;

    async fn on_changed(self: Box<Self>, ctx: &mut Self::Ctx) -> StateResult<Self::Ctx, Self::Result> {
        let refund_deadline = ctx.refund_start_time();
        let search_interval = ctx.conf.search_interval;

        loop {
            let now = now_ms() / 1000;
            // Search before checking the refund deadline: a claim can reveal the
            // secret just before the refund gate opens.
            match ctx.taker_spend().await {
                Ok(Some(coins::FoundSwapTxSpend::Spent(spend_tx))) => {
                    info!("Watcher {}: taker payment was spent, extracting secret", ctx.data.uuid);
                    match ctx.taker_coin.extract_secret(&ctx.data.secret_hash, &spend_tx.tx_hex()) {
                        Ok(secret) => {
                            return Self::change_state(SpendMakerPayment { secret });
                        },
                        Err(e) => {
                            return Self::change_state(Stopped {
                                result: WatcherResult::StoppedOnError(format!(
                                    "Watcher {}: failed to extract secret: {}",
                                    ctx.data.uuid, e
                                )),
                            });
                        },
                    }
                },
                Ok(Some(coins::FoundSwapTxSpend::Refunded(_))) => {
                    return Self::change_state(RefundTakerPayment);
                },
                Ok(None) => {
                    if now >= refund_deadline {
                        return Self::change_state(RefundTakerPayment);
                    }
                    debug!(
                        "Watcher {}: taker payment not yet spent, checking again in {}s",
                        ctx.data.uuid, search_interval
                    );
                },
                Err(e) => {
                    warn!(
                        "Watcher {}: error searching for taker payment spend: {}",
                        ctx.data.uuid, e
                    );
                },
            }

            Timer::sleep(search_interval).await;
        }
    }
}

#[async_trait::async_trait]
impl State for SpendMakerPayment {
    type Ctx = WatcherStateMachineCtx;
    type Result = WatcherResult;

    async fn on_changed(self: Box<Self>, ctx: &mut Self::Ctx) -> StateResult<Self::Ctx, Self::Result> {
        if ctx.v2.is_none() {
            return Self::change_state(Stopped {
                result: WatcherResult::StoppedOnError("the delegated claim has no validated V2 context".into()),
            });
        }
        let preimage = match ctx.maker_coin.complete_watcher_claim(
            &ctx.data.maker_payment_spend_preimage,
            &self.secret,
            &ctx.data.secret_hash,
        ) {
            Ok(preimage) => preimage,
            Err(error) => {
                return Self::change_state(Stopped {
                    result: WatcherResult::StoppedOnError(error),
                })
            },
        };
        loop {
            // A broadcast acknowledgement can be lost. Inspect the same payment
            // before retrying the already-signed transaction.
            if let Some(terms) = &ctx.v2 {
                let found = ctx
                    .maker_coin
                    .watcher_search_for_swap_tx_spend(
                        match terms.maker_locktime() {
                            Ok(locktime) => locktime,
                            Err(error) => {
                                return Self::change_state(Stopped {
                                    result: WatcherResult::StoppedOnError(error),
                                })
                            },
                        },
                        &terms.maker_claim_pub,
                        &terms.maker_refund_pub,
                        &ctx.data.secret_hash,
                        &terms.maker_payment,
                        ctx.data.maker_coin_start_block,
                    )
                    .await;
                match found {
                    Ok(Some(coins::FoundSwapTxSpend::Refunded(transaction))) => {
                        if ctx
                            .maker_coin
                            .wait_for_confirmations(
                                &transaction.tx_hex(),
                                terms
                                    .maker_payment_confirmations
                                    .max(ctx.maker_coin.required_confirmations())
                                    .max(1),
                                terms.maker_payment_requires_nota || ctx.maker_coin.requires_notarization(),
                                now_ms() / 1000 + 60,
                                5,
                            )
                            .compat()
                            .await
                            .is_ok()
                        {
                            return Self::change_state(Stopped {
                                result: WatcherResult::StoppedOnError(
                                    "the maker payment was refunded before the delegated claim".into(),
                                ),
                            });
                        }
                        Timer::sleep(10.0).await;
                        continue;
                    },
                    Ok(Some(coins::FoundSwapTxSpend::Spent(transaction))) => {
                        let deadline = now_ms() / 1000 + 60;
                        if ctx
                            .maker_coin
                            .wait_for_confirmations(
                                &transaction.tx_hex(),
                                terms
                                    .maker_payment_confirmations
                                    .max(ctx.maker_coin.required_confirmations())
                                    .max(1),
                                terms.maker_payment_requires_nota || ctx.maker_coin.requires_notarization(),
                                deadline,
                                5,
                            )
                            .compat()
                            .await
                            .is_ok()
                        {
                            return Self::change_state(Stopped {
                                result: WatcherResult::MakerPaymentSpent,
                            });
                        }
                        Timer::sleep(10.0).await;
                        continue;
                    },
                    Ok(None) => {},
                    Err(_) => {
                        Timer::sleep(10.0).await;
                        continue;
                    },
                }
            }
            let _ = ctx.maker_coin.send_raw_tx_bytes(&preimage).compat().await;
            Timer::sleep(10.0).await;
        }
    }
}

#[async_trait::async_trait]
impl State for RefundTakerPayment {
    type Ctx = WatcherStateMachineCtx;
    type Result = WatcherResult;

    async fn on_changed(self: Box<Self>, ctx: &mut Self::Ctx) -> StateResult<Self::Ctx, Self::Result> {
        if ctx.v2.is_none() {
            return Self::change_state(Stopped {
                result: WatcherResult::StoppedOnError("the delegated refund has no validated V2 context".into()),
            });
        }
        loop {
            // A maker can claim while the watcher waits for the refund gate.
            // Re-read the chain and follow that claim instead of refunding blindly.
            match ctx.taker_spend().await {
                Ok(Some(coins::FoundSwapTxSpend::Spent(transaction))) => {
                    match ctx
                        .taker_coin
                        .extract_secret(&ctx.data.secret_hash, &transaction.tx_hex())
                    {
                        Ok(secret) => return Self::change_state(SpendMakerPayment { secret }),
                        Err(error) => {
                            return Self::change_state(Stopped {
                                result: WatcherResult::StoppedOnError(error),
                            })
                        },
                    }
                },
                Ok(Some(coins::FoundSwapTxSpend::Refunded(transaction))) => {
                    if ctx
                        .taker_coin
                        .wait_for_confirmations(
                            &transaction.tx_hex(),
                            ctx.data
                                .taker_payment_confirmations
                                .max(ctx.taker_coin.required_confirmations())
                                .max(1),
                            ctx.data.taker_payment_requires_nota.unwrap_or(false)
                                || ctx.taker_coin.requires_notarization(),
                            now_ms() / 1000 + 60,
                            5,
                        )
                        .compat()
                        .await
                        .is_ok()
                    {
                        return Self::change_state(Stopped {
                            result: WatcherResult::TakerPaymentRefunded,
                        });
                    }
                },
                Ok(None) => {
                    if matches!(
                        ctx.taker_coin.can_refund_htlc(ctx.taker_locktime()).compat().await,
                        Ok(coins::CanRefundHtlc::CanRefundNow)
                    ) {
                        // Every retry uses the original signed transaction. A watcher
                        // cannot raise its fee or change its destination.
                        let _ = ctx
                            .taker_coin
                            .send_raw_tx_bytes(&ctx.data.taker_payment_refund_preimage)
                            .compat()
                            .await;
                    }
                },
                Err(_) => {},
            }
            Timer::sleep(10.0).await;
        }
    }
}

#[async_trait::async_trait]
impl LastState for Stopped {
    type Ctx = WatcherStateMachineCtx;
    type Result = WatcherResult;

    async fn on_changed(self: Box<Self>, ctx: &mut Self::Ctx) -> Self::Result {
        match &self.result {
            WatcherResult::MakerPaymentSpent => {
                info!("Watcher {}: completed — maker payment spent", ctx.data.uuid);
            },
            WatcherResult::TakerPaymentRefunded => {
                info!("Watcher {}: completed — taker payment refunded", ctx.data.uuid);
            },
            WatcherResult::CompletedNormally => {
                info!("Watcher {}: swap completed normally", ctx.data.uuid);
            },
            WatcherResult::StoppedOnError(e) => {
                warn!("Watcher {}: stopped with error: {}", ctx.data.uuid, e);
            },
        }
        self.result
    }
}

// ── Watcher Lock ─────────────────────────────────────────────────────

/// Prevents duplicate watchers from running for the same swap.
/// Uses the taker fee hash as the deduplication key.
pub(super) struct SwapWatcherLock {
    swap_ctx: Arc<SwapsContext>,
    fee_hash: Vec<u8>,
    expires: u64,
}

impl SwapWatcherLock {
    /// Attempt to acquire a watcher lock for the given fee hash.
    /// Returns `None` for a running job or a full admission queue.
    fn try_lock(swap_ctx: Arc<SwapsContext>, fee_hash: Vec<u8>) -> Option<Self> {
        let expires = now_ms() / 1000 + TAKER_SWAP_WATCHER_ENTRY_TIMEOUT_SECS;
        {
            let mut watchers = swap_ctx.taker_swap_watchers.lock();
            let now_sec = now_ms() / 1000;
            watchers.retain(|_, expiry| *expiry > now_sec);
            if watchers.len() >= MAX_CONCURRENT_WATCHERS {
                return None;
            }
            if watchers.contains_key(&fee_hash) {
                return None;
            }
            watchers.insert(fee_hash.clone(), expires);
        } // lock guard dropped before moving swap_ctx
        Some(SwapWatcherLock {
            swap_ctx,
            fee_hash,
            expires,
        })
    }

    fn renew(&mut self) -> bool {
        let mut watchers = self.swap_ctx.taker_swap_watchers.lock();
        if watchers.get(&self.fee_hash) != Some(&self.expires) {
            return false;
        }
        self.expires = now_ms() / 1000 + TAKER_SWAP_WATCHER_ENTRY_TIMEOUT_SECS;
        watchers.insert(self.fee_hash.clone(), self.expires);
        true
    }
}

impl Drop for SwapWatcherLock {
    fn drop(&mut self) {
        let mut watchers = self.swap_ctx.taker_swap_watchers.lock();
        if watchers.get(&self.fee_hash) == Some(&self.expires) {
            watchers.remove(&self.fee_hash);
        }
    }
}

// ── Entry Points ─────────────────────────────────────────────────────

/// Build the gossipsub topic for watcher messages of a given coin.
pub fn watcher_topic(coin_ticker: &str) -> String { mm2_p2p::pub_sub_topic(WATCHER_PREFIX, coin_ticker) }

/// Follow the common coin registry so every activation API reaches the watcher.
/// Only operator nodes that explicitly enable watching subscribe to these topics.
pub(crate) fn start_watcher_subscriptions(ctx: &MmArc) {
    if !ctx.conf["is_watcher"].as_bool().unwrap_or(false) {
        return;
    }
    let weak = ctx.weak();
    spawn(async move {
        let mut subscribed = HashSet::new();
        loop {
            let Some(ctx) = MmArc::from_weak(&weak) else { return };
            if ctx.is_stopping() {
                return;
            }
            let coins = match coins::CoinsContext::from_ctx(&ctx) {
                Ok(coins) => coins,
                Err(error) => {
                    warn!("Cannot read watcher coin subscriptions: {}", error);
                    return;
                },
            };
            let tickers = coins
                .watcher_supported_tickers(ctx.conf["watcher_htlc_v2"].as_bool() == Some(true))
                .await;
            let p2p = crate::mm2::lp_network::P2PContext::fetch_from_mm_arc(&ctx);
            for ticker in tickers {
                if subscribed.contains(&ticker) {
                    continue;
                }
                let cmd = mm2_p2p::atomicdex_behaviour::AdexBehaviourCmd::Subscribe {
                    topic: watcher_topic(&ticker),
                };
                if p2p.cmd_tx.lock().try_send(cmd).is_ok() {
                    subscribed.insert(ticker);
                }
            }
            drop(ctx);
            Timer::sleep(1.0).await;
        }
    });
}

/// Process an incoming watcher gossipsub message.
/// Verifies the message signature, extracts watcher data, and spawns the
/// watcher state machine if both coins are enabled and support watchers.
pub async fn process_watcher_msg(ctx: MmArc, msg: &[u8]) {
    if !ctx.conf["is_watcher"].as_bool().unwrap_or(false) || ctx.is_stopping() {
        return;
    }
    let (watcher_msg, _raw, verified_pubkey) = match mm2_p2p::decode_signed::<SwapWatcherMsg>(msg) {
        Ok(m) => m,
        Err(e) => {
            warn!("Failed to decode watcher message: {:?}", e);
            return;
        },
    };

    match watcher_msg {
        SwapWatcherMsg::TakerSwapWatcherMsg(data) => {
            spawn_taker_swap_watcher(ctx, *data, verified_pubkey.to_bytes().to_vec(), None).await;
        },
        SwapWatcherMsg::TakerSwapWatcherMsgV2(terms) => {
            let terms = *terms;
            if ctx.conf["watcher_htlc_v2"].as_bool() != Some(true) {
                return;
            }
            if let Err(error) = terms.validate_publisher(ctx.netid(), &verified_pubkey.to_bytes()) {
                warn!("Refused watcher delegation: {}", error);
                return;
            }
            spawn_taker_swap_watcher(
                ctx,
                terms.data.clone(),
                verified_pubkey.to_bytes().to_vec(),
                Some(terms),
            )
            .await;
        },
    }
}

/// Spawn a watcher state machine for a taker swap.
async fn spawn_taker_swap_watcher(
    ctx: MmArc,
    data: TakerSwapWatcherData,
    verified_pub: Vec<u8>,
    v2: Option<TakerSwapWatcherV2>,
) {
    let uuid = data.uuid;
    if data.taker_fee_hash.len() != 32 || data.taker_payment_hash.len() != 32 || data.maker_payment_hash.len() != 32 {
        warn!("Watcher {}: invalid payment transaction identifier", uuid);
        return;
    }

    // Look up both coins; skip if not enabled
    let taker_coin = match lp_coinfind(&ctx, &data.taker_coin).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            debug!("Watcher {}: taker coin {} not enabled, skipping", uuid, data.taker_coin);
            return;
        },
        Err(e) => {
            error!("Watcher {}: error finding taker coin: {}", uuid, e);
            return;
        },
    };

    let maker_coin = match lp_coinfind(&ctx, &data.maker_coin).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            debug!("Watcher {}: maker coin {} not enabled, skipping", uuid, data.maker_coin);
            return;
        },
        Err(e) => {
            error!("Watcher {}: error finding maker coin: {}", uuid, e);
            return;
        },
    };

    // Both coins must support watchers
    if !taker_coin.is_supported_by_watchers() || !maker_coin.is_supported_by_watchers() {
        debug!(
            "Watcher {}: coin pair {}/{} not supported by watchers",
            uuid, data.taker_coin, data.maker_coin
        );
        return;
    }
    let supports_v2 = taker_coin.supports_watcher_v2() && maker_coin.supports_watcher_v2();
    if v2.is_some() != supports_v2 {
        debug!(
            "Watcher {}: this coin pair requires a different delegation version",
            uuid
        );
        return;
    }

    // Acquire lock — only one watcher per swap
    let swap_ctx = match SwapsContext::from_ctx(&ctx) {
        Ok(sc) => sc,
        Err(e) => {
            error!("Watcher {}: SwapsContext error: {}", uuid, e);
            return;
        },
    };

    let key = match &v2 {
        Some(_) => [b"watcher-v2".as_slice(), &data.taker_fee_hash].concat(),
        None => data.taker_fee_hash.clone(),
    };
    let shutdown_rx = swap_ctx.shutdown_rx.clone();
    let mut lock = match SwapWatcherLock::try_lock(swap_ctx, key) {
        Some(l) => l,
        None => {
            debug!("Watcher {}: duplicate job or watcher capacity reached, skipping", uuid);
            return;
        },
    };

    info!(
        "Watcher {}: starting watcher for {}/{} swap",
        uuid, data.taker_coin, data.maker_coin
    );

    let machine_ctx = WatcherStateMachineCtx {
        ctx,
        data,
        conf: WatcherConf::default(),
        taker_coin,
        maker_coin,
        verified_pub,
        v2,
        taker_payment: None,
    };

    // Gossip processing must return immediately so a relay can forward the job.
    // The task owns its lock and is cancelled when this daemon instance stops.
    spawn(async move {
        let machine = StateMachine::from_ctx(machine_ctx);
        let run = machine.run(ValidateTakerFee).fuse();
        let shutdown = shutdown_rx.recv().fuse();
        futures::pin_mut!(run, shutdown);
        loop {
            let renew = Timer::sleep(60.0).fuse();
            futures::pin_mut!(renew);
            futures::select! {
                result = run => {
                    info!("Watcher {}: state machine finished with result: {:?}", uuid, result);
                    break;
                },
                _ = shutdown => break,
                _ = renew => {
                    if !lock.renew() { break; }
                },
            }
        }
    });
}

// ── Unit Tests ───────────────────────────────────────────────────────

#[cfg(test)]
#[path = "swap_watcher_tests.rs"]
mod watcher_regressions;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_watcher_topic_format() {
        assert_eq!(watcher_topic("BTC"), "swpwtchr/BTC");
        assert_eq!(watcher_topic("KMD"), "swpwtchr/KMD");
        assert_eq!(watcher_topic("ETH"), "swpwtchr/ETH");
    }

    #[test]
    fn test_watcher_data_serialization() {
        let data = TakerSwapWatcherData {
            uuid: Uuid::new_v4(),
            secret_hash: vec![0u8; 20],
            maker_payment_spend_preimage: vec![1u8; 100],
            taker_payment_refund_preimage: vec![2u8; 100],
            swap_started_at: 1700000000,
            lock_duration: 7500,
            taker_coin: "BTC".to_string(),
            taker_fee_hash: vec![3u8; 32],
            taker_payment_hash: vec![4u8; 32],
            taker_coin_start_block: 800000,
            taker_payment_confirmations: 1,
            taker_payment_requires_nota: None,
            maker_coin: "KMD".to_string(),
            maker_pub: vec![5u8; 33],
            maker_payment_hash: vec![6u8; 32],
            maker_coin_start_block: 3000000,
        };

        let msg = SwapWatcherMsg::TakerSwapWatcherMsg(data.clone().into());
        let serialized = serde_json::to_string(&msg).unwrap();
        // Boxing is an in-memory detail; preserve the original wire payload.
        #[derive(Serialize)]
        enum LegacyEnvelope {
            TakerSwapWatcherMsg(TakerSwapWatcherData),
        }
        let original = LegacyEnvelope::TakerSwapWatcherMsg(data.clone());
        assert_eq!(serialized, serde_json::to_string(&original).unwrap());
        assert_eq!(
            mm2_p2p::encode_message(&msg).unwrap(),
            mm2_p2p::encode_message(&original).unwrap()
        );
        let deserialized: SwapWatcherMsg = serde_json::from_str(&serialized).unwrap();

        match deserialized {
            SwapWatcherMsg::TakerSwapWatcherMsg(d) => {
                assert_eq!(d.uuid, data.uuid);
                assert_eq!(d.taker_coin, "BTC");
                assert_eq!(d.maker_coin, "KMD");
                assert_eq!(d.lock_duration, 7500);
            },
            SwapWatcherMsg::TakerSwapWatcherMsgV2(_) => panic!("expected the legacy watcher envelope"),
        }
    }

    #[test]
    fn test_watcher_conf_defaults() {
        let conf = WatcherConf::default();
        assert_eq!(conf.wait_taker_payment, 60.0);
        assert_eq!(conf.search_interval, 300.0);
        assert!((conf.refund_start_factor - 1.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_refund_start_time_calculation() {
        // With lock_duration=7500 and refund_factor=1.5:
        // refund_start = 1700000000 + (1.5 * 7500) = 1700000000 + 11250 = 1700011250
        let data = TakerSwapWatcherData {
            uuid: Uuid::nil(),
            secret_hash: vec![],
            maker_payment_spend_preimage: vec![],
            taker_payment_refund_preimage: vec![],
            swap_started_at: 1_700_000_000,
            lock_duration: 7500,
            taker_coin: String::new(),
            taker_fee_hash: vec![],
            taker_payment_hash: vec![],
            taker_coin_start_block: 0,
            taker_payment_confirmations: 1,
            taker_payment_requires_nota: None,
            maker_coin: String::new(),
            maker_pub: vec![],
            maker_payment_hash: vec![],
            maker_coin_start_block: 0,
        };
        let conf = WatcherConf::default();

        // taker_locktime = swap_started_at + lock_duration
        let taker_locktime = data.swap_started_at + data.lock_duration;
        assert_eq!(taker_locktime, 1_700_000_000 + 7500);

        // refund_start_time = swap_started_at + (refund_start_factor * lock_duration)
        let refund_start = data.swap_started_at + (conf.refund_start_factor * data.lock_duration as f64) as u64;
        assert_eq!(refund_start, 1_700_000_000 + 11250);
    }

    /// CRD ch.28 R16a, T4a (a): the watcher message the node sends passes the bounds check.
    #[test]
    fn t4a_a_watcher_message_nests_within_half_the_bound() {
        use common::msgpack_bounds::{check_msgpack_bounds, max_nesting_depth, MAX_PEER_NESTING};

        let msg = SwapWatcherMsg::TakerSwapWatcherMsg(Box::new(TakerSwapWatcherData {
            uuid: Uuid::new_v4(),
            secret_hash: vec![0u8; 20],
            maker_payment_spend_preimage: vec![1u8; 100],
            taker_payment_refund_preimage: vec![2u8; 100],
            swap_started_at: 1_700_000_000,
            lock_duration: 7500,
            taker_coin: "BTC".to_string(),
            taker_fee_hash: vec![3u8; 32],
            taker_payment_hash: vec![4u8; 32],
            taker_coin_start_block: 800_000,
            taker_payment_confirmations: 1,
            taker_payment_requires_nota: Some(false),
            maker_coin: "KMD".to_string(),
            maker_pub: vec![5u8; 33],
            maker_payment_hash: vec![6u8; 32],
            maker_coin_start_block: 3_000_000,
        }));
        let payload = mm2_p2p::encode_message(&msg).unwrap();
        let envelope = mm2_p2p::encode_and_sign(&msg, &[3; 32]).unwrap();
        for (what, encoded) in [("payload", &payload), ("envelope", &envelope)] {
            assert_eq!(check_msgpack_bounds(encoded), Ok(encoded.len()), "{what}");
            let depth = max_nesting_depth(encoded);
            println!("t4a depth SwapWatcherMsg {what}: {depth}");
            assert!(depth <= MAX_PEER_NESTING / 2, "{what}: depth {depth}");
        }
        mm2_p2p::decode_signed::<SwapWatcherMsg>(&envelope).unwrap();
    }
}
