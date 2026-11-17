// crd:pin-begin
use crate::account::{AccountId, AccountInfo, AccountType, AccountWithCoins, AccountWithEnabledFlag, EnabledAccountId,
                     EnabledAccountType, HwPubkey};
// crd:pin-end
use async_trait::async_trait;
use derive_more::Display;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use mm2_number::BigDecimal;
use rpc::v1::types::H160 as H160Json;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error as StdError;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod account_storage_tests;
// On the browser target the suite is meant to exercise the IndexedDB backend
// via `wasm-bindgen-test`. That backend is still a stub (D2), so wiring the
// tests into `wasm32` is postponed until the real port lands; for now they are
// compiled for native only.
#[cfg(not(target_arch = "wasm32"))] mod sqlite_storage;
#[cfg(target_arch = "wasm32")] mod wasm_storage;

/// Sentinel `account_idx` used by variants that do not carry an HD index.
const DEFAULT_ACCOUNT_IDX: u32 = 0; // crd:pin
/// Sentinel device pubkey used by variants that are not hardware-wallet keyed.
const DEFAULT_DEVICE_PUB: HwPubkey = H160Json([0; 20]); // crd:pin

pub(crate) type AccountStorageBoxed = Box<dyn AccountStorage>;
pub type AccountStorageResult<T> = MmResult<T, AccountStorageError>;

#[derive(Debug, Display)]
pub enum AccountStorageError {
    // crd:pin-begin
    #[display(fmt = "Account {_0:?} was not found")]
    NoSuchAccount(AccountId),
    #[display(fmt = "There is no enabled account")]
    NoEnabledAccount,
    #[display(fmt = "Account {_0:?} is already present")]
    AccountExistsAlready(AccountId),
    #[display(fmt = "Failed to persist account changes: {_0}")]
    ErrorSaving(String),
    #[display(fmt = "Failed to read account: {_0}")]
    ErrorLoading(String),
    #[display(fmt = "Failed to decode a stored account: {_0}")]
    ErrorDeserializing(String),
    #[display(fmt = "Failed to encode an account: {_0}")]
    ErrorSerializing(String),
    #[display(fmt = "Internal error: {_0}")]
    Internal(String),
    // crd:pin-end
}

impl StdError for AccountStorageError {}

impl AccountStorageError {
    /// Built when the enabled-account marker points at an identity that is
    /// absent from the accounts table — a broken storage invariant.
    pub(crate) fn unknown_account_in_enabled_table(account_id: AccountId) -> AccountStorageError {
        AccountStorageError::Internal(format!(
            "Enabled-account marker references {account_id:?}, which is missing from the accounts table"
        ))
    }
}

impl AccountId {
    /// Projects an identity onto its three storage columns.
    ///
    /// Columns that the variant does not use are filled with the module
    /// sentinels (`DEFAULT_ACCOUNT_IDX` / `DEFAULT_DEVICE_PUB`), which keeps
    /// the composite primary key collision-free across variants.
    pub(crate) fn to_tuple(&self) -> (AccountType, u32, HwPubkey) {
        match *self {
            AccountId::Iguana => (AccountType::Iguana, DEFAULT_ACCOUNT_IDX, DEFAULT_DEVICE_PUB), // crd:pin
            AccountId::HD { account_idx } => (AccountType::HD, account_idx, DEFAULT_DEVICE_PUB), // crd:pin
            AccountId::HW { device_pubkey } => (AccountType::HW, DEFAULT_ACCOUNT_IDX, device_pubkey), // crd:pin
        }
    }

    /// Reconstructs an identity from its three storage columns.
    ///
    /// The sentinel columns of the inactive dimensions are validated so a
    /// malformed row cannot masquerade as a different variant.
    // crd:pin-begin
    pub(crate) fn try_from_tuple(
        account_type: AccountType,
        account_idx: u32,
        device_pubkey: HwPubkey,
    ) -> AccountStorageResult<AccountId> {
        // crd:pin-end
        // A variant is only well-formed when the column(s) it does not own still
        // hold their sentinel. Evaluate those two predicates once, then let the
        // matched discriminant decide which of them must be satisfied.
        let idx_unused = account_idx == DEFAULT_ACCOUNT_IDX;
        let pubkey_unused = device_pubkey == DEFAULT_DEVICE_PUB;
        let reject = || {
            MmError::err(AccountStorageError::ErrorDeserializing(format!(
                "Malformed AccountId columns: {account_type:?}/{account_idx:?}/{device_pubkey:?}"
            )))
        };
        match account_type {
            AccountType::HD if pubkey_unused => Ok(AccountId::HD { account_idx }),
            AccountType::HW if idx_unused => Ok(AccountId::HW { device_pubkey }),
            AccountType::Iguana if idx_unused && pubkey_unused => Ok(AccountId::Iguana),
            _ => reject(),
        }
    }
}

