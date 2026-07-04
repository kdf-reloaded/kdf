//! MmCoin implementation for TendermintCoin.
//!
//! Provides framework-level operations: withdraw, trade-fee estimation,
//! address validation, history sync, and protocol negotiation.

use super::rpc::*;
use super::tendermint_helpers::TendermintCommons;
use super::tendermint_types::*;
use crate::utxo::sat_from_big_decimal;
use crate::utxo::utxo_common::{big_decimal_from_sat, big_decimal_from_sat_unsigned};
use crate::{BalanceError, FeeApproxStage, HistorySyncState, MarketCoinOps, MmCoin, RawTransactionError,
            RawTransactionFut, RawTransactionRequest, RawTransactionRes, TradeFee, TradePreimageError,
            TradePreimageFut, TradePreimageResult, TradePreimageValue, TransactionDetails, TransactionType,
            TxFeeDetails, ValidateAddressResult, WithdrawError, WithdrawFee, WithdrawFut, WithdrawRequest};
use bigdecimal::BigDecimal;
use common::mm_number::MmNumber;
use common::now_ms;
use cosmrs::proto::cosmos::bank::v1beta1::MsgSend as MsgSendProto;
use cosmrs::proto::cosmos::base::v1beta1::Coin as CoinProto;
use cosmrs::proto::prost::Message;
use cosmrs::tx::{Fee, Raw};
use cosmrs::{AccountId, Any, Coin};
use futures::compat::Future01CompatExt;
use futures::{FutureExt, TryFutureExt};
use kdf_crypto::sha256;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use mm2_net_config::net_config_or_panic;
use rpc::v1::types::Bytes as BytesJson;
use serde_json::Value as Json;
use std::str::FromStr;

const MSG_SEND_TYPE_URL: &str = "/cosmos.bank.v1beta1.MsgSend";

// ————————————————————————————————————————————————————————————————
// Trade-fee helpers (pub(super) for token reuse)
// ————————————————————————————————————————————————————————————————

impl TendermintCoin {
    /// Resolves the active netid for fee-preimage destination lookups.
    ///
    /// Tendermint coins hold a weak `MmCtx` reference; if the context has
    /// been dropped (only possible during shutdown), fall back to the
    /// default community netid. The resulting address is only used to
    /// build a representative HTLC / `MsgSend` for fee estimation, never
    /// for an actual fund transfer — the real destination is resolved
    /// through `DexFee` / `DexFeeBurnDestination` on the swap path.
    fn netid_for_preimage(&self) -> u16 {
        match MmArc::from_weak(&self.ctx) {
            Some(ctx) => ctx.netid(),
            None => mm2_net_config::SUPPORTED_NETIDS[0],
        }
    }

    /// Estimate the sender (maker/taker) trade fee for a given denom.
    pub(super) async fn get_sender_trade_fee_for_denom(
        &self,
        ticker: String,
        denom: cosmrs::Denom,
        decimals: u8,
        amount: BigDecimal,
    ) -> TradePreimageResult<TradeFee> {
        const TIME_LOCK: u64 = 1750;

        let mut sec = [0u8; 32];
        common::os_rng(&mut sec).map_err(|e| MmError::new(TradePreimageError::InternalError(e.to_string())))?;
        let secret_hash = sha256(&sec);

        let net_cfg = net_config_or_panic(self.netid_for_preimage());
        let to_address = account_id_from_pubkey_hex(&self.protocol_info.account_prefix, net_cfg.dex_fee_addr_pubkey())
            .map_err(|e| MmError::new(TradePreimageError::InternalError(e.to_string())))?;

        let amount_sat = sat_from_big_decimal(&amount, decimals).map_mm_err()?;

        let create_htlc_tx = self
            .gen_create_htlc_tx(
                denom,
                &to_address,
                (amount_sat as u64).into(),
                secret_hash.as_slice(),
                TIME_LOCK,
            )
            .map_err(|e| {
                MmError::new(TradePreimageError::InternalError(format!(
                    "Could not create HTLC: {:?}",
                    e.into_inner()
                )))
            })?;

        let current_block = self
            .current_block()
            .compat()
            .await
            .map_err(|e| MmError::new(TradePreimageError::Transport(e)))?;
        let timeout_height = current_block + TIMEOUT_HEIGHT_DELTA;

        let fee = self
            .calculate_fee(create_htlc_tx.msg_payload, timeout_height, TX_DEFAULT_MEMO, None)
            .await
            .map_err(|e| MmError::new(TradePreimageError::Transport(e.to_string())))?;

        let fee_amount_u64 = fee.amount.first().map(|c| c.amount as u64).unwrap_or(0);
        let fee_amount_dec = big_decimal_from_sat_unsigned(fee_amount_u64, decimals);

        Ok(TradeFee {
            coin: ticker,
            amount: fee_amount_dec.into(),
            paid_from_trading_vol: false,
        })
    }

