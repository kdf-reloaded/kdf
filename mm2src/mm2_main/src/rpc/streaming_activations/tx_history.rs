use async_trait::async_trait;
use coins::tx_history_streaming::{tx_history_error_payload, TxHistoryStreamerInput};
use coins::{lp_coinfind, MmCoinEnum};
use derive_more::Display;
use futures::future::{select, Either};
use http::StatusCode;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use mm2_event_stream::{mpsc, oneshot, Broadcaster, Event, EventStreamer, StreamerId};
use ser_error_derive::SerializeErrorType;
use serde::{Deserialize, Serialize};

use super::{EnableStreamingRequest, EnableStreamingResponse};

#[derive(Deserialize)]
pub struct EnableTxHistoryRequest {
    pub coin: String,
}

pub struct TxHistoryStreamer {
    ticker: String,
}

impl TxHistoryStreamer {
    pub fn new(ticker: String) -> Self { Self { ticker } }
}

#[async_trait]
impl EventStreamer for TxHistoryStreamer {
    type DataInType = TxHistoryStreamerInput;

    fn streamer_id(&self) -> StreamerId { StreamerId::TxHistory(self.ticker.clone()) }

    async fn handle(
        self,
        broadcaster: Broadcaster,
        ready_tx: oneshot::Sender<Result<(), String>>,
        shutdown_rx: oneshot::Receiver<()>,
        mut data_rx: mpsc::UnboundedReceiver<TxHistoryStreamerInput>,
    ) {
        let _ = ready_tx.send(Ok(()));

        let sid = StreamerId::TxHistory(self.ticker);
        let mut shutdown = core::pin::pin!(shutdown_rx);

        loop {
            let data = core::pin::pin!(data_rx.recv());
            match select(data, &mut shutdown).await {
                Either::Left((Some(TxHistoryStreamerInput::Records(records)), _)) => {
                    for record in records {
                        broadcaster.broadcast(Event::new(sid.clone(), record));
                    }
                },
                Either::Left((Some(TxHistoryStreamerInput::Error(error)), _)) => {
                    broadcaster.broadcast(Event::err(sid.clone(), tx_history_error_payload(error)));
                },
                Either::Left((None, _)) | Either::Right(_) => break,
            }
        }
    }
}

#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum TxHistoryStreamingError {
    #[display(fmt = "Coin {} is not activated", _0)]
    CoinIsNotActive(String),
    #[display(fmt = "Transaction-history streaming is not supported for {}", _0)]
    NotSupportedFor(String),
    #[display(fmt = "Could not add transaction-history streamer: {}", _0)]
    BrokerError(String),
    #[display(fmt = "Unexpected transaction-history activation error: {}", _0)]
    Internal(String),
}

