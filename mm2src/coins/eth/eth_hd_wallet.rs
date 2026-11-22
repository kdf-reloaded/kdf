use crate::coin_balance::{coin_balance_map_for_ticker, AddressBalanceStatus, CoinBalanceMap, HDAddressBalance,
                          HDAddressBalanceScanner, HDWalletBalanceOps};
use crate::hd_pubkey::{ExtractExtendedPubkey, HDExtractPubkeyError, HDXPubExtractor};
use crate::hd_wallet::{choose_new_account_id, AccountUpdatingError, AddressDerivingError, AsyncMutexGuard,
                       GetNewHDAddressParams, GetNewHDAddressResponse, HDAccountMut, HDAccountOps, HDAccountsMutex,
                       HDAddress, HDAddressId, HDWalletCoinOps, HDWalletOps, HDWalletRpcError, HDWalletRpcOps,
                       InvalidBip44ChainError, NewAccountCreatingError, NewAccountIndex, NewAddressDerivingError};
use crate::hd_wallet_storage::{HDAccountStorageItem, HDWalletCoinStorage, HDWalletCoinWithStorageOps,
                               HDWalletStorageResult};
use crate::{coin_balance, hd_wallet, lp_coinfind_registered, BalanceError, BalanceResult, CoinBalance,
            CoinWithDerivationMethod, DerivationMethod, MarketCoinOps, MmCoinEnum};
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

