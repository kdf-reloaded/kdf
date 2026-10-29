//! Multi-account HD wallet support for Siacoin (CRD ch.20 §20.10 D1).
//!
//! §20.4.1 dictates SLIP-10 ed25519 derivation under SLIP-44 coin type 1991 and,
//! today, a single derived address per HD account (`SINGLE_ADDRESS_MODE_PATH` in
//! `siacoin_types.rs`, still used unchanged by the existing single-address
//! activation path in `siacoin_helpers.rs`). This module is the real
//! multi-account implementation, now wired into activation and the HD RPC
//! surface (see below): an `HDWalletOps`/`HDAccountOps` pair for Sia
//! (mirroring the trait-conformance shape of `utxo_common/utxo_common_hd.rs`
//! and `eth/eth_hd_wallet.rs`), plus gap-limit address discovery via
//! `HDWalletBalanceOps`.
//!
//! ## Derivation model (why this differs in shape from UTXO/ETH)
//!
//! UTXO and ETH store an *account-level extended **public** key* on their
//! `HDAccount` types and derive further (non-hardened) child public keys from it
//! without touching the private key -- the standard BIP32 xpub pattern. SLIP-10
//! ed25519 (as implemented by the bound `ed25519_dalek_bip32` crate) has no
//! such capability: every child derivation step requires the parent *private*
//! key (`ExtendedSigningKey::derive_child` rejects non-hardened indices
//! outright), so there is no ed25519 analog of an "xpub". Consequently
//! `SiaHDAccount` carries the account-level *private* `ExtendedSigningKey`
//! (`m/44'/1991'/{account}'`) instead of a public key, and `derive_address` is a
//! pure function of that key plus `(chain, address_id)` -- it needs no access to
//! wallet- or coin-level secrets. This is a discretionary consequence of Sia's
//! dictated derivation scheme (§20.4.1 binding-scope note), not a deviation from
//! it: the coin type, the SLIP-10 ed25519 scheme, and the fixed single-address
//! path this module now generalises are all unchanged.
//!
//! Both `Bip44Chain` variants are structurally hardened-derivable, but only
//! `External` is wired here (`known_addresses_number`/`derive_address` reject
//! `Internal`), matching `eth_hd_wallet.rs`'s own choice for the same reason:
//! Sia's KDF-side wallet model, like ETH's, has no dictated change/internal
//! address convention (§20.4.1 fixes only the single-address-mode path, not a
//! `Bip44Chain::Internal` role for Sia).
//!
//! ## Wired into activation (CRD ch.20 D1 activation-wiring follow-up)
//!
//! `SiaCoinGeneric` now carries a `derivation_method: Arc<DerivationMethod<Address,
//! SiaHDWallet>>` field (`mod.rs`), and `SiaCoinBuilder`/`SiaCoin::new`
//! (`siacoin_helpers.rs`) construct `DerivationMethod::HDWallet(wallet)` -- with
//! account 0 *not* pre-created there; see `CoinWithDerivationMethod`/the HD RPC
//! surface below -- whenever the priv-key policy is `GlobalHDAccount`, and
//! `DerivationMethod::Iguana(address)` for the legacy single-key policy. This
//! module now also implements `CoinWithDerivationMethod`, `HDWalletRpcOps`,
//! `AccountBalanceRpcOps`, and `InitAccountBalanceRpcOps` for `SiaCoin` (pure
//! delegation to the shared `common_impl` helpers, mirroring `eth_hd_wallet.rs`).
//! `InitCreateHDAccountRpcOps` is implemented too, but *not* via the shared
//! delegation pattern -- see its own doc comment for why.
//!
//! The existing single-address HD activation path (§20.4.1) is observably
//! unaffected: `priv_key_policy` still resolves to the exact same
//! `PrivKeyPolicy::KeyPair` built from `SINGLE_ADDRESS_MODE_PATH` as before (the
//! same key `derive_address`'s account-0/External/address-0 output is proven,
//! by this module's own test below, to equal), so every existing signing call
//! site (`my_keypair()`, swap ops, withdraw) is untouched. What changed is that
//! the coin *also* now reports `DerivationMethod::HDWallet` and can serve
//! read-only multi-account/multi-address balance discovery and account creation
//! through the RPC surface above -- spending remains scoped to the single
//! activated key (ch.20 R-T3). See the coder pass report for the full
//! reasoning and the scope this pass deliberately left open.

