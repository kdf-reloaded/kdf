use crate::coin_balance::{AddressBalanceStatus, HDAddressBalance, HDAddressBalanceScanner, HDWalletBalanceOps};
use crate::hd_pubkey::{ExtractExtendedPubkey, HDExtractPubkeyError, HDXPubExtractor};
use crate::hd_wallet::{AccountUpdatingError, AddressDerivingError, AsyncMutexGuard, GetNewHDAddressParams,
                       GetNewHDAddressResponse, HDAccountMut, HDAccountOps, HDAccountsMutex, HDAddress,
                       HDWalletCoinOps, HDWalletOps, HDWalletRpcError, HDWalletRpcOps, InvalidBip44ChainError,
                       NewAccountCreatingError, NewAddressDerivingError};
use crate::hd_wallet_storage::{HDAccountStorageItem, HDWalletCoinStorage, HDWalletCoinWithStorageOps,
                               HDWalletStorageResult};
use crate::{coin_balance, hd_wallet, BalanceError, BalanceResult, CoinBalance, CoinWithDerivationMethod,
            DerivationMethod};
use async_trait::async_trait;
use bip32::ChildNumber;
use crypto::{Bip32DerPathOps, Bip44Chain, Bip44PathToAccount, Bip44PathToCoin, CryptoCtx, DerivationPath,
             KeyPairPolicy, RpcDerivationPath, Secp256k1ExtendedPublicKey};
use ethereum_types::Address;
use mm2_err_handle::prelude::*;
use mm2_eth::keys::{public_to_address, Public};
use std::str::FromStr;
// LP-17: alloy provider replaces `web3::Web3` for nonce/balance lookups.
use super::alloy_compat::{assert_send_future, KdfProvider};
use alloy::primitives::Address as AlloyAddress;
use alloy::providers::Provider;
use std::future::IntoFuture;

use super::EthCoin;

// -------------------------------------------------------------------
// Type aliases mirroring the UTXO pattern
// -------------------------------------------------------------------

pub type EthHDAddress = HDAddress<Address, Public>;

// -------------------------------------------------------------------
// EthHDAccount
// -------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct EthHDAccount {
    pub account_id: u32,
    pub extended_pubkey: Secp256k1ExtendedPublicKey,
    pub account_derivation_path: Bip44PathToAccount,
    pub external_addresses_number: u32,
}

impl HDAccountOps for EthHDAccount {
    fn known_addresses_number(&self, chain: Bip44Chain) -> MmResult<u32, InvalidBip44ChainError> {
        match chain {
            // ETH only uses the external chain.
            Bip44Chain::External => Ok(self.external_addresses_number),
            other => MmError::err(InvalidBip44ChainError { chain: other }),
        }
    }

    fn account_derivation_path(&self) -> DerivationPath { self.account_derivation_path.to_derivation_path() }

    fn account_id(&self) -> u32 { self.account_id }
}

impl EthHDAccount {
    pub fn try_from_storage_item(
        wallet_der_path: &Bip44PathToCoin,
        account_info: &HDAccountStorageItem,
    ) -> HDWalletStorageResult<EthHDAccount> {
        const ACCOUNT_CHILD_HARDENED: bool = true;
        let account_child = ChildNumber::new(account_info.account_id, ACCOUNT_CHILD_HARDENED)?;
        let account_derivation_path = wallet_der_path
            .derive(account_child)
            .map_to_mm(crypto::Bip44DerPathError::from)
            .mm_err(Into::into)?;
        let extended_pubkey = Secp256k1ExtendedPublicKey::from_str(&account_info.account_xpub)?;
        Ok(EthHDAccount {
            account_id: account_info.account_id,
            extended_pubkey,
            account_derivation_path,
            external_addresses_number: account_info.external_addresses_number,
        })
    }

