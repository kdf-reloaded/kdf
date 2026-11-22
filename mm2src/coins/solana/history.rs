//! # Purpose
//! The transaction history of a Solana platform coin and of the SPL tokens it serves (CRD ch.40 §40.6C).
//!
//! One writer per platform lists the signatures of each watched account, reads every new transaction once, and
//! stores the rows of the platform coin and of each served token in the per-ticker history lists that both
//! `my_tx_history` methods read (§38.5A). It runs on native and browser builds.
//!
//! # Sources
//! - `getSignaturesForAddress` (limit 1000, `finalized`, `before` = the oldest signature of the previous page) for
//!   the platform address and for every token account the platform address owns for a served mint.
//! - `getTransaction` (`base64`, `finalized`, version 0) once per new signature; `getBlock` (`none`) once per new
//!   slot for the block height.
//! - `getTokenAccountsByOwner` with the mint filter, once per served token on each pass.
//!
//! # Invariants
//! - Finalized transactions only, so a stored row is never changed or removed.
//! - The first pass of a process walks each watched account's listing to its end, which is the first empty page (a
//!   node may serve fewer entries than asked); a later pass stops at the first page that has no new signature. A
//!   walk that failed leaves its account unwalked, so no gap is left.
//! - At most four requests per second (a pause before every request).
//! - A transaction that cannot be read is logged and left out; it never stops the pass (R40.6C.11).
//! - No log or error text carries a node URL beyond its host: the node errors arrive scrubbed.

use super::history_decode::{parse_transaction, ParsedTx, TokenSpec};
use super::rpc_client::{RpcError, SignatureInfo, TokenAccountsFilter};
use super::spl::SplToken;
use super::SolanaCoin;
use crate::coin_work::carry_current_scope;
use crate::{lp_coinfind_registered, HistorySyncState, MarketCoinOps, MmCoin, MmCoinEnum, TransactionDetails};
use common::executor::Timer;
use common::log::{debug, warn};
use derive_more::Display;
use futures::compat::Future01CompatExt;
use mm2_core::mm_ctx::MmArc;
use serde_json::json;
use solana_pubkey::Pubkey;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;

/// The pause between the end of a pass and the start of the next one, in seconds (CRD ch.40 R40.6C.9).
pub(crate) const HISTORY_POLL_INTERVAL_S: f64 = 60.;
/// The pause before every request, in seconds: at most four requests per second (R40.6C.4).
const REQUEST_SPACING_S: f64 = 0.25;
/// The `limit` of a signature listing (R40.6C.4).
const SIGNATURES_PAGE_LIMIT: usize = 1000;

/// The timing and paging of a history process. [`Default`] is what a running node uses; tests shorten it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct HistoryConfig {
    pub poll_interval_s: f64,
    pub request_spacing_s: f64,
    pub page_limit: usize,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        HistoryConfig {
            poll_interval_s: HISTORY_POLL_INTERVAL_S,
            request_spacing_s: REQUEST_SPACING_S,
            page_limit: SIGNATURES_PAGE_LIMIT,
        }
    }
}

/// Why a pass of the history process failed. Every text is credential-free.
#[derive(Debug, Display)]
pub(crate) enum SolanaHistoryError {
    #[display(fmt = "{}", _0)]
    Node(String),
    #[display(fmt = "history store: {}", _0)]
    Store(String),
    #[display(fmt = "{}", _0)]
    Internal(String),
}

impl From<RpcError> for SolanaHistoryError {
    fn from(e: RpcError) -> Self { SolanaHistoryError::Node(e.to_string()) }
}

/// What the passes of this process have covered.
#[derive(Default)]
pub(crate) struct WalkState {
    /// The watched accounts whose listing has been read to its end.
    walked: HashSet<Pubkey>,
    /// The signatures that were read (or left out as unreadable) in this process.
    seen: HashSet<String>,
    /// The block height of each slot read so far.
    heights: HashMap<u64, u64>,
    /// The token tickers the earlier passes served.
    tokens: BTreeSet<String>,
}

/// Whether the history of the coin is enabled: of a platform coin, its activation asked for it; of a token, its
/// platform's did (CRD ch.40 R40.6C.1). The versioned `my_tx_history` serves the coin only then (R40.6C.2).
pub fn solana_history_enabled(coin: &MmCoinEnum) -> bool {
    match coin {
        MmCoinEnum::SolanaCoin(coin) => coin.history.is_enabled(),
        MmCoinEnum::SplToken(token) => token.platform_coin.history.is_enabled(),
        _ => false,
    }
}

