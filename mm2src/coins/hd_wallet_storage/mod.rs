use crate::hd_wallet::HDWalletCoinOps;
use async_trait::async_trait;
use common::log::warn;
use crypto::{ChildNumber, CryptoCtx, CryptoCtxError, CryptoInitError, DerivationPath, Secp256k1ExtendedPublicKey, XPub};
use derive_more::Display;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
#[cfg(test)] use mocktopus::macros::*;
use primitives::hash::H160;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fmt::Formatter;
use std::ops::Deref;
use std::str::FromStr;

#[cfg(not(target_arch = "wasm32"))] mod sqlite_storage;
#[cfg(target_arch = "wasm32")] mod wasm_storage;

#[cfg(test)] mod mock_storage;
#[cfg(test)] pub use mock_storage::HDWalletMockStorage;

cfg_wasm32! {
    use wasm_storage::HDWalletIndexedDbStorage as HDWalletStorageInstance;

    pub use wasm_storage::{HDWalletDb, HDWalletDbLocked};
}

cfg_native! {
    use sqlite_storage::HDWalletSqliteStorage as HDWalletStorageInstance;
}

pub type HDWalletStorageResult<T> = MmResult<T, HDWalletStorageError>;
type HDWalletStorageBoxed = Box<dyn HDWalletStorageInternalOps + Send + Sync>;

#[derive(Debug, Display)]
pub enum HDWalletStorageError {
    #[display(fmt = "HD wallet not allowed")]
    HDWalletUnavailable,
    #[display(fmt = "HD account '{:?}':{} not found", wallet_id, account_id)]
    HDAccountNotFound { wallet_id: HDWalletId, account_id: u32 },
    #[display(fmt = "Error saving changes in HD wallet storage: {}", _0)]
    ErrorSaving(String),
    #[display(fmt = "Error loading from HD wallet storage: {}", _0)]
    ErrorLoading(String),
    #[display(fmt = "Error deserializing a swap: {}", _0)]
    ErrorDeserializing(String),
    #[display(fmt = "Error serializing a swap: {}", _0)]
    ErrorSerializing(String),
    #[display(fmt = "Internal error: {}", _0)]
    Internal(String),
}

impl From<CryptoInitError> for HDWalletStorageError {
    fn from(e: CryptoInitError) -> Self { HDWalletStorageError::Internal(e.to_string()) }
}

impl From<CryptoCtxError> for HDWalletStorageError {
    fn from(e: CryptoCtxError) -> Self { HDWalletStorageError::Internal(e.to_string()) }
}

