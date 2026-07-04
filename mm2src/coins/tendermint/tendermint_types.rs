use super::rpc::*;
use crate::{HistorySyncState, PrivKeyNotAllowed, PrivKeyPolicy, TransactionEnum};
use cosmrs::proto::cosmos::tx::v1beta1::TxRaw;
use cosmrs::proto::prost::{DecodeError, Message};
use cosmrs::tendermint::chain::Id as ChainId;
use cosmrs::{AccountId, Any, Denom, ErrorReport};
use crypto::{HDPathToCoin, Secp256k1Secret};
use derive_more::Display;
use futures::lock::Mutex as AsyncMutex;
use hex::FromHexError;
use kdf_crypto::sha256;
use keys::Public;
use mm2_core::mm_ctx::MmWeak;
use mm2_err_handle::prelude::*;
use parking_lot::Mutex as PaMutex;
use rpc::v1::types::Bytes as BytesJson;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use std::collections::HashMap;
use std::io;
use std::ops::Deref;
use std::sync::{Arc, Mutex};

use bigdecimal::BigDecimal;

// ————————————————————————————————————————————————————————————————
// ABCI Constants
// ————————————————————————————————————————————————————————————————

pub(super) const ABCI_GET_LATEST_BLOCK_PATH: &str = "/cosmos.base.tendermint.v1beta1.Service/GetLatestBlock";
pub(super) const ABCI_GET_BLOCK_BY_HEIGHT_PATH: &str = "/cosmos.base.tendermint.v1beta1.Service/GetBlockByHeight";
pub(super) const ABCI_SIMULATE_TX_PATH: &str = "/cosmos.tx.v1beta1.Service/Simulate";
pub(super) const ABCI_QUERY_ACCOUNT_PATH: &str = "/cosmos.auth.v1beta1.Query/Account";
pub(super) const ABCI_QUERY_BALANCE_PATH: &str = "/cosmos.bank.v1beta1.Query/Balance";
pub(super) const ABCI_GET_TX_PATH: &str = "/cosmos.tx.v1beta1.Service/GetTx";

pub(crate) const MIN_TX_SATOSHIS: i64 = 1;

pub(super) const ABCI_REQUEST_HEIGHT: Option<cosmrs::tendermint::block::Height> = None;
pub(super) const ABCI_REQUEST_PROVE: bool = false;

/// 0.25 is a good average gas price on ATOM and IRIS.
pub(super) const DEFAULT_GAS_PRICE: f64 = 0.25;
pub(super) const TIMEOUT_HEIGHT_DELTA: u64 = 100;
pub const GAS_LIMIT_DEFAULT: u64 = 125_000;
pub const GAS_WANTED_BASE_VALUE: f64 = 50_000.;
pub(crate) const TX_DEFAULT_MEMO: &str = "";

// IRIS HTLC time-lock bounds (in blocks)
pub(super) const MAX_TIME_LOCK: i64 = 34560;
pub(super) const MIN_TIME_LOCK: i64 = 50;

pub(super) const ACCOUNT_SEQUENCE_ERR: &str = "account sequence mismatch";

// ————————————————————————————————————————————————————————————————
// Key-pair wrapper
// ————————————————————————————————————————————————————————————————

pub struct TendermintKeyPair {
    pub(super) private_key_secret: Secp256k1Secret,
    pub(super) public_key: Public,
}

impl TendermintKeyPair {
    pub fn new(private_key_secret: Secp256k1Secret, public_key: Public) -> Self {
        Self {
            private_key_secret,
            public_key,
        }
    }
}

pub(super) type TendermintPrivKeyPolicy = PrivKeyPolicy<TendermintKeyPair>;

// ————————————————————————————————————————————————————————————————
// RPC node descriptor
// ————————————————————————————————————————————————————————————————

#[derive(Clone, Deserialize)]
pub struct RpcNode {
    pub(crate) url: String,
    #[serde(default)]
    pub(crate) komodo_proxy: bool,
}

impl RpcNode {
    #[cfg(test)]
    pub fn for_test(url: &str) -> Self {
        Self {
            url: url.to_string(),
            komodo_proxy: false,
        }
    }
}

