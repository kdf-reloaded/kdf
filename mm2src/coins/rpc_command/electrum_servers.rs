//! The read-only `electrum_servers` request (CRD ch.38 R38.6.4o item 10): a snapshot of the pool of an active
//! Electrum-backed coin. It sends nothing to any server and changes nothing.

use common::HttpStatusCode;
use derive_more::Display;
use http::StatusCode;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;

use crate::utxo::rpc_clients::electrum_pool::ElectrumServersView;
use crate::utxo::rpc_clients::UtxoRpcClientEnum;
use crate::utxo::UtxoCoinFields;
use crate::{lp_coinfind_or_err, CoinFindError, MmCoinEnum};

pub type ElectrumServersRpcResult<T> = Result<T, MmError<ElectrumServersError>>;

#[derive(Deserialize)]
pub struct ElectrumServersRequest {
    coin: String,
}

#[derive(Debug, Serialize, Display, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum ElectrumServersError {
    /// The ticker is not active.
    NoSuchCoin(String),
    /// The coin is active but is not served by the Electrum client (a native daemon, lightwalletd or a coin that is
    /// not a UTXO coin).
    #[display(fmt = "The coin {} is not served by an Electrum client.", _0)]
    CoinIsNotElectrum(String),
}

impl HttpStatusCode for ElectrumServersError {
    fn status_code(&self) -> StatusCode {
        match self {
            ElectrumServersError::NoSuchCoin(_) => StatusCode::NOT_FOUND,
            ElectrumServersError::CoinIsNotElectrum(_) => StatusCode::BAD_REQUEST,
        }
    }
}

impl From<CoinFindError> for ElectrumServersError {
    fn from(err: CoinFindError) -> Self { Self::NoSuchCoin(err.to_string()) }
}

/// The UTXO fields of `coin`, if it is a coin that has any.
fn utxo_fields(coin: &MmCoinEnum) -> Option<&UtxoCoinFields> {
    match coin {
        MmCoinEnum::UtxoCoin(coin) => Some(coin.as_ref()),
        MmCoinEnum::QtumCoin(coin) => Some(coin.as_ref()),
        MmCoinEnum::Qrc20Coin(coin) => Some(coin.as_ref()),
        MmCoinEnum::Bch(coin) => Some(coin.as_ref()),
        MmCoinEnum::SlpToken(coin) => Some(coin.as_ref()),
        MmCoinEnum::ZCoin(coin) => Some(coin.as_ref()),
        _ => None,
    }
}

/// Answers `electrum_servers`: the pool of the coin, or `NoSuchCoin` for a ticker that is not active and
/// `CoinIsNotElectrum` for an active coin the Electrum client does not serve.
pub async fn electrum_servers_rpc(
    ctx: MmArc,
    req: ElectrumServersRequest,
) -> ElectrumServersRpcResult<ElectrumServersView> {
    let coin = lp_coinfind_or_err(&ctx, &req.coin).await.map_mm_err()?;
    match utxo_fields(&coin).map(|fields| &fields.rpc_client) {
        Some(UtxoRpcClientEnum::Electrum(client)) => Ok(client.servers_view()),
        _ => MmError::err(ElectrumServersError::CoinIsNotElectrum(req.coin)),
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::utxo::rpc_clients::electrum_stub_server::{StubBehavior, StubServer};
    use crate::utxo::rpc_clients::{ElectrumClient, ElectrumClientImpl, NativeClient, NativeClientImpl};
    use crate::utxo::utxo_tests::{software_coin_for_test, utxo_coin_fields_for_test, utxo_coin_from_fields};
    use crate::CoinsContext;
    use common::block_on;
    use mm2_core::mm_ctx::MmCtxBuilder;
    use serde_json::Value as Json;
    use std::sync::Arc;

    fn ctx_with(coin: impl Into<MmCoinEnum>) -> MmArc {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();
        block_on(coins_ctx.add_coin(coin.into())).unwrap();
        ctx
    }

    fn call(ctx: &MmArc, coin: &str) -> Result<Json, ElectrumServersError> {
        block_on(electrum_servers_rpc(ctx.clone(), ElectrumServersRequest {
            coin: coin.to_owned(),
        }))
        .map(|view| serde_json::to_value(&view).unwrap())
        .map_err(|e| e.into_inner())
    }

    /// T38.6.4o.10: a ticker that is not active answers `NoSuchCoin`.
    #[test]
    fn an_inactive_ticker_is_no_such_coin() {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        let error = call(&ctx, "NOTACTIVE").unwrap_err();
        assert!(matches!(error, ElectrumServersError::NoSuchCoin(_)), "{error}");
        assert_eq!(error.status_code(), StatusCode::NOT_FOUND);
    }

    /// T38.6.4o.10: an active coin served by a native daemon answers `CoinIsNotElectrum`.
    #[test]
    fn a_coin_with_a_native_daemon_is_not_electrum() {
        let ctx = ctx_with(software_coin_for_test("NATIVE"));
        let error = call(&ctx, "NATIVE").unwrap_err();
        assert!(matches!(error, ElectrumServersError::CoinIsNotElectrum(_)), "{error}");
        assert_eq!(error.status_code(), StatusCode::BAD_REQUEST);
    }

    /// T38.6.4o.10: an Electrum-backed coin answers the snapshot of its pool, in the shape of the chapter.
    #[test]
    fn an_electrum_coin_answers_its_pool() {
        let ticker = "ELEC";
        let (a, b, client) = block_on(async {
            let (a, b) = (
                StubServer::start(StubBehavior::Answer).await,
                StubServer::start(StubBehavior::Answer).await,
            );
            let client = ElectrumClientImpl::new(ticker.to_owned(), vec![]);
            client.add_server(&a.request()).unwrap();
            client.add_server(&b.request()).unwrap();
            let client = Arc::new(client);
            client.activation_probe().await.unwrap();
            (a, b, client)
        });
        let native = NativeClient(Arc::new(NativeClientImpl::default()));
        let mut fields = utxo_coin_fields_for_test(UtxoRpcClientEnum::Native(native), None, false);
        fields.conf.ticker = ticker.to_owned();
        fields.rpc_client = ElectrumClient(client).into();
        let ctx = ctx_with(utxo_coin_from_fields(fields));

        let answer = call(&ctx, ticker).unwrap();
        assert_eq!(answer["coin"], ticker);
        assert_eq!(answer["server_order"], "listed");
        assert_eq!(answer["tolerance_blocks"], 1);
        // The stubs answer no tip, so there is no height and no agreed tip.
        assert!(answer["agreed_tip"].is_null());
        let servers = answer["servers"].as_array().unwrap();
        assert_eq!(servers.len(), 2);
        for (rank, (server, stub)) in servers.iter().zip([&a, &b]).enumerate() {
            assert_eq!(server["server"], stub.addr());
            assert_eq!(server["rank"], rank);
            assert_eq!(server["removed"], false);
            for key in [
                "maintained",
                "near",
                "height",
                "lag",
                "out_of_date",
                "latency_ms",
                "suspended",
                "poor_demoted",
            ] {
                assert!(server.get(key).is_some(), "{key} is missing: {server}");
            }
        }
    }
}