use super::*;

use crate::coin_balance::{self, coin_balance_map_for_ticker, AddressBalanceStatus, EnableCoinBalanceError,
                          EnableCoinScanPolicy, HDAccountBalance, HDAddressBalance, HDAddressBalanceScanner,
                          HDWalletBalance, HDWalletBalanceOps};
use crate::hd_pubkey::HDXPubExtractor;
use crate::hd_wallet::{self, AccountUpdatingError, AddressDerivingError, AsyncMutexGuard, GetNewHDAddressParams,
                       GetNewHDAddressResponse, HDAccountMut, HDAccountOps, HDAccountsMap, HDAccountsMutex, HDAddress,
                       HDWalletCoinOps, HDWalletOps, HDWalletRpcError, HDWalletRpcOps, InvalidBip44ChainError,
                       NewAccountCreatingError};
use crate::rpc_command::account_balance::{self, AccountBalanceParams, AccountBalanceRpcOps, HDAccountBalanceResponse};
use crate::rpc_command::hd_account_balance_rpc_error::HDAccountBalanceRpcError;
use crate::rpc_command::init_account_balance::{self as init_account_balance_mod, InitAccountBalanceParams,
                                               InitAccountBalanceRpcOps};
use crate::rpc_command::init_create_account::{CreateNewAccountParams, InitCreateHDAccountRpcOps};
use crate::BalanceResult;
use crypto::{Bip44Chain, ChildNumber, DerivationPath as CryptoDerivationPath, GlobalHDAccountArc, RpcDerivationPath};
use ed25519_dalek_bip32::{ChildIndex, ExtendedSigningKey};

/// Sia's SLIP-44 registered coin type (§20.4.1, dictated).
pub(crate) const SIA_COIN_TYPE: u32 = 1991;

lazy_static! {
    /// The `m/44'/1991'` node every [`SiaHDWallet`] account is derived from.
    static ref SIA_HD_ROOT_PATH: DalekDerivationPath =
        DalekDerivationPath::from_str("m/44'/1991'").expect("valid Sia HD root path");
}

// ─── HD account ──────────────────────────────────────────────────────

/// One activated Siacoin HD account: `m/44'/1991'/{account_id}'`.
///
/// Carries the account-level *private* extended signing key rather than a
/// public key -- see the module doc comment for why ed25519 SLIP-10 leaves no
/// other option.
pub struct SiaHDAccount {
    account_id: u32,
    account_signing_key: ExtendedSigningKey,
    /// Number of derived (External-chain) addresses considered "known" --
    /// same accounting convention as `UtxoHDAccount`/`EthHDAccount`.
    external_addresses_number: u32,
}

impl Clone for SiaHDAccount {
    fn clone(&self) -> Self {
        SiaHDAccount {
            account_id: self.account_id,
            // `ExtendedSigningKey` has no public `Clone` impl (only a private
            // inherent one used internally by the `ed25519_dalek_bip32` crate),
            // but every field it wraps is public, so an equivalent clone is
            // built by hand from those fields. `HDWalletOps::get_account`
            // requires `HDAccount: Clone` (`hd_wallet.rs`), so this is load-bearing,
            // not decorative.
            account_signing_key: ExtendedSigningKey {
                depth: self.account_signing_key.depth,
                child_index: self.account_signing_key.child_index,
                signing_key: self.account_signing_key.signing_key.clone(),
                chain_code: self.account_signing_key.chain_code,
            },
            external_addresses_number: self.external_addresses_number,
        }
    }
}

impl HDAccountOps for SiaHDAccount {
    fn known_addresses_number(&self, chain: Bip44Chain) -> MmResult<u32, InvalidBip44ChainError> {
        match chain {
            Bip44Chain::External => Ok(self.external_addresses_number),
            other => MmError::err(InvalidBip44ChainError { chain: other }),
        }
    }

