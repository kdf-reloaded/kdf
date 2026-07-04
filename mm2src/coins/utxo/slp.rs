//! The module implementing Simple Ledger Protocol (SLP) support.
//! It's a custom token format mostly used on the Bitcoin Cash blockchain.
//! Tracking issue: https://github.com/KomodoPlatform/atomicDEX-API/issues/701
//! More info about the protocol and implementation guides can be found at https://slp.dev/

use crate::utxo::bch::BchCoin;
use crate::utxo::bchd_grpc::{check_slp_transaction, validate_slp_utxos, ValidateSlpUtxosErr};
use crate::utxo::rpc_clients::{UnspentInfo, UtxoRpcClientEnum, UtxoRpcError, UtxoRpcResult};
use crate::utxo::utxo_common::{self, big_decimal_from_sat_unsigned, payment_script, UtxoTxBuilder};
use crate::utxo::{generate_and_send_tx, sat_from_big_decimal, ActualTxFee, AdditionalTxData, BroadcastTxErr,
                  FeePolicy, GenerateTxError, RecentlySpentOutPointsGuard, UtxoCoinConf, UtxoCoinFields,
                  UtxoCommonOps, UtxoTx, UtxoTxBroadcastOps, UtxoTxGenerationOps};
use crate::{BalanceFut, CoinBalance, DexFee, FeeApproxStage, FoundSwapTxSpend, HistorySyncState, MarketCoinOps,
            MmCoin, NegotiateSwapContractAddrErr, NumConversError, PrivKeyNotAllowed, RawTransactionFut,
            RawTransactionRequest, SignRawTransactionRequest, SignatureResult, SwapOps, TradeFee, TradePreimageError,
            TradePreimageFut, TradePreimageResult, TradePreimageValue, TransactionDetails, TransactionEnum,
            TransactionErr, TransactionFut, TxFeeDetails, UnexpectedDerivationMethod, ValidateAddressResult,
            ValidateFeeArgs, ValidatePaymentInput, VerificationError, VerificationResult, WatcherOps, WithdrawError,
            WithdrawFee, WithdrawFut, WithdrawRequest};
use async_trait::async_trait;
use chain::constants::SEQUENCE_FINAL;
use chain::{OutPoint, TransactionOutput};
use common::log::warn;
use common::mm_number::{BigDecimal, MmNumber};
use common::now_ms;
use crypto::privkey::key_pair_from_secret;
use derive_more::Display;
use futures::compat::Future01CompatExt;
use futures::{FutureExt, TryFutureExt};
use futures01::Future;
use hex::FromHexError;
use kdf_crypto::dhash160;
use keys::hash::H160;
use keys::{AddressHashEnum, CashAddrType, CashAddress, CompactSignature, KeyPair, NetworkPrefix as CashAddrPrefix,
           Public};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use primitives::hash::H256;
use rpc::v1::types::{Bytes as BytesJson, ToTxHash, H256 as H256Json};
use script::bytes::Bytes;
use script::{Builder as ScriptBuilder, Opcode, Script, TransactionInputSigner};
use serde_json::Value as Json;
use serialization::{deserialize, serialize, Deserializable, Error, Reader};
use serialization_derive::Deserializable;
use std::convert::TryInto;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;
use utxo_signer::with_key_pair::{p2pkh_spend, p2sh_spend, sign_tx, UtxoSignWithKeyPairError};

const SLP_SWAP_VOUT: usize = 1;
const SLP_FEE_VOUT: usize = 1;
const SLP_HTLC_SPEND_SIZE: u64 = 555;
const SLP_LOKAD_ID: &str = "SLP\x00";
const SLP_FUNGIBLE: u8 = 1;
const SLP_SEND: &str = "SEND";
const SLP_MINT: &str = "MINT";
const SLP_GENESIS: &str = "GENESIS";

#[path = "slp/slp_token_ops.rs"] mod slp_token_ops;

#[path = "slp/slp_swap_ops.rs"] mod slp_swap_ops;

#[path = "slp/slp_coin_ops.rs"] mod slp_coin_ops;

#[derive(Debug)]
pub struct SlpTokenConf {
    decimals: u8,
    ticker: String,
    token_id: H256,
    required_confirmations: AtomicU64,
}

/// Minimalistic info that is used to be stored outside of the token's context
/// E.g. in the platform BCHCoin
#[derive(Debug)]
pub struct SlpTokenInfo {
    pub token_id: H256,
    pub decimals: u8,
}