/// Starts the history process of the registered Solana platform coin `ticker`, if its history is enabled. The
/// activation calls it once the platform and its tokens are registered, inside the scope of the registered instance;
/// it runs on every target. The process is work of that instance: removing the platform aborts it (CRD ch.43
/// R43.1E.5 item 4).
pub fn spawn_platform_history(ctx: &MmArc, ticker: &str) {
    let (task_ctx, ticker) = (ctx.clone(), ticker.to_owned());
    common::executor::spawn(carry_current_scope(async move {
        if let Ok(Some(MmCoinEnum::SolanaCoin(coin))) = lp_coinfind_registered(&task_ctx, &ticker).await {
            if coin.history.is_enabled() {
                crate::eth::tron::history::instance_task(
                    &task_ctx,
                    history_loop(coin, task_ctx.clone(), HistoryConfig::default()),
                )
                .await;
            }
        }
    }));
}

/// Whether `coin` is still the coin registered under its ticker: a disabled coin ends its process.
async fn is_registered_instance(ctx: &MmArc, coin: &SolanaCoin) -> bool {
    matches!(
        lp_coinfind_registered(ctx, &coin.ticker).await,
        Ok(Some(MmCoinEnum::SolanaCoin(found))) if Arc::ptr_eq(&found.0, &coin.0)
    )
}

/// The history process of `coin`: a pass, then a pause, until the node stops or the coin is no longer the registered
/// coin of its ticker (CRD ch.40 R40.6C.1, R40.6C.9).
pub(crate) async fn history_loop(coin: SolanaCoin, ctx: MmArc, config: HistoryConfig) {
    let mut state = WalkState::default();
    loop {
        if ctx.is_stopping() || !is_registered_instance(&ctx, &coin).await {
            break;
        }
        run_pass_and_report(&coin, &ctx, &mut state, &config).await;
        Timer::sleep(config.poll_interval_s).await;
    }
}

/// One pass and the history states that follow from it (R40.6C.9): `Finished` after a pass that read everything,
/// `Error` with a message until the history has finished once, and only a log line after that.
pub(crate) async fn run_pass_and_report(coin: &SolanaCoin, ctx: &MmArc, state: &mut WalkState, config: &HistoryConfig) {
    let result = run_pass(coin, ctx, state, config).await;
    let tickers = served_tickers(coin);
    match result {
        Ok(()) => {
            for ticker in &tickers {
                coin.history.set(ticker, HistorySyncState::Finished);
            }
        },
        Err(error) => {
            warn!("{}: a pass of the transaction history failed: {}", coin.ticker, error);
            for ticker in &tickers {
                coin.history
                    .set_unfinished(ticker, HistorySyncState::Error(json!({ "message": error.to_string() })));
            }
            // A failed store may have lost rows the process had counted as read: read everything again.
            if matches!(error, SolanaHistoryError::Store(_)) {
                state.seen.clear();
                state.walked.clear();
            }
        },
    }
}

/// The tickers the history writes: the platform's and every token's on its record.
fn served_tickers(coin: &SolanaCoin) -> Vec<String> {
    let mut tickers = vec![coin.ticker.clone()];
    let mut tokens: Vec<String> = coin.get_spl_tokens_infos().into_keys().collect();
    tokens.sort();
    tickers.extend(tokens);
    tickers
}

/// The tokens on the platform's record, in ticker order.
fn served_tokens(coin: &SolanaCoin) -> Vec<TokenSpec> {
    let mut tokens: Vec<TokenSpec> = coin
        .get_spl_tokens_infos()
        .into_iter()
        .map(|(ticker, info)| TokenSpec {
            ticker,
            mint: info.token_contract_address,
            decimals: info.decimals,
        })
        .collect();
    tokens.sort_by(|a, b| a.ticker.cmp(&b.ticker));
    tokens
}

/// The stored history of one coin and the rows added to it in this pass.
struct Target {
    coin: MmCoinEnum,
    rows: Vec<TransactionDetails>,
    ids: HashSet<Vec<u8>>,
    /// The `tx_hash` of every stored row: a listed signature in it is known already.
    hashes: HashSet<String>,
    dirty: bool,
}

impl Target {
    async fn load(coin: MmCoinEnum, ctx: &MmArc) -> Result<Self, SolanaHistoryError> {
        let rows = coin
            .load_history_from_file(ctx)
            .compat()
            .await
            .map_err(|e| SolanaHistoryError::Store(e.to_string()))?;
        let ids = rows.iter().map(|row| row.internal_id.0.clone()).collect();
        let hashes = rows.iter().map(|row| row.tx_hash.clone()).collect();
        Ok(Target {
            coin,
            rows,
            ids,
            hashes,
            dirty: false,
        })
    }

