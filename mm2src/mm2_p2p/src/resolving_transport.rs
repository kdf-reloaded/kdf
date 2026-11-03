//! The hostname-resolving layer of the native peer-to-peer transport (CRD ch.28 R0b and R18, ch.45 R45.6B.9 item 1).
//!
//! It stands where the transport library's own DNS layer stood, around the TCP transport, under the WebSocket layer
//! (which therefore keeps the hostname of a `/wss` address for its TLS check). A dial of `/dns/<name>`, `/dns4/<name>` or
//! `/dns6/<name>` resolves the name through the node's resolver (`common::dns_resolver`), replaces that first component
//! with `/ip4` or `/ip6` and dials the resolved addresses in turn until one connects. The rest of the address is dialed
//! unchanged. Everything else is delegated to the inner transport.
//!
//! A `/dnsaddr` address is not supported: the configured seeds never produce one (ch.28 R21) and peer exchange skips it
//! (R8c), so its dial fails at once with an error that says so.

use common::dns_resolver::{lookup, DnsFamily};
use futures::future::{self, BoxFuture};
use futures::prelude::*;
use libp2p::core::transport::{ListenerId, TransportError, TransportEvent};
use libp2p::core::{Multiaddr, Transport};
use libp2p::multiaddr::Protocol;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};
use std::{error, fmt, io};

/// A transport that resolves the DNS names of the addresses it dials with the node's resolver, then delegates to `T`.
pub struct ResolvingTransport<T> {
    inner: Arc<Mutex<T>>,
}

impl<T> ResolvingTransport<T> {
    pub fn new(inner: T) -> Self {
        ResolvingTransport {
            inner: Arc::new(Mutex::new(inner)),
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for ResolvingTransport<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ResolvingTransport").field(&self.inner).finish()
    }
}

fn lock<T>(inner: &Mutex<T>) -> MutexGuard<'_, T> { inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) }

/// The errors of a [`ResolvingTransport`].
#[derive(Debug)]
pub enum ResolvingError<E> {
    /// The inner transport failed.
    Transport(E),
    /// The name could not be resolved.
    Resolve(io::Error),
    /// A `/dnsaddr` address: not supported (CRD ch.45 R45.6B.9 item 1).
    DnsaddrNotSupported(Multiaddr),
    /// The inner transport does not support the resolved address.
    Unsupported(Multiaddr),
    /// The name resolved to no address.
    NoAddress,
}

impl<E: fmt::Display> fmt::Display for ResolvingError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolvingError::Transport(error) => write!(f, "{}", error),
            ResolvingError::Resolve(error) => write!(f, "could not resolve the name: {}", error),
            ResolvingError::DnsaddrNotSupported(address) => {
                write!(f, "/dnsaddr addresses are not supported: {}", address)
            },
            ResolvingError::Unsupported(address) => write!(f, "unsupported resolved address: {}", address),
            ResolvingError::NoAddress => write!(f, "the name resolved to no address"),
        }
    }
}

impl<E: error::Error + 'static> error::Error for ResolvingError<E> {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match self {
            ResolvingError::Transport(error) => Some(error),
            ResolvingError::Resolve(error) => Some(error),
            _ => None,
        }
    }
}

/// What the first components of a dialed address ask for.
enum Dial {
    /// `/dns*/<name>/tcp/<port>` with at most a trailing `/p2p/<id>`: resolve, then dial.
    Resolve { name: String, family: DnsFamily, port: u16 },
    /// `/dnsaddr/...`.
    Dnsaddr,
    /// A DNS component the inner TCP transport would reject after the lookup: refused up front, so that a transport
    /// composed with `or_transport` can try its other half (the WebSocket half takes `/ws` and `/wss`).
    NotSupported,
    /// No DNS name: the inner transport takes the address as it is.
    Plain,
}

fn classify_dial(address: &Multiaddr) -> Dial {
    let mut components = address.iter();
    let (name, family) = match components.next() {
        Some(Protocol::Dns(name)) => (name.to_string(), DnsFamily::Any),
        Some(Protocol::Dns4(name)) => (name.to_string(), DnsFamily::V4),
        Some(Protocol::Dns6(name)) => (name.to_string(), DnsFamily::V6),
        Some(Protocol::Dnsaddr(_)) => return Dial::Dnsaddr,
        _ => return Dial::Plain,
    };
    let port = match components.next() {
        Some(Protocol::Tcp(port)) => port,
        _ => return Dial::NotSupported,
    };
    match (components.next(), components.next()) {
        (None, _) | (Some(Protocol::P2p(_)), None) => Dial::Resolve { name, family, port },
        _ => Dial::NotSupported,
    }
}

