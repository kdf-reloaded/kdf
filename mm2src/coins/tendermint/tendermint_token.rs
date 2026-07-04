//! TendermintToken — an IBC/CW20-style token running on a Tendermint platform chain.
//!
//! A thin wrapper around `TendermintCoin` that parameterises operations
//! with its own denom and decimals, delegating all infrastructure to the
//! platform coin.

use super::tendermint_helpers::TendermintCommons;
use super::tendermint_types::*;
use crate::utxo::sat_from_big_decimal;
use crate::utxo::utxo_common::{big_decimal_from_sat, big_decimal_from_sat_unsigned};
use crate::{BalanceFut, CoinBalance, DexFee, FeeApproxStage, FoundSwapTxSpend, HistorySyncState, MarketCoinOps,
            MmCoin, NegotiateSwapContractAddrErr, RawTransactionError, RawTransactionFut, RawTransactionRequest,
            RawTransactionRes, SignRawTransactionRequest, SignatureError, SignatureResult, SwapOps, TradeFee,
            TradePreimageError, TradePreimageFut, TradePreimageResult, TradePreimageValue, TransactionDetails,
            TransactionEnum, TransactionErr, TransactionFut, TransactionType, TxFeeDetails,
            UnexpectedDerivationMethod, ValidateAddressResult, ValidateFeeArgs, ValidatePaymentInput,
            VerificationError, VerificationResult, WatcherOps, WithdrawError, WithdrawFut, WithdrawRequest};
use bigdecimal::BigDecimal;
use common::mm_number::MmNumber;
use common::now_ms;
use cosmrs::proto::cosmos::bank::v1beta1::MsgSend as MsgSendProto;
use cosmrs::proto::cosmos::base::v1beta1::Coin as CoinProto;
use cosmrs::proto::prost::Message;
use cosmrs::tx::{Fee, Raw};
use cosmrs::{AccountId, Any, Coin, Denom};
use derive_more::Display;
use futures::compat::Future01CompatExt;
use futures::{FutureExt, TryFutureExt};
use kdf_crypto::sha256;
use keys::KeyPair;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use rpc::v1::types::Bytes as BytesJson;
use serde_json::Value as Json;
use std::ops::Deref;
use std::str::FromStr;
use std::sync::Arc;

// ————————————————————————————————————————————————————————————————
// Token types
// ————————————————————————————————————————————————————————————————

/// Protocol definition used during activation.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct TendermintTokenProtocolInfo {
    pub platform: String,
    pub decimals: u8,
    pub denom: String,
}

/// Activation parameters (currently empty, reserved for future use).
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct TendermintTokenActivationParams {}

/// Errors specific to token initialisation.
#[derive(Debug, Display)]
pub enum TendermintTokenInitError {
    #[display(fmt = "Internal error: {}", _0)]
    InternalError(String),
}

pub struct TendermintTokenImpl {
    pub ticker: String,
    pub platform_coin: TendermintCoin,
    pub decimals: u8,
    pub denom: Denom,
}

#[derive(Clone)]
pub struct TendermintToken(pub Arc<TendermintTokenImpl>);

impl Deref for TendermintToken {
    type Target = TendermintTokenImpl;
    fn deref(&self) -> &Self::Target { &self.0 }
}

impl std::fmt::Debug for TendermintToken {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { write!(f, "TendermintToken({})", self.ticker) }
}

impl TendermintToken {
    pub fn new(ticker: String, platform_coin: TendermintCoin, decimals: u8, denom: Denom) -> Self {
        TendermintToken(Arc::new(TendermintTokenImpl {
            ticker,
            platform_coin,
            decimals,
            denom,
        }))
    }

    /// Unique identifier for the token, derived from the denom.
    pub fn token_id(&self) -> String {
        let hash = sha256(self.denom.to_string().to_lowercase().as_bytes());
        hex::encode(hash.as_slice())
    }

    /// Build a token from its protocol descriptor, parsing the on-chain
    /// denomination (a bank `u<base>` denom or an `ibc/<HASH>` IBC denom)
    /// from configuration. Used by the V2 token activation layer.
    pub fn from_protocol(
        ticker: String,
        platform_coin: TendermintCoin,
        decimals: u8,
        denom: &str,
    ) -> MmResult<Self, TendermintTokenInitError> {
        let denom = Denom::from_str(denom)
            .map_to_mm(|e| TendermintTokenInitError::InternalError(format!("Invalid denom '{denom}': {e}")))?;
        Ok(TendermintToken::new(ticker, platform_coin, decimals, denom))
    }
}

