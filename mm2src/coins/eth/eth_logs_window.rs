//! Bounded `eth_getLogs` windows of the EVM swap paths and the V1 wait for a payment's spend
//! (CRD ch.17 R17.8.1 -- R17.8.3a).
//!
//! # Purpose
//!
//! Every `eth_getLogs` that an EVM swap path issues covers at most `W` blocks, the inclusive range
//! `[from, from + W - 1]`. `W` starts at the coin's `logs_block_range` and shrinks, for the rest of the coin's
//! session, when a node refuses a range. The wait for one of the node's own V1 payments to be spent searches
//! forward from a cursor and leaves at its deadline whatever the nodes answer. The search for the spend or the
//! refund of a V1 payment scans from the tip downwards and remembers, per payment, the blocks it has scanned
//! (R17.8.3a).
//!
//! # Public exports
//!
//! - [`LogsWindow`]: the per-coin window state; a token shares its platform's.
//! - [`PaymentScans`]: the blocks already scanned for each payment, per coin session (R17.8.3a); it lives in the
//!   [`LogsWindow`] state, so a token shares it with its platform.
//! - [`range_refusal`]: classifies a JSON-RPC error as a refusal of the range's size (R17.8.2).
//! - `EthCoinImpl::logs_window`, `EthCoinImpl::log_window_end`, `EthCoinImpl::logs_querier`: the current `W`,
//!   the end of the window that starts at a block, and the owned handle that runs the bounded query.
//! - `EthCoin::wait_for_payment_spend`: the V1 spend wait (R17.8.3).
//!
//! # Invariants
//!
//! - `W` is never above `logs_block_range` and never below 1.
//! - Windows of one range neither overlap nor skip a block.
//! - Only a refusal of the range's size shrinks `W`; every other failure is returned (a bare -32602 without a
//!   range or limit phrase can come from a malformed request of our own).
//! - The cursors of the spend wait and of the V2 search go back to their start at most once per minute, after a
//!   poll that reached the tip without a result.
//! - The spend wait checks its deadline on every iteration, after a failed query as after a successful one.
//! - A scanned interval never contains a block closer than `REORG_MARGIN_BLOCKS` below the tip that the scan saw.
//! - A search that scans its whole range without a result drops the payment's scanned blocks (a lagging node can have
//!   answered a window as empty), not its receipt block.
//! - The scanned blocks are bounded per payment (`SCANNED_INTERVALS_CAP` intervals) and in all
//!   (`SCANNED_PAYMENTS_CAP` payments, least recently used first out). Forgetting only costs a rescan.
//! - The log lines name a node by host only: the all-failed error of the transport already carries the host-only
//!   label of the node whose answer it renders (CRD ch.35 R35.1.14), and nothing else about a node is added.

use super::*;
use alloy::rpc::json_rpc::RpcError;
use common::log::{debug, info, warn};
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;

/// The JSON-RPC code a node answers when a request is too large (EIP-1474 "limit exceeded").
const LIMIT_EXCEEDED_CODE: i64 = -32005;

/// The phrases of an error message that name a block range or a limit (R17.8.2), lower case.
const REFUSAL_PHRASES: [&str; 5] = ["range", "limit", "too large", "exceed", "more than"];

/// How many of the newest blocks of a poll the spend wait's cursor stays behind (CRD ch.17 R17.8.3). A reorganization
/// can replace the tip block, or one of the newest few, with a block that carries the spend; the next poll scans
/// these blocks again, so such a spend is still found.
pub(crate) const REORG_MARGIN_BLOCKS: u64 = 12;

/// Seconds between two polls of the spend wait.
pub(crate) const SPEND_WAIT_POLL_S: f64 = 5.;

/// The least time between two returns of a wait's cursor to its start (CRD ch.17 R17.8.3), in milliseconds. A
/// window that a lagging node answered as empty is searched again within this time.
const CURSOR_RESET_EVERY_MS: u64 = 60_000;

/// How many payments' scanned blocks a coin session keeps (CRD ch.17 R17.8.3a). The least recently used payment is
/// forgotten first; forgetting a payment costs a rescan of its range, nothing else.
pub(crate) const SCANNED_PAYMENTS_CAP: usize = 1024;

/// How many disjoint scanned intervals one payment keeps. A scan cut short leaves its newest windows as a separate
/// interval above the older one; the next call closes the gap, so more than a few are not expected. Past the cap the
/// narrowest interval is forgotten.
const SCANNED_INTERVALS_CAP: usize = 16;

