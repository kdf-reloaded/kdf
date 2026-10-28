// Copyright 2020 Sigma Prime Pty Ltd.
//
// Permission is hereby granted, free of charge, to any person obtaining a
// copy of this software and associated documentation files (the "Software"),
// to deal in the Software without restriction, including without limitation
// the rights to use, copy, modify, merge, publish, distribute, sublicense,
// and/or sell copies of the Software, and to permit persons to whom the
// Software is furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
// OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
// FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

use crate::protocol::{GossipsubCodec, ProtocolConfig};
use crate::rpc_proto::proto;
use crate::send_queue::{DropLog, SendQueue, SendQueueConfig};
use crate::types::{PeerKind, RawMessage, Rpc};
use crate::ValidationError;
use asynchronous_codec::Framed;
use futures::future::Either;
use futures::prelude::*;
use futures::StreamExt;
use futures_ticker::Ticker;
use instant::Instant;
use libp2p_core::upgrade::DeniedUpgrade;
use libp2p_swarm::handler::{
    ConnectionEvent, ConnectionHandler, ConnectionHandlerEvent, DialUpgradeError,
    FullyNegotiatedInbound, FullyNegotiatedOutbound, KeepAlive, StreamUpgradeError,
    SubstreamProtocol,
};
use libp2p_identity::PeerId;
use libp2p_swarm::Stream;
use std::{
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use void::Void;

/// The event emitted by the Handler. This informs the behaviour of various events created
/// by the handler.
#[derive(Debug)]
pub enum HandlerEvent {
    /// A GossipsubRPC message has been received. This also contains a list of invalid messages (if
    /// any) that were received.
    Message {
        /// The GossipsubRPC message excluding any invalid messages.
        rpc: Rpc,
        /// Any invalid messages that were received in the RPC, along with the associated
        /// validation error.
        invalid_messages: Vec<(RawMessage, ValidationError)>,
    },
    /// An inbound or outbound substream has been established with the peer and this informs over
    /// which protocol. This message only occurs once per connection.
    PeerKind(PeerKind),
    /// The send queue toward the peer has stayed at its cap for the whole stuck period. The
    /// behaviour closes the connection.
    SendQueueStuck,
}

/// A message sent from the behaviour to the handler.
#[derive(Debug)]
pub enum HandlerIn {
    /// A gossipsub message to send.
    Message(proto::RPC),
    /// The peer has joined the mesh.
    JoinedMesh,
    /// The peer has left the mesh.
    LeftMesh,
}

/// The maximum number of inbound or outbound substreams attempts we allow.
///
/// Gossipsub is supposed to have a single long-lived inbound and outbound substream. On failure we
/// attempt to recreate these. This imposes an upper bound of new substreams before we consider the
/// connection faulty and disable the handler. This also prevents against potential substream
/// creation loops.
const MAX_SUBSTREAM_ATTEMPTS: usize = 5;

#[allow(clippy::large_enum_variant)]
pub enum Handler {
    Enabled(EnabledHandler),
    Disabled(DisabledHandler),
}

/// Protocol Handler that manages a single long-lived substream with a peer.
pub struct EnabledHandler {
    /// Upgrade configuration for the gossipsub protocol.
    listen_protocol: ProtocolConfig,

    /// The single long-lived outbound substream.
    outbound_substream: Option<OutboundSubstreamState>,

    /// The single long-lived inbound substream.
    inbound_substream: Option<InboundSubstreamState>,

    /// The peer this connection belongs to, for log lines.
    peer_id: PeerId,

    /// Queue of values that we want to send to the remote. First in, first out; the data RPCs in
    /// it are capped (CRD ch.28 §28.9B RQ1).
    send_queue: SendQueue,

    /// Wakes the handler when the queue's stuck deadline passes.
    stuck_timer: Option<Ticker>,

    /// Flag indicating that an outbound substream is being established to prevent duplicate
    /// requests.
    outbound_substream_establishing: bool,

    /// The number of outbound substreams we have requested.
    outbound_substream_attempts: usize,

    /// The number of inbound substreams that have been created by the peer.
    inbound_substream_attempts: usize,

    /// The type of peer this handler is associated to.
    peer_kind: Option<PeerKind>,

    /// Keeps track on whether we have sent the peer kind to the behaviour.
    //
    // NOTE: Use this flag rather than checking the substream count each poll.
    peer_kind_sent: bool,

    last_io_activity: Instant,

    /// The amount of time we keep an idle connection alive.
    idle_timeout: Duration,

    /// Keeps track of whether this connection is for a peer in the mesh. This is used to make
    /// decisions about the keep alive state for this connection.
    in_mesh: bool,
}

pub enum DisabledHandler {
    /// If the peer doesn't support the gossipsub protocol we do not immediately disconnect.
    /// Rather, we disable the handler and prevent any incoming or outgoing substreams from being
    /// established.
    ProtocolUnsupported {
        /// Keeps track on whether we have sent the peer kind to the behaviour.
        peer_kind_sent: bool,
    },
    /// The maximum number of inbound or outbound substream attempts have happened and thereby the
    /// handler has been disabled.
    MaxSubstreamAttempts,
}

/// State of the inbound substream, opened either by us or by the remote.
enum InboundSubstreamState {
    /// Waiting for a message from the remote. The idle state for an inbound substream.
    WaitingInput(Framed<Stream, GossipsubCodec>),
    /// The substream is being closed.
    Closing(Framed<Stream, GossipsubCodec>),
    /// An error occurred during processing.
    Poisoned,
}

/// State of the outbound substream, opened either by us or by the remote.
enum OutboundSubstreamState {
    /// Waiting for the user to send a message. The idle state for an outbound substream.
    WaitingOutput(Framed<Stream, GossipsubCodec>),
    /// Waiting to send a message to the remote.
    PendingSend(Framed<Stream, GossipsubCodec>, proto::RPC),
    /// Waiting to flush the substream so that the data arrives to the remote.
    PendingFlush(Framed<Stream, GossipsubCodec>),
    /// An error occurred during processing.
    Poisoned,
}

impl Handler {
    /// Builds a new [`Handler`].
    pub fn new(
        protocol_config: ProtocolConfig,
        idle_timeout: Duration,
        peer_id: PeerId,
        send_queue_config: SendQueueConfig,
    ) -> Self {
        Handler::Enabled(EnabledHandler {
            listen_protocol: protocol_config,
            inbound_substream: None,
            outbound_substream: None,
            outbound_substream_establishing: false,
            outbound_substream_attempts: 0,
            inbound_substream_attempts: 0,
            peer_id,
            send_queue: SendQueue::new(send_queue_config),
            stuck_timer: None,
            peer_kind: None,
            peer_kind_sent: false,
            last_io_activity: Instant::now(),
            idle_timeout,
            in_mesh: false,
        })
    }
}

impl EnabledHandler {
    fn push_to_send_queue(&mut self, rpc: proto::RPC) {
        match self.send_queue.push(rpc, Instant::now()) {
            Some(DropLog::First {
                queued_messages,
                queued_bytes,
            }) => log::warn!(
                "Gossip send queue toward peer {} is full: dropping the oldest queued messages \
                 (queued {queued_messages} messages, {queued_bytes} bytes)",
                self.peer_id
            ),
            Some(DropLog::Summary { dropped }) => log::warn!(
                "Gossip send queue toward peer {} is still full: dropped {dropped} messages \
                 since the last report",
                self.peer_id
            ),
            None => {},
        }
    }

    /// Reports a connection that stayed at the send queue cap for the whole stuck period, and
    /// keeps a timer armed for the deadline otherwise.
    fn poll_send_queue_stuck(&mut self, cx: &mut Context<'_>) -> Option<HandlerEvent> {
        let Some(deadline) = self.send_queue.stuck_deadline() else {
            self.stuck_timer = None;
            return None;
        };
        let now = Instant::now();
        if now >= deadline {
            self.stuck_timer = None;
            let report = self.send_queue.take_stuck(now)?;
            log::warn!(
                "Closing the connection to peer {}: gossip send queue at its limit for {:?}, \
                 {} messages dropped",
                self.peer_id,
                report.at_cap,
                report.total_dropped
            );
            return Some(HandlerEvent::SendQueueStuck);
        }
        let timer = self
            .stuck_timer
            .get_or_insert_with(|| Ticker::new(deadline - now));
        // A tick only means the deadline is near or the period changed: drop the timer and
        // re-evaluate on the next poll.
        if timer.poll_next_unpin(cx).is_ready() {
            self.stuck_timer = None;
            cx.waker().wake_by_ref();
        }
        None
    }

    fn on_fully_negotiated_inbound(
        &mut self,
        (substream, peer_kind): (Framed<Stream, GossipsubCodec>, PeerKind),
    ) {
        // update the known kind of peer
        if self.peer_kind.is_none() {
            self.peer_kind = Some(peer_kind);
        }

        // new inbound substream. Replace the current one, if it exists.
        log::trace!("New inbound substream request");
        self.inbound_substream = Some(InboundSubstreamState::WaitingInput(substream));
    }

    fn on_fully_negotiated_outbound(
        &mut self,
        FullyNegotiatedOutbound { protocol, .. }: FullyNegotiatedOutbound<
            <Handler as ConnectionHandler>::OutboundProtocol,
            <Handler as ConnectionHandler>::OutboundOpenInfo,
        >,
    ) {
        let (substream, peer_kind) = protocol;

        // update the known kind of peer
        if self.peer_kind.is_none() {
            self.peer_kind = Some(peer_kind);
        }

        assert!(
            self.outbound_substream.is_none(),
            "Established an outbound substream with one already available"
        );
        self.outbound_substream = Some(OutboundSubstreamState::WaitingOutput(substream));
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<
        ConnectionHandlerEvent<
            <Handler as ConnectionHandler>::OutboundProtocol,
            <Handler as ConnectionHandler>::OutboundOpenInfo,
            <Handler as ConnectionHandler>::ToBehaviour,
            <Handler as ConnectionHandler>::Error,
        >,
    > {
        if let Some(event) = self.poll_send_queue_stuck(cx) {
            return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event));
        }

        if !self.peer_kind_sent {
            if let Some(peer_kind) = self.peer_kind.as_ref() {
                self.peer_kind_sent = true;
                return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(
                    HandlerEvent::PeerKind(peer_kind.clone()),
                ));
            }
        }

        // determine if we need to create the outbound stream
        if !self.send_queue.is_empty()
            && self.outbound_substream.is_none()
            && !self.outbound_substream_establishing
        {
            self.outbound_substream_establishing = true;
            return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                protocol: SubstreamProtocol::new(self.listen_protocol.clone(), ()),
            });
        }

        loop {
            match std::mem::replace(
                &mut self.inbound_substream,
                Some(InboundSubstreamState::Poisoned),
            ) {
                // inbound idle state
                Some(InboundSubstreamState::WaitingInput(mut substream)) => {
                    match substream.poll_next_unpin(cx) {
                        Poll::Ready(Some(Ok(message))) => {
                            self.last_io_activity = Instant::now();
                            self.inbound_substream =
                                Some(InboundSubstreamState::WaitingInput(substream));
                            return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(message));
                        }
                        Poll::Ready(Some(Err(error))) => {
                            log::debug!("Failed to read from inbound stream: {error}");
                            // Close this side of the stream. If the
                            // peer is still around, they will re-establish their
                            // outbound stream i.e. our inbound stream.
                            self.inbound_substream =
                                Some(InboundSubstreamState::Closing(substream));
                        }
                        // peer closed the stream
                        Poll::Ready(None) => {
                            log::debug!("Inbound stream closed by remote");
                            self.inbound_substream =
                                Some(InboundSubstreamState::Closing(substream));
                        }
                        Poll::Pending => {
                            self.inbound_substream =
                                Some(InboundSubstreamState::WaitingInput(substream));
                            break;
                        }
                    }
                }
                Some(InboundSubstreamState::Closing(mut substream)) => {
                    match Sink::poll_close(Pin::new(&mut substream), cx) {
                        Poll::Ready(res) => {
                            if let Err(e) = res {
                                // Don't close the connection but just drop the inbound substream.
                                // In case the remote has more to send, they will open up a new
                                // substream.
                                log::debug!("Inbound substream error while closing: {e}");
                            }
                            self.inbound_substream = None;
                            break;
                        }
                        Poll::Pending => {
                            self.inbound_substream =
                                Some(InboundSubstreamState::Closing(substream));
                            break;
                        }
                    }
                }
                None => {
                    self.inbound_substream = None;
                    break;
                }
                Some(InboundSubstreamState::Poisoned) => {
                    unreachable!("Error occurred during inbound stream processing")
                }
            }
        }

        // process outbound stream
        loop {
            match std::mem::replace(
                &mut self.outbound_substream,
                Some(OutboundSubstreamState::Poisoned),
            ) {
                // outbound idle state
                Some(OutboundSubstreamState::WaitingOutput(substream)) => {
                    if let Some(message) = self.send_queue.pop() {
                        self.outbound_substream =
                            Some(OutboundSubstreamState::PendingSend(substream, message));
                        continue;
                    }

                    self.outbound_substream =
                        Some(OutboundSubstreamState::WaitingOutput(substream));
                    break;
                }
                Some(OutboundSubstreamState::PendingSend(mut substream, message)) => {
                    match Sink::poll_ready(Pin::new(&mut substream), cx) {
                        Poll::Ready(Ok(())) => {
                            match Sink::start_send(Pin::new(&mut substream), message) {
                                Ok(()) => {
                                    self.outbound_substream =
                                        Some(OutboundSubstreamState::PendingFlush(substream))
                                }
                                Err(e) => {
                                    log::debug!("Failed to send message on outbound stream: {e}");
                                    self.outbound_substream = None;
                                    break;
                                }
                            }
                        }
                        Poll::Ready(Err(e)) => {
                            log::debug!("Failed to send message on outbound stream: {e}");
                            self.outbound_substream = None;
                            break;
                        }
                        Poll::Pending => {
                            self.outbound_substream =
                                Some(OutboundSubstreamState::PendingSend(substream, message));
                            break;
                        }
                    }
                }
                Some(OutboundSubstreamState::PendingFlush(mut substream)) => {
                    match Sink::poll_flush(Pin::new(&mut substream), cx) {
                        Poll::Ready(Ok(())) => {
                            self.last_io_activity = Instant::now();
                            self.outbound_substream =
                                Some(OutboundSubstreamState::WaitingOutput(substream))
                        }
                        Poll::Ready(Err(e)) => {
                            log::debug!("Failed to flush outbound stream: {e}");
                            self.outbound_substream = None;
                            break;
                        }
                        Poll::Pending => {
                            self.outbound_substream =
                                Some(OutboundSubstreamState::PendingFlush(substream));
                            break;
                        }
                    }
                }
                None => {
                    self.outbound_substream = None;
                    break;
                }
                Some(OutboundSubstreamState::Poisoned) => {
                    unreachable!("Error occurred during outbound stream processing")
                }
            }
        }

        Poll::Pending
    }
}

