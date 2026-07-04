#[cfg(not(target_arch = "wasm32"))]
use crate::sql_tx_history_storage::SqliteTxHistoryStorage;
use crate::{lp_coinfind_or_err, BlockHeightAndTime, CoinFindError, HistorySyncState, MarketCoinOps, MmCoinEnum,
            Transaction, TransactionDetails, TransactionType, TxFeeDetails};
use async_trait::async_trait;
use common::mm_number::BigDecimal;
use common::{calc_total_pages, ten, HttpStatusCode, PagingOptionsEnum, StatusCode};
use derive_more::Display;
use futures::compat::Future01CompatExt;
use kdf_crypto::sha256;
use keys::{Address, CashAddress};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use rpc::v1::types::{Bytes as BytesJson, ToTxHash};
use std::collections::HashSet;

#[derive(Debug)]
pub enum RemoveTxResult {
    TxRemoved,
    TxDidNotExist,
}

impl RemoveTxResult {
    pub fn tx_existed(&self) -> bool { matches!(self, RemoveTxResult::TxRemoved) }
}

pub struct GetHistoryResult {
    pub transactions: Vec<TransactionDetails>,
    pub skipped: usize,
    pub total: usize,
}

pub trait TxHistoryStorageError: std::fmt::Debug + NotMmError + Send {}

#[async_trait]
pub trait TxHistoryStorage: Send + Sync + 'static {
    type Error: TxHistoryStorageError;

    /// Initializes collection/tables in storage for a specified coin
    async fn init(&self, for_coin: &str) -> Result<(), MmError<Self::Error>>;

    async fn is_initialized_for(&self, for_coin: &str) -> Result<bool, MmError<Self::Error>>;

    /// Adds multiple transactions to the selected coin's history
    /// Also consider adding tx_hex to the cache during this operation
    async fn add_transactions_to_history(
        &self,
        for_coin: &str,
        transactions: impl IntoIterator<Item = TransactionDetails> + Send + 'static,
    ) -> Result<(), MmError<Self::Error>>;

    /// Removes the transaction by internal_id from the selected coin's history
    async fn remove_tx_from_history(
        &self,
        for_coin: &str,
        internal_id: &BytesJson,
    ) -> Result<RemoveTxResult, MmError<Self::Error>>;

    /// Gets the transaction by internal_id from the selected coin's history
    async fn get_tx_from_history(
        &self,
        for_coin: &str,
        internal_id: &BytesJson,
    ) -> Result<Option<TransactionDetails>, MmError<Self::Error>>;

    /// Returns whether the history contains unconfirmed transactions
    async fn history_contains_unconfirmed_txes(&self, for_coin: &str) -> Result<bool, MmError<Self::Error>>;

    /// Gets the unconfirmed transactions from the history
    async fn get_unconfirmed_txes_from_history(
        &self,
        for_coin: &str,
    ) -> Result<Vec<TransactionDetails>, MmError<Self::Error>>;

    /// Updates transaction in the selected coin's history
    async fn update_tx_in_history(&self, for_coin: &str, tx: &TransactionDetails) -> Result<(), MmError<Self::Error>>;

    async fn history_has_tx_hash(&self, for_coin: &str, tx_hash: &str) -> Result<bool, MmError<Self::Error>>;

    async fn unique_tx_hashes_num_in_history(&self, for_coin: &str) -> Result<usize, MmError<Self::Error>>;

    async fn add_tx_to_cache(
        &self,
        for_coin: &str,
        tx_hash: &BytesJson,
        tx_hex: &BytesJson,
    ) -> Result<(), MmError<Self::Error>>;

    async fn tx_bytes_from_cache(
        &self,
        for_coin: &str,
        tx_hash: &BytesJson,
    ) -> Result<Option<BytesJson>, MmError<Self::Error>>;

    async fn get_history(
        &self,
        coin_type: HistoryCoinType,
        paging: PagingOptionsEnum<BytesJson>,
        limit: usize,
    ) -> Result<GetHistoryResult, MmError<Self::Error>>;
}

pub trait DisplayAddress {
    fn display_address(&self) -> String;
}

impl DisplayAddress for Address {
    fn display_address(&self) -> String { self.to_string() }
}

