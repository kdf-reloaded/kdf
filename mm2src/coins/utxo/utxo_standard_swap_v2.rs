// ─────────────────────────────────────────────────────────────────────────────
//  V2 atomic-swap trait impls for UtxoStandardCoin (chapter 15).
//
//  This module is intentionally `include!`d into `utxo_standard.rs` so the
//  trait impls live on the same type without bloating the parent file.
//  Maker/taker V2 swap ops delegate into the chapter-bound UTXO helpers.
//  Deferred variants must fail explicitly instead of panicking or falling back
//  to unrelated key material.
// ─────────────────────────────────────────────────────────────────────────────

use crate::hd_wallet::{HDAccountOps, HDAddress, HDWalletOps};
use crate::utxo::utxo_common;
use crate::utxo::utxo_standard::UtxoStandardCoin;
use crate::utxo::{UtxoCoinFields, UtxoHDAccount, UtxoHDWallet, UtxoTx};
use crate::{CommonSwapOpsV2, DerivationMethod, DexFee, FindPaymentSpendError, FundingTxSpend, GenPreimageResult,
            GenTakerFundingSpendArgs, GenTakerPaymentSpendArgs, MakerCoinSwapOpsV2, ParseCoinAssocTypes,
            PrivKeyPolicy, RefundFundingSecretArgs, RefundMakerPaymentSecretArgs, RefundMakerPaymentTimelockArgs,
            RefundTakerPaymentArgs, SearchForFundingSpendErr, SendMakerPaymentArgs, SendTakerFundingArgs,
            SpendMakerPaymentArgs, TakerCoinSwapOpsV2, ToBytes, TransactionErr, TxPreimageWithSig,
            ValidateMakerPaymentArgs, ValidateSwapV2TxResult, ValidateTakerFundingArgs,
            ValidateTakerFundingSpendPreimageResult, ValidateTakerPaymentSpendPreimageResult};
use async_trait::async_trait;
use crypto::{Bip32DerPathOps, Bip44Chain, ChildNumber};
use keys::{Address, Error as KeysError, Public, Signature};
use mm2_err_handle::prelude::*;
use primitives::hash::H264;
use script::TransactionInputSigner;
use serialization::{deserialize, serialize, Error as SerError};
use std::str::FromStr;

/// Local newtype around `TransactionInputSigner` used as the V2 swap `Preimage`
/// associated type. Needed because the blanket `impl<T: AsRef<[u8]>> ToBytes for T`
/// makes a direct `impl ToBytes for TransactionInputSigner` impossible due to
/// coherence — wrapping in a local type lets us provide a concrete `ToBytes` impl
/// that serialises the underlying transaction.
#[derive(Clone, Debug)]
pub struct UtxoTxPreimage(pub TransactionInputSigner);

impl From<TransactionInputSigner> for UtxoTxPreimage {
    fn from(signer: TransactionInputSigner) -> Self { UtxoTxPreimage(signer) }
}

impl ToBytes for UtxoTxPreimage {
    fn to_bytes(&self) -> Vec<u8> {
        let tx: UtxoTx = self.0.clone().into();
        serialize(&tx).into()
    }
}

pub(crate) fn trezor_v2_htlc_error() -> String { trezor_v2_unsupported_script_signing_error() }

pub(crate) fn trezor_v2_address_error() -> String {
    trezor_v2_missing_derivation_metadata_error("UTXO Standard Swap V2 local address selection")
}

pub(crate) fn trezor_v2_unsupported_script_signing_error() -> String {
    "hardware_wallet:unsupported_script_signing_mode: UTXO Standard Swap V2 P2SH HTLC input signing is not supported by the current Trezor UTXO signer".to_owned()
}

pub(crate) fn trezor_v2_missing_derivation_metadata_error(context: &str) -> String {
    format!(
        "hardware_wallet:missing_derivation_metadata: enabled hardware HD address metadata is unavailable for {}",
        context
    )
}

pub(crate) fn trezor_v2_unsupported_coin_mapping_error(ticker: &str) -> String {
    format!(
        "hardware_wallet:unsupported_coin_mapping: '{}' has no Trezor UTXO coin mapping",
        ticker
    )
}

pub(crate) fn trezor_v2_invalid_response_error(context: &str) -> String {
    format!("hardware_wallet:invalid_response: {}", context)
}