    fn account_derivation_path(&self) -> CryptoDerivationPath { sia_account_derivation_path(self.account_id) }

    fn account_id(&self) -> u32 { self.account_id }
}

/// Builds the discretionary display/RPC derivation path `m/44'/1991'/{account_id}'`
/// (the coin type is dictated; the path is otherwise informational, R36).
fn sia_account_derivation_path(account_id: u32) -> CryptoDerivationPath {
    let mut path = CryptoDerivationPath::default();
    path.push(ChildNumber::new(44, true).expect("44 < ChildNumber::HARDENED_FLAG"));
    path.push(ChildNumber::new(SIA_COIN_TYPE, true).expect("1991 < ChildNumber::HARDENED_FLAG"));
    // `account_id` is only ever assigned by `create_new_account` below, which
    // already rejects ids >= HARDENED_FLAG before a `SiaHDAccount` is built.
    path.push(ChildNumber::new(account_id, true).expect("account_id already validated below HARDENED_FLAG"));
    path
}

// ─── HD wallet ───────────────────────────────────────────────────────

/// A Siacoin HD wallet: the SLIP-10 ed25519 root key at `m/44'/1991'` plus the
/// set of activated accounts derived from it. Purely in-memory -- no
/// persistent account storage (`HDWalletCoinStorage`) is wired in this pass;
/// see the module doc comment.
pub struct SiaHDWallet {
    root_signing_key: ExtendedSigningKey,
    accounts: HDAccountsMutex<SiaHDAccount>,
    gap_limit: u32,
}

impl SiaHDWallet {
    /// Derives the wallet's `m/44'/1991'` root from the given global HD
    /// context (the same `GlobalHDAccountArc` the single-address path's
    /// `SiaCoin::new` already consumes for the `GlobalHDAccount` priv-key
    /// policy) and starts it with no activated accounts.
    pub fn new(global_hd: &GlobalHDAccountArc, gap_limit: u32) -> MmResult<Self, SiaHDWalletCreationError> {
        let root_signing_key = global_hd
            .derive_ed25519_signing_key(&SIA_HD_ROOT_PATH)
            .map_err(|e| SiaHDWalletCreationError::DeriveRoot(e.to_string()))?;
        Ok(SiaHDWallet {
            root_signing_key,
            accounts: HDAccountsMutex::new(HDAccountsMap::new()),
            gap_limit,
        })
    }
}

impl HDWalletOps for SiaHDWallet {
    type HDAccount = SiaHDAccount;

    fn coin_type(&self) -> u32 { SIA_COIN_TYPE }

    fn gap_limit(&self) -> u32 { self.gap_limit }

    fn get_accounts_mutex(&self) -> &HDAccountsMutex<Self::HDAccount> { &self.accounts }
}

// ─── Derivation / account-creation free functions (mirrors utxo_common_hd.rs) ─

/// Derives the address at `(hd_account, chain, address_id)`. `chain` must be
/// `Bip44Chain::External` -- see the module doc comment for why `Internal` is
/// rejected.
pub fn derive_address(
    hd_account: &SiaHDAccount,
    chain: Bip44Chain,
    address_id: u32,
) -> MmResult<HDAddress<Address, PublicKey>, AddressDerivingError> {
    if !matches!(chain, Bip44Chain::External) {
        // `HDAccountOps::known_addresses_number` already rejects `Internal` for
        // Sia; mirror that here so a caller cannot bypass it by calling
        // `derive_address` directly with an id it never validated through
        // `known_addresses_number`/`is_address_activated`.
        return MmError::err(AddressDerivingError::Ed25519Bip32Error(format!(
            "Sia HD wallet doesn't support the '{:?}' BIP44 chain",
            chain
        )));
    }

    let chain_value = chain as u32;
    let derived = hd_account
        .account_signing_key
        .derive_child(ChildIndex::Hardened(chain_value))
        .and_then(|node| node.derive_child(ChildIndex::Hardened(address_id)))
        .map_err(|e| AddressDerivingError::Ed25519Bip32Error(e.to_string()))?;

    let keypair = SiaKeypair::from_private_bytes(derived.signing_key.as_bytes())
        .map_err(|e| AddressDerivingError::Ed25519Bip32Error(format!("SiaKeypair::from_private_bytes: {}", e)))?;
    let pubkey = keypair.public();
    let address = pubkey.address();

    let mut derivation_path = hd_account.account_derivation_path();
    derivation_path
        .push(ChildNumber::new(chain_value, true).map_err(|e| AddressDerivingError::Ed25519Bip32Error(e.to_string()))?);
    derivation_path
        .push(ChildNumber::new(address_id, true).map_err(|e| AddressDerivingError::Ed25519Bip32Error(e.to_string()))?);

    Ok(HDAddress {
        address,
        pubkey,
        derivation_path,
    })
}