// ————————————————————————————————————————————————————————————————
// SwapOps
// ————————————————————————————————————————————————————————————————

#[async_trait::async_trait]
impl SwapOps for TendermintToken {
    fn send_taker_fee(&self, dex_fee: &DexFee, fee_addr: &[u8], uuid: &[u8]) -> TransactionFut {
        self.platform_coin
            .send_taker_fee_for_denom(dex_fee, self.denom.clone(), self.decimals, fee_addr, uuid)
    }

    fn send_maker_payment(
        &self,
        time_lock: u32,
        _maker_pub: &[u8],
        taker_pub: &[u8],
        secret_hash: &[u8],
        amount: BigDecimal,
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        let now_sec = (now_ms() / 1000) as u32;
        let duration = time_lock.saturating_sub(now_sec) as u64;
        self.platform_coin.send_htlc_for_denom(
            duration,
            taker_pub,
            secret_hash,
            amount,
            self.denom.clone(),
            self.decimals,
        )
    }

    fn send_taker_payment(
        &self,
        time_lock: u32,
        _taker_pub: &[u8],
        maker_pub: &[u8],
        secret_hash: &[u8],
        amount: BigDecimal,
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        let now_sec = (now_ms() / 1000) as u32;
        let duration = time_lock.saturating_sub(now_sec) as u64;
        self.platform_coin.send_htlc_for_denom(
            duration,
            maker_pub,
            secret_hash,
            amount,
            self.denom.clone(),
            self.decimals,
        )
    }

    fn send_maker_spends_taker_payment(
        &self,
        taker_payment_tx: &[u8],
        time_lock: u32,
        taker_pub: &[u8],
        secret: &[u8],
        htlc_privkey: &[u8],
        swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        self.platform_coin.send_maker_spends_taker_payment(
            taker_payment_tx,
            time_lock,
            taker_pub,
            secret,
            htlc_privkey,
            swap_contract_address,
        )
    }

    fn send_taker_spends_maker_payment(
        &self,
        maker_payment_tx: &[u8],
        time_lock: u32,
        maker_pub: &[u8],
        secret: &[u8],
        htlc_privkey: &[u8],
        swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        self.platform_coin.send_taker_spends_maker_payment(
            maker_payment_tx,
            time_lock,
            maker_pub,
            secret,
            htlc_privkey,
            swap_contract_address,
        )
    }

    fn send_taker_refunds_payment(
        &self,
        _taker_payment_tx: &[u8],
        _time_lock: u32,
        _maker_pub: &[u8],
        _secret_hash: &[u8],
        _htlc_privkey: &[u8],
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        Box::new(futures01::future::err(TransactionErr::Plain(
            "Tendermint HTLCs auto-refund on chain; no broadcast required.".into(),
        )))
    }

    fn send_maker_refunds_payment(
        &self,
        _maker_payment_tx: &[u8],
        _time_lock: u32,
        _taker_pub: &[u8],
        _secret_hash: &[u8],
        _htlc_privkey: &[u8],
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        Box::new(futures01::future::err(TransactionErr::Plain(
            "Tendermint HTLCs auto-refund on chain; no broadcast required.".into(),
        )))
    }

    fn validate_fee(&self, args: ValidateFeeArgs<'_>) -> Box<dyn futures01::Future<Item = (), Error = String> + Send> {
        self.platform_coin.validate_fee_for_denom(
            args.fee_tx,
            args.expected_sender,
            args.fee_addr,
            args.dex_fee,
            self.decimals,
            args.uuid,
            self.denom.to_string(),
        )
    }

    fn validate_maker_payment(
        &self,
        input: ValidatePaymentInput,
    ) -> Box<dyn futures01::Future<Item = (), Error = String> + Send> {
        let sender_pub = input.maker_pub.clone();
        self.platform_coin
            .validate_payment_for_denom(input, &sender_pub, self.denom.clone(), self.decimals)
    }

    fn validate_taker_payment(
        &self,
        input: ValidatePaymentInput,
    ) -> Box<dyn futures01::Future<Item = (), Error = String> + Send> {
        let sender_pub = input.taker_pub.clone();
        self.platform_coin
            .validate_payment_for_denom(input, &sender_pub, self.denom.clone(), self.decimals)
    }

    fn check_if_my_payment_sent(
        &self,
        _time_lock: u32,
        _my_pub: &[u8],
        other_pub: &[u8],
        secret_hash: &[u8],
        _search_from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
    ) -> Box<dyn futures01::Future<Item = Option<TransactionEnum>, Error = String> + Send> {
        self.platform_coin
            .check_if_my_payment_sent_for_denom(other_pub, secret_hash)
    }