impl HDWalletStorageError {
    pub fn is_deserializing_err(&self) -> bool { matches!(self, HDWalletStorageError::ErrorDeserializing(_)) }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HDWalletId {
    coin: String,
    /// RIPEMD160(SHA256(x)) where x is a pubkey with which mm2 is launched.
    /// It's expected to be equal to [`MmCtx::rmd160`].
    /// This property allows us to store DB items that are unique to each user (passphrase).
    mm2_rmd160: String,
    /// RIPEMD160(SHA256(x)) where x is a pubkey extracted from a Hardware Wallet device or passphrase.
    /// This property allows us to store DB items that are unique to each Hardware Wallet device.
    /// Please note it can be equal to [`HDWalletId::mm2_rmd160`] if mm2 is launched with a HD private key derived from a passphrase.
    hd_wallet_rmd160: String,
}

impl HDWalletId {
    pub fn new(coin: String, mm2_rmd160: &H160, hd_wallet_rmd160: &H160) -> HDWalletId {
        HDWalletId {
            coin,
            mm2_rmd160: display_rmd160(mm2_rmd160),
            hd_wallet_rmd160: display_rmd160(hd_wallet_rmd160),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct HDAccountStorageItem {
    pub account_id: u32,
    pub account_xpub: XPub,
    /// The number of addresses that we know have been used by the user.
    pub external_addresses_number: u32,
    pub internal_addresses_number: u32,
}

#[async_trait]
#[cfg_attr(test, mockable)]
pub trait HDWalletStorageInternalOps {
    async fn init(ctx: &MmArc) -> HDWalletStorageResult<Self>
    where
        Self: Sized;

    async fn load_accounts(&self, wallet_id: HDWalletId) -> HDWalletStorageResult<Vec<HDAccountStorageItem>>;

    async fn load_account(
        &self,
        wallet_id: HDWalletId,
        account_id: u32,
    ) -> HDWalletStorageResult<Option<HDAccountStorageItem>>;

    async fn update_external_addresses_number(
        &self,
        wallet_id: HDWalletId,
        account_id: u32,
        new_external_addresses_number: u32,
    ) -> HDWalletStorageResult<()>;

    async fn update_internal_addresses_number(
        &self,
        wallet_id: HDWalletId,
        account_id: u32,
        new_internal_addresses_number: u32,
    ) -> HDWalletStorageResult<()>;

    async fn upload_new_account(
        &self,
        wallet_id: HDWalletId,
        account: HDAccountStorageItem,
    ) -> HDWalletStorageResult<()>;

    /// Deletes the account `account_id` of the wallet. An account the storage does not hold is not an error.
    async fn remove_account(&self, wallet_id: HDWalletId, account_id: u32) -> HDWalletStorageResult<()>;

    async fn clear_accounts(&self, wallet_id: HDWalletId) -> HDWalletStorageResult<()>;
}

#[async_trait]
pub trait HDWalletCoinWithStorageOps: HDWalletCoinOps {
    fn hd_wallet_storage<'a>(&self, hd_wallet: &'a Self::HDWallet) -> &'a HDWalletCoinStorage;

    async fn load_all_accounts(&self, hd_wallet: &Self::HDWallet) -> HDWalletStorageResult<Vec<HDAccountStorageItem>> {
        let storage = self.hd_wallet_storage(hd_wallet);
        storage.load_all_accounts().await
    }

    async fn load_account(
        &self,
        hd_wallet: &Self::HDWallet,
        account_id: u32,
    ) -> HDWalletStorageResult<Option<HDAccountStorageItem>> {
        let storage = self.hd_wallet_storage(hd_wallet);
        storage.load_account(account_id).await
    }

    async fn update_external_addresses_number(
        &self,
        hd_wallet: &Self::HDWallet,
        account_id: u32,
        new_external_addresses_number: u32,
    ) -> HDWalletStorageResult<()> {
        let storage = self.hd_wallet_storage(hd_wallet);
        storage
            .update_external_addresses_number(account_id, new_external_addresses_number)
            .await
    }

    async fn update_internal_addresses_number(
        &self,
        hd_wallet: &Self::HDWallet,
        account_id: u32,
        new_internal_addresses_number: u32,
    ) -> HDWalletStorageResult<()> {
        let storage = self.hd_wallet_storage(hd_wallet);
        storage
            .update_internal_addresses_number(account_id, new_internal_addresses_number)
            .await
    }

    async fn upload_new_account(
        &self,
        hd_wallet: &Self::HDWallet,
        account_info: HDAccountStorageItem,
    ) -> HDWalletStorageResult<()> {
        let storage = self.hd_wallet_storage(hd_wallet);
        storage.upload_new_account(account_info).await
    }

    async fn remove_account(&self, hd_wallet: &Self::HDWallet, account_id: u32) -> HDWalletStorageResult<()> {
        let storage = self.hd_wallet_storage(hd_wallet);
        storage.remove_account(account_id).await
    }

    async fn clear_accounts(&self, hd_wallet: &Self::HDWallet) -> HDWalletStorageResult<()> {
        let storage = self.hd_wallet_storage(hd_wallet);
        storage.clear_accounts().await
    }
}

/// The wrapper over the [`HDWalletStorage::inner`] database implementation.
/// It's associated with a specific mm2 user, HD wallet and coin.
pub struct HDWalletCoinStorage {
    coin: String,
    /// RIPEMD160(SHA256(x)) where x is a pubkey with which mm2 is launched.
    /// It's expected to be equal to [`MmCtx::rmd160`].
    /// This property allows us to store DB items that are unique to each user (passphrase).
    mm2_rmd160: H160,
    /// RIPEMD160(SHA256(x)) where x is a pubkey extracted from a Hardware Wallet device or passphrase.
    /// This property allows us to store DB items that are unique to each Hardware Wallet device.
    /// Please note it can be equal to [`HDWalletId::mm2_rmd160`] if mm2 is launched with a HD private key derived from a passphrase.
    hd_wallet_rmd160: H160,
    inner: HDWalletStorageBoxed,
}

impl fmt::Debug for HDWalletCoinStorage {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("HDWalletCoinStorage")
            .field("coin", &self.coin)
            .field("mm2_rmd160", &self.mm2_rmd160)
            .field("hd_wallet_rmd160", &self.hd_wallet_rmd160)
            .finish()
    }
}

#[cfg(test)]
impl Default for HDWalletCoinStorage {
    fn default() -> Self {
        HDWalletCoinStorage {
            coin: String::default(),
            mm2_rmd160: H160::default(),
            hd_wallet_rmd160: H160::default(),
            inner: Box::new(HDWalletMockStorage),
        }
    }
}

/// The key policy of the coin whose accounts the storage holds. It names the digest that identifies the wallet in
/// the storage: a node may hold a software global-HD login and a hardware context together, and a coin's accounts
/// belong to the key source of that coin alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HDWalletStorageKeyPolicy {
    /// A coin of the software (context-key, global-HD) policy: the wallet is the software identity.
    Software,
    /// A coin of the Trezor policy: the wallet is the device identity.
    Hardware,
}

impl HDWalletStorageKeyPolicy {
    /// The digest that identifies the wallet of a coin of this policy, chosen from the two the node may hold.
    /// A missing digest is not replaced by the other one.
    fn select_wallet_rmd160(self, software: Option<H160>, hardware: Option<H160>) -> Option<H160> {
        match self {
            HDWalletStorageKeyPolicy::Software => software,
            HDWalletStorageKeyPolicy::Hardware => hardware,
        }
    }
}

impl HDWalletCoinStorage {
    /// Opens the storage of `coin`. The wallet is identified by the digest of the coin's own key policy: the
    /// software global-HD identity for [`HDWalletStorageKeyPolicy::Software`], the device identity for
    /// [`HDWalletStorageKeyPolicy::Hardware`] (CRD ch.05 R29). There is no fallback to the other digest.
    ///
    /// # Errors
    ///
    /// [`HDWalletStorageError::HDWalletUnavailable`] if the digest of the policy does not exist: no software
    /// global-HD login (Iguana mode), or no hardware context.
    pub async fn init(
        ctx: &MmArc,
        coin: String,
        key_policy: HDWalletStorageKeyPolicy,
    ) -> HDWalletStorageResult<HDWalletCoinStorage> {
        let crypto_ctx = CryptoCtx::from_ctx(ctx).mm_err(Into::into)?;
        Self::init_with_identities(
            ctx,
            coin,
            key_policy,
            crypto_ctx.global_hd_wallet_rmd160(),
            crypto_ctx.hw_wallet_rmd160(),
        )
        .await
    }

    /// [`HDWalletCoinStorage::init`] over the two digests the node holds.
    async fn init_with_identities(
        ctx: &MmArc,
        coin: String,
        key_policy: HDWalletStorageKeyPolicy,
        software_rmd160: Option<H160>,
        hardware_rmd160: Option<H160>,
    ) -> HDWalletStorageResult<HDWalletCoinStorage> {
        let hd_wallet_rmd160 = key_policy
            .select_wallet_rmd160(software_rmd160, hardware_rmd160)
            .or_mm_err(|| HDWalletStorageError::HDWalletUnavailable)?;
        let inner = Box::new(HDWalletStorageInstance::init(ctx).await?);
        Ok(HDWalletCoinStorage {
            coin,
            mm2_rmd160: *ctx.rmd160(),
            hd_wallet_rmd160,
            inner,
        })
    }