// ————————————————————————————————————————————————————————————————
// Protocol / configuration types
// ————————————————————————————————————————————————————————————————

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TendermintFeeDetails {
    pub coin: String,
    pub amount: BigDecimal,
    #[serde(skip)]
    pub uamount: u64,
    pub gas_limit: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TendermintProtocolInfo {
    pub decimals: u8,
    pub(crate) denom: Denom,
    pub account_prefix: String,
    pub chain_id: ChainId,
    pub(super) gas_price: Option<f64>,
}

#[derive(Clone)]
pub struct ActivatedTokenInfo {
    pub(crate) decimals: u8,
    pub ticker: String,
    pub(crate) denom: Denom,
}

pub struct TendermintConf {
    pub(super) avg_blocktime: u8,
    pub(super) derivation_path: Option<HDPathToCoin>,
}

impl TendermintConf {
    pub fn try_from_json(ticker: &str, conf: &Json) -> MmResult<Self, TendermintInitError> {
        let avg_blocktime = conf.get("avg_blocktime").or_mm_err(|| TendermintInitError {
            ticker: ticker.to_string(),
            kind: TendermintInitErrorKind::AvgBlockTimeMissing,
        })?;

        let avg_blocktime = avg_blocktime.as_i64().or_mm_err(|| TendermintInitError {
            ticker: ticker.to_string(),
            kind: TendermintInitErrorKind::AvgBlockTimeInvalid,
        })?;

        let avg_blocktime = u8::try_from(avg_blocktime).map_to_mm(|_| TendermintInitError {
            ticker: ticker.to_string(),
            kind: TendermintInitErrorKind::AvgBlockTimeInvalid,
        })?;

        let derivation_path =
            serde_json::from_value(conf["derivation_path"].clone()).map_to_mm(|e| TendermintInitError {
                ticker: ticker.to_string(),
                kind: TendermintInitErrorKind::ErrorDeserializingDerivationPath(e.to_string()),
            })?;

        Ok(TendermintConf {
            avg_blocktime,
            derivation_path,
        })
    }
}

// ————————————————————————————————————————————————————————————————
// Activation policy (key dispatch)
// ————————————————————————————————————————————————————————————————

pub enum TendermintActivationPolicy {
    PrivateKey(PrivKeyPolicy<TendermintKeyPair>),
}

impl TendermintActivationPolicy {
    pub fn with_private_key_policy(private_key_policy: PrivKeyPolicy<TendermintKeyPair>) -> Self {
        Self::PrivateKey(private_key_policy)
    }

    pub(super) fn generate_account_id(&self, account_prefix: &str) -> Result<AccountId, ErrorReport> {
        match self {
            Self::PrivateKey(priv_key_policy) => {
                let pk = priv_key_policy.key_pair().ok_or_else(|| {
                    ErrorReport::new(io::Error::new(io::ErrorKind::NotFound, "Activated key not found"))
                })?;
                account_id_from_privkey(pk.private_key_secret.as_slice(), account_prefix)
                    .map_err(|e| ErrorReport::new(io::Error::new(io::ErrorKind::InvalidData, e.to_string())))
            },
        }
    }

    pub(super) fn public_key(&self) -> Result<cosmrs::tendermint::PublicKey, io::Error> {
        match self {
            Self::PrivateKey(private_key_policy) => match private_key_policy {
                PrivKeyPolicy::KeyPair(pair) => cosmrs::tendermint::PublicKey::from_raw_secp256k1(&*pair.public_key)
                    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Couldn't generate public key")),
                PrivKeyPolicy::HDWallet { activated_key, .. } => {
                    cosmrs::tendermint::PublicKey::from_raw_secp256k1(&*activated_key.public_key)
                        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Couldn't generate public key"))
                },
                PrivKeyPolicy::Trezor => Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "Trezor is not supported yet!",
                )),
            },
        }
    }

    pub(crate) fn activated_key_or_err(&self) -> Result<&Secp256k1Secret, MmError<PrivKeyNotAllowed>> {
        match self {
            Self::PrivateKey(private_key) => Ok(private_key.key_pair_or_err()?.private_key_secret.as_ref()),
        }
    }

    pub(crate) fn activated_key(&self) -> Option<Secp256k1Secret> {
        match self {
            Self::PrivateKey(private_key) => Some(*private_key.key_pair()?.private_key_secret.as_ref()),
        }
    }
}

// ————————————————————————————————————————————————————————————————
// RPC client wrapper
// ————————————————————————————————————————————————————————————————

pub(super) struct TendermintRpcClient(pub(super) AsyncMutex<TendermintRpcClientImpl>);

pub(super) struct TendermintRpcClientImpl {
    pub(super) rpc_clients: Vec<HttpClient>,
}

// ————————————————————————————————————————————————————————————————
// Core coin struct
// ————————————————————————————————————————————————————————————————

pub struct TendermintCoinImpl {
    pub(super) ticker: String,
    pub(super) avg_blocktime: u8,
    pub account_id: AccountId,
    pub activation_policy: TendermintActivationPolicy,
    pub tokens_info: PaMutex<HashMap<String, ActivatedTokenInfo>>,
    pub(crate) history_sync_state: Mutex<HistorySyncState>,
    pub(super) client: TendermintRpcClient,
    pub ctx: MmWeak,
    pub(crate) protocol_info: TendermintProtocolInfo,
    /// Whether activation should report balances in its result. Captured at
    /// activation time because the platform-with-tokens framework builds the
    /// activation result from `&self` without access to the original request.
    pub(super) get_balances: bool,
}

