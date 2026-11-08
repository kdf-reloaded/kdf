use bigdecimal::BigDecimal;
use common::HttpStatusCode;
use derive_more::Display;
use http::StatusCode;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;

use crate::hd_wallet::{HDAccountOps, HDWalletCoinOps, HDWalletOps};
use crate::{lp_coinfind_or_err,
            utxo::{rpc_clients::UnspentInfo, utxo_common::big_decimal_from_sat_unsigned, GetUtxoListOps},
            CoinFindError, DerivationMethod, MmCoinEnum};
use crypto::Bip44Chain;

#[derive(Deserialize)]
pub struct FetchUtxosRequest {
    pub coin: String,
}

#[derive(Serialize)]
pub struct AddressUtxos {
    pub address: String,
    pub count: usize,
    pub utxos: Vec<UnspentOutputs>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub derivation_path: Option<String>,
}

#[derive(Serialize)]
pub struct FetchUtxosResponse {
    pub total_count: usize,
    pub addresses: Vec<AddressUtxos>,
}

#[derive(Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum FetchUtxosError {
    NoSuchCoin,
    CoinNotSupported,
    InvalidAddress(String),
    Internal(String),
}

impl HttpStatusCode for FetchUtxosError {
    fn status_code(&self) -> StatusCode {
        match self {
            FetchUtxosError::NoSuchCoin => StatusCode::NOT_FOUND,
            FetchUtxosError::CoinNotSupported => StatusCode::BAD_REQUEST,
            FetchUtxosError::InvalidAddress(_) => StatusCode::BAD_REQUEST,
            FetchUtxosError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl From<CoinFindError> for FetchUtxosError {
    fn from(e: CoinFindError) -> Self {
        match e {
            CoinFindError::NoSuchCoin { .. } => FetchUtxosError::NoSuchCoin,
        }
    }
}

#[derive(Serialize)]
pub struct UnspentOutputs {
    txid: String,
    vout: u32,
    value: BigDecimal,
}

fn unspent_outputs(unspents: Vec<UnspentInfo>, decimals: u8) -> Vec<UnspentOutputs> {
    unspents
        .into_iter()
        .map(|u| UnspentOutputs {
            txid: u.outpoint.hash.reversed().to_string(),
            vout: u.outpoint.index,
            value: big_decimal_from_sat_unsigned(u.value, decimals),
        })
        .collect()
}

pub async fn fetch_utxos_rpc(ctx: MmArc, req: FetchUtxosRequest) -> MmResult<FetchUtxosResponse, FetchUtxosError> {
    let coin = lp_coinfind_or_err(&ctx, &req.coin).await.map_mm_err()?;

    match coin {
        MmCoinEnum::UtxoCoin(ref coin) => match &coin.as_ref().derivation_method {
            DerivationMethod::Iguana(my_address) => {
                let (unspents, _) = coin
                    .get_unspent_ordered_list(my_address)
                    .await
                    .mm_err(|e| FetchUtxosError::Internal(format!("Couldn't fetch unspent UTXOs: {e}")))?;

                let decimals = coin.as_ref().decimals;
                let addresses_utxos = if unspents.is_empty() {
                    vec![]
                } else {
                    vec![AddressUtxos {
                        address: format!("{}", my_address),
                        count: unspents.len(),
                        utxos: unspent_outputs(unspents, decimals),
                        derivation_path: None,
                    }]
                };

                let total_count = addresses_utxos.iter().map(|a| a.count).sum();
                Ok(FetchUtxosResponse {
                    total_count,
                    addresses: addresses_utxos,
                })
            },
            // CRD ch.05 R35b: under HD the unspent outputs of every known address of every account, on both
            // chains. Only addresses the wallet already knows are read; none is derived beyond the known count
            // and none is persisted.
            DerivationMethod::HDWallet(hd_wallet) => {
                let decimals = coin.as_ref().decimals;
                let mut addresses_utxos = Vec::new();
                for (_account_id, account) in hd_wallet.get_accounts().await {
                    for chain in [Bip44Chain::External, Bip44Chain::Internal] {
                        let known_addresses_number = account.known_addresses_number(chain).mm_err(|e| {
                            FetchUtxosError::Internal(format!("Couldn't count the known addresses: {e}"))
                        })?;
                        for address_id in 0..known_addresses_number {
                            let hd_address = coin.derive_address(&account, chain, address_id).mm_err(|e| {
                                FetchUtxosError::InvalidAddress(format!("Couldn't derive an address: {e}"))
                            })?;
                            let (unspents, recently_spent) =
                                coin.get_unspent_ordered_list(&hd_address.address).await.mm_err(|e| {
                                    FetchUtxosError::Internal(format!("Couldn't fetch unspent UTXOs: {e}"))
                                })?;
                            drop(recently_spent);
                            if unspents.is_empty() {
                                continue;
                            }
                            addresses_utxos.push(AddressUtxos {
                                address: format!("{}", hd_address.address),
                                count: unspents.len(),
                                utxos: unspent_outputs(unspents, decimals),
                                derivation_path: Some(hd_address.derivation_path.to_string()),
                            });
                        }
                    }
                }

                let total_count = addresses_utxos.iter().map(|a| a.count).sum();
                Ok(FetchUtxosResponse {
                    total_count,
                    addresses: addresses_utxos,
                })
            },
        },
        _ => Err(FetchUtxosError::CoinNotSupported.into()),
    }
}