    async fn search_for_swap_tx_spend_my(
        &self,
        time_lock: u32,
        other_pub: &[u8],
        secret_hash: &[u8],
        tx: &[u8],
        search_from_block: u64,
        swap_contract_address: &Option<BytesJson>,
    ) -> Result<Option<FoundSwapTxSpend>, String> {
        self.platform_coin
            .search_for_swap_tx_spend_my(
                time_lock,
                other_pub,
                secret_hash,
                tx,
                search_from_block,
                swap_contract_address,
            )
            .await
    }

    async fn search_for_swap_tx_spend_other(
        &self,
        time_lock: u32,
        other_pub: &[u8],
        secret_hash: &[u8],
        tx: &[u8],
        search_from_block: u64,
        swap_contract_address: &Option<BytesJson>,
    ) -> Result<Option<FoundSwapTxSpend>, String> {
        self.platform_coin
            .search_for_swap_tx_spend_other(
                time_lock,
                other_pub,
                secret_hash,
                tx,
                search_from_block,
                swap_contract_address,
            )
            .await
    }

    fn extract_secret(&self, secret_hash: &[u8], spend_tx: &[u8]) -> Result<Vec<u8>, String> {
        self.platform_coin.extract_secret(secret_hash, spend_tx)
    }

    fn negotiate_swap_contract_addr(
        &self,
        other_side_address: Option<&[u8]>,
    ) -> Result<Option<BytesJson>, MmError<NegotiateSwapContractAddrErr>> {
        self.platform_coin.negotiate_swap_contract_addr(other_side_address)
    }

    fn get_htlc_key_pair(&self) -> Option<KeyPair> { self.platform_coin.get_htlc_key_pair() }
}

// ————————————————————————————————————————————————————————————————
// WatcherOps (empty default impls)
// ————————————————————————————————————————————————————————————————

impl WatcherOps for TendermintToken {}

// ————————————————————————————————————————————————————————————————
// MarketCoinOps
// ————————————————————————————————————————————————————————————————

impl MarketCoinOps for TendermintToken {
    fn ticker(&self) -> &str { &self.ticker }

    fn my_address(&self) -> Result<String, String> { self.platform_coin.my_address() }

    fn get_public_key(&self) -> Result<String, MmError<UnexpectedDerivationMethod>> {
        self.platform_coin.get_public_key()
    }

    fn sign_message_hash(&self, message: &str) -> Option<[u8; 32]> { self.platform_coin.sign_message_hash(message) }

    fn sign_message(&self, message: &str) -> SignatureResult<String> { self.platform_coin.sign_message(message) }

    fn verify_message(&self, signature: &str, message: &str, address: &str) -> VerificationResult<bool> {
        self.platform_coin.verify_message(signature, message, address)
    }

    fn my_balance(&self) -> BalanceFut<CoinBalance> {
        let coin = self.clone();
        let fut = async move {
            let balance = coin
                .platform_coin
                .account_balance_for_denom(&coin.platform_coin.account_id, coin.denom.to_string())
                .await
                .map_mm_err()?;
            Ok(CoinBalance {
                spendable: big_decimal_from_sat_unsigned(balance, coin.decimals),
                unspendable: BigDecimal::default(),
            })
        };
        Box::new(fut.boxed().compat())
    }

    fn base_coin_balance(&self) -> BalanceFut<BigDecimal> { self.platform_coin.base_coin_balance() }

    fn platform_ticker(&self) -> &str { self.platform_coin.ticker() }

    fn send_raw_tx(&self, tx: &str) -> Box<dyn futures01::Future<Item = String, Error = String> + Send> {
        self.platform_coin.send_raw_tx(tx)
    }

    fn send_raw_tx_bytes(&self, tx: &[u8]) -> Box<dyn futures01::Future<Item = String, Error = String> + Send> {
        self.platform_coin.send_raw_tx_bytes(tx)
    }

    fn wait_for_confirmations(
        &self,
        tx: &[u8],
        confirmations: u64,
        requires_nota: bool,
        wait_until: u64,
        check_every: u64,
    ) -> Box<dyn futures01::Future<Item = (), Error = String> + Send> {
        self.platform_coin
            .wait_for_confirmations(tx, confirmations, requires_nota, wait_until, check_every)
    }

    fn wait_for_tx_spend(
        &self,
        transaction: &[u8],
        wait_until: u64,
        from_block: u64,
        swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        self.platform_coin
            .wait_for_tx_spend(transaction, wait_until, from_block, swap_contract_address)
    }

    fn tx_enum_from_bytes(&self, bytes: &[u8]) -> Result<TransactionEnum, String> {
        self.platform_coin.tx_enum_from_bytes(bytes)
    }