    #[cfg(any(test, target_arch = "wasm32"))]
    pub async fn init_with_rmd160(
        ctx: &MmArc,
        coin: String,
        mm2_rmd160: H160,
        hd_wallet_rmd160: H160,
    ) -> HDWalletStorageResult<HDWalletCoinStorage> {
        let inner = Box::new(HDWalletStorageInstance::init(ctx).await?);
        Ok(HDWalletCoinStorage {
            coin,
            mm2_rmd160,
            hd_wallet_rmd160,
            inner,
        })
    }

    /// Loads the stored accounts of a coin of the software policy, and removes the rows that the wallet's seed
    /// did not derive (CRD ch.05 R55 (1)): a row whose `account_xpub` differs from the extended public key the
    /// seed derives for that `account_id` below `coin_path` (an older build stored the accounts of a Trezor-policy
    /// coin under the seed's identity). Such a row is deleted, so that the account created for its index later is
    /// not a second row, and is not loaded. One WARN per load names the ticker and the removed account ids; it
    /// carries no key, address or host. A row whose `account_xpub` cannot be read is left to the loader.
    ///
    /// A coin of the Trezor policy does not come here: its keys need the device.
    ///
    /// # Errors
    ///
    /// Any storage failure, a failed derivation or a failed removal fails the load; no unchecked row is returned.
    pub async fn load_accounts_derived_from_seed(
        &self,
        master: &bip32::ExtendedPrivateKey<secp256k1::SecretKey>,
        coin_path: &DerivationPath,
    ) -> HDWalletStorageResult<Vec<HDAccountStorageItem>> {
        let stored = self.load_all_accounts().await?;
        let mut accounts = Vec::with_capacity(stored.len());
        let mut foreign_account_ids = Vec::new();
        for item in stored {
            if is_account_of_the_seed(master, coin_path, &item)? {
                accounts.push(item);
            } else {
                foreign_account_ids.push(item.account_id);
            }
        }
        for account_id in &foreign_account_ids {
            self.remove_account(*account_id).await?;
        }
        if !foreign_account_ids.is_empty() {
            warn!(
                "{}: removed {} stored HD account(s) that the wallet's seed did not derive, account ids {:?}",
                self.coin,
                foreign_account_ids.len(),
                foreign_account_ids
            );
        }
        Ok(accounts)
    }

    pub fn wallet_id(&self) -> HDWalletId {
        HDWalletId::new(self.coin.clone(), &self.mm2_rmd160, &self.hd_wallet_rmd160)
    }

    pub async fn load_all_accounts(&self) -> HDWalletStorageResult<Vec<HDAccountStorageItem>> {
        let wallet_id = self.wallet_id();
        self.inner.load_accounts(wallet_id).await
    }

    async fn load_account(&self, account_id: u32) -> HDWalletStorageResult<Option<HDAccountStorageItem>> {
        let wallet_id = self.wallet_id();
        self.inner.load_account(wallet_id, account_id).await
    }

    async fn update_external_addresses_number(
        &self,
        account_id: u32,
        new_external_addresses_number: u32,
    ) -> HDWalletStorageResult<()> {
        let wallet_id = self.wallet_id();
        self.inner
            .update_external_addresses_number(wallet_id, account_id, new_external_addresses_number)
            .await
    }

    async fn update_internal_addresses_number(
        &self,
        account_id: u32,
        new_internal_addresses_number: u32,
    ) -> HDWalletStorageResult<()> {
        let wallet_id = self.wallet_id();
        self.inner
            .update_internal_addresses_number(wallet_id, account_id, new_internal_addresses_number)
            .await
    }

    async fn upload_new_account(&self, account_info: HDAccountStorageItem) -> HDWalletStorageResult<()> {
        let wallet_id = self.wallet_id();
        self.inner.upload_new_account(wallet_id, account_info).await
    }

    async fn remove_account(&self, account_id: u32) -> HDWalletStorageResult<()> {
        let wallet_id = self.wallet_id();
        self.inner.remove_account(wallet_id, account_id).await
    }

    pub async fn clear_accounts(&self) -> HDWalletStorageResult<()> {
        let wallet_id = self.wallet_id();
        self.inner.clear_accounts(wallet_id).await
    }
}

/// Whether the stored row holds the extended public key the seed derives for its account index (CRD ch.05 R55).
/// The expected key is the one the software derivation of R30 gives: the coin's derivation path plus the hardened
/// account index.
fn is_account_of_the_seed(
    master: &bip32::ExtendedPrivateKey<secp256k1::SecretKey>,
    coin_path: &DerivationPath,
    item: &HDAccountStorageItem,
) -> HDWalletStorageResult<bool> {
    let stored = match Secp256k1ExtendedPublicKey::from_str(&item.account_xpub) {
        Ok(stored) => stored,
        // An unreadable row is the loader's own case (it clears the accounts), not a foreign key.
        Err(_) => return Ok(true),
    };
    let account_child = ChildNumber::new(item.account_id, true)
        .map_to_mm(|e| HDWalletStorageError::Internal(format!("Invalid stored account index: {}", e)))?;
    let mut account_path = coin_path.clone();
    account_path.push(account_child);
    let derived = crypto::derive_secp256k1_extended_pubkey(master.clone(), &account_path)
        .mm_err(|e| HDWalletStorageError::Internal(format!("BIP32 derivation error: {}", e)))?;
    Ok(derived.to_string(bip32::Prefix::XPUB) == stored.to_string(bip32::Prefix::XPUB))
}

fn display_rmd160(rmd160: &H160) -> String { hex::encode(rmd160.deref()) }

#[cfg(any(test, target_arch = "wasm32"))]
mod tests {
    use super::*;
    use itertools::Itertools;
    use mm2_core::mm_ctx::MmCtxBuilder;
    use primitives::hash::H160;

    cfg_wasm32! {
        use crate::hd_wallet_storage::wasm_storage::get_all_storage_items;
        use wasm_bindgen_test::*;

        wasm_bindgen_test_configure!(run_in_browser);
    }

    cfg_native! {
        use crate::hd_wallet_storage::sqlite_storage::get_all_storage_items;
        use common::block_on;
        use db_common::sqlite::rusqlite::Connection;
        use std::sync::{Arc, Mutex};
    }