/// What identifies a V1 payment in [`PaymentScans`]: the swap contract and the payment's swap id.
pub(crate) type PaymentKey = (Address, Vec<u8>);

/// The state of the window `W` of one coin. Clones share it, so a token and its platform share one `W` (they
/// ask the same nodes).
#[derive(Clone, Debug, Default)]
pub(crate) struct LogsWindow(Arc<LogsWindowState>);

#[derive(Debug, Default)]
struct LogsWindowState {
    /// The reduced `W`; 0 while `W` has not been reduced, in which case it is the coin's `logs_block_range`.
    reduced_to: AtomicU64,
    /// Whether the first reduction has been logged at WARN.
    warned: AtomicBool,
    /// Whether the window has been logged at INFO on the coin's first bounded query.
    announced: AtomicBool,
    /// The blocks scanned for each V1 payment (R17.8.3a). The lock is never held across an await.
    payments: Mutex<PaymentScans>,
    /// The time between two returns of a wait's cursor to its start, in milliseconds; 0 while it has the
    /// default `CURSOR_RESET_EVERY_MS`. Only a test sets it.
    cursor_reset_ms: AtomicU64,
}

/// The blocks already scanned for the spend or the refund of each V1 payment, for the rest of the coin's session
/// (CRD ch.17 R17.8.3a).
#[derive(Debug, Default)]
struct PaymentScans {
    payments: HashMap<PaymentKey, PaymentScan>,
    /// Counts the uses, to find the least recently used payment.
    clock: u64,
}

#[derive(Debug, Default)]
struct PaymentScan {
    /// The block of the payment's receipt, once it has been read.
    receipt_block: Option<u64>,
    /// The scanned blocks as disjoint, non-adjacent inclusive intervals, ascending.
    scanned: Vec<(u64, u64)>,
    last_used: u64,
}

impl PaymentScans {
    fn touch(&mut self, key: &PaymentKey) -> &mut PaymentScan {
        self.clock += 1;
        let clock = self.clock;
        if !self.payments.contains_key(key) && self.payments.len() >= SCANNED_PAYMENTS_CAP {
            let least_recent = self
                .payments
                .iter()
                .min_by_key(|(_, scan)| scan.last_used)
                .map(|(key, _)| key.clone());
            if let Some(least_recent) = least_recent {
                self.payments.remove(&least_recent);
            }
        }
        let scan = self.payments.entry(key.clone()).or_default();
        scan.last_used = clock;
        scan
    }
}

impl PaymentScan {
    /// Adds the inclusive interval `[from, to]` to the scanned blocks.
    fn mark(&mut self, from: u64, to: u64) {
        self.scanned.push((from, to));
        self.scanned.sort_unstable();
        let mut merged: Vec<(u64, u64)> = Vec::with_capacity(self.scanned.len());
        for &(lo, hi) in &self.scanned {
            match merged.last_mut() {
                Some(last) if lo <= last.1.saturating_add(1) => last.1 = last.1.max(hi),
                _ => merged.push((lo, hi)),
            }
        }
        while merged.len() > SCANNED_INTERVALS_CAP {
            if let Some(narrowest) = (0..merged.len()).min_by_key(|i| merged[*i].1 - merged[*i].0) {
                merged.remove(narrowest);
            }
        }
        self.scanned = merged;
    }

    /// The highest run of unscanned blocks in `[floor, end]`, as `(lowest, highest)`; `None` when every block of
    /// the range is scanned.
    fn highest_unscanned(&self, floor: u64, end: u64) -> Option<(u64, u64)> {
        if end < floor {
            return None;
        }
        let mut highest = end;
        for &(lo, hi) in self.scanned.iter().rev() {
            if hi < floor {
                break;
            }
            if lo > highest {
                continue;
            }
            if hi >= highest {
                // The interval covers the top of what is left; continue below it.
                if lo <= floor {
                    return None;
                }
                highest = lo - 1;
                continue;
            }
            return Some((hi.saturating_add(1).max(floor), highest));
        }
        Some((floor, highest))
    }
}