    fn current_block(&self) -> Box<dyn futures01::Future<Item = u64, Error = String> + Send> {
        self.platform_coin.current_block()
    }

    fn display_priv_key(&self) -> Result<String, String> { self.platform_coin.display_priv_key() }

    #[inline]
    fn min_tx_amount(&self) -> BigDecimal { big_decimal_from_sat(MIN_TX_SATOSHIS, self.decimals) }

    #[inline]
    fn min_trading_vol(&self) -> MmNumber { self.min_tx_amount().into() }

    fn sign_raw_tx(&self, _args: &SignRawTransactionRequest) -> crate::RawTransactionFut {
        let coin = self.ticker().to_string();
        Box::new(futures01::future::err(MmError::new(
            RawTransactionError::NotImplemented { coin },
        )))
    }
}

// ————————————————————————————————————————————————————————————————
// MmCoin
// ————————————————————————————————————————————————————————————————

#[async_trait::async_trait]
#[allow(unused_variables)]
impl MmCoin for TendermintToken {
    fn is_asset_chain(&self) -> bool { false }

    fn withdraw(&self, req: WithdrawRequest) -> WithdrawFut {
        // Token withdraw: constructs a MsgSend in the token's denom,
        // but pays fees in the platform coin's denom.
        let token = self.clone();
        let fut = async move {
            let to_address =
                AccountId::from_str(&req.to).map_to_mm(|e| WithdrawError::InvalidAddress(e.to_string()))?;

            let account_id = token.platform_coin.account_id.clone();
            let priv_key = token
                .platform_coin
                .activation_policy
                .activated_key_or_err()
                .map_err(|e| WithdrawError::InternalError(e.to_string()))?;

            let balance_denom = token
                .platform_coin
                .account_balance_for_denom(&account_id, token.denom.to_string())
                .await
                .map_mm_err()?;
            let balance_dec = big_decimal_from_sat_unsigned(balance_denom, token.decimals);

            let (amount_denom, amount_dec) = if req.max {
                (
                    balance_denom,
                    big_decimal_from_sat_unsigned(balance_denom, token.decimals),
                )
            } else {
                (
                    sat_from_big_decimal(&req.amount, token.decimals).map_mm_err()? as u64,
                    req.amount.clone(),
                )
            };

            if !token.platform_coin.is_tx_amount_enough(token.decimals, &amount_dec) {
                return MmError::err(WithdrawError::AmountTooLow {
                    amount: amount_dec,
                    threshold: token.min_tx_amount(),
                });
            }

            let received_by_me = if to_address == account_id {
                amount_dec.clone()
            } else {
                BigDecimal::default()
            };

            let msg = MsgSendProto {
                from_address: account_id.to_string(),
                to_address: to_address.to_string(),
                amount: vec![CoinProto {
                    denom: token.denom.to_string(),
                    amount: amount_denom.to_string(),
                }],
            };
            let msg_payload = Any {
                type_url: "/cosmos.bank.v1beta1.MsgSend".to_string(),
                value: msg.encode_to_vec(),
            };

            let memo = TX_DEFAULT_MEMO.to_string();

            let current_block = token
                .platform_coin
                .current_block()
                .compat()
                .await
                .map_to_mm(WithdrawError::Transport)?;
            let timeout_height = current_block + TIMEOUT_HEIGHT_DELTA;

            let (_, gas_limit) = token.platform_coin.gas_info_for_withdraw(&req.fee, GAS_LIMIT_DEFAULT);

            let fee_amount_u64 = token
                .platform_coin
                .calculate_account_fee_amount_as_u64(
                    &account_id,
                    Some(priv_key.clone()),
                    msg_payload.clone(),
                    timeout_height,
                    &memo,
                    req.fee,
                )
                .await
                .map_mm_err()?;

            let fee_amount_dec = big_decimal_from_sat_unsigned(fee_amount_u64, token.platform_coin.decimals());

            let fee_coin = Coin {
                denom: token.platform_coin.protocol_info.denom.clone(),
                amount: fee_amount_u64.into(),
            };
            let fee = Fee::from_amount_and_gas(fee_coin, gas_limit);

            // For non-max sends, verify total (amount + fee in platform denom).
            if !req.max {
                let total = &req.amount + &fee_amount_dec;
                // Note: fee is in platform denom, amount in token denom —
                // just check token balance for amount, platform balance for fee.
                if balance_dec < req.amount {
                    return MmError::err(WithdrawError::NotSufficientBalance {
                        coin: token.ticker.clone(),
                        available: balance_dec,
                        required: req.amount.clone(),
                    });
                }
            }

            let account_info = token.platform_coin.account_info(&account_id).await.map_mm_err()?;

            let tx_raw = token
                .platform_coin
                .any_to_signed_raw_tx(priv_key, &account_info, msg_payload, fee, timeout_height, &memo)
                .map_to_mm(|e| WithdrawError::InternalError(format!("Failed to sign tx: {}", e)))?;

            let tx_bytes = tx_raw
                .to_bytes()
                .map_to_mm(|e| WithdrawError::InternalError(format!("Failed to encode tx: {}", e)))?;
            let tx_hash = hex::encode_upper(sha256(&tx_bytes).as_slice());

            let tx_details = TransactionDetails {
                tx_hex: tx_bytes.into(),
                tx_hash: tx_hash.clone(),
                from: vec![account_id.to_string()],
                to: vec![req.to],
                my_balance_change: &received_by_me - &amount_dec,
                spent_by_me: amount_dec.clone(),
                total_amount: amount_dec,
                received_by_me,
                block_height: 0,
                timestamp: 0,
                fee_details: Some(TxFeeDetails::Tendermint(TendermintFeeDetails {
                    coin: token.platform_coin.ticker.clone(),
                    amount: fee_amount_dec,
                    uamount: fee_amount_u64,
                    gas_limit,
                })),
                coin: token.ticker.to_string(),
                internal_id: tx_hash.as_bytes().to_vec().into(),
                kmd_rewards: None,
                transaction_type: TransactionType::StandardTransfer,
            };
            token
                .platform_coin
                .publish_tx_history_record(token.ticker(), &tx_details);
            Ok(tx_details)
        };
        Box::new(fut.boxed().compat())
    }

