use crate::{decode_message, encode_message};
use async_trait::async_trait;
use core::iter;
use futures::channel::{mpsc, oneshot};
use futures::io::{AsyncRead, AsyncWrite};
use futures::task::{Context, Poll};
use futures::StreamExt;
use libp2p::core::upgrade::{read_length_prefixed, write_length_prefixed};
use libp2p::core::{Endpoint, Multiaddr};
use libp2p::request_response::{Behaviour as RequestResponse, Codec as RequestResponseCodec,
                               Config as RequestResponseConfig, Event as RequestResponseEvent, InboundFailure,
                               Message as RequestResponseMessage, OutboundFailure, ProtocolSupport, RequestId,
                               ResponseChannel};
use libp2p::swarm::{ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, PollParameters, THandler,
                    THandlerInEvent, THandlerOutEvent, ToSwarm};
use libp2p::PeerId;
use log::{debug, error, warn};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::io;
use std::time::Duration;
use wasm_timer::{Instant, Interval};

const MAX_BUFFER_SIZE: usize = 1024 * 1024 - 100;

pub type RequestResponseReceiver = mpsc::UnboundedReceiver<(PeerId, PeerRequest, oneshot::Sender<PeerResponse>)>;
pub type RequestResponseSender = mpsc::UnboundedSender<(PeerId, PeerRequest, oneshot::Sender<PeerResponse>)>;

/// Build a request-response network behaviour.
pub fn build_request_response_behaviour() -> RequestResponseBehaviour {
    let config = RequestResponseConfig::default();
    let protocol = iter::once((Protocol::Version2, ProtocolSupport::Full));
    let inner = RequestResponse::new(protocol, config);

    let (tx, rx) = mpsc::unbounded();
    let pending_requests = HashMap::new();
    let events = VecDeque::new();
    let timeout = Duration::from_secs(10);
    let timeout_interval = Interval::new(Duration::from_secs(1));

    RequestResponseBehaviour {
        inner,
        rx,
        tx,
        pending_requests,
        events,
        timeout,
        timeout_interval,
    }
}

#[derive(Debug)]
pub enum RequestResponseBehaviourEvent {
    InboundRequest {
        peer_id: PeerId,
        request: PeerRequest,
        response_channel: ResponseChannel<PeerResponse>,
    },
}

struct PendingRequest {
    tx: oneshot::Sender<PeerResponse>,
    initiated_at: Instant,
}

pub struct RequestResponseBehaviour {
    /// The inner RequestResponse network behaviour.
    inner: RequestResponse<Codec<Protocol, PeerRequest, PeerResponse>>,
    rx: RequestResponseReceiver,
    tx: RequestResponseSender,
    pending_requests: HashMap<RequestId, PendingRequest>,
    /// Events that need to be yielded to the outside when polling.
    events: VecDeque<RequestResponseBehaviourEvent>,
    /// Timeout for pending requests
    timeout: Duration,
    /// Interval for request timeout check
    timeout_interval: Interval,
}

impl RequestResponseBehaviour {
    pub fn sender(&self) -> RequestResponseSender { self.tx.clone() }

    pub fn send_response(&mut self, ch: ResponseChannel<PeerResponse>, rs: PeerResponse) -> Result<(), PeerResponse> {
        self.inner.send_response(ch, rs)
    }

    pub fn send_request(
        &mut self,
        peer_id: &PeerId,
        request: PeerRequest,
        response_tx: oneshot::Sender<PeerResponse>,
    ) -> RequestId {
        let request_id = self.inner.send_request(peer_id, request);
        let pending_request = PendingRequest {
            tx: response_tx,
            initiated_at: Instant::now(),
        };
        assert!(self.pending_requests.insert(request_id, pending_request).is_none());
        request_id
    }

    fn process_request(
        &mut self,
        peer_id: PeerId,
        request: PeerRequest,
        response_channel: ResponseChannel<PeerResponse>,
    ) {
        self.events.push_back(RequestResponseBehaviourEvent::InboundRequest {
            peer_id,
            request,
            response_channel,
        })
    }