impl DisplayAddress for CashAddress {
    fn display_address(&self) -> String { self.encode().expect("A valid cash address") }
}

pub struct TxDetailsBuilder<'a, Addr: DisplayAddress, Tx: Transaction> {
    coin: String,
    tx: &'a Tx,
    my_addresses: HashSet<Addr>,
    total_amount: BigDecimal,
    received_by_me: BigDecimal,
    spent_by_me: BigDecimal,
    from_addresses: HashSet<Addr>,
    to_addresses: HashSet<Addr>,
    transaction_type: TransactionType,
    block_height_and_time: Option<BlockHeightAndTime>,
    tx_fee: Option<TxFeeDetails>,
}

impl<'a, Addr: Clone + DisplayAddress + Eq + std::hash::Hash, Tx: Transaction> TxDetailsBuilder<'a, Addr, Tx> {
    pub fn new(
        coin: String,
        tx: &'a Tx,
        block_height_and_time: Option<BlockHeightAndTime>,
        my_addresses: impl IntoIterator<Item = Addr>,
    ) -> Self {
        TxDetailsBuilder {
            coin,
            tx,
            my_addresses: my_addresses.into_iter().collect(),
            total_amount: Default::default(),
            received_by_me: Default::default(),
            spent_by_me: Default::default(),
            from_addresses: Default::default(),
            to_addresses: Default::default(),
            block_height_and_time,
            transaction_type: TransactionType::StandardTransfer,
            tx_fee: None,
        }
    }

    pub fn set_tx_fee(&mut self, tx_fee: Option<TxFeeDetails>) { self.tx_fee = tx_fee; }

    pub fn set_transaction_type(&mut self, tx_type: TransactionType) { self.transaction_type = tx_type; }

    pub fn transferred_to(&mut self, address: Addr, amount: &BigDecimal) {
        if self.my_addresses.contains(&address) {
            self.received_by_me += amount;
        }
        self.to_addresses.insert(address);
    }

    pub fn transferred_from(&mut self, address: Addr, amount: &BigDecimal) {
        if self.my_addresses.contains(&address) {
            self.spent_by_me += amount;
        }
        self.total_amount += amount;
        self.from_addresses.insert(address);
    }

    pub fn build(self) -> TransactionDetails {
        let (block_height, timestamp) = match self.block_height_and_time {
            Some(height_with_time) => (height_with_time.height, height_with_time.timestamp),
            None => (0, 0),
        };

        let mut from: Vec<_> = self
            .from_addresses
            .iter()
            .map(DisplayAddress::display_address)
            .collect();
        from.sort();

        let mut to: Vec<_> = self.to_addresses.iter().map(DisplayAddress::display_address).collect();
        to.sort();

        let tx_hash = self.tx.tx_hash();
        let internal_id = match &self.transaction_type {
            TransactionType::TokenTransfer(token_id) => {
                let mut bytes_for_hash = tx_hash.0.clone();
                bytes_for_hash.extend_from_slice(&token_id.0);
                sha256(&bytes_for_hash).to_vec().into()
            },
            TransactionType::StakingDelegation
            | TransactionType::RemoveDelegation
            | TransactionType::ClaimDelegationRewards
            | TransactionType::StandardTransfer => tx_hash.clone(),
        };

        TransactionDetails {
            coin: self.coin,
            tx_hex: self.tx.tx_hex().into(),
            tx_hash: tx_hash.to_tx_hash(),
            from,
            to,
            total_amount: self.total_amount,
            my_balance_change: &self.received_by_me - &self.spent_by_me,
            spent_by_me: self.spent_by_me,
            received_by_me: self.received_by_me,
            block_height,
            timestamp,
            fee_details: self.tx_fee,
            internal_id,
            kmd_rewards: None,
            transaction_type: self.transaction_type,
        }
    }
}

#[derive(Deserialize)]
pub struct MyTxHistoryRequestV2 {
    coin: String,
    #[serde(default = "ten")]
    limit: usize,
    #[serde(default)]
    paging_options: PagingOptionsEnum<BytesJson>,
}

#[derive(Serialize)]
pub struct MyTxHistoryDetails {
    #[serde(flatten)]
    details: TransactionDetails,
    confirmations: u64,
}