    pub fn to_storage_item(&self) -> HDAccountStorageItem {
        HDAccountStorageItem {
            account_id: self.account_id,
            account_xpub: self.extended_pubkey.to_string(bip32::Prefix::XPUB),
            external_addresses_number: self.external_addresses_number,
            // ETH doesn't use internal chain; store 0 for compatibility.
            internal_addresses_number: 0,
        }
    }
}

// -------------------------------------------------------------------
// EthHDWallet
// -------------------------------------------------------------------

#[derive(Debug)]
pub struct EthHDWallet {
    pub hd_wallet_storage: HDWalletCoinStorage,
    pub derivation_path: Bip44PathToCoin,
    pub accounts: HDAccountsMutex<EthHDAccount>,
    pub gap_limit: u32,
}

impl HDWalletOps for EthHDWallet {
    type HDAccount = EthHDAccount;

    fn coin_type(&self) -> u32 { self.derivation_path.coin_type() }
    fn gap_limit(&self) -> u32 { self.gap_limit }
    fn get_accounts_mutex(&self) -> &HDAccountsMutex<Self::HDAccount> { &self.accounts }
}

// -------------------------------------------------------------------
// HDWalletCoinWithStorageOps for EthCoin
// -------------------------------------------------------------------

impl HDWalletCoinWithStorageOps for EthCoin {
    fn hd_wallet_storage<'a>(&self, hd_wallet: &'a Self::HDWallet) -> &'a HDWalletCoinStorage {
        &hd_wallet.hd_wallet_storage
    }
}

// -------------------------------------------------------------------
// Helper: secp256k1 extended pubkey → uncompressed ETH Public key
// -------------------------------------------------------------------

pub fn pubkey_from_extended(extended_pubkey: &Secp256k1ExtendedPublicKey) -> Public {
    let serialized = extended_pubkey.public_key().serialize_uncompressed();
    // Skip the 0x04 prefix byte; ETH Public is the raw 64-byte uncompressed key.
    let mut pubkey = Public::default();
    pubkey.as_mut().copy_from_slice(&serialized[1..]);
    pubkey
}

// -------------------------------------------------------------------
// HDWalletCoinOps for EthCoin — derive ETH addresses from HD keys
// -------------------------------------------------------------------

#[async_trait]
impl HDWalletCoinOps for EthCoin {
    type Address = Address;
    type Pubkey = Public;
    type HDWallet = EthHDWallet;
    type HDAccount = EthHDAccount;

    fn derive_address(
        &self,
        hd_account: &Self::HDAccount,
        chain: Bip44Chain,
        address_id: u32,
    ) -> MmResult<HDAddress<Self::Address, Self::Pubkey>, AddressDerivingError> {
        let change_child = chain.to_child_number();
        let address_id_child = ChildNumber::from(address_id);

        let derived_pubkey = hd_account
            .extended_pubkey
            .derive_child(change_child)?
            .derive_child(address_id_child)?;

        let pubkey = pubkey_from_extended(&derived_pubkey);
        let address = public_to_address(&pubkey);

        let mut derivation_path = hd_account.account_derivation_path.to_derivation_path();
        derivation_path.push(change_child);
        derivation_path.push(address_id_child);

        Ok(HDAddress {
            address,
            pubkey,
            derivation_path,
        })
    }

    async fn create_new_account<'a, XPubExtractor>(
        &self,
        hd_wallet: &'a Self::HDWallet,
        xpub_extractor: Option<&XPubExtractor>,
    ) -> MmResult<HDAccountMut<'a, Self::HDAccount>, NewAccountCreatingError>
    where
        XPubExtractor: HDXPubExtractor + Sync,
    {
        create_new_account(self, hd_wallet, xpub_extractor).await
    }

    async fn set_known_addresses_number(
        &self,
        hd_wallet: &Self::HDWallet,
        hd_account: &mut Self::HDAccount,
        chain: Bip44Chain,
        new_known_addresses_number: u32,
    ) -> MmResult<(), AccountUpdatingError> {
        set_known_addresses_number(self, hd_wallet, hd_account, chain, new_known_addresses_number).await
    }
}

