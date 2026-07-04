use async_trait::async_trait;
use common::executor::spawn;
use common::log::warn;
use derive_more::Display;
use http::StatusCode;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use mm2_event_stream::{mpsc, oneshot, Broadcaster, Event, EventStreamer, StreamerId};
use ser_error_derive::SerializeErrorType;
use serde::de::{self, IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Serialize};
use std::fmt;
use tokio::signal::unix::{signal, Signal, SignalKind};

use super::{EnableStreamingRequest, EnableStreamingResponse};
use crate::mm2::lp_dispatcher::{dispatch_lp_event, StopCtxEvent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
// The `Sig` prefix mirrors the POSIX signal names these variants represent.
#[allow(clippy::enum_variant_names)]
pub enum ShutdownSignalName {
    SigInt,
    SigTerm,
    SigQuit,
}

impl ShutdownSignalName {
    fn as_str(self) -> &'static str {
        match self {
            ShutdownSignalName::SigInt => "SIGINT",
            ShutdownSignalName::SigTerm => "SIGTERM",
            ShutdownSignalName::SigQuit => "SIGQUIT",
        }
    }
}

pub struct ShutdownSignalInput {
    signal: ShutdownSignalName,
    delivered_tx: Option<oneshot::Sender<()>>,
}

impl ShutdownSignalInput {
    fn new(signal: ShutdownSignalName) -> Self {
        Self {
            signal,
            delivered_tx: None,
        }
    }

    fn with_ack(signal: ShutdownSignalName, delivered_tx: oneshot::Sender<()>) -> Self {
        Self {
            signal,
            delivered_tx: Some(delivered_tx),
        }
    }
}

pub struct ShutdownSignalStreamer;

#[async_trait]
impl EventStreamer for ShutdownSignalStreamer {
    type DataInType = ShutdownSignalInput;

    fn streamer_id(&self) -> StreamerId { StreamerId::ShutdownSignal }

    async fn handle(
        self,
        broadcaster: Broadcaster,
        ready_tx: oneshot::Sender<Result<(), String>>,
        shutdown_rx: oneshot::Receiver<()>,
        mut data_rx: mpsc::UnboundedReceiver<ShutdownSignalInput>,
    ) {
        let _ = ready_tx.send(Ok(()));
        let mut shutdown = core::pin::pin!(shutdown_rx);

        loop {
            let data = core::pin::pin!(data_rx.recv());
            match futures::future::select(data, &mut shutdown).await {
                futures::future::Either::Left((Some(input), _)) => {
                    broadcaster.broadcast(Event::new(
                        StreamerId::ShutdownSignal,
                        serde_json::json!(input.signal.as_str()),
                    ));
                    if let Some(delivered_tx) = input.delivered_tx {
                        let _ = delivered_tx.send(());
                    }
                },
                futures::future::Either::Left((None, _)) | futures::future::Either::Right(_) => break,
            }
        }
    }
}

pub struct EnableShutdownSignalRequest {}

impl<'de> Deserialize<'de> for EnableShutdownSignalRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct NoFieldsVisitor;

        impl<'de> Visitor<'de> for NoFieldsVisitor {
            type Value = EnableShutdownSignalRequest;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("no shutdown-signal-specific fields")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                if let Some(field) = map.next_key::<String>()? {
                    let _ = map.next_value::<IgnoredAny>()?;
                    return Err(de::Error::unknown_field(&field, &[]));
                }
                Ok(EnableShutdownSignalRequest {})
            }
        }

        deserializer.deserialize_map(NoFieldsVisitor)
    }
}

#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum ShutdownSignalStreamingError {
    #[display(fmt = "Unknown streaming client: {}", _0)]
    UnknownClient(u64),
    #[display(fmt = "Streaming client {} is already subscribed to SHUTDOWN_SIGNAL", _0)]
    DuplicateSubscription(u64),
    #[display(fmt = "Could not add shutdown-signal streamer: {}", _0)]
    BrokerError(String),
}

impl common::HttpStatusCode for ShutdownSignalStreamingError {
    fn status_code(&self) -> StatusCode { StatusCode::BAD_REQUEST }
}