#[derive(Clone, Debug)]
pub struct SlpToken {
    conf: Arc<SlpTokenConf>,
    platform_coin: BchCoin,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SlpUnspent {
    pub bch_unspent: UnspentInfo,
    pub slp_amount: u64,
}

#[derive(Clone, Debug)]
pub struct SlpOutput {
    pub amount: u64,
    pub script_pubkey: Bytes,
}

/// The SLP transaction preimage
pub(crate) struct SlpTxPreimage {
    slp_inputs: Vec<SlpUnspent>,
    available_bch_inputs: Vec<UnspentInfo>,
    outputs: Vec<TransactionOutput>,
}

#[derive(Debug, Display)]
pub(crate) enum ValidateHtlcError {
    TxLackOfOutputs,
    #[display(fmt = "TxParseError: {:?}", _0)]
    TxParseError(Error),
    #[display(fmt = "OpReturnParseError: {:?}", _0)]
    OpReturnParseError(ParseSlpScriptError),
    InvalidSlpDetails,
    InvalidSlpUtxo(ValidateSlpUtxosErr),
    NumConversionErr(NumConversError),
    ValidatePaymentError(String),
    UnexpectedDerivationMethod(UnexpectedDerivationMethod),
}

impl From<NumConversError> for ValidateHtlcError {
    fn from(err: NumConversError) -> ValidateHtlcError { ValidateHtlcError::NumConversionErr(err) }
}

impl From<ParseSlpScriptError> for ValidateHtlcError {
    fn from(err: ParseSlpScriptError) -> Self { ValidateHtlcError::OpReturnParseError(err) }
}

impl From<ValidateSlpUtxosErr> for ValidateHtlcError {
    fn from(err: ValidateSlpUtxosErr) -> Self { ValidateHtlcError::InvalidSlpUtxo(err) }
}

impl From<UnexpectedDerivationMethod> for ValidateHtlcError {
    fn from(e: UnexpectedDerivationMethod) -> Self { ValidateHtlcError::UnexpectedDerivationMethod(e) }
}

#[derive(Debug, Display)]
pub(crate) enum ValidateDexFeeError {
    TxLackOfOutputs,
    #[display(fmt = "OpReturnParseError: {:?}", _0)]
    OpReturnParseError(ParseSlpScriptError),
    InvalidSlpDetails,
    NumConversionErr(NumConversError),
    ValidatePaymentError(String),
}

impl From<NumConversError> for ValidateDexFeeError {
    fn from(err: NumConversError) -> ValidateDexFeeError { ValidateDexFeeError::NumConversionErr(err) }
}

impl From<ParseSlpScriptError> for ValidateDexFeeError {
    fn from(err: ParseSlpScriptError) -> Self { ValidateDexFeeError::OpReturnParseError(err) }
}

#[allow(clippy::upper_case_acronyms, clippy::large_enum_variant)]
#[derive(Debug, Display)]
pub enum SpendP2SHError {
    GenerateTxErr(GenerateTxError),
    Rpc(UtxoRpcError),
    SignTxErr(UtxoSignWithKeyPairError),
    PrivKeyNotAllowed(PrivKeyNotAllowed),
    UnexpectedDerivationMethod(UnexpectedDerivationMethod),
    String(String),
}

impl From<GenerateTxError> for SpendP2SHError {
    fn from(err: GenerateTxError) -> SpendP2SHError { SpendP2SHError::GenerateTxErr(err) }
}

impl From<UtxoRpcError> for SpendP2SHError {
    fn from(err: UtxoRpcError) -> SpendP2SHError { SpendP2SHError::Rpc(err) }
}

impl From<UtxoSignWithKeyPairError> for SpendP2SHError {
    fn from(sign: UtxoSignWithKeyPairError) -> SpendP2SHError { SpendP2SHError::SignTxErr(sign) }
}

impl From<PrivKeyNotAllowed> for SpendP2SHError {
    fn from(e: PrivKeyNotAllowed) -> Self { SpendP2SHError::PrivKeyNotAllowed(e) }
}

impl From<UnexpectedDerivationMethod> for SpendP2SHError {
    fn from(e: UnexpectedDerivationMethod) -> Self { SpendP2SHError::UnexpectedDerivationMethod(e) }
}

impl From<String> for SpendP2SHError {
    fn from(err: String) -> SpendP2SHError { SpendP2SHError::String(err) }
}

#[derive(Debug, Display)]
pub enum SpendHtlcError {
    TxLackOfOutputs,
    #[display(fmt = "DeserializationErr: {:?}", _0)]
    DeserializationErr(Error),
    #[display(fmt = "PubkeyParseError: {:?}", _0)]
    PubkeyParseErr(keys::Error),
    InvalidSlpDetails,
    NumConversionErr(NumConversError),
    RpcErr(UtxoRpcError),
    #[allow(clippy::upper_case_acronyms)]
    SpendP2SHErr(SpendP2SHError),
    OpReturnParseError(ParseSlpScriptError),
    UnexpectedDerivationMethod(UnexpectedDerivationMethod),
}

impl From<UnexpectedDerivationMethod> for SpendHtlcError {
    fn from(e: UnexpectedDerivationMethod) -> Self { SpendHtlcError::UnexpectedDerivationMethod(e) }
}

impl From<NumConversError> for SpendHtlcError {
    fn from(err: NumConversError) -> SpendHtlcError { SpendHtlcError::NumConversionErr(err) }
}

impl From<Error> for SpendHtlcError {
    fn from(err: Error) -> SpendHtlcError { SpendHtlcError::DeserializationErr(err) }
}

impl From<keys::Error> for SpendHtlcError {
    fn from(err: keys::Error) -> SpendHtlcError { SpendHtlcError::PubkeyParseErr(err) }
}

impl From<SpendP2SHError> for SpendHtlcError {
    fn from(err: SpendP2SHError) -> SpendHtlcError { SpendHtlcError::SpendP2SHErr(err) }
}

impl From<UtxoRpcError> for SpendHtlcError {
    fn from(err: UtxoRpcError) -> SpendHtlcError { SpendHtlcError::RpcErr(err) }
}

impl From<ParseSlpScriptError> for SpendHtlcError {
    fn from(err: ParseSlpScriptError) -> Self { SpendHtlcError::OpReturnParseError(err) }
}

fn slp_send_output(token_id: &H256, amounts: &[u64]) -> TransactionOutput {
    let mut script_builder = ScriptBuilder::default()
        .push_opcode(Opcode::OP_RETURN)
        .push_data(SLP_LOKAD_ID.as_bytes())
        .push_data(&[SLP_FUNGIBLE])
        .push_data(SLP_SEND.as_bytes())
        .push_data(token_id.as_slice());
    for amount in amounts {
        script_builder = script_builder.push_data(&amount.to_be_bytes());
    }
    TransactionOutput {
        value: 0,
        script_pubkey: script_builder.into_bytes(),
    }
}

pub fn slp_genesis_output(
    ticker: &str,
    name: &str,
    token_document_url: Option<&str>,
    token_document_hash: Option<H256>,
    decimals: u8,
    mint_baton_vout: Option<u8>,
    initial_token_mint_quantity: u64,
) -> TransactionOutput {
    let mut script_builder = ScriptBuilder::default()
        .push_opcode(Opcode::OP_RETURN)
        .push_data(SLP_LOKAD_ID.as_bytes())
        .push_data(&[SLP_FUNGIBLE])
        .push_data(SLP_GENESIS.as_bytes())
        .push_data(ticker.as_bytes())
        .push_data(name.as_bytes());

    script_builder = match token_document_url {
        Some(url) => script_builder.push_data(url.as_bytes()),
        None => script_builder
            .push_opcode(Opcode::OP_PUSHDATA1)
            .push_opcode(Opcode::OP_0),
    };

    script_builder = match token_document_hash {
        Some(hash) => script_builder.push_data(hash.as_slice()),
        None => script_builder
            .push_opcode(Opcode::OP_PUSHDATA1)
            .push_opcode(Opcode::OP_0),
    };

    script_builder = script_builder.push_data(&[decimals]);
    script_builder = match mint_baton_vout {
        Some(vout) => script_builder.push_data(&[vout]),
        None => script_builder
            .push_opcode(Opcode::OP_PUSHDATA1)
            .push_opcode(Opcode::OP_0),
    };

    script_builder = script_builder.push_data(&initial_token_mint_quantity.to_be_bytes());
    TransactionOutput {
        value: 0,
        script_pubkey: script_builder.into_bytes(),
    }
}

#[derive(Debug)]
pub struct SlpProtocolConf {
    pub platform_coin_ticker: String,
    pub token_id: H256,
    pub decimals: u8,
    pub required_confirmations: Option<u64>,
}

#[derive(Debug)]
pub struct ValidateHtlcInput {
    tx: Vec<u8>,
    other_pub: Public,
    my_pub: Public,
    time_lock: u32,
    secret_hash: Vec<u8>,
    amount: BigDecimal,
    confirmations: u64,
}

#[derive(Debug, Eq, PartialEq)]
pub struct SlpGenesisParams {
    pub(super) token_ticker: String,
    token_name: String,
    token_document_url: String,
    token_document_hash: Vec<u8>,
    pub(super) decimals: Vec<u8>,
    pub(super) mint_baton_vout: Option<u8>,
    pub(super) initial_token_mint_quantity: u64,
}

/// https://slp.dev/specs/slp-token-type-1/#transaction-detail
#[derive(Debug, Eq, PartialEq)]
pub enum SlpTransaction {
    /// https://slp.dev/specs/slp-token-type-1/#genesis-token-genesis-transaction
    Genesis(SlpGenesisParams),
    /// https://slp.dev/specs/slp-token-type-1/#mint-extended-minting-transaction
    Mint {
        token_id: H256,
        mint_baton_vout: Option<u8>,
        additional_token_quantity: u64,
    },
    /// https://slp.dev/specs/slp-token-type-1/#send-spend-transaction
    Send { token_id: H256, amounts: Vec<u64> },
}

impl SlpTransaction {
    pub fn token_id(&self) -> Option<H256> {
        match self {
            SlpTransaction::Send { token_id, .. } | SlpTransaction::Mint { token_id, .. } => Some(*token_id),
            SlpTransaction::Genesis(_) => None,
        }
    }
}

impl Deserializable for SlpTransaction {
    fn deserialize<T>(reader: &mut Reader<T>) -> Result<Self, Error>
    where
        Self: Sized,
        T: std::io::Read,
    {
        let transaction_type: String = reader.read()?;
        match transaction_type.as_str() {
            SLP_GENESIS => {
                let token_ticker = reader.read()?;
                let token_name = reader.read()?;
                let maybe_push_op_code: u8 = reader.read()?;
                let token_document_url = if maybe_push_op_code == Opcode::OP_PUSHDATA1 as u8 {
                    reader.read()?
                } else {
                    let mut url = vec![0; maybe_push_op_code as usize];
                    reader.read_slice(&mut url)?;
                    String::from_utf8(url).map_err(|e| Error::Custom(e.to_string()))?
                };

                let maybe_push_op_code: u8 = reader.read()?;
                let token_document_hash = if maybe_push_op_code == Opcode::OP_PUSHDATA1 as u8 {
                    reader.read_list()?
                } else {
                    let mut hash = vec![0; maybe_push_op_code as usize];
                    reader.read_slice(&mut hash)?;
                    hash
                };
                let decimals = reader.read_list()?;
                let maybe_push_op_code: u8 = reader.read()?;
                let mint_baton_vout = if maybe_push_op_code == Opcode::OP_PUSHDATA1 as u8 {
                    let _zero: u8 = reader.read()?;
                    None
                } else {
                    Some(reader.read()?)
                };
                let bytes: Vec<u8> = reader.read_list()?;
                if bytes.len() != 8 {
                    return Err(Error::Custom(format!("Expected 8 bytes, got {}", bytes.len())));
                }
                let initial_token_mint_quantity = u64::from_be_bytes(bytes.try_into().expect("length is 8 bytes"));

                Ok(SlpTransaction::Genesis(SlpGenesisParams {
                    token_ticker,
                    token_name,
                    token_document_url,
                    token_document_hash,
                    decimals,
                    mint_baton_vout,
                    initial_token_mint_quantity,
                }))
            },
            SLP_MINT => {
                let maybe_id: Vec<u8> = reader.read_list()?;
                if maybe_id.len() != 32 {
                    return Err(Error::Custom(format!("Unexpected token id length {}", maybe_id.len())));
                }

                let maybe_push_op_code: u8 = reader.read()?;
                let mint_baton_vout = if maybe_push_op_code == Opcode::OP_PUSHDATA1 as u8 {
                    let _zero: u8 = reader.read()?;
                    None
                } else {
                    Some(reader.read()?)
                };

                let bytes: Vec<u8> = reader.read_list()?;
                if bytes.len() != 8 {
                    return Err(Error::Custom(format!("Expected 8 bytes, got {}", bytes.len())));
                }
                let additional_token_quantity = u64::from_be_bytes(bytes.try_into().expect("length is 8 bytes"));

                Ok(SlpTransaction::Mint {
                    token_id: H256::from(maybe_id.as_slice()),
                    mint_baton_vout,
                    additional_token_quantity,
                })
            },
            SLP_SEND => {
                let maybe_id: Vec<u8> = reader.read_list()?;
                if maybe_id.len() != 32 {
                    return Err(Error::Custom(format!("Unexpected token id length {}", maybe_id.len())));
                }

                let token_id = H256::from(maybe_id.as_slice());
                let mut amounts = Vec::with_capacity(1);
                while !reader.is_finished() {
                    let bytes: Vec<u8> = reader.read_list()?;
                    if bytes.len() != 8 {
                        return Err(Error::Custom(format!("Expected 8 bytes, got {}", bytes.len())));
                    }
                    let amount = u64::from_be_bytes(bytes.try_into().expect("length is 8 bytes"));
                    amounts.push(amount)
                }

                if amounts.len() > 19 {
                    return Err(Error::Custom(format!(
                        "Expected at most 19 token amounts, got {}",
                        amounts.len()
                    )));
                }
                Ok(SlpTransaction::Send { token_id, amounts })
            },
            _ => Err(Error::Custom(format!(
                "Unsupported transaction type {}",
                transaction_type
            ))),
        }
    }
}

#[derive(Debug, Deserializable)]
pub struct SlpTxDetails {
    op_code: u8,
    lokad_id: String,
    token_type: Vec<u8>,
    pub transaction: SlpTransaction,
}

#[derive(Debug, Display, PartialEq)]
pub enum ParseSlpScriptError {
    NotOpReturn,
    UnexpectedLokadId(String),
    #[display(fmt = "UnexpectedTokenType: {:?}", _0)]
    UnexpectedTokenType(Vec<u8>),
    #[display(fmt = "DeserializeFailed: {:?}", _0)]
    DeserializeFailed(Error),
}

impl From<Error> for ParseSlpScriptError {
    fn from(err: Error) -> ParseSlpScriptError { ParseSlpScriptError::DeserializeFailed(err) }
}

pub fn parse_slp_script(script: &[u8]) -> Result<SlpTxDetails, MmError<ParseSlpScriptError>> {
    let details: SlpTxDetails = deserialize(script)?;
    if Opcode::from_u8(details.op_code) != Some(Opcode::OP_RETURN) {
        return MmError::err(ParseSlpScriptError::NotOpReturn);
    }

    if details.lokad_id != SLP_LOKAD_ID {
        return MmError::err(ParseSlpScriptError::UnexpectedLokadId(details.lokad_id));
    }

    if details.token_type.first() != Some(&SLP_FUNGIBLE) {
        return MmError::err(ParseSlpScriptError::UnexpectedTokenType(details.token_type));
    }

    Ok(details)
}

#[derive(Debug, Display)]
pub(crate) enum GenSlpSpendErr {
    RpcError(UtxoRpcError),
    TooManyOutputs,
    #[display(
        fmt = "Not enough {} to generate SLP spend: available {}, required at least {}",
        coin,
        available,
        required
    )]
    InsufficientSlpBalance {
        coin: String,
        available: BigDecimal,
        required: BigDecimal,
    },
    InvalidSlpUtxos(ValidateSlpUtxosErr),
    Internal(String),
}

