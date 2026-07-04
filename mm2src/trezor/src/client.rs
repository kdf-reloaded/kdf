//! This file is inspired by https://github.com/tezedge/tezedge-client/blob/master/trezor_api/src/client.rs

use crate::error::OperationFailure;
use crate::proto::messages::MessageType;
use crate::proto::messages_common as proto_common;
use crate::proto::messages_management as proto_management;
use crate::proto::{ProtoMessage, TrezorMessage};
use crate::response::TrezorResponse;
use crate::result_handler::ResultHandler;
use crate::transport::Transport;
use crate::{TrezorError, TrezorResult};
use futures::lock::{Mutex as AsyncMutex, MutexGuard as AsyncMutexGuard};
use mm2_err_handle::prelude::*;
use std::sync::Arc;

#[derive(Clone)]
pub struct TrezorClient {
    inner: Arc<AsyncMutex<TrezorClientImpl>>,
}

impl TrezorClient {
    pub fn from_transport<T>(transport: T) -> TrezorClient
    where
        T: Transport + Send + Sync + 'static,
    {
        let transport = Box::new(transport);
        let inner = Arc::new(AsyncMutex::new(TrezorClientImpl { transport }));
        TrezorClient { inner }
    }

    /// Initialize a Trezor session by sending
    /// [Initialize](https://docs.trezor.io/trezor-firmware/common/communication/sessions.html#examples).
    pub async fn session(&self) -> TrezorResult<TrezorSession<'_>> {
        let mut session = TrezorSession {
            inner: self.inner.lock().await,
        };
        session.initialize_device().await?;
        Ok(session)
    }

    /// Lightweight, non-mutating reachability probe.
    ///
    /// Returns `true` when the device is reachable: either the session is
    /// already held by a concurrent task (reported as reachable *without*
    /// contending for the session lock), or a fresh `Initialize` exchange
    /// succeeds. Returns `false` when the session is free but the underlying
    /// transport/device call reports failure. This never enqueues a
    /// user-interaction request.
    pub async fn is_connected(&self) -> bool {
        // Don't contend for the session: if another task already holds it the
        // device is in use and therefore reachable.
        let guard = match self.inner.try_lock() {
            Some(guard) => guard,
            None => return true,
        };
        let mut session = TrezorSession { inner: guard };
        session.initialize_device().await.is_ok()
    }
}

pub struct TrezorClientImpl {
    transport: Box<dyn Transport + Send + Sync + 'static>,
}

pub struct TrezorSession<'a> {
    inner: AsyncMutexGuard<'a, TrezorClientImpl>,
}

impl<'a> TrezorSession<'a> {
    /// Sends a message and returns a TrezorResponse with either the
    /// expected response message, a failure or an interaction request.
    pub async fn call<'b, T: 'static, S: TrezorMessage>(
        &'b mut self,
        message: S,
        result_handler: ResultHandler<T>,
    ) -> TrezorResult<TrezorResponse<'a, 'b, T>> {
        let resp = self.call_raw(message).await?;
        match resp.message_type() {
            mt if mt == result_handler.message_type() => Ok(TrezorResponse::Ready(result_handler.handle_raw(resp)?)),
            MessageType::Failure => {
                let fail_msg: proto_common::Failure = resp.into_message()?;
                MmError::err(TrezorError::Failure(OperationFailure::from(fail_msg)))
            },
            MessageType::ButtonRequest => {
                let req_msg = resp.into_message()?;
                Ok(TrezorResponse::new_button_request(self, req_msg, result_handler))
            },
            MessageType::PinMatrixRequest => {
                let req_msg = resp.into_message()?;
                Ok(TrezorResponse::new_pin_matrix_request(self, req_msg, result_handler))
            },
            MessageType::PassphraseRequest => {
                let req_msg = resp.into_message()?;
                Ok(TrezorResponse::new_passphrase_request(self, req_msg, result_handler))
            },
            mtype => MmError::err(TrezorError::UnexpectedMessageType(mtype)),
        }
    }

    /// Sends a message and returns the raw ProtoMessage struct that was
    /// responded by the device.
    async fn call_raw<S: TrezorMessage>(&mut self, message: S) -> TrezorResult<ProtoMessage> {
        let mut buf = Vec::with_capacity(message.encoded_len());
        message.encode(&mut buf)?;

        let proto_msg = ProtoMessage::new(S::message_type(), buf);
        self.inner.transport.write_message(proto_msg).await?;
        self.inner.transport.read_message().await
    }