// -------------------------------------------------------------------
// Account creation (mirrors utxo_common::create_new_account)
// -------------------------------------------------------------------

async fn create_new_account<'a, XPubExtractor>(
    coin: &EthCoin,
    hd_wallet: &'a EthHDWallet,
    xpub_extractor: Option<&XPubExtractor>,
) -> MmResult<HDAccountMut<'a, EthHDAccount>, NewAccountCreatingError>
where
    XPubExtractor: HDXPubExtractor + Sync,
{
    const INIT_ACCOUNT_ID: u32 = 0;
    let new_account_id = hd_wallet
        .accounts
        .lock()
        .await
        .iter()
        .last()
        .map(|(account_id, _)| *account_id + 1)
        .unwrap_or(INIT_ACCOUNT_ID);
    if new_account_id >= ChildNumber::HARDENED_FLAG {
        return MmError::err(NewAccountCreatingError::AccountLimitReached {
            max_accounts_number: ChildNumber::HARDENED_FLAG,
        });
    }

    let account_child =
        ChildNumber::new(new_account_id, true).map_to_mm(|e| NewAccountCreatingError::Internal(e.to_string()))?;

    let account_derivation_path: Bip44PathToAccount = hd_wallet.derivation_path.derive(account_child)?;
    let account_pubkey = coin
        .extract_extended_pubkey(xpub_extractor, account_derivation_path.to_derivation_path())
        .await
        .mm_err(Into::into)?;

    let new_account = EthHDAccount {
        account_id: new_account_id,
        extended_pubkey: account_pubkey,
        account_derivation_path,
        external_addresses_number: 0,
    };

    let accounts = hd_wallet.accounts.lock().await;
    if accounts.contains_key(&new_account_id) {
        let error = format!(
            "Account '{}' has been activated while we proceed the 'create_new_account' function",
            new_account_id
        );
        return MmError::err(NewAccountCreatingError::Internal(error));
    }

    coin.upload_new_account(hd_wallet, new_account.to_storage_item())
        .await
        .mm_err(Into::into)?;

    Ok(AsyncMutexGuard::map(accounts, |accounts| {
        accounts.entry(new_account_id).or_insert(new_account)
    }))
}

// -------------------------------------------------------------------
// Known-address-number update (mirrors utxo_common pattern)
// -------------------------------------------------------------------

async fn set_known_addresses_number(
    coin: &EthCoin,
    hd_wallet: &EthHDWallet,
    hd_account: &mut EthHDAccount,
    chain: Bip44Chain,
    new_known_addresses_number: u32,
) -> MmResult<(), AccountUpdatingError> {
    if new_known_addresses_number >= ChildNumber::HARDENED_FLAG {
        return MmError::err(AccountUpdatingError::AddressLimitReached {
            max_addresses_number: ChildNumber::HARDENED_FLAG,
        });
    }
    match chain {
        Bip44Chain::External => {
            coin.update_external_addresses_number(hd_wallet, hd_account.account_id, new_known_addresses_number)
                .await
                .mm_err(Into::into)?;
            hd_account.external_addresses_number = new_known_addresses_number;
        },
        Bip44Chain::Internal => {
            // ETH doesn't use internal addresses but handle gracefully for protocol compliance.
            coin.update_internal_addresses_number(hd_wallet, hd_account.account_id, new_known_addresses_number)
                .await
                .mm_err(Into::into)?;
        },
    }
    Ok(())
}

// -------------------------------------------------------------------
// ExtractExtendedPubkey for EthCoin — derive account xpub from seed
// -------------------------------------------------------------------

#[async_trait]
impl ExtractExtendedPubkey for EthCoin {
    type ExtendedPublicKey = Secp256k1ExtendedPublicKey;

