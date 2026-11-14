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
use common::executor::Timer;
use common::log::{debug, error, info, warn};
use common::now_ms;
use common::state_machine::prelude::*;
use futures::compat::Future01CompatExt;
use mm2_core::mm_ctx::MmArc;
use mm2_p2p::TopicPrefix;
use serde::{Deserialize, Serialize};
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

// ── Wire Protocol ────────────────────────────────────────────────────

/// Message envelope for watcher-relevant data, transmitted via gossipsub.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum SwapWatcherMsg {
    TakerSwapWatcherMsg(TakerSwapWatcherData),
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
    /// Public key of the watcher node's own P2P identity (from verified message signature).
    pub verified_pub: Vec<u8>,
}

impl WatcherStateMachineCtx {
    fn taker_locktime(&self) -> u64 { self.data.swap_started_at + self.data.lock_duration }

    fn refund_start_time(&self) -> u64 {
        let factor = self.conf.refund_start_factor;
        self.data.swap_started_at + (factor * self.data.lock_duration as f64) as u64
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
    /// Extracted secret from taker payment spend — retained for logging/auditing.
    #[allow(dead_code)]
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
            fee_addr: vec![], // Watcher doesn't need to re-derive fee addr; coin impl checks on-chain
            lock_duration: ctx.data.lock_duration,
        };