    /// Initialize the device.
    ///
    /// The Initialize packet will cause the device to stop what it is currently doing
    /// and should work at any time.
    /// Thus, it can also be used to recover from previous errors.
    ///
    /// # Usage
    ///
    /// Must be called before sending requests to Trezor.
    async fn initialize_device(&mut self) -> TrezorResult<proto_management::Features> {
        // Don't set the session_id since currently there is no need to restore the previous session.
        // https://docs.trezor.io/trezor-firmware/common/communication/sessions.html#session-lifecycle
        let req = proto_management::Initialize { session_id: None };

        let result_handler = ResultHandler::<proto_management::Features>::new(Ok);
        self.call(req, result_handler).await?.ok()
    }

    pub(crate) async fn cancel_last_op(&mut self) {
        let req = proto_management::Cancel {};
        let result_handler = ResultHandler::new(|_m: proto_common::Failure| Ok(()));
        // Ignore result.
        self.call(req, result_handler).await.ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::messages::MessageType;
    use crate::proto::messages_management::Features;
    use crate::proto::{ProtoMessage, TrezorMessage};
    use crate::transport::Transport;
    use async_trait::async_trait;
    use common::block_on;
    use futures::future::pending;
    use prost::Message;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[derive(Clone, Copy)]
    enum ReadBehavior {
        Features,
        Error,
        Pending,
    }

    #[derive(Clone, Default)]
    struct TransportCounters {
        writes: Arc<AtomicUsize>,
        reads: Arc<AtomicUsize>,
    }

    struct TestTransport {
        counters: TransportCounters,
        read_behavior: ReadBehavior,
    }

    impl TestTransport {
        fn new(read_behavior: ReadBehavior) -> (TestTransport, TransportCounters) {
            let counters = TransportCounters::default();
            let transport = TestTransport {
                counters: counters.clone(),
                read_behavior,
            };
            (transport, counters)
        }
    }

    #[async_trait]
    impl Transport for TestTransport {
        async fn session_begin(&mut self) -> TrezorResult<()> { Ok(()) }

        async fn session_end(&mut self) -> TrezorResult<()> { Ok(()) }

        async fn write_message(&mut self, message: ProtoMessage) -> TrezorResult<()> {
            self.counters.writes.fetch_add(1, Ordering::SeqCst);
            assert_eq!(message.message_type(), proto_management::Initialize::message_type());
            Ok(())
        }

        async fn read_message(&mut self) -> TrezorResult<ProtoMessage> {
            self.counters.reads.fetch_add(1, Ordering::SeqCst);
            match self.read_behavior {
                ReadBehavior::Features => {
                    let mut payload = Vec::new();
                    Features::default().encode(&mut payload).unwrap();
                    Ok(ProtoMessage::new(MessageType::Features, payload))
                },
                ReadBehavior::Error => MmError::err(TrezorError::DeviceDisconnected),
                ReadBehavior::Pending => pending().await,
            }
        }
    }

    #[test]
    fn busy_session_reports_connected_without_waiting_or_probing() {
        let (transport, counters) = TestTransport::new(ReadBehavior::Pending);
        let client = TrezorClient::from_transport(transport);
        let _held_session = block_on(client.inner.lock());

        assert!(block_on(client.is_connected()));
        assert_eq!(counters.writes.load(Ordering::SeqCst), 0);
        assert_eq!(counters.reads.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn reachability_probe_reports_connected_on_initialize_success() {
        let (transport, counters) = TestTransport::new(ReadBehavior::Features);
        let client = TrezorClient::from_transport(transport);

        assert!(block_on(client.is_connected()));
        assert_eq!(counters.writes.load(Ordering::SeqCst), 1);
        assert_eq!(counters.reads.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn reachability_probe_reports_unreachable_on_initialize_failure() {
        let (transport, counters) = TestTransport::new(ReadBehavior::Error);
        let client = TrezorClient::from_transport(transport);

        assert!(!block_on(client.is_connected()));
        assert_eq!(counters.writes.load(Ordering::SeqCst), 1);
        assert_eq!(counters.reads.load(Ordering::SeqCst), 1);
    }
}
