/// EIP-1559 fee-estimator event streamer.
///
/// Produces a continuous, timer-paced EIP-1559 fee-per-gas estimate for a
/// single EVM coin and emits it as an SSE event every cycle (timer-paced,
/// not emit-on-change). Each coin gets its own streamer instance identified
/// by `StreamerId::FeeEstimation(ticker)`.
use async_trait::async_trait;
use common::executor::Timer;
use common::log;
use derive_more::Display;
use futures::future::{select, Either};
use http::StatusCode;
use mm2_event_stream::{mpsc, oneshot, Broadcaster, Event, EventStreamer, StreamerId};
use ser_error_derive::SerializeErrorType;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use std::convert::TryFrom;

use super::{EnableStreamingRequest, EnableStreamingResponse};
use coins::eth::fee_estimation::ser::FeePerGasEstimated;
use coins::eth::EthCoin;
use coins::{lp_coinfind, MmCoinEnum};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;

/// Cadence floor (seconds): if the remaining wait after a cycle falls below
/// this, the next cycle begins immediately (R32).
const RESTART_FLOOR: f64 = 0.1;

/// Which estimation strategy the streamer uses.
///
/// `Simple` selects the internal historical estimator; `Provider` selects the
/// external gas-API provider configured on the coin itself.
#[derive(Clone, Copy, Default, Deserialize)]
pub enum FeeEstimatorType {
    #[default]
    Simple,
    Provider,
}

/// Estimator configuration object (the `config` field of the request).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeeEstimatorConfig {
    /// Target cadence in seconds between successive re-estimations. Default: 15.
    #[serde(default = "default_estimate_every")]
    pub estimate_every: f64,
    /// Estimation strategy. Default: `Simple`.
    #[serde(default)]
    pub estimator_type: FeeEstimatorType,
}

fn default_estimate_every() -> f64 { 15.0 }

/// Per-coin activation request for the fee-estimator streamer.
#[derive(Deserialize)]
pub struct EnableFeeEstimatorRequest {
    /// EVM coin ticker to estimate fees for (must already be activated).
    pub coin: String,
    /// Estimator configuration (minimal accepted form is the empty object `{}`).
    pub config: FeeEstimatorConfig,
}

/// The fee-estimator streamer for a single EVM coin.
pub struct FeeEstimatorStreamer {
    ticker: String,
    estimate_every: f64,
    use_simple: bool,
    coin: EthCoin,
}

impl FeeEstimatorStreamer {
    pub fn new(ticker: String, estimate_every: f64, estimator_type: FeeEstimatorType, coin: EthCoin) -> Self {
        Self {
            ticker,
            estimate_every,
            use_simple: matches!(estimator_type, FeeEstimatorType::Simple),
            coin,
        }
    }
}

#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum FeeEstimatorStreamingError {
    #[display(fmt = "Coin {} is not activated", _0)]
    CoinIsNotActive(String),
    #[display(fmt = "EIP-1559 fee-estimator streaming is not supported for {}", _0)]
    NotSupportedFor(String),
    #[display(fmt = "Could not add fee-estimator streamer: {}", _0)]
    BrokerError(String),
    #[display(fmt = "Unexpected fee-estimator activation error: {}", _0)]
    Internal(String),
}