#[derive(Clone)]
pub struct TendermintCoin(pub(super) Arc<TendermintCoinImpl>);

impl TendermintCoinImpl {
    /// Assemble the inner coin implementation from already-resolved activation
    /// inputs. Sets history syncing to `NotEnabled` and starts with no
    /// activated tokens; tokens are registered later via
    /// [`TendermintCoin::add_activated_token_info`].
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        ticker: String,
        conf: TendermintConf,
        protocol_info: TendermintProtocolInfo,
        account_id: AccountId,
        activation_policy: TendermintActivationPolicy,
        rpc_clients: Vec<HttpClient>,
        ctx: MmWeak,
        get_balances: bool,
    ) -> Self {
        TendermintCoinImpl {
            ticker,
            avg_blocktime: conf.avg_blocktime,
            account_id,
            activation_policy,
            tokens_info: PaMutex::new(HashMap::new()),
            history_sync_state: Mutex::new(HistorySyncState::NotEnabled),
            client: TendermintRpcClient(AsyncMutex::new(TendermintRpcClientImpl { rpc_clients })),
            ctx,
            protocol_info,
            get_balances,
        }
    }
}

impl From<TendermintCoinImpl> for TendermintCoin {
    fn from(coin_impl: TendermintCoinImpl) -> Self { TendermintCoin(Arc::new(coin_impl)) }
}

impl std::fmt::Debug for TendermintCoin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "TendermintCoin({})", self.ticker) }
}

impl Deref for TendermintCoin {
    type Target = TendermintCoinImpl;
    fn deref(&self) -> &Self::Target { &self.0 }
}

// ————————————————————————————————————————————————————————————————
// CosmosTransaction (TransactionEnum variant)
// ————————————————————————————————————————————————————————————————

#[derive(Clone, Debug, PartialEq)]
pub struct CosmosTransaction {
    pub data: TxRaw,
}

impl crate::Transaction for CosmosTransaction {
    fn tx_hex(&self) -> Vec<u8> { self.data.encode_to_vec() }

    fn tx_hash(&self) -> BytesJson {
        let bytes = self.data.encode_to_vec();
        let hash = sha256(&bytes);
        hash.to_vec().into()
    }
}

impl From<cosmrs::tx::Raw> for CosmosTransaction {
    fn from(raw: cosmrs::tx::Raw) -> Self { CosmosTransaction { data: raw.into() } }
}

// ————————————————————————————————————————————————————————————————
// Error types
// ————————————————————————————————————————————————————————————————

#[derive(Debug, Clone)]
pub struct TendermintInitError {
    pub ticker: String,
    pub kind: TendermintInitErrorKind,
}

#[derive(Display, Debug, Clone)]
pub enum TendermintInitErrorKind {
    Internal(String),
    InvalidPrivKey(String),
    CouldNotGenerateAccountId(String),
    EmptyRpcUrls,
    RpcClientInitError(String),
    AvgBlockTimeMissing,
    AvgBlockTimeInvalid,
    ErrorDeserializingDerivationPath(String),
}

impl std::fmt::Display for TendermintInitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TendermintInitError({}): {}", self.ticker, self.kind)
    }
}

#[derive(Display, Debug)]
pub enum TendermintCoinRpcError {
    #[display(fmt = "Prost decode error: {}", _0)]
    Prost(String),
    #[display(fmt = "RPC client error: {}", _0)]
    RpcClientError(String),
    #[display(fmt = "Invalid response: {}", _0)]
    InvalidResponse(String),
    #[display(fmt = "Internal error: {}", _0)]
    InternalError(String),
    #[display(fmt = "Account type '{}' is not supported for HTLCs", prefix)]
    UnexpectedAccountType { prefix: String },
    #[display(fmt = "Performance fee is too low")]
    PerformanceFeeIsTooLow,
}

impl From<DecodeError> for TendermintCoinRpcError {
    fn from(e: DecodeError) -> Self { TendermintCoinRpcError::Prost(e.to_string()) }
}

impl From<tendermint_rpc::Error> for TendermintCoinRpcError {
    fn from(e: tendermint_rpc::Error) -> Self { TendermintCoinRpcError::RpcClientError(e.to_string()) }
}

#[cfg(target_arch = "wasm32")]
impl From<crate::tendermint::rpc::PerformError> for TendermintCoinRpcError {
    fn from(e: crate::tendermint::rpc::PerformError) -> Self { TendermintCoinRpcError::RpcClientError(e.to_string()) }
}

impl From<TendermintCoinRpcError> for crate::WithdrawError {
    fn from(e: TendermintCoinRpcError) -> Self { crate::WithdrawError::Transport(e.to_string()) }
}