    /// Estimate the fee for sending a taker DEX fee transaction.
    pub(super) async fn get_fee_to_send_taker_fee_for_denom(
        &self,
        ticker: String,
        denom: cosmrs::Denom,
        decimals: u8,
        dex_fee_amount: BigDecimal,
    ) -> TradePreimageResult<TradeFee> {
        let amount_sat = sat_from_big_decimal(&dex_fee_amount, decimals).map_mm_err()?;

        let net_cfg = net_config_or_panic(self.netid_for_preimage());
        let to_address = account_id_from_pubkey_hex(&self.protocol_info.account_prefix, net_cfg.dex_fee_addr_pubkey())
            .map_err(|e| MmError::new(TradePreimageError::InternalError(e.to_string())))?;

        let msg = MsgSendProto {
            from_address: self.account_id.to_string(),
            to_address: to_address.to_string(),
            amount: vec![CoinProto {
                denom: denom.to_string(),
                amount: amount_sat.to_string(),
            }],
        };
        let tx_payload = Any {
            type_url: MSG_SEND_TYPE_URL.to_string(),
            value: msg.encode_to_vec(),
        };

        let current_block = self
            .current_block()
            .compat()
            .await
            .map_err(|e| MmError::new(TradePreimageError::Transport(e)))?;
        let timeout_height = current_block + TIMEOUT_HEIGHT_DELTA;

        let fee = self
            .calculate_fee(tx_payload, timeout_height, TX_DEFAULT_MEMO, None)
            .await
            .map_err(|e| MmError::new(TradePreimageError::Transport(e.to_string())))?;

        let fee_amount_u64 = fee.amount.first().map(|c| c.amount as u64).unwrap_or(0);
        let fee_amount_dec = big_decimal_from_sat_unsigned(fee_amount_u64, decimals);

        Ok(TradeFee {
            coin: ticker,
            amount: fee_amount_dec.into(),
            paid_from_trading_vol: false,
        })
    }
}

// ————————————————————————————————————————————————————————————————
// MmCoin trait implementation
// ————————————————————————————————————————————————————————————————

#[async_trait::async_trait]
#[allow(unused_variables)]
impl MmCoin for TendermintCoin {
    fn is_asset_chain(&self) -> bool { false }