impl From<UtxoRpcError> for GenSlpSpendErr {
    fn from(err: UtxoRpcError) -> GenSlpSpendErr { GenSlpSpendErr::RpcError(err) }
}

impl From<ValidateSlpUtxosErr> for GenSlpSpendErr {
    fn from(err: ValidateSlpUtxosErr) -> GenSlpSpendErr { GenSlpSpendErr::InvalidSlpUtxos(err) }
}

impl From<UnexpectedDerivationMethod> for GenSlpSpendErr {
    fn from(e: UnexpectedDerivationMethod) -> Self { GenSlpSpendErr::Internal(e.to_string()) }
}

impl From<GenSlpSpendErr> for WithdrawError {
    fn from(err: GenSlpSpendErr) -> WithdrawError {
        match err {
            GenSlpSpendErr::RpcError(e) => e.into(),
            GenSlpSpendErr::TooManyOutputs | GenSlpSpendErr::InvalidSlpUtxos(_) => {
                WithdrawError::InternalError(err.to_string())
            },
            GenSlpSpendErr::InsufficientSlpBalance {
                coin,
                available,
                required,
            } => WithdrawError::NotSufficientBalance {
                coin,
                available,
                required,
            },
            GenSlpSpendErr::Internal(internal) => WithdrawError::InternalError(internal),
        }
    }
}

