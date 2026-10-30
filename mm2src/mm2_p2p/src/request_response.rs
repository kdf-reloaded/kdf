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
/// The window in which failures of one kind toward one peer are counted instead of logged one by one.
const FAILURE_REPORT_WINDOW: Duration = Duration::from_secs(60);

/// One peer's window of outbound request failures of one kind: the first failure was reported when the window
/// opened, the later ones are only counted.
struct FailureWindow {
    started: Instant,
    more: usize,
}

/// Rate limit of the log lines of one kind of outbound request failure: the first failure toward a peer in a
/// [`FAILURE_REPORT_WINDOW`] is logged at once, the later ones are counted and summarized when the window ends.
/// One instance is used per failure kind.
#[derive(Default)]
struct FailureReports {
    windows: HashMap<PeerId, FailureWindow>,
    ended: Vec<(PeerId, usize)>,
}

impl FailureReports {
    /// Records a failure toward `peer` at `now`. Returns `true` when it is the first one in a window, i.e. when
    /// the caller should log it.
    fn record(&mut self, peer: PeerId, now: Instant) -> bool {
        match self.windows.get_mut(&peer) {
            Some(window) if now.duration_since(window.started) < FAILURE_REPORT_WINDOW => {
                window.more += 1;
                false
            },
            Some(window) => {
                if window.more > 0 {
                    self.ended.push((peer, window.more));
                }
                *window = FailureWindow { started: now, more: 0 };
                true
            },
            None => {
                self.windows.insert(peer, FailureWindow { started: now, more: 0 });
                true
            },
        }
    }

    /// Closes the windows that ended by `now` and returns the number of later failures of every peer whose
    /// window had any. A window without later failures yields nothing.
    fn flush(&mut self, now: Instant) -> Vec<(PeerId, usize)> {
        let ended = &mut self.ended;
        self.windows.retain(|peer, window| {
            if now.duration_since(window.started) < FAILURE_REPORT_WINDOW {
                return true;
            }
            if window.more > 0 {
                ended.push((*peer, window.more));
            }
            false
        });
        std::mem::take(&mut self.ended)
    }
}

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
        timeout_reports: FailureReports::default(),
        closed_reports: FailureReports::default(),
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
    peer: PeerId,
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
    /// Rate limit of the timeout log lines.
    timeout_reports: FailureReports,
    /// Rate limit of the log lines of requests that failed because the connection closed.
    closed_reports: FailureReports,
}

