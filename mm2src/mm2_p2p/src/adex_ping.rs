use libp2p::core::{Endpoint, Multiaddr};
use libp2p::ping;
use libp2p::swarm::{CloseConnection, ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, PollParameters,
                    THandler, THandlerInEvent, THandlerOutEvent, ToSwarm};
use libp2p::PeerId;
use log::error;
use std::collections::HashMap;
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
    /// Measurement only (feature `lab-knobs`): round-trip pressure per connection.
    #[cfg(feature = "lab-knobs")]
    rtt_pressure: HashMap<ConnectionId, (PeerId, crate::lab_knobs::RttPressure)>,
    /// Measurement only: when the current run of consecutive ping failures of a peer began.
    #[cfg(feature = "lab-knobs")]
    first_failure: HashMap<PeerId, std::time::Instant>,
}

#[allow(clippy::new_without_default)]
impl AdexPing {
    pub fn new() -> Self {
        AdexPing {
            ping: ping::Behaviour::new(ping::Config::new()),
            failed_counts: HashMap::new(),
            #[cfg(feature = "lab-knobs")]
            rtt_pressure: HashMap::new(),
            #[cfg(feature = "lab-knobs")]
            first_failure: HashMap::new(),
        }
    }

    /// Measurement only: whether the ping round trip of any connection to one of `peers` is under pressure.
    #[cfg(feature = "lab-knobs")]
    pub fn rtt_pressure_among(&self, peers: &[PeerId]) -> bool {
        self.rtt_pressure
            .values()
            .any(|(peer, pressure)| pressure.is_pressured() && peers.contains(peer))
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
        #[cfg(feature = "lab-knobs")]
        if let FromSwarm::ConnectionClosed(closed) = &event {
            self.rtt_pressure.remove(&closed.connection_id);
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
                Poll::Ready(ToSwarm::GenerateEvent(event)) => {
                    #[cfg(feature = "lab-knobs")]
                    if !matches!(event.result, Err(ping::Failure::Unsupported)) {
                        crate::lab_knobs::record_ping(event.result.as_ref().ok().copied());
                    }
                    #[cfg(feature = "lab-knobs")]
                    match &event.result {
                        Ok(rtt) => self
                            .rtt_pressure
                            .entry(event.connection)
                            .or_insert_with(|| (event.peer, Default::default()))
                            .1
                            .sample(Some(*rtt)),
                        Err(ping::Failure::Unsupported) => {},
                        Err(_) => self
                            .rtt_pressure
                            .entry(event.connection)
                            .or_insert_with(|| (event.peer, Default::default()))
                            .1
                            .sample(None),
                    }
                    match event.result {
                        Ok(_) => {
                            // A successful ping clears the consecutive-failure counter for the peer.
                            self.failed_counts.remove(&event.peer);
                            #[cfg(feature = "lab-knobs")]
                            self.first_failure.remove(&event.peer);
                        },
                        // A peer that does not support the ping protocol is not penalised.
                        Err(ping::Failure::Unsupported) => {},
                        Err(e) => {
                            let count = self.failed_counts.entry(event.peer).or_insert(0);
                            *count += 1;
                            // Measurement only: while the outbox is degraded the connection is kept until pings have
                            // failed for a long time.
                            #[cfg(feature = "lab-knobs")]
                            let held = {
                                let since = *self
                                    .first_failure
                                    .entry(event.peer)
                                    .or_insert_with(std::time::Instant::now);
                                crate::lab_knobs::hold_ping_close_when_degraded()
                                    && crate::lab_knobs::degraded()
                                    && since.elapsed() <= crate::lab_knobs::HOLD_CLOSE_AFTER
                            };
                            #[cfg(not(feature = "lab-knobs"))]
                            let held = false;
                            if *count >= MAX_CONSECUTIVE_FAILURES && !held {
                                error!(
                                    "Ping error {}. Disconnecting peer {} after {} consecutive failures",
                                    e, event.peer, count
                                );
                                self.failed_counts.remove(&event.peer);
                                #[cfg(feature = "lab-knobs")]
                                self.first_failure.remove(&event.peer);
                                return Poll::Ready(ToSwarm::CloseConnection {
                                    peer_id: event.peer,
                                    connection: CloseConnection::All,
                                });
                            }
                        },
                    }
                },
                Poll::Ready(other) => return Poll::Ready(other.map_out(|_| unreachable!())),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}
