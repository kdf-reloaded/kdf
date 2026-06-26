use super::ethermint_account::EthermintAccount;
use super::htlc::{ClaimHtlcMsg, CreateHtlcMsg, HtlcType, QueryHtlcRequestProto, QueryHtlcResponse, TendermintHtlc};
use super::rpc::*;
use super::tendermint_types::*;
use super::IRIS_PREFIX;
use crate::utxo::sat_from_big_decimal;
use crate::utxo::utxo_common::{big_decimal_from_sat, big_decimal_from_sat_unsigned};
use crate::{CoinBalance, HistorySyncState, MarketCoinOps, TransactionEnum, TransactionErr, WithdrawFee};
use async_trait::async_trait;
use bigdecimal::BigDecimal;
use common::executor::Timer;
use common::log::debug;
use common::now_ms;
use cosmrs::crypto::secp256k1::SigningKey;
use cosmrs::proto::cosmos::auth::v1beta1::{BaseAccount, QueryAccountRequest, QueryAccountResponse};
use cosmrs::proto::cosmos::bank::v1beta1::{QueryBalanceRequest, QueryBalanceResponse};
use cosmrs::proto::cosmos::base::tendermint::v1beta1::{GetLatestBlockRequest, GetLatestBlockResponse};
use cosmrs::proto::cosmos::tx::v1beta1::{GetTxRequest, GetTxResponse, SimulateRequest, SimulateResponse, Tx, TxRaw};
use cosmrs::proto::prost::Message;
use cosmrs::tx::{self, Fee, Raw, SignDoc, SignerInfo};
use cosmrs::{AccountId, Any, Coin, Denom};
use crypto::Secp256k1Secret;
use futures::compat::Future01CompatExt;
use futures::FutureExt;
use kdf_crypto::sha256;
use mm2_err_handle::prelude::*;
use std::collections::{HashMap, HashSet};
use std::num::NonZeroU32;
use std::str::FromStr;
use std::time::Duration;

// ————————————————————————————————————————————————————————————————
// TendermintCommons trait
// ————————————————————————————————————————————————————————————————

#[async_trait]
pub trait TendermintCommons {
    fn platform_denom(&self) -> &Denom;
    fn set_history_sync_state(&self, new_state: HistorySyncState);
    async fn rpc_client(&self) -> MmResult<HttpClient, TendermintCoinRpcError>;
}

#[async_trait]
impl TendermintCommons for TendermintCoin {
    fn platform_denom(&self) -> &Denom { &self.protocol_info.denom }

    fn set_history_sync_state(&self, new_state: HistorySyncState) {
        *self.history_sync_state.lock().unwrap() = new_state;
    }

    async fn rpc_client(&self) -> MmResult<HttpClient, TendermintCoinRpcError> {
        let mut client_impl = self.client.0.lock().await;
        for (i, client) in client_impl.rpc_clients.clone().into_iter().enumerate() {
            match client.perform(HealthRequest).await {
                Ok(_) => {
                    client_impl.rpc_clients.rotate_left(i);
                    return Ok(client);
                },
                Err(rpc_error) => {
                    debug!("Healthcheck failed on RPC node {}: {}", i, rpc_error);
                },
            }
        }
        MmError::err(TendermintCoinRpcError::RpcClientError(
            "All RPC nodes are unavailable.".to_string(),
        ))
    }
}

// ————————————————————————————————————————————————————————————————
// Inherent helper methods
// ————————————————————————————————————————————————————————————————

impl TendermintCoin {
    pub fn decimals(&self) -> u8 { self.protocol_info.decimals }

    pub fn supports_htlc(&self) -> bool {
        matches!(
            self.protocol_info.account_prefix.as_str(),
            super::NUCLEUS_PREFIX | super::IRIS_PREFIX
        )
    }

    #[inline(always)]
    pub(super) fn gas_price(&self) -> f64 { self.protocol_info.gas_price.unwrap_or(DEFAULT_GAS_PRICE) }

    pub(super) fn estimate_blocks_from_duration(&self, duration: u64) -> i64 {
        let estimated = (duration / self.avg_blocktime as u64) as i64;
        estimated.clamp(MIN_TIME_LOCK, MAX_TIME_LOCK)
    }

    pub(super) fn gas_info_for_withdraw(
        &self,
        withdraw_fee: &Option<WithdrawFee>,
        fallback_gas_limit: u64,
    ) -> (f64, u64) {
        // Fork's WithdrawFee has no CosmosGas variant yet, so always use defaults.
        let _ = withdraw_fee;
        (self.gas_price(), fallback_gas_limit)
    }

