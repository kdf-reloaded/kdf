// utxo_common_hd — HD wallet derivation, scanning, account management

use super::*;

pub fn derive_address<T: UtxoCommonOps>(
    coin: &T,
    hd_account: &UtxoHDAccount,
    chain: Bip44Chain,
    address_id: u32,
) -> MmResult<HDAddress<Address, Public>, AddressDerivingError> {
    let change_child = chain.to_child_number();
    let address_id_child = ChildNumber::from(address_id);

    let derived_pubkey = hd_account
        .extended_pubkey
        .derive_child(change_child)?
        .derive_child(address_id_child)?;
    let address = coin.address_from_extended_pubkey(&derived_pubkey);
    let pubkey = Public::Compressed(H264::from(derived_pubkey.public_key().serialize()));

    let mut derivation_path = hd_account.account_derivation_path.to_derivation_path();
    derivation_path.push(change_child);
    derivation_path.push(address_id_child);
    Ok(HDAddress {
        address,
        pubkey,
        derivation_path,
    })
}

pub async fn create_new_account<'a, Coin, XPubExtractor>(
    coin: &Coin,
    hd_wallet: &'a UtxoHDWallet,
    xpub_extractor: Option<&XPubExtractor>,
) -> MmResult<HDAccountMut<'a, UtxoHDAccount>, NewAccountCreatingError>
where
    Coin: ExtractExtendedPubkey<ExtendedPublicKey = Secp256k1ExtendedPublicKey>
        + HDWalletCoinWithStorageOps<HDWallet = UtxoHDWallet, HDAccount = UtxoHDAccount>
        + Sync,
    XPubExtractor: HDXPubExtractor + Sync,
{
    const INIT_ACCOUNT_ID: u32 = 0;
    let new_account_id = hd_wallet
        .accounts
        .lock()
        .await
        .iter()
        // The last element of the BTreeMap has the max account index.
        .last()
        .map(|(account_id, _account)| *account_id + 1)
        .unwrap_or(INIT_ACCOUNT_ID);
    if new_account_id >= ChildNumber::HARDENED_FLAG {
        return MmError::err(NewAccountCreatingError::AccountLimitReached {
            max_accounts_number: ChildNumber::HARDENED_FLAG,
        });
    }

    let account_child_hardened = true;
    let account_child = ChildNumber::new(new_account_id, account_child_hardened)
        .map_to_mm(|e| NewAccountCreatingError::Internal(e.to_string()))?;

    let account_derivation_path: HDPathToAccount = hd_wallet.derivation_path.derive(account_child)?;
    let account_pubkey = coin
        .extract_extended_pubkey(xpub_extractor, account_derivation_path.to_derivation_path())
        .await
        .mm_err(Into::into)?;

    let new_account = UtxoHDAccount {
        account_id: new_account_id,
        extended_pubkey: account_pubkey,
        account_derivation_path,
        // We don't know how many addresses are used by the user at this moment.
        external_addresses_number: 0,
        internal_addresses_number: 0,
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
        accounts
            .entry(new_account_id)
            // the `entry` method should return [`Entry::Vacant`] due to the checks above
            .or_insert(new_account)
    }))
}

pub async fn set_known_addresses_number<T>(
    coin: &T,
    hd_wallet: &UtxoHDWallet,
    hd_account: &mut UtxoHDAccount,
    chain: Bip44Chain,
    new_known_addresses_number: u32,
) -> MmResult<(), AccountUpdatingError>
where
    T: HDWalletCoinWithStorageOps<HDWallet = UtxoHDWallet, HDAccount = UtxoHDAccount> + Sync,
{
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
            coin.update_internal_addresses_number(hd_wallet, hd_account.account_id, new_known_addresses_number)
                .await
                .mm_err(Into::into)?;
            hd_account.internal_addresses_number = new_known_addresses_number;
        },
    }
    Ok(())
}

pub async fn produce_hd_address_scanner<T>(coin: &T) -> BalanceResult<UtxoAddressScanner>
where
    T: AsRef<UtxoCoinFields>,
{
    Ok(UtxoAddressScanner::init(coin.as_ref().rpc_client.clone())
        .await
        .mm_err(Into::into)?)
}

pub async fn scan_for_new_addresses<T>(
    coin: &T,
    hd_wallet: &T::HDWallet,
    hd_account: &mut T::HDAccount,
    address_scanner: &T::HDAddressScanner,
    gap_limit: u32,
) -> BalanceResult<Vec<HDAddressBalance>>
where
    T: HDWalletBalanceOps + MarketCoinOps + Sync,
    T::Address: std::fmt::Display,
{
    let mut addresses = scan_for_new_addresses_impl(
        coin,
        hd_wallet,
        hd_account,
        address_scanner,
        Bip44Chain::External,
        gap_limit,
    )
    .await?;
    addresses.extend(
        scan_for_new_addresses_impl(
            coin,
            hd_wallet,
            hd_account,
            address_scanner,
            Bip44Chain::Internal,
            gap_limit,
        )
        .await?,
    );

    Ok(addresses)
}

