//! MarketCoinOps implementation for TendermintCoin.
//!
//! Provides market-facing operations: balance queries, transaction broadcast,
//! confirmation waiting, block height, and display helpers.

use super::htlc::{CreateHtlcMsg, CreateHtlcProto, HtlcType};
use super::rpc::*;
use super::tendermint_helpers::TendermintCommons;
use super::tendermint_types::*;
use crate::utxo::utxo_common::{big_decimal_from_sat, big_decimal_from_sat_unsigned};
use crate::{BalanceFut, CoinBalance, MarketCoinOps, RawTransactionError, RawTransactionFut, SignRawTransactionRequest,
            SignatureError, SignatureResult, TransactionEnum, TransactionErr, TransactionFut,
            UnexpectedDerivationMethod, VerificationError, VerificationResult};
use bigdecimal::BigDecimal;
use common::executor::Timer;
use common::mm_number::MmNumber;
use common::now_ms;
use cosmrs::proto::cosmos::tx::v1beta1::TxRaw;
use cosmrs::proto::prost::Message;
use cosmrs::tx::Raw;
use futures::compat::Future01CompatExt;
use futures::{FutureExt, TryFutureExt};
use kdf_crypto::sha256;
use mm2_err_handle::prelude::*;
use rpc::v1::types::Bytes as BytesJson;
use std::str::FromStr;

/// Whether a node's answer to a broadcast says it already holds the transaction (its cache has it).
fn broadcast_error_is_already_known(error: &str) -> bool { error.contains("tx already exists in cache") }

impl MarketCoinOps for TendermintCoin {
    fn ticker(&self) -> &str { &self.ticker }

    fn my_address(&self) -> Result<String, String> { Ok(self.account_id.to_string()) }

    fn get_public_key(&self) -> Result<String, MmError<UnexpectedDerivationMethod>> {
        let key = cosmrs::crypto::secp256k1::SigningKey::from_slice(
            self.activation_policy
                .activated_key_or_err()
                .map_err(|_| MmError::new(UnexpectedDerivationMethod::IguanaPrivKeyUnavailable))?
                .as_slice(),
        )
        .expect("privkey validity is checked on coin creation");
        Ok(key.public_key().to_string())
    }

    fn sign_message_hash(&self, _message: &str) -> Option<[u8; 32]> { None }

    fn sign_message(&self, _message: &str) -> SignatureResult<String> {
        // A watch-only coin names the refusal of CRD ch.36 R36.14.17 (`PrivKeyNotAllowed::WatchOnlyCoin`).
        if let Err(e) = self.activation_policy.activated_key_or_err() {
            if self.activation_policy.watch_only_ledger_flag().is_some() {
                return MmError::err(SignatureError::InternalError(e.get_inner().to_string()));
            }
        }
        MmError::err(SignatureError::InternalError("Not implemented".into()))
    }

    fn verify_message(&self, _signature: &str, _message: &str, _address: &str) -> VerificationResult<bool> {
        MmError::err(VerificationError::InternalError("Not implemented".into()))
    }

    fn my_balance(&self) -> BalanceFut<CoinBalance> {
        let coin = self.clone();
        let fut = async move {
            let balance_denom = coin
                .account_balance_for_denom(&coin.account_id, coin.protocol_info.denom.to_string())
                .await
                .map_mm_err()?;
            Ok(CoinBalance {
                spendable: big_decimal_from_sat_unsigned(balance_denom, coin.decimals()),
                unspendable: BigDecimal::default(),
            })
        };
        Box::new(fut.boxed().compat())
    }

    fn base_coin_balance(&self) -> BalanceFut<BigDecimal> {
        let coin = self.clone();
        let fut = async move {
            let balance = coin.my_balance().compat().await?;
            Ok(balance.spendable)
        };
        Box::new(fut.boxed().compat())
    }

    fn platform_ticker(&self) -> &str { &self.ticker }

    fn send_raw_tx(&self, tx: &str) -> Box<dyn futures01::Future<Item = String, Error = String> + Send> {
        let tx_bytes = try_fus!(hex::decode(tx));
        self.send_raw_tx_bytes(&tx_bytes)
    }

    fn send_raw_tx_bytes(&self, tx: &[u8]) -> Box<dyn futures01::Future<Item = String, Error = String> + Send> {
        try_fus!(Raw::from_bytes(tx));

        let coin = self.clone();
        let tx_bytes = tx.to_owned();
        let fut = async move {
            let client = try_s!(coin.rpc_client().await);
            let outcome = client.broadcast_tx_commit(tx_bytes.clone()).await;
            // The node already holds this exact transaction (broadcast before, possibly already included): the
            // broadcast succeeded earlier, so the answer is the transaction's hash, unless the chain shows it failed
            // (CRD ch.36 R36.14.15, as-built note of 2026-11-17). It serves every broadcast of this coin.
            if let Err(e) = &outcome {
                if broadcast_error_is_already_known(&e.to_string()) {
                    let hash = hex::encode_upper(sha256(&tx_bytes).as_slice());
                    if let Ok(Some(cosmrs::tendermint::abci::Code::Err(code))) =
                        coin.get_tx_status_code_or_none(hash.clone()).await
                    {
                        return ERR!("Tx deliver failed with code {}", code);
                    }
                    return Ok(hash);
                }
            }
            let broadcast_res = try_s!(outcome);

            if broadcast_res.check_tx.log.contains(ACCOUNT_SEQUENCE_ERR)
                || broadcast_res.tx_result.log.contains(ACCOUNT_SEQUENCE_ERR)
            {
                return ERR!(
                    "{}. check_tx log: {}, deliver_tx log: {}",
                    ACCOUNT_SEQUENCE_ERR,
                    broadcast_res.check_tx.log,
                    broadcast_res.tx_result.log
                );
            }

            if !broadcast_res.check_tx.code.is_ok() {
                return ERR!("Tx check failed {:?}", broadcast_res.check_tx);
            }

            if !broadcast_res.tx_result.code.is_ok() {
                return ERR!("Tx deliver failed {:?}", broadcast_res.tx_result);
            }

            Ok(broadcast_res.hash.to_string())
        };
        Box::new(fut.boxed().compat())
    }

