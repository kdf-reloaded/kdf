use libp2p::core::{Endpoint, Multiaddr};
use libp2p::ping;
use libp2p::swarm::{CloseConnection, ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, PollParameters,
                    THandler, THandlerInEvent, THandlerOutEvent, ToSwarm};
use libp2p::PeerId;
use log::error;
use std::collections::{HashMap, HashSet};
use std::task::{Context, Poll};
use void::Void;

/// After this many consecutive ping failures a peer is forcefully disconnected.
const MAX_CONSECUTIVE_FAILURES: u32 = 2;

/// Wrapper around the libp2p ping behaviour that forcefully disconnects a peer via
/// [`ToSwarm::CloseConnection`] once it accumulates [`MAX_CONSECUTIVE_FAILURES`] consecutive ping
/// failures.
///
/// The fork's `ping::Config` no longer exposes a `with_max_failures` knob, so the consecutive
/// failure count is tracked here. Libp2p has unclear `ConnectionHandler` keep-alive logic, so in
/// some cases even if the ping handler reports a failure the connection is kept active, which is
/// undesirable.
pub struct AdexPing {
    ping: ping::Behaviour,
    /// Consecutive ping failures per peer. Reset to zero on a successful ping.
    failed_counts: HashMap<PeerId, u32>,
    /// The established connections of each peer, so that the connections a safeguard close ends are known.
    connections: HashMap<PeerId, Vec<ConnectionId>>,
    /// The connections that the safeguard has started to close and that the swarm has not yet reported closed (CRD
    /// ch.28 R27a item 1a). [`AdexPing::take_ping_close`] removes an entry when the swarm reports the close.
    closing: HashSet<ConnectionId>,
}

#[allow(clippy::new_without_default)]
impl AdexPing {
    pub fn new() -> Self {
        AdexPing {
            ping: ping::Behaviour::new(ping::Config::new()),
            failed_counts: HashMap::new(),
            connections: HashMap::new(),
            closing: HashSet::new(),
        }
    }

    /// Whether the connection `connection_id`, which the swarm has just reported closed, was closed by the safeguard.
    /// Forgets the connection. The caller asks it for every closed connection, so no entry outlives its connection.
    pub(crate) fn take_ping_close(&mut self, connection_id: ConnectionId) -> bool {
        self.closing.remove(&connection_id)
    }
}

impl NetworkBehaviour for AdexPing {
    type ConnectionHandler = THandler<ping::Behaviour>;
    type ToSwarm = Void;

    fn handle_established_inbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        local_addr: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        self.ping
            .handle_established_inbound_connection(connection_id, peer, local_addr, remote_addr)
    }

    fn handle_established_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        addr: &Multiaddr,
        role_override: Endpoint,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        self.ping
            .handle_established_outbound_connection(connection_id, peer, addr, role_override)
    }

    fn handle_pending_inbound_connection(
        &mut self,
        connection_id: ConnectionId,
        local_addr: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<(), ConnectionDenied> {
        self.ping
            .handle_pending_inbound_connection(connection_id, local_addr, remote_addr)
    }

    fn handle_pending_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        maybe_peer: Option<PeerId>,
        addresses: &[Multiaddr],
        effective_role: Endpoint,
    ) -> Result<Vec<Multiaddr>, ConnectionDenied> {
        self.ping
            .handle_pending_outbound_connection(connection_id, maybe_peer, addresses, effective_role)
    }

    fn on_swarm_event(&mut self, event: FromSwarm<Self::ConnectionHandler>) {
        match &event {
            FromSwarm::ConnectionEstablished(established) => self
                .connections
                .entry(established.peer_id)
                .or_default()
                .push(established.connection_id),
            FromSwarm::ConnectionClosed(closed) => {
                let none_left = match self.connections.get_mut(&closed.peer_id) {
                    Some(connections) => {
                        connections.retain(|connection_id| *connection_id != closed.connection_id);
                        connections.is_empty()
                    },
                    None => false,
                };
                if none_left {
                    self.connections.remove(&closed.peer_id);
                }
            },
            _ => {},
        }
        self.ping.on_swarm_event(event)
    }

    fn on_connection_handler_event(
        &mut self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        self.ping.on_connection_handler_event(peer_id, connection_id, event)
    }

    fn poll(
        &mut self,
        cx: &mut Context,
        params: &mut impl PollParameters,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        loop {
            match self.ping.poll(cx, params) {
                Poll::Ready(ToSwarm::GenerateEvent(event)) => match event.result {
                    Ok(_) => {
                        // A successful ping clears the consecutive-failure counter for the peer.
                        self.failed_counts.remove(&event.peer);
                    },
                    // A peer that does not support the ping protocol is not penalised.
                    Err(ping::Failure::Unsupported) => {},
                    Err(e) => {
                        let count = self.failed_counts.entry(event.peer).or_insert(0);
                        *count += 1;
                        if *count >= MAX_CONSECUTIVE_FAILURES {
                            error!(
                                "Ping error {}. Disconnecting peer {} after {} consecutive failures",
                                e, event.peer, count
                            );
                            self.failed_counts.remove(&event.peer);
                            // Record the connections this close ends (CRD ch.28 R27a item 1a).
                            if let Some(connections) = self.connections.get(&event.peer) {
                                self.closing.extend(connections.iter().copied());
                            }
                            return Poll::Ready(ToSwarm::CloseConnection {
                                peer_id: event.peer,
                                connection: CloseConnection::All,
                            });
                        }
                    },
                },
                Poll::Ready(other) => return Poll::Ready(other.map_out(|_| unreachable!())),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}