impl ConnectionHandler for Handler {
    type FromBehaviour = HandlerIn;
    type ToBehaviour = HandlerEvent;
    type Error = Void;
    type InboundOpenInfo = ();
    type InboundProtocol = either::Either<ProtocolConfig, DeniedUpgrade>;
    type OutboundOpenInfo = ();
    type OutboundProtocol = ProtocolConfig;

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        match self {
            Handler::Enabled(handler) => {
                SubstreamProtocol::new(either::Either::Left(handler.listen_protocol.clone()), ())
            }
            Handler::Disabled(_) => {
                SubstreamProtocol::new(either::Either::Right(DeniedUpgrade), ())
            }
        }
    }

    fn on_behaviour_event(&mut self, message: HandlerIn) {
        match self {
            Handler::Enabled(handler) => match message {
                HandlerIn::Message(m) => handler.push_to_send_queue(m),
                HandlerIn::JoinedMesh => {
                    handler.in_mesh = true;
                }
                HandlerIn::LeftMesh => {
                    handler.in_mesh = false;
                }
            },
            Handler::Disabled(_) => {
                log::debug!("Handler is disabled. Dropping message {:?}", message);
            }
        }
    }

    fn connection_keep_alive(&self) -> KeepAlive {
        match self {
            Handler::Enabled(handler) => {
                if handler.in_mesh {
                    return KeepAlive::Yes;
                }

                if let Some(
                    OutboundSubstreamState::PendingSend(_, _)
                    | OutboundSubstreamState::PendingFlush(_),
                ) = handler.outbound_substream
                {
                    return KeepAlive::Yes;
                }

                KeepAlive::Until(handler.last_io_activity + handler.idle_timeout)
            }
            Handler::Disabled(_) => KeepAlive::No,
        }
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<
        ConnectionHandlerEvent<
            Self::OutboundProtocol,
            Self::OutboundOpenInfo,
            Self::ToBehaviour,
            Self::Error,
        >,
    > {
        match self {
            Handler::Enabled(handler) => handler.poll(cx),
            Handler::Disabled(DisabledHandler::ProtocolUnsupported { peer_kind_sent }) => {
                if !*peer_kind_sent {
                    *peer_kind_sent = true;
                    return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(
                        HandlerEvent::PeerKind(PeerKind::NotSupported),
                    ));
                }

                Poll::Pending
            }
            Handler::Disabled(DisabledHandler::MaxSubstreamAttempts) => Poll::Pending,
        }
    }

    fn on_connection_event(
        &mut self,
        event: ConnectionEvent<
            Self::InboundProtocol,
            Self::OutboundProtocol,
            Self::InboundOpenInfo,
            Self::OutboundOpenInfo,
        >,
    ) {
        match self {
            Handler::Enabled(handler) => {
                if event.is_inbound() {
                    handler.inbound_substream_attempts += 1;

                    if handler.inbound_substream_attempts == MAX_SUBSTREAM_ATTEMPTS {
                        log::warn!(
                            "The maximum number of inbound substreams attempts has been exceeded"
                        );
                        *self = Handler::Disabled(DisabledHandler::MaxSubstreamAttempts);
                        return;
                    }
                }

                if event.is_outbound() {
                    handler.outbound_substream_establishing = false;

                    handler.outbound_substream_attempts += 1;

                    if handler.outbound_substream_attempts == MAX_SUBSTREAM_ATTEMPTS {
                        log::warn!(
                            "The maximum number of outbound substream attempts has been exceeded"
                        );
                        *self = Handler::Disabled(DisabledHandler::MaxSubstreamAttempts);
                        return;
                    }
                }

                match event {
                    ConnectionEvent::FullyNegotiatedInbound(FullyNegotiatedInbound {
                        protocol,
                        ..
                    }) => match protocol {
                        Either::Left(protocol) => handler.on_fully_negotiated_inbound(protocol),
                        Either::Right(v) => void::unreachable(v),
                    },
                    ConnectionEvent::FullyNegotiatedOutbound(fully_negotiated_outbound) => {
                        handler.on_fully_negotiated_outbound(fully_negotiated_outbound)
                    }
                    ConnectionEvent::DialUpgradeError(DialUpgradeError {
                        error: StreamUpgradeError::Timeout,
                        ..
                    }) => {
                        log::debug!("Dial upgrade error: Protocol negotiation timeout");
                    }
                    ConnectionEvent::DialUpgradeError(DialUpgradeError {
                        error: StreamUpgradeError::Apply(e),
                        ..
                    }) => void::unreachable(e),
                    ConnectionEvent::DialUpgradeError(DialUpgradeError {
                        error: StreamUpgradeError::NegotiationFailed,
                        ..
                    }) => {
                        // The protocol is not supported
                        log::debug!(
                            "The remote peer does not support gossipsub on this connection"
                        );
                        *self = Handler::Disabled(DisabledHandler::ProtocolUnsupported {
                            peer_kind_sent: false,
                        });
                    }
                    ConnectionEvent::DialUpgradeError(DialUpgradeError {
                        error: StreamUpgradeError::Io(e),
                        ..
                    }) => {
                        log::debug!("Protocol negotiation failed: {e}")
                    }
                    ConnectionEvent::AddressChange(_)
                    | ConnectionEvent::ListenUpgradeError(_)
                    | ConnectionEvent::LocalProtocolsChange(_)
                    | ConnectionEvent::RemoteProtocolsChange(_) => {}
                }
            }
            Handler::Disabled(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::task::noop_waker;

    fn data_rpc(tag: u8) -> proto::RPC {
        proto::RPC {
            subscriptions: vec![],
            publish: vec![proto::Message {
                from: None,
                data: Some(vec![tag; 10]),
                seqno: None,
                topic: "t".into(),
                signature: None,
                key: None,
            }],
            control: None,
        }
    }

    fn control_rpc() -> proto::RPC {
        proto::RPC {
            subscriptions: vec![proto::SubOpts {
                subscribe: Some(true),
                topic_id: Some("t".into()),
            }],
            publish: vec![],
            control: None,
        }
    }

    fn handler(max_messages: usize, stuck_timeout: Duration) -> Handler {
        Handler::new(
            ProtocolConfig::default(),
            Duration::from_secs(120),
            PeerId::random(),
            SendQueueConfig {
                max_messages,
                max_bytes: 1 << 20,
                stuck_timeout,
            },
        )
    }

    fn enabled(handler: &mut Handler) -> &mut EnabledHandler {
        match handler {
            Handler::Enabled(h) => h,
            Handler::Disabled(_) => panic!("handler is disabled"),
        }
    }

    #[test]
    fn behaviour_events_are_queued_in_order_and_capped() {
        let mut h = handler(3, Duration::from_secs(60));
        for tag in 0..5u8 {
            h.on_behaviour_event(HandlerIn::Message(data_rpc(tag)));
            if tag == 1 {
                h.on_behaviour_event(HandlerIn::Message(control_rpc()));
            }
        }
        let q = &mut enabled(&mut h).send_queue;
        assert_eq!(q.data_count(), 3);
        assert_eq!(q.total_dropped(), 2);
        let mut seen = vec![];
        while let Some(rpc) = q.pop() {
            seen.push(if rpc.publish.is_empty() {
                None
            } else {
                Some(rpc.publish[0].data.as_ref().unwrap()[0])
            });
        }
        // The control RPC survives; the two oldest data RPCs are gone.
        assert_eq!(seen, vec![None, Some(2), Some(3), Some(4)]);
    }

    #[test]
    fn stuck_connection_is_reported_to_the_behaviour_after_the_period() {
        let mut h = handler(2, Duration::from_millis(200));
        for tag in 0..3u8 {
            h.on_behaviour_event(HandlerIn::Message(data_rpc(tag)));
        }
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        let mut poll_events = |h: &mut Handler| {
            let mut events = vec![];
            while let Poll::Ready(ev) = ConnectionHandler::poll(h, &mut cx) {
                events.push(ev);
            }
            events
        };
        let is_stuck = |ev: &ConnectionHandlerEvent<_, _, HandlerEvent, Void>| {
            matches!(
                ev,
                ConnectionHandlerEvent::NotifyBehaviour(HandlerEvent::SendQueueStuck)
            )
        };
        // Before the period is over: only the outbound substream request.
        assert!(!poll_events(&mut h).iter().any(is_stuck));
        std::thread::sleep(Duration::from_millis(250));
        let events = poll_events(&mut h);
        assert_eq!(events.iter().filter(|ev| is_stuck(ev)).count(), 1);
        // Reported once per period.
        assert!(!poll_events(&mut h).iter().any(is_stuck));
    }

    #[test]
    fn draining_before_the_period_ends_prevents_the_report() {
        let mut h = handler(4, Duration::from_millis(100));
        for tag in 0..5u8 {
            h.on_behaviour_event(HandlerIn::Message(data_rpc(tag)));
        }
        let q = &mut enabled(&mut h).send_queue;
        while q.data_count() > 2 {
            q.pop();
        }
        std::thread::sleep(Duration::from_millis(150));
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        while let Poll::Ready(ev) = ConnectionHandler::poll(&mut h, &mut cx) {
            assert!(!matches!(
                ev,
                ConnectionHandlerEvent::NotifyBehaviour(HandlerEvent::SendQueueStuck)
            ));
        }
    }
}