    async fn extract_extended_pubkey<XPubExtractor>(
        &self,
        _xpub_extractor: Option<&XPubExtractor>,
        derivation_path: DerivationPath,
    ) -> MmResult<Self::ExtendedPublicKey, HDExtractPubkeyError>
    where
        XPubExtractor: HDXPubExtractor + Sync,
    {
        // For software wallets we derive from the BIP39 root key directly.
        let ctx = MmArc::from_weak(&self.ctx)
            .or_mm_err(|| HDExtractPubkeyError::Internal("MmArc not available".to_string()))?;
        let crypto_ctx = CryptoCtx::from_ctx(&ctx).mm_err(Into::into)?;
        let global_hd = match crypto_ctx.key_pair_policy() {
            KeyPairPolicy::GlobalHDAccount(ref hd_arc) => hd_arc,
            KeyPairPolicy::Iguana => {
                return MmError::err(HDExtractPubkeyError::HwContextNotInitialized);
            },
        };

        // Walk the derivation path from the root private key, then extract the public key.
        let mut priv_key = global_hd.root_priv_key().clone();
        for child in derivation_path.iter() {
            priv_key = priv_key
                .derive_child(child)
                .map_to_mm(|e| HDExtractPubkeyError::Internal(format!("BIP32 derivation error: {}", e)))?;
        }
        Ok(priv_key.public_key())
    }
}

// -------------------------------------------------------------------
// HDWalletRpcOps for EthCoin — delegates to common_impl
// -------------------------------------------------------------------

#[async_trait]
impl HDWalletRpcOps for EthCoin {
    async fn get_new_address_rpc(
        &self,
        params: GetNewHDAddressParams,
    ) -> MmResult<GetNewHDAddressResponse, HDWalletRpcError> {
        hd_wallet::common_impl::get_new_address_rpc(self, params).await
    }
}

// -------------------------------------------------------------------
// CoinWithDerivationMethod for EthCoin
// -------------------------------------------------------------------

impl CoinWithDerivationMethod for EthCoin {
    type Address = Address;
    type HDWallet = EthHDWallet;

    fn derivation_method(&self) -> &DerivationMethod<Self::Address, Self::HDWallet> { &self.derivation_method }
}

// -------------------------------------------------------------------
// HDAddressBalanceScanner for ETH — checks nonce/balance
// -------------------------------------------------------------------

/// LP-17: scanner now drives an alloy [`KdfProvider`] instead of
/// `web3::Web3<Web3Transport>`. Wire-level RPC methods
/// (`eth_getTransactionCount`, `eth_getBalance`) and the implicit
/// `latest` block tag are unchanged.
pub struct EthAddressScanner {
    provider: KdfProvider,
}

#[async_trait]
impl HDAddressBalanceScanner for EthAddressScanner {
    type Address = Address;

    async fn is_address_used(&self, address: &Address) -> BalanceResult<bool> {
        // An address is considered "used" if its nonce > 0 or balance > 0.
        let alloy_addr = AlloyAddress::from(address.0);
        let nonce = assert_send_future(self.provider.get_transaction_count(alloy_addr).into_future())
            .await
            .map_err(|e| BalanceError::Transport(format!("{}", e)))?;
        if nonce != 0 {
            return Ok(true);
        }
        let balance = assert_send_future(self.provider.get_balance(alloy_addr).into_future())
            .await
            .map_err(|e| BalanceError::Transport(format!("{}", e)))?;
        Ok(!balance.is_zero())
    }
}

// -------------------------------------------------------------------
// HDWalletBalanceOps for EthCoin
// -------------------------------------------------------------------

use crate::coin_balance::{EnableCoinBalanceError, EnableCoinScanPolicy, HDWalletBalance};

#[async_trait]
impl HDWalletBalanceOps for EthCoin {
    type HDAddressScanner = EthAddressScanner;

    async fn produce_hd_address_scanner(&self) -> BalanceResult<Self::HDAddressScanner> {
        Ok(EthAddressScanner {
            provider: self.alloy_provider(),
        })
    }