use super::tron::balance::coin_balance_units_of;
use super::tron::hd::TronUsedAddressProbe;
use super::tron::TronAddress;
use super::{checksum_address, Erc20TokenInfo, EthCoin, EthCoinType};

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
        index: NewAccountIndex,
    ) -> MmResult<HDAccountMut<'a, Self::HDAccount>, NewAccountCreatingError>
    where
        XPubExtractor: HDXPubExtractor + Sync,
    {
        create_new_account(self, hd_wallet, xpub_extractor, index).await
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
    index: NewAccountIndex,
) -> MmResult<HDAccountMut<'a, EthHDAccount>, NewAccountCreatingError>
where
    XPubExtractor: HDXPubExtractor + Sync,
{
    // The index is chosen, and an out-of-range or taken one refused, before any key is derived or any device is
    // asked (CRD ch.05 R35c item 1).
    let new_account_id = {
        let accounts = hd_wallet.accounts.lock().await;
        choose_new_account_id(&*accounts, index)?
    };

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
        // The index was free when it was chosen: the account was created while its key was being derived.
        return MmError::err(NewAccountCreatingError::AccountAlreadyExists {
            account_id: new_account_id,
        });
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

    #[cfg_attr(any(target_arch = "wasm32", target_os = "ios"), allow(unused_variables))]
    async fn extract_extended_pubkey<XPubExtractor>(
        &self,
        xpub_extractor: Option<&XPubExtractor>,
        derivation_path: DerivationPath,
    ) -> MmResult<Self::ExtendedPublicKey, HDExtractPubkeyError>
    where
        XPubExtractor: HDXPubExtractor + Sync,
    {
        // A hardware-wallet coin takes the account key from the device; deriving it from the software seed
        // would bind the account to a key the device does not control.
        #[cfg(all(not(target_arch = "wasm32"), not(target_os = "ios")))]
        if let super::EthSigner::Trezor(signer) = &self.signer {
            let xpub_extractor = xpub_extractor.or_mm_err(|| HDExtractPubkeyError::HwContextNotInitialized)?;
            // CRD ch.05 R58a: the device that would answer is the one the coin was activated with.
            crate::hd_pubkey::require_extractor_device(Some(&signer.hw_ctx), xpub_extractor)?;
            let xpub = xpub_extractor.extract_eth_xpub(derivation_path).await?;
            return Secp256k1ExtendedPublicKey::from_str(&xpub)
                .map_to_mm(|e| HDExtractPubkeyError::Internal(format!("Invalid device EVM xpub: {}", e)));
        }
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
        // A token asks the HD wallet of its platform (CRD ch.21 R21.10B.4, ch.49 R49.32). The address is written
        // in the coin's own form (R49.33); the common implementation would abbreviate a 20-byte address.
        let serving = self.hd_serving_coin().await.map_to_mm(HDWalletRpcError::Internal)?;
        let mut response = hd_get_new_address_rpc(&serving, params).await?;
        self.add_tokens_to_platform_entries(std::slice::from_mut(&mut response.new_address))
            .await
            .mm_err(HDWalletRpcError::from)?;
        self.keep_own_ticker_if_trc20(&mut response.new_address.balance);
        self.narrow_to_erc20_ticker(std::slice::from_mut(&mut response.new_address))
            .await
            .mm_err(HDWalletRpcError::from)?;
        Ok(response)
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
    /// The used-address rule of a TRON platform (CRD ch.21 R21.10B.4), which asks the TRON node and not the
    /// placeholder provider.
    tron: Option<TronUsedAddressProbe>,
    /// The used-address rule of an ERC-20 ticker (CRD ch.49 R49.32): the token's balance stands in for the
    /// platform balance.
    token: Option<(EthCoin, Erc20TokenInfo)>,
    /// The used-address rule and the reported maps of an EVM platform ticker's scan (CRD ch.49 R49.32 items 2, 3): the
    /// platform and the tokens activated on it when the scan was created. An address is also used when one of
    /// these tokens has a balance at it, a used address reports the platform and every one of them, and an unused
    /// address between the known range and a used one reports an empty map. `None` for the activation-time scan, which
    /// keeps its own rule and reports (the activation report adds the tokens afterwards).
    platform_tokens: Option<(EthCoin, Vec<(String, Erc20TokenInfo)>)>,
}

#[async_trait]
impl HDAddressBalanceScanner for EthAddressScanner {
    type Address = Address;

    async fn is_address_used(&self, address: &Address) -> BalanceResult<bool> {
        if let Some(tron) = &self.tron {
            return tron.is_used(address).await;
        }
        // An address is considered "used" if its nonce > 0 or balance > 0.
        let alloy_addr = AlloyAddress::from(address.0);
        let nonce = assert_send_future(self.provider.get_transaction_count(alloy_addr).into_future())
            .await
            .map_err(|e| BalanceError::Transport(format!("{}", e)))?;
        if nonce != 0 {
            return Ok(true);
        }
        if let Some((token, info)) = &self.token {
            let balance = token.erc20_balance_of(info, *address).await?;
            let zero = BigDecimal::from(0);
            return Ok(balance.spendable != zero || balance.unspendable != zero);
        }
        let balance = assert_send_future(self.provider.get_balance(alloy_addr).into_future())
            .await
            .map_err(|e| BalanceError::Transport(format!("{}", e)))?;
        if !balance.is_zero() {
            return Ok(true);
        }
        if let Some((platform, tokens)) = &self.platform_tokens {
            let zero = BigDecimal::from(0);
            for (_, info) in tokens {
                let balance = platform.erc20_balance_of(info, *address).await?;
                if balance.spendable != zero || balance.unspendable != zero {
                    return Ok(true);
                }
            }
        }
        Ok(false)
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
        // The tokens enabled at this moment count for the used-address rule of a TRON platform.
        let tron = match (&self.tron_api, &self.coin_type) {
            (Some(api), EthCoinType::Tron) => Some(TronUsedAddressProbe::new(
                api.clone(),
                self.get_erc20_tokens_infos().into_values().collect(),
            )),
            _ => None,
        };
        Ok(EthAddressScanner {
            provider: self.alloy_provider(),
            tron,
            token: None,
            platform_tokens: None,
        })
    }

    async fn enable_hd_wallet<XPubExtractor>(
        &self,
        hd_wallet: &Self::HDWallet,
        xpub_extractor: Option<&XPubExtractor>,
        scan_policy: EnableCoinScanPolicy,
        min_addresses_number: u32,
        path_to_address: Option<HDAddressId>,
    ) -> MmResult<HDWalletBalance, EnableCoinBalanceError>
    where
        XPubExtractor: HDXPubExtractor + Sync,
    {
        coin_balance::common_impl::enable_hd_wallet(
            self,
            hd_wallet,
            xpub_extractor,
            scan_policy,
            min_addresses_number,
            path_to_address,
        )
        .await
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

    /// Overrides the trait default only in how an address is rendered: the default uses the
    /// `Display` of the address, which abbreviates a 20-byte EVM address, while the wire form of an
    /// EVM address is the full EIP-55 checksummed string (CRD R35.1.3) and that of a TRON address is
    /// base58check (CRD ch.21 R21.10B.4). A TRON platform also reports the balance of each enabled token.
    async fn known_addresses_balances_with_ids<Ids>(
        &self,
        hd_account: &Self::HDAccount,
        chain: Bip44Chain,
        address_ids: Ids,
    ) -> BalanceResult<Vec<HDAddressBalance>>
    where
        Self: MarketCoinOps,
        Self::Address: std::fmt::Display + Clone,
        Ids: Iterator<Item = u32> + Send,
    {
        let mut derived = Vec::new();
        for address_id in address_ids {
            let HDAddress {
                address,
                derivation_path,
                ..
            } = self.derive_address(hd_account, chain, address_id).mm_err(Into::into)?;
            derived.push((address, derivation_path));
        }

        let addresses = derived.iter().map(|(address, _)| *address).collect();
        let balances = self.known_addresses_balances(addresses).await?;
        // [`HDWalletBalanceOps::known_addresses_balances`] returns pairs in the order they were requested.
        let mut reported = Vec::with_capacity(derived.len());
        for ((address, balance), (_, derivation_path)) in balances.into_iter().zip(derived) {
            let balance = self.hd_balance_map(&address, balance).await?;
            reported.push(self.hd_address_balance(&address, derivation_path, chain, balance).await);
        }
        Ok(reported)
    }

    async fn all_known_addresses_balances(&self, hd_account: &Self::HDAccount) -> BalanceResult<Vec<HDAddressBalance>> {
        let external_ids = 0..hd_account.external_addresses_number;
        self.known_addresses_balances_with_ids(hd_account, Bip44Chain::External, external_ids)
            .await
    }

    async fn known_address_balance(&self, address: &Self::Address) -> BalanceResult<CoinBalance> {
        // A TRON coin reads the TRON node: TRX at 6 decimals, a TRC20 token through `balanceOf` (CRD ch.21 R-WP1,
        // R-WP3).
        if self.tron_api.is_some() {
            let units = coin_balance_units_of(self, *address).await?;
            let spendable = u256_to_big_decimal(units, self.decimals)?;
            return Ok(CoinBalance {
                spendable,
                unspendable: BigDecimal::from(0),
            });
        }
        // The balance of a token coin is the `balanceOf` of its contract, not the native balance.
        if let Some(info) = self.erc20_token_info() {
            return self.erc20_balance_of(&info, *address).await;
        }
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
                    balances.push(
                        coin.hd_address_balance(
                            &empty_address.address,
                            empty_address.derivation_path,
                            chain,
                            if address_scanner.platform_tokens.is_some() {
                                CoinBalanceMap::default()
                            } else {
                                coin.hd_empty_balance_map()
                            },
                        )
                        .await,
                    );
                }
                let mut balance = coin.hd_balance_map(&checking_address, non_empty_balance).await?;
                if let Some((_, tokens)) = &address_scanner.platform_tokens {
                    coin.add_tokens_to_hd_balance_map(tokens, &checking_address, &mut balance)
                        .await?;
                }
                balances.push(
                    coin.hd_address_balance(&checking_address, checking_address_der_path, chain, balance)
                        .await,
                );
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
        // A TRC20 token asks the HD wallet of its platform (CRD ch.21 R21.10B.4); the page balance of the answer
        // is then the token's own, and each balance map holds the token's ticker only.
        let serving = self
            .hd_serving_coin()
            .await
            .map_to_mm(HDAccountBalanceRpcError::Internal)?;
        let mut response = account_balance::common_impl::account_balance_rpc(&serving, params).await?;
        self.add_tokens_to_platform_entries(&mut response.addresses)
            .await
            .mm_err(HDAccountBalanceRpcError::from)?;
        self.narrow_to_erc20_ticker(&mut response.addresses)
            .await
            .mm_err(HDAccountBalanceRpcError::from)?;
        if matches!(self.coin_type, EthCoinType::Trc20 { .. } | EthCoinType::Erc20 { .. }) {
            response.page_balance = response
                .addresses
                .iter()
                .fold(CoinBalance::default(), |total, address| {
                    total + address.balance.get(self.ticker()).cloned().unwrap_or_default()
                });
        }
        for address in response.addresses.iter_mut() {
            self.keep_own_ticker_if_trc20(&mut address.balance);
        }
        Ok(response)
    }
}

// -------------------------------------------------------------------
// InitCreateHDAccountRpcOps for EthCoin
// -------------------------------------------------------------------

use crate::coin_balance::HDAccountBalance;
use crate::rpc_command::init_account_balance::{self as init_account_balance_mod, InitAccountBalanceParams,
                                               InitAccountBalanceRpcOps};
use crate::rpc_command::init_create_account::{self, CreateNewAccountParams, CreatedAccount, InitCreateHDAccountRpcOps};

#[async_trait]
impl InitAccountBalanceRpcOps for EthCoin {
    async fn init_account_balance_rpc(
        &self,
        params: InitAccountBalanceParams,
    ) -> MmResult<HDAccountBalance, HDAccountBalanceRpcError> {
        // A TRC20 token asks the HD wallet of its platform (CRD ch.21 R21.10B.4). For the platform's ticker the
        // balance maps cover the platform and its enabled tokens; for a token's ticker, that token only.
        let serving = self
            .hd_serving_coin()
            .await
            .map_to_mm(HDAccountBalanceRpcError::Internal)?;
        let mut response = init_account_balance_mod::common_impl::init_account_balance_rpc(&serving, params).await?;
        self.keep_own_ticker_if_trc20(&mut response.total_balance);
        if self.is_evm_platform_coin() {
            self.add_tokens_to_platform_entries(&mut response.addresses)
                .await
                .mm_err(HDAccountBalanceRpcError::from)?;
            response.total_balance = crate::coin_balance::sum_hd_address_balances(self.ticker(), &response.addresses);
        }
        if self.erc20_ticker_info().is_some() {
            self.narrow_to_erc20_ticker(&mut response.addresses)
                .await
                .mm_err(HDAccountBalanceRpcError::from)?;
            response.total_balance = crate::coin_balance::sum_hd_address_balances(self.ticker(), &response.addresses);
        }
        for address in response.addresses.iter_mut() {
            self.keep_own_ticker_if_trc20(&mut address.balance);
        }
        Ok(response)
    }
}

#[async_trait]
impl InitCreateHDAccountRpcOps for EthCoin {
    async fn init_create_account_rpc<XPubExtractor>(
        &self,
        params: CreateNewAccountParams,
        xpub_extractor: Option<&XPubExtractor>,
        created_account: &CreatedAccount,
    ) -> MmResult<HDAccountBalance, HDWalletRpcError>
    where
        XPubExtractor: HDXPubExtractor + Sync,
    {
        init_create_account::common_impl::init_create_new_account_rpc(self, params, xpub_extractor, created_account)
            .await
    }
}

// -------------------------------------------------------------------
// get_new_address in the coin's address form, and the scan task (CRD ch.21 R21.10B.4, ch.49 R49.33)
// -------------------------------------------------------------------

/// `get_new_address` of an HD platform: the common derivation, with the address in the coin's form (base58check for
/// TRON, the EIP-55 string for an EVM coin) and the balance map of [`EthCoin::hd_balance_map`]. The common
/// implementation renders an address with its `Display`, which abbreviates a 20-byte address.
async fn hd_get_new_address_rpc(
    coin: &EthCoin,
    params: GetNewHDAddressParams,
) -> MmResult<GetNewHDAddressResponse, HDWalletRpcError> {
    use std::ops::DerefMut;

    let (account_id, chain) = (params.account_id, params.chain);
    let hd_wallet = coin.derivation_method().hd_wallet_or_err().mm_err(Into::into)?;
    let mut hd_account = hd_wallet
        .get_account_mut(account_id)
        .await
        .or_mm_err(|| HDWalletRpcError::UnknownAccount { account_id })?;
    let HDAddress {
        address,
        derivation_path,
        ..
    } = coin
        .generate_new_address(hd_wallet, hd_account.deref_mut(), chain)
        .await
        .mm_err(Into::into)?;
    let balance = coin.known_address_balance(&address).await.mm_err(Into::into)?;
    let balance = coin.hd_balance_map(&address, balance).await.mm_err(Into::into)?;
    Ok(GetNewHDAddressResponse {
        new_address: coin.hd_address_balance(&address, derivation_path, chain, balance).await,
    })
}

use crate::rpc_command::init_scan_for_new_addresses::{self, InitScanAddressesRpcOps, ScanAddressesParams,
                                                      ScanAddressesResponse};

/// `task::scan_for_new_addresses::init` of an HD platform and of a token ticker (CRD ch.21 R21.10B.4, ch.49
/// R49.32). A token ticker is served from its platform's wallet and counts an address as used by its nonce or that
/// token's balance.
#[async_trait]
impl InitScanAddressesRpcOps for EthCoin {
    async fn init_scan_for_new_addresses_rpc(
        &self,
        params: ScanAddressesParams,
    ) -> MmResult<ScanAddressesResponse, HDAccountBalanceRpcError> {
        let serving = self
            .hd_serving_coin()
            .await
            .map_to_mm(HDAccountBalanceRpcError::Internal)?;
        let mut response = match self.erc20_ticker_info() {
            Some(info) => scan_for_erc20_ticker(self, info, &serving, params).await?,
            None if self.is_evm_platform_coin() => scan_for_evm_platform_ticker(&serving, params).await?,
            None => init_scan_for_new_addresses::common_impl::scan_for_new_addresses_rpc(&serving, params).await?,
        };
        for address in response.new_addresses.iter_mut() {
            self.keep_own_ticker_if_trc20(&mut address.balance);
        }
        Ok(response)
    }
}

/// The scan of an ERC-20 ticker: the platform's wallet is scanned with an address counted as used by its nonce or the
/// token's balance, and each entry then reports that token only.
async fn scan_for_erc20_ticker(
    token: &EthCoin,
    info: Erc20TokenInfo,
    platform: &EthCoin,
    params: ScanAddressesParams,
) -> MmResult<ScanAddressesResponse, HDAccountBalanceRpcError> {
    let address_scanner = EthAddressScanner {
        provider: platform.alloy_provider(),
        tron: None,
        token: Some((token.clone(), info)),
        platform_tokens: None,
    };
    let mut response = scan_with(platform, &address_scanner, params).await?;
    token
        .narrow_to_erc20_ticker(&mut response.new_addresses)
        .await
        .mm_err(HDAccountBalanceRpcError::from)?;
    Ok(response)
}

/// The scan of an EVM platform ticker (CRD ch.49 R49.32 items 2, 3): an address is used by its nonce, its platform
/// balance or the balance of a token activated now, and the entries report as the scanner says.
async fn scan_for_evm_platform_ticker(
    platform: &EthCoin,
    params: ScanAddressesParams,
) -> MmResult<ScanAddressesResponse, HDAccountBalanceRpcError> {
    let address_scanner = EthAddressScanner {
        provider: platform.alloy_provider(),
        tron: None,
        token: None,
        platform_tokens: Some((
            platform.clone(),
            platform.get_erc20_tokens_infos().into_iter().collect(),
        )),
    };
    scan_with(platform, &address_scanner, params).await
}

/// Scans the platform's wallet with `address_scanner`.
async fn scan_with(
    platform: &EthCoin,
    address_scanner: &EthAddressScanner,
    params: ScanAddressesParams,
) -> MmResult<ScanAddressesResponse, HDAccountBalanceRpcError> {
    use std::ops::DerefMut;

    let hd_wallet = platform.derivation_method().hd_wallet_or_err().mm_err(Into::into)?;
    let account_id = params.account_index;
    let mut hd_account = hd_wallet
        .get_account_mut(account_id)
        .await
        .or_mm_err(|| HDAccountBalanceRpcError::CoinIsActivatedNotWithHDWallet)?;
    let account_derivation_path = hd_account.account_derivation_path();
    let gap_limit = params.gap_limit.unwrap_or_else(|| hd_wallet.gap_limit());
    let new_addresses = platform
        .scan_for_new_addresses(hd_wallet, hd_account.deref_mut(), address_scanner, gap_limit)
        .await
        .mm_err(Into::into)?;
    Ok(ScanAddressesResponse {
        account_index: account_id,
        derivation_path: RpcDerivationPath(account_derivation_path),
        new_addresses,
    })
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

// -------------------------------------------------------------------
// ERC-20 balances of an HD wallet report (CRD R35.1.3(b), R35.3A.5)
// -------------------------------------------------------------------

impl EthCoin {
    /// How an address of this coin's HD wallet is rendered in a report: base58check for a TRON coin (CRD ch.21
    /// R21.10B.4), the EIP-55 checksummed string for an EVM coin (CRD R35.1.3).
    pub(crate) fn hd_address_string(&self, address: &Address) -> String {
        if self.is_tron_family() {
            TronAddress::from_evm_address(*address).to_base58()
        } else {
            checksum_address(&format!("{:#x}", address))
        }
    }

    /// One address entry of an HD wallet report of this coin. Every HD report of the EVM family is built from the
    /// entries this function makes (`get_new_address`, `account_balance`, the task forms of the account balance
    /// and of the scan for new addresses, and the HD results of the platform and token activations), so the GasFree
    /// custody address is attached here and nowhere else (CRD ch.63 §63.3.3, R18, and the finding in §63.10). The
    /// field is `None` for a coin that is not a TRON coin with a provider binding
    /// ([`Self::gasfree_report_address`]).
    pub(crate) async fn hd_address_balance(
        &self,
        address: &Address,
        derivation_path: DerivationPath,
        chain: Bip44Chain,
        balance: CoinBalanceMap,
    ) -> HDAddressBalance {
        HDAddressBalance {
            address: self.hd_address_string(address),
            gasfree_address: self.gasfree_report_address(address).await,
            derivation_path: RpcDerivationPath(derivation_path),
            chain,
            balance,
        }
    }

    /// Every address of this coin's wallet that is known now: the enabled address of a single-key coin, and for an
    /// HD wallet the known (activated) external addresses of every account. No node is asked. The TRON
    /// transaction history lists each of them (CRD ch.21 §21.10C).
    ///
    /// # Errors
    ///
    /// The text of why an address of the HD wallet cannot be derived.
    pub(crate) async fn known_wallet_addresses(&self) -> Result<Vec<Address>, String> {
        let hd_wallet = match &self.derivation_method {
            DerivationMethod::HDWallet(hd_wallet) => hd_wallet,
            DerivationMethod::Iguana(address) => return Ok(vec![*address]),
        };
        let accounts = hd_wallet.accounts.lock().await;
        let mut addresses = Vec::new();
        for account in accounts.values() {
            for address_id in 0..account.external_addresses_number {
                let hd_address = self
                    .derive_address(account, Bip44Chain::External, address_id)
                    .map_err(|e| e.into_inner().to_string())?;
                addresses.push(hd_address.address);
            }
        }
        if !addresses.contains(&self.my_address) {
            addresses.push(self.my_address);
        }
        Ok(addresses)
    }

    /// The balance map of one address of an HD wallet report: the coin's own ticker, and for a TRON platform also
    /// every enabled TRC20 token (CRD ch.21 R21.10B.4). An EVM coin reports its own ticker only; its tokens are
    /// added by [`Self::add_erc20_balances_to_hd_report`].
    pub(crate) async fn hd_balance_map(
        &self,
        address: &Address,
        platform_balance: CoinBalance,
    ) -> BalanceResult<CoinBalanceMap> {
        let mut map = coin_balance_map_for_ticker(self.ticker(), platform_balance);
        if self.is_tron_platform_coin() {
            for (ticker, info) in self.get_erc20_tokens_infos() {
                let balance = self.erc20_balance_of(&info, *address).await?;
                map.insert(ticker, balance);
            }
        }
        Ok(map)
    }

    /// Adds the balance of each of `tokens` at `address` to `map` (CRD ch.49 R49.32 item 1). A failing query fails
    /// the whole request, so no partial map is produced.
    async fn add_tokens_to_hd_balance_map(
        &self,
        tokens: &[(String, Erc20TokenInfo)],
        address: &Address,
        map: &mut CoinBalanceMap,
    ) -> BalanceResult<()> {
        for (ticker, info) in tokens {
            let balance = self.erc20_balance_of(info, *address).await?;
            map.insert(ticker.clone(), balance);
        }
        Ok(())
    }

    /// Adds every ERC-20 token activated on this EVM platform now to the balance map of each entry, when the method
    /// was asked for the platform ticker (CRD ch.49 R49.32 item 1). Any other coin leaves the entries as they are: a
    /// token ticker answers its own token only, and a TRON platform already holds its tokens.
    async fn add_tokens_to_platform_entries(&self, entries: &mut [HDAddressBalance]) -> BalanceResult<()> {
        if !self.is_evm_platform_coin() {
            return Ok(());
        }
        let tokens: Vec<_> = self.get_erc20_tokens_infos().into_iter().collect();
        if tokens.is_empty() {
            return Ok(());
        }
        for entry in entries.iter_mut() {
            let address = super::addr_from_str(&entry.address).map_to_mm(BalanceError::Internal)?;
            self.add_tokens_to_hd_balance_map(&tokens, &address, &mut entry.balance)
                .await?;
        }
        Ok(())
    }

    /// The balance map of an address known to hold nothing: zero for each ticker [`Self::hd_balance_map`] reports.
    fn hd_empty_balance_map(&self) -> CoinBalanceMap {
        let mut map = coin_balance_map_for_ticker(self.ticker(), CoinBalance::default());
        if self.is_tron_platform_coin() {
            for ticker in self.get_erc20_tokens_infos().into_keys() {
                map.insert(ticker, CoinBalance::default());
            }
        }
        map
    }

    /// Narrows a balance map of an HD method's answer to this coin's own ticker when the method was asked for a
    /// TRC20 ticker, as for an ERC-20 ticker (CRD ch.21 status note of 2026-11-19, ch.49 R49.32). The platform's
    /// wallet serves the method ([`Self::hd_serving_coin`]) and reports the platform and every enabled token; a
    /// TRC20 ticker answers that token only. A map asked for the TRX ticker is left whole.
    fn keep_own_ticker_if_trc20(&self, map: &mut CoinBalanceMap) {
        if matches!(self.coin_type, EthCoinType::Trc20 { .. }) {
            let ticker = self.ticker();
            map.retain(|reported, _| reported == ticker);
        }
    }

    /// The token this coin is when it is an ERC-20 token: the HD methods asked of it answer that token only.
    fn erc20_ticker_info(&self) -> Option<Erc20TokenInfo> {
        match self.coin_type {
            EthCoinType::Erc20 { .. } => self.erc20_token_info(),
            _ => None,
        }
    }

    /// Replaces the balance map of each entry of an HD method's answer with this token's own balance when the method
    /// was asked for an ERC-20 ticker (CRD ch.49 R49.32, last paragraph). The platform's wallet serves the method
    /// and reports the platform ticker; an ERC-20 ticker answers its `balanceOf` only, and leaves any other coin's
    /// answer as it is.
    async fn narrow_to_erc20_ticker(&self, entries: &mut [HDAddressBalance]) -> BalanceResult<()> {
        let Some(info) = self.erc20_ticker_info() else {
            return Ok(());
        };
        for entry in entries.iter_mut() {
            let address = super::addr_from_str(&entry.address).map_to_mm(BalanceError::Internal)?;
            let balance = self.erc20_balance_of(&info, address).await?;
            entry.balance = coin_balance_map_for_ticker(self.ticker(), balance);
        }
        Ok(())
    }

    /// The coin whose HD wallet serves the HD RPC methods asked of this coin: the platform for a token (a token
    /// coin carries the enabled address only, CRD ch.21 R21.10B.4, ch.49 R49.34), this coin otherwise.
    ///
    /// The token's own look-up of its platform finds a passive platform (CRD ch.43 D43.1F.3 (b)).
    ///
    /// # Errors
    ///
    /// The text of why the platform of a token cannot be found.
    pub(crate) async fn hd_serving_coin(&self) -> Result<EthCoin, String> {
        let (EthCoinType::Trc20 { platform, .. } | EthCoinType::Erc20 { platform, .. }) = &self.coin_type else {
            return Ok(self.clone());
        };
        let ctx = MmArc::from_weak(&self.ctx).ok_or_else(|| "MM context has been dropped".to_owned())?;
        match lp_coinfind_registered(&ctx, platform).await? {
            Some(MmCoinEnum::EthCoin(platform_coin)) => Ok(platform_coin),
            _ => Err(format!(
                "The platform coin {platform} of {} is not active",
                self.ticker()
            )),
        }
    }

    /// Adds the balance of every ERC-20 token registered on this platform coin to each address of an HD wallet
    /// report, and recomputes the per-account totals (CRD R35.1.3(b): each balance map covers the platform
    /// ticker and every activated token ticker). A TRON platform's reports carry its tokens already
    /// ([`Self::hd_balance_map`]).
    pub async fn add_erc20_balances_to_hd_report(&self, report: &mut HDWalletBalance) -> BalanceResult<()> {
        if self.is_tron_platform_coin() {
            return Ok(());
        }
        let tokens = self.get_erc20_tokens_infos();
        if tokens.is_empty() {
            return Ok(());
        }
        for account in report.accounts.iter_mut() {
            for address in account.addresses.iter_mut() {
                let owner = super::addr_from_str(&address.address).map_to_mm(BalanceError::Internal)?;
                for (ticker, info) in tokens.iter() {
                    let balance = self.erc20_balance_of(info, owner).await?;
                    address.balance.insert(ticker.clone(), balance);
                }
            }
            account.total_balance = crate::coin_balance::sum_hd_address_balances(self.ticker(), &account.addresses);
        }
        Ok(())
    }

    /// The HD wallet report of one ERC-20 token over the known addresses of this platform coin's HD wallet:
    /// every balance map holds the token ticker only (CRD R35.3A.5).
    pub async fn erc20_hd_wallet_balance(
        &self,
        token_ticker: &str,
        info: &Erc20TokenInfo,
    ) -> BalanceResult<HDWalletBalance> {
        let hd_wallet = match &self.derivation_method {
            DerivationMethod::HDWallet(hd_wallet) => hd_wallet,
            DerivationMethod::Iguana(_) => {
                return MmError::err(BalanceError::UnexpectedDerivationMethod(
                    crate::UnexpectedDerivationMethod::HDWalletUnavailable,
                ))
            },
        };
        let accounts = hd_wallet.accounts.lock().await;
        let mut report = HDWalletBalance {
            accounts: Vec::with_capacity(accounts.len()),
        };
        for account in accounts.values() {
            let mut addresses = Vec::with_capacity(account.external_addresses_number as usize);
            for address_id in 0..account.external_addresses_number {
                let hd_address = self
                    .derive_address(account, Bip44Chain::External, address_id)
                    .mm_err(Into::into)?;
                let balance = self.erc20_balance_of(info, hd_address.address).await?;
                addresses.push(
                    self.hd_address_balance(
                        &hd_address.address,
                        hd_address.derivation_path,
                        Bip44Chain::External,
                        coin_balance_map_for_ticker(token_ticker, balance),
                    )
                    .await,
                );
            }
            report.accounts.push(HDAccountBalance {
                account_index: account.account_id,
                derivation_path: RpcDerivationPath(account.account_derivation_path()),
                total_balance: crate::coin_balance::sum_hd_address_balances(token_ticker, &addresses),
                addresses,
            });
        }
        Ok(report)
    }
}