impl<T> Transport for ResolvingTransport<T>
where
    T: Transport + Send + Unpin + 'static,
    T::Error: Send,
    T::Dial: Send,
{
    type Output = T::Output;
    type Error = ResolvingError<T::Error>;
    type ListenerUpgrade = future::MapErr<T::ListenerUpgrade, fn(T::Error) -> Self::Error>;
    type Dial = BoxFuture<'static, Result<Self::Output, Self::Error>>;

    fn listen_on(&mut self, id: ListenerId, addr: Multiaddr) -> Result<(), TransportError<Self::Error>> {
        lock(&self.inner)
            .listen_on(id, addr)
            .map_err(|e| e.map(ResolvingError::Transport))
    }

    fn remove_listener(&mut self, id: ListenerId) -> bool { lock(&self.inner).remove_listener(id) }

    fn dial(&mut self, addr: Multiaddr) -> Result<Self::Dial, TransportError<Self::Error>> { self.do_dial(addr, false) }

    fn dial_as_listener(&mut self, addr: Multiaddr) -> Result<Self::Dial, TransportError<Self::Error>> {
        self.do_dial(addr, true)
    }

    fn address_translation(&self, listen: &Multiaddr, observed: &Multiaddr) -> Option<Multiaddr> {
        lock(&self.inner).address_translation(listen, observed)
    }

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<TransportEvent<Self::ListenerUpgrade, Self::Error>> {
        let mut inner = lock(&self.inner);
        Transport::poll(Pin::new(&mut *inner), cx).map(|event| {
            event
                .map_upgrade(|upgrade| upgrade.map_err::<_, fn(_) -> _>(ResolvingError::Transport))
                .map_err(ResolvingError::Transport)
        })
    }
}