impl common::HttpStatusCode for FeeEstimatorStreamingError {
    fn status_code(&self) -> StatusCode {
        match self {
            FeeEstimatorStreamingError::CoinIsNotActive(_) => StatusCode::NOT_FOUND,
            FeeEstimatorStreamingError::NotSupportedFor(_) => StatusCode::NOT_IMPLEMENTED,
            FeeEstimatorStreamingError::BrokerError(_) => StatusCode::BAD_REQUEST,
            FeeEstimatorStreamingError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

fn require_evm_coin(ticker: &str, coin: MmCoinEnum) -> Result<EthCoin, FeeEstimatorStreamingError> {
    match coin {
        MmCoinEnum::EthCoin(coin) => Ok(coin),
        _ => Err(FeeEstimatorStreamingError::NotSupportedFor(ticker.to_owned())),
    }
}

#[async_trait]
impl EventStreamer for FeeEstimatorStreamer {
    type DataInType = mm2_event_stream::NoDataIn;

    fn streamer_id(&self) -> StreamerId { StreamerId::FeeEstimation(self.ticker.clone()) }

    async fn handle(
        self,
        broadcaster: Broadcaster,
        ready_tx: oneshot::Sender<Result<(), String>>,
        shutdown_rx: oneshot::Receiver<()>,
        _data_rx: mpsc::UnboundedReceiver<mm2_event_stream::NoDataIn>,
    ) {
        let _ = ready_tx.send(Ok(()));

        let ticker = self.ticker;
        let estimate_every = self.estimate_every;
        let use_simple = self.use_simple;
        let coin = self.coin;
        let sid = StreamerId::FeeEstimation(ticker.clone());
        let mut shutdown = shutdown_rx;

        loop {
            let start = common::now_float();

            // Re-estimate and broadcast unconditionally (timer-paced, not emit-on-change).
            match coin.get_eip1559_gas_fee(use_simple).await {
                Ok(fee) => match FeePerGasEstimated::try_from(fee) {
                    Ok(estimate) => match serde_json::to_value(&estimate) {
                        Ok(payload) => broadcaster.broadcast(Event::new(sid.clone(), payload)),
                        Err(e) => {
                            log::error!("Fee estimate serialization error for {}: {}", ticker, e);
                            broadcaster.broadcast(Event::err(sid.clone(), json!({ "error": e.to_string() })));
                        },
                    },
                    Err(e) => {
                        log::error!("Fee estimate conversion error for {}: {}", ticker, e);
                        broadcaster.broadcast(Event::err(sid.clone(), json!({ "error": e.to_string() })));
                    },
                },
                Err(e) => {
                    log::error!("Fee estimation error for {}: {}", ticker, e);
                    broadcaster.broadcast(Event::err(sid.clone(), json!({ "error": e.to_string() })));
                },
            }

            // Wait `estimate_every` minus the elapsed estimation time of this cycle.
            let wait = estimate_every - (common::now_float() - start);
            if wait < RESTART_FLOOR {
                // Below the floor: begin the next cycle immediately, but still
                // honour an already-fired shutdown signal.
                match shutdown.try_recv() {
                    Ok(()) | Err(oneshot::error::TryRecvError::Closed) => break,
                    Err(oneshot::error::TryRecvError::Empty) => continue,
                }
            }

            let sleep = core::pin::pin!(Timer::sleep(wait));
            match select(sleep, &mut shutdown).await {
                Either::Left(_) => {},
                Either::Right(_) => break,
            }
        }
    }
}

/// RPC handler for `stream::fee_estimator::enable`.
pub async fn enable_fee_estimator(
    ctx: MmArc,
    req: EnableStreamingRequest<EnableFeeEstimatorRequest>,
) -> MmResult<EnableStreamingResponse, FeeEstimatorStreamingError> {
    let client_id = req.client_id;
    let ticker = req.inner.coin.clone();
    let estimate_every = req.inner.config.estimate_every;
    let estimator_type = req.inner.config.estimator_type;

    let coin = lp_coinfind(&ctx, &ticker)
        .await
        .map_err(|e| MmError::new(FeeEstimatorStreamingError::Internal(e)))?
        .ok_or_else(|| MmError::new(FeeEstimatorStreamingError::CoinIsNotActive(ticker.clone())))?;
    let coin = require_evm_coin(&ticker, coin).map_err(MmError::new)?;

    let streamer = FeeEstimatorStreamer::new(ticker, estimate_every, estimator_type, coin);
    let streamer_id = streamer.streamer_id().to_string();
    ctx.event_stream_manager
        .add(client_id, streamer)
        .await
        .map_err(|e| MmError::new(FeeEstimatorStreamingError::BrokerError(e)))?;

    Ok(EnableStreamingResponse::new(streamer_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use coins::{CoinsContext, TestCoin};
    use common::HttpStatusCode;
    use mm2_core::mm_ctx::MmCtxBuilder;
    use mm2_event_stream::StreamerId;

    #[test]
    fn fee_estimation_wire_string() {
        assert_eq!(
            StreamerId::FeeEstimation("ETH".to_string()).to_string(),
            "FEE_ESTIMATION:ETH"
        );
    }

    #[test]
    fn config_defaults_from_empty_object() {
        let cfg: FeeEstimatorConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg.estimate_every, 15.0);
        assert!(matches!(cfg.estimator_type, FeeEstimatorType::Simple));
    }

    #[test]
    fn config_rejects_unknown_fields() {
        let res: Result<FeeEstimatorConfig, _> = serde_json::from_str(r#"{"bogus": 1}"#);
        assert!(res.is_err());
    }

    #[test]
    fn config_parses_provider_and_estimate_every() {
        let cfg: FeeEstimatorConfig =
            serde_json::from_str(r#"{"estimate_every": 5.5, "estimator_type": "Provider"}"#).unwrap();
        assert_eq!(cfg.estimate_every, 5.5);
        assert!(matches!(cfg.estimator_type, FeeEstimatorType::Provider));
    }

    #[test]
    fn activation_errors_map_to_r38_1_status_codes() {
        assert_eq!(
            FeeEstimatorStreamingError::CoinIsNotActive("ETH".to_owned()).status_code(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            FeeEstimatorStreamingError::NotSupportedFor("KMD".to_owned()).status_code(),
            StatusCode::NOT_IMPLEMENTED
        );
        assert_eq!(
            FeeEstimatorStreamingError::BrokerError("setup failed".to_owned()).status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            FeeEstimatorStreamingError::Internal("lookup failed".to_owned()).status_code(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn require_evm_coin_rejects_activated_non_evm_coin() {
        let err = require_evm_coin("RICK", MmCoinEnum::Test(TestCoin::new("RICK"))).unwrap_err();
        assert!(matches!(err, FeeEstimatorStreamingError::NotSupportedFor(ticker) if ticker == "RICK"));
    }

    #[tokio::test]
    async fn enable_fee_estimator_missing_coin_fails_before_subscription() {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        let _handle = ctx.event_stream_manager.new_client(7);

        let err = match enable_fee_estimator(ctx.clone(), EnableStreamingRequest {
            client_id: 7,
            inner: EnableFeeEstimatorRequest {
                coin: "MISSING".to_owned(),
                config: FeeEstimatorConfig {
                    estimate_every: 15.0,
                    estimator_type: FeeEstimatorType::Simple,
                },
            },
        })
        .await
        {
            Ok(_) => panic!("missing coin activation unexpectedly succeeded"),
            Err(err) => err,
        };

        assert!(matches!(
            err.into_inner(),
            FeeEstimatorStreamingError::CoinIsNotActive(ticker) if ticker == "MISSING"
        ));
        let streamer_id = StreamerId::FeeEstimation("MISSING".to_owned());
        assert!(!ctx.event_stream_manager.is_active(&streamer_id));
        assert!(!ctx.event_stream_manager.client_subscribed_to(7, &streamer_id));
    }

    #[tokio::test]
    async fn enable_fee_estimator_non_evm_coin_fails_before_subscription() {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();
        coins_ctx
            .add_coin(MmCoinEnum::Test(TestCoin::new("RICK")))
            .await
            .unwrap();
        let _handle = ctx.event_stream_manager.new_client(7);

        let err = match enable_fee_estimator(ctx.clone(), EnableStreamingRequest {
            client_id: 7,
            inner: EnableFeeEstimatorRequest {
                coin: "RICK".to_owned(),
                config: FeeEstimatorConfig {
                    estimate_every: 15.0,
                    estimator_type: FeeEstimatorType::Simple,
                },
            },
        })
        .await
        {
            Ok(_) => panic!("non-EVM coin activation unexpectedly succeeded"),
            Err(err) => err,
        };

        assert!(matches!(
            err.into_inner(),
            FeeEstimatorStreamingError::NotSupportedFor(ticker) if ticker == "RICK"
        ));
        let streamer_id = StreamerId::FeeEstimation("RICK".to_owned());
        assert!(!ctx.event_stream_manager.is_active(&streamer_id));
        assert!(!ctx.event_stream_manager.client_subscribed_to(7, &streamer_id));
    }
}
