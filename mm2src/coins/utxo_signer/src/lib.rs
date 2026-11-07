use async_trait::async_trait;
use chain::Transaction as UtxoTx;
use crypto::trezor::client::TrezorClient;
use crypto::trezor::utxo::TrezorUtxoCoin;
use crypto::trezor::TrezorError;
use derive_more::Display;
use keys::bytes::Bytes;
use keys::KeyPair;
use mm2_err_handle::prelude::*;
use rpc::v1::types::{Transaction as RpcTransaction, H256 as H256Json};
use script::Script;

mod sign_common;
pub mod sign_params;
pub mod with_key_pair;
pub mod with_trezor;

use crate::with_key_pair::UtxoSignWithKeyPairError;
use sign_params::UtxoSignTxParams;

pub type UtxoSignTxResult<T> = Result<T, MmError<UtxoSignTxError>>;

type Signature = Bytes;

pub enum TxProviderError {
    Transport(String),
    InvalidResponse(String),
    Internal(String),
}

#[derive(Debug, Display)]
pub enum UtxoSignTxError {
    #[display(fmt = "Coin '{}' is not supported with Trezor", coin)]
    CoinNotSupportedWithTrezor { coin: String },
    #[display(fmt = "Trezor doesn't support P2WPKH outputs yet")]
    TrezorDoesntSupportP2WPKH,
    #[display(
        fmt = "The sighash suffix {:#010x} of this coin is not supported by hardware wallets: the device signs \
               its own digest and cannot append the suffix",
        suffix
    )]
    SighashSuffixNotSupportedByHardwareWallet { suffix: u32 },
    #[display(fmt = "Trezor client error: {}", _0)]
    TrezorError(TrezorError),
    #[display(fmt = "Encountered invalid parameter '{}': {}", param, description)]
    InvalidSignParam { param: String, description: String },
    #[display(
        fmt = "Hardware Device returned an invalid number of signatures: '{}', number of inputs: '{}'",
        actual,
        expected
    )]
    InvalidSignaturesNumber { actual: usize, expected: usize },
    #[display(fmt = "Error signing using a private key")]
    ErrorSigning(keys::Error),
    #[display(
        fmt = "{} script '{}' built from input key pair doesn't match expected prev script '{}'",
        script_type,
        script,
        prev_script
    )]
    MismatchScript {
        script_type: String,
        script: Script,
        prev_script: Script,
    },
    #[display(fmt = "Transport error: {}", _0)]
    Transport(String),
    #[display(fmt = "Internal error: {}", _0)]
    Internal(String),
}

/// A hardware wallet signs the digest its own firmware defines and cannot be given a sighash suffix (CRD ch.61
/// R61.4.4). For a coin whose resolved suffix is non-zero, every hardware signing request must fail with this
/// error before anything is sent to the device, instead of producing a transaction the network rejects.
///
/// # Errors
///
/// [`UtxoSignTxError::SighashSuffixNotSupportedByHardwareWallet`] when `sighash_suffix` is not zero.
pub fn refuse_sighash_suffix_for_hardware_wallet(sighash_suffix: u32) -> UtxoSignTxResult<()> {
    if sighash_suffix == 0 {
        return Ok(());
    }
    MmError::err(UtxoSignTxError::SighashSuffixNotSupportedByHardwareWallet { suffix: sighash_suffix })
}

impl From<TrezorError> for UtxoSignTxError {
    fn from(e: TrezorError) -> Self { UtxoSignTxError::TrezorError(e) }
}

impl From<UtxoSignWithKeyPairError> for UtxoSignTxError {
    fn from(error_with_key: UtxoSignWithKeyPairError) -> Self {
        let error = error_with_key.to_string();
        match error_with_key {
            UtxoSignWithKeyPairError::MismatchScript {
                script_type,
                script,
                prev_script,
            } => UtxoSignTxError::MismatchScript {
                script_type,
                script,
                prev_script,
            },
            // `with_key_pair` contains methods that checks parameters
            // that are expected to be checked by [`sign_common::UtxoSignTxParamsBuilder::build`] already.
            // So if this error happens, it's our internal error.
            UtxoSignWithKeyPairError::InputIndexOutOfBound { .. } => UtxoSignTxError::Internal(error),
            UtxoSignWithKeyPairError::ErrorSigning(sign) => UtxoSignTxError::ErrorSigning(sign),
        }
    }
}

impl From<keys::Error> for UtxoSignTxError {
    fn from(e: keys::Error) -> Self { UtxoSignTxError::ErrorSigning(e) }
}

impl From<TxProviderError> for UtxoSignTxError {
    fn from(e: TxProviderError) -> Self {
        match e {
            TxProviderError::Transport(transport) | TxProviderError::InvalidResponse(transport) => {
                UtxoSignTxError::Transport(transport)
            },
            TxProviderError::Internal(internal) => UtxoSignTxError::Internal(internal),
        }
    }
}

/// The trait declares a transaction getter.
/// The provider can use cache or RPC client.
#[async_trait]
pub trait TxProvider {
    async fn get_rpc_transaction(&self, tx_hash: &H256Json) -> Result<RpcTransaction, MmError<TxProviderError>>;
}

pub enum SignPolicy<'a> {
    WithTrezor(TrezorClient),
    WithKeyPair(&'a KeyPair),
}

#[async_trait]
pub trait UtxoSignerOps {
    type TxGetter: TxProvider + Send + Sync;

    fn trezor_coin(&self) -> UtxoSignTxResult<TrezorUtxoCoin>;

    fn fork_id(&self) -> u32;

    fn branch_id(&self) -> u32;

    fn tx_provider(&self) -> Self::TxGetter;

    async fn sign_tx(&self, params: UtxoSignTxParams, sign_policy: SignPolicy<'_>) -> UtxoSignTxResult<UtxoTx> {
        match sign_policy {
            SignPolicy::WithTrezor(trezor) => {
                let signer = with_trezor::TrezorTxSigner {
                    trezor,
                    tx_provider: self.tx_provider(),
                    trezor_coin: self.trezor_coin()?,
                    params,
                    fork_id: self.fork_id(),
                    branch_id: self.branch_id(),
                };
                signer.sign_tx().await
            },
            SignPolicy::WithKeyPair(key_pair) => {
                let signed = with_key_pair::sign_tx(
                    params.unsigned_tx,
                    key_pair,
                    params.prev_script,
                    params.signature_version,
                    self.fork_id(),
                )
                .mm_err(Into::into)?;
                Ok(signed)
            },
        }
    }
}

#[cfg(test)]
mod sighash_suffix_tests {
    use super::*;

    /// CRD ch.61 T7: a coin with a non-zero suffix is refused for a hardware wallet with an error naming the
    /// suffix, and a zero suffix is not. `TrezorTxSigner::sign_tx`, the wallet-funded V2 signing path and the
    /// withdraw path call this function before any device is contacted, so the refusal precedes every device
    /// message by construction.
    #[test]
    fn t7_a_hardware_wallet_refuses_a_non_zero_suffix() {
        assert!(refuse_sighash_suffix_for_hardware_wallet(0).is_ok());

        let err = refuse_sighash_suffix_for_hardware_wallet(0x0132_4342)
            .unwrap_err()
            .into_inner();
        assert!(matches!(
            err,
            UtxoSignTxError::SighashSuffixNotSupportedByHardwareWallet { suffix: 0x0132_4342 }
        ));
        let message = err.to_string();
        assert!(message.contains("sighash suffix 0x01324342"), "{message}");
        assert!(message.contains("hardware wallet"), "{message}");
    }
}