    fn get_raw_transaction(&self, req: RawTransactionRequest) -> RawTransactionFut {
        self.platform_coin.get_raw_transaction(req)
    }

    fn decimals(&self) -> u8 { self.decimals }

    fn convert_to_address(&self, from: &str, to_address_format: Json) -> Result<String, String> {
        self.platform_coin.convert_to_address(from, to_address_format)
    }

    fn validate_address(&self, address: &str) -> ValidateAddressResult { self.platform_coin.validate_address(address) }

    fn process_history_loop(&self, _ctx: MmArc) -> Box<dyn futures01::Future<Item = (), Error = ()> + Send> {
        common::log::warn!("process_history_loop is deprecated for TendermintToken");
        Box::new(futures01::future::err(()))
    }

    fn history_sync_status(&self) -> HistorySyncState { self.platform_coin.history_sync_status() }

    fn get_trade_fee(&self) -> Box<dyn futures01::Future<Item = TradeFee, Error = String> + Send> {
        self.platform_coin.get_trade_fee()
    }

    async fn get_sender_trade_fee(
        &self,
        value: TradePreimageValue,
        _stage: FeeApproxStage,
    ) -> TradePreimageResult<TradeFee> {
        let amount = match value {
            TradePreimageValue::Exact(d) | TradePreimageValue::UpperBound(d) => d,
        };
        self.platform_coin
            .get_sender_trade_fee_for_denom(self.ticker.clone(), self.denom.clone(), self.decimals, amount)
            .await
    }

    fn get_receiver_trade_fee(&self, stage: FeeApproxStage) -> TradePreimageFut<TradeFee> {
        self.platform_coin.get_receiver_trade_fee(stage)
    }

    async fn get_fee_to_send_taker_fee(
        &self,
        dex_fee_amount: BigDecimal,
        _stage: FeeApproxStage,
    ) -> TradePreimageResult<TradeFee> {
        self.platform_coin
            .get_fee_to_send_taker_fee_for_denom(self.ticker.clone(), self.denom.clone(), self.decimals, dex_fee_amount)
            .await
    }

    fn required_confirmations(&self) -> u64 { self.platform_coin.required_confirmations() }

    fn requires_notarization(&self) -> bool { self.platform_coin.requires_notarization() }

    fn set_required_confirmations(&self, _confirmations: u64) {
        common::log::warn!("set_required_confirmations is not supported for TendermintToken");
    }

    fn set_requires_notarization(&self, requires_nota: bool) {
        self.platform_coin.set_requires_notarization(requires_nota)
    }

    fn swap_contract_address(&self) -> Option<BytesJson> { None }

    fn mature_confirmations(&self) -> Option<u32> { None }

    fn coin_protocol_info(&self) -> Vec<u8> { Vec::new() }

    fn is_coin_protocol_supported(&self, info: &Option<Vec<u8>>) -> bool { true }
}