impl LogsWindow {
    fn with_scans<R>(&self, f: impl FnOnce(&mut PaymentScans) -> R) -> R {
        let mut scans = self.0.payments.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&mut scans)
    }

    /// The block of the payment's receipt, when an earlier call has read it.
    pub(crate) fn receipt_block(&self, key: &PaymentKey) -> Option<u64> {
        self.with_scans(|scans| scans.payments.get(key).and_then(|scan| scan.receipt_block))
    }

    /// Forgets the blocks scanned for the payment, not its receipt block (R17.8.3a): a call that scanned the whole
    /// range without a result can have been misled by a node that lagged behind the tip.
    pub(crate) fn forget_scanned(&self, key: &PaymentKey) {
        self.with_scans(|scans| {
            if let Some(scan) = scans.payments.get_mut(key) {
                scan.scanned.clear();
            }
        });
    }

    /// The least time between two returns of a wait's cursor to its start, in milliseconds (R17.8.3).
    pub(crate) fn cursor_reset_every_ms(&self) -> u64 {
        match self.0.cursor_reset_ms.load(AtomicOrdering::SeqCst) {
            0 => CURSOR_RESET_EVERY_MS,
            set => set,
        }
    }

    /// Sets the time between two cursor resets (tests).
    #[cfg(test)]
    pub(crate) fn set_cursor_reset_every_ms(&self, ms: u64) {
        self.0.cursor_reset_ms.store(ms, AtomicOrdering::SeqCst);
    }

    /// Remembers the block of the payment's receipt.
    pub(crate) fn set_receipt_block(&self, key: &PaymentKey, block: u64) {
        self.with_scans(|scans| scans.touch(key).receipt_block = Some(block));
    }

    /// The highest run of blocks in `[floor, end]` that have not been scanned for the payment, as
    /// `(lowest, highest)`; `None` when the whole range has been scanned.
    pub(crate) fn highest_unscanned(&self, key: &PaymentKey, floor: u64, end: u64) -> Option<(u64, u64)> {
        self.with_scans(|scans| match scans.payments.get(key) {
            Some(scan) => scan.highest_unscanned(floor, end),
            None => (floor <= end).then_some((floor, end)),
        })
    }

    /// Records the inclusive range `[from, to]` as scanned for the payment, without error and without a spend or
    /// refund. Blocks closer than `REORG_MARGIN_BLOCKS` below `tip` are never recorded (R17.8.3a): a
    /// reorganization can replace them, so the next call scans them again.
    pub(crate) fn mark_scanned(&self, key: &PaymentKey, from: u64, to: u64, tip: u64) {
        let to = to.min(tip.saturating_sub(REORG_MARGIN_BLOCKS));
        // With a tip below the margin nothing is old enough to record (`tip - margin` would be block 0 itself).
        if tip < REORG_MARGIN_BLOCKS || from > to {
            return;
        }
        self.with_scans(|scans| scans.touch(key).mark(from, to));
    }

    /// The scanned intervals of a payment (tests).
    #[cfg(test)]
    pub(crate) fn scanned_intervals(&self, key: &PaymentKey) -> Vec<(u64, u64)> {
        self.with_scans(|scans| {
            scans
                .payments
                .get(key)
                .map(|scan| scan.scanned.clone())
                .unwrap_or_default()
        })
    }

    /// How many payments are remembered (tests).
    #[cfg(test)]
    pub(crate) fn remembered_payments(&self) -> usize { self.with_scans(|scans| scans.payments.len()) }
}

/// A node's refusal of a range's size (R17.8.2).
#[derive(Debug, PartialEq)]
pub(crate) struct RangeRefusal {
    /// The block count the message names, if it names one.
    pub(crate) named_blocks: Option<u64>,
}

/// Classifies a JSON-RPC error as a refusal of the range's size, or `None` when it is an ordinary failure. An
/// error refuses the size when its code is -32005, or when its message names a block range or a limit. A bare
/// -32602 without such a phrase is not a refusal.
pub(crate) fn range_refusal(code: i64, message: &str) -> Option<RangeRefusal> {
    let lower = without_node_suffix(message).to_lowercase();
    let refuses = code == LIMIT_EXCEEDED_CODE || REFUSAL_PHRASES.iter().any(|phrase| lower.contains(phrase));
    refuses.then(|| RangeRefusal {
        named_blocks: named_block_count(&lower),
    })
}

/// The transport renders the node an error came from as a trailing ` (node <host>)`; the host is the node's, not
/// part of its answer, and must not take part in the classification.
fn without_node_suffix(message: &str) -> &str {
    match message.rfind(" (node ") {
        Some(at) if message.ends_with(')') => &message[..at],
        _ => message,
    }
}