impl AsRef<UtxoCoinFields> for SlpToken {
    fn as_ref(&self) -> &UtxoCoinFields { self.platform_coin.as_ref() }
}

#[async_trait]
impl UtxoTxBroadcastOps for SlpToken {
    async fn broadcast_tx(&self, tx: &UtxoTx) -> Result<H256Json, MmError<BroadcastTxErr>> {
        let tx_bytes = serialize(tx);
        check_slp_transaction(self.platform_coin.bchd_urls(), tx_bytes.clone().take())
            .await
            .mm_err(|e| BroadcastTxErr::Other(e.to_string()))?;

        let hash = self
            .rpc()
            .send_raw_transaction(tx_bytes.into())
            .compat()
            .await
            .mm_err(Into::into)?;

        Ok(hash)
    }
}

#[async_trait]
impl UtxoTxGenerationOps for SlpToken {
    async fn get_tx_fee(&self) -> UtxoRpcResult<ActualTxFee> { self.platform_coin.get_tx_fee().await }

    async fn calc_interest_if_required(
        &self,
        unsigned: TransactionInputSigner,
        data: AdditionalTxData,
        my_script_pub: Bytes,
    ) -> UtxoRpcResult<(TransactionInputSigner, AdditionalTxData)> {
        self.platform_coin
            .calc_interest_if_required(unsigned, data, my_script_pub)
            .await
    }
}

impl MarketCoinOps for SlpToken {
    fn ticker(&self) -> &str { &self.conf.ticker }

    fn my_address(&self) -> Result<String, String> {
        let my_address = try_s!(self.as_ref().derivation_method.iguana_or_err());
        let slp_address = try_s!(self.platform_coin.slp_address(my_address));
        slp_address.encode()
    }

    fn get_public_key(&self) -> Result<String, MmError<UnexpectedDerivationMethod>> { unimplemented!() }

    fn sign_message_hash(&self, message: &str) -> Option<[u8; 32]> {
        utxo_common::sign_message_hash(self.as_ref(), message)
    }

    fn sign_message(&self, message: &str) -> SignatureResult<String> {
        utxo_common::sign_message(self.as_ref(), message)
    }

    fn verify_message(&self, signature: &str, message: &str, address: &str) -> VerificationResult<bool> {
        let message_hash = self
            .sign_message_hash(message)
            .ok_or(VerificationError::PrefixNotFound)?;
        let signature = CompactSignature::from(base64::decode(signature)?);
        let pubkey = Public::recover_compact(&H256::from(message_hash), &signature)?;
        let address_from_pubkey = self.platform_coin.address_from_pubkey(&pubkey);
        let slp_address = self
            .platform_coin
            .slp_address(&address_from_pubkey)
            .map_err(VerificationError::InternalError)?
            .encode()
            .map_err(VerificationError::InternalError)?;
        Ok(slp_address == address)
    }

    fn my_balance(&self) -> BalanceFut<CoinBalance> {
        let coin = self.clone();
        let fut = async move { Ok(coin.my_coin_balance().await.mm_err(Into::into)?) };
        Box::new(fut.boxed().compat())
    }

    fn base_coin_balance(&self) -> BalanceFut<BigDecimal> {
        Box::new(self.platform_coin.my_balance().map(|res| res.spendable))
    }