/// Creates and registers a new HD account, deriving its `m/44'/1991'/{id}'`
/// signing key from the wallet's root. Account ids are assigned sequentially
/// (`utxo_common_hd`/`eth_hd_wallet`'s own convention).
pub async fn create_new_account(
    hd_wallet: &SiaHDWallet,
) -> MmResult<HDAccountMut<'_, SiaHDAccount>, NewAccountCreatingError> {
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

    let account_signing_key = hd_wallet
        .root_signing_key
        .derive_child(ChildIndex::Hardened(new_account_id))
        .map_err(|e| NewAccountCreatingError::Internal(format!("ed25519 account derivation failed: {}", e)))?;

    let new_account = SiaHDAccount {
        account_id: new_account_id,
        account_signing_key,
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

    Ok(AsyncMutexGuard::map(accounts, |accounts| {
        accounts
            .entry(new_account_id)
            // the `entry` method should return [`Entry::Vacant`] due to the checks above
            .or_insert(new_account)
    }))
}

/// Updates `hd_account`'s known-addresses count in memory. No persistent
/// storage is wired in this pass, so `hd_wallet` is unused; it is kept as a
/// parameter to mirror `HDWalletCoinOps::set_known_addresses_number`'s shape
/// (and for a future storage-backed pass to fill in without a signature
/// change).
pub async fn set_known_addresses_number(
    _hd_wallet: &SiaHDWallet,
    hd_account: &mut SiaHDAccount,
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
            hd_account.external_addresses_number = new_known_addresses_number;
            Ok(())
        },
        other => MmError::err(AccountUpdatingError::InvalidBip44Chain(InvalidBip44ChainError {
            chain: other,
        })),
    }
}

// ─── HDWalletCoinOps for SiaCoin ───────────────────────────────────────

#[async_trait]
impl HDWalletCoinOps for SiaCoin {
    type Address = Address;
    type Pubkey = PublicKey;
    type HDWallet = SiaHDWallet;
    type HDAccount = SiaHDAccount;

    fn derive_address(
        &self,
        hd_account: &Self::HDAccount,
        chain: Bip44Chain,
        address_id: u32,
    ) -> MmResult<HDAddress<Self::Address, Self::Pubkey>, AddressDerivingError> {
        derive_address(hd_account, chain, address_id)
    }

    async fn create_new_account<'a, XPubExtractor>(
        &self,
        hd_wallet: &'a Self::HDWallet,
        xpub_extractor: Option<&XPubExtractor>,
    ) -> MmResult<HDAccountMut<'a, Self::HDAccount>, NewAccountCreatingError>
    where
        XPubExtractor: HDXPubExtractor + Sync,
    {
        // Sia has no hardware-wallet signing support (R-T3: only the single-key
        // and single-address HD-account priv-key policies reach an activated
        // coin); a caller that supplied an xpub extractor is asking for a
        // derivation this coin cannot service in software.
        if xpub_extractor.is_some() {
            return MmError::err(NewAccountCreatingError::CoinDoesntSupportTrezor);
        }
        create_new_account(hd_wallet).await
    }

    async fn set_known_addresses_number(
        &self,
        hd_wallet: &Self::HDWallet,
        hd_account: &mut Self::HDAccount,
        chain: Bip44Chain,
        new_known_addresses_number: u32,
    ) -> MmResult<(), AccountUpdatingError> {
        set_known_addresses_number(hd_wallet, hd_account, chain, new_known_addresses_number).await
    }
}