#[derive(Serialize)]
pub struct MyTxHistoryResponseV2 {
    coin: String,
    current_block: u64,
    transactions: Vec<MyTxHistoryDetails>,
    sync_status: HistorySyncState,
    limit: usize,
    skipped: usize,
    total: usize,
    total_pages: usize,
    paging_options: PagingOptionsEnum<BytesJson>,
}

#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum MyTxHistoryErrorV2 {
    CoinIsNotActive(String),
    StorageIsNotInitialized(String),
    StorageError(String),
    RpcError(String),
    NotSupportedFor(String),
    #[cfg(target_arch = "wasm32")]
    NotSupportedInWasm,
}

impl HttpStatusCode for MyTxHistoryErrorV2 {
    fn status_code(&self) -> StatusCode {
        match self {
            MyTxHistoryErrorV2::CoinIsNotActive(_) => StatusCode::NOT_FOUND,
            MyTxHistoryErrorV2::NotSupportedFor(_) => StatusCode::BAD_REQUEST,
            MyTxHistoryErrorV2::StorageIsNotInitialized(_)
            | MyTxHistoryErrorV2::StorageError(_)
            | MyTxHistoryErrorV2::RpcError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            #[cfg(target_arch = "wasm32")]
            MyTxHistoryErrorV2::NotSupportedInWasm => StatusCode::BAD_REQUEST,
        }
    }
}

impl From<CoinFindError> for MyTxHistoryErrorV2 {
    fn from(err: CoinFindError) -> Self {
        match err {
            CoinFindError::NoSuchCoin { coin } => MyTxHistoryErrorV2::CoinIsNotActive(coin),
        }
    }
}

impl<T: TxHistoryStorageError> From<T> for MyTxHistoryErrorV2 {
    fn from(err: T) -> Self {
        let msg = format!("{:?}", err);
        MyTxHistoryErrorV2::StorageError(msg)
    }
}

pub enum HistoryCoinType {
    Coin(String),
    Token { platform: String, token_id: BytesJson },
    // TODO extend with the L2 required info
    L2 { platform: String },
}

impl HistoryCoinType {
    fn storage_ticker(&self) -> &str {
        match self {
            HistoryCoinType::Coin(ticker) => ticker,
            HistoryCoinType::Token { platform, .. } | HistoryCoinType::L2 { platform } => platform,
        }
    }
}

trait GetHistoryCoinType {
    fn get_history_coin_type(&self) -> Option<HistoryCoinType>;
}

impl GetHistoryCoinType for MmCoinEnum {
    fn get_history_coin_type(&self) -> Option<HistoryCoinType> {
        match self {
            MmCoinEnum::Bch(bch) => Some(HistoryCoinType::Coin(bch.ticker().to_owned())),
            MmCoinEnum::SlpToken(token) => Some(HistoryCoinType::Token {
                platform: token.platform_ticker().to_owned(),
                token_id: token.token_id().take().to_vec().into(),
            }),
            _ => None,
        }
    }
}

fn skipped_by_paging(
    transactions: &[TransactionDetails],
    paging: &PagingOptionsEnum<BytesJson>,
    limit: usize,
) -> Option<usize> {
    match paging {
        PagingOptionsEnum::FromId(from_id) => transactions
            .iter()
            .position(|item| item.internal_id == *from_id)
            .map(|idx| idx + 1),
        PagingOptionsEnum::PageNumber(page_number) => Some((page_number.get() - 1) * limit),
    }
}

fn build_response_from_history(
    request: MyTxHistoryRequestV2,
    sync_status: HistorySyncState,
    current_block: u64,
    history: Vec<TransactionDetails>,
) -> MyTxHistoryResponseV2 {
    let total = history.len();
    let (transactions, skipped) = match skipped_by_paging(&history, &request.paging_options, request.limit) {
        Some(skipped) => {
            let transactions = history
                .into_iter()
                .skip(skipped)
                .take(request.limit)
                .map(|details| {
                    let confirmations = if details.block_height == 0 || details.block_height > current_block {
                        0
                    } else {
                        current_block + 1 - details.block_height
                    };
                    MyTxHistoryDetails { confirmations, details }
                })
                .collect();
            (transactions, skipped)
        },
        None => (Vec::new(), 0),
    };

    MyTxHistoryResponseV2 {
        coin: request.coin,
        current_block,
        transactions,
        sync_status,
        limit: request.limit,
        skipped,
        total,
        total_pages: calc_total_pages(total, request.limit),
        paging_options: request.paging_options,
    }
}