/// Checks addresses that either had empty transaction history last time we checked or has not been checked before.
/// The checking stops at the moment when we find `gap_limit` consecutive empty addresses.
pub async fn scan_for_new_addresses_impl<T>(
    coin: &T,
    hd_wallet: &T::HDWallet,
    hd_account: &mut T::HDAccount,
    address_scanner: &T::HDAddressScanner,
    chain: Bip44Chain,
    gap_limit: u32,
) -> BalanceResult<Vec<HDAddressBalance>>
where
    T: HDWalletBalanceOps + MarketCoinOps + Sync,
    T::Address: std::fmt::Display,
{
    let mut balances = Vec::with_capacity(gap_limit as usize);

    // Get the first unknown address id.
    let mut checking_address_id = hd_account
        .known_addresses_number(chain)
        // A UTXO coin should support both [`Bip44Chain::External`] and [`Bip44Chain::Internal`].
        .mm_err(|e| BalanceError::Internal(e.to_string()))?;

    let mut unused_addresses_counter = 0;
    while checking_address_id < ChildNumber::HARDENED_FLAG && unused_addresses_counter < gap_limit {
        let HDAddress {
            address: checking_address,
            derivation_path: checking_address_der_path,
            ..
        } = coin
            .derive_address(hd_account, chain, checking_address_id)
            .mm_err(Into::into)?;

        match coin.is_address_used(&checking_address, address_scanner).await? {
            // We found a non-empty address, so we have to fill up the balance list
            // with zeros starting from `last_non_empty_address_id = checking_address_id - unused_addresses_counter`.
            AddressBalanceStatus::Used(non_empty_balance) => {
                let last_non_empty_address_id = checking_address_id - unused_addresses_counter;
                for empty_address_id in last_non_empty_address_id..checking_address_id {
                    let empty_address = coin
                        .derive_address(hd_account, chain, empty_address_id)
                        .mm_err(Into::into)?;

                    balances.push(HDAddressBalance {
                        address: empty_address.address.to_string(),
                        derivation_path: RpcDerivationPath(empty_address.derivation_path),
                        chain,
                        balance: coin_balance_map_for_ticker(coin.ticker(), CoinBalance::default()),
                    });
                }

                balances.push(HDAddressBalance {
                    address: checking_address.to_string(),
                    derivation_path: RpcDerivationPath(checking_address_der_path),
                    chain,
                    balance: coin_balance_map_for_ticker(coin.ticker(), non_empty_balance),
                });
                // Reset the counter of unused addresses to zero since we found a non-empty address.
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

pub async fn all_known_addresses_balances<T>(
    coin: &T,
    hd_account: &T::HDAccount,
) -> BalanceResult<Vec<HDAddressBalance>>
where
    T: HDWalletBalanceOps + MarketCoinOps + Sync,
    T::Address: std::fmt::Display + Clone,
{
    let external_addresses = hd_account
        .known_addresses_number(Bip44Chain::External)
        // A UTXO coin should support both [`Bip44Chain::External`] and [`Bip44Chain::Internal`].
        .mm_err(|e| BalanceError::Internal(e.to_string()))?;
    let internal_addresses = hd_account
        .known_addresses_number(Bip44Chain::Internal)
        // A UTXO coin should support both [`Bip44Chain::External`] and [`Bip44Chain::Internal`].
        .mm_err(|e| BalanceError::Internal(e.to_string()))?;

    let mut balances = coin
        .known_addresses_balances_with_ids(hd_account, Bip44Chain::External, 0..external_addresses)
        .await?;
    balances.extend(
        coin.known_addresses_balances_with_ids(hd_account, Bip44Chain::Internal, 0..internal_addresses)
            .await?,
    );

    Ok(balances)
}

pub async fn load_hd_accounts_from_storage(
    hd_wallet_storage: &HDWalletCoinStorage,
    derivation_path: &HDPathToCoin,
) -> HDWalletStorageResult<HDAccountsMap<UtxoHDAccount>> {
    let accounts = hd_wallet_storage.load_all_accounts().await?;
    let res: HDWalletStorageResult<HDAccountsMap<UtxoHDAccount>> = accounts
        .iter()
        .map(|account_info| {
            let account = UtxoHDAccount::try_from_storage_item(derivation_path, account_info)?;
            Ok((account.account_id, account))
        })
        .collect();
    match res {
        Ok(accounts) => Ok(accounts),
        Err(e) if e.get_inner().is_deserializing_err() => {
            warn!("Error loading HD accounts from the storage: '{}'. Clear accounts", e);
            hd_wallet_storage.clear_accounts().await?;
            Ok(HDAccountsMap::new())
        },
        Err(e) => Err(e),
    }
}

pub async fn extract_extended_pubkey<T, XPubExtractor>(
    coin: &T,
    xpub_extractor: Option<&XPubExtractor>,
    derivation_path: DerivationPath,
) -> MmResult<Secp256k1ExtendedPublicKey, HDExtractPubkeyError>
where
    T: AsRef<UtxoCoinFields>,
    XPubExtractor: HDXPubExtractor,
{
    match xpub_extractor {
        // Hardware source: require the coin's `trezor_coin` config and run the device protocol.
        Some(extractor) => {
            let trezor_coin = coin
                .as_ref()
                .conf
                .trezor_coin
                .or_mm_err(|| HDExtractPubkeyError::CoinDoesntSupportTrezor)?;
            let xpub = extractor.extract_utxo_xpub(trezor_coin, derivation_path).await?;
            Secp256k1ExtendedPublicKey::from_str(&xpub).map_to_mm(HDExtractPubkeyError::InvalidXpub)
        },
        // Software global-HD source: derive the account extended pubkey from the in-memory
        // BIP-32 master held by the coin's HD priv-key policy. No `trezor_coin` and no device.
        None => {
            let bip39_secp_priv_key = match &coin.as_ref().priv_key_policy {
                PrivKeyPolicy::HDWallet {
                    bip39_secp_priv_key, ..
                } => bip39_secp_priv_key.clone(),
                _ => return MmError::err(HDExtractPubkeyError::HwContextNotInitialized),
            };
            crypto::derive_secp256k1_extended_pubkey(bip39_secp_priv_key, &derivation_path)
                .mm_err(|e| HDExtractPubkeyError::Internal(format!("BIP32 derivation error: {}", e)))
        },
    }
}

pub async fn get_withdraw_hd_sender<T>(
    coin: &T,
    req: &WithdrawRequest,
    hd_wallet: &T::HDWallet,
) -> MmResult<WithdrawSenderAddress<Address, Public>, WithdrawError>
where
    T: HDWalletCoinOps<Address = Address, Pubkey = Public>,
{
    let HDAddressId {
        account_id,
        chain,
        address_id,
    } = match req.from.clone() {
        Some(from) => match from {
            WithdrawFrom::AddressId(id) => id,
            WithdrawFrom::DerivationPath { derivation_path } => {
                let derivation_path = Bip44DerivationPath::from_str(&derivation_path)
                    .map_to_mm(Bip44DerPathError::from)
                    .mm_err(|e| WithdrawError::UnexpectedFromAddress(e.to_string()))?;
                let coin_type = derivation_path.coin_type();
                let expected_coin_type = hd_wallet.coin_type();
                if coin_type != expected_coin_type {
                    let error = format!(
                        "Derivation path '{}' must has '{}' coin type",
                        derivation_path, expected_coin_type
                    );
                    return MmError::err(WithdrawError::UnexpectedFromAddress(error));
                }
                HDAddressId::from(derivation_path)
            },
        },
        None => {
            // Compatibility divergence from upstream: when `from` is omitted for an HD wallet,
            // default to the single enabled/active address (account 0, External chain, address 0)
            // — the same address used for balance and swaps — so clients that don't send `from`
            // can still preview and withdraw. Upstream rejects an omitted `from`.
            let default_account = hd_wallet
                .get_account(0)
                .await
                .or_mm_err(|| WithdrawError::FromAddressNotFound)?;
            let hd_address = coin
                .derive_address(&default_account, Bip44Chain::External, 0)
                .mm_err(Into::into)?;
            return Ok(WithdrawSenderAddress::from(hd_address));
        },
    };

    let hd_account = hd_wallet
        .get_account(account_id)
        .await
        .or_mm_err(|| WithdrawError::UnknownAccount { account_id })?;
    let hd_address = coin.derive_address(&hd_account, chain, address_id).mm_err(Into::into)?;

    let is_address_activated = hd_account
        .is_address_activated(chain, address_id)
        // If [`HDWalletCoinOps::derive_address`] succeeds, [`HDAccountOps::is_address_activated`] shouldn't fails with an `InvalidBip44ChainError`.
        .mm_err(|e| WithdrawError::InternalError(e.to_string()))?;
    if !is_address_activated {
        let error = format!("'{}' address is not activated", hd_address.address);
        return MmError::err(WithdrawError::UnexpectedFromAddress(error));
    }

    Ok(WithdrawSenderAddress::from(hd_address))
}