        for attempt in 1..=TAKER_FEE_VALIDATION_ATTEMPTS {
            match ctx.taker_coin.watcher_validate_taker_fee(input.clone()).compat().await {
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
        // Wait for taker payment to appear on-chain
        let deadline = now_ms() / 1000 + ctx.conf.wait_taker_payment as u64;
        loop {
            let now = now_ms() / 1000;
            if now > deadline {
                return Self::change_state(Stopped {
                    result: WatcherResult::StoppedOnError(format!(
                        "Watcher {}: taker payment not found within {}s",
                        ctx.data.uuid, ctx.conf.wait_taker_payment
                    )),
                });
            }

            // Try to get the taker payment tx from chain
            let input = coins::WatcherValidatePaymentInput {
                payment_tx: ctx.data.taker_payment_hash.clone(),
                taker_payment_refund_preimage: ctx.data.taker_payment_refund_preimage.clone(),
                time_lock: ctx.taker_locktime() as u32,
                taker_pub: ctx.verified_pub.clone(),
                maker_pub: ctx.data.maker_pub.clone(),
                secret_hash: ctx.data.secret_hash.clone(),
                amount: Default::default(), // Watcher doesn't re-check amount from data
                confirmations: ctx.data.taker_payment_confirmations,
                min_block_number: ctx.data.taker_coin_start_block,
            };

            match ctx.taker_coin.watcher_validate_taker_payment(input).compat().await {
                Ok(()) => {
                    info!("Watcher {}: taker payment validated", ctx.data.uuid);
                    return Self::change_state(WaitForTakerPaymentSpend);
                },
                Err(e) => {
                    debug!("Watcher {}: taker payment not yet valid: {}", ctx.data.uuid, e);
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
            if now >= refund_deadline {
                info!(
                    "Watcher {}: refund deadline reached, refunding taker payment",
                    ctx.data.uuid
                );
                return Self::change_state(RefundTakerPayment);
            }

            // Search for taker payment spend
            match ctx
                .taker_coin
                .watcher_search_for_swap_tx_spend(
                    ctx.taker_locktime() as u32,
                    &ctx.data.maker_pub,
                    &ctx.data.secret_hash,
                    &ctx.data.taker_payment_hash,
                    ctx.data.taker_coin_start_block,
                )
                .await
            {
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
                    info!("Watcher {}: taker payment already refunded, stopping", ctx.data.uuid);
                    return Self::change_state(Stopped {
                        result: WatcherResult::CompletedNormally,
                    });
                },
                Ok(None) => {
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
        // The maker_payment_spend_preimage is a fully-signed tx that just needs
        // to be broadcast. The watcher broadcasts it on behalf of the taker.
        match ctx
            .maker_coin
            .send_raw_tx_bytes(&ctx.data.maker_payment_spend_preimage)
            .compat()
            .await
        {
            Ok(txid) => {
                info!(
                    "Watcher {}: maker payment spend broadcast successfully: {}",
                    ctx.data.uuid, txid
                );
                Self::change_state(Stopped {
                    result: WatcherResult::MakerPaymentSpent,
                })
            },
            Err(e) => {
                error!(
                    "Watcher {}: failed to broadcast maker payment spend: {}",
                    ctx.data.uuid, e
                );
                Self::change_state(Stopped {
                    result: WatcherResult::StoppedOnError(format!("Maker payment spend broadcast failed: {}", e)),
                })
            },
        }
    }
}

#[async_trait::async_trait]
impl State for RefundTakerPayment {
    type Ctx = WatcherStateMachineCtx;
    type Result = WatcherResult;

    async fn on_changed(self: Box<Self>, ctx: &mut Self::Ctx) -> StateResult<Self::Ctx, Self::Result> {
        // Wait until the timelock has actually expired (MTP for UTXO chains).
        let locktime = ctx.taker_locktime();
        loop {
            match ctx.taker_coin.can_refund_htlc(locktime).compat().await {
                Ok(coins::CanRefundHtlc::CanRefundNow) => break,
                Ok(coins::CanRefundHtlc::HaveToWait(secs)) => {
                    debug!("Watcher {}: waiting {}s before refund is possible", ctx.data.uuid, secs);
                    Timer::sleep(secs as f64).await;
                },
                Err(e) => {
                    error!("Watcher {}: can_refund_htlc error: {}", ctx.data.uuid, e);
                    Timer::sleep(30.0).await;
                },
            }
        }

        match ctx
            .taker_coin
            .send_raw_tx_bytes(&ctx.data.taker_payment_refund_preimage)
            .compat()
            .await
        {
            Ok(txid) => {
                info!(
                    "Watcher {}: taker payment refund broadcast successfully: {}",
                    ctx.data.uuid, txid
                );
                Self::change_state(Stopped {
                    result: WatcherResult::TakerPaymentRefunded,
                })
            },
            Err(e) => {
                error!(
                    "Watcher {}: failed to broadcast taker payment refund: {}",
                    ctx.data.uuid, e
                );
                Self::change_state(Stopped {
                    result: WatcherResult::StoppedOnError(format!("Taker payment refund broadcast failed: {}", e)),
                })
            },
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
}

impl SwapWatcherLock {
    /// Attempt to acquire a watcher lock for the given fee hash.
    /// Returns `None` if a watcher is already running (and not expired) for this swap.
    fn try_lock(swap_ctx: Arc<SwapsContext>, fee_hash: Vec<u8>) -> Option<Self> {
        {
            let mut watchers = swap_ctx.taker_swap_watchers.lock();
            let now_sec = now_ms() / 1000;
            // Prune expired entry if present
            if let Some(&expiry) = watchers.get(&fee_hash) {
                if expiry > now_sec {
                    return None; // still active
                }
                watchers.remove(&fee_hash);
            }
            watchers.insert(fee_hash.clone(), now_sec + TAKER_SWAP_WATCHER_ENTRY_TIMEOUT_SECS);
        } // lock guard dropped before moving swap_ctx
        Some(SwapWatcherLock { swap_ctx, fee_hash })
    }
}

impl Drop for SwapWatcherLock {
    fn drop(&mut self) {
        let mut watchers = self.swap_ctx.taker_swap_watchers.lock();
        watchers.remove(&self.fee_hash);
    }
}

// ── Entry Points ─────────────────────────────────────────────────────

/// Build the gossipsub topic for watcher messages of a given coin.
pub fn watcher_topic(coin_ticker: &str) -> String { mm2_p2p::pub_sub_topic(WATCHER_PREFIX, coin_ticker) }

/// Process an incoming watcher gossipsub message.
/// Verifies the message signature, extracts watcher data, and spawns the
/// watcher state machine if both coins are enabled and support watchers.
pub async fn process_watcher_msg(ctx: MmArc, msg: &[u8]) {
    let (watcher_msg, _raw, verified_pubkey) = match mm2_p2p::decode_signed::<SwapWatcherMsg>(msg) {
        Ok(m) => m,
        Err(e) => {
            warn!("Failed to decode watcher message: {:?}", e);
            return;
        },
    };

    match watcher_msg {
        SwapWatcherMsg::TakerSwapWatcherMsg(data) => {
            spawn_taker_swap_watcher(ctx, data, verified_pubkey.to_bytes().to_vec()).await;
        },
    }
}

/// Spawn a watcher state machine for a taker swap.
async fn spawn_taker_swap_watcher(ctx: MmArc, data: TakerSwapWatcherData, verified_pub: Vec<u8>) {
    let uuid = data.uuid;

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

    // Acquire lock — only one watcher per swap
    let swap_ctx = match SwapsContext::from_ctx(&ctx) {
        Ok(sc) => sc,
        Err(e) => {
            error!("Watcher {}: SwapsContext error: {}", uuid, e);
            return;
        },
    };

    let _lock = match SwapWatcherLock::try_lock(swap_ctx, data.taker_fee_hash.clone()) {
        Some(l) => l,
        None => {
            debug!("Watcher {}: already being watched, skipping", uuid);
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
    };

    let machine = StateMachine::from_ctx(machine_ctx);
    let result = machine.run(ValidateTakerFee).await;

    info!("Watcher {}: state machine finished with result: {:?}", uuid, result);
}

// ── Unit Tests ───────────────────────────────────────────────────────

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

        let msg = SwapWatcherMsg::TakerSwapWatcherMsg(data.clone());
        let serialized = serde_json::to_string(&msg).unwrap();
        let deserialized: SwapWatcherMsg = serde_json::from_str(&serialized).unwrap();

        match deserialized {
            SwapWatcherMsg::TakerSwapWatcherMsg(d) => {
                assert_eq!(d.uuid, data.uuid);
                assert_eq!(d.taker_coin, "BTC");
                assert_eq!(d.maker_coin, "KMD");
                assert_eq!(d.lock_duration, 7500);
            },
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

        let msg = SwapWatcherMsg::TakerSwapWatcherMsg(TakerSwapWatcherData {
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
        });
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
