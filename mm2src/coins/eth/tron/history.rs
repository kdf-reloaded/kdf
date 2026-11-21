//! The TRON transaction history (CRD ch.21 §21.10C).
//!
//! # Purpose
//!
//! One writer per activated coin reads the account listings of a keyed TronGrid node and stores finished history
//! rows in the coin's history store, the one both `my_tx_history` methods read. A TRX platform coin writes its own
//! rows and the rows of every TRC20 token enabled on it; a TRC20 coin activated on its own (the legacy route)
//! writes its own rows.
//!
//! # Sources
//!
//! - `GET /v1/accounts/{address}/transactions` (TRX rows) and `GET /v1/accounts/{address}/transactions/trc20`
//!   (token transfer events), confirmed rows only, from keyed TronGrid nodes only.
//! - `POST /wallet/gettransactioninfobyid` of the configured nodes, once per new token transaction, for the block
//!   height and the fee a token event does not carry.
//! - `POST /wallet/getchainparameters`, once per pass, for the bandwidth price.
//!
//! # Invariants
//!
//! - A row is stored only when every member is known: a token row whose receipt cannot be read is left out and
//!   asked again on a later pass, never stored with a made-up height or fee.
//! - A stored row is never asked for again and never rewritten (confirmed rows only; no reorganization handling).
//! - The first walk of an address in a process reads its whole listing; a later walk stops at the first page that
//!   has nothing new. A walk that was cut short therefore never leaves a gap.
//! - No error or log text of this module carries a node URL beyond its host, a header value or an API key: the
//!   node errors arrive scrubbed from the node client.

use super::api::{AccountListing, TransactionInfoResponse, TronApiClient, TronApiError};
use super::fee::TronTxFeeDetails;
use super::{TronAddress, TRX_DECIMALS};
use crate::coin_work::{carry_current_scope, CoinWork};
use crate::eth::{u256_to_big_decimal, EthCoin, EthCoinType};
use crate::{lp_coinfind_registered, BytesJson, HistorySyncState, MarketCoinOps, MmCoin, MmCoinEnum,
            TransactionDetails, TransactionType, TxFeeDetails};
use common::executor::Timer;
use common::log::{debug, warn};
use common::mm_number::BigDecimal;
use derive_more::Display;
use ethereum_types::{Address, U256};
use futures::compat::Future01CompatExt;
use futures::future::abortable;
use kdf_crypto::sha256;
use mm2_core::mm_ctx::MmArc;
use serde::Deserialize;
use serde_json::{json, Value as Json};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::future::Future;
use std::num::NonZeroU64;
use std::sync::{Arc, PoisonError};

/// The pause between two passes of the history process, in seconds. A pass with nothing new costs two keyed
/// requests per known address.
pub(crate) const HISTORY_POLL_INTERVAL_S: f64 = 60.;

/// After this many receipt reads in a row failed during one walk, the walk stops asking: the rows stay unresolved
/// and are asked again on the next pass, instead of every row waiting for every node's cutoff.
const MAX_RECEIPT_FAILURES_IN_A_ROW: u32 = 3;

/// The first four bytes of a TRC20 `transfer(address,uint256)` call, in hex.
const TRC20_TRANSFER_SELECTOR: &str = "a9059cbb";

/// Why a pass of the history process failed. Every text is credential-free.
#[derive(Clone, Debug, Display)]
pub(crate) enum TronHistoryError {
    /// A node request failed on every node it may be asked of.
    #[display(fmt = "{}", _0)]
    Node(String),
    /// The history store could not be read or written.
    #[display(fmt = "history store: {}", _0)]
    Store(String),
    #[display(fmt = "{}", _0)]
    Internal(String),
}

impl From<TronApiError> for TronHistoryError {
    fn from(e: TronApiError) -> Self { TronHistoryError::Node(e.to_string()) }
}

/// What the walks of earlier passes of this process have covered.
#[derive(Default)]
pub(crate) struct WalkState {
    /// The addresses whose transaction listing has been read to its end.
    walked_trx: HashSet<Address>,
    /// The addresses whose token listing has been read to its end with every row resolved.
    walked_trc20: HashSet<Address>,
    /// The transaction ids of the transaction listing that were handled, per address.
    seen_trx: HashSet<(Address, String)>,
    /// The row ids of the token listing that were handled, per address.
    seen_trc20: HashSet<(Address, Vec<u8>)>,
    /// The token tickers the token walks covered.
    tokens: BTreeSet<String>,
    /// The own addresses the stored TRX rows were last brought in line with ([`Target::reconcile`]).
    reconciled_trx: Option<HashSet<Address>>,
    /// The same for the stored token rows.
    reconciled_trc20: Option<HashSet<Address>>,
}

/// How one pass ended, for the transaction listing and for the token listing.
pub(crate) struct PassReport {
    /// The TRX part. `Ok` also when the coin has no TRX part.
    pub trx: Result<(), TronHistoryError>,
    /// The token part: the number of token rows that could not be resolved yet.
    pub tokens: Result<usize, TronHistoryError>,
}

impl PassReport {
    fn failed(error: TronHistoryError) -> Self {
        PassReport {
            trx: Err(error.clone()),
            tokens: Err(error),
        }
    }
}

impl EthCoin {
    /// Whether the transaction history is enabled for this TRON coin: the activation asked for it and a keyed
    /// TronGrid node is configured (CRD ch.21 §21.10C). The versioned `my_tx_history` serves the coin only then.
    pub fn tron_history_enabled(&self) -> bool {
        self.is_tron_family() && !matches!(*self.history_state(), HistorySyncState::NotEnabled)
    }