    #[cfg(target_arch = "wasm32")]
    fn mm_ctx_with_custom_db() -> MmArc { MmCtxBuilder::new().with_test_db_namespace().into_mm_arc() }

    #[cfg(not(target_arch = "wasm32"))]
    fn mm_ctx_with_custom_db() -> MmArc {
        let ctx = MmCtxBuilder::new().into_mm_arc();
        let connection = Connection::open_in_memory().unwrap();
        let _ = ctx.sqlite_connection.pin(Arc::new(Mutex::new(connection)));
        ctx
    }

    async fn test_unique_wallets_impl() {
        let doc_user0_device0_account0 = HDAccountStorageItem {
            account_id: 0,
            account_xpub: "xpub6DEHSksajpRPM59RPw7Eg6PKdU7E2ehxJWtYdrfQ6JFmMGBsrR6jA78ANCLgzKYm4s5UqQ4ydLEYPbh3TRVvn5oAZVtWfi4qJLMntpZ8uGJ".to_owned(),
            external_addresses_number: 1,
            internal_addresses_number: 2,
        };
        let doc_user0_device0_account1 = HDAccountStorageItem {
            account_id: 1,
            account_xpub: "xpub6DEHSksajpRPQq2FdGT6JoieiQZUpTZ3WZn8fcuLJhFVmtCpXbuXxp5aPzaokwcLV2V9LE55Dwt8JYkpuMv7jXKwmyD28WbHYjBH2zhbW2p".to_owned(),
            external_addresses_number: 1,
            internal_addresses_number: 2,
        };
        let doc_user0_device1_account0 = HDAccountStorageItem {
            account_id: 0,
            account_xpub: "xpub6EuV33a2DXxAhoJTRTnr8qnysu81AA4YHpLY6o8NiGkEJ8KADJ35T64eJsStWsmRf1xXkEANVjXFXnaUKbRtFwuSPCLfDdZwYNZToh4LBCd".to_owned(),
            external_addresses_number: 3,
            internal_addresses_number: 4,
        };
        let doc_user1_device0_account0 = HDAccountStorageItem {
            account_id: 0,
            account_xpub: "xpub6CUGRUonZSQ4TWtTMmzXdrXDtypWKiKrhko4egpiMZbpiaQL2jkwSB1icqYh2cfDfVxdx4df189oLKnC5fSwqPfgyP3hooxujYzAu3fDVmz".to_owned(),
            external_addresses_number: 5,
            internal_addresses_number: 6,
        };
        let marty_user0_device0_account0 = HDAccountStorageItem {
            account_id: 0,
            account_xpub: "xpub6AHA9hZDN11k2ijHMeS5QqHx2KP9aMBRhTDqANMnwVtdyw2TDYRmF8PjpvwUFcL1Et8Hj59S3gTSMcUQ5gAqTz3Wd8EsMTmF3DChhqPQBnU".to_owned(),
            external_addresses_number: 7,
            internal_addresses_number: 8,
        };

        let ctx = mm_ctx_with_custom_db();
        let user0_rmd160 = H160::from("0000000000000000000000000000000000000000");
        let user1_rmd160 = H160::from("0000000000000000000000000000000000000001");
        let device0_rmd160 = H160::from("0000000000000000000000000000000000000020");
        let device1_rmd160 = H160::from("0000000000000000000000000000000000000030");

        let doc_user0_device0_db =
            HDWalletCoinStorage::init_with_rmd160(&ctx, "DOC".to_owned(), user0_rmd160, device0_rmd160)
                .await
                .expect("!HDWalletCoinStorage::new");
        let doc_user0_device1_db =
            HDWalletCoinStorage::init_with_rmd160(&ctx, "DOC".to_owned(), user0_rmd160, device1_rmd160)
                .await
                .expect("!HDWalletCoinStorage::new");
        let doc_user1_device0_db =
            HDWalletCoinStorage::init_with_rmd160(&ctx, "DOC".to_owned(), user1_rmd160, device0_rmd160)
                .await
                .expect("!HDWalletCoinStorage::new");
        let marty_user0_device0_db =
            HDWalletCoinStorage::init_with_rmd160(&ctx, "MARTY".to_owned(), user0_rmd160, device0_rmd160)
                .await
                .expect("!HDWalletCoinStorage::new");

        doc_user0_device0_db
            .upload_new_account(doc_user0_device0_account0.clone())
            .await
            .expect("!HDWalletCoinStorage::upload_new_account: DOC user=0 device=0 account=0");
        doc_user0_device0_db
            .upload_new_account(doc_user0_device0_account1.clone())
            .await
            .expect("!HDWalletCoinStorage::upload_new_account: DOC user=0 device=0 account=1");
        doc_user0_device1_db
            .upload_new_account(doc_user0_device1_account0.clone())
            .await
            .expect("!HDWalletCoinStorage::upload_new_account: DOC user=0 device=1 account=0");
        doc_user1_device0_db
            .upload_new_account(doc_user1_device0_account0.clone())
            .await
            .expect("!HDWalletCoinStorage::upload_new_account: DOC user=1 device=0 account=0");
        marty_user0_device0_db
            .upload_new_account(marty_user0_device0_account0.clone())
            .await
            .expect("!HDWalletCoinStorage::upload_new_account: MARTY user=0 device=0 account=0");

        // All accounts must be in the only one database.
        // Rows in the database must differ by only `coin`, `mm2_rmd160`, `hd_wallet_rmd160` and `account_id` values.
        let all_accounts: Vec<_> = get_all_storage_items(&ctx)
            .await
            .into_iter()
            .sorted_by(|x, y| x.external_addresses_number.cmp(&y.external_addresses_number))
            .collect();
        assert_eq!(all_accounts, vec![
            doc_user0_device0_account0.clone(),
            doc_user0_device0_account1.clone(),
            doc_user0_device1_account0.clone(),
            doc_user1_device0_account0.clone(),
            marty_user0_device0_account0.clone()
        ]);

        let mut actual = doc_user0_device0_db
            .load_all_accounts()
            .await
            .expect("HDWalletCoinStorage::load_all_accounts: DOC user=0 device=0");
        actual.sort_by(|x, y| x.account_id.cmp(&y.account_id));
        assert_eq!(actual, vec![doc_user0_device0_account0, doc_user0_device0_account1]);

        let actual = doc_user0_device1_db
            .load_all_accounts()
            .await
            .expect("HDWalletCoinStorage::load_all_accounts: DOC user=0 device=1");
        assert_eq!(actual, vec![doc_user0_device1_account0]);

        let actual = doc_user1_device0_db
            .load_all_accounts()
            .await
            .expect("HDWalletCoinStorage::load_all_accounts: DOC user=1 device=0");
        assert_eq!(actual, vec![doc_user1_device0_account0]);

        let actual = marty_user0_device0_db
            .load_all_accounts()
            .await
            .expect("HDWalletCoinStorage::load_all_accounts: MARTY user=0 device=0");
        assert_eq!(actual, vec![marty_user0_device0_account0]);
    }