    pub(crate) fn is_tx_amount_enough(&self, decimals: u8, amount: &BigDecimal) -> bool {
        let min_tx_amount = big_decimal_from_sat(MIN_TX_SATOSHIS, decimals);
        amount >= &min_tx_amount
    }

    // ————————————————————————————————————————————————————————————
    // Account / balance queries
    // ————————————————————————————————————————————————————————————

    pub(super) async fn account_info(&self, account_id: &AccountId) -> MmResult<BaseAccount, TendermintCoinRpcError> {
        let request = QueryAccountRequest {
            address: account_id.to_string(),
        };
        let request = AbciRequest::new(
            Some(ABCI_QUERY_ACCOUNT_PATH.to_string()),
            request.encode_to_vec(),
            ABCI_REQUEST_HEIGHT,
            ABCI_REQUEST_PROVE,
        );

        let response = self.rpc_client().await?.perform(request).await?;
        let account_response = QueryAccountResponse::decode(response.response.value.as_slice())?;
        let account = account_response
            .account
            .or_mm_err(|| TendermintCoinRpcError::InvalidResponse("Account is None".into()))?;

        let account_prefix = self.protocol_info.account_prefix.clone();
        let base_account = match BaseAccount::decode(account.value.as_slice()) {
            Ok(account) => account,
            Err(err) if account_prefix.as_str() == IRIS_PREFIX => {
                let ethermint_account = EthermintAccount::decode(account.value.as_slice())?;
                ethermint_account
                    .base_account
                    .or_mm_err(|| TendermintCoinRpcError::Prost(err.to_string()))?
            },
            Err(err) => {
                return MmError::err(TendermintCoinRpcError::Prost(err.to_string()));
            },
        };

        Ok(base_account)
    }

    pub(super) async fn account_balance_for_denom(
        &self,
        account_id: &AccountId,
        denom: String,
    ) -> MmResult<u64, TendermintCoinRpcError> {
        let request = QueryBalanceRequest {
            address: account_id.to_string(),
            denom,
        };
        let request = AbciRequest::new(
            Some(ABCI_QUERY_BALANCE_PATH.to_string()),
            request.encode_to_vec(),
            ABCI_REQUEST_HEIGHT,
            ABCI_REQUEST_PROVE,
        );

        let response = self.rpc_client().await?.perform(request).await?;
        let response = QueryBalanceResponse::decode(response.response.value.as_slice())?;
        response
            .balance
            .or_mm_err(|| TendermintCoinRpcError::InvalidResponse("balance is None".into()))?
            .amount
            .parse()
            .map_to_mm(|e| TendermintCoinRpcError::InvalidResponse(format!("balance is not u64, err {e}")))
    }

    // ————————————————————————————————————————————————————————————
    // Activation helpers (token registration & balance enumeration)
    // ————————————————————————————————————————————————————————————

    /// Register a freshly activated token so the platform coin's activation
    /// result and token-balance enumeration include it.
    pub fn add_activated_token_info(&self, ticker: String, decimals: u8, denom: Denom) {
        self.tokens_info.lock().insert(ticker.clone(), ActivatedTokenInfo {
            decimals,
            ticker,
            denom,
        });
    }

    /// Whether activation should report balances in its result (the
    /// `get_balances` activation flag captured at coin creation).
    pub fn activation_get_balances(&self) -> bool { self.get_balances }

    /// The set of currently activated token tickers.
    pub fn activated_token_tickers(&self) -> HashSet<String> { self.tokens_info.lock().keys().cloned().collect() }

    /// Query the balance of every activated token, keyed by token ticker.
    pub async fn get_activated_tokens_balances(
        &self,
    ) -> MmResult<HashMap<String, CoinBalance>, TendermintCoinRpcError> {
        let tokens: Vec<(String, u8, Denom)> = self
            .tokens_info
            .lock()
            .values()
            .map(|info| (info.ticker.clone(), info.decimals, info.denom.clone()))
            .collect();

        let mut balances = HashMap::new();
        for (ticker, decimals, denom) in tokens {
            let amount = self
                .account_balance_for_denom(&self.account_id, denom.to_string())
                .await?;
            balances.insert(ticker, CoinBalance {
                spendable: big_decimal_from_sat_unsigned(amount, decimals),
                unspendable: BigDecimal::default(),
                ..Default::default()
            });
        }
        Ok(balances)
    }