    fn wait_for_confirmations(
        &self,
        tx: &[u8],
        _confirmations: u64,
        _requires_nota: bool,
        wait_until: u64,
        check_every: u64,
    ) -> Box<dyn futures01::Future<Item = (), Error = String> + Send> {
        // Sanity check: ensure tx is decodable.
        let _: TxRaw = try_fus!(Message::decode(tx));

        let tx_hash = hex::encode_upper(sha256(tx).as_slice());
        let coin = self.clone();
        let fut = async move {
            loop {
                let now_sec = now_ms() / 1000;
                if now_sec > wait_until {
                    return ERR!(
                        "Waited too long until {} for payment {} to be confirmed",
                        wait_until,
                        tx_hash
                    );
                }

                let tx_status_code = try_s!(coin.get_tx_status_code_or_none(tx_hash.clone()).await);

                if let Some(code) = tx_status_code {
                    return match code {
                        cosmrs::tendermint::abci::Code::Ok => Ok(()),
                        cosmrs::tendermint::abci::Code::Err(err_code) => Err(format!(
                            "Got error code: '{}' for tx: '{}'. Broadcast tx invalid.",
                            err_code, tx_hash
                        )),
                    };
                };

                Timer::sleep(check_every as f64).await;
            }
        };

        Box::new(fut.boxed().compat())
    }

    fn wait_for_tx_spend(
        &self,
        transaction: &[u8],
        wait_until: u64,
        _from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        let tx = try_tx_fus!(cosmrs::Tx::from_bytes(transaction));
        let first_msg = try_tx_fus!(tx.body.messages.first().ok_or("Tx body couldn't be read."));

        let htlc_type = try_tx_fus!(HtlcType::from_str(&self.protocol_info.account_prefix));
        let htlc_proto = try_tx_fus!(CreateHtlcProto::decode(htlc_type, first_msg.value.as_slice()));
        let htlc = try_tx_fus!(CreateHtlcMsg::try_from(htlc_proto));

        // We need the secret_hash but it's not in CreateHtlcMsg.
        // Use the HTLC's hash_lock from proto instead. Re-decode to get it.
        let htlc_proto2 = try_tx_fus!(CreateHtlcProto::decode(htlc_type, first_msg.value.as_slice()));
        let hash_lock_hex = htlc_proto2.hash_lock().to_string();
        let secret_hash = try_tx_fus!(hex::decode(&hash_lock_hex));

        let htlc_id = self.calculate_htlc_id(htlc.sender(), htlc.to(), htlc.amount(), &secret_hash);

        // CRD ch.18 R18.11.19 / ch.51 R91: each round takes only a successful claim of this HTLC; a failed or
        // unrelated result is skipped and the polling goes on until the deadline.
        let coin = self.clone();
        let fut = async move {
            loop {
                if let Some(tx) = try_tx_s!(coin.find_claim_of_htlc(&htlc_id).await) {
                    return Ok(TransactionEnum::CosmosTransaction(CosmosTransaction {
                        data: TxRaw {
                            body_bytes: try_tx_s!(tx.body.into_bytes()),
                            auth_info_bytes: try_tx_s!(tx.auth_info.into_bytes()),
                            signatures: tx.signatures,
                        },
                    }));
                }

                let now_sec = now_ms() / 1000;
                if now_sec > wait_until {
                    return Err(TransactionErr::Plain("Waited too long for HTLC claim".into()));
                }

                Timer::sleep(5.).await;
            }
        };

        Box::new(fut.boxed().compat())
    }

    fn tx_enum_from_bytes(&self, bytes: &[u8]) -> Result<TransactionEnum, String> {
        let tx_raw: TxRaw = Message::decode(bytes).map_err(|e| format!("Failed to decode TxRaw: {}", e))?;
        Ok(TransactionEnum::CosmosTransaction(CosmosTransaction { data: tx_raw }))
    }

    fn current_block(&self) -> Box<dyn futures01::Future<Item = u64, Error = String> + Send> {
        let coin = self.clone();
        let fut = async move {
            let info = try_s!(try_s!(coin.rpc_client().await).abci_info().await);
            Ok(info.response.last_block_height.into())
        };
        Box::new(fut.boxed().compat())
    }

    fn display_priv_key(&self) -> Result<String, String> {
        Ok(self
            .activation_policy
            .activated_key_or_err()
            .map_err(|e| e.to_string())?
            .to_string())
    }

    #[inline]
    fn min_tx_amount(&self) -> BigDecimal { big_decimal_from_sat(MIN_TX_SATOSHIS, self.protocol_info.decimals) }

    #[inline]
    fn min_trading_vol(&self) -> MmNumber { self.min_tx_amount().into() }

    fn sign_raw_tx(&self, _args: &SignRawTransactionRequest) -> RawTransactionFut {
        let coin = self.ticker().to_string();
        Box::new(futures01::future::err(MmError::new(
            RawTransactionError::NotImplemented { coin },
        )))
    }
}
