use bigdecimal::BigDecimal;
use common::{now_ms, HttpStatusCode};
use derive_more::Display;
use http::StatusCode;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use rpc::v1::types::ToTxHash;

use crate::{lp_coinfind_or_err,
            utxo::{utxo_common::{big_decimal_from_sat_unsigned, merge_utxos_at_enabled_address, MergeConditions,
                                 UtxoMergeError},
                   UtxoCommonOps, UtxoFeeDetails},
            CoinFindError, MmCoinEnum, Transaction, TransactionDetails};

#[derive(Deserialize)]
pub struct ConsolidateUtxoRequest {
    coin: String,
    #[serde(default)]
    merge_conditions: MergeConditions,
    #[serde(default)]
    broadcast: bool,
}

#[derive(Serialize)]
pub struct ConsolidateUtxoResponse {
    tx: TransactionDetails,
    consolidated_utxos: Vec<SpentUtxo>,
}

#[derive(Serialize, Display, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum ConsolidateUtxoError {
    NoSuchCoin,
    CoinNotSupported,
    InvalidAddress(String),
    BadMergeConditions(String),
    InternalError(String),
}

impl HttpStatusCode for ConsolidateUtxoError {
    fn status_code(&self) -> StatusCode {
        match self {
            ConsolidateUtxoError::NoSuchCoin => StatusCode::NOT_FOUND,
            ConsolidateUtxoError::CoinNotSupported => StatusCode::BAD_REQUEST,
            ConsolidateUtxoError::InvalidAddress(_) => StatusCode::BAD_REQUEST,
            ConsolidateUtxoError::BadMergeConditions(_) => StatusCode::BAD_REQUEST,
            ConsolidateUtxoError::InternalError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl From<CoinFindError> for ConsolidateUtxoError {
    fn from(err: CoinFindError) -> Self {
        match err {
            CoinFindError::NoSuchCoin { .. } => ConsolidateUtxoError::NoSuchCoin,
        }
    }
}

impl From<UtxoMergeError> for ConsolidateUtxoError {
    fn from(err: UtxoMergeError) -> Self {
        match err {
            UtxoMergeError::BadMergeConditions(e) => ConsolidateUtxoError::BadMergeConditions(e),
            UtxoMergeError::InvalidAddress(e) => ConsolidateUtxoError::InvalidAddress(e),
            UtxoMergeError::InternalError(e) => ConsolidateUtxoError::InternalError(e),
        }
    }
}

#[derive(Serialize)]
struct SpentUtxo {
    txid: String,
    vout: u32,
    value: BigDecimal,
}

pub async fn consolidate_utxos_rpc(
    ctx: MmArc,
    request: ConsolidateUtxoRequest,
) -> MmResult<ConsolidateUtxoResponse, ConsolidateUtxoError> {
    let coin = lp_coinfind_or_err(&ctx, &request.coin).await.map_mm_err()?;
    match coin {
        MmCoinEnum::UtxoCoin(coin) => {
            // The coin's own address: the single address of an iguana coin, the enabled address of an HD coin
            // (CRD ch.05 R35b). The merge pays that same address.
            let (transaction, spent_utxos, from_address) =
                merge_utxos_at_enabled_address(&coin, &request.merge_conditions, request.broadcast)
                    .await
                    .map_mm_err()?;

            let received_by_me = transaction.outputs.iter().map(|o| o.value).sum();
            let received_by_me = big_decimal_from_sat_unsigned(received_by_me, coin.as_ref().decimals);

            let spent_by_me = spent_utxos.iter().map(|i| i.value).sum();
            let spent_by_me = big_decimal_from_sat_unsigned(spent_by_me, coin.as_ref().decimals);

            let tx = TransactionDetails {
                tx_json: None,
                from: vec![format!("{}", from_address)],
                to: vec![format!("{}", from_address)],
                received_by_me: received_by_me.clone(),
                spent_by_me: spent_by_me.clone(),
                total_amount: spent_by_me.clone(),
                my_balance_change: &received_by_me - &spent_by_me,
                tx_hash: transaction.hash().reversed().to_vec().to_tx_hash(),
                tx_hex: transaction.tx_hex().into(),
                fee_details: Some(crate::TxFeeDetails::Utxo(UtxoFeeDetails {
                    coin: Some(coin.as_ref().conf.ticker.clone()),
                    amount: &spent_by_me - &received_by_me,
                })),
                block_height: 0,
                coin: coin.as_ref().conf.ticker.clone(),
                internal_id: transaction.hash().reversed().to_vec().into(),
                timestamp: now_ms() / 1000,
                kmd_rewards: None,
                transaction_type: Default::default(),
            };

            Ok(ConsolidateUtxoResponse {
                tx,
                consolidated_utxos: spent_utxos
                    .into_iter()
                    .map(|spent| SpentUtxo {
                        txid: spent.outpoint.hash.reversed().to_string(),
                        vout: spent.outpoint.index,
                        value: big_decimal_from_sat_unsigned(spent.value, coin.as_ref().decimals),
                    })
                    .collect(),
            })
        },
        _ => Err(ConsolidateUtxoError::CoinNotSupported.into()),
    }
}