pub async fn enable_shutdown_signal(
    ctx: MmArc,
    req: EnableStreamingRequest<EnableShutdownSignalRequest>,
) -> MmResult<EnableStreamingResponse, ShutdownSignalStreamingError> {
    let client_id = req.client_id;
    let streamer = ShutdownSignalStreamer;
    let streamer_id = streamer.streamer_id().to_string();

    if !ctx.event_stream_manager.client_registered(client_id) {
        return MmError::err(ShutdownSignalStreamingError::UnknownClient(client_id));
    }
    if ctx
        .event_stream_manager
        .client_subscribed_to(client_id, &StreamerId::ShutdownSignal)
    {
        return MmError::err(ShutdownSignalStreamingError::DuplicateSubscription(client_id));
    }

    ctx.event_stream_manager
        .add(client_id, streamer)
        .await
        .map_err(|e| MmError::new(ShutdownSignalStreamingError::BrokerError(e)))?;

    Ok(EnableStreamingResponse::new(streamer_id))
}

pub async fn publish_shutdown_signal(ctx: &MmArc, signal: ShutdownSignalName) -> Result<bool, String> {
    if !ctx.event_stream_manager.is_active(&StreamerId::ShutdownSignal) {
        return Ok(false);
    }

    let (delivered_tx, delivered_rx) = oneshot::channel();
    ctx.event_stream_manager.send(
        &StreamerId::ShutdownSignal,
        ShutdownSignalInput::with_ack(signal, delivered_tx),
    )?;
    delivered_rx
        .await
        .map_err(|_| "Shutdown-signal streamer stopped before acknowledging delivery".to_owned())?;
    Ok(true)
}

pub async fn handle_shutdown_signal(ctx: MmArc, signal: ShutdownSignalName) -> Result<(), String> {
    let _ = publish_shutdown_signal(&ctx, signal).await?;
    dispatch_lp_event(ctx.clone(), StopCtxEvent.into()).await;
    ctx.stop()
}

fn spawn_signal_task(ctx: MmArc, mut signal_stream: Signal, signal_name: ShutdownSignalName) {
    spawn(async move {
        if signal_stream.recv().await.is_some() {
            if let Err(err) = handle_shutdown_signal(ctx, signal_name).await {
                warn!("Error handling shutdown signal {}: {}", signal_name.as_str(), err);
            }
        }
    });
}