    async fn enable_hd_wallet<XPubExtractor>(
        &self,
        hd_wallet: &Self::HDWallet,
        xpub_extractor: Option<&XPubExtractor>,
        scan_policy: EnableCoinScanPolicy,
    ) -> MmResult<HDWalletBalance, EnableCoinBalanceError>
    where
        XPubExtractor: HDXPubExtractor + Sync,
    {
        coin_balance::common_impl::enable_hd_wallet(self, hd_wallet, xpub_extractor, scan_policy).await
    }

    async fn scan_for_new_addresses(
        &self,
        hd_wallet: &Self::HDWallet,
        hd_account: &mut Self::HDAccount,
        address_scanner: &Self::HDAddressScanner,
        gap_limit: u32,
    ) -> BalanceResult<Vec<HDAddressBalance>> {
        // ETH only uses the External chain.
        scan_for_new_addresses_impl(
            self,
            hd_wallet,
            hd_account,
            address_scanner,
            Bip44Chain::External,
            gap_limit,
        )
        .await
    }

    async fn all_known_addresses_balances(&self, hd_account: &Self::HDAccount) -> BalanceResult<Vec<HDAddressBalance>> {
        let external_ids = 0..hd_account.external_addresses_number;
        self.known_addresses_balances_with_ids(hd_account, Bip44Chain::External, external_ids)
            .await
    }

    async fn known_address_balance(&self, address: &Self::Address) -> BalanceResult<CoinBalance> {
        let provider = self.alloy_provider();
        let alloy_addr = AlloyAddress::from(address.0);
        let balance = assert_send_future(provider.get_balance(alloy_addr).into_future())
            .await
            .map_err(|e| BalanceError::Transport(format!("{}", e)))?;
        // alloy returns its own `U256`; round-trip through bytes to feed the
        // existing `u256_to_big_decimal` helper that takes the legacy
        // `ethereum_types::U256`.
        let balance_legacy = ethereum_types::U256::from_big_endian(&balance.to_be_bytes::<32>());

        let balance_decimal = u256_to_big_decimal(balance_legacy, self.decimals)?;
        Ok(CoinBalance {
            spendable: balance_decimal,
            unspendable: BigDecimal::from(0),
        })
    }

    async fn known_addresses_balances(
        &self,
        addresses: Vec<Self::Address>,
    ) -> BalanceResult<Vec<(Self::Address, CoinBalance)>> {
        let mut result = Vec::with_capacity(addresses.len());
        for addr in addresses {
            let balance = self.known_address_balance(&addr).await?;
            result.push((addr, balance));
        }
        Ok(result)
    }
}

// -------------------------------------------------------------------
// Address scanning (mirrors utxo_common::scan_for_new_addresses_impl)
// -------------------------------------------------------------------

async fn scan_for_new_addresses_impl(
    coin: &EthCoin,
    hd_wallet: &EthHDWallet,
    hd_account: &mut EthHDAccount,
    address_scanner: &EthAddressScanner,
    chain: Bip44Chain,
    gap_limit: u32,
) -> BalanceResult<Vec<HDAddressBalance>> {
    let mut balances = Vec::with_capacity(gap_limit as usize);

    let mut checking_address_id = hd_account
        .known_addresses_number(chain)
        .mm_err(|e| BalanceError::Internal(e.to_string()))?;

    let mut unused_addresses_counter = 0u32;
    while checking_address_id < ChildNumber::HARDENED_FLAG && unused_addresses_counter < gap_limit {
        let HDAddress {
            address: checking_address,
            derivation_path: checking_address_der_path,
            ..
        } = coin
            .derive_address(hd_account, chain, checking_address_id)
            .mm_err(Into::into)?;

        match coin.is_address_used(&checking_address, address_scanner).await? {
            AddressBalanceStatus::Used(non_empty_balance) => {
                let last_non_empty_address_id = checking_address_id - unused_addresses_counter;
                for empty_address_id in last_non_empty_address_id..checking_address_id {
                    let empty_address = coin
                        .derive_address(hd_account, chain, empty_address_id)
                        .mm_err(Into::into)?;
                    balances.push(HDAddressBalance {
                        address: format!("{:#02x}", empty_address.address),
                        derivation_path: RpcDerivationPath(empty_address.derivation_path),
                        chain,
                        balance: CoinBalance::default(),
                    });
                }
                balances.push(HDAddressBalance {
                    address: format!("{:#02x}", checking_address),
                    derivation_path: RpcDerivationPath(checking_address_der_path),
                    chain,
                    balance: non_empty_balance,
                });
                unused_addresses_counter = 0;
            },
            AddressBalanceStatus::NotUsed => unused_addresses_counter += 1,
        }
        checking_address_id += 1;
    }

    coin.set_known_addresses_number(
        hd_wallet,
        hd_account,
        chain,
        checking_address_id - unused_addresses_counter,
    )
    .await
    .mm_err(Into::into)?;

    Ok(balances)
}