fn address_from_pubkey(fields: &UtxoCoinFields, public: &Public, addr_format: keys::AddressFormat) -> Address {
    utxo_common::address_from_pubkey(
        public,
        fields.conf.pub_addr_prefix,
        fields.conf.pub_t_addr_prefix,
        fields.conf.checksum_type,
        fields.conf.bech32_hrp.clone(),
        addr_format,
    )
}

pub(crate) fn enabled_hd_address_info_from_account(
    fields: &UtxoCoinFields,
    addr_format: &keys::AddressFormat,
    hd_account: &UtxoHDAccount,
    context: &str,
) -> Result<HDAddress<Address, Public>, String> {
    let is_enabled = hd_account
        .is_address_activated(Bip44Chain::External, 0)
        .map_err(|e| format!("Failed to inspect enabled HD address for {}: {}", context, e))?;
    if !is_enabled {
        return Err(format!("No enabled HD address found for {}", context));
    }

    let change_child = Bip44Chain::External.to_child_number();
    let address_id_child = ChildNumber::from(0);
    let derived_pubkey = hd_account
        .extended_pubkey
        .derive_child(change_child)
        .and_then(|account_pubkey| account_pubkey.derive_child(address_id_child))
        .map_err(|e| format!("Failed to derive enabled HD address for {}: {}", context, e))?;
    let public = Public::Compressed(H264::from(derived_pubkey.public_key().serialize()));
    let address = address_from_pubkey(fields, &public, addr_format.clone());

    let mut derivation_path = hd_account.account_derivation_path.to_derivation_path();
    derivation_path.push(change_child);
    derivation_path.push(address_id_child);

    Ok(HDAddress {
        address,
        pubkey: public,
        derivation_path,
    })
}

pub(crate) fn enabled_hd_address_from_account(
    fields: &UtxoCoinFields,
    addr_format: &keys::AddressFormat,
    hd_account: &UtxoHDAccount,
    context: &str,
) -> Result<Address, String> {
    enabled_hd_address_info_from_account(fields, addr_format, hd_account, context).map(|info| info.address)
}

pub(crate) async fn enabled_hd_address_info(
    fields: &UtxoCoinFields,
    hd_wallet: &UtxoHDWallet,
    context: &str,
) -> Result<HDAddress<Address, Public>, String> {
    let accounts = hd_wallet.accounts.lock().await;
    let default_account = accounts
        .get(&0)
        .ok_or_else(|| format!("No enabled HD account found for {}", context))?;
    enabled_hd_address_info_from_account(fields, &hd_wallet.address_format, default_account, context)
}

pub(crate) fn try_enabled_hd_address_info(
    fields: &UtxoCoinFields,
    hd_wallet: &UtxoHDWallet,
    context: &str,
) -> Result<HDAddress<Address, Public>, String> {
    let accounts = hd_wallet
        .accounts
        .try_lock()
        .ok_or_else(|| format!("HD accounts lock is busy for {}", context))?;
    let default_account = accounts
        .get(&0)
        .ok_or_else(|| format!("No enabled HD account found for {}", context))?;
    enabled_hd_address_info_from_account(fields, &hd_wallet.address_format, default_account, context)
}

#[async_trait]
impl ParseCoinAssocTypes for UtxoStandardCoin {
    type Address = Address;
    type AddressParseError = KeysError;
    type Pubkey = Public;
    type PubkeyParseError = KeysError;
    type Tx = UtxoTx;
    type TxParseError = SerError;
    type Preimage = UtxoTxPreimage;
    type PreimageParseError = SerError;
    type Sig = Signature;
    type SigParseError = KeysError;

    async fn my_addr(&self) -> Self::Address {
        self.try_my_addr()
            .await
            .expect("UtxoStandardCoin V2 address selection invariant failed")
    }

    async fn try_my_addr(&self) -> Result<Self::Address, String> {
        let fields: &UtxoCoinFields = self.as_ref();
        match (&fields.derivation_method, &fields.priv_key_policy) {
            (DerivationMethod::Iguana(addr), _) => Ok(addr.clone()),
            (DerivationMethod::HDWallet(utxo_hd_wallet), _) => {
                enabled_hd_address_info(fields, utxo_hd_wallet, "UTXO Standard Swap V2 address selection")
                    .await
                    .map(|info| info.address)
            },
        }
    }

    fn parse_address(&self, address: &str) -> Result<Self::Address, Self::AddressParseError> {
        Address::from_str(address).map_err(|_| KeysError::InvalidAddress)
    }