    async fn test_delete_accounts_impl() {
        let wallet0_account0 = HDAccountStorageItem {
            account_id: 0,
            account_xpub: "xpub6DEHSksajpRPM59RPw7Eg6PKdU7E2ehxJWtYdrfQ6JFmMGBsrR6jA78ANCLgzKYm4s5UqQ4ydLEYPbh3TRVvn5oAZVtWfi4qJLMntpZ8uGJ".to_owned(),
            external_addresses_number: 1,
            internal_addresses_number: 2,
        };
        let wallet0_account1 = HDAccountStorageItem {
            account_id: 1,
            account_xpub: "xpub6DEHSksajpRPQq2FdGT6JoieiQZUpTZ3WZn8fcuLJhFVmtCpXbuXxp5aPzaokwcLV2V9LE55Dwt8JYkpuMv7jXKwmyD28WbHYjBH2zhbW2p".to_owned(),
            external_addresses_number: 1,
            internal_addresses_number: 2,
        };
        let wallet1_account0 = HDAccountStorageItem {
            account_id: 0,
            account_xpub: "xpub6EuV33a2DXxAhoJTRTnr8qnysu81AA4YHpLY6o8NiGkEJ8KADJ35T64eJsStWsmRf1xXkEANVjXFXnaUKbRtFwuSPCLfDdZwYNZToh4LBCd".to_owned(),
            external_addresses_number: 3,
            internal_addresses_number: 4,
        };
        let wallet2_account0 = HDAccountStorageItem {
            account_id: 0,
            account_xpub: "xpub6CUGRUonZSQ4TWtTMmzXdrXDtypWKiKrhko4egpiMZbpiaQL2jkwSB1icqYh2cfDfVxdx4df189oLKnC5fSwqPfgyP3hooxujYzAu3fDVmz".to_owned(),
            external_addresses_number: 5,
            internal_addresses_number: 6,
        };

        let ctx = mm_ctx_with_custom_db();
        let user_rmd160 = H160::from("0000000000000000000000000000000000000000");
        let device0_rmd160 = H160::from("0000000000000000000000000000000000000010");
        let device1_rmd160 = H160::from("0000000000000000000000000000000000000020");
        let device2_rmd160 = H160::from("0000000000000000000000000000000000000030");

        let wallet0_db = HDWalletCoinStorage::init_with_rmd160(&ctx, "DOC".to_owned(), user_rmd160, device0_rmd160)
            .await
            .expect("!HDWalletCoinStorage::new");
        let wallet1_db = HDWalletCoinStorage::init_with_rmd160(&ctx, "DOC".to_owned(), user_rmd160, device1_rmd160)
            .await
            .expect("!HDWalletCoinStorage::new");
        let wallet2_db = HDWalletCoinStorage::init_with_rmd160(&ctx, "DOC".to_owned(), user_rmd160, device2_rmd160)
            .await
            .expect("!HDWalletCoinStorage::new");

        wallet0_db
            .upload_new_account(wallet0_account0.clone())
            .await
            .expect("!HDWalletCoinStorage::upload_new_account: DOC wallet=0 account=0");
        wallet0_db
            .upload_new_account(wallet0_account1.clone())
            .await
            .expect("!HDWalletCoinStorage::upload_new_account: DOC wallet=0 account=1");
        wallet1_db
            .upload_new_account(wallet1_account0.clone())
            .await
            .expect("!HDWalletCoinStorage::upload_new_account: DOC wallet=1 account=0");
        wallet2_db
            .upload_new_account(wallet2_account0.clone())
            .await
            .expect("!HDWalletCoinStorage::upload_new_account: DOC wallet=2 account=0");

        wallet0_db
            .clear_accounts()
            .await
            .expect("HDWalletCoinStorage::clear_accounts: DOC wallet=0");

        // All accounts must be in the only one database.
        // Rows in the database must differ by only `coin`, `mm2_rmd160`, `hd_wallet_rmd160` and `account_id` values.
        let all_accounts: Vec<_> = get_all_storage_items(&ctx)
            .await
            .into_iter()
            .sorted_by(|x, y| x.external_addresses_number.cmp(&y.external_addresses_number))
            .collect();
        assert_eq!(all_accounts, vec![wallet1_account0, wallet2_account0]);
    }