#[cfg(not(target_arch = "wasm32"))]
async fn build_response_from_runtime_history(
    ctx: MmArc,
    request: MyTxHistoryRequestV2,
    coin: MmCoinEnum,
) -> Result<MyTxHistoryResponseV2, MmError<MyTxHistoryErrorV2>> {
    let current_block = coin
        .current_block()
        .compat()
        .await
        .map_to_mm(MyTxHistoryErrorV2::RpcError)?;
    let history = coin
        .load_history_from_file(&ctx)
        .compat()
        .await
        .map_err(|e| MmError::new(MyTxHistoryErrorV2::RpcError(e.to_string())))?;
    let sync_status = coin.history_sync_status();

    Ok(build_response_from_history(
        request,
        sync_status,
        current_block,
        history,
    ))
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn my_tx_history_v2_rpc(
    ctx: MmArc,
    request: MyTxHistoryRequestV2,
) -> Result<MyTxHistoryResponseV2, MmError<MyTxHistoryErrorV2>> {
    let coin = lp_coinfind_or_err(&ctx, &request.coin).await.mm_err(Into::into)?;
    if matches!(coin, MmCoinEnum::UtxoCoin(_) | MmCoinEnum::QtumCoin(_)) {
        return build_response_from_runtime_history(ctx, request, coin).await;
    }

    let tx_history_storage = SqliteTxHistoryStorage(
        ctx.sqlite_connection
            .ok_or(MmError::new(MyTxHistoryErrorV2::StorageIsNotInitialized(
                "sqlite_connection is not initialized".into(),
            )))?
            .clone(),
    );
    let history_coin_type = match coin.get_history_coin_type() {
        Some(t) => t,
        None => return MmError::err(MyTxHistoryErrorV2::NotSupportedFor(coin.ticker().to_owned())),
    };
    let is_storage_init = tx_history_storage
        .is_initialized_for(history_coin_type.storage_ticker())
        .await
        .mm_err(Into::into)?;
    if !is_storage_init {
        let msg = format!("Storage is not initialized for {}", history_coin_type.storage_ticker());
        return MmError::err(MyTxHistoryErrorV2::StorageIsNotInitialized(msg));
    }
    let current_block = coin
        .current_block()
        .compat()
        .await
        .map_to_mm(MyTxHistoryErrorV2::RpcError)?;

    let history = tx_history_storage
        .get_history(history_coin_type, request.paging_options.clone(), request.limit)
        .await
        .mm_err(Into::into)?;

    let history = history
        .transactions
        .into_iter()
        .map(|mut details| {
            // it can be the platform ticker instead of the token ticker for a pre-saved record
            if details.coin != request.coin {
                details.coin = request.coin.clone();
            }
            details
        })
        .collect();
    let sync_status = coin.history_sync_status();

    Ok(build_response_from_history(
        request,
        sync_status,
        current_block,
        history,
    ))
}

/// Shared-envelope address-scope selector for the shielded history request (R39.8.5).
///
/// Accepted for wire-envelope compatibility and, in the forward-spec data path
/// (§39.8.0b), echoed back unchanged in the response. It does not scope which
/// shielded transactions are returned. On the current native-only substrate the
/// method always fails before any response is built (§39.8.0a), so the selector
/// is validated at the boundary and otherwise unused.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Default, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ZCoinTxHistoryTarget {
    #[default]
    Iguana,
    AccountId {
        account_id: u32,
    },
    AddressId(crate::hd_wallet::HDAddressId),
}

/// Shielded-coin transaction-history request envelope (R39.8.3).
///
/// Mirrors the shared v2 history request but keys paging on a signed 64-bit
/// integer identifier (R39.8.4, R39.8.10) and carries the shared `target`
/// selector (R39.8.5).
#[cfg(not(target_arch = "wasm32"))]
#[derive(Deserialize)]
pub struct ZCoinTxHistoryRequest {
    pub coin: String,
    #[serde(default = "ten")]
    pub limit: usize,
    #[serde(default)]
    pub paging_options: PagingOptionsEnum<i64>,
    #[serde(default)]
    pub target: ZCoinTxHistoryTarget,
}