impl<T> ResolvingTransport<T>
where
    T: Transport + Send + Unpin + 'static,
    T::Error: Send,
    T::Dial: Send,
{
    fn do_dial(
        &mut self,
        addr: Multiaddr,
        as_listener: bool,
    ) -> Result<<Self as Transport>::Dial, TransportError<<Self as Transport>::Error>> {
        let inner = self.inner.clone();
        let dial_inner = move |address: Multiaddr| {
            let mut transport = lock(&inner);
            if as_listener {
                transport.dial_as_listener(address)
            } else {
                transport.dial(address)
            }
        };
        match classify_dial(&addr) {
            Dial::Dnsaddr => Err(TransportError::Other(ResolvingError::DnsaddrNotSupported(addr))),
            Dial::NotSupported => Err(TransportError::MultiaddrNotSupported(addr)),
            Dial::Plain => match dial_inner(addr) {
                Ok(dialing) => Ok(dialing.map_err(ResolvingError::Transport).boxed()),
                Err(error) => Err(error.map(ResolvingError::Transport)),
            },
            Dial::Resolve { name, family, port } => Ok(async move {
                let addresses = lookup(&name, port, family).await.map_err(ResolvingError::Resolve)?;
                let mut last_error = None;
                for socket in addresses {
                    let resolved = match socket.ip() {
                        IpAddr::V4(ip) => addr.replace(0, |_| Some(Protocol::Ip4(ip))),
                        IpAddr::V6(ip) => addr.replace(0, |_| Some(Protocol::Ip6(ip))),
                    };
                    let resolved = match resolved {
                        Some(resolved) => resolved,
                        None => continue,
                    };
                    let outcome = match dial_inner(resolved) {
                        Ok(dialing) => dialing.await.map_err(ResolvingError::Transport),
                        Err(TransportError::MultiaddrNotSupported(address)) => {
                            Err(ResolvingError::Unsupported(address))
                        },
                        Err(TransportError::Other(error)) => Err(ResolvingError::Transport(error)),
                    };
                    match outcome {
                        Ok(output) => return Ok(output),
                        Err(error) => {
                            log::debug!("Dialing {} failed: {}", socket, error);
                            last_error = Some(error);
                        },
                    }
                }
                Err(last_error.unwrap_or(ResolvingError::NoAddress))
            }
            .boxed()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::dns_resolver::install_test_map;
    use libp2p::tcp;
    use libp2p::websocket::{tls, WsConfig};
    use libp2p::PeerId;
    use std::net::{Ipv4Addr, TcpListener};

    type Tcp = tcp::tokio::Transport;

    fn tcp_transport() -> Tcp { Tcp::new(tcp::Config::new().nodelay(true)) }

    fn runtime() -> tokio::runtime::Runtime { tokio::runtime::Runtime::new().unwrap() }

    fn address(text: &str) -> Multiaddr { text.parse().unwrap() }

    fn loopback(octet: u8) -> IpAddr { IpAddr::V4(Ipv4Addr::new(127, 0, 0, octet)) }

    fn dial(
        runtime: &tokio::runtime::Runtime,
        transport: &mut ResolvingTransport<Tcp>,
        text: &str,
    ) -> Result<tcp::tokio::TcpStream, String> {
        let _enter = runtime.enter();
        let dialing = transport.dial(address(text)).map_err(|error| format!("{:?}", error))?;
        runtime.block_on(dialing).map_err(|error| error.to_string())
    }

    #[test]
    fn a_dns4_dial_reaches_the_address_the_name_resolves_to() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let _guard = install_test_map(&[("seed.test", &[loopback(1)])]);
        let rt = runtime();
        let mut transport = ResolvingTransport::new(tcp_transport());
        let stream = dial(&rt, &mut transport, &format!("/dns4/seed.test/tcp/{}", port)).unwrap();
        let (accepted, from) = listener.accept().unwrap();
        drop(accepted);
        assert_eq!(from.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
        drop(stream);
        assert_eq!(common::dns_resolver::gauges().unwrap().lookups, 1);
    }

    #[test]
    fn a_dns_dial_with_a_peer_id_keeps_the_rest_of_the_address() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let _guard = install_test_map(&[("peer.test", &[loopback(1)])]);
        let rt = runtime();
        let mut transport = ResolvingTransport::new(tcp_transport());
        let text = format!("/dns/peer.test/tcp/{}/p2p/{}", port, PeerId::random());
        dial(&rt, &mut transport, &text).unwrap();
        listener.accept().unwrap();
    }

    #[test]
    fn with_two_addresses_and_the_first_unreachable_the_dial_connects_to_the_second() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        // Nothing listens on 127.0.0.2: the connection is refused, and the second address is tried.
        let _guard = install_test_map(&[("two.test", &[loopback(2), loopback(1)])]);
        let rt = runtime();
        let mut transport = ResolvingTransport::new(tcp_transport());
        let stream = dial(&rt, &mut transport, &format!("/dns4/two.test/tcp/{}", port)).unwrap();
        let (_accepted, from) = listener.accept().unwrap();
        assert_eq!(from.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
        drop(stream);
    }

    #[test]
    fn a_name_of_the_other_family_or_an_unknown_name_fails_the_dial() {
        let _guard = install_test_map(&[("v4only.test", &[loopback(1)])]);
        let rt = runtime();
        let mut transport = ResolvingTransport::new(tcp_transport());
        let error = dial(&rt, &mut transport, "/dns6/v4only.test/tcp/9").unwrap_err();
        assert!(error.contains("could not resolve"), "{}", error);
    }

    #[test]
    fn a_dnsaddr_dial_fails_at_once_and_says_it_is_not_supported() {
        let mut transport = ResolvingTransport::new(tcp_transport());
        match transport.dial(address("/dnsaddr/seed.test/tcp/9")) {
            Err(TransportError::Other(error)) => {
                assert!(matches!(error, ResolvingError::DnsaddrNotSupported(_)));
                assert!(error.to_string().contains("not supported"), "{}", error);
            },
            Err(other) => panic!("unexpected error: {:?}", other),
            Ok(_) => panic!("the dial of a /dnsaddr address must fail"),
        }
    }

    #[test]
    fn an_address_the_tcp_transport_cannot_take_is_refused_up_front() {
        let mut transport = ResolvingTransport::new(tcp_transport());
        for text in [
            "/dns4/seed.test/tcp/9/ws",
            "/dns4/seed.test/tcp/9/wss",
            "/dns4/seed.test/udp/9",
            "/dns4/seed.test",
        ] {
            assert!(
                matches!(
                    transport.dial(address(text)),
                    Err(TransportError::MultiaddrNotSupported(_))
                ),
                "{}",
                text
            );
        }
    }

    #[test]
    fn an_address_without_a_name_is_dialed_as_it_is_and_makes_no_lookup() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let _guard = install_test_map(&[]);
        let rt = runtime();
        let mut transport = ResolvingTransport::new(tcp_transport());
        dial(&rt, &mut transport, &format!("/ip4/127.0.0.1/tcp/{}", port)).unwrap();
        listener.accept().unwrap();
        assert_eq!(common::dns_resolver::gauges().unwrap().lookups, 0);
    }

    // The certificate and the key are the throwaway ones of the vendored WebSocket crate's handshake tests: a
    // certificate for the name `localhost` (subject alternative name `DNS:localhost`).
    const CERT: &[u8] = include_bytes!("../../../vendor-patches/libp2p-websocket-0.42.0/tests/fixtures/rsa_cert.der");
    const KEY: &[u8] =
        include_bytes!("../../../vendor-patches/libp2p-websocket-0.42.0/tests/fixtures/rsa_pkcs1_key.der");

    /// A WSS server on the loopback address, and a client that dials it as `/dns4/<name>/tcp/<port>/wss`, with the
    /// hostname resolved by the test resolver. Returns the outcome of the server's and the client's handshake.
    fn wss_dial(name: &str) -> (io::Result<()>, io::Result<()>) {
        let server_tls = tls::Config::new(tls::PrivateKey::new(KEY.to_vec()).unwrap(), vec![
            tls::Certificate::new(CERT.to_vec()),
        ])
        .unwrap();
        let mut client_tls = tls::Config::builder();
        client_tls.add_trust(&tls::Certificate::new(CERT.to_vec())).unwrap();
        let client_tls = client_tls.finish();

        let name = name.to_owned();
        runtime().block_on(async move {
            let mut server = WsConfig::new(tcp_transport());
            server.set_tls_config(server_tls);
            let mut server = server.boxed();
            server
                .listen_on(ListenerId::next(), address("/ip4/127.0.0.1/tcp/0/wss"))
                .unwrap();
            let listen_address = server.next().await.unwrap().into_new_address().unwrap();
            let port = match listen_address.iter().nth(1) {
                Some(Protocol::Tcp(port)) => port,
                other => panic!("unexpected listen address component {:?}", other),
            };

            let inbound = async move {
                let (connection, _) = server
                    .select_next_some()
                    .map(|event| event.into_incoming())
                    .await
                    .unwrap();
                let mut connection = connection.await.map_err(|e| io::Error::other(e.to_string()))?;
                let mut buffer = [0u8; 4];
                connection.read_exact(&mut buffer).await?;
                assert_eq!(&buffer, b"ping");
                Ok::<(), io::Error>(())
            };
            let outbound = async move {
                let mut client = WsConfig::new(ResolvingTransport::new(tcp_transport()));
                client.set_tls_config(client_tls);
                let dial_address =
                    address(&format!("/dns4/{}/tcp/{}/wss", name, port)).with(Protocol::P2p(PeerId::random()));
                let mut connection = client
                    .dial(dial_address)
                    .map_err(|e| io::Error::other(format!("{:?}", e)))?
                    .await
                    .map_err(|e| io::Error::other(e.to_string()))?;
                connection.write_all(b"ping").await?;
                connection.flush().await?;
                Ok::<(), io::Error>(())
            };
            // The server side of a failed handshake never completes: bound it.
            let inbound = async move {
                match future::select(
                    Box::pin(inbound),
                    wasm_timer::Delay::new(std::time::Duration::from_secs(5)),
                )
                .await
                {
                    future::Either::Left((result, _)) => result,
                    future::Either::Right(_) => Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "the server saw no complete handshake",
                    )),
                }
            };
            futures::join!(inbound, outbound)
        })
    }

    #[test]
    fn a_wss_dial_to_a_hostname_checks_the_certificate_against_that_hostname() {
        let _guard = install_test_map(&[("localhost", &[loopback(1)])]);
        let (server, client) = wss_dial("localhost");
        server.unwrap();
        client.unwrap();
        assert_eq!(common::dns_resolver::gauges().unwrap().lookups, 1);
    }

    #[test]
    fn a_wss_dial_fails_when_the_certificate_is_not_for_the_hostname_even_if_the_address_is_the_same() {
        // `elsewhere.test` resolves to the same loopback server, whose certificate is for `localhost` only.
        let _guard = install_test_map(&[("elsewhere.test", &[loopback(1)])]);
        let (_server, client) = wss_dial("elsewhere.test");
        assert!(client.is_err(), "the TLS check must use the name, not the address");
    }

    #[test]
    fn the_plain_and_the_websocket_halves_compose_in_the_order_of_the_node() {
        // `dns_tcp.or_transport(ws_dns_tcp)` as built in `atomicdex_behaviour`: a /wss address must reach the WebSocket
        // half, which only works because the plain half refuses it up front.
        let mut transport = ResolvingTransport::new(tcp_transport())
            .or_transport(WsConfig::new(ResolvingTransport::new(tcp_transport())));
        let _guard = install_test_map(&[("seed.test", &[loopback(1)])]);
        // The plain half would have taken the address and failed later. Reaching the WebSocket half is shown by the
        // dial being accepted synchronously: only that half understands `/wss`.
        assert!(transport.dial(address("/dns4/seed.test/tcp/9/wss")).is_ok());
    }
}