/// The count of blocks a lower-cased message names: a number directly followed by a word that starts with
/// "block" ("limited to 100 blocks", "up to a 2k block range", "10,000 blocks"). A number that counts something
/// else ("more than 10000 results") is not a block count.
fn named_block_count(lower: &str) -> Option<u64> {
    let words: Vec<&str> = lower.split_whitespace().collect();
    words
        .windows(2)
        .filter(|pair| pair[1].starts_with("block"))
        .find_map(|pair| parse_count(pair[0]))
        .filter(|count| *count >= 1)
}

fn parse_count(word: &str) -> Option<u64> {
    let word = word.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    let (digits, multiplier) = match word.strip_suffix('k') {
        Some(digits) => (digits, 1000),
        None => (word, 1),
    };
    let digits: String = digits.chars().filter(|c| *c != ',').collect();
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u64>().ok()?.checked_mul(multiplier)
}

impl EthCoinImpl {
    /// The current window `W`: the coin's `logs_block_range` until a node refuses a range, then the reduced
    /// value; never above `logs_block_range`, never below 1.
    pub(crate) fn logs_window(&self) -> u64 { effective_window(&self.logs_window, self.logs_block_range) }

    /// The last block of the window that starts at `from` and is cut off at `tip`: `from + W - 1`, or `tip` when
    /// that is lower.
    pub(crate) fn log_window_end(&self, from: u64, tip: u64) -> u64 {
        tip.min(from.saturating_add(self.logs_window() - 1))
    }

    /// An owned handle that runs bounded `eth_getLogs` queries of this coin.
    pub(crate) fn logs_querier(&self) -> LogsQuerier {
        LogsQuerier {
            provider: self.alloy_provider(),
            window: self.logs_window.clone(),
            cap: self.logs_block_range,
            ticker: self.ticker.clone(),
        }
    }
}

fn effective_window(window: &LogsWindow, cap: u64) -> u64 {
    let cap = cap.max(1);
    match window.0.reduced_to.load(AtomicOrdering::SeqCst) {
        0 => cap,
        reduced => reduced.min(cap).max(1),
    }
}

/// Everything a bounded `eth_getLogs` query needs, owned, so that the query future does not borrow the coin.
pub(crate) struct LogsQuerier {
    provider: super::alloy_compat::KdfProvider,
    window: LogsWindow,
    cap: u64,
    ticker: String,
}

impl LogsQuerier {
    /// The logs of `address` whose first topic is `topic`, from block `from` to block `to`, both inclusive
    /// (R17.8.1). The range is searched as consecutive windows of at most `W` blocks. A window that a node refuses
    /// for its size shrinks `W` (R17.8.2) and is retried at once; any other failure is returned.
    pub(crate) async fn get_logs(self, topic: H256, address: Address, from: u64, to: u64) -> Result<Vec<Log>, String> {
        use crate::eth::alloy_compat::assert_send_future;
        use alloy::providers::Provider;

        if !self.window.0.announced.swap(true, AtomicOrdering::SeqCst) {
            info!(
                "[{}] eth_getLogs window is {} blocks",
                self.ticker,
                effective_window(&self.window, self.cap)
            );
        }
        let mut logs = Vec::new();
        let mut start = from;
        while start <= to {
            let end = to.min(start.saturating_add(effective_window(&self.window, self.cap) - 1));
            let filter = FilterBuilder::default()
                .topics(Some(vec![topic]), None, None, None)
                .from_block(BlockNumber::Number(start))
                .to_block(BlockNumber::Number(end))
                .address(vec![address])
                .build();
            match assert_send_future(self.provider.client().request::<_, Vec<Log>>("eth_getLogs", (filter,))).await {
                Ok(mut chunk) => {
                    logs.append(&mut chunk);
                    match end.checked_add(1) {
                        Some(next) => start = next,
                        None => break,
                    }
                },
                Err(e) => {
                    let refusal = match &e {
                        RpcError::ErrorResp(payload) => range_refusal(payload.code, &payload.message)
                            .map(|refusal| (refusal, format!("error {}: {}", payload.code, payload.message))),
                        _ => None,
                    };
                    match refusal {
                        Some((refusal, answer)) if self.shrink(&refusal, &answer) => continue,
                        _ => return Err(ERRL!("{}", e)),
                    }
                },
            }
        }
        Ok(logs)
    }