/// `z_coin_tx_history` handler — clean-failure contract on the current substrate (R39.8.0a).
///
/// Reloaded's `ZCoin` is a native-full-node port with no shielded wallet-history
/// store, no incoming-viewing-key compact-block scanner, and no signed-integer
/// `internal_id` keyspace (verdict B, §39.8.0). The shielded history therefore
/// cannot be produced here without fabricating note ownership, which would be a
/// correctness and privacy hazard. The method is still dispatched and validates
/// its input at the boundary:
/// - an unactivated `coin` resolves to `CoinIsNotActive`;
/// - an activated non-shielded coin resolves to `NotSupportedFor`;
/// - an activated shielded coin resolves to `StorageIsNotInitialized`, because
///   no wallet-history store exists on this substrate.
///
/// It never panics, never fabricates or partially synthesizes history entries,
/// and never emits shielded amounts/addresses it cannot derive. The success
/// type is the shared v2 envelope (R39.8.10); it is never constructed here.
#[cfg(not(target_arch = "wasm32"))]
pub async fn z_coin_tx_history_rpc(
    ctx: MmArc,
    request: ZCoinTxHistoryRequest,
) -> Result<MyTxHistoryResponseV2, MmError<MyTxHistoryErrorV2>> {
    let coin = lp_coinfind_or_err(&ctx, &request.coin).await.mm_err(Into::into)?;
    match coin {
        MmCoinEnum::ZCoin(_) => MmError::err(MyTxHistoryErrorV2::StorageIsNotInitialized(format!(
            "Shielded transaction-history store is not initialized for {}",
            request.coin
        ))),
        _ => MmError::err(MyTxHistoryErrorV2::NotSupportedFor(request.coin)),
    }
}