    // ————————————————————————————————————————————————————————————
    // Transaction queries
    // ————————————————————————————————————————————————————————————

    pub(super) async fn request_tx(&self, hash: String) -> MmResult<Tx, TendermintCoinRpcError> {
        let request = GetTxRequest { hash: hash.clone() };
        let response = self
            .rpc_client()
            .await?
            .abci_query(
                Some(ABCI_GET_TX_PATH.to_string()),
                request.encode_to_vec(),
                ABCI_REQUEST_HEIGHT,
                ABCI_REQUEST_PROVE,
            )
            .await?;

        let response = GetTxResponse::decode(response.value.as_slice())?;
        response
            .tx
            .or_mm_err(|| TendermintCoinRpcError::InvalidResponse(format!("Tx {} does not exist", hash)))
    }

    pub(super) async fn get_tx_status_code_or_none(
        &self,
        hash: String,
    ) -> MmResult<Option<cosmrs::tendermint::abci::Code>, TendermintCoinRpcError> {
        let request = GetTxRequest { hash };
        let response = self
            .rpc_client()
            .await?
            .abci_query(
                Some(ABCI_GET_TX_PATH.to_string()),
                request.encode_to_vec(),
                ABCI_REQUEST_HEIGHT,
                ABCI_REQUEST_PROVE,
            )
            .await?;

        let tx = GetTxResponse::decode(response.value.as_slice())?;

        if let Some(tx_response) = tx.tx_response {
            match tx_response.code {
                TX_SUCCESS_CODE => Ok(Some(cosmrs::tendermint::abci::Code::Ok)),
                err_code => Ok(Some(cosmrs::tendermint::abci::Code::Err(
                    NonZeroU32::new(err_code).unwrap(),
                ))),
            }
        } else {
            Ok(None)
        }
    }

    // ————————————————————————————————————————————————————————————
    // Transaction construction & signing
    // ————————————————————————————————————————————————————————————

    pub(super) fn gen_simulated_tx(
        &self,
        account_info: &BaseAccount,
        priv_key: &Secp256k1Secret,
        tx_payload: Any,
        timeout_height: u64,
        memo: &str,
    ) -> cosmrs::Result<Vec<u8>> {
        let fee_amount = Coin {
            denom: self.protocol_info.denom.clone(),
            amount: 0_u64.into(),
        };
        let fee = Fee::from_amount_and_gas(fee_amount, GAS_LIMIT_DEFAULT);

        let signkey = SigningKey::from_slice(priv_key.as_slice())?;
        let tx_body = tx::Body::new(vec![tx_payload], memo, timeout_height as u32);
        let auth_info = SignerInfo::single_direct(Some(signkey.public_key()), account_info.sequence).auth_info(fee);
        let sign_doc = SignDoc::new(
            &tx_body,
            &auth_info,
            &self.protocol_info.chain_id,
            account_info.account_number,
        )?;
        sign_doc.sign(&signkey)?.to_bytes()
    }

    pub(super) fn any_to_signed_raw_tx(
        &self,
        priv_key: &Secp256k1Secret,
        account_info: &BaseAccount,
        tx_payload: Any,
        fee: Fee,
        timeout_height: u64,
        memo: &str,
    ) -> cosmrs::Result<Raw> {
        let signkey = SigningKey::from_slice(priv_key.as_slice())?;
        let tx_body = tx::Body::new(vec![tx_payload], memo, timeout_height as u32);
        let auth_info = SignerInfo::single_direct(Some(signkey.public_key()), account_info.sequence).auth_info(fee);
        let sign_doc = SignDoc::new(
            &tx_body,
            &auth_info,
            &self.protocol_info.chain_id,
            account_info.account_number,
        )?;
        sign_doc.sign(&signkey)
    }

    // ————————————————————————————————————————————————————————————
    // Fee calculation
    // ————————————————————————————————————————————————————————————