    fn process_response(&mut self, request_id: RequestId, response: PeerResponse) {
        match self.pending_requests.remove(&request_id) {
            Some(pending) => {
                if let Err(e) = pending.tx.send(response) {
                    error!("{:?}. Request {:?} is not processed", e, request_id);
                }
            },
            _ => debug!("Ignoring response for no-longer-pending request {:?}", request_id),
        }
    }

    fn process_event(&mut self, event: RequestResponseEvent<PeerRequest, PeerResponse>) {
        let (peer_id, message) = match event {
            RequestResponseEvent::Message { peer, message } => (peer, message),
            RequestResponseEvent::InboundFailure { error, .. } => {
                match error {
                    InboundFailure::UnsupportedProtocols => {
                        debug!("Remote peer requested unsupported request-response protocol; keeping connection")
                    },
                    error => error!("Error on receive a request: {:?}", error),
                }
                return;
            },
            RequestResponseEvent::OutboundFailure {
                peer,
                request_id,
                error,
            } => {
                // The local timeout can expire before libp2p emits the terminal failure.
                // The caller has already been notified by the dropped oneshot sender in that case.
                if !self.pending_requests.contains_key(&request_id) {
                    debug!(
                        "Ignoring late outbound failure {:?} for no-longer-pending request {:?} to peer {:?}",
                        error, request_id, peer
                    );
                    return;
                }
                match &error {
                    OutboundFailure::UnsupportedProtocols => debug!(
                        "Peer {:?} does not support request-response protocol for request {:?}",
                        peer, request_id
                    ),
                    _ => error!("Error on send request {:?} to peer {:?}: {:?}", request_id, peer, error),
                }
                let err_response = PeerResponse::Err {
                    err: format!("{:?}", error),
                };
                self.process_response(request_id, err_response);
                return;
            },
            RequestResponseEvent::ResponseSent { .. } => return,
        };

        match message {
            RequestResponseMessage::Request { request, channel, .. } => {
                debug!("Received a request from {:?} peer", peer_id);
                self.process_request(peer_id, request, channel)
            },
            RequestResponseMessage::Response { request_id, response } => {
                debug!(
                    "Received a response to the {:?} request from peer {:?}",
                    request_id, peer_id
                );
                self.process_response(request_id, response)
            },
        }
    }
}

impl NetworkBehaviour for RequestResponseBehaviour {
    type ConnectionHandler = THandler<RequestResponse<Codec<Protocol, PeerRequest, PeerResponse>>>;
    type ToSwarm = RequestResponseBehaviourEvent;

    fn handle_established_inbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        local_addr: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        self.inner
            .handle_established_inbound_connection(connection_id, peer, local_addr, remote_addr)
    }

    fn handle_established_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        addr: &Multiaddr,
        role_override: Endpoint,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        self.inner
            .handle_established_outbound_connection(connection_id, peer, addr, role_override)
    }

    fn handle_pending_inbound_connection(
        &mut self,
        connection_id: ConnectionId,
        local_addr: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<(), ConnectionDenied> {
        self.inner
            .handle_pending_inbound_connection(connection_id, local_addr, remote_addr)
    }

    fn handle_pending_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        maybe_peer: Option<PeerId>,
        addresses: &[Multiaddr],
        effective_role: Endpoint,
    ) -> Result<Vec<Multiaddr>, ConnectionDenied> {
        self.inner
            .handle_pending_outbound_connection(connection_id, maybe_peer, addresses, effective_role)
    }

    fn on_swarm_event(&mut self, event: FromSwarm<Self::ConnectionHandler>) { self.inner.on_swarm_event(event) }

    fn on_connection_handler_event(
        &mut self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        self.inner.on_connection_handler_event(peer_id, connection_id, event)
    }

    fn poll(
        &mut self,
        cx: &mut Context,
        params: &mut impl PollParameters,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        // Poll the `rx` for locally-initiated requests to forward through the network.
        match self.rx.poll_next_unpin(cx) {
            Poll::Ready(Some((peer_id, request, response_tx))) => {
                let _request_id = self.send_request(&peer_id, request, response_tx);
            },
            Poll::Ready(None) => panic!("request-response channel has been closed"),
            Poll::Pending => (),
        }

        // Drive the inner request-response behaviour, processing its generated events locally
        // and forwarding all other swarm actions unchanged.
        loop {
            match self.inner.poll(cx, params) {
                Poll::Ready(ToSwarm::GenerateEvent(event)) => self.process_event(event),
                Poll::Ready(other) => return Poll::Ready(other.map_out(|_| unreachable!())),
                Poll::Pending => break,
            }
        }

        if let Some(event) = self.events.pop_front() {
            return Poll::Ready(ToSwarm::GenerateEvent(event));
        }

        while let Poll::Ready(Some(())) = self.timeout_interval.poll_next_unpin(cx) {
            let now = Instant::now();
            let timeout = self.timeout;
            self.pending_requests.retain(|request_id, pending_request| {
                let retain = now.duration_since(pending_request.initiated_at) < timeout;
                if !retain {
                    warn!("Request {} timed out", request_id);
                }
                retain
            });
        }

        Poll::Pending
    }
}