    /// Shrinks `W` after `refusal` (R17.8.2): to the block count the message names when that is below `W`,
    /// otherwise to half of `W`; never below 1. `false` when `W` cannot shrink any more, in which case the
    /// failure is an ordinary one. `answer` is the error as the transport rendered it; it names the node by host
    /// only.
    fn shrink(&self, refusal: &RangeRefusal, answer: &str) -> bool {
        let current = effective_window(&self.window, self.cap);
        if current <= 1 {
            return false;
        }
        let new = match refusal.named_blocks {
            Some(named) if named < current => named,
            _ => (current / 2).max(1),
        };
        let cap = self.cap;
        let lowered = self
            .window
            .0
            .reduced_to
            .fetch_update(AtomicOrdering::SeqCst, AtomicOrdering::SeqCst, |raw| {
                let now = match raw {
                    0 => cap.max(1),
                    reduced => reduced.min(cap.max(1)).max(1),
                };
                (new < now).then_some(new)
            })
            .is_ok();
        if !lowered {
            // Another request lowered `W` first; the retry runs with its value if that is below ours.
            return effective_window(&self.window, self.cap) < current;
        }
        info!(
            "[{}] eth_getLogs window is {} blocks (was {}) after {}",
            self.ticker, new, current, answer
        );
        if !self.window.0.warned.swap(true, AtomicOrdering::SeqCst) {
            warn!(
                "[{}] a node refused the eth_getLogs block range; the window is now {} blocks for this session: {}",
                self.ticker, new, answer
            );
        }
        true
    }
}

/// Why one poll of the spend wait did not produce the spend.
enum PollError {
    /// The poll failed or found nothing usable; the wait goes on to its deadline check.
    Retry(String),
    /// The wait cannot go on.
    Fatal(crate::TransactionErr),
}

/// What the spend wait remembers between its polls.
struct SpendWait {
    /// The first block that has not been searched yet.
    cursor: u64,
    /// Where the cursor starts: the receipt's block once it is known, else the search's start block. The cursor
    /// goes back here at most once per `LogsWindow::cursor_reset_every_ms`.
    start: u64,
    /// When (UNIX milliseconds) the cursor last went back to its start, or the wait began.
    last_reset_ms: u64,
    /// Whether the payment's receipt has been read; the cursor then starts at the receipt's block.
    receipt_read: bool,
    /// The transaction a window returned as the spend, while it is being fetched.
    found: Option<H256>,
    /// Whether the contract-state WARN has been logged for this wait.
    spent_warned: bool,
}

impl EthCoin {
    /// Waits until the V1 payment `payment_tx` (swap id `id`) is spent, or `wait_until` (UNIX seconds) passes
    /// (CRD ch.17 R17.8.3). Every `poll_every` seconds it searches the logs forward from a cursor, in windows of
    /// at most `W` blocks; the cursor starts at the block of the payment's receipt when that is known, else at
    /// `from_block`, and moves past every window scanned without error, except for the newest `REORG_MARGIN_BLOCKS`
    /// blocks of the poll, which the next poll scans again. The deadline is checked after every
    /// poll, whether or not its queries failed.
    pub(crate) async fn wait_for_payment_spend(
        &self,
        payment_tx: &SignedEthTx,
        id: &[u8],
        swap_contract_address: Address,
        wait_until: u64,
        from_block: u64,
        poll_every: f64,
    ) -> Result<TransactionEnum, crate::TransactionErr> {
        let mut wait = SpendWait {
            cursor: from_block,
            start: from_block,
            last_reset_ms: now_ms(),
            receipt_read: false,
            found: None,
            spent_warned: false,
        };
        loop {
            match self
                .poll_payment_spend(&mut wait, payment_tx, id, swap_contract_address, wait_until)
                .await
            {
                Ok(Some(spend)) => return Ok(spend),
                Ok(None) => (),
                Err(PollError::Fatal(e)) => return Err(e),
                Err(PollError::Retry(e)) => {
                    log!("Error " (e) " waiting for the spend of the " (self.ticker()) " payment " [payment_tx.tx_hash()])
                },
            }

            if now_ms() / 1000 > wait_until {
                // A spend that a window returned but whose transaction could not be fetched is named, so the log
                // shows that the secret was on the chain.
                let known_spend = match wait.found {
                    Some(spend_hash) => format!("(spend {:02x} found but not fetched)", spend_hash),
                    None => String::new(),
                };
                return TX_PLAIN_ERR!(
                    "Waited too long until {} for transaction {:?} to be spent {}",
                    wait_until,
                    payment_tx,
                    known_spend,
                );
            }
            Timer::sleep(poll_every).await;
        }
    }