impl common::HttpStatusCode for TxHistoryStreamingError {
    fn status_code(&self) -> StatusCode {
        match self {
            TxHistoryStreamingError::CoinIsNotActive(_) => StatusCode::NOT_FOUND,
            TxHistoryStreamingError::NotSupportedFor(_) => StatusCode::NOT_IMPLEMENTED,
            TxHistoryStreamingError::BrokerError(_) => StatusCode::BAD_REQUEST,
            TxHistoryStreamingError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

fn supports_tx_history_streaming(coin: &MmCoinEnum) -> bool {
    match coin {
        MmCoinEnum::UtxoCoin(_)
        | MmCoinEnum::QtumCoin(_)
        | MmCoinEnum::Qrc20Coin(_)
        | MmCoinEnum::Bch(_)
        | MmCoinEnum::SlpToken(_)
        | MmCoinEnum::TendermintCoin(_)
        | MmCoinEnum::TendermintToken(_) => true,
        #[cfg(not(target_arch = "wasm32"))]
        MmCoinEnum::ZCoin(_) => true,
        _ => false,
    }
}

pub async fn enable_tx_history(
    ctx: MmArc,
    req: EnableStreamingRequest<EnableTxHistoryRequest>,
) -> MmResult<EnableStreamingResponse, TxHistoryStreamingError> {
    let client_id = req.client_id;
    let ticker = req.inner.coin;
    let coin = lp_coinfind(&ctx, &ticker)
        .await
        .map_err(|e| MmError::new(TxHistoryStreamingError::Internal(e.to_string())))?
        .ok_or_else(|| MmError::new(TxHistoryStreamingError::CoinIsNotActive(ticker.clone())))?;

    if !supports_tx_history_streaming(&coin) {
        return MmError::err(TxHistoryStreamingError::NotSupportedFor(ticker));
    }

    let streamer = TxHistoryStreamer::new(ticker);
    let streamer_id = streamer.streamer_id().to_string();
    ctx.event_stream_manager
        .add(client_id, streamer)
        .await
        .map_err(|e| MmError::new(TxHistoryStreamingError::BrokerError(e)))?;

    Ok(EnableStreamingResponse::new(streamer_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use coins::tx_history_streaming::TxHistoryStreamerInput;
    use mm2_core::mm_ctx::MmCtxBuilder;
    use mm2_event_stream::EventStreamer;
    use serde_json::json;

    async fn recv_payload(
        rx: &mut mm2_event_stream::mpsc::Receiver<std::sync::Arc<mm2_event_stream::Event>>,
    ) -> serde_json::Value {
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("timeout waiting for tx-history event")
            .expect("event channel closed");
        assert_eq!(event.origin(), "TX_HISTORY:RICK");
        assert!(!event.is_error());
        event.get().1.clone()
    }

    #[test]
    fn tx_history_streamer_id_pins_activation_response_origin() {
        let streamer = TxHistoryStreamer::new("RICK".to_owned());
        assert_eq!(streamer.streamer_id().to_string(), "TX_HISTORY:RICK");
        let response = EnableStreamingResponse::new(streamer.streamer_id().to_string());
        assert_eq!(
            serde_json::to_value(response).unwrap(),
            json!({ "streamer_id": "TX_HISTORY:RICK" })
        );
    }

    #[tokio::test]
    async fn tx_history_streamer_emits_one_normal_event_per_record_without_timer() {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        let mut handle = ctx.event_stream_manager.new_client(1);
        ctx.event_stream_manager
            .add(1, TxHistoryStreamer::new("RICK".to_owned()))
            .await
            .unwrap();

        ctx.event_stream_manager
            .send(
                &StreamerId::TxHistory("RICK".to_owned()),
                TxHistoryStreamerInput::Records(vec![json!({ "internal_id": "a" }), json!({ "internal_id": "b" })]),
            )
            .unwrap();

        assert_eq!(recv_payload(&mut handle.rx).await, json!({ "internal_id": "a" }));
        assert_eq!(recv_payload(&mut handle.rx).await, json!({ "internal_id": "b" }));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), handle.rx.recv())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn tx_history_disable_removes_only_one_client_subscription() {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        let mut handle1 = ctx.event_stream_manager.new_client(1);
        let mut handle2 = ctx.event_stream_manager.new_client(2);
        ctx.event_stream_manager
            .add(1, TxHistoryStreamer::new("RICK".to_owned()))
            .await
            .unwrap();
        ctx.event_stream_manager
            .add(2, TxHistoryStreamer::new("RICK".to_owned()))
            .await
            .unwrap();

        super::super::disable_streaming(ctx.clone(), super::super::DisableStreamingRequest {
            client_id: 1,
            streamer_id: "TX_HISTORY:RICK".to_owned(),
        })
        .await
        .unwrap();

        ctx.event_stream_manager
            .send(
                &StreamerId::TxHistory("RICK".to_owned()),
                TxHistoryStreamerInput::Records(vec![json!({ "internal_id": "after-disable" })]),
            )
            .unwrap();

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), handle1.rx.recv())
                .await
                .is_err()
        );
        assert_eq!(
            recv_payload(&mut handle2.rx).await,
            json!({ "internal_id": "after-disable" })
        );
        assert!(ctx
            .event_stream_manager
            .is_active(&StreamerId::TxHistory("RICK".to_owned())));
    }
}