    pub(super) async fn calculate_fee(
        &self,
        msg: Any,
        timeout_height: u64,
        memo: &str,
        withdraw_fee: Option<WithdrawFee>,
    ) -> MmResult<Fee, TendermintCoinRpcError> {
        let activated_priv_key = if let Ok(key) = self.activation_policy.activated_key_or_err() {
            key
        } else {
            let (gas_price, gas_limit) = self.gas_info_for_withdraw(&withdraw_fee, GAS_LIMIT_DEFAULT);
            let amount = ((GAS_WANTED_BASE_VALUE * 1.5) * gas_price).ceil();
            let fee_amount = Coin {
                denom: self.platform_denom().clone(),
                amount: (amount as u64).into(),
            };
            return Ok(Fee::from_amount_and_gas(fee_amount, gas_limit));
        };

        let mut account_info = self.account_info(&self.account_id).await?;

        let (response, raw_response) = loop {
            let tx_bytes = self
                .gen_simulated_tx(&account_info, activated_priv_key, msg.clone(), timeout_height, memo)
                .map_to_mm(|e| TendermintCoinRpcError::InternalError(format!("{e}")))?;

            let request = AbciRequest::new(
                Some(ABCI_SIMULATE_TX_PATH.to_string()),
                SimulateRequest { tx_bytes, tx: None }.encode_to_vec(),
                ABCI_REQUEST_HEIGHT,
                ABCI_REQUEST_PROVE,
            );

            let raw_response = self.rpc_client().await?.perform(request).await?;
            let log = raw_response.response.log.to_string();

            if log.contains(ACCOUNT_SEQUENCE_ERR) {
                account_info.sequence = parse_expected_sequence_number(&log)?;
                debug!("Got wrong account sequence, trying again.");
                continue;
            }

            match raw_response.response.code {
                cosmrs::tendermint::abci::Code::Ok => {},
                cosmrs::tendermint::abci::Code::Err(ecode) => {
                    return MmError::err(TendermintCoinRpcError::InvalidResponse(format!(
                        "Could not read gas_info. Error code: {} Message: {}",
                        ecode, raw_response.response.log
                    )));
                },
            }

            break (
                SimulateResponse::decode(raw_response.response.value.as_slice())?,
                raw_response,
            );
        };

        let gas = response.gas_info.as_ref().ok_or_else(|| {
            TendermintCoinRpcError::InvalidResponse(format!("Could not read gas_info. Response: {raw_response:?}"))
        })?;

        let (gas_price, gas_limit) = self.gas_info_for_withdraw(&withdraw_fee, GAS_LIMIT_DEFAULT);
        let amount = ((gas.gas_used as f64 * 1.5) * gas_price).ceil();
        let fee_amount = Coin {
            denom: self.platform_denom().clone(),
            amount: (amount as u64).into(),
        };

        Ok(Fee::from_amount_and_gas(fee_amount, gas_limit))
    }

    pub(super) async fn calculate_account_fee_amount_as_u64(
        &self,
        account_id: &AccountId,
        priv_key: Option<Secp256k1Secret>,
        msg: Any,
        timeout_height: u64,
        memo: &str,
        withdraw_fee: Option<WithdrawFee>,
    ) -> MmResult<u64, TendermintCoinRpcError> {
        let priv_key = if let Some(pk) = priv_key {
            pk
        } else {
            let (gas_price, _) = self.gas_info_for_withdraw(&withdraw_fee, 0);
            return Ok(((GAS_WANTED_BASE_VALUE * 1.5) * gas_price).ceil() as u64);
        };

        let mut account_info = self.account_info(account_id).await?;

        let (response, raw_response) = loop {
            let tx_bytes = self
                .gen_simulated_tx(&account_info, &priv_key, msg.clone(), timeout_height, memo)
                .map_to_mm(|e| TendermintCoinRpcError::InternalError(format!("{e}")))?;

            let request = AbciRequest::new(
                Some(ABCI_SIMULATE_TX_PATH.to_string()),
                SimulateRequest { tx_bytes, tx: None }.encode_to_vec(),
                ABCI_REQUEST_HEIGHT,
                ABCI_REQUEST_PROVE,
            );

            let raw_response = self.rpc_client().await?.perform(request).await?;
            let log = raw_response.response.log.to_string();

            if log.contains(ACCOUNT_SEQUENCE_ERR) {
                account_info.sequence = parse_expected_sequence_number(&log)?;
                debug!("Got wrong account sequence, trying again.");
                continue;
            }

            match raw_response.response.code {
                cosmrs::tendermint::abci::Code::Ok => {},
                cosmrs::tendermint::abci::Code::Err(ecode) => {
                    return MmError::err(TendermintCoinRpcError::InvalidResponse(format!(
                        "Could not read gas_info. Error code: {} Message: {}",
                        ecode, raw_response.response.log
                    )));
                },
            }

            break (
                SimulateResponse::decode(raw_response.response.value.as_slice())?,
                raw_response,
            );
        };

        let gas = response.gas_info.as_ref().ok_or_else(|| {
            TendermintCoinRpcError::InvalidResponse(format!("Could not read gas_info. Response: {raw_response:?}"))
        })?;

        let (gas_price, _) = self.gas_info_for_withdraw(&withdraw_fee, 0);
        Ok(((gas.gas_used as f64 * 1.5) * gas_price).ceil() as u64)
    }