impl EnabledAccountId {
    /// Projects an enabled identity onto its columns.
    ///
    /// The device-pubkey column is always the sentinel, since no enabled
    /// variant is hardware-wallet keyed.
    pub(crate) fn to_tuple(self) -> (EnabledAccountType, u32, HwPubkey) {
        match self {
            EnabledAccountId::Iguana => (EnabledAccountType::Iguana, DEFAULT_ACCOUNT_IDX, DEFAULT_DEVICE_PUB), // crd:pin
            EnabledAccountId::HD { account_idx } => (EnabledAccountType::HD, account_idx, DEFAULT_DEVICE_PUB), // crd:pin
        }
    }

    /// Reconstructs an enabled identity from the `(type, idx)` pair.
    // crd:pin-begin
    pub(crate) fn try_from_pair(
        account_type: EnabledAccountType,
        account_idx: u32,
    ) -> AccountStorageResult<EnabledAccountId> {
        // crd:pin-end
        match account_type {
            EnabledAccountType::HD => Ok(EnabledAccountId::HD { account_idx }), // crd:pin
            // The enabled-Iguana row carries no index, so a non-sentinel value
            // in that column marks the row as corrupt.
            EnabledAccountType::Iguana => {
                if account_idx == DEFAULT_ACCOUNT_IDX {
                    Ok(EnabledAccountId::Iguana)
                } else {
                    MmError::err(AccountStorageError::ErrorDeserializing(format!(
                        "Malformed EnabledAccountId columns: {account_type:?}/{account_idx:?}"
                    )))
                }
            },
        }
    }
}

/// Constructs the target-appropriate [`AccountStorageBoxed`] for a context.
pub(crate) struct AccountStorageBuilder<'a> {
    ctx: &'a MmArc,
}

impl<'a> AccountStorageBuilder<'a> {
    pub fn new(ctx: &'a MmArc) -> Self { AccountStorageBuilder { ctx } }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn build(self) -> AccountStorageResult<AccountStorageBoxed> {
        let storage = sqlite_storage::SqliteAccountStorage::new(self.ctx)?;
        Ok(Box::new(storage))
    }

    #[cfg(target_arch = "wasm32")]
    pub fn build(self) -> AccountStorageResult<AccountStorageBoxed> {
        Ok(Box::new(wasm_storage::WasmAccountStorage::new(self.ctx)))
    }
}

/// Persistence surface for GUI-facing account state.
///
/// Every method is async and returns the crate's `MmError`-wrapped result.
/// Input validation happens above this trait (chapter 24 R3); backends trust
/// the values they receive.
#[async_trait]
pub(crate) trait AccountStorage: Send + Sync {
    /// Bootstraps the backend (idempotent: safe to call before every use).
    async fn init(&self) -> AccountStorageResult<()>;

    /// Returns the activated tickers of `account_id`.
    async fn load_account_coins(&self, account_id: AccountId) -> AccountStorageResult<BTreeSet<String>>;

    /// Returns every stored account keyed by its identity.
    #[allow(dead_code)]
    async fn load_accounts(&self) -> AccountStorageResult<BTreeMap<AccountId, AccountInfo>>;

    /// Returns every stored account, tagging exactly one as enabled.
    async fn load_accounts_with_enabled_flag(
        &self, // crd:pin
    ) -> AccountStorageResult<BTreeMap<AccountId, AccountWithEnabledFlag>>; // crd:pin

    /// Returns the enabled account id, or `NoEnabledAccount` if none is set.
    #[allow(dead_code)]
    async fn load_enabled_account_id(&self) -> AccountStorageResult<EnabledAccountId>;

    /// Returns the enabled account together with its activated coins.
    async fn load_enabled_account_with_coins(&self) -> AccountStorageResult<AccountWithCoins>;

    /// Marks `account_id` as the enabled account, failing if it is unknown.
    async fn enable_account(&self, account_id: EnabledAccountId) -> AccountStorageResult<()>;

    /// Persists a brand-new account, failing if its identity already exists.
    async fn upload_account(&self, account: AccountInfo) -> AccountStorageResult<()>;

    /// Removes `account_id` and everything cascaded from it.
    async fn delete_account(&self, account_id: AccountId) -> AccountStorageResult<()>;

    /// Overwrites the display name of `account_id`.
    async fn set_name(&self, account_id: AccountId, name: String) -> AccountStorageResult<()>;

    /// Overwrites the description of `account_id`.
    async fn set_description(&self, account_id: AccountId, description: String) -> AccountStorageResult<()>;

    /// Overwrites the cached fiat balance of `account_id`.
    async fn set_balance(&self, account_id: AccountId, balance_usd: BigDecimal) -> AccountStorageResult<()>;

    /// Appends `tickers` to the activated-coin set of `account_id`.
    async fn activate_coins(&self, account_id: AccountId, tickers: Vec<String>) -> AccountStorageResult<()>;

    /// Drops `tickers` from the activated-coin set of `account_id`.
    async fn deactivate_coins(&self, account_id: AccountId, tickers: Vec<String>) -> AccountStorageResult<()>;
}