    /// Adds `row` unless a row of the same id is stored already (a restart, or the transaction listed twice).
    fn insert(&mut self, row: TransactionDetails) {
        if self.ids.insert(row.internal_id.0.clone()) {
            self.hashes.insert(row.tx_hash.clone());
            self.rows.push(row);
            self.dirty = true;
        }
    }

    /// Stores the rows, newest first, when this pass added any.
    async fn save_if_dirty(&mut self, ctx: &MmArc) -> Result<(), SolanaHistoryError> {
        if !self.dirty {
            return Ok(());
        }
        self.rows.sort_by(|a, b| {
            b.block_height
                .cmp(&a.block_height)
                .then_with(|| b.timestamp.cmp(&a.timestamp))
                .then_with(|| a.tx_hash.cmp(&b.tx_hash))
                .then_with(|| a.internal_id.0.cmp(&b.internal_id.0))
        });
        self.coin
            .save_history_to_file(ctx, self.rows.clone())
            .compat()
            .await
            .map_err(|e| SolanaHistoryError::Store(e.to_string()))?;
        self.dirty = false;
        Ok(())
    }
}

/// Everything a pass works with.
struct Pass<'a> {
    coin: &'a SolanaCoin,
    ctx: &'a MmArc,
    config: &'a HistoryConfig,
    me: Pubkey,
    tokens: Vec<TokenSpec>,
    /// The platform's target first, then the tokens' in the order of `tokens`.
    targets: Vec<Target>,
}

impl Pass<'_> {
    async fn pace(&self) {
        if self.config.request_spacing_s > 0. {
            Timer::sleep(self.config.request_spacing_s).await;
        }
    }

    fn report_left(&self, left: usize) {
        for target in &self.targets {
            self.coin.history.set_unfinished(
                target.coin.ticker(),
                HistorySyncState::InProgress(json!({ "transactions_left": left })),
            );
        }
    }

    async fn save_all(&mut self) -> Result<(), SolanaHistoryError> {
        for target in self.targets.iter_mut() {
            target.save_if_dirty(self.ctx).await?;
        }
        Ok(())
    }

    /// Whether a stored row already answers for `signature` in the listing of a watched account: for the platform
    /// address (`scope` `None`) a row of any list, for a token account the row of that token's list (`scope` is the
    /// index of its target). A transaction that made no row is not known and may be read again after a restart.
    fn is_stored(&self, scope: Option<usize>, signature: &str) -> bool {
        match scope {
            Some(index) => self
                .targets
                .get(index)
                .is_some_and(|target| target.hashes.contains(signature)),
            None => self.targets.iter().any(|target| target.hashes.contains(signature)),
        }
    }

    /// The watched accounts (R40.6C.4): the platform address, then every token account it owns for a served mint,
    /// each once, with the index of the target of the token it belongs to (`None` for the platform address).
    async fn watched_accounts(&self) -> Result<Vec<(Pubkey, Option<usize>)>, SolanaHistoryError> {
        let mut accounts = vec![(self.me, None)];
        for (position, token) in self.tokens.iter().enumerate() {
            self.pace().await;
            let owned = self
                .coin
                .client
                .get_token_accounts_by_owner(&self.me, TokenAccountsFilter::Mint(token.mint))
                .await?;
            for account in owned {
                let key = Pubkey::from_str(&account.pubkey).map_err(|_| {
                    SolanaHistoryError::Node("a token account of the node's answer is not a valid key".to_owned())
                })?;
                if !accounts.iter().any(|(known, _)| *known == key) {
                    // The platform's target is first, the tokens' follow in the order of `tokens`.
                    accounts.push((key, Some(position + 1)));
                }
            }
        }
        Ok(accounts)
    }

    /// The block height of `slot`; `None` when the block has no `blockHeight` (R40.6C.11).
    async fn block_height(&self, state: &mut WalkState, slot: u64) -> Result<Option<u64>, SolanaHistoryError> {
        if let Some(height) = state.heights.get(&slot) {
            return Ok(Some(*height));
        }
        self.pace().await;
        let block = self.coin.client.get_block_summary(slot).await?.ok_or_else(|| {
            SolanaHistoryError::Node("the node has no finalized block for a listed transaction".to_owned())
        })?;
        if let Some(height) = block.block_height {
            state.heights.insert(slot, height);
        }
        Ok(block.block_height)
    }

    /// Reads one new signature and stores the rows it makes. An unreadable transaction is logged and left out.
    async fn read_signature(&mut self, state: &mut WalkState, signature: &str) -> Result<(), SolanaHistoryError> {
        self.pace().await;
        let answer = self
            .coin
            .client
            .get_transaction_base64(signature)
            .await?
            .ok_or_else(|| {
                SolanaHistoryError::Node("the node has no finalized transaction for a listed signature".to_owned())
            })?;
        let ticker = self.coin.ticker.clone();
        let parsed: ParsedTx = match parse_transaction(&answer, signature) {
            Ok(parsed) => parsed,
            Err(reason) => {
                warn!(
                    "{}: the listed transaction {} cannot be read and is left out of the history: {}",
                    ticker, signature, reason
                );
                state.seen.insert(signature.to_owned());
                return Ok(());
            },
        };
        let affects = match parsed.affects(&self.me, &self.tokens) {
            Ok(affects) => affects,
            Err(reason) => {
                warn!(
                    "{}: the listed transaction {} cannot be read and is left out of the history: {}",
                    ticker, signature, reason
                );
                state.seen.insert(signature.to_owned());
                return Ok(());
            },
        };
        if affects {
            let Some(height) = self.block_height(state, parsed.slot).await? else {
                warn!(
                    "{}: the block of the transaction {} has no height and the transaction is left out of the history",
                    ticker, signature
                );
                state.seen.insert(signature.to_owned());
                return Ok(());
            };
            match parsed.rows(&self.me, &ticker, &self.tokens, height) {
                Ok(rows) => {
                    for (row_ticker, row) in rows {
                        match self
                            .targets
                            .iter_mut()
                            .find(|target| target.coin.ticker() == row_ticker)
                        {
                            Some(target) => target.insert(row),
                            None => debug!("{}: no history list for {}", ticker, row_ticker),
                        }
                    }
                },
                Err(reason) => warn!(
                    "{}: the listed transaction {} cannot be read and is left out of the history: {}",
                    ticker, signature, reason
                ),
            }
        }
        state.seen.insert(signature.to_owned());
        Ok(())
    }

    /// Lists `account` page by page and reads the new signatures (R40.6C.4, R40.6C.5): new means neither read in
    /// this process nor stored already (see [`Pass::is_stored`]).
    async fn walk(
        &mut self,
        state: &mut WalkState,
        account: &Pubkey,
        scope: Option<usize>,
    ) -> Result<(), SolanaHistoryError> {
        let whole_listing = !state.walked.contains(account);
        let mut before: Option<String> = None;
        loop {
            self.pace().await;
            let page: Vec<SignatureInfo> = self
                .coin
                .client
                .get_signatures_for_address(account, before.as_deref(), self.config.page_limit)
                .await?;
            let fresh: Vec<&SignatureInfo> = page
                .iter()
                .filter(|entry| !state.seen.contains(&entry.signature) && !self.is_stored(scope, &entry.signature))
                .collect();
            let outcome = {
                let mut outcome = Ok(());
                let total = fresh.len();
                for (done, entry) in fresh.iter().enumerate() {
                    self.report_left(total - done);
                    if let Err(error) = self.read_signature(state, &entry.signature).await {
                        outcome = Err(error);
                        break;
                    }
                }
                outcome
            };
            // What was read is stored even when a request failed: the next pass need not read it again.
            self.save_all().await?;
            outcome?;
            if page.is_empty() || (!whole_listing && fresh.is_empty()) {
                break;
            }
            let Some(last) = page.last() else { break };
            before = Some(last.signature.clone());
        }
        state.walked.insert(*account);
        Ok(())
    }
}