#[cfg(target_arch = "wasm32")]
pub async fn my_tx_history_v2_rpc(
    _ctx: MmArc,
    _request: MyTxHistoryRequestV2,
) -> Result<MyTxHistoryResponseV2, MmError<MyTxHistoryErrorV2>> {
    MmError::err(MyTxHistoryErrorV2::NotSupportedInWasm)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod z_coin_tx_history_tests {
    use super::*;
    use std::num::NonZeroUsize;

    fn tx_details(id: u8, block_height: u64) -> TransactionDetails {
        TransactionDetails {
            tx_hex: vec![id].into(),
            tx_hash: format!("{id:02x}"),
            from: vec![],
            to: vec![],
            total_amount: BigDecimal::from(0),
            spent_by_me: BigDecimal::from(0),
            received_by_me: BigDecimal::from(0),
            my_balance_change: BigDecimal::from(0),
            block_height,
            timestamp: 0,
            fee_details: None,
            coin: "RICK".to_owned(),
            internal_id: vec![id].into(),
            kmd_rewards: None,
            transaction_type: TransactionType::StandardTransfer,
        }
    }

    #[test]
    fn from_id_not_found_returns_empty_page_with_total_preserved() {
        let request = MyTxHistoryRequestV2 {
            coin: "RICK".to_owned(),
            limit: 2,
            paging_options: PagingOptionsEnum::FromId(vec![99u8].into()),
        };
        let response = build_response_from_history(request, HistorySyncState::Finished, 100, vec![
            tx_details(1, 98),
            tx_details(2, 99),
            tx_details(3, 100),
        ]);

        assert_eq!(response.total, 3);
        assert_eq!(response.skipped, 0);
        assert!(response.transactions.is_empty());
    }

    #[test]
    fn from_id_found_returns_following_records() {
        let request = MyTxHistoryRequestV2 {
            coin: "RICK".to_owned(),
            limit: 2,
            paging_options: PagingOptionsEnum::FromId(vec![2u8].into()),
        };
        let response = build_response_from_history(request, HistorySyncState::Finished, 100, vec![
            tx_details(1, 97),
            tx_details(2, 98),
            tx_details(3, 99),
            tx_details(4, 100),
        ]);

        assert_eq!(response.total, 4);
        assert_eq!(response.skipped, 2);
        assert_eq!(response.transactions.len(), 2);
        assert_eq!(response.transactions[0].details.internal_id, vec![3u8].into());
        assert_eq!(response.transactions[1].details.internal_id, vec![4u8].into());
        assert_eq!(response.transactions[0].confirmations, 2);
        assert_eq!(response.transactions[1].confirmations, 1);
    }

    #[test]
    fn page_number_paging_uses_expected_offset() {
        let request = MyTxHistoryRequestV2 {
            coin: "RICK".to_owned(),
            limit: 2,
            paging_options: PagingOptionsEnum::PageNumber(NonZeroUsize::new(2).unwrap()),
        };
        let response = build_response_from_history(request, HistorySyncState::Finished, 100, vec![
            tx_details(1, 97),
            tx_details(2, 98),
            tx_details(3, 99),
            tx_details(4, 100),
        ]);

        assert_eq!(response.total, 4);
        assert_eq!(response.skipped, 2);
        assert_eq!(response.transactions.len(), 2);
        assert_eq!(response.transactions[0].details.internal_id, vec![3u8].into());
        assert_eq!(response.transactions[1].details.internal_id, vec![4u8].into());
    }

    // R39.8.3/R39.8.4/R39.8.5: the request envelope deserializes with `coin`,
    // `limit`, `paging_options` and the shared `target`, and `paging_options`
    // keys on a signed 64-bit integer (FromId).
    #[test]
    fn deserializes_request_envelope() {
        // Defaults: omitted limit -> 10, omitted paging_options -> PageNumber(1),
        // omitted target -> iguana.
        let req: ZCoinTxHistoryRequest = serde_json::from_str(r#"{"coin":"ZOMBIE"}"#).unwrap();
        assert_eq!(req.coin, "ZOMBIE");
        assert_eq!(req.limit, 10);
        assert_eq!(
            req.paging_options,
            PagingOptionsEnum::PageNumber(NonZeroUsize::new(1).unwrap())
        );
        assert!(matches!(req.target, ZCoinTxHistoryTarget::Iguana));

        // Explicit PageNumber paging with an iguana target.
        let req: ZCoinTxHistoryRequest = serde_json::from_str(
            r#"{"coin":"ZOMBIE","limit":25,"paging_options":{"PageNumber":3},"target":{"type":"iguana"}}"#,
        )
        .unwrap();
        assert_eq!(req.limit, 25);
        assert_eq!(
            req.paging_options,
            PagingOptionsEnum::PageNumber(NonZeroUsize::new(3).unwrap())
        );
        assert!(matches!(req.target, ZCoinTxHistoryTarget::Iguana));

        // FromId paging carries a signed 64-bit integer (R39.8.4, R39.8.10).
        let req: ZCoinTxHistoryRequest =
            serde_json::from_str(r#"{"coin":"ZOMBIE","paging_options":{"FromId":-7}}"#).unwrap();
        assert_eq!(req.paging_options, PagingOptionsEnum::FromId(-7i64));
    }

    // R39.8.0a / R39.8.4: the three boundary discriminants serialize to the
    // documented `error_type` wire names and carry the upstream-aligned HTTP
    // statuses of the shared v2 tx-history error enum (404 / 400 / 500).
    #[test]
    fn error_discriminants_and_status_codes() {
        let not_active = MyTxHistoryErrorV2::CoinIsNotActive("ZOMBIE".into());
        assert_eq!(
            serde_json::to_value(&not_active).unwrap()["error_type"],
            serde_json::json!("CoinIsNotActive")
        );
        assert_eq!(not_active.status_code(), StatusCode::NOT_FOUND);

        let not_supported = MyTxHistoryErrorV2::NotSupportedFor("RICK".into());
        assert_eq!(
            serde_json::to_value(&not_supported).unwrap()["error_type"],
            serde_json::json!("NotSupportedFor")
        );
        assert_eq!(not_supported.status_code(), StatusCode::BAD_REQUEST);

        let no_storage = MyTxHistoryErrorV2::StorageIsNotInitialized("ZOMBIE".into());
        assert_eq!(
            serde_json::to_value(&no_storage).unwrap()["error_type"],
            serde_json::json!("StorageIsNotInitialized")
        );
        assert_eq!(no_storage.status_code(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