// ─── CoinWithDerivationMethod ───────────────────────────────────────────

/// CRD ch.20 D1: exposes the `Iguana`/`HDWallet` derivation mode `SiaCoinBuilder`
/// now constructs (`siacoin_helpers.rs`) to the coin-generic HD balance/RPC surface
/// (`EnableCoinBalanceOps`, `HDWalletRpcOps`, `AccountBalanceRpcOps`, ...), mirroring
/// `eth_hd_wallet.rs`'s `impl CoinWithDerivationMethod for EthCoin`.
impl CoinWithDerivationMethod for SiaCoin {
    type Address = Address;
    type HDWallet = SiaHDWallet;

    fn derivation_method(&self) -> &DerivationMethod<Self::Address, Self::HDWallet> { &self.derivation_method }
}

// ─── Address discovery (HDWalletBalanceOps) ────────────────────────────

/// Checks whether an address has ever been used by querying walletd's
/// per-address event log (`GET /api/addresses/:addr/events`, §20.8) -- the
/// same "has history, not just balance" signal UTXO's own scanner uses, so a
/// spent-back-to-zero address still stops the gap-limit counter from
/// resetting incorrectly.
pub struct SiaAddressScanner {
    client: Arc<SiaClient>,
}

#[async_trait]
impl HDAddressBalanceScanner for SiaAddressScanner {
    type Address = Address;

    async fn is_address_used(&self, address: &Address) -> BalanceResult<bool> {
        let events = self
            .client
            .get_address_events(address.clone())
            .await
            .map_to_mm(|e| BalanceError::Transport(sia_text(&e)))?;
        Ok(!events.is_empty())
    }
}

#[async_trait]
impl HDWalletBalanceOps for SiaCoin {
    type HDAddressScanner = SiaAddressScanner;

    async fn produce_hd_address_scanner(&self) -> BalanceResult<Self::HDAddressScanner> {
        Ok(SiaAddressScanner {
            client: self.client.clone(),
        })
    }

    async fn enable_hd_wallet<XPubExtractor>(
        &self,
        hd_wallet: &Self::HDWallet,
        xpub_extractor: Option<&XPubExtractor>,
        scan_policy: EnableCoinScanPolicy,
        min_addresses_number: u32,
    ) -> MmResult<HDWalletBalance, EnableCoinBalanceError>
    where
        XPubExtractor: HDXPubExtractor + Sync,
    {
        coin_balance::common_impl::enable_hd_wallet(self, hd_wallet, xpub_extractor, scan_policy, min_addresses_number)
            .await
    }

    async fn scan_for_new_addresses(
        &self,
        hd_wallet: &Self::HDWallet,
        hd_account: &mut Self::HDAccount,
        address_scanner: &Self::HDAddressScanner,
        gap_limit: u32,
    ) -> BalanceResult<Vec<HDAddressBalance>> {
        // Sia only supports the External chain (see the module doc comment).
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
        let balance = self
            .client
            .address_balance(address.clone())
            .await
            .map_to_mm(|e| BalanceError::Transport(sia_text(&e)))?;
        Ok(CoinBalance {
            spendable: hastings_to_siacoin(balance.siacoins),
            unspendable: hastings_to_siacoin(balance.immature_siacoins),
        })
    }

    async fn known_addresses_balances(
        &self,
        addresses: Vec<Self::Address>,
    ) -> BalanceResult<Vec<(Self::Address, CoinBalance)>> {
        // walletd's balance/events endpoints (§20.8) are per-address only --
        // there is no batch/multi-address query in the bound sia-rust client
        // (`ApiClientHelpers`, checked against its public surface for this
        // pass) -- so this sums N per-address calls rather than a single
        // batched one. Feasible, just not O(1) in request count; see the
        // coder pass report.
        let mut result = Vec::with_capacity(addresses.len());
        for addr in addresses {
            let balance = self.known_address_balance(&addr).await?;
            result.push((addr, balance));
        }
        Ok(result)
    }
}

