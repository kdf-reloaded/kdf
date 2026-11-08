use crate::hd_pubkey::HDXPubExtractor;
use crate::hd_wallet::{HDAddressId, HDWalletCoinOps, NewAccountCreatingError, NewAccountIndex};
use crate::{BalanceError, BalanceResult, CoinBalance, CoinWithDerivationMethod, DerivationMethod, HDAddress,
            MarketCoinOps};
use async_trait::async_trait;
use common::custom_iter::TryUnzip;
use common::log::{debug, info};
use crypto::{Bip44Chain, RpcDerivationPath};
use derive_more::Display;
use futures::compat::Future01CompatExt;
use mm2_err_handle::prelude::*;
use std::fmt;
use std::ops::Range;

pub type AddressIdRange = Range<u32>;

/// A ticker-keyed map of balances, e.g. `{ "RIN": { "spendable": "..", "unspendable": ".." } }`.
/// Used for HD/v2 balance responses so that clients can parse balances as a map keyed by ticker.
pub type CoinBalanceMap = std::collections::HashMap<String, CoinBalance>;

/// Wraps a single `CoinBalance` into a ticker-keyed map with a single entry.
pub fn coin_balance_map_for_ticker(ticker: &str, balance: CoinBalance) -> CoinBalanceMap {
    let mut map = CoinBalanceMap::new();
    map.insert(ticker.to_string(), balance);
    map
}

/// Sums per-ticker balances across the given HD address balances into a single ticker-keyed total map.
///
/// The activated coin's ticker is present even when `balances` is empty.
pub fn sum_hd_address_balances<'a, I>(ticker: &str, balances: I) -> CoinBalanceMap
where
    I: IntoIterator<Item = &'a HDAddressBalance>,
{
    let mut total = coin_balance_map_for_ticker(ticker, CoinBalance::default());
    for addr_balance in balances {
        for (ticker, balance) in &addr_balance.balance {
            let entry = total.entry(ticker.clone()).or_default();
            *entry = entry.clone() + balance.clone();
        }
    }
    total
}

#[derive(Display)]
pub enum EnableCoinBalanceError {
    NewAccountCreatingError(NewAccountCreatingError),
    BalanceError(BalanceError),
}

impl From<NewAccountCreatingError> for EnableCoinBalanceError {
    fn from(e: NewAccountCreatingError) -> Self { EnableCoinBalanceError::NewAccountCreatingError(e) }
}

impl From<BalanceError> for EnableCoinBalanceError {
    fn from(e: BalanceError) -> Self { EnableCoinBalanceError::BalanceError(e) }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "wallet_type")]