    fn withdraw(&self, req: WithdrawRequest) -> WithdrawFut {
        let coin = self.clone();
        let fut = async move {
            let to_address =
                AccountId::from_str(&req.to).map_to_mm(|e| WithdrawError::InvalidAddress(e.to_string()))?;

            let account_id = coin.account_id.clone();
            let priv_key = coin
                .activation_policy
                .activated_key_or_err()
                .map_err(|e| WithdrawError::InternalError(e.to_string()))?;

            let balance_denom = coin
                .account_balance_for_denom(&account_id, coin.protocol_info.denom.to_string())
                .await
                .map_mm_err()?;
            let balance_dec = big_decimal_from_sat_unsigned(balance_denom, coin.decimals());

            let (amount_denom, amount_dec) = if req.max {
                (
                    balance_denom,
                    big_decimal_from_sat_unsigned(balance_denom, coin.decimals()),
                )
            } else {
                (
                    sat_from_big_decimal(&req.amount, coin.decimals()).map_mm_err()? as u64,
                    req.amount.clone(),
                )
            };

            if !coin.is_tx_amount_enough(coin.decimals(), &amount_dec) {
                return MmError::err(WithdrawError::AmountTooLow {
                    amount: amount_dec,
                    threshold: coin.min_tx_amount(),
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
                    denom: coin.protocol_info.denom.to_string(),
                    amount: amount_denom.to_string(),
                }],
            };
            let msg_payload = Any {
                type_url: MSG_SEND_TYPE_URL.to_string(),
                value: msg.encode_to_vec(),
            };

            let memo = TX_DEFAULT_MEMO.to_string();

            let current_block = coin
                .current_block()
                .compat()
                .await
                .map_to_mm(WithdrawError::Transport)?;
            let timeout_height = current_block + TIMEOUT_HEIGHT_DELTA;

            let (_, gas_limit) = coin.gas_info_for_withdraw(&req.fee, GAS_LIMIT_DEFAULT);

            let fee_amount_u64 = coin
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

            let fee_amount_dec = big_decimal_from_sat_unsigned(fee_amount_u64, coin.decimals());

            let fee_coin = Coin {
                denom: coin.protocol_info.denom.clone(),
                amount: fee_amount_u64.into(),
            };
            let fee = Fee::from_amount_and_gas(fee_coin, gas_limit);

            let (amount_denom, total_amount) = if req.max {
                if balance_denom < fee_amount_u64 {
                    return MmError::err(WithdrawError::NotSufficientBalance {
                        coin: coin.ticker.clone(),
                        available: balance_dec,
                        required: fee_amount_dec,
                    });
                }
                let amount_denom = balance_denom - fee_amount_u64;
                (amount_denom, balance_dec)
            } else {
                let total = &req.amount + &fee_amount_dec;
                if balance_dec < total {
                    return MmError::err(WithdrawError::NotSufficientBalance {
                        coin: coin.ticker.clone(),
                        available: balance_dec,
                        required: total,
                    });
                }
                (
                    sat_from_big_decimal(&req.amount, coin.decimals()).map_mm_err()? as u64,
                    total,
                )
            };

            // Rebuild message with adjusted amount (for max send).
            let msg = MsgSendProto {
                from_address: account_id.to_string(),
                to_address: to_address.to_string(),
                amount: vec![CoinProto {
                    denom: coin.protocol_info.denom.to_string(),
                    amount: amount_denom.to_string(),
                }],
            };
            let msg_payload = Any {
                type_url: MSG_SEND_TYPE_URL.to_string(),
                value: msg.encode_to_vec(),
            };

            let account_info = coin.account_info(&account_id).await.map_mm_err()?;

            let tx_raw = coin
                .any_to_signed_raw_tx(priv_key, &account_info, msg_payload, fee, timeout_height, &memo)
                .map_to_mm(|e| WithdrawError::InternalError(format!("Failed to sign tx: {}", e)))?;

            let tx_bytes = tx_raw
                .to_bytes()
                .map_to_mm(|e| WithdrawError::InternalError(format!("Failed to encode tx: {}", e)))?;
            let tx_hash = hex::encode_upper(sha256(&tx_bytes).as_slice());
            let internal_id = tx_hash_to_internal_id(&tx_hash);

            let tx_details = TransactionDetails {
                tx_hex: tx_bytes.into(),
                tx_hash,
                from: vec![account_id.to_string()],
                to: vec![req.to],
                my_balance_change: &received_by_me - &total_amount,
                spent_by_me: total_amount.clone(),
                total_amount,
                received_by_me,
                block_height: 0,
                timestamp: 0,
                fee_details: Some(TxFeeDetails::Tendermint(TendermintFeeDetails {
                    coin: coin.ticker.clone(),
                    amount: fee_amount_dec,
                    uamount: fee_amount_u64,
                    gas_limit,
                })),
                coin: coin.ticker.to_string(),
                internal_id,
                kmd_rewards: None,
                transaction_type: TransactionType::StandardTransfer,
            };
            coin.publish_tx_history_record(coin.ticker(), &tx_details);
            Ok(tx_details)
        };
        Box::new(fut.boxed().compat())
    }

    fn get_raw_transaction(&self, mut req: RawTransactionRequest) -> RawTransactionFut {
        let coin = self.clone();
        let fut = async move {
            req.tx_hash.make_ascii_uppercase();
            let tx_from_rpc = coin.request_tx(req.tx_hash).await.map_mm_err()?;
            Ok(RawTransactionRes {
                tx_hex: tx_from_rpc.encode_to_vec().into(),
            })
        };
        Box::new(fut.boxed().compat())
    }