    /// The block of the receipt of `payment_tx`, or `None` when the node does not know it yet or the read failed
    /// (the caller then falls back to its own start block).
    pub(crate) async fn payment_receipt_block(&self, payment_tx: &SignedEthTx) -> Option<u64> {
        use crate::eth::alloy_compat::assert_send_future;
        use alloy::providers::Provider;

        let provider = self.alloy_provider();
        let payment_hash = alloy::primitives::B256::from_slice(&payment_tx.tx_hash().0);
        match assert_send_future(provider.get_transaction_receipt(payment_hash)).await {
            Ok(receipt) => receipt.and_then(|receipt| receipt.block_number),
            Err(e) => {
                debug!("[{}] couldn't read the payment's receipt: {}", self.ticker(), e);
                None
            },
        }
    }

    async fn poll_payment_spend(
        &self,
        wait: &mut SpendWait,
        payment_tx: &SignedEthTx,
        id: &[u8],
        swap_contract_address: Address,
        wait_until: u64,
    ) -> Result<Option<TransactionEnum>, PollError> {
        if let Some(spend_hash) = wait.found {
            return self.fetch_spend_tx(spend_hash).await;
        }

        let scan = self
            .scan_for_spend(wait, payment_tx, id, swap_contract_address, wait_until)
            .await;
        if let Ok(Some(spend_hash)) = scan {
            wait.found = Some(spend_hash);
            return self.fetch_spend_tx(spend_hash).await;
        }

        // The contract's state is a signal only (it keeps no secret and no block), read on every poll.
        if !wait.spent_warned {
            let state = self
                .payment_status(swap_contract_address, Token::FixedBytes(id.to_vec()))
                .compat()
                .await;
            match state {
                Ok(state) if state == U256::from(PAYMENT_STATE_SPENT) => {
                    wait.spent_warned = true;
                    warn!(
                        "[{}] the swap contract reports the payment {:?} as spent, but no log window has returned the spend yet",
                        self.ticker(),
                        payment_tx.tx_hash()
                    );
                },
                Ok(_) => (),
                Err(e) => debug!("[{}] couldn't read the payment state: {}", self.ticker(), e),
            }
        }
        scan.map(|_| None).map_err(PollError::Retry)
    }

    /// Searches the windows from the cursor to the tip for the spend of payment `id`. The cursor moves past each
    /// window that was scanned without error, but never above `tip - REORG_MARGIN_BLOCKS + 1`, so the newest blocks
    /// are scanned again by the next poll. Returns the hash of the transaction that spent the payment. A poll
    /// that is far behind the tip scans many windows, so the deadline is checked before every window after the
    /// first; a spend found in a window still wins over the deadline, because it returns before the next check.
    async fn scan_for_spend(
        &self,
        wait: &mut SpendWait,
        payment_tx: &SignedEthTx,
        id: &[u8],
        swap_contract_address: Address,
        wait_until: u64,
    ) -> Result<Option<H256>, String> {
        use crate::eth::alloy_compat::assert_send_future;
        use alloy::providers::Provider;

        let tip = self.current_block().compat().await?;

        if !wait.receipt_read {
            // The spend cannot precede the payment: the search starts at the payment's block when it is known.
            let provider = self.alloy_provider();
            let payment_hash = alloy::primitives::B256::from_slice(&payment_tx.tx_hash().0);
            match assert_send_future(provider.get_transaction_receipt(payment_hash)).await {
                Ok(Some(receipt)) => {
                    if let Some(block) = receipt.block_number {
                        wait.cursor = wait.cursor.max(block);
                        wait.start = wait.start.max(block);
                        wait.receipt_read = true;
                    }
                },
                Ok(None) => (),
                Err(e) => debug!("[{}] couldn't read the payment's receipt: {}", self.ticker(), e),
            }
        }

        // The poll scans up to the tip, but the cursor it leaves behind stays `REORG_MARGIN_BLOCKS` short of it, and
        // never moves back.
        let cursor_cap = tip.saturating_sub(REORG_MARGIN_BLOCKS - 1);
        let mut next = wait.cursor;
        let mut scanned = 0usize;
        while next <= tip {
            if scanned > 0 && now_ms() / 1000 > wait_until {
                return ERR!("the deadline passed while scanning, the next block is {}", next);
            }
            scanned += 1;
            let to_block = self.log_window_end(next, tip);
            let events = assert_send_future(self.spend_events(swap_contract_address, next, to_block).compat()).await?;
            if let Some(event) = events.iter().find(|event| event.data.0.get(..32) == Some(id)) {
                return match event.transaction_hash {
                    Some(spend_hash) => Ok(Some(spend_hash)),
                    None => ERR!("The ReceiverSpent event of the payment has no tx_hash"),
                };
            }
            wait.cursor = wait.cursor.max(to_block.saturating_add(1).min(cursor_cap));
            if to_block >= tip {
                break;
            }
            next = to_block + 1;
        }
        // The poll reached the tip without the spend. A node that lagged behind the tip answers a window above its
        // head as empty, and the cursor has moved past that window; so at most once per
        // `cursor_reset_every_ms` the cursor goes back to its start and the next poll scans the range again.
        let now = now_ms();
        if now.saturating_sub(wait.last_reset_ms) >= self.logs_window.cursor_reset_every_ms() {
            wait.cursor = wait.start;
            wait.last_reset_ms = now;
        }
        Ok(None)
    }