// ─── HD RPC surface ─────────────────────────────────────────────────────
//
// CRD ch.20 D1's remaining "not yet wired" gap: `HDWalletRpcOps`,
// `AccountBalanceRpcOps`, and `InitAccountBalanceRpcOps` are pure delegation to
// the crate's shared `common_impl` helpers -- the same helpers `eth_hd_wallet.rs`
// delegates to for `EthCoin` -- because those helpers are already generic over
// any coin satisfying `HDWalletBalanceOps + CoinWithDerivationMethod + MarketCoinOps`,
// which `SiaCoin` now does. `InitCreateHDAccountRpcOps` is the one exception; see
// its own doc comment below.

#[async_trait]
impl HDWalletRpcOps for SiaCoin {
    async fn get_new_address_rpc(
        &self,
        params: GetNewHDAddressParams,
    ) -> MmResult<GetNewHDAddressResponse, HDWalletRpcError> {
        hd_wallet::common_impl::get_new_address_rpc(self, params).await
    }
}

#[async_trait]
impl AccountBalanceRpcOps for SiaCoin {
    async fn account_balance_rpc(
        &self,
        params: AccountBalanceParams,
    ) -> MmResult<HDAccountBalanceResponse, HDAccountBalanceRpcError> {
        account_balance::common_impl::account_balance_rpc(self, params).await
    }
}

#[async_trait]
impl InitAccountBalanceRpcOps for SiaCoin {
    async fn init_account_balance_rpc(
        &self,
        params: InitAccountBalanceParams,
    ) -> MmResult<HDAccountBalance, HDAccountBalanceRpcError> {
        init_account_balance_mod::common_impl::init_account_balance_rpc(self, params).await
    }
}

/// Unlike `HDWalletRpcOps`/`AccountBalanceRpcOps`/`InitAccountBalanceRpcOps` above,
/// this does **not** delegate to `init_create_account::common_impl`. That shared
/// helper is what `EthCoin`/`UtxoStandardCoin` use, but it always forwards
/// `Some(xpub_extractor)` into `HDWalletCoinOps::create_new_account` (mirroring the
/// hardware-wallet-capable account-creation flow those coins support). Sia's own
/// `create_new_account` (above) rejects *any* `Some(_)` extractor outright --
/// SLIP-10 ed25519 has no public-key-only child derivation, so there is no
/// software-or-hardware "extract an xpub for a not-yet-existing account" operation
/// to perform (module doc comment; the chapter's own D1 architectural-risk note).
/// Delegating verbatim would therefore make `task::create_new_account` permanently
/// fail for every Sia HD wallet with `CoinDoesntSupportTrezor`, not merely for a
/// hardware-backed one.
///
/// This impl instead calls the crate's `create_new_account` free function
/// directly (this module, above) -- always the software path, which is the only
/// path a `SiaCoin` can ever reach (R-T3, ch.46 R46.1.3: a Trezor priv-key policy
/// is rejected long before an `HDWallet`-mode `SiaCoin` exists), so `xpub_extractor`
/// is intentionally unused rather than threaded through as `Some(_)`.
///
/// Separately, and not fixed here: `task::create_new_account`'s *dispatcher*
/// (`coins_activation`/`coins::rpc_command::init_create_account`) unconditionally
/// constructs its extractor via `RpcTaskXPubExtractor::new` (not `new_unchecked`),
/// which itself fails immediately with `HwContextNotInitialized` whenever no
/// hardware wallet is connected -- for *every* coin wired to it today, Sia
/// included once wired. That is a pre-existing, coin-agnostic gap in the shared
/// RPC-task plumbing, not something this pass introduces or can fix from within
/// `coins::siacoin`; see the coder pass report.
#[async_trait]
impl InitCreateHDAccountRpcOps for SiaCoin {
    async fn init_create_account_rpc<XPubExtractor>(
        &self,
        params: CreateNewAccountParams,
        _xpub_extractor: &XPubExtractor,
    ) -> MmResult<HDAccountBalance, HDWalletRpcError>
    where
        XPubExtractor: HDXPubExtractor + Sync,
    {
        init_create_new_account_software(self, params).await
    }
}