    fn history_state(&self) -> std::sync::MutexGuard<'_, HistorySyncState> {
        self.history_sync_state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn history_finished(&self) -> bool { matches!(*self.history_state(), HistorySyncState::Finished) }

    fn set_history_state(&self, state: HistorySyncState) { *self.history_state() = state; }

    /// Sets a state that describes work in progress or a failure: only until the history has finished once.
    fn set_unfinished_history_state(&self, state: HistorySyncState) {
        if !self.history_finished() {
            self.set_history_state(state);
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The process
// ---------------------------------------------------------------------------------------------------------------

/// Starts the history process of the activated TRX platform coin `ticker`, if its history is enabled. The mmrpc
/// 2.0 activation calls it once the platform and its tokens are registered; it runs on every target. The history is
/// the platform's own background work, so every look-up of this module uses the registered view, which counts a
/// passive platform, and the work goes on while the platform is passive (CRD ch.43 R43.1E.6 item 3).
///
/// The process is work of the registered instance (CRD ch.43 R43.1E.5 item 4): the caller runs this inside
/// [`crate::coin_work::CoinWork::run_in_scope`] of that instance. The look-up of the coin runs in that scope without
/// registering anything; only once the coin is a TRX platform does the process register its abort handle with the
/// instance, so removing the platform aborts it, and it runs in the instance's scope (CRD ch.55 R55.4.9). The
/// platform of another family leaves no work on the instance.
pub fn spawn_platform_history(ctx: &MmArc, ticker: &str) {
    let (task_ctx, ticker) = (ctx.clone(), ticker.to_owned());
    common::executor::spawn(carry_current_scope(async move {
        if let Ok(Some(MmCoinEnum::EthCoin(coin))) = lp_coinfind_registered(&task_ctx, &ticker).await {
            if coin.is_tron_platform_coin() {
                instance_task(&task_ctx, tron_history_loop(coin, task_ctx.clone())).await;
            }
        }
    }));
}

/// Makes `fut` abortable work of the instance whose scope is current: its abort handle goes to that instance's
/// `CoinWork`, and `fut` runs in the same scope. Outside any scope the handle goes to the node's list, as the other
/// spawn sites do. Returns the future to spawn, which ends when `fut` ends or is aborted.
pub(crate) fn instance_task(ctx: &MmArc, fut: impl Future<Output = ()>) -> impl Future<Output = ()> {
    let (fut, handle) = abortable(carry_current_scope(fut));
    if let Some(handle) = CoinWork::register_with_current(handle) {
        ctx.abort_handlers.lock().unwrap().push(handle);
    }
    async move {
        let _ = fut.await;
    }
}

/// The history process of `coin`: a pass, then a pause, until the node stops or the coin is no longer the
/// registered coin of its ticker. It does nothing for a coin whose history is not enabled.
pub(crate) async fn tron_history_loop(coin: EthCoin, ctx: MmArc) {
    let Some(api) = coin.tron_api.clone() else {
        return;
    };
    if !coin.tron_history_enabled() || !api.has_history_nodes() {
        return;
    }
    let mut state = WalkState::default();
    loop {
        if ctx.is_stopping() || !is_registered_instance(&ctx, &coin).await {
            break;
        }
        run_pass_and_report(&coin, &api, &ctx, &mut state).await;
        Timer::sleep(HISTORY_POLL_INTERVAL_S).await;
    }
}

/// Whether `coin` is still the coin registered under its ticker. A coin that was disabled, or disabled and enabled
/// again (another instance), ends its process. A passive platform is still registered, so its process goes on
/// (CRD ch.43 R43.1E.6 item 3).
pub(crate) async fn is_registered_instance(ctx: &MmArc, coin: &EthCoin) -> bool {
    matches!(
        lp_coinfind_registered(ctx, coin.ticker()).await,
        Ok(Some(MmCoinEnum::EthCoin(found))) if Arc::ptr_eq(&found.0, &coin.0)
    )
}

/// One pass over every known address of `coin`'s wallet, and the history states that follow from it
/// ([`apply_report`]).
pub(crate) async fn run_pass_and_report(coin: &EthCoin, api: &TronApiClient, ctx: &MmArc, state: &mut WalkState) {
    let report = match coin.known_wallet_addresses().await {
        Ok(addresses) => collect_history(coin, api, ctx, state, &addresses).await,
        Err(e) => PassReport::failed(TronHistoryError::Internal(e)),
    };
    apply_report(coin, ctx, &report).await;
}

/// Sets the history states that follow from a pass, on every coin the process of `coin` writes:
///
/// - a part that ended with nothing unresolved is `Finished`;
/// - token rows still unresolved leave the tokens `InProgress` with their number, until they have finished once;
/// - a failed part is `Error` with a credential-free text until it has finished once, and is only logged after.
pub(crate) async fn apply_report(coin: &EthCoin, ctx: &MmArc, report: &PassReport) {
    let (platform, tokens) = served_coins(coin, ctx).await;
    let failed = |served: &EthCoin, error: &TronHistoryError| {
        warn!(
            "{}: a pass of the transaction history failed: {}",
            served.ticker(),
            error
        );
        served.set_unfinished_history_state(HistorySyncState::Error(json!({ "message": error.to_string() })));
    };
    if let Some(platform) = &platform {
        match &report.trx {
            Ok(()) => platform.set_history_state(HistorySyncState::Finished),
            Err(error) => failed(platform, error),
        }
    }
    for token in &tokens {
        match &report.tokens {
            Ok(0) => token.set_history_state(HistorySyncState::Finished),
            Ok(unresolved) => {
                token.set_unfinished_history_state(HistorySyncState::InProgress(
                    json!({ "transactions_left": unresolved }),
                ));
            },
            Err(error) => failed(token, error),
        }
    }
}

/// The coins whose history `coin`'s process writes: for a TRX platform the platform itself and every TRC20 token
/// enabled on it whose history is enabled; for a TRC20 coin with a process of its own, that coin.
async fn served_coins(coin: &EthCoin, ctx: &MmArc) -> (Option<EthCoin>, Vec<EthCoin>) {
    match &coin.coin_type {
        EthCoinType::Tron => {
            let mut tickers: Vec<String> = coin.get_erc20_tokens_infos().into_keys().collect();
            tickers.sort();
            let mut tokens = Vec::with_capacity(tickers.len());
            for ticker in tickers {
                if let Ok(Some(MmCoinEnum::EthCoin(token))) = lp_coinfind_registered(ctx, &ticker).await {
                    if matches!(token.coin_type, EthCoinType::Trc20 { .. }) && token.tron_history_enabled() {
                        tokens.push(token);
                    }
                }
            }
            (Some(coin.clone()), tokens)
        },
        EthCoinType::Trc20 { .. } => (None, vec![coin.clone()]),
        EthCoinType::Eth | EthCoinType::Erc20 { .. } => (None, Vec::new()),
    }
}

/// One pass over `addresses`, the known addresses of the wallet: the transaction listing of each for the TRX rows,
/// then the token listing of each for the rows of the served tokens. Rows are stored page by page.
pub(crate) async fn collect_history(
    coin: &EthCoin,
    api: &TronApiClient,
    ctx: &MmArc,
    state: &mut WalkState,
    addresses: &[Address],
) -> PassReport {
    // The bandwidth price of this pass, for the bandwidth figure of every row. Without it no row can be built.
    let bandwidth_price = match bandwidth_price(api).await {
        Ok(price) => price,
        Err(e) => return PassReport::failed(e),
    };
    let mine: HashSet<Address> = addresses.iter().copied().collect();
    let (platform, tokens) = served_coins(coin, ctx).await;

    let trx = match platform {
        Some(platform) => walk_all_trx(api, ctx, state, platform, addresses, &mine, bandwidth_price).await,
        None => Ok(()),
    };
    let tokens = if tokens.is_empty() {
        Ok(0)
    } else {
        walk_all_trc20(api, ctx, state, tokens, addresses, &mine, bandwidth_price).await
    };
    PassReport { trx, tokens }
}

/// The price of one byte of bandwidth in SUN, the chain parameter `getTransactionFee`.
async fn bandwidth_price(api: &TronApiClient) -> Result<NonZeroU64, TronHistoryError> {
    let parameters = api.get_chain_parameters().await?;
    parameters
        .chain_parameter
        .iter()
        .find(|parameter| parameter.key == "getTransactionFee")
        .and_then(|parameter| parameter.value)
        .and_then(|price| u64::try_from(price).ok())
        .and_then(NonZeroU64::new)
        .ok_or_else(|| TronHistoryError::Node("the chain parameters carry no positive getTransactionFee".to_owned()))
}

/// The stored history of one coin and the rows added to it in this pass.
struct Target {
    coin: EthCoin,
    rows: Vec<TransactionDetails>,
    ids: HashSet<Vec<u8>>,
    dirty: bool,
}

impl Target {
    async fn load(coin: EthCoin, ctx: &MmArc) -> Result<Self, TronHistoryError> {
        let rows = coin
            .load_history_from_file(ctx)
            .compat()
            .await
            .map_err(|e| TronHistoryError::Store(e.to_string()))?;
        let ids = rows.iter().map(|row| row.internal_id.0.clone()).collect();
        Ok(Target {
            coin,
            rows,
            ids,
            dirty: false,
        })
    }

    /// Adds `row` unless a row of the same id is stored already (the same transaction listed by two own addresses).
    fn insert(&mut self, row: TransactionDetails) {
        if self.ids.insert(row.internal_id.0.clone()) {
            self.rows.push(row);
            self.dirty = true;
        }
    }

    /// Brings the stored rows in line with `mine`, the addresses the wallet knows now. A row is built with the
    /// addresses known at that time; when another address of it becomes known later (a new HD address), what the
    /// wallet spent and received in it changes: a send to that address becomes a transfer between own addresses.
    /// The figures are recomputed from the row's own members with the rules of the mapping: a TRX transfer is
    /// spent with its fee by an own sender and received by an own receiver; a token transfer likewise without the
    /// fee; a fee-only row and a row whose members cannot be read stay as they are.
    fn reconcile(&mut self, mine: &HashSet<Address>, kind: RowKind) {
        for row in self.rows.iter_mut() {
            let Some((spent_by_me, received_by_me)) = own_amounts(row, mine, kind) else {
                continue;
            };
            if row.spent_by_me != spent_by_me || row.received_by_me != received_by_me {
                row.my_balance_change = &received_by_me - &spent_by_me;
                row.spent_by_me = spent_by_me;
                row.received_by_me = received_by_me;
                self.dirty = true;
            }
        }
    }

    /// Stores the rows, newest first, when this pass added or changed any.
    async fn save_if_dirty(&mut self, ctx: &MmArc) -> Result<(), TronHistoryError> {
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
            .map_err(|e| TronHistoryError::Store(e.to_string()))?;
        self.dirty = false;
        Ok(())
    }
}

/// Which history a stored row belongs to.
#[derive(Clone, Copy)]
enum RowKind {
    /// A row of the TRX coin: an own sender also spends the fee.
    Trx,
    /// A row of a TRC20 coin: the fee is paid in TRX and is never part of what the token wallet spent.
    Trc20,
}

/// What the wallet of `mine` spent and received in the stored transfer row `row`, from the row's own members.
/// `None` for a row that is not a transfer (a fee-only row of TRX) and for a row whose members cannot be read.
fn own_amounts(row: &TransactionDetails, mine: &HashSet<Address>, kind: RowKind) -> Option<(BigDecimal, BigDecimal)> {
    let zero = BigDecimal::from(0);
    if row.total_amount == zero {
        return None;
    }
    let own = |addresses: &[String]| match addresses {
        [address] => TronAddress::from_base58(address)
            .ok()
            .map(|address| mine.contains(&address.to_evm_address())),
        _ => None,
    };
    let (from_is_mine, to_is_mine) = (own(&row.from)?, own(&row.to)?);
    let spent_by_me = match (from_is_mine, kind) {
        (false, _) => zero.clone(),
        (true, RowKind::Trc20) => row.total_amount.clone(),
        (true, RowKind::Trx) => match &row.fee_details {
            Some(TxFeeDetails::Tron(fee)) => &row.total_amount + &fee.total_fee,
            _ => return None,
        },
    };
    let received_by_me = if to_is_mine { row.total_amount.clone() } else { zero };
    Some((spent_by_me, received_by_me))
}

/// `transactions_left` of the coins that have not finished once: the rows of the page being worked on.
fn report_page(coins: &[&EthCoin], rows_left: usize) {
    for coin in coins {
        coin.set_unfinished_history_state(HistorySyncState::InProgress(json!({ "transactions_left": rows_left })));
    }
}

/// The cursor of the page after `page`, or `None` on the last page.
///
/// # Errors
///
/// A cursor that repeats the one just used: the listing would never end.
fn next_cursor(previous: Option<&str>, next: Option<String>) -> Result<Option<String>, TronHistoryError> {
    match next {
        Some(next) if previous == Some(next.as_str()) => Err(TronHistoryError::Node(
            "an account listing repeats its cursor".to_owned(),
        )),
        other => Ok(other),
    }
}

async fn walk_all_trx(
    api: &TronApiClient,
    ctx: &MmArc,
    state: &mut WalkState,
    platform: EthCoin,
    addresses: &[Address],
    mine: &HashSet<Address>,
    bandwidth_price: NonZeroU64,
) -> Result<(), TronHistoryError> {
    let mut target = Target::load(platform, ctx).await?;
    // At the first pass of a process, and whenever the wallet knows another address than before.
    if state.reconciled_trx.as_ref() != Some(mine) {
        target.reconcile(mine, RowKind::Trx);
        target.save_if_dirty(ctx).await?;
        state.reconciled_trx = Some(mine.clone());
    }
    for address in addresses {
        walk_trx(api, ctx, state, &mut target, address, mine, bandwidth_price).await?;
    }
    Ok(())
}

/// Reads the transaction listing of `address` and stores the TRX rows it yields.
async fn walk_trx(
    api: &TronApiClient,
    ctx: &MmArc,
    state: &mut WalkState,
    target: &mut Target,
    address: &Address,
    mine: &HashSet<Address>,
    bandwidth_price: NonZeroU64,
) -> Result<(), TronHistoryError> {
    let owner = TronAddress::from_evm_address(*address);
    let whole_listing = !state.walked_trx.contains(address);
    let ticker = target.coin.ticker().to_owned();
    let mut cursor: Option<String> = None;
    loop {
        let page = api
            .get_account_listing(&owner, AccountListing::Transactions, cursor.as_deref())
            .await?;
        let mut fresh = Vec::new();
        for row in &page.data {
            // A row without a transaction id is not a transaction of the account's own (the listing can carry
            // rows of other kinds); there is nothing to show for it.
            let Some(tx_id) = row.get("txID").and_then(Json::as_str) else {
                continue;
            };
            let tx_id = tx_id.to_ascii_lowercase();
            if !state.seen_trx.contains(&(*address, tx_id.clone())) {
                fresh.push((tx_id, row));
            }
        }
        report_page(&[&target.coin], fresh.len());
        let had_fresh = !fresh.is_empty();
        for (tx_id, row) in fresh {
            match trx_history_row(&ticker, row, mine, bandwidth_price) {
                Ok(Some(details)) => target.insert(details),
                Ok(None) => {},
                Err(reason) => warn!(
                    "{}: the listed transaction {} cannot be read and is left out of the history: {}",
                    ticker, tx_id, reason
                ),
            }
            state.seen_trx.insert((*address, tx_id));
        }
        target.save_if_dirty(ctx).await?;
        let next = next_cursor(cursor.as_deref(), page.meta.and_then(|meta| meta.fingerprint))?;
        if next.is_none() || (!whole_listing && !had_fresh) {
            break;
        }
        cursor = next;
    }
    state.walked_trx.insert(*address);
    Ok(())
}

async fn walk_all_trc20(
    api: &TronApiClient,
    ctx: &MmArc,
    state: &mut WalkState,
    tokens: Vec<EthCoin>,
    addresses: &[Address],
    mine: &HashSet<Address>,
    bandwidth_price: NonZeroU64,
) -> Result<usize, TronHistoryError> {
    // A token that was not covered by the earlier walks needs the whole listings again.
    let tickers: BTreeSet<String> = tokens.iter().map(|token| token.ticker().to_owned()).collect();
    if !tickers.is_subset(&state.tokens) {
        state.walked_trc20.clear();
        state.seen_trc20.clear();
        state.reconciled_trc20 = None;
    }
    state.tokens = tickers;

    let mut targets = Vec::with_capacity(tokens.len());
    for token in tokens {
        targets.push(Target::load(token, ctx).await?);
    }
    // At the first pass of a process, for a newly served token, and whenever the wallet knows another address.
    if state.reconciled_trc20.as_ref() != Some(mine) {
        for target in targets.iter_mut() {
            target.reconcile(mine, RowKind::Trc20);
            target.save_if_dirty(ctx).await?;
        }
        state.reconciled_trc20 = Some(mine.clone());
    }
    let mut unresolved = 0;
    for address in addresses {
        unresolved += walk_trc20(api, ctx, state, &mut targets, address, mine, bandwidth_price).await?;
    }
    Ok(unresolved)
}

/// The token contract and the platform ticker of a TRC20 coin.
fn trc20_identity(coin: &EthCoin) -> Option<(Address, &str)> {
    match &coin.coin_type {
        EthCoinType::Trc20 { token_addr, platform } => Some((*token_addr, platform.as_str())),
        _ => None,
    }
}

/// Reads the token listing of `address` and stores the rows of the served tokens. Returns the number of rows that
/// could not be resolved (their receipt could not be read); they are asked again on the next pass.
async fn walk_trc20(
    api: &TronApiClient,
    ctx: &MmArc,
    state: &mut WalkState,
    targets: &mut [Target],
    address: &Address,
    mine: &HashSet<Address>,
    bandwidth_price: NonZeroU64,
) -> Result<usize, TronHistoryError> {
    let owner = TronAddress::from_evm_address(*address);
    let whole_listing = !state.walked_trc20.contains(address);
    let by_contract: HashMap<Address, usize> = targets
        .iter()
        .enumerate()
        .filter_map(|(index, target)| trc20_identity(&target.coin).map(|(contract, _)| (contract, index)))
        .collect();
    // One served token: the provider filters; several: one listing of every token, split here.
    let only_contract = match &*targets {
        [single] => trc20_identity(&single.coin).map(|(contract, _)| TronAddress::from_evm_address(contract)),
        _ => None,
    };
    let listing = AccountListing::Trc20 {
        contract: only_contract.as_ref(),
    };

    let mut occurrences: HashMap<(String, Address, Address, Address, U256), u32> = HashMap::new();
    // The receipts read in this walk; `None` for a transaction whose receipt could not be read.
    let mut receipts: HashMap<String, Option<TransactionInfoResponse>> = HashMap::new();
    let mut receipt_failures_in_a_row = 0u32;
    let mut unresolved = 0usize;
    let mut cursor: Option<String> = None;
    loop {
        let page = api.get_account_listing(&owner, listing, cursor.as_deref()).await?;
        let mut fresh = Vec::new();
        let mut had_fresh = false;
        for row in &page.data {
            let event = match Trc20Event::read(row) {
                Ok(Some(event)) => event,
                // Not a transfer event.
                Ok(None) => continue,
                Err(reason) => {
                    debug!(
                        "a listed token event cannot be read and is left out of the history: {}",
                        reason
                    );
                    continue;
                },
            };
            let occurrence = {
                let count = occurrences.entry(event.key()).or_insert(0);
                let occurrence = *count;
                *count += 1;
                occurrence
            };
            let id = event.internal_id(occurrence);
            if state.seen_trc20.contains(&(*address, id.clone())) {
                continue;
            }
            had_fresh = true;
            match by_contract.get(&event.contract) {
                Some(index) => fresh.push((id, *index, event)),
                // An event of a token that is not served.
                None => {
                    state.seen_trc20.insert((*address, id));
                },
            }
        }
        let served: Vec<&EthCoin> = targets.iter().map(|target| &target.coin).collect();
        report_page(&served, fresh.len());

        for (id, index, event) in fresh {
            let target = &mut targets[index];
            if target.ids.contains(&id) {
                // Stored by an earlier run, or listed by another own address in this pass.
                state.seen_trc20.insert((*address, id));
                continue;
            }
            if !receipts.contains_key(&event.tx_id) {
                if receipt_failures_in_a_row >= MAX_RECEIPT_FAILURES_IN_A_ROW {
                    unresolved += 1;
                    continue;
                }
                let receipt = match api.get_confirmed_transaction_info(&event.tx_id).await {
                    Ok(receipt) => {
                        receipt_failures_in_a_row = 0;
                        Some(receipt)
                    },
                    Err(e) => {
                        receipt_failures_in_a_row += 1;
                        debug!(
                            "{}: the receipt of the transaction {} cannot be read yet: {}",
                            target.coin.ticker(),
                            event.tx_id,
                            e
                        );
                        None
                    },
                };
                receipts.insert(event.tx_id.clone(), receipt);
            }
            let Some(Some(receipt)) = receipts.get(&event.tx_id) else {
                unresolved += 1;
                continue;
            };
            let Some((_, platform_ticker)) = trc20_identity(&target.coin) else {
                continue;
            };
            let built = trc20_history_row(
                target.coin.ticker(),
                platform_ticker,
                target.coin.decimals,
                &event,
                id.clone(),
                receipt,
                mine,
                bandwidth_price,
            );
            match built {
                Ok(details) => {
                    target.insert(details);
                    state.seen_trc20.insert((*address, id));
                },
                Err(reason) => {
                    debug!(
                        "{}: the token transfer of the transaction {} cannot be read yet: {}",
                        target.coin.ticker(),
                        event.tx_id,
                        reason
                    );
                    unresolved += 1;
                },
            }
        }
        for target in targets.iter_mut() {
            target.save_if_dirty(ctx).await?;
        }
        let next = next_cursor(cursor.as_deref(), page.meta.and_then(|meta| meta.fingerprint))?;
        if next.is_none() || (!whole_listing && !had_fresh) {
            break;
        }
        cursor = next;
    }
    if unresolved == 0 {
        state.walked_trc20.insert(*address);
    }
    Ok(unresolved)
}

// ---------------------------------------------------------------------------------------------------------------
// The fee of a row
// ---------------------------------------------------------------------------------------------------------------

/// What the chain reports of the cost of one transaction, in SUN, bytes and energy units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReceiptFigures {
    /// The whole fee the owner paid.
    pub fee_sun: u64,
    /// The part of the fee burned for bandwidth.
    pub net_fee_sun: u64,
    /// The part of the fee burned for energy.
    pub energy_fee_sun: u64,
    /// The bandwidth drawn from the owner's free or staked bandwidth, in bytes.
    pub net_usage: u64,
    /// The total energy the transaction used.
    pub energy_usage_total: u64,
}

/// The bytes of bandwidth a transaction consumed, whatever paid for them (staked, free or burned TRX): the bytes
/// drawn from the owner's bandwidth plus the bytes the burned bandwidth fee bought at `bandwidth_price` SUN per
/// byte (the chain parameter `getTransactionFee`). The same meaning as `bandwidth_used` of a withdraw result.
///
/// A TRX transfer to an address the chain had not created (`is_trx_transfer` with a fee beyond bandwidth and
/// energy) is the exception, as in a withdraw result: its `net_fee` is the chain's fixed create-account charge,
/// not a price per byte, so only the bytes drawn from the owner's bandwidth count.
pub(crate) fn bandwidth_used(figures: &ReceiptFigures, bandwidth_price: NonZeroU64, is_trx_transfer: bool) -> u64 {
    if is_trx_transfer && fee_beyond_resources(figures).is_some() {
        return figures.net_usage;
    }
    let burned_bytes = figures.net_fee_sun / bandwidth_price.get();
    figures.net_usage.saturating_add(burned_bytes)
}

/// The part of the fee beyond bandwidth and energy, when there is one. On a TRX transfer to an address the chain
/// had not created, this is the account creation fee.
pub(crate) fn fee_beyond_resources(figures: &ReceiptFigures) -> Option<u64> {
    figures
        .fee_sun
        .checked_sub(figures.net_fee_sun.checked_add(figures.energy_fee_sun)?)
        .filter(|remainder| *remainder > 0)
}

fn sun_to_trx(sun: u64) -> Result<BigDecimal, String> {
    u256_to_big_decimal(U256::from(sun), TRX_DECIMALS).map_err(|e| e.into_inner().to_string())
}

/// The fee details of a history row. The only place of the history that builds a [`TronTxFeeDetails`].
///
/// `is_trx_transfer` says whether the row is a TRX transfer, the only kind whose fee beyond bandwidth and energy
/// is reported as `account_creation_fee`.
fn history_fee_details(
    platform_ticker: &str,
    figures: &ReceiptFigures,
    bandwidth_price: NonZeroU64,
    is_trx_transfer: bool,
) -> Result<TxFeeDetails, String> {
    let account_creation_fee = match fee_beyond_resources(figures) {
        Some(remainder) if is_trx_transfer => Some(sun_to_trx(remainder)?),
        _ => None,
    };
    Ok(TxFeeDetails::Tron(TronTxFeeDetails {
        coin: platform_ticker.to_owned(),
        bandwidth_used: bandwidth_used(figures, bandwidth_price, is_trx_transfer),
        energy_used: figures.energy_usage_total,
        bandwidth_fee: sun_to_trx(figures.net_fee_sun)?,
        energy_fee: sun_to_trx(figures.energy_fee_sun)?,
        total_fee: sun_to_trx(figures.fee_sun)?,
        account_creation_fee,
    }))
}

// ---------------------------------------------------------------------------------------------------------------
// A row of the transaction listing
// ---------------------------------------------------------------------------------------------------------------

/// A row of `GET /v1/accounts/{address}/transactions`, the members the history reads.
#[derive(Debug, Deserialize)]
struct TrxListingRow {
    #[serde(rename = "txID")]
    tx_id: String,
    #[serde(rename = "blockNumber")]
    block_number: u64,
    /// Milliseconds.
    block_timestamp: u64,
    ret: Vec<TrxListingResult>,
    #[serde(default)]
    signature: Vec<String>,
    raw_data_hex: String,
    net_usage: u64,
    net_fee: u64,
    energy_fee: u64,
    energy_usage_total: u64,
    raw_data: TrxListingRawData,
}

#[derive(Debug, Deserialize)]
struct TrxListingResult {
    fee: u64,
    #[serde(rename = "contractRet")]
    contract_ret: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TrxListingRawData {
    contract: Vec<TrxListingContract>,
}

#[derive(Debug, Deserialize)]
struct TrxListingContract {
    #[serde(rename = "type")]
    kind: String,
    parameter: TrxListingParameter,
}

#[derive(Debug, Deserialize)]
struct TrxListingParameter {
    value: Json,
}

/// The address the member `name` of a contract value holds, in the `41...` hex form the listing uses.
fn hex_address_member(value: &Json, name: &str) -> Result<TronAddress, String> {
    let text = value
        .get(name)
        .and_then(Json::as_str)
        .ok_or_else(|| format!("no {name}"))?;
    TronAddress::from_hex(text).map_err(|e| format!("{name}: {e}"))
}

fn tx_id_bytes(tx_id: &str) -> Result<Vec<u8>, String> {
    let bytes = hex::decode(tx_id).map_err(|e| format!("transaction id: {e}"))?;
    if bytes.len() == 32 {
        Ok(bytes)
    } else {
        Err(format!("transaction id of {} bytes", bytes.len()))
    }
}

/// Appends a protobuf length-delimited field of field number `field` (below 16).
fn push_length_delimited(out: &mut Vec<u8>, field: u8, bytes: &[u8]) {
    out.push((field << 3) | 2);
    let mut length = bytes.len();
    while length >= 0x80 {
        out.push((length & 0x7f) as u8 | 0x80);
        length >>= 7;
    }
    out.push(length as u8);
    out.extend_from_slice(bytes);
}

/// The signed transaction in its protobuf form: the raw data as the chain has it (field 1) and each signature
/// (field 2). The raw bytes are taken as they are, so no member of the transaction is lost.
pub(crate) fn signed_transaction_bytes(raw_data_hex: &str, signatures: &[String]) -> Result<Vec<u8>, String> {
    let raw = hex::decode(raw_data_hex).map_err(|e| format!("raw data: {e}"))?;
    let mut out = Vec::with_capacity(raw.len() + 72 * signatures.len() + 4);
    push_length_delimited(&mut out, 1, &raw);
    for signature in signatures {
        let signature = hex::decode(signature).map_err(|e| format!("signature: {e}"))?;
        push_length_delimited(&mut out, 2, &signature);
    }
    Ok(out)
}

/// The history row of the TRX coin `ticker` for one row of the transaction listing, where `mine` are the known
/// addresses of the wallet. `Ok(None)` for a row that changes no TRX balance of the wallet.
///
/// - A successful `TransferContract` from or to an own address is a transfer row; a sender of the wallet also
///   spends the fee.
/// - Any other transaction an own address sent and paid a fee for is a fee-only row: `FeeForTokenTx` when it is
///   a TRC20 `transfer` call, `StandardTransfer` otherwise. The token amount is in the token's own history.
///
/// # Errors
///
/// The reason the row cannot be read.
pub(crate) fn trx_history_row(
    ticker: &str,
    row: &Json,
    mine: &HashSet<Address>,
    bandwidth_price: NonZeroU64,
) -> Result<Option<TransactionDetails>, String> {
    let row = TrxListingRow::deserialize(row).map_err(|e| e.to_string())?;
    let contract = row.raw_data.contract.first().ok_or("no contract")?;
    let result = row.ret.first().ok_or("no result")?;
    let value = &contract.parameter.value;
    let owner = hex_address_member(value, "owner_address")?;
    let owner_is_mine = mine.contains(&owner.to_evm_address());
    let succeeded = result.contract_ret.as_deref().map_or(true, |text| text == "SUCCESS");
    let figures = ReceiptFigures {
        fee_sun: result.fee,
        net_fee_sun: row.net_fee,
        energy_fee_sun: row.energy_fee,
        net_usage: row.net_usage,
        energy_usage_total: row.energy_usage_total,
    };
    let fee = sun_to_trx(result.fee)?;
    let zero = BigDecimal::from(0);

    let is_trx_transfer = contract.kind == "TransferContract" && succeeded;
    let (to, total_amount, spent_by_me, received_by_me, transaction_type) = if is_trx_transfer {
        let to = hex_address_member(value, "to_address")?;
        let to_is_mine = mine.contains(&to.to_evm_address());
        if !owner_is_mine && !to_is_mine {
            return Ok(None);
        }
        let amount = value.get("amount").and_then(Json::as_u64).ok_or("no amount")?;
        let amount = sun_to_trx(amount)?;
        let spent = if owner_is_mine { &amount + &fee } else { zero.clone() };
        let received = if to_is_mine { amount.clone() } else { zero.clone() };
        (
            vec![to.to_base58()],
            amount,
            spent,
            received,
            TransactionType::StandardTransfer,
        )
    } else {
        if !owner_is_mine || result.fee == 0 {
            return Ok(None);
        }
        let counterparty = ["contract_address", "to_address"]
            .into_iter()
            .find(|name| value.get(*name).is_some());
        let to = match counterparty {
            Some(name) => vec![hex_address_member(value, name)?.to_base58()],
            None => Vec::new(),
        };
        let is_token_transfer = contract.kind == "TriggerSmartContract"
            && value
                .get("data")
                .and_then(Json::as_str)
                .is_some_and(|data| data.starts_with(TRC20_TRANSFER_SELECTOR));
        let transaction_type = if is_token_transfer {
            TransactionType::FeeForTokenTx
        } else {
            TransactionType::StandardTransfer
        };
        (to, zero.clone(), fee, zero, transaction_type)
    };

    let tx_hash = row.tx_id.to_ascii_lowercase();
    Ok(Some(TransactionDetails {
        unsigned: None,
        tx_json: None,
        tx_hex: BytesJson(signed_transaction_bytes(&row.raw_data_hex, &row.signature)?),
        internal_id: BytesJson(tx_id_bytes(&tx_hash)?),
        tx_hash,
        from: vec![owner.to_base58()],
        to,
        total_amount,
        my_balance_change: &received_by_me - &spent_by_me,
        spent_by_me,
        received_by_me,
        block_height: row.block_number,
        timestamp: row.block_timestamp / 1000,
        fee_details: Some(history_fee_details(ticker, &figures, bandwidth_price, is_trx_transfer)?),
        coin: ticker.to_owned(),
        kmd_rewards: None,
        transaction_type,
        memo: None,
    }))
}

// ---------------------------------------------------------------------------------------------------------------
// A row of the token listing
// ---------------------------------------------------------------------------------------------------------------

/// A row of `GET /v1/accounts/{address}/transactions/trc20`, the members the history reads.
#[derive(Debug, Deserialize)]
struct Trc20ListingRow {
    transaction_id: String,
    /// Milliseconds.
    block_timestamp: u64,
    from: String,
    to: String,
    value: String,
    #[serde(rename = "type")]
    kind: String,
    token_info: Trc20ListingToken,
}

#[derive(Debug, Deserialize)]
struct Trc20ListingToken {
    address: String,
}

/// One TRC20 transfer event of the token listing.
pub(crate) struct Trc20Event {
    /// Lowercase hex.
    pub tx_id: String,
    pub timestamp: u64,
    pub from: TronAddress,
    pub to: TronAddress,
    /// In the token's smallest unit.
    pub value: U256,
    /// The token contract.
    pub contract: Address,
}

impl Trc20Event {
    /// Reads a row of the token listing. `Ok(None)` for a row that is not a transfer event.
    ///
    /// # Errors
    ///
    /// The reason the row cannot be read.
    pub(crate) fn read(row: &Json) -> Result<Option<Self>, String> {
        let row = Trc20ListingRow::deserialize(row).map_err(|e| e.to_string())?;
        if row.kind != "Transfer" {
            return Ok(None);
        }
        let address = |name: &str, text: &str| TronAddress::parse(text).map_err(|e| format!("{name}: {e}"));
        let tx_id = row.transaction_id.to_ascii_lowercase();
        tx_id_bytes(&tx_id)?;
        Ok(Some(Trc20Event {
            tx_id,
            timestamp: row.block_timestamp / 1000,
            from: address("from", &row.from)?,
            to: address("to", &row.to)?,
            value: U256::from_dec_str(&row.value).map_err(|e| format!("value: {e:?}"))?,
            contract: address("token", &row.token_info.address)?.to_evm_address(),
        }))
    }

    /// What tells two events of one listing apart, before their position among equal events. The token contract
    /// is part of it, so the position of an event is counted among the events of its own token only: it is the
    /// same in a listing of every token and in a listing the provider filtered by the contract.
    fn key(&self) -> (String, Address, Address, Address, U256) {
        (
            self.tx_id.clone(),
            self.contract,
            self.from.to_evm_address(),
            self.to.to_evm_address(),
            self.value,
        )
    }

    /// The id of the history row of this event: a hash of the transaction id, the token contract, the two
    /// addresses, the position among the equal events of the transaction (`occurrence`, from 0) and the value. A
    /// transaction can carry several transfer events, of one token or of several, so the transaction id alone
    /// does not name a row.
    pub(crate) fn internal_id(&self, occurrence: u32) -> Vec<u8> {
        let mut preimage = Vec::with_capacity(64 + 20 + 20 + 20 + 4 + 78);
        preimage.extend_from_slice(self.tx_id.as_bytes());
        preimage.extend_from_slice(self.contract.as_bytes());
        preimage.extend_from_slice(self.from.to_evm_address().as_bytes());
        preimage.extend_from_slice(self.to.to_evm_address().as_bytes());
        preimage.extend_from_slice(&occurrence.to_be_bytes());
        preimage.extend_from_slice(self.value.to_string().as_bytes());
        sha256(&preimage).to_vec()
    }
}

/// The cost figures of a `gettransactioninfobyid` answer. The node leaves a figure out when it is zero (a transfer
/// paid from staked bandwidth answers a receipt with `net_usage` only), so an absent figure is zero.
///
/// # Errors
///
/// A negative figure.
pub(crate) fn receipt_figures(receipt: &TransactionInfoResponse) -> Result<ReceiptFigures, String> {
    let sun = |name: &str, figure: Option<i64>| {
        u64::try_from(figure.unwrap_or(0)).map_err(|_| format!("a negative {name} in the receipt"))
    };
    let inner = receipt.receipt.as_ref();
    Ok(ReceiptFigures {
        fee_sun: sun("fee", receipt.fee)?,
        net_fee_sun: sun("net_fee", inner.and_then(|inner| inner.net_fee))?,
        energy_fee_sun: sun("energy_fee", inner.and_then(|inner| inner.energy_fee))?,
        net_usage: inner.and_then(|inner| inner.net_usage).unwrap_or(0),
        energy_usage_total: inner.and_then(|inner| inner.energy_usage_total).unwrap_or(0),
    })
}

/// The history row of the TRC20 coin `ticker` (with `decimals`, on the platform `platform_ticker`) for one
/// transfer event, with the block height and the fee from the transaction's receipt. The fee is paid in the
/// platform coin and is not part of `spent_by_me`. The listing carries no raw transaction, so `tx_hex` is empty.
///
/// # Errors
///
/// The reason the row cannot be built: a receipt without a block number or with a negative figure, or an amount
/// that cannot be scaled.
#[allow(clippy::too_many_arguments)]
pub(crate) fn trc20_history_row(
    ticker: &str,
    platform_ticker: &str,
    decimals: u8,
    event: &Trc20Event,
    internal_id: Vec<u8>,
    receipt: &TransactionInfoResponse,
    mine: &HashSet<Address>,
    bandwidth_price: NonZeroU64,
) -> Result<TransactionDetails, String> {
    let block_height = receipt.block_number.ok_or("the receipt carries no block number")?;
    let figures = receipt_figures(receipt)?;
    let amount = u256_to_big_decimal(event.value, decimals).map_err(|e| e.into_inner().to_string())?;
    let zero = BigDecimal::from(0);
    let spent_by_me = if mine.contains(&event.from.to_evm_address()) {
        amount.clone()
    } else {
        zero.clone()
    };
    let received_by_me = if mine.contains(&event.to.to_evm_address()) {
        amount.clone()
    } else {
        zero
    };
    Ok(TransactionDetails {
        unsigned: None,
        tx_json: None,
        tx_hex: BytesJson(Vec::new()),
        tx_hash: event.tx_id.clone(),
        from: vec![event.from.to_base58()],
        to: vec![event.to.to_base58()],
        total_amount: amount,
        my_balance_change: &received_by_me - &spent_by_me,
        spent_by_me,
        received_by_me,
        block_height,
        timestamp: event.timestamp,
        fee_details: Some(history_fee_details(platform_ticker, &figures, bandwidth_price, false)?),
        coin: ticker.to_owned(),
        internal_id: BytesJson(internal_id),
        kmd_rewards: None,
        transaction_type: TransactionType::StandardTransfer,
        memo: None,
    })
}