pub enum EnableCoinBalance {
    Iguana(IguanaWalletBalance),
    HD(HDWalletBalance),
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct IguanaWalletBalance {
    pub address: String,
    pub balance: CoinBalance,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct HDWalletBalance {
    pub accounts: Vec<HDAccountBalance>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct HDAccountBalance {
    pub account_index: u32,
    pub derivation_path: RpcDerivationPath,
    pub total_balance: CoinBalanceMap,
    pub addresses: Vec<HDAddressBalance>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct HDAddressBalance {
    pub address: String,
    pub derivation_path: RpcDerivationPath,
    pub chain: Bip44Chain,
    pub balance: CoinBalanceMap,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnableCoinScanPolicy {
    /// Don't scan for new addresses.
    DoNotScan,
    /// Scan for new addresses if the coin HD wallet hasn't been enabled *only*.
    /// In other words, scan for new addresses if there were no HD accounts in the HD wallet storage.
    ScanIfNewWallet,
    /// Scan for new addresses even if the coin HD wallet has been enabled before.
    Scan,
}

impl Default for EnableCoinScanPolicy {
    fn default() -> Self { EnableCoinScanPolicy::ScanIfNewWallet }
}

#[async_trait]
pub trait EnableCoinBalanceOps {
    /// `path_to_address` is the address a software global-HD activation handles (CRD ch.05 R35a): its account is
    /// created when the wallet holds none at that index, and its chain is listed up to that address. `None` for a
    /// coin whose activation does not honor `path_to_address` (the Trezor policy), and it has no effect on a
    /// single-address (Iguana) coin.
    async fn enable_coin_balance<XPubExtractor>(
        &self,
        xpub_extractor: Option<&XPubExtractor>,
        scan_policy: EnableCoinScanPolicy,
        min_addresses_number: u32,
        path_to_address: Option<HDAddressId>,
    ) -> MmResult<EnableCoinBalance, EnableCoinBalanceError>
    where
        XPubExtractor: HDXPubExtractor + Sync;
}

#[async_trait]
impl<Coin> EnableCoinBalanceOps for Coin
where
    Coin: CoinWithDerivationMethod<HDWallet = <Coin as HDWalletCoinOps>::HDWallet>
        + HDWalletBalanceOps
        + MarketCoinOps
        + Sync,
    <Coin as CoinWithDerivationMethod>::Address: fmt::Display + Sync,
{
    async fn enable_coin_balance<XPubExtractor>(
        &self,
        xpub_extractor: Option<&XPubExtractor>,
        scan_policy: EnableCoinScanPolicy,
        min_addresses_number: u32,
        path_to_address: Option<HDAddressId>,
    ) -> MmResult<EnableCoinBalance, EnableCoinBalanceError>
    where
        XPubExtractor: HDXPubExtractor + Sync,
    {
        match self.derivation_method() {
            DerivationMethod::Iguana(my_address) => self
                .my_balance()
                .compat()
                .await
                .map(|balance| {
                    EnableCoinBalance::Iguana(IguanaWalletBalance {
                        address: my_address.to_string(),
                        balance,
                    })
                })
                .mm_err(EnableCoinBalanceError::from),
            DerivationMethod::HDWallet(hd_wallet) => self
                .enable_hd_wallet(
                    hd_wallet,
                    xpub_extractor,
                    scan_policy,
                    min_addresses_number,
                    path_to_address,
                )
                .await
                .map(EnableCoinBalance::HD),
        }
    }
}

#[async_trait]
pub trait HDWalletBalanceOps: HDWalletCoinOps {
    type HDAddressScanner: HDAddressBalanceScanner<Address = Self::Address>;

    async fn produce_hd_address_scanner(&self) -> BalanceResult<Self::HDAddressScanner>;

    /// Requests balances of already known addresses, and if it's prescribed by [`EnableCoinParams::scan_policy`],
    /// scans for new addresses of every HD account by using [`HDWalletBalanceOps::scan_for_new_addresses`].
    /// This method is used on coin initialization to index working addresses and to return the wallet balance to the user.
    ///
    /// `path_to_address` is the activated address of a software global-HD activation (CRD ch.05 R35a):
    /// - when the wallet holds no account at its `account_id`, that account is created, and the result reports
    ///   that account only;
    /// - the known-address count of its chain is raised to at least `address_id + 1`, beside
    ///   `min_addresses_number`, which raises the external chain of every account.
    async fn enable_hd_wallet<XPubExtractor>(
        &self,
        hd_wallet: &Self::HDWallet,
        xpub_extractor: Option<&XPubExtractor>,
        scan_policy: EnableCoinScanPolicy,
        min_addresses_number: u32,
        path_to_address: Option<HDAddressId>,
    ) -> MmResult<HDWalletBalance, EnableCoinBalanceError>
    where
        XPubExtractor: HDXPubExtractor + Sync;

    /// Scans for the new addresses of the specified `hd_account` using the given `address_scanner`.
    /// Returns balances of the new addresses.
    async fn scan_for_new_addresses(
        &self,
        hd_wallet: &Self::HDWallet,
        hd_account: &mut Self::HDAccount,
        address_scanner: &Self::HDAddressScanner,
        gap_limit: u32,
    ) -> BalanceResult<Vec<HDAddressBalance>>;

    /// Requests balances of every known addresses of the given `hd_account`.
    async fn all_known_addresses_balances(&self, hd_account: &Self::HDAccount) -> BalanceResult<Vec<HDAddressBalance>>;

    /// Requests balances of known addresses of the given `address_ids` addresses at the specified `chain`.
    async fn known_addresses_balances_with_ids<Ids>(
        &self,
        hd_account: &Self::HDAccount,
        chain: Bip44Chain,
        address_ids: Ids,
    ) -> BalanceResult<Vec<HDAddressBalance>>
    where
        Self: MarketCoinOps,
        Self::Address: fmt::Display + Clone,
        Ids: Iterator<Item = u32> + Send,
    {
        let (addresses, der_paths) = address_ids
            .into_iter()
            .map(|address_id| -> BalanceResult<_> {
                let HDAddress {
                    address,
                    derivation_path,
                    ..
                } = self.derive_address(hd_account, chain, address_id).mm_err(Into::into)?;
                Ok((address, derivation_path))
            })
            // Try to unzip `Result<(Address, DerivationPath)>` elements into `Result<(Vec<Address>, Vec<DerivationPath>)>`.
            .try_unzip::<Vec<_>, Vec<_>>()?;

        let balances = self
            .known_addresses_balances(addresses)
            .await?
            .into_iter()
            // [`HDWalletBalanceOps::known_addresses_balances`] returns pairs `(Address, CoinBalance)`
            // that are guaranteed to be in the same order in which they were requested.
            // So we can zip the derivation paths with the pairs `(Address, CoinBalance)`.
            .zip(der_paths)
            .map(|((address, balance), derivation_path)| HDAddressBalance {
                address: address.to_string(),
                derivation_path: RpcDerivationPath(derivation_path),
                chain,
                balance: coin_balance_map_for_ticker(self.ticker(), balance),
            })
            .collect();
        Ok(balances)
    }

    /// Requests balance of the given `address`.
    /// This function is expected to be more efficient than ['HDWalletBalanceOps::is_address_used'] in most cases
    /// since many of RPC clients allow us to request the address balance without the history.
    async fn known_address_balance(&self, address: &Self::Address) -> BalanceResult<CoinBalance>;

    /// Requests balances of the given `addresses`.
    /// The pairs `(Address, CoinBalance)` are guaranteed to be in the same order in which they were requested.
    async fn known_addresses_balances(
        &self,
        addresses: Vec<Self::Address>,
    ) -> BalanceResult<Vec<(Self::Address, CoinBalance)>>;

    /// Checks if the address has been used by the user by checking if the transaction history of the given `address` is not empty.
    /// Please note the function can return zero balance even if the address has been used before.
    async fn is_address_used(
        &self,
        address: &Self::Address,
        address_scanner: &Self::HDAddressScanner,
    ) -> BalanceResult<AddressBalanceStatus<CoinBalance>> {
        if !address_scanner.is_address_used(address).await? {
            return Ok(AddressBalanceStatus::NotUsed);
        }
        // Now we know that the address has been used.
        let balance = self.known_address_balance(address).await?;
        Ok(AddressBalanceStatus::Used(balance))
    }
}

#[async_trait]
pub trait HDAddressBalanceScanner: Sync {
    type Address;

    async fn is_address_used(&self, address: &Self::Address) -> BalanceResult<bool>;
}

pub enum AddressBalanceStatus<Balance> {
    Used(Balance),
    NotUsed,
}

pub mod common_impl {
    use super::*;
    use crate::hd_wallet::{HDAccountOps, HDWalletOps};

    /// The known-address counts an activation raises the chains of an account to (external, internal): the external
    /// chain to `min_addresses_number`, and the chain of `path_to_address` to at least `address_id + 1` when the
    /// account is the path's (CRD ch.05 R35a item 2, ch.38 R38.8.5). On the external chain the larger of the two
    /// applies. A count of `0` leaves a chain alone.
    pub(crate) fn minimum_known_addresses(
        min_addresses_number: u32,
        path_to_address: Option<&HDAddressId>,
    ) -> (u32, u32) {
        let mut external = min_addresses_number;
        let mut internal = 0;
        if let Some(path) = path_to_address {
            // `address_id` can reach the limit of the hardened children; the sum stays beyond it and is refused.
            let path_minimum = path.address_id.saturating_add(1);
            match path.chain {
                Bip44Chain::External => external = external.max(path_minimum),
                Bip44Chain::Internal => internal = path_minimum,
            }
        }
        (external, internal)
    }

    /// Activates `hd_account`: reports its known addresses, scans it when asked, and raises its chains to the
    /// minimum counts of [`minimum_known_addresses`]. `path_to_address` is `Some` for the path's account only.
    pub(crate) async fn enable_hd_account<Coin>(
        coin: &Coin,
        hd_wallet: &Coin::HDWallet,
        hd_account: &mut Coin::HDAccount,
        address_scanner: &Coin::HDAddressScanner,
        scan_new_addresses: bool,
        min_addresses_number: u32,
        path_to_address: Option<&HDAddressId>,
    ) -> MmResult<HDAccountBalance, EnableCoinBalanceError>
    where
        Coin: HDWalletBalanceOps + MarketCoinOps + Sync,
        Coin::Address: fmt::Display + Clone,
    {
        let gap_limit = hd_wallet.gap_limit();
        let mut addresses = coin.all_known_addresses_balances(hd_account).await.mm_err(Into::into)?;
        if scan_new_addresses {
            addresses.extend(
                coin.scan_for_new_addresses(hd_wallet, hd_account, address_scanner, gap_limit)
                    .await
                    .mm_err(Into::into)?,
            );
        }
        let (external_minimum, internal_minimum) = minimum_known_addresses(min_addresses_number, path_to_address);
        ensure_minimum_addresses(
            coin,
            hd_wallet,
            hd_account,
            Bip44Chain::External,
            external_minimum,
            &mut addresses,
        )
        .await
        .mm_err(Into::into)?;
        // A coin without an internal chain (the EVM coins) has nothing to raise there.
        if internal_minimum > 0 && hd_account.known_addresses_number(Bip44Chain::Internal).is_ok() {
            ensure_minimum_addresses(
                coin,
                hd_wallet,
                hd_account,
                Bip44Chain::Internal,
                internal_minimum,
                &mut addresses,
            )
            .await
            .mm_err(Into::into)?;
        }

        let total_balance = sum_hd_address_balances(coin.ticker(), &addresses);
        let account_balance = HDAccountBalance {
            account_index: hd_account.account_id(),
            derivation_path: RpcDerivationPath(hd_account.account_derivation_path()),
            total_balance,
            addresses,
        };

        Ok(account_balance)
    }

    /// Raises the number of known addresses of `chain` to `min_addresses_number`: the missing addresses are derived
    /// at the next contiguous indices, their balances are read and the new count is persisted. An account that
    /// already has that many is left alone.
    pub(crate) async fn ensure_minimum_addresses<Coin>(
        coin: &Coin,
        hd_wallet: &Coin::HDWallet,
        hd_account: &mut Coin::HDAccount,
        chain: Bip44Chain,
        min_addresses_number: u32,
        addresses: &mut Vec<HDAddressBalance>,
    ) -> BalanceResult<()>
    where
        Coin: HDWalletBalanceOps + MarketCoinOps + Sync,
        Coin::Address: fmt::Display + Clone,
    {
        let known_addresses_number = hd_account
            .known_addresses_number(chain)
            .mm_err(|error| BalanceError::Internal(error.to_string()))?;
        if known_addresses_number >= min_addresses_number {
            return Ok(());
        }
        if min_addresses_number >= crypto::ChildNumber::HARDENED_FLAG {
            return MmError::err(
                crate::hd_wallet::AccountUpdatingError::AddressLimitReached {
                    max_addresses_number: crypto::ChildNumber::HARDENED_FLAG,
                }
                .into(),
            );
        }

        let new_addresses = coin
            .known_addresses_balances_with_ids(hd_account, chain, known_addresses_number..min_addresses_number)
            .await?;
        coin.set_known_addresses_number(hd_wallet, hd_account, chain, min_addresses_number)
            .await
            .mm_err(Into::into)?;
        addresses.extend(new_addresses);
        Ok(())
    }

    pub(crate) async fn enable_hd_wallet<Coin, XPubExtractor>(
        coin: &Coin,
        hd_wallet: &Coin::HDWallet,
        xpub_extractor: Option<&XPubExtractor>,
        scan_policy: EnableCoinScanPolicy,
        min_addresses_number: u32,
        path_to_address: Option<HDAddressId>,
    ) -> MmResult<HDWalletBalance, EnableCoinBalanceError>
    where
        Coin: HDWalletBalanceOps + MarketCoinOps + Sync,
        Coin::Address: fmt::Display + Clone,
        XPubExtractor: HDXPubExtractor + Sync,
    {
        let address_scanner = coin.produce_hd_address_scanner().await.mm_err(Into::into)?;

        // The account the activation must create (CRD ch.05 R35a item 1): the path's account when the wallet holds
        // none at that index; without a path, the first account of an empty wallet.
        let index_to_create = {
            let accounts = hd_wallet.get_accounts_mut().await;
            match &path_to_address {
                Some(path) if !accounts.contains_key(&path.account_id) => {
                    Some(NewAccountIndex::Requested(path.account_id))
                },
                Some(_) => None,
                None if accounts.is_empty() => Some(NewAccountIndex::LowestUnused),
                None => None,
            }
        };

        if let Some(index) = index_to_create {
            info!(
                "{} HD wallet holds no account at the activated index. Create it: {:?}",
                coin.ticker(),
                index
            );

            // Create the new HD account. The result reports this account only.
            let mut new_account = coin
                .create_new_account(hd_wallet, xpub_extractor, index)
                .await
                .mm_err(Into::into)?;
            let scan_new_addresses = matches!(
                scan_policy,
                EnableCoinScanPolicy::ScanIfNewWallet | EnableCoinScanPolicy::Scan
            );

            let account_balance = enable_hd_account(
                coin,
                hd_wallet,
                &mut new_account,
                &address_scanner,
                scan_new_addresses,
                min_addresses_number,
                path_to_address.as_ref(),
            )
            .await?;
            return Ok(HDWalletBalance {
                accounts: vec![account_balance],
            });
        }

        let mut accounts = hd_wallet.get_accounts_mut().await;
        debug!(
            "{} HD accounts were found on {} coin activation",
            accounts.len(),
            coin.ticker()
        );
        let mut result = HDWalletBalance {
            accounts: Vec::with_capacity(accounts.len()),
        };
        let scan_new_addresses = matches!(scan_policy, EnableCoinScanPolicy::Scan);
        for (account_id, hd_account) in accounts.iter_mut() {
            // Only the path's account is raised to the activated address.
            let account_path = path_to_address.as_ref().filter(|path| path.account_id == *account_id);
            let account_balance = enable_hd_account(
                coin,
                hd_wallet,
                hd_account,
                &address_scanner,
                scan_new_addresses,
                min_addresses_number,
                account_path,
            )
            .await?;
            result.accounts.push(account_balance);
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(account_id: u32, chain: Bip44Chain, address_id: u32) -> HDAddressId {
        HDAddressId {
            account_id,
            chain,
            address_id,
        }
    }

    /// CRD ch.05 R35a item 2: the path's chain is raised to `address_id + 1`, and on the external chain the larger
    /// of that and `min_addresses_number` applies. Without a path, `min_addresses_number` alone applies.
    #[test]
    fn t14a_the_path_raises_its_own_chain() {
        use common_impl::minimum_known_addresses;

        // The default path: address 0 of the external chain is listed without `min_addresses_number`.
        assert_eq!(
            minimum_known_addresses(0, Some(&path(0, Bip44Chain::External, 0))),
            (1, 0)
        );
        // Account 1, external, address 3, no minimum: addresses 0 to 3.
        assert_eq!(
            minimum_known_addresses(0, Some(&path(1, Bip44Chain::External, 3))),
            (4, 0)
        );
        // The larger of the two wins on the external chain, in both directions.
        assert_eq!(
            minimum_known_addresses(10, Some(&path(0, Bip44Chain::External, 3))),
            (10, 0)
        );
        assert_eq!(
            minimum_known_addresses(2, Some(&path(0, Bip44Chain::External, 3))),
            (4, 0)
        );
        // The internal chain is raised, not the external one.
        assert_eq!(
            minimum_known_addresses(0, Some(&path(0, Bip44Chain::Internal, 5))),
            (0, 6)
        );
        assert_eq!(
            minimum_known_addresses(2, Some(&path(0, Bip44Chain::Internal, 5))),
            (2, 6)
        );
        // No path: `min_addresses_number` alone, and none forces an address.
        assert_eq!(minimum_known_addresses(0, None), (0, 0));
        assert_eq!(minimum_known_addresses(3, None), (3, 0));
        // An address at the hardened limit leaves a minimum beyond it, which `ensure_minimum_addresses` refuses.
        let (external, _) = minimum_known_addresses(
            0,
            Some(&path(0, Bip44Chain::External, crypto::ChildNumber::HARDENED_FLAG)),
        );
        assert!(external >= crypto::ChildNumber::HARDENED_FLAG);
    }

    #[test]
    fn empty_hd_balance_keeps_ticker_key() {
        let total = sum_hd_address_balances("KMD", std::iter::empty::<&HDAddressBalance>());

        assert_eq!(total, coin_balance_map_for_ticker("KMD", CoinBalance::default()));
    }
}