/// Mirrors `mm2src/coins/rpc_command/init_create_account.rs`'s own
/// `common_impl::init_create_new_account_rpc`, minus the xpub-extractor plumbing --
/// see `InitCreateHDAccountRpcOps for SiaCoin`'s doc comment for why.
async fn init_create_new_account_software(
    coin: &SiaCoin,
    params: CreateNewAccountParams,
) -> MmResult<HDAccountBalance, HDWalletRpcError> {
    let hd_wallet = coin.derivation_method().hd_wallet_or_err().mm_err(Into::into)?;

    let mut new_account = create_new_account(hd_wallet).await.mm_err(Into::into)?;
    let address_scanner = coin.produce_hd_address_scanner().await.mm_err(Into::into)?;
    let account_index = new_account.account_id();
    let account_derivation_path = new_account.account_derivation_path();

    let addresses = if params.scan {
        let gap_limit = params.gap_limit.unwrap_or_else(|| hd_wallet.gap_limit());
        coin.scan_for_new_addresses(hd_wallet, &mut new_account, &address_scanner, gap_limit)
            .await
            .mm_err(Into::into)?
    } else {
        Vec::new()
    };

    let total_balance = crate::coin_balance::sum_hd_address_balances(coin.ticker(), &addresses);

    Ok(HDAccountBalance {
        account_index,
        derivation_path: RpcDerivationPath(account_derivation_path),
        total_balance,
        addresses,
    })
}