    // ————————————————————————————————————————————————————————————
    // Send raw TX (with sequence number retry)
    // ————————————————————————————————————————————————————————————

    pub(super) async fn common_send_raw_tx_bytes(
        &self,
        tx_payload: Any,
        fee: Fee,
        timeout_height: u64,
        memo: &str,
        _timeout: Duration,
    ) -> Result<(String, Raw), TransactionErr> {
        self.seq_safe_send_raw_tx_bytes(tx_payload, fee, timeout_height, memo)
            .await
    }

    async fn seq_safe_send_raw_tx_bytes(
        &self,
        tx_payload: Any,
        fee: Fee,
        timeout_height: u64,
        memo: &str,
    ) -> Result<(String, Raw), TransactionErr> {
        let mut account_info = try_tx_s!(self.account_info(&self.account_id).await);

        loop {
            let tx_raw = try_tx_s!(self.any_to_signed_raw_tx(
                try_tx_s!(self.activation_policy.activated_key_or_err()),
                &account_info,
                tx_payload.clone(),
                fee.clone(),
                timeout_height,
                memo,
            ));

            match self.send_raw_tx_bytes(try_tx_s!(&tx_raw.to_bytes())).compat().await {
                Ok(tx_id) => return Ok((tx_id, tx_raw)),
                Err(e) => {
                    if e.contains(ACCOUNT_SEQUENCE_ERR) {
                        account_info.sequence = try_tx_s!(parse_expected_sequence_number(&e));
                        debug!("Account sequence mismatch, retrying...");
                        continue;
                    }
                    return Err(TransactionErr::Plain(ERRL!("Transaction failed: {}", e)));
                },
            }
        }
    }

    // ————————————————————————————————————————————————————————————
    // HTLC helpers
    // ————————————————————————————————————————————————————————————

    pub(super) fn calculate_htlc_id(
        &self,
        from_address: &AccountId,
        to_address: &AccountId,
        amount: &[Coin],
        secret_hash: &[u8],
    ) -> String {
        let coins_string = amount
            .iter()
            .map(|t| format!("{}{}", t.amount, t.denom))
            .collect::<Vec<String>>()
            .join(",");

        let mut htlc_id = vec![];
        htlc_id.extend_from_slice(secret_hash);
        htlc_id.extend_from_slice(&from_address.to_bytes());
        htlc_id.extend_from_slice(&to_address.to_bytes());
        htlc_id.extend_from_slice(coins_string.as_bytes());
        sha256(&htlc_id).to_string().to_uppercase()
    }

    pub(super) fn gen_create_htlc_tx(
        &self,
        denom: Denom,
        to: &AccountId,
        amount: cosmrs::Amount,
        secret_hash: &[u8],
        time_lock: u64,
    ) -> MmResult<TendermintHtlc, HtlcMsgError> {
        let amount = vec![Coin { denom, amount }];
        let timestamp = 0_u64;

        let htlc_type = HtlcType::from_str(&self.protocol_info.account_prefix).map_err(|_| {
            HtlcMsgError::NotSupported(format!(
                "Account type '{}' is not supported for HTLCs",
                self.protocol_info.account_prefix
            ))
        })?;

        let msg_payload = CreateHtlcMsg::new(
            htlc_type,
            self.account_id.clone(),
            to.clone(),
            amount.clone(),
            hex::encode(secret_hash),
            timestamp,
            time_lock,
        );

        let htlc_id = self.calculate_htlc_id(&self.account_id, to, &amount, secret_hash);

        Ok(TendermintHtlc {
            id: htlc_id,
            msg_payload: msg_payload
                .to_any()
                .map_err(|e| MmError::new(HtlcMsgError::InvalidInput(e.to_string())))?,
        })
    }