    fn parse_pubkey(&self, pubkey: &[u8]) -> Result<Self::Pubkey, Self::PubkeyParseError> { Public::from_slice(pubkey) }

    fn parse_tx(&self, tx: &[u8]) -> Result<Self::Tx, Self::TxParseError> { deserialize(tx) }

    fn parse_preimage(&self, preimage: &[u8]) -> Result<Self::Preimage, Self::PreimageParseError> {
        let tx: UtxoTx = deserialize(preimage)?;
        Ok(UtxoTxPreimage(tx.into()))
    }

    fn parse_signature(&self, sig: &[u8]) -> Result<Self::Sig, Self::SigParseError> {
        Ok(Signature::from(sig.to_vec()))
    }
}

#[async_trait]
impl CommonSwapOpsV2 for UtxoStandardCoin {
    fn derive_htlc_pubkey_v2(&self, swap_unique_data: &[u8]) -> Public {
        self.try_derive_htlc_pubkey_v2(swap_unique_data)
            .expect("UtxoStandardCoin V2 HTLC public-key derivation invariant failed")
    }

    fn try_derive_htlc_pubkey_v2(&self, _swap_unique_data: &[u8]) -> Result<Public, String> {
        let fields: &UtxoCoinFields = self.as_ref();
        match (&fields.derivation_method, &fields.priv_key_policy) {
            (_, PrivKeyPolicy::KeyPair(kp)) => Ok(*kp.public()),
            (DerivationMethod::HDWallet(hd_wallet), PrivKeyPolicy::HDWallet { .. })
            | (DerivationMethod::HDWallet(hd_wallet), PrivKeyPolicy::Trezor) => {
                try_enabled_hd_address_info(fields, hd_wallet, "UTXO Standard Swap V2 HTLC public-key derivation")
                    .map(|info| info.pubkey)
            },
            (DerivationMethod::Iguana(_), PrivKeyPolicy::HDWallet { .. }) => {
                Err("UTXO Standard Swap V2 HD HTLC public-key derivation requires an HD derivation method".to_owned())
            },
            (DerivationMethod::Iguana(_), PrivKeyPolicy::Trezor) => Err(trezor_v2_missing_derivation_metadata_error(
                "UTXO Standard Swap V2 HTLC public-key derivation",
            )),
        }
    }

    fn derive_htlc_pubkey_v2_bytes(&self, swap_unique_data: &[u8]) -> Vec<u8> {
        self.derive_htlc_pubkey_v2(swap_unique_data).to_vec()
    }

    fn try_derive_htlc_pubkey_v2_bytes(&self, swap_unique_data: &[u8]) -> Result<Vec<u8>, String> {
        self.try_derive_htlc_pubkey_v2(swap_unique_data)
            .map(|pubkey| pubkey.to_vec())
    }
}

#[async_trait]
impl MakerCoinSwapOpsV2 for UtxoStandardCoin {
    async fn send_maker_payment_v2(&self, args: SendMakerPaymentArgs<'_, Self>) -> Result<UtxoTx, TransactionErr> {
        utxo_common::send_maker_payment_v2(self.clone(), args).await
    }

    async fn validate_maker_payment_v2(&self, args: ValidateMakerPaymentArgs<'_, Self>) -> ValidateSwapV2TxResult {
        utxo_common::validate_maker_payment_v2(self, args).await
    }

    async fn refund_maker_payment_v2_timelock(
        &self,
        args: RefundMakerPaymentTimelockArgs<'_>,
    ) -> Result<UtxoTx, TransactionErr> {
        utxo_common::refund_maker_payment_v2_timelock(self, args).await
    }

    async fn refund_maker_payment_v2_secret(
        &self,
        args: RefundMakerPaymentSecretArgs<'_, Self>,
    ) -> Result<UtxoTx, TransactionErr> {
        utxo_common::refund_maker_payment_v2_secret(self, args).await
    }

    async fn spend_maker_payment_v2(&self, args: SpendMakerPaymentArgs<'_, Self>) -> Result<UtxoTx, TransactionErr> {
        utxo_common::spend_maker_payment_v2(self, args).await
    }
}

#[async_trait]
impl TakerCoinSwapOpsV2 for UtxoStandardCoin {
    async fn send_taker_funding(&self, args: SendTakerFundingArgs<'_>) -> Result<UtxoTx, TransactionErr> {
        utxo_common::send_taker_funding(self.clone(), args).await
    }