    async fn fetch_spend_tx(&self, spend_hash: H256) -> Result<Option<TransactionEnum>, PollError> {
        // LP-17: alloy `Provider::get_transaction_by_hash` + `signed_tx_from_alloy_tx` round-trip replaces the
        // web3 `transaction(...)` + `signed_tx_from_web3_tx` pair. Same wire RPC method (`eth_getTransactionByHash`).
        use crate::eth::alloy_compat::assert_send_future;
        use alloy::providers::Provider;

        let provider = self.alloy_provider();
        let alloy_hash = alloy::primitives::B256::from_slice(&spend_hash.0);
        match assert_send_future(provider.get_transaction_by_hash(alloy_hash)).await {
            Ok(Some(transaction)) => match signed_tx_from_alloy_tx(transaction) {
                Ok(tx) => Ok(Some(TransactionEnum::from(tx))),
                Err(e) => Err(PollError::Fatal(crate::TransactionErr::Plain(format!(
                    "{}:{}] {:?}",
                    file!(),
                    line!(),
                    e
                )))),
            },
            Ok(None) => Err(PollError::Retry(format!("Tx {:?} not found yet", spend_hash))),
            Err(e) => Err(PollError::Retry(format!("Get tx {:?} error {}", spend_hash, e))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_code_32005_always_refuses_the_size() {
        assert_eq!(
            range_refusal(-32005, "anything at all"),
            Some(RangeRefusal { named_blocks: None })
        );
    }

    #[test]
    fn a_range_or_limit_phrase_refuses_the_size_under_any_code() {
        for message in [
            "block range too large",
            "Query exceeds the maximum",
            "query returned more than 10000 results",
            "request limit reached",
            "eth_getLogs is limited to a 10,000 block range",
        ] {
            assert!(range_refusal(-32602, message).is_some(), "{message}");
            assert!(range_refusal(-32000, message).is_some(), "{message}");
        }
    }

    #[test]
    fn a_bare_invalid_params_is_an_ordinary_failure() {
        assert_eq!(range_refusal(-32602, "invalid params"), None);
        assert_eq!(
            range_refusal(-32602, "invalid argument 0: hex string without 0x prefix"),
            None
        );
        assert_eq!(range_refusal(-32000, "header not found"), None);
    }

    #[test]
    fn the_block_count_a_message_names_is_read() {
        let named = |message: &str| range_refusal(-32602, message).unwrap().named_blocks;
        assert_eq!(named("eth_getLogs is limited to 100 blocks"), Some(100));
        assert_eq!(named("ranges over 10000 blocks are not supported"), Some(10_000));
        assert_eq!(named("you can make requests with up to a 2K block range"), Some(2000));
        assert_eq!(named("limited to 10,000 blocks."), Some(10_000));
        assert_eq!(named("block range too large"), None);
        // A number that counts something else is not a block count.
        assert_eq!(named("query returned more than 10000 results"), None);
    }

    fn test_key(id: u8) -> PaymentKey { (Address::zero(), vec![id; 32]) }

    #[test]
    fn scanned_intervals_merge_when_they_overlap_or_touch() {
        let mut scan = PaymentScan::default();
        scan.mark(100, 199);
        scan.mark(300, 399);
        assert_eq!(scan.scanned, [(100, 199), (300, 399)]);
        scan.mark(200, 299);
        assert_eq!(scan.scanned, [(100, 399)]);
        scan.mark(150, 160);
        scan.mark(0, 100);
        assert_eq!(scan.scanned, [(0, 399)]);
    }

    #[test]
    fn the_highest_unscanned_run_is_found_from_the_top() {
        let mut scan = PaymentScan::default();
        assert_eq!(scan.highest_unscanned(10, 50), Some((10, 50)));
        assert_eq!(scan.highest_unscanned(51, 50), None);
        scan.mark(20, 30);
        scan.mark(40, 45);
        assert_eq!(scan.highest_unscanned(10, 50), Some((46, 50)));
        assert_eq!(scan.highest_unscanned(10, 45), Some((31, 39)));
        assert_eq!(scan.highest_unscanned(10, 39), Some((31, 39)));
        assert_eq!(scan.highest_unscanned(10, 30), Some((10, 19)));
        assert_eq!(scan.highest_unscanned(10, 19), Some((10, 19)));
        assert_eq!(scan.highest_unscanned(25, 45), Some((31, 39)));
        assert_eq!(scan.highest_unscanned(25, 30), None);
        assert_eq!(scan.highest_unscanned(31, 39), Some((31, 39)));
        assert_eq!(scan.highest_unscanned(46, 50), Some((46, 50)));
        // A range below every scanned block is whole.
        assert_eq!(scan.highest_unscanned(0, 5), Some((0, 5)));
    }

    #[test]
    fn the_newest_blocks_are_never_recorded_as_scanned() {
        let window = LogsWindow::default();
        let key = test_key(1);
        // Everything up to the tip, but the newest `REORG_MARGIN_BLOCKS` blocks stay unrecorded.
        window.mark_scanned(&key, 0, 1_000, 1_000);
        assert_eq!(window.scanned_intervals(&key), [(0, 1_000 - REORG_MARGIN_BLOCKS)]);
        assert_eq!(
            window.highest_unscanned(&key, 0, 1_000),
            Some((1_000 - REORG_MARGIN_BLOCKS + 1, 1_000))
        );
        // A window inside the margin records nothing; neither does a tip below the margin.
        let other = test_key(2);
        window.mark_scanned(&other, 990, 1_000, 1_000);
        window.mark_scanned(&other, 0, 5, 5);
        assert_eq!(window.scanned_intervals(&other), []);
        assert_eq!(window.highest_unscanned(&other, 0, 5), Some((0, 5)));
    }

    #[test]
    fn forgetting_the_scanned_blocks_keeps_the_receipt_block() {
        let window = LogsWindow::default();
        let key = test_key(3);
        window.set_receipt_block(&key, 77);
        window.mark_scanned(&key, 0, 500, 1_000);
        assert_eq!(window.scanned_intervals(&key), [(0, 500)]);
        window.forget_scanned(&key);
        assert_eq!(window.scanned_intervals(&key), []);
        assert_eq!(window.receipt_block(&key), Some(77));
        assert_eq!(window.highest_unscanned(&key, 0, 1_000), Some((0, 1_000)));
        // An unknown payment is not an error.
        window.forget_scanned(&test_key(4));
    }

    #[test]
    fn the_memory_of_scanned_blocks_is_bounded() {
        let mut scan = PaymentScan::default();
        for i in 0..(SCANNED_INTERVALS_CAP as u64 + 10) {
            scan.mark(i * 1_000, i * 1_000 + 10 + i);
        }
        assert_eq!(scan.scanned.len(), SCANNED_INTERVALS_CAP);

        let window = LogsWindow::default();
        for id in 0..(SCANNED_PAYMENTS_CAP + 5) {
            let key = (Address::zero(), (id as u64).to_be_bytes().to_vec());
            window.set_receipt_block(&key, id as u64);
        }
        assert_eq!(window.remembered_payments(), SCANNED_PAYMENTS_CAP);
        // The least recently used payments went first.
        assert_eq!(
            window.receipt_block(&(Address::zero(), 0u64.to_be_bytes().to_vec())),
            None
        );
        let newest = SCANNED_PAYMENTS_CAP as u64 + 4;
        assert_eq!(
            window.receipt_block(&(Address::zero(), newest.to_be_bytes().to_vec())),
            Some(newest)
        );
    }

    #[test]
    fn the_node_name_the_transport_appends_takes_no_part_in_the_classification() {
        assert_eq!(
            range_refusal(-32602, "invalid params (node http://range-limit.example:8545)"),
            None
        );
        assert!(range_refusal(-32602, "block range too large (node http://a.example)").is_some());
    }
}