/// One pass over the watched accounts (R40.6C.4, R40.6C.5).
pub(crate) async fn run_pass(
    coin: &SolanaCoin,
    ctx: &MmArc,
    state: &mut WalkState,
    config: &HistoryConfig,
) -> Result<(), SolanaHistoryError> {
    let me = Pubkey::from_str(&coin.my_address)
        .map_err(|_| SolanaHistoryError::Internal("the coin's own address is not a valid key".to_owned()))?;
    let tokens = served_tokens(coin);
    // A token that the earlier passes did not serve needs the whole listings read again: a transaction that was read
    // before it was served made no row for it.
    let tickers: BTreeSet<String> = tokens.iter().map(|token| token.ticker.clone()).collect();
    if !tickers.is_subset(&state.tokens) {
        state.seen.clear();
        state.walked.clear();
    }
    state.tokens = tickers;

    let mut targets = vec![Target::load(MmCoinEnum::from(coin.clone()), ctx).await?];
    for token in &tokens {
        let spl = SplToken::new(
            token.decimals,
            token.ticker.clone(),
            token.mint,
            spl_token::ID,
            coin.clone(),
        );
        targets.push(Target::load(MmCoinEnum::from(spl), ctx).await?);
    }
    let mut pass = Pass {
        coin,
        ctx,
        config,
        me,
        tokens,
        targets,
    };
    let accounts = pass.watched_accounts().await?;
    for (account, scope) in &accounts {
        pass.walk(state, account, *scope).await?;
    }
    Ok(())
}