/// Mirrors `utxo_common_hd::scan_for_new_addresses_impl` /
/// `eth_hd_wallet::scan_for_new_addresses_impl`: walks addresses from the
/// first unknown index until `gap_limit` consecutive unused addresses are
/// seen, then persists the new known-addresses count.
async fn scan_for_new_addresses_impl(
    coin: &SiaCoin,
    hd_wallet: &SiaHDWallet,
    hd_account: &mut SiaHDAccount,
    address_scanner: &SiaAddressScanner,
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

// ─── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use common::block_on;
    use crypto::{CryptoCtx, KeyPairPolicy};
    use mm2_core::mm_ctx::MmCtxBuilder;

    /// Standard BIP39 test mnemonic (zero entropy). DO NOT use in production.
    /// Same vector `crypto::global_hd_ctx`'s own tests use.
    const TEST_MNEMONIC: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    /// `GlobalHDAccountCtx` lives in a private `crypto` module and has no
    /// constructor reachable from outside that crate -- the only public path
    /// to a `GlobalHDAccountArc` is through `CryptoCtx`, same as
    /// `eth_tests.rs`'s own `eth_hd_coin_for_test` helper.
    fn test_global_hd() -> GlobalHDAccountArc {
        let ctx = MmCtxBuilder::new().into_mm_arc();
        let crypto_ctx = CryptoCtx::init_with_global_hd_account(ctx, TEST_MNEMONIC).expect("valid test mnemonic");
        match crypto_ctx.key_pair_policy() {
            KeyPairPolicy::GlobalHDAccount(global_hd) => global_hd.clone(),
            KeyPairPolicy::Iguana => panic!("expected global-HD test context"),
        }
    }

    fn test_wallet(gap_limit: u32) -> SiaHDWallet {
        SiaHDWallet::new(&test_global_hd(), gap_limit).expect("Sia HD root derivation should succeed")
    }

    #[test]
    fn coin_type_and_gap_limit_are_reported() {
        let wallet = test_wallet(7);
        assert_eq!(wallet.coin_type(), SIA_COIN_TYPE);
        assert_eq!(wallet.gap_limit(), 7);
    }

    #[test]
    fn account_0_external_0_matches_the_existing_single_address_mode_path() {
        // §20.4.1's dictated single-address path is `m/44'/1991'/0'/0'/0'`
        // (`SINGLE_ADDRESS_MODE_PATH`, `siacoin_types.rs`), already exercised
        // unchanged by the existing single-key-HD activation path. Multi-account
        // HD's canonical (account=0, chain=External, address=0) address MUST
        // derive to the exact same address, or the two mechanisms would
        // silently disagree about "my Sia address" for the same seed.
        let global_hd = test_global_hd();
        let expected_key = global_hd
            .derive_ed25519_signing_key(&SINGLE_ADDRESS_MODE_PATH)
            .expect("existing single-address-mode derivation should succeed");
        let expected_keypair = SiaKeypair::from_private_bytes(expected_key.signing_key.as_bytes()).unwrap();
        let expected_address = expected_keypair.public().address();

        let wallet = SiaHDWallet::new(&global_hd, 20).unwrap();
        let account = block_on(create_new_account(&wallet)).expect("account 0 creation should succeed");
        let derived = derive_address(&account, Bip44Chain::External, 0).expect("address 0 derivation should succeed");

        assert_eq!(derived.address, expected_address);
    }

    #[test]
    fn create_new_account_assigns_sequential_ids() {
        let wallet = test_wallet(20);
        let account0_id = block_on(create_new_account(&wallet)).unwrap().account_id();
        let account1_id = block_on(create_new_account(&wallet)).unwrap().account_id();
        let account2_id = block_on(create_new_account(&wallet)).unwrap().account_id();
        assert_eq!((account0_id, account1_id, account2_id), (0, 1, 2));
    }

    #[test]
    fn different_accounts_derive_different_addresses() {
        let wallet = test_wallet(20);
        let account0 = block_on(create_new_account(&wallet)).unwrap().clone();
        let account1 = block_on(create_new_account(&wallet)).unwrap().clone();

        let addr0 = derive_address(&account0, Bip44Chain::External, 0).unwrap().address;
        let addr1 = derive_address(&account1, Bip44Chain::External, 0).unwrap().address;
        assert_ne!(addr0, addr1);
    }

    #[test]
    fn different_address_ids_within_an_account_derive_different_addresses() {
        let wallet = test_wallet(20);
        let account = block_on(create_new_account(&wallet)).unwrap().clone();

        let addr0 = derive_address(&account, Bip44Chain::External, 0).unwrap().address;
        let addr1 = derive_address(&account, Bip44Chain::External, 1).unwrap().address;
        assert_ne!(addr0, addr1);
    }

    #[test]
    fn derive_address_is_deterministic() {
        let wallet = test_wallet(20);
        let account = block_on(create_new_account(&wallet)).unwrap().clone();

        let first = derive_address(&account, Bip44Chain::External, 3).unwrap().address;
        let second = derive_address(&account, Bip44Chain::External, 3).unwrap().address;
        assert_eq!(first, second);
    }

    #[test]
    fn internal_chain_is_rejected_for_derive_address() {
        let wallet = test_wallet(20);
        let account = block_on(create_new_account(&wallet)).unwrap().clone();
        assert!(derive_address(&account, Bip44Chain::Internal, 0).is_err());
    }

    #[test]
    fn internal_chain_is_rejected_for_known_addresses_number() {
        let wallet = test_wallet(20);
        let account = block_on(create_new_account(&wallet)).unwrap().clone();
        assert!(account.known_addresses_number(Bip44Chain::Internal).is_err());
        assert_eq!(account.known_addresses_number(Bip44Chain::External).unwrap(), 0);
    }

    #[test]
    fn set_known_addresses_number_updates_external_count() {
        let wallet = test_wallet(20);
        let mut account = block_on(create_new_account(&wallet)).unwrap().clone();
        block_on(set_known_addresses_number(
            &wallet,
            &mut account,
            Bip44Chain::External,
            5,
        ))
        .unwrap();
        assert_eq!(account.known_addresses_number(Bip44Chain::External).unwrap(), 5);
    }

    #[test]
    fn set_known_addresses_number_rejects_internal_chain() {
        let wallet = test_wallet(20);
        let mut account = block_on(create_new_account(&wallet)).unwrap().clone();
        assert!(block_on(set_known_addresses_number(
            &wallet,
            &mut account,
            Bip44Chain::Internal,
            5
        ))
        .is_err());
    }

    #[test]
    fn account_derivation_path_reflects_account_id() {
        let wallet = test_wallet(20);
        let account = block_on(create_new_account(&wallet)).unwrap().clone();
        assert_eq!(account.account_derivation_path().to_string(), "m/44'/1991'/0'");
    }
}