// -------------------------------------------------------------------
// AccountBalanceRpcOps for EthCoin
// -------------------------------------------------------------------

use crate::rpc_command::account_balance;
use crate::rpc_command::account_balance::{AccountBalanceParams, AccountBalanceRpcOps, HDAccountBalanceResponse};
use crate::rpc_command::hd_account_balance_rpc_error::HDAccountBalanceRpcError;

#[async_trait]
impl AccountBalanceRpcOps for EthCoin {
    async fn account_balance_rpc(
        &self,
        params: AccountBalanceParams,
    ) -> MmResult<HDAccountBalanceResponse, HDAccountBalanceRpcError> {
        account_balance::common_impl::account_balance_rpc(self, params).await
    }
}

// -------------------------------------------------------------------
// InitCreateHDAccountRpcOps for EthCoin
// -------------------------------------------------------------------

use crate::coin_balance::HDAccountBalance;
use crate::rpc_command::init_account_balance::{self as init_account_balance_mod, InitAccountBalanceParams,
                                               InitAccountBalanceRpcOps};
use crate::rpc_command::init_create_account::{self, CreateNewAccountParams, InitCreateHDAccountRpcOps};

#[async_trait]
impl InitAccountBalanceRpcOps for EthCoin {
    async fn init_account_balance_rpc(
        &self,
        params: InitAccountBalanceParams,
    ) -> MmResult<HDAccountBalance, HDAccountBalanceRpcError> {
        init_account_balance_mod::common_impl::init_account_balance_rpc(self, params).await
    }
}

#[async_trait]
impl InitCreateHDAccountRpcOps for EthCoin {
    async fn init_create_account_rpc<XPubExtractor>(
        &self,
        params: CreateNewAccountParams,
        xpub_extractor: &XPubExtractor,
    ) -> MmResult<HDAccountBalance, HDWalletRpcError>
    where
        XPubExtractor: HDXPubExtractor + Sync,
    {
        init_create_account::common_impl::init_create_new_account_rpc(self, params, xpub_extractor).await
    }
}

// -------------------------------------------------------------------
// Helpers
// -------------------------------------------------------------------

use bigdecimal::BigDecimal;
use futures::compat::Future01CompatExt;
use mm2_core::mm_ctx::MmArc;

fn u256_to_big_decimal(value: ethereum_types::U256, decimals: u8) -> BalanceResult<BigDecimal> {
    let ten = ethereum_types::U256::from(10u64);
    let divisor = ten.pow(ethereum_types::U256::from(decimals));
    let whole = value / divisor;
    let remainder = value % divisor;

    let whole_str = format!("{}", whole);
    let remainder_str = format!("{:0>width$}", remainder, width = decimals as usize);
    let decimal_str = format!("{}.{}", whole_str, remainder_str);
    BigDecimal::from_str(&decimal_str)
        .map_err(|e| BalanceError::Internal(format!("BigDecimal parse error: {}", e)).into())
}