    fn platform_ticker(&self) -> &str { self.platform_coin.ticker() }

    /// Receives raw transaction bytes in hexadecimal format as input and returns tx hash in hexadecimal format
    fn send_raw_tx(&self, tx: &str) -> Box<dyn Future<Item = String, Error = String> + Send> {
        let selfi = self.clone();
        let tx = tx.to_owned();
        let fut = async move {
            let bytes = hex::decode(tx).map_to_mm(|e| e).map_err(|e| format!("{:?}", e))?;
            let tx = try_s!(deserialize(bytes.as_slice()));
            let hash = selfi.broadcast_tx(&tx).await.map_err(|e| format!("{:?}", e))?;
            Ok(format!("{:?}", hash))
        };

        Box::new(fut.boxed().compat())
    }

    fn send_raw_tx_bytes(&self, tx: &[u8]) -> Box<dyn Future<Item = String, Error = String> + Send> {
        let selfi = self.clone();
        let bytes = tx.to_owned();
        let fut = async move {
            let tx = try_s!(deserialize(bytes.as_slice()));
            let hash = selfi.broadcast_tx(&tx).await.map_err(|e| format!("{:?}", e))?;
            Ok(format!("{:?}", hash))
        };

        Box::new(fut.boxed().compat())
    }

    fn wait_for_confirmations(
        &self,
        tx: &[u8],
        confirmations: u64,
        requires_nota: bool,
        wait_until: u64,
        check_every: u64,
    ) -> Box<dyn Future<Item = (), Error = String> + Send> {
        self.platform_coin
            .wait_for_confirmations(tx, confirmations, requires_nota, wait_until, check_every)
    }

    fn wait_for_tx_spend(
        &self,
        transaction: &[u8],
        wait_until: u64,
        from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        utxo_common::wait_for_output_spend(
            self.platform_coin.as_ref(),
            transaction,
            SLP_SWAP_VOUT,
            from_block,
            wait_until,
        )
    }

    fn tx_enum_from_bytes(&self, bytes: &[u8]) -> Result<TransactionEnum, String> {
        self.platform_coin.tx_enum_from_bytes(bytes)
    }

    fn current_block(&self) -> Box<dyn Future<Item = u64, Error = String> + Send> { self.platform_coin.current_block() }

    fn display_priv_key(&self) -> Result<String, String> { self.platform_coin.display_priv_key() }

    fn min_tx_amount(&self) -> BigDecimal { big_decimal_from_sat_unsigned(1, self.decimals()) }

    fn min_trading_vol(&self) -> MmNumber { big_decimal_from_sat_unsigned(1, self.decimals()).into() }

    fn sign_raw_tx(&self, args: &SignRawTransactionRequest) -> RawTransactionFut {
        Box::new(utxo_common::sign_raw_tx(self.clone(), args.clone()).boxed().compat())
    }
}