    pub(super) fn gen_claim_htlc_tx(&self, htlc_id: String, secret: &[u8]) -> MmResult<TendermintHtlc, HtlcMsgError> {
        let htlc_type = HtlcType::from_str(&self.protocol_info.account_prefix).map_err(|_| {
            HtlcMsgError::NotSupported(format!(
                "Account type '{}' is not supported for HTLCs",
                self.protocol_info.account_prefix
            ))
        })?;

        let msg_payload = ClaimHtlcMsg::new(htlc_type, htlc_id.clone(), self.account_id.clone(), hex::encode(secret));

        Ok(TendermintHtlc {
            id: htlc_id,
            msg_payload: msg_payload
                .to_any()
                .map_err(|e| MmError::new(HtlcMsgError::InvalidInput(e.to_string())))?,
        })
    }

    pub(crate) async fn query_htlc(&self, id: String) -> MmResult<QueryHtlcResponse, TendermintCoinRpcError> {
        let htlc_type = HtlcType::from_str(&self.protocol_info.account_prefix).map_err(|_| {
            TendermintCoinRpcError::UnexpectedAccountType {
                prefix: self.protocol_info.account_prefix.clone(),
            }
        })?;

        let request = QueryHtlcRequestProto { id };
        let response = self
            .rpc_client()
            .await?
            .abci_query(
                Some(htlc_type.get_htlc_abci_query_path()),
                request.encode_to_vec(),
                ABCI_REQUEST_HEIGHT,
                ABCI_REQUEST_PROVE,
            )
            .await?;

        Ok(QueryHtlcResponse::decode(htlc_type, response.value.as_slice())?)
    }

    // ————————————————————————————————————————————————————————————
    // Search for swap tx spend (used by SwapOps)
    // ————————————————————————————————————————————————————————————

    pub(super) async fn search_for_swap_tx_spend(
        &self,
        tx_bytes: &[u8],
        secret_hash: &[u8],
    ) -> MmResult<Option<crate::FoundSwapTxSpend>, SearchForSwapTxSpendErr> {
        use super::htlc::{CreateHtlcProto, HTLC_STATE_COMPLETED, HTLC_STATE_OPEN, HTLC_STATE_REFUNDED};

        let tx = cosmrs::Tx::from_bytes(tx_bytes)?;
        let first_message = tx
            .body
            .messages
            .first()
            .or_mm_err(|| SearchForSwapTxSpendErr::TxMessagesEmpty)?;

        let htlc_type = HtlcType::from_str(&self.protocol_info.account_prefix).map_err(|_| {
            SearchForSwapTxSpendErr::UnexpectedAccountType {
                prefix: self.protocol_info.account_prefix.clone(),
            }
        })?;

        let htlc_proto = CreateHtlcProto::decode(htlc_type, first_message.value.as_slice())?;
        let htlc = CreateHtlcMsg::try_from(htlc_proto)?;
        let htlc_id = self.calculate_htlc_id(htlc.sender(), htlc.to(), htlc.amount(), secret_hash);

        let htlc_response = self.query_htlc(htlc_id.clone()).await.map_mm_err()?;

        let htlc_state = match htlc_response.htlc_state() {
            Some(state) => state,
            None => return Ok(None),
        };

        match htlc_state {
            HTLC_STATE_OPEN => Ok(None),
            HTLC_STATE_COMPLETED => {
                let query = format!("claim_htlc.id='{htlc_id}'");
                let request = TxSearchRequest {
                    query,
                    order_by: TendermintResultOrder::Ascending.into(),
                    page: 1,
                    per_page: 1,
                    prove: false,
                };

                let response = self
                    .rpc_client()
                    .await
                    .map_mm_err()?
                    .perform(request)
                    .await
                    .map_to_mm(TendermintCoinRpcError::from)
                    .map_mm_err()?;

                match response.txs.first() {
                    Some(raw_tx) => {
                        let tx = cosmrs::Tx::from_bytes(&raw_tx.tx)?;
                        let tx = TransactionEnum::CosmosTransaction(CosmosTransaction {
                            data: TxRaw {
                                body_bytes: tx.body.into_bytes()?,
                                auth_info_bytes: tx.auth_info.into_bytes()?,
                                signatures: tx.signatures,
                            },
                        });
                        Ok(Some(crate::FoundSwapTxSpend::Spent(tx)))
                    },
                    None => MmError::err(SearchForSwapTxSpendErr::ClaimHtlcTxNotFound),
                }
            },
            HTLC_STATE_REFUNDED => Ok(Some(crate::FoundSwapTxSpend::Refunded(
                TransactionEnum::CosmosTransaction(CosmosTransaction { data: TxRaw::default() }),
            ))),
            unexpected_state => MmError::err(SearchForSwapTxSpendErr::UnexpectedHtlcState(unexpected_state)),
        }
    }
}