    async fn validate_taker_funding(&self, args: ValidateTakerFundingArgs<'_, Self>) -> ValidateSwapV2TxResult {
        utxo_common::validate_taker_funding(self, args).await
    }

    async fn refund_taker_funding_timelock(&self, args: RefundTakerPaymentArgs<'_>) -> Result<UtxoTx, TransactionErr> {
        utxo_common::refund_taker_funding_timelock(self, args).await
    }

    async fn refund_taker_funding_secret(
        &self,
        args: RefundFundingSecretArgs<'_, Self>,
    ) -> Result<UtxoTx, TransactionErr> {
        utxo_common::refund_taker_funding_secret(self, args).await
    }

    async fn search_for_taker_funding_spend(
        &self,
        tx: &UtxoTx,
        from_block: u64,
        secret_hash: &[u8],
    ) -> Result<Option<FundingTxSpend<Self>>, SearchForFundingSpendErr> {
        utxo_common::search_for_taker_funding_spend(self, tx, from_block, secret_hash).await
    }

    async fn gen_taker_funding_spend_preimage(
        &self,
        args: &GenTakerFundingSpendArgs<'_, Self>,
        swap_unique_data: &[u8],
    ) -> GenPreimageResult<Self> {
        utxo_common::gen_taker_funding_spend_preimage(self, args, swap_unique_data).await
    }

    async fn validate_taker_funding_spend_preimage(
        &self,
        gen_args: &GenTakerFundingSpendArgs<'_, Self>,
        preimage: &TxPreimageWithSig<Self>,
    ) -> ValidateTakerFundingSpendPreimageResult {
        utxo_common::validate_taker_funding_spend_preimage(self, gen_args, preimage).await
    }

    async fn sign_and_send_taker_funding_spend(
        &self,
        preimage: &TxPreimageWithSig<Self>,
        args: &GenTakerFundingSpendArgs<'_, Self>,
        swap_unique_data: &[u8],
    ) -> Result<UtxoTx, TransactionErr> {
        utxo_common::sign_and_send_taker_funding_spend(self, preimage, args, swap_unique_data).await
    }

    async fn refund_combined_taker_payment(&self, args: RefundTakerPaymentArgs<'_>) -> Result<UtxoTx, TransactionErr> {
        utxo_common::refund_combined_taker_payment(self, args).await
    }

    async fn gen_taker_payment_spend_preimage(
        &self,
        args: &GenTakerPaymentSpendArgs<'_, Self>,
        swap_unique_data: &[u8],
    ) -> GenPreimageResult<Self> {
        utxo_common::gen_taker_payment_spend_preimage(self, args, swap_unique_data).await
    }

    async fn validate_taker_payment_spend_preimage(
        &self,
        gen_args: &GenTakerPaymentSpendArgs<'_, Self>,
        preimage: &TxPreimageWithSig<Self>,
    ) -> ValidateTakerPaymentSpendPreimageResult {
        utxo_common::validate_taker_payment_spend_preimage(self, gen_args, preimage).await
    }

    async fn sign_and_broadcast_taker_payment_spend(
        &self,
        preimage: Option<&TxPreimageWithSig<Self>>,
        gen_args: &GenTakerPaymentSpendArgs<'_, Self>,
        secret: &[u8],
        swap_unique_data: &[u8],
    ) -> Result<UtxoTx, TransactionErr> {
        utxo_common::sign_and_broadcast_taker_payment_spend(self, preimage, gen_args, secret, swap_unique_data).await
    }

    async fn find_taker_payment_spend_tx(
        &self,
        taker_payment: &UtxoTx,
        from_block: u64,
        wait_until: u64,
    ) -> MmResult<UtxoTx, FindPaymentSpendError> {
        utxo_common::find_taker_payment_spend_tx(self, taker_payment, from_block, wait_until).await
    }

    async fn extract_secret_v2(&self, secret_hash: &[u8], spend_tx: &UtxoTx) -> Result<[u8; 32], String> {
        utxo_common::extract_secret_v2(secret_hash, spend_tx)
    }
}

// Silence "unused import" — `utxo_common` is imported for the ch15-phase-2
// helpers that this module will eventually call into.
#[allow(dead_code)]
fn _ch15_imports_anchor() { let _ = utxo_common::DEFAULT_SWAP_VOUT; }