impl From<GenSlpSpendErr> for TradePreimageError {
    fn from(slp: GenSlpSpendErr) -> TradePreimageError {
        match slp {
            GenSlpSpendErr::InsufficientSlpBalance {
                coin,
                available,
                required,
            } => TradePreimageError::NotSufficientBalance {
                coin,
                available,
                required,
            },
            GenSlpSpendErr::RpcError(e) => e.into(),
            GenSlpSpendErr::TooManyOutputs | GenSlpSpendErr::InvalidSlpUtxos(_) => {
                TradePreimageError::InternalError(slp.to_string())
            },
            GenSlpSpendErr::Internal(internal) => TradePreimageError::InternalError(internal),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SlpFeeDetails {
    pub amount: BigDecimal,
    pub coin: String,
}

impl From<SlpFeeDetails> for TxFeeDetails {
    fn from(slp: SlpFeeDetails) -> TxFeeDetails { TxFeeDetails::Slp(slp) }
}

#[derive(Debug, Display)]
pub enum SlpAddrFromPubkeyErr {
    InvalidHex(hex::FromHexError),
    CashAddrError(String),
    EncodeError(String),
}

impl From<hex::FromHexError> for SlpAddrFromPubkeyErr {
    fn from(err: FromHexError) -> SlpAddrFromPubkeyErr { SlpAddrFromPubkeyErr::InvalidHex(err) }
}

pub fn slp_addr_from_pubkey_str(pubkey: &str, prefix: &str) -> Result<String, MmError<SlpAddrFromPubkeyErr>> {
    let pubkey_bytes = hex::decode(pubkey)?;
    let hash = dhash160(&pubkey_bytes);
    let addr =
        CashAddress::new(prefix, hash.to_vec(), CashAddrType::P2PKH).map_to_mm(SlpAddrFromPubkeyErr::CashAddrError)?;
    addr.encode().map_to_mm(SlpAddrFromPubkeyErr::EncodeError)
}

#[cfg(test)]
mod slp_tests {
    use super::*;
    use crate::utxo::GetUtxoListOps;
    use crate::{utxo::bch::tbch_coin_for_test, TransactionErr};
    use common::block_on;
    use std::mem::discriminant;

    // https://slp.dev/specs/slp-token-type-1/#examples
    #[test]
    fn test_parse_slp_script() {
        // Send single output
        let script = hex::decode("6a04534c500001010453454e4420e73b2b28c14db8ebbf97749988b539508990e1708021067f206f49d55807dbf4080000000005f5e100").unwrap();
        let slp_data = parse_slp_script(&script).unwrap();
        assert_eq!(slp_data.lokad_id, "SLP\0");
        let expected_amount = 100000000u64;
        let expected_transaction = SlpTransaction::Send {
            token_id: "e73b2b28c14db8ebbf97749988b539508990e1708021067f206f49d55807dbf4".into(),
            amounts: vec![expected_amount],
        };

        assert_eq!(expected_transaction, slp_data.transaction);

        // Genesis
        let script =
            hex::decode("6a04534c500001010747454e45534953044144455804414445584c004c0001084c0008000000174876e800")
                .unwrap();
        let slp_data = parse_slp_script(&script).unwrap();
        assert_eq!(slp_data.lokad_id, "SLP\0");
        let initial_token_mint_quantity = 1000_0000_0000u64;
        let expected_transaction = SlpTransaction::Genesis(SlpGenesisParams {
            token_ticker: "ADEX".to_string(),
            token_name: "ADEX".to_string(),
            token_document_url: "".to_string(),
            token_document_hash: vec![],
            decimals: vec![8],
            mint_baton_vout: None,
            initial_token_mint_quantity,
        });

        assert_eq!(expected_transaction, slp_data.transaction);

        // Genesis from docs example
        let script =
            hex::decode("6a04534c500001010747454e45534953045553445423546574686572204c74642e20555320646f6c6c6172206261636b656420746f6b656e734168747470733a2f2f7465746865722e746f2f77702d636f6e74656e742f75706c6f6164732f323031362f30362f546574686572576869746550617065722e70646620db4451f11eda33950670aaf59e704da90117ff7057283b032cfaec77793139160108010208002386f26fc10000").unwrap();
        let slp_data = parse_slp_script(&script).unwrap();
        assert_eq!(slp_data.lokad_id, "SLP\0");
        let initial_token_mint_quantity = 10000000000000000u64;
        let expected_transaction = SlpTransaction::Genesis(SlpGenesisParams {
            token_ticker: "USDT".to_string(),
            token_name: "Tether Ltd. US dollar backed tokens".to_string(),
            token_document_url: "https://tether.to/wp-content/uploads/2016/06/TetherWhitePaper.pdf".to_string(),
            token_document_hash: hex::decode("db4451f11eda33950670aaf59e704da90117ff7057283b032cfaec7779313916")
                .unwrap(),
            decimals: vec![8],
            mint_baton_vout: Some(2),
            initial_token_mint_quantity,
        });

        assert_eq!(expected_transaction, slp_data.transaction);

        // Mint
        let script =
            hex::decode("6a04534c50000101044d494e5420550d19eb820e616a54b8a73372c4420b5a0567d8dc00f613b71c5234dc884b35010208002386f26fc10000").unwrap();
        let slp_data = parse_slp_script(&script).unwrap();
        assert_eq!(slp_data.lokad_id, "SLP\0");
        let expected_transaction = SlpTransaction::Mint {
            token_id: "550d19eb820e616a54b8a73372c4420b5a0567d8dc00f613b71c5234dc884b35".into(),
            mint_baton_vout: Some(2),
            additional_token_quantity: 10000000000000000,
        };

        assert_eq!(expected_transaction, slp_data.transaction);

        // SEND with 3 outputs
        let script = hex::decode("6a04534c500001010453454e4420550d19eb820e616a54b8a73372c4420b5a0567d8dc00f613b71c5234dc884b350800000000000003e80800000000000003e90800000000000003ea").unwrap();
        let token_id = "550d19eb820e616a54b8a73372c4420b5a0567d8dc00f613b71c5234dc884b35".into();

        let slp_data = parse_slp_script(&script).unwrap();
        assert_eq!(slp_data.lokad_id, "SLP\0");
        let expected_transaction = SlpTransaction::Send {
            token_id,
            amounts: vec![1000, 1001, 1002],
        };
        assert_eq!(expected_transaction, slp_data.transaction);

        // NFT Genesis, unsupported token type
        // https://explorer.bitcoin.com/bch/tx/3dc17770ff832726aace53d305e087601d8b27cf881089d7849173736995f43e
        let script = hex::decode("6a04534c500001410747454e45534953055357454443174573736b65657469742043617264204e6f2e20313136302b68747470733a2f2f636f6c6c65637469626c652e73776565742e696f2f7365726965732f35382f313136302040f8d39b6fc8725d9c766d66643d8ec644363ba32391c1d9a89a3edbdea8866a01004c00080000000000000001").unwrap();

        let actual_err = parse_slp_script(&script).unwrap_err().into_inner();
        let expected_err = ParseSlpScriptError::UnexpectedTokenType(vec![0x41]);
        assert_eq!(expected_err, actual_err);
    }

    #[test]
    fn test_slp_send_output() {
        // Send single output
        let expected_script = hex::decode("6a04534c500001010453454e4420e73b2b28c14db8ebbf97749988b539508990e1708021067f206f49d55807dbf4080000000005f5e100").unwrap();
        let expected_output = TransactionOutput {
            value: 0,
            script_pubkey: expected_script.into(),
        };

        let actual_output = slp_send_output(
            &"e73b2b28c14db8ebbf97749988b539508990e1708021067f206f49d55807dbf4".into(),
            &[100000000],
        );

        assert_eq!(expected_output, actual_output);

        let expected_script = hex::decode("6a04534c500001010453454e4420550d19eb820e616a54b8a73372c4420b5a0567d8dc00f613b71c5234dc884b350800005af3107a40000800232bff5f46c000").unwrap();
        let expected_output = TransactionOutput {
            value: 0,
            script_pubkey: expected_script.into(),
        };

        let actual_output = slp_send_output(
            &"550d19eb820e616a54b8a73372c4420b5a0567d8dc00f613b71c5234dc884b35".into(),
            &[100000000000000, 9900000000000000],
        );

        assert_eq!(expected_output, actual_output);
    }

    #[test]
    fn test_slp_genesis_output() {
        let expected_script =
            hex::decode("6a04534c500001010747454e45534953044144455804414445584c004c0001084c0008000000174876e800")
                .unwrap();
        let expected_output = TransactionOutput {
            value: 0,
            script_pubkey: expected_script.into(),
        };

        let actual_output = slp_genesis_output("ADEX", "ADEX", None, None, 8, None, 1000_0000_0000);
        assert_eq!(expected_output, actual_output);

        let expected_script =
            hex::decode("6a04534c500001010747454e45534953045553445423546574686572204c74642e20555320646f6c6c6172206261636b656420746f6b656e734168747470733a2f2f7465746865722e746f2f77702d636f6e74656e742f75706c6f6164732f323031362f30362f546574686572576869746550617065722e70646620db4451f11eda33950670aaf59e704da90117ff7057283b032cfaec77793139160108010208002386f26fc10000")
                .unwrap();
        let expected_output = TransactionOutput {
            value: 0,
            script_pubkey: expected_script.into(),
        };

        let actual_output = slp_genesis_output(
            "USDT",
            "Tether Ltd. US dollar backed tokens",
            Some("https://tether.to/wp-content/uploads/2016/06/TetherWhitePaper.pdf"),
            Some("db4451f11eda33950670aaf59e704da90117ff7057283b032cfaec7779313916".into()),
            8,
            Some(2),
            10000000000000000,
        );
        assert_eq!(expected_output, actual_output);
    }

    #[test]
    fn test_slp_address() {
        let bch = tbch_coin_for_test();
        let token_id = H256::from("bb309e48930671582bea508f9a1d9b491e49b69be3d6f372dc08da2ac6e90eb7");
        let fusd = SlpToken::new(4, "FUSD".into(), token_id, bch, 0);

        let slp_address = fusd.my_address().unwrap();
        assert_eq!("slptest:qzx0llpyp8gxxsmad25twksqnwd62xm3lsg8lecug8", slp_address);
    }

    #[test]
    fn test_validate_htlc_valid() {
        let bch = tbch_coin_for_test();
        let token_id = H256::from("bb309e48930671582bea508f9a1d9b491e49b69be3d6f372dc08da2ac6e90eb7");
        let fusd = SlpToken::new(4, "FUSD".into(), token_id, bch, 0);

        // https://testnet.simpleledger.info/tx/e935160bfb5b45007a0fc6f8fbe8da618f28df6573731f1ffb54d9560abb49b2
        let tx = hex::decode("0100000002736cf584f877ec7b6b95974bc461a9cfb9f126655b5d335471683154cc6cf4c5020000006a47304402206be99fe56a98e7a8c2ffe6f2d05c5c1f46a6577064b84d27d45fe0e959f6e77402201c512629313b48cd4df873222aa49046ae9a3a6e34e359d10d4308cb40438fba4121036879df230663db4cd083c8eeb0f293f46abc460ad3c299b0089b72e6d472202cffffffff736cf584f877ec7b6b95974bc461a9cfb9f126655b5d335471683154cc6cf4c5030000006a473044022020d774d045bbe3dce5b04af836f6a5629c6c4ce75b0b5ba8a1da0ae9a4ecc0530220522f86d20c9e4142e40f9a9c8d25db16fde91d4a0ad6f6ff2107e201386131b64121036879df230663db4cd083c8eeb0f293f46abc460ad3c299b0089b72e6d472202cffffffff040000000000000000406a04534c500001010453454e4420bb309e48930671582bea508f9a1d9b491e49b69be3d6f372dc08da2ac6e90eb70800000000000003e8080000000000001f3ee80300000000000017a914b0ca1fea17cf522c7e858416093fc6d95e55824087e8030000000000001976a9148cfffc2409d063437d6aa8b75a009b9ba51b71fc88accf614801000000001976a9148cfffc2409d063437d6aa8b75a009b9ba51b71fc88ac8c83d460").unwrap();

        let other_pub = hex::decode("036879df230663db4cd083c8eeb0f293f46abc460ad3c299b0089b72e6d472202c").unwrap();
        let other_pub = Public::from_slice(&other_pub).unwrap();

        let my_pub = hex::decode("03c6a78589e18b482aea046975e6d0acbdea7bf7dbf04d9d5bd67fda917815e3ed").unwrap();
        let my_pub = Public::from_slice(&my_pub).unwrap();

        let lock_time = 1624547837;
        let secret_hash = hex::decode("5d9e149ad9ccb20e9f931a69b605df2ffde60242").unwrap();
        let amount: BigDecimal = "0.1".parse().unwrap();
        let input = ValidateHtlcInput {
            tx,
            other_pub,
            my_pub,
            time_lock: lock_time,
            secret_hash,
            amount,
            confirmations: 1,
        };
        block_on(fusd.validate_htlc(input)).unwrap();
    }

    #[test]
    fn construct_and_send_invalid_slp_htlc_should_fail() {
        let bch = tbch_coin_for_test();
        let token_id = H256::from("bb309e48930671582bea508f9a1d9b491e49b69be3d6f372dc08da2ac6e90eb7");
        let fusd = SlpToken::new(4, "FUSD".into(), token_id, bch.clone(), 0);

        let bch_address = bch.as_ref().derivation_method.unwrap_iguana();
        let (unspents, recently_spent) = block_on(bch.get_unspent_ordered_list(bch_address)).unwrap();

        let secret_hash = hex::decode("5d9e149ad9ccb20e9f931a69b605df2ffde60242").unwrap();
        let other_pub = hex::decode("036879df230663db4cd083c8eeb0f293f46abc460ad3c299b0089b72e6d472202c").unwrap();
        let other_pub = Public::from_slice(&other_pub).unwrap();

        let my_public_key = bch.my_public_key().unwrap();
        let htlc_script = payment_script(1624547837, &secret_hash, &other_pub, my_public_key);

        let slp_send_op_return_out = slp_send_output(&token_id, &[1000]);

        let invalid_slp_send_out = TransactionOutput {
            value: 1000,
            script_pubkey: ScriptBuilder::build_p2sh(&dhash160(&htlc_script).into()).into(),
        };

        let tx_err = block_on(generate_and_send_tx(
            &fusd,
            unspents,
            None,
            FeePolicy::SendExact,
            recently_spent,
            vec![slp_send_op_return_out, invalid_slp_send_out],
        ))
        .unwrap_err();

        let err = match tx_err.clone() {
            TransactionErr::TxRecoverable(_tx, err) => err,
            TransactionErr::Plain(err) => err,
        };

        println!("{:?}", err);
        assert!(err.contains("is not valid with reason outputs greater than inputs"));

        // this is invalid tx bytes generated by one of this test runs, ensure that FUSD won't broadcast it using
        // different methods
        let tx_bytes: &[u8] = &[
            1, 0, 0, 0, 1, 105, 91, 221, 196, 250, 138, 113, 118, 165, 149, 181, 70, 15, 224, 124, 67, 133, 237, 31,
            88, 125, 178, 69, 166, 27, 211, 32, 54, 1, 238, 134, 102, 2, 0, 0, 0, 106, 71, 48, 68, 2, 32, 103, 105,
            238, 187, 198, 194, 7, 162, 250, 17, 240, 45, 93, 168, 223, 35, 92, 23, 84, 70, 193, 234, 183, 130, 114,
            49, 198, 118, 69, 22, 128, 118, 2, 32, 127, 44, 73, 98, 217, 254, 44, 181, 87, 175, 114, 138, 223, 173,
            201, 168, 38, 198, 49, 23, 9, 101, 50, 154, 55, 236, 126, 253, 37, 114, 111, 218, 65, 33, 3, 104, 121, 223,
            35, 6, 99, 219, 76, 208, 131, 200, 238, 176, 242, 147, 244, 106, 188, 70, 10, 211, 194, 153, 176, 8, 155,
            114, 230, 212, 114, 32, 44, 255, 255, 255, 255, 3, 0, 0, 0, 0, 0, 0, 0, 0, 55, 106, 4, 83, 76, 80, 0, 1, 1,
            4, 83, 69, 78, 68, 32, 187, 48, 158, 72, 147, 6, 113, 88, 43, 234, 80, 143, 154, 29, 155, 73, 30, 73, 182,
            155, 227, 214, 243, 114, 220, 8, 218, 42, 198, 233, 14, 183, 8, 0, 0, 0, 0, 0, 0, 3, 232, 232, 3, 0, 0, 0,
            0, 0, 0, 23, 169, 20, 149, 59, 57, 9, 255, 106, 162, 105, 248, 93, 163, 76, 19, 42, 146, 66, 68, 64, 225,
            142, 135, 205, 228, 173, 0, 0, 0, 0, 0, 25, 118, 169, 20, 140, 255, 252, 36, 9, 208, 99, 67, 125, 106, 168,
            183, 90, 0, 155, 155, 165, 27, 113, 252, 136, 172, 216, 36, 92, 97,
        ];

        let tx_bytes_str = hex::encode(tx_bytes);
        let err = fusd.send_raw_tx(&tx_bytes_str).wait().unwrap_err();
        println!("{:?}", err);
        assert!(err.contains("is not valid with reason outputs greater than inputs"));

        let err2 = fusd.send_raw_tx_bytes(tx_bytes).wait().unwrap_err();
        println!("{:?}", err2);
        assert!(err2.contains("is not valid with reason outputs greater than inputs"));
        assert_eq!(err, err2);

        let utxo_tx: UtxoTx = deserialize(tx_bytes).unwrap();
        let err = block_on(fusd.broadcast_tx(&utxo_tx)).unwrap_err();
        match err.into_inner() {
            BroadcastTxErr::Other(err) => assert!(err.contains("is not valid with reason outputs greater than inputs")),
            e @ _ => panic!("Unexpected err {:?}", e),
        };

        // The error variant should equal to `TxRecoverable`
        assert_eq!(
            discriminant(&tx_err),
            discriminant(&TransactionErr::TxRecoverable(
                TransactionEnum::from(utxo_tx),
                String::new()
            ))
        );
    }

    #[test]
    fn test_validate_htlc_invalid_slp_utxo() {
        let bch = tbch_coin_for_test();
        let token_id = H256::from("bb309e48930671582bea508f9a1d9b491e49b69be3d6f372dc08da2ac6e90eb7");
        let fusd = SlpToken::new(4, "FUSD".into(), token_id, bch.clone(), 0);

        // https://www.blockchain.com/ru/bch-testnet/tx/6686ee013620d31ba645b27d581fed85437ce00f46b595a576718afac4dd5b69
        let tx = hex::decode("0100000001ce59a734f33811afcc00c19dcb12202ed00067a50efed80424fabd2b723678c0020000006b483045022100ec1fecff9c60fb7e821b9a412bd8c4ce4a757c68287f9cf9e0f461165492d6530220222f020dd05d65ba35cddd0116c99255612ec90d63019bb1cea45e2cf09a62a94121036879df230663db4cd083c8eeb0f293f46abc460ad3c299b0089b72e6d472202cffffffff030000000000000000376a04534c500001010453454e4420bb309e48930671582bea508f9a1d9b491e49b69be3d6f372dc08da2ac6e90eb70800000000000003e8e80300000000000017a914953b3909ff6aa269f85da34c132a92424440e18e879decad00000000001976a9148cfffc2409d063437d6aa8b75a009b9ba51b71fc88acd1215c61").unwrap();

        let other_pub = hex::decode("036879df230663db4cd083c8eeb0f293f46abc460ad3c299b0089b72e6d472202c").unwrap();
        let other_pub = Public::from_slice(&other_pub).unwrap();

        let lock_time = 1624547837;
        let secret_hash = hex::decode("5d9e149ad9ccb20e9f931a69b605df2ffde60242").unwrap();
        let amount: BigDecimal = "0.1".parse().unwrap();
        let my_pub = bch.my_public_key().unwrap();

        // standard BCH validation should pass as the output itself is correct
        utxo_common::validate_payment(
            bch.clone(),
            deserialize(tx.as_slice()).unwrap(),
            SLP_SWAP_VOUT,
            my_pub,
            &other_pub,
            &secret_hash,
            fusd.platform_dust_dec(),
            lock_time,
            now_ms() / 1000 + 60,
            1,
        )
        .wait()
        .unwrap();

        let input = ValidateHtlcInput {
            tx,
            other_pub,
            my_pub: my_pub.clone(),
            time_lock: lock_time,
            secret_hash,
            amount,
            confirmations: 1,
        };
        let validity_err = block_on(fusd.validate_htlc(input)).unwrap_err();
        match validity_err.into_inner() {
            ValidateHtlcError::InvalidSlpUtxo(e) => println!("{:?}", e),
            err @ _ => panic!("Unexpected err {:?}", err),
        };
    }

    #[test]
    fn test_sign_message() {
        let bch = tbch_coin_for_test();
        let token_id = H256::from("bb309e48930671582bea508f9a1d9b491e49b69be3d6f372dc08da2ac6e90eb7");
        let fusd = SlpToken::new(4, "FUSD".into(), token_id, bch, 0);
        let signature = fusd.sign_message("test").unwrap();
        assert_eq!(
            signature,
            "ILuePKMsycXwJiNDOT7Zb7TfIlUW7Iq+5ylKd15AK72vGVYXbnf7Gj9Lk9MFV+6Ub955j7MiAkp0wQjvuIoRPPA="
        );
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn test_verify_message() {
        let bch = tbch_coin_for_test();
        let token_id = H256::from("bb309e48930671582bea508f9a1d9b491e49b69be3d6f372dc08da2ac6e90eb7");
        let fusd = SlpToken::new(4, "FUSD".into(), token_id, bch, 0);
        let is_valid = fusd
            .verify_message(
                "ILuePKMsycXwJiNDOT7Zb7TfIlUW7Iq+5ylKd15AK72vGVYXbnf7Gj9Lk9MFV+6Ub955j7MiAkp0wQjvuIoRPPA=",
                "test",
                "slptest:qzx0llpyp8gxxsmad25twksqnwd62xm3lsg8lecug8",
            )
            .unwrap();
        assert!(is_valid);
    }
}