pub fn install_shutdown_signal_listener(ctx: MmArc) -> Result<(), String> {
    spawn_signal_task(
        ctx.clone(),
        signal(SignalKind::interrupt()).map_err(|e| e.to_string())?,
        ShutdownSignalName::SigInt,
    );
    spawn_signal_task(
        ctx.clone(),
        signal(SignalKind::terminate()).map_err(|e| e.to_string())?,
        ShutdownSignalName::SigTerm,
    );
    spawn_signal_task(
        ctx,
        signal(SignalKind::quit()).map_err(|e| e.to_string())?,
        ShutdownSignalName::SigQuit,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::HttpStatusCode;
    use mm2_core::mm_ctx::MmCtxBuilder;
    use serde_json::json;
    use std::time::Duration;

    async fn recv_shutdown_signal(
        rx: &mut mpsc::Receiver<std::sync::Arc<mm2_event_stream::Event>>,
    ) -> serde_json::Value {
        let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("timeout waiting for shutdown-signal event")
            .expect("event channel closed");
        assert_eq!(event.origin(), "SHUTDOWN_SIGNAL");
        assert!(!event.is_error());
        event.get().1.clone()
    }

    #[test]
    fn request_decoding_accepts_only_client_id_with_default_zero() {
        let req: EnableStreamingRequest<EnableShutdownSignalRequest> = serde_json::from_value(json!({})).unwrap();
        assert_eq!(req.client_id, 0);

        let req: EnableStreamingRequest<EnableShutdownSignalRequest> =
            serde_json::from_value(json!({ "client_id": 42 })).unwrap();
        assert_eq!(req.client_id, 42);

        let invalid_client: Result<EnableStreamingRequest<EnableShutdownSignalRequest>, _> =
            serde_json::from_value(json!({ "client_id": "not-a-u64" }));
        assert!(invalid_client.is_err());

        let unknown_field: Result<EnableStreamingRequest<EnableShutdownSignalRequest>, _> =
            serde_json::from_value(json!({ "client_id": 1, "signal": "SIGTERM" }));
        assert!(unknown_field.is_err());
    }

    #[tokio::test]
    async fn activation_returns_shutdown_signal_streamer_id() {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        let _handle = ctx.event_stream_manager.new_client(7);

        let res = enable_shutdown_signal(ctx.clone(), EnableStreamingRequest {
            client_id: 7,
            inner: EnableShutdownSignalRequest {},
        })
        .await
        .unwrap();

        assert_eq!(
            serde_json::to_value(res).unwrap(),
            json!({ "streamer_id": "SHUTDOWN_SIGNAL" })
        );
        assert!(ctx
            .event_stream_manager
            .client_subscribed_to(7, &StreamerId::ShutdownSignal));
    }

    #[tokio::test]
    async fn activation_rejects_unknown_client_and_duplicate_subscription_with_bad_request() {
        let ctx = MmCtxBuilder::default().into_mm_arc();

        let unknown = match enable_shutdown_signal(ctx.clone(), EnableStreamingRequest {
            client_id: 7,
            inner: EnableShutdownSignalRequest {},
        })
        .await
        {
            Ok(_) => panic!("unknown client unexpectedly activated shutdown-signal streamer"),
            Err(err) => err,
        };
        assert!(matches!(
            unknown.into_inner(),
            ShutdownSignalStreamingError::UnknownClient(7)
        ));

        let _handle = ctx.event_stream_manager.new_client(7);
        enable_shutdown_signal(ctx.clone(), EnableStreamingRequest {
            client_id: 7,
            inner: EnableShutdownSignalRequest {},
        })
        .await
        .unwrap();
        let duplicate = match enable_shutdown_signal(ctx, EnableStreamingRequest {
            client_id: 7,
            inner: EnableShutdownSignalRequest {},
        })
        .await
        {
            Ok(_) => panic!("duplicate client unexpectedly activated shutdown-signal streamer"),
            Err(err) => err,
        };
        assert!(matches!(
            duplicate.into_inner(),
            ShutdownSignalStreamingError::DuplicateSubscription(7)
        ));
        assert_eq!(
            ShutdownSignalStreamingError::BrokerError("failed".to_owned()).status_code(),
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn streamer_emits_json_string_for_each_supported_signal_without_timer() {
        for (signal, expected) in [
            (ShutdownSignalName::SigInt, json!("SIGINT")),
            (ShutdownSignalName::SigTerm, json!("SIGTERM")),
            (ShutdownSignalName::SigQuit, json!("SIGQUIT")),
        ] {
            let ctx = MmCtxBuilder::default().into_mm_arc();
            let mut handle = ctx.event_stream_manager.new_client(1);
            ctx.event_stream_manager.add(1, ShutdownSignalStreamer).await.unwrap();

            ctx.event_stream_manager
                .send(&StreamerId::ShutdownSignal, ShutdownSignalInput::new(signal))
                .unwrap();

            assert_eq!(recv_shutdown_signal(&mut handle.rx).await, expected);
            assert!(tokio::time::timeout(Duration::from_millis(100), handle.rx.recv())
                .await
                .is_err());
        }
    }

    #[tokio::test]
    async fn handle_shutdown_signal_publishes_before_stopping_runtime() {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        let mut handle = ctx.event_stream_manager.new_client(1);
        ctx.event_stream_manager.add(1, ShutdownSignalStreamer).await.unwrap();

        handle_shutdown_signal(ctx.clone(), ShutdownSignalName::SigTerm)
            .await
            .unwrap();

        assert_eq!(recv_shutdown_signal(&mut handle.rx).await, json!("SIGTERM"));
        assert!(ctx.is_stopping());
    }

    #[tokio::test]
    async fn handle_shutdown_signal_stops_runtime_without_active_streamer() {
        let ctx = MmCtxBuilder::default().into_mm_arc();

        handle_shutdown_signal(ctx.clone(), ShutdownSignalName::SigQuit)
            .await
            .unwrap();

        assert!(ctx.is_stopping());
        assert!(!ctx.event_stream_manager.is_active(&StreamerId::ShutdownSignal));
    }

    #[tokio::test]
    async fn disabling_final_subscriber_prevents_later_signal_delivery() {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        let mut handle = ctx.event_stream_manager.new_client(1);
        enable_shutdown_signal(ctx.clone(), EnableStreamingRequest {
            client_id: 1,
            inner: EnableShutdownSignalRequest {},
        })
        .await
        .unwrap();

        super::super::disable_streaming(ctx.clone(), super::super::DisableStreamingRequest {
            client_id: 1,
            streamer_id: "SHUTDOWN_SIGNAL".to_owned(),
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert!(!ctx.event_stream_manager.is_active(&StreamerId::ShutdownSignal));
        assert_eq!(
            publish_shutdown_signal(&ctx, ShutdownSignalName::SigInt).await,
            Ok(false)
        );
        assert!(tokio::time::timeout(Duration::from_millis(100), handle.rx.recv())
            .await
            .is_err());
    }
}