#[derive(Clone)]
pub struct Codec<Proto, Req, Res> {
    phantom: std::marker::PhantomData<(Proto, Req, Res)>,
}

impl<Proto, Req, Res> Default for Codec<Proto, Req, Res> {
    fn default() -> Self {
        Codec {
            phantom: Default::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Protocol {
    Version2,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PeerRequest {
    pub req: Vec<u8>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum PeerResponse {
    Ok { res: Vec<u8> },
    None,
    Err { err: String },
}

macro_rules! try_io {
    ($e: expr) => {
        match $e {
            Ok(ok) => ok,
            Err(err) => return Err(io::Error::new(io::ErrorKind::InvalidData, err)),
        }
    };
}

impl AsRef<str> for Protocol {
    fn as_ref(&self) -> &str {
        match self {
            Protocol::Version2 => "/request-response/2",
        }
    }
}

#[async_trait]
impl<
        Proto: Clone + AsRef<str> + Send + Sync,
        Req: DeserializeOwned + Serialize + Send + Sync,
        Res: DeserializeOwned + Serialize + Send + Sync,
    > RequestResponseCodec for Codec<Proto, Req, Res>
{
    type Protocol = Proto;
    type Request = Req;
    type Response = Res;

    async fn read_request<T>(&mut self, _protocol: &Self::Protocol, io: &mut T) -> io::Result<Self::Request>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_to_end(io).await
    }

    async fn read_response<T>(&mut self, _protocol: &Self::Protocol, io: &mut T) -> io::Result<Self::Response>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_to_end(io).await
    }

    async fn write_request<T>(&mut self, _protocol: &Self::Protocol, io: &mut T, req: Self::Request) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        write_all(io, &req).await
    }

    async fn write_response<T>(&mut self, _protocol: &Self::Protocol, io: &mut T, res: Self::Response) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        write_all(io, &res).await
    }
}

async fn read_to_end<T, M>(io: &mut T) -> io::Result<M>
where
    T: AsyncRead + Unpin + Send,
    M: DeserializeOwned,
{
    match read_length_prefixed(io, MAX_BUFFER_SIZE).await {
        Ok(data) => Ok(try_io!(decode_message(&data))),
        Err(e) => Err(io::Error::new(io::ErrorKind::InvalidData, e)),
    }
}

async fn write_all<T, M>(io: &mut T, msg: &M) -> io::Result<()>
where
    T: AsyncWrite + Unpin + Send,
    M: Serialize,
{
    let data = try_io!(encode_message(msg));
    if data.len() > MAX_BUFFER_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Try to send data size over maximum",
        ));
    }
    write_length_prefixed(io, data).await
}

#[cfg(test)]
mod tests {
    use super::Protocol;

    #[test]
    fn protocol_name_is_version2() {
        assert_eq!(Protocol::Version2.as_ref(), "/request-response/2");
    }
}