    fn decimals(&self) -> u8 { self.protocol_info.decimals }

    fn convert_to_address(&self, _from: &str, _to_address_format: Json) -> Result<String, String> {
        Err("Not implemented".into())
    }

    fn validate_address(&self, address: &str) -> ValidateAddressResult {
        match AccountId::from_str(address) {
            Ok(_) => ValidateAddressResult {
                is_valid: true,
                reason: None,
            },
            Err(e) => ValidateAddressResult {
                is_valid: false,
                reason: Some(e.to_string()),
            },
        }
    }

    fn process_history_loop(&self, _ctx: MmArc) -> Box<dyn futures01::Future<Item = (), Error = ()> + Send> {
        common::log::warn!("process_history_loop is deprecated for Tendermint");
        Box::new(futures01::future::err(()))
    }

    fn history_sync_status(&self) -> HistorySyncState { self.history_sync_state.lock().unwrap().clone() }

    fn get_trade_fee(&self) -> Box<dyn futures01::Future<Item = TradeFee, Error = String> + Send> {
        let coin = self.clone();
        let fut = async move {
            let fee = try_s!(
                coin.get_sender_trade_fee_for_denom(
                    coin.ticker.to_owned(),
                    coin.protocol_info.denom.clone(),
                    coin.protocol_info.decimals,
                    coin.min_tx_amount(),
                )
                .await
            );
            Ok(TradeFee {
                coin: coin.ticker.to_owned(),
                amount: fee.amount,
                paid_from_trading_vol: false,
            })
        };
        Box::new(fut.boxed().compat())
    }

    async fn get_sender_trade_fee(
        &self,
        value: TradePreimageValue,
        _stage: FeeApproxStage,
    ) -> TradePreimageResult<TradeFee> {
        let amount = match value {
            TradePreimageValue::Exact(decimal) | TradePreimageValue::UpperBound(decimal) => decimal,
        };
        self.get_sender_trade_fee_for_denom(
            self.ticker.clone(),
            self.protocol_info.denom.clone(),
            self.protocol_info.decimals,
            amount,
        )
        .await
    }

    fn get_receiver_trade_fee(&self, _stage: FeeApproxStage) -> TradePreimageFut<TradeFee> {
        let coin = self.clone();
        let fut = async move {
            coin.get_sender_trade_fee_for_denom(
                coin.ticker.clone(),
                coin.protocol_info.denom.clone(),
                coin.decimals(),
                coin.min_tx_amount(),
            )
            .await
        };
        Box::new(fut.boxed().compat())
    }

    async fn get_fee_to_send_taker_fee(
        &self,
        dex_fee_amount: BigDecimal,
        _stage: FeeApproxStage,
    ) -> TradePreimageResult<TradeFee> {
        self.get_fee_to_send_taker_fee_for_denom(
            self.ticker.clone(),
            self.protocol_info.denom.clone(),
            self.protocol_info.decimals,
            dex_fee_amount,
        )
        .await
    }

    fn required_confirmations(&self) -> u64 { 0 }

    fn requires_notarization(&self) -> bool { false }

    fn set_required_confirmations(&self, _confirmations: u64) {
        common::log::warn!("set_required_confirmations is not supported for Tendermint");
    }

    fn set_requires_notarization(&self, _requires_nota: bool) {
        common::log::warn!("Tendermint doesn't support notarization");
    }

    fn swap_contract_address(&self) -> Option<BytesJson> { None }

    fn mature_confirmations(&self) -> Option<u32> { None }

    fn coin_protocol_info(&self) -> Vec<u8> { Vec::new() }

    fn is_coin_protocol_supported(&self, _info: &Option<Vec<u8>>) -> bool { true }
}

// ————————————————————————————————————————————————————————————————
// Helpers
// ————————————————————————————————————————————————————————————————

/// Generate an internal transaction id from a tx hash string.
fn tx_hash_to_internal_id(tx_hash: &str) -> BytesJson { tx_hash.as_bytes().to_vec().into() }