    async fn test_update_account_impl() {
        let mut account0 = HDAccountStorageItem {
            account_id: 0,
            account_xpub: "xpub6DEHSksajpRPM59RPw7Eg6PKdU7E2ehxJWtYdrfQ6JFmMGBsrR6jA78ANCLgzKYm4s5UqQ4ydLEYPbh3TRVvn5oAZVtWfi4qJLMntpZ8uGJ".to_owned(),
            external_addresses_number: 1,
            internal_addresses_number: 2,
        };
        let mut account1 = HDAccountStorageItem {
            account_id: 1,
            account_xpub: "xpub6DEHSksajpRPQq2FdGT6JoieiQZUpTZ3WZn8fcuLJhFVmtCpXbuXxp5aPzaokwcLV2V9LE55Dwt8JYkpuMv7jXKwmyD28WbHYjBH2zhbW2p".to_owned(),
            external_addresses_number: 3,
            internal_addresses_number: 4,
        };

        let ctx = mm_ctx_with_custom_db();
        let user_rmd160 = H160::from("0000000000000000000000000000000000000000");
        let device_rmd160 = H160::from("0000000000000000000000000000000000000010");

        let db = HDWalletCoinStorage::init_with_rmd160(&ctx, "DOC".to_owned(), user_rmd160, device_rmd160)
            .await
            .expect("!HDWalletCoinStorage::new");

        db.upload_new_account(account0.clone())
            .await
            .expect("!HDWalletCoinStorage::upload_new_account: DOC wallet=0 account=0");
        db.upload_new_account(account1.clone())
            .await
            .expect("!HDWalletCoinStorage::upload_new_account: DOC wallet=0 account=1");

        db.update_internal_addresses_number(0, 5)
            .await
            .expect("!HDWalletCoinStorage::update_internal_addresses_number");
        db.update_external_addresses_number(1, 10)
            .await
            .expect("!HDWalletCoinStorage::update_external_addresses_number");

        let actual: Vec<_> = db
            .load_all_accounts()
            .await
            .expect("!HDWalletCoinStorage::load_all_accounts")
            .into_iter()
            .sorted_by(|x, y| x.external_addresses_number.cmp(&y.external_addresses_number))
            .collect();

        account0.internal_addresses_number = 5;
        account1.external_addresses_number = 10;
        assert_eq!(actual, vec![account0, account1]);
    }

    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen_test]
    async fn test_unique_wallets() { test_unique_wallets_impl().await }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn test_unique_wallets() { block_on(test_unique_wallets_impl()) }

    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen_test]
    async fn test_delete_accounts() { test_delete_accounts_impl().await }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn test_delete_accounts() { block_on(test_delete_accounts_impl()) }

    /// CRD ch.05 R35c item 4: removing an account deletes that account of that wallet only, and an account the
    /// storage does not hold is not an error.
    async fn test_remove_account_impl() {
        let account = |account_id: u32, xpub: &str| HDAccountStorageItem {
            account_id,
            account_xpub: xpub.to_owned(),
            external_addresses_number: 1,
            internal_addresses_number: 2,
        };
        let wallet0_account0 = account(0, "xpub6DEHSksajpRPM59RPw7Eg6PKdU7E2ehxJWtYdrfQ6JFmMGBsrR6jA78ANCLgzKYm4s5UqQ4ydLEYPbh3TRVvn5oAZVtWfi4qJLMntpZ8uGJ");
        let wallet0_account1 = account(1, "xpub6DEHSksajpRPQq2FdGT6JoieiQZUpTZ3WZn8fcuLJhFVmtCpXbuXxp5aPzaokwcLV2V9LE55Dwt8JYkpuMv7jXKwmyD28WbHYjBH2zhbW2p");
        let wallet1_account1 = account(1, "xpub6EuV33a2DXxAhoJTRTnr8qnysu81AA4YHpLY6o8NiGkEJ8KADJ35T64eJsStWsmRf1xXkEANVjXFXnaUKbRtFwuSPCLfDdZwYNZToh4LBCd");

        let ctx = mm_ctx_with_custom_db();
        let user_rmd160 = H160::from("0000000000000000000000000000000000000000");
        let device0_rmd160 = H160::from("0000000000000000000000000000000000000010");
        let device1_rmd160 = H160::from("0000000000000000000000000000000000000020");
        let wallet0_db = HDWalletCoinStorage::init_with_rmd160(&ctx, "DOC".to_owned(), user_rmd160, device0_rmd160)
            .await
            .expect("!HDWalletCoinStorage::new");
        let wallet1_db = HDWalletCoinStorage::init_with_rmd160(&ctx, "DOC".to_owned(), user_rmd160, device1_rmd160)
            .await
            .expect("!HDWalletCoinStorage::new");
        for (db, item) in [
            (&wallet0_db, &wallet0_account0),
            (&wallet0_db, &wallet0_account1),
            (&wallet1_db, &wallet1_account1),
        ] {
            db.upload_new_account(item.clone())
                .await
                .expect("!HDWalletCoinStorage::upload_new_account");
        }

        wallet0_db
            .remove_account(1)
            .await
            .expect("!HDWalletCoinStorage::remove_account");
        wallet0_db
            .remove_account(7)
            .await
            .expect("an account the storage does not hold is not an error");

        let actual = wallet0_db.load_all_accounts().await.expect("!load_all_accounts");
        assert_eq!(actual, vec![wallet0_account0]);
        let actual = wallet1_db.load_all_accounts().await.expect("!load_all_accounts");
        assert_eq!(actual, vec![wallet1_account1], "another wallet's account 1 is kept");
    }

    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen_test]
    async fn test_update_account() { test_update_account_impl().await }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn test_update_account() { block_on(test_update_account_impl()) }

    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen_test]
    async fn test_remove_account() { test_remove_account_impl().await }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn test_remove_account() { block_on(test_remove_account_impl()) }

    const SOFTWARE_XPUB: &str = "xpub6DEHSksajpRPM59RPw7Eg6PKdU7E2ehxJWtYdrfQ6JFmMGBsrR6jA78ANCLgzKYm4s5UqQ4ydLEYPbh3TRVvn5oAZVtWfi4qJLMntpZ8uGJ";
    const DEVICE_XPUB: &str = "xpub6EuV33a2DXxAhoJTRTnr8qnysu81AA4YHpLY6o8NiGkEJ8KADJ35T64eJsStWsmRf1xXkEANVjXFXnaUKbRtFwuSPCLfDdZwYNZToh4LBCd";

    fn account_with_xpub(xpub: &str) -> HDAccountStorageItem {
        HDAccountStorageItem {
            account_id: 0,
            account_xpub: xpub.to_owned(),
            external_addresses_number: 1,
            internal_addresses_number: 0,
        }
    }

    /// CRD ch.05 R29, mixed mode: a node holds a software global-HD login and a hardware context together. The
    /// accounts of a Trezor-policy coin are stored under the DEVICE digest, those of a software coin under the
    /// software digest, and neither policy loads the other's accounts.
    async fn test_wallet_identity_follows_the_coin_key_policy_impl() {
        let ctx = mm_ctx_with_custom_db();
        let software_rmd160 = H160::from("0000000000000000000000000000000000000001");
        let device_rmd160 = H160::from("0000000000000000000000000000000000000002");
        let open = |policy: HDWalletStorageKeyPolicy| {
            HDWalletCoinStorage::init_with_identities(
                &ctx,
                "DOC".to_owned(),
                policy,
                Some(software_rmd160),
                Some(device_rmd160),
            )
        };

        // A Trezor-policy activation stores its account under the device digest.
        let trezor_db = open(HDWalletStorageKeyPolicy::Hardware)
            .await
            .expect("!init: Trezor policy");
        assert_eq!(trezor_db.hd_wallet_rmd160, device_rmd160);
        trezor_db
            .upload_new_account(account_with_xpub(DEVICE_XPUB))
            .await
            .expect("!upload_new_account: Trezor policy");

        // A later software activation of the same ticker loads none of those accounts.
        let software_db = open(HDWalletStorageKeyPolicy::Software)
            .await
            .expect("!init: software policy");
        assert_eq!(software_db.hd_wallet_rmd160, software_rmd160);
        assert!(software_db
            .load_all_accounts()
            .await
            .expect("!load_all_accounts")
            .is_empty());
        assert!(software_db.load_account(0).await.expect("!load_account").is_none());

        // The reverse: a software coin's accounts are not loaded by a Trezor-policy activation.
        software_db
            .upload_new_account(account_with_xpub(SOFTWARE_XPUB))
            .await
            .expect("!upload_new_account: software policy");
        let actual = open(HDWalletStorageKeyPolicy::Hardware)
            .await
            .expect("!init: Trezor policy")
            .load_all_accounts()
            .await
            .expect("!load_all_accounts");
        assert_eq!(actual, vec![account_with_xpub(DEVICE_XPUB)]);
        let actual = open(HDWalletStorageKeyPolicy::Software)
            .await
            .expect("!init: software policy")
            .load_all_accounts()
            .await
            .expect("!load_all_accounts");
        assert_eq!(actual, vec![account_with_xpub(SOFTWARE_XPUB)]);
    }

    /// CRD ch.05 R29: the digest of the coin's policy is the only one used. A missing digest is not replaced by the
    /// other one: the storage is unavailable.
    async fn test_wallet_identity_has_no_fallback_impl() {
        let ctx = mm_ctx_with_custom_db();
        let some_rmd160 = H160::from("0000000000000000000000000000000000000003");
        let open = |policy: HDWalletStorageKeyPolicy, software: Option<H160>, hardware: Option<H160>| {
            HDWalletCoinStorage::init_with_identities(&ctx, "DOC".to_owned(), policy, software, hardware)
        };

        // A Trezor-policy coin without a hardware context, under a software login.
        let error = open(HDWalletStorageKeyPolicy::Hardware, Some(some_rmd160), None)
            .await
            .expect_err("a Trezor-policy coin needs the device digest");
        assert!(matches!(error.into_inner(), HDWalletStorageError::HDWalletUnavailable));

        // A software coin without a software global-HD login (Iguana), beside a hardware context.
        let error = open(HDWalletStorageKeyPolicy::Software, None, Some(some_rmd160))
            .await
            .expect_err("a software coin needs the software digest");
        assert!(matches!(error.into_inner(), HDWalletStorageError::HDWalletUnavailable));
    }

    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen_test]
    async fn test_wallet_identity_follows_the_coin_key_policy() {
        test_wallet_identity_follows_the_coin_key_policy_impl().await
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn test_wallet_identity_follows_the_coin_key_policy() {
        block_on(test_wallet_identity_follows_the_coin_key_policy_impl())
    }

    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen_test]
    async fn test_wallet_identity_has_no_fallback() { test_wallet_identity_has_no_fallback_impl().await }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn test_wallet_identity_has_no_fallback() { block_on(test_wallet_identity_has_no_fallback_impl()) }

    const FOREIGN_MNEMONIC: &str = "legal winner thank year wave sausage worth useful legal winner thank yellow";
    const SEED_MNEMONIC: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    fn master_of(mnemonic: &str) -> bip32::ExtendedPrivateKey<secp256k1::SecretKey> {
        use crypto::KeyPairPolicy;

        let ctx = MmCtxBuilder::new().into_mm_arc();
        let crypto_ctx = CryptoCtx::init_with_global_hd_account(ctx, mnemonic).unwrap();
        match crypto_ctx.key_pair_policy() {
            KeyPairPolicy::GlobalHDAccount(global_hd) => global_hd.root_priv_key().clone(),
            KeyPairPolicy::Iguana => panic!("expected a global-HD login"),
        }
    }

    /// The extended public key the software derivation gives at `path`, as it is stored.
    fn xpub_at(master: &bip32::ExtendedPrivateKey<secp256k1::SecretKey>, path: &str) -> String {
        crypto::derive_secp256k1_extended_pubkey(master.clone(), &DerivationPath::from_str(path).unwrap())
            .unwrap()
            .to_string(bip32::Prefix::XPUB)
    }

    fn row(account_id: u32, xpub: String, external_addresses_number: u32) -> HDAccountStorageItem {
        HDAccountStorageItem {
            account_id,
            account_xpub: xpub,
            external_addresses_number,
            internal_addresses_number: 0,
        }
    }

    async fn stored_rows(db: &HDWalletCoinStorage) -> Vec<HDAccountStorageItem> {
        let mut rows = db.load_all_accounts().await.expect("!load_all_accounts");
        rows.sort_by_key(|row| row.account_id);
        rows
    }

    async fn software_db(ctx: &MmArc) -> HDWalletCoinStorage {
        HDWalletCoinStorage::init_with_identities(
            ctx,
            "DOC".to_owned(),
            HDWalletStorageKeyPolicy::Software,
            Some(H160::from("0000000000000000000000000000000000000001")),
            None,
        )
        .await
        .expect("!init")
    }

    /// CRD ch.05 R55 (1): a software activation does not load a row the seed did not derive. The row is removed,
    /// the account created for its index later is the only row of that index, and a second load changes nothing.
    async fn test_r55_a_foreign_row_is_removed_and_not_loaded_impl() {
        let master = master_of(SEED_MNEMONIC);
        let foreign_master = master_of(FOREIGN_MNEMONIC);
        let coin_path = DerivationPath::from_str("m/44'/141'").unwrap();
        let own_account_1 = row(1, xpub_at(&master, "m/44'/141'/1'"), 2);
        let foreign_account_0 = row(0, xpub_at(&foreign_master, "m/44'/141'/0'"), 3);

        let ctx = mm_ctx_with_custom_db();
        let db = software_db(&ctx).await;
        db.upload_new_account(foreign_account_0).await.expect("!upload");
        db.upload_new_account(own_account_1.clone()).await.expect("!upload");

        // Only the seed's account is loaded; the foreign row is removed.
        let loaded = db
            .load_accounts_derived_from_seed(&master, &coin_path)
            .await
            .expect("!load_accounts_derived_from_seed");
        assert_eq!(loaded, vec![own_account_1.clone()]);
        assert_eq!(stored_rows(&db).await, vec![own_account_1.clone()]);

        // The activation creates account 0 from the seed: exactly one row with account_id 0 remains.
        let seed_account_0 = row(0, xpub_at(&master, "m/44'/141'/0'"), 0);
        db.upload_new_account(seed_account_0.clone()).await.expect("!upload");
        let rows = stored_rows(&db).await;
        assert_eq!(rows, vec![seed_account_0.clone(), own_account_1.clone()]);
        assert_eq!(rows.iter().filter(|row| row.account_id == 0).count(), 1);

        // A second activation loads both, and removes nothing.
        let mut loaded = db
            .load_accounts_derived_from_seed(&master, &coin_path)
            .await
            .expect("!load_accounts_derived_from_seed");
        loaded.sort_by_key(|row| row.account_id);
        assert_eq!(loaded, vec![seed_account_0.clone(), own_account_1.clone()]);
        assert_eq!(stored_rows(&db).await, vec![seed_account_0, own_account_1]);
    }

    /// CRD ch.05 R55 (1): where every stored row is the seed's, nothing is removed.
    async fn test_r55_matching_rows_are_all_loaded_impl() {
        let master = master_of(SEED_MNEMONIC);
        let coin_path = DerivationPath::from_str("m/44'/141'").unwrap();
        let rows: Vec<_> = (0..3)
            .map(|id| row(id, xpub_at(&master, &format!("m/44'/141'/{}'", id)), id + 1))
            .collect();

        let ctx = mm_ctx_with_custom_db();
        let db = software_db(&ctx).await;
        for item in &rows {
            db.upload_new_account(item.clone()).await.expect("!upload");
        }

        let mut loaded = db
            .load_accounts_derived_from_seed(&master, &coin_path)
            .await
            .expect("!load_accounts_derived_from_seed");
        loaded.sort_by_key(|row| row.account_id);
        assert_eq!(loaded, rows);
        assert_eq!(stored_rows(&db).await, rows);
    }

    /// CRD ch.05 R55 (1) through the UTXO loader: a coin of the software policy (the seed's master given) drops
    /// the foreign row; a coin of the Trezor policy (no master) loads its rows as stored, unchecked.
    async fn test_r55_the_utxo_loader_checks_software_coins_only_impl() {
        use crate::utxo::utxo_common::load_hd_accounts_from_storage;

        let master = master_of(SEED_MNEMONIC);
        let foreign_master = master_of(FOREIGN_MNEMONIC);
        let coin_path = crypto::HDPathToCoin::from_str("m/44'/141'").unwrap();

        let ctx = mm_ctx_with_custom_db();
        let db = software_db(&ctx).await;
        db.upload_new_account(row(0, xpub_at(&foreign_master, "m/44'/141'/0'"), 3))
            .await
            .expect("!upload");
        db.upload_new_account(row(1, xpub_at(&master, "m/44'/141'/1'"), 2))
            .await
            .expect("!upload");

        // The Trezor-policy coin: unchecked, both rows are loaded and kept.
        let unchecked = load_hd_accounts_from_storage(&db, &coin_path, None)
            .await
            .expect("!load: Trezor policy");
        assert_eq!(unchecked.keys().cloned().collect::<Vec<_>>(), vec![0, 1]);
        assert_eq!(stored_rows(&db).await.len(), 2);

        // The software coin: the foreign account 0 is not loaded and is removed.
        let checked = load_hd_accounts_from_storage(&db, &coin_path, Some(&master))
            .await
            .expect("!load: software policy");
        assert_eq!(checked.keys().cloned().collect::<Vec<_>>(), vec![1]);
        assert_eq!(
            stored_rows(&db)
                .await
                .iter()
                .map(|row| row.account_id)
                .collect::<Vec<_>>(),
            vec![1]
        );
    }

    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen_test]
    async fn test_r55_a_foreign_row_is_removed_and_not_loaded() {
        test_r55_a_foreign_row_is_removed_and_not_loaded_impl().await
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn test_r55_a_foreign_row_is_removed_and_not_loaded() {
        block_on(test_r55_a_foreign_row_is_removed_and_not_loaded_impl())
    }

    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen_test]
    async fn test_r55_matching_rows_are_all_loaded() { test_r55_matching_rows_are_all_loaded_impl().await }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn test_r55_matching_rows_are_all_loaded() { block_on(test_r55_matching_rows_are_all_loaded_impl()) }

    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen_test]
    async fn test_r55_the_utxo_loader_checks_software_coins_only() {
        test_r55_the_utxo_loader_checks_software_coins_only_impl().await
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn test_r55_the_utxo_loader_checks_software_coins_only() {
        block_on(test_r55_the_utxo_loader_checks_software_coins_only_impl())
    }
}