/// The kind of a failure of an outbound request: a timeout, or any other transport failure.
fn outbound_failure_kind(error: &OutboundFailure) -> LocalFailure {
    match error {
        OutboundFailure::Timeout => LocalFailure::Timeout,
        _ => LocalFailure::Transport,
    }
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
            peer: *peer_id,
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
                // The requester is gone (another peer answered first, or it timed out), so nothing is lost. The
                // response itself is not logged.
                if pending.tx.send(response).is_err() {
                    debug!("Request {:?} is not processed: the requester is gone", request_id);
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
                    // A slow remote peer is not a local error: the first timeout toward a peer in a window is
                    // logged at WARN, the later ones are counted and summarized when the window ends.
                    OutboundFailure::Timeout => {
                        if self.timeout_reports.record(peer, Instant::now()) {
                            warn!("Error on send request {:?} to peer {:?}: {:?}", request_id, peer, error);
                        }
                    },
                    // The connection closed under the request. A closing connection fails all its requests at once,
                    // so they are aggregated per peer in the same way as the timeouts.
                    OutboundFailure::ConnectionClosed => {
                        if self.closed_reports.record(peer, Instant::now()) {
                            warn!("Error on send request {:?} to peer {:?}: {:?}", request_id, peer, error);
                        }
                    },
                    _ => error!("Error on send request {:?} to peer {:?}: {:?}", request_id, peer, error),
                }
                let err_response = PeerResponse::LocalErr {
                    kind: outbound_failure_kind(&error),
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
            let timeout_reports = &mut self.timeout_reports;
            self.pending_requests.retain(|request_id, pending_request| {
                let retain = now.duration_since(pending_request.initiated_at) < timeout;
                // The local timeout counts as a timeout toward the peer as well.
                if !retain && timeout_reports.record(pending_request.peer, now) {
                    warn!("Request {} to peer {} timed out", request_id, pending_request.peer);
                }
                retain
            });
            for (peer, more) in self.timeout_reports.flush(now) {
                warn!("{} more requests to peer {} timed out in the last 60 s", more, peer);
            }
            for (peer, more) in self.closed_reports.flush(now) {
                warn!(
                    "{} more requests to peer {} failed because the connection closed, in the last 60 s",
                    more, peer
                );
            }
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

/// Why a request of this node failed without an answer from the peer. It is known only locally and never
/// crosses the wire.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalFailure {
    /// The request-response timeout of libp2p or the local pending-request timeout expired.
    Timeout,
    /// Any other failure to send the request or to receive an answer: a closed connection, a failed dial,
    /// an unsupported protocol, an I/O error, a full command channel.
    Transport,
    /// The answer arrived but this node could not decode it. It is created by the application layer, which
    /// owns the decoding; this crate never produces it.
    Decode,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum PeerResponse {
    Ok {
        res: Vec<u8>,
    },
    None,
    Err {
        err: String,
    },
    /// A failure of this node's own request, with its [`LocalFailure`] kind. The variant stays last so the
    /// indices of the wire variants above do not move, and it is skipped by serde: it is never written to a
    /// peer and never read from one.
    #[serde(skip)]
    LocalErr {
        kind: LocalFailure,
        err: String,
    },
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
    use super::{outbound_failure_kind, FailureReports, LocalFailure, OutboundFailure, PeerResponse, Protocol,
                FAILURE_REPORT_WINDOW};
    use crate::atomicdex_behaviour::AdexResponse;
    use crate::{decode_message, encode_message};
    use libp2p::PeerId;
    use serde::{Deserialize, Serialize};
    use std::time::Duration;
    use wasm_timer::Instant;

    #[test]
    fn protocol_name_is_version2() {
        assert_eq!(Protocol::Version2.as_ref(), "/request-response/2");
    }

    #[test]
    fn first_timeout_is_reported_later_ones_are_counted() {
        let mut reports = FailureReports::default();
        let peer = PeerId::random();
        let other = PeerId::random();
        let start = Instant::now();

        assert!(reports.record(peer, start));
        // Five more in the same second toward the same peer are only counted.
        for i in 1..=5 {
            assert!(!reports.record(peer, start + Duration::from_millis(i * 100)));
        }
        // Another peer has a window of its own.
        assert!(reports.record(other, start + Duration::from_secs(1)));

        // Nothing is summarized while the windows are open.
        assert!(reports.flush(start + Duration::from_secs(30)).is_empty());

        // The window of `peer` ends first; `other` had no later timeout, so it yields no summary.
        let summary = reports.flush(start + FAILURE_REPORT_WINDOW);
        assert_eq!(summary, vec![(peer, 5)]);
        assert!(reports.flush(start + Duration::from_secs(120)).is_empty());

        // A timeout after the window opens a new one and is reported at once.
        assert!(reports.record(peer, start + Duration::from_secs(130)));
    }

    #[test]
    fn summary_appears_only_when_something_was_counted() {
        let mut reports = FailureReports::default();
        let peer = PeerId::random();
        let start = Instant::now();
        assert!(reports.record(peer, start));
        assert!(reports.flush(start + FAILURE_REPORT_WINDOW).is_empty());
    }

    /// The connection-closed failures use an instance of their own, so the two kinds do not share a window.
    #[test]
    fn failure_kinds_have_windows_of_their_own() {
        let mut timeouts = FailureReports::default();
        let mut closed = FailureReports::default();
        let peer = PeerId::random();
        let start = Instant::now();
        assert!(timeouts.record(peer, start));
        assert!(closed.record(peer, start));
        assert!(!closed.record(peer, start + Duration::from_secs(1)));
        assert!(!closed.record(peer, start + Duration::from_secs(2)));
        assert!(timeouts.flush(start + FAILURE_REPORT_WINDOW).is_empty());
        assert_eq!(closed.flush(start + FAILURE_REPORT_WINDOW), vec![(peer, 2)]);
    }

    #[test]
    fn timeout_after_the_window_ended_keeps_the_summary() {
        let mut reports = FailureReports::default();
        let peer = PeerId::random();
        let start = Instant::now();
        assert!(reports.record(peer, start));
        assert!(!reports.record(peer, start + Duration::from_secs(10)));
        assert!(!reports.record(peer, start + Duration::from_secs(20)));
        // The interval did not flush yet; the next timeout opens a new window and keeps the summary of the old.
        assert!(reports.record(peer, start + Duration::from_secs(61)));
        assert_eq!(reports.flush(start + Duration::from_secs(62)), vec![(peer, 2)]);
    }

    /// The wire form of `PeerResponse` before the local failure variant existed.
    #[derive(Deserialize, Serialize)]
    enum WirePeerResponse {
        Ok { res: Vec<u8> },
        None,
        Err { err: String },
    }

    #[test]
    fn the_wire_variants_are_encoded_as_before() {
        let pairs = [
            (
                encode_message(&PeerResponse::Ok { res: vec![1, 2] }),
                encode_message(&WirePeerResponse::Ok { res: vec![1, 2] }),
            ),
            (
                encode_message(&PeerResponse::None),
                encode_message(&WirePeerResponse::None),
            ),
            (
                encode_message(&PeerResponse::Err { err: "e".to_owned() }),
                encode_message(&WirePeerResponse::Err { err: "e".to_owned() }),
            ),
        ];
        for (now, before) in pairs {
            assert_eq!(now.unwrap(), before.unwrap());
        }
        // And a peer's bytes still decode.
        let bytes = encode_message(&WirePeerResponse::Err { err: "e".to_owned() }).unwrap();
        assert!(matches!(
            decode_message::<PeerResponse>(&bytes).unwrap(),
            PeerResponse::Err { err } if err == "e"
        ));
    }

    #[test]
    fn a_local_failure_is_never_written_to_a_peer_nor_read_from_one() {
        let local = PeerResponse::LocalErr {
            kind: LocalFailure::Timeout,
            err: "Timeout".to_owned(),
        };
        assert!(encode_message(&local).is_err());

        // The answer to a peer's request never carries a local failure: it is sent as a plain error.
        let adex = AdexResponse::LocalErr {
            kind: LocalFailure::Transport,
            error: "ConnectionClosed".to_owned(),
        };
        match PeerResponse::from(adex) {
            PeerResponse::Err { err } => assert_eq!(err, "ConnectionClosed"),
            other => panic!("unexpected {:?}", other),
        }
    }

    #[test]
    fn outbound_failures_are_a_timeout_or_a_transport_failure() {
        assert_eq!(outbound_failure_kind(&OutboundFailure::Timeout), LocalFailure::Timeout);
        for error in [
            OutboundFailure::DialFailure,
            OutboundFailure::ConnectionClosed,
            OutboundFailure::UnsupportedProtocols,
        ] {
            assert_eq!(outbound_failure_kind(&error), LocalFailure::Transport, "{:?}", error);
        }
    }

    #[test]
    fn a_local_failure_reaches_the_application_with_its_kind() {
        let local = PeerResponse::LocalErr {
            kind: LocalFailure::Timeout,
            err: "Timeout".to_owned(),
        };
        assert_eq!(AdexResponse::from(local), AdexResponse::LocalErr {
            kind: LocalFailure::Timeout,
            error: "Timeout".to_owned(),
        });
        // A peer's error stays a peer's error.
        assert_eq!(
            AdexResponse::from(PeerResponse::Err {
                err: "Timeout".to_owned()
            }),
            AdexResponse::Err {
                error: "Timeout".to_owned()
            }
        );
    }
}