impl From<TendermintCoinRpcError> for crate::BalanceError {
    fn from(e: TendermintCoinRpcError) -> Self { crate::BalanceError::Transport(e.to_string()) }
}

impl From<TendermintCoinRpcError> for crate::RawTransactionError {
    fn from(e: TendermintCoinRpcError) -> Self { crate::RawTransactionError::Transport(e.to_string()) }
}

#[derive(Display, Debug)]
pub(super) enum SearchForSwapTxSpendErr {
    #[display(fmt = "{}", _0)]
    Cosmrs(ErrorReport),
    #[display(fmt = "{}", _0)]
    Rpc(TendermintCoinRpcError),
    TxMessagesEmpty,
    ClaimHtlcTxNotFound,
    #[display(fmt = "Unexpected HTLC state: {}", _0)]
    UnexpectedHtlcState(i32),
    #[display(fmt = "Account type '{}' is not supported for HTLCs", prefix)]
    UnexpectedAccountType {
        prefix: String,
    },
    #[display(fmt = "{}", _0)]
    Proto(DecodeError),
}

impl From<ErrorReport> for SearchForSwapTxSpendErr {
    fn from(e: ErrorReport) -> Self { SearchForSwapTxSpendErr::Cosmrs(e) }
}

impl From<TendermintCoinRpcError> for SearchForSwapTxSpendErr {
    fn from(e: TendermintCoinRpcError) -> Self { SearchForSwapTxSpendErr::Rpc(e) }
}

impl From<DecodeError> for SearchForSwapTxSpendErr {
    fn from(e: DecodeError) -> Self { SearchForSwapTxSpendErr::Proto(e) }
}

#[derive(Display, Debug)]
pub enum AccountIdFromPubkeyHexErr {
    InvalidHexString(FromHexError),
    CouldNotCreateAccountId(ErrorReport),
}

impl From<FromHexError> for AccountIdFromPubkeyHexErr {
    fn from(err: FromHexError) -> Self { AccountIdFromPubkeyHexErr::InvalidHexString(err) }
}

impl From<ErrorReport> for AccountIdFromPubkeyHexErr {
    fn from(err: ErrorReport) -> Self { AccountIdFromPubkeyHexErr::CouldNotCreateAccountId(err) }
}

/// Error type for HTLC message construction.
#[derive(Display, Debug)]
pub(super) enum HtlcMsgError {
    #[display(fmt = "Not supported: {}", _0)]
    NotSupported(String),
    #[display(fmt = "Invalid input: {}", _0)]
    InvalidInput(String),
}

// ————————————————————————————————————————————————————————————————
// Free functions
// ————————————————————————————————————————————————————————————————

pub(crate) fn account_id_from_privkey(priv_key: &[u8], prefix: &str) -> MmResult<AccountId, TendermintInitErrorKind> {
    let signing_key = cosmrs::crypto::secp256k1::SigningKey::from_slice(priv_key)
        .map_to_mm(|e| TendermintInitErrorKind::InvalidPrivKey(e.to_string()))?;

    signing_key
        .public_key()
        .account_id(prefix)
        .map_to_mm(|e| TendermintInitErrorKind::CouldNotGenerateAccountId(e.to_string()))
}

fn account_id_from_raw_pubkey(prefix: &str, pubkey: &[u8]) -> Result<AccountId, ErrorReport> {
    let tendermint_pk = cosmrs::tendermint::PublicKey::from_raw_secp256k1(pubkey).ok_or_else(|| {
        ErrorReport::new(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid secp256k1 pubkey bytes",
        ))
    })?;
    let cosmrs_pk = cosmrs::crypto::PublicKey::from(tendermint_pk);
    cosmrs_pk.account_id(prefix)
}

pub fn account_id_from_pubkey_hex(prefix: &str, pubkey: &str) -> Result<AccountId, AccountIdFromPubkeyHexErr> {
    let pubkey_bytes = hex::decode(pubkey)?;
    Ok(account_id_from_raw_pubkey(prefix, &pubkey_bytes)?)
}

pub(super) fn parse_expected_sequence_number(e: &str) -> MmResult<u64, TendermintCoinRpcError> {
    // Parse "expected N" from the error message without regex.
    if let Some(idx) = e.find("expected ") {
        let rest = &e[idx + "expected ".len()..];
        let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !num_str.is_empty() {
            let account_sequence = u64::from_str_radix(&num_str, 10)
                .map_to_mm(|e| TendermintCoinRpcError::InternalError(e.to_string()))?;
            return Ok(account_sequence);
        }
    }

    MmError::err(TendermintCoinRpcError::InternalError(format!(
        "Could not parse the expected sequence number from: '{e}'"
    )))
}
