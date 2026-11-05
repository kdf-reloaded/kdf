//! Bounded `eth_getLogs` windows of the EVM swap paths and the V1 wait for a payment's spend
//! (CRD ch.17 R17.8.1 -- R17.8.3).
//!
//! # Purpose
//!
//! Every `eth_getLogs` that an EVM swap path issues covers at most `W` blocks, the inclusive range
//! `[from, from + W - 1]`. `W` starts at the coin's `logs_block_range` and shrinks, for the rest of the coin's
//! session, when a node refuses a range. The wait for one of the node's own V1 payments to be spent searches
//! forward from a cursor and leaves at its deadline whatever the nodes answer.
//!
//! # Public exports
//!
//! - [`LogsWindow`]: the per-coin window state; a token shares its platform's.
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
//! - The spend wait checks its deadline on every iteration, after a failed query as after a successful one.
//! - The log lines name a node by host only: the all-failed error of the transport already carries the host-only
//!   label of the node whose answer it renders (CRD ch.35 R35.1.14), and nothing else about a node is added.

use super::*;
use alloy::rpc::json_rpc::RpcError;
use common::log::{debug, info, warn};
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

    #[test]
    fn the_node_name_the_transport_appends_takes_no_part_in_the_classification() {
        assert_eq!(
            range_refusal(-32602, "invalid params (node http://range-limit.example:8545)"),
            None
        );
        assert!(range_refusal(-32602, "block range too large (node http://a.example)").is_some());
    }
}
