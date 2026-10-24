// VENDOR PATCH test: end-to-end wss handshakes through the rustls 0.23 TLS layer, on both the
// client and the server path. See PATCH-NOTES.md.

use futures::prelude::*;
use libp2p_core::{multiaddr::Protocol, transport::ListenerId, Multiaddr, Transport};
use libp2p_dns as dns;
use libp2p_identity::PeerId;
use libp2p_tcp as tcp;
use libp2p_websocket::{tls, WsConfig};

type Dns = dns::DnsConfig<tcp::async_io::Transport>;

async fn new_ws(tls_config: tls::Config) -> WsConfig<Dns> {
    let inner = dns::DnsConfig::system(tcp::async_io::Transport::new(tcp::Config::default()))
        .await
        .unwrap();
    let mut ws = WsConfig::new(inner);
    ws.set_tls_config(tls_config);
    ws
}

/// Starts a wss server with `server_cfg`, dials it by DNS name `localhost` with `client_cfg`, and
/// returns the handshake outcome of both sides after a one-way payload exchange.
async fn run(server_cfg: tls::Config, client_cfg: tls::Config) -> (std::io::Result<()>, std::io::Result<()>) {
    let mut server = new_ws(server_cfg).await.boxed();
    server
        .listen_on(ListenerId::next(), "/ip4/127.0.0.1/tcp/0/wss".parse().unwrap())
        .expect("listener");
    let listen_addr = server
        .next()
        .await
        .expect("no error")
        .into_new_address()
        .expect("listen address");
    assert_eq!(Some(Protocol::Wss("/".into())), listen_addr.iter().nth(2));
    let port = match listen_addr.iter().nth(1) {
        Some(Protocol::Tcp(p)) => p,
        other => panic!("unexpected {other:?}"),
    };
    let dial_addr: Multiaddr = format!("/dns4/localhost/tcp/{port}/wss").parse().unwrap();

    let inbound = async move {
        let (conn, _) = server
            .select_next_some()
            .map(|ev| ev.into_incoming())
            .await
            .unwrap();
        let mut conn = conn.await.map_err(|e| std::io::Error::other(e.to_string()))?;
        let mut buf = [0u8; 4];
        conn.read_exact(&mut buf).await?;
        assert_eq!(&buf, b"ping");
        Ok(())
    };
    let outbound = async move {
        let mut client = new_ws(client_cfg).await.boxed();
        let mut conn = client
            .dial(dial_addr.with(Protocol::P2p(PeerId::random())))
            .unwrap()
            .await
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        conn.write_all(b"ping").await?;
        conn.flush().await?;
        Ok(())
    };
    futures::join!(inbound, outbound)
}

fn trusting(key: tls::PrivateKey, cert_der: Vec<u8>) -> (tls::Config, tls::Config) {
    let server = tls::Config::new(key, vec![tls::Certificate::new(cert_der.clone())]).unwrap();
    let mut client = tls::Config::builder();
    client.add_trust(&tls::Certificate::new(cert_der)).unwrap();
    (server, client.finish())
}

#[async_std::test]
async fn handshake_with_pkcs8_key() {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let key = tls::PrivateKey::new(cert.serialize_private_key_der()).unwrap();
    let (server, client) = trusting(key, cert.serialize_der().unwrap());
    let (a, b) = run(server, client).await;
    a.unwrap();
    b.unwrap();
}

#[async_std::test]
async fn handshake_with_pkcs1_rsa_key() {
    let key = tls::PrivateKey::new(include_bytes!("fixtures/rsa_pkcs1_key.der").to_vec()).unwrap();
    let (server, client) = trusting(key, include_bytes!("fixtures/rsa_cert.der").to_vec());
    let (a, b) = run(server, client).await;
    a.unwrap();
    b.unwrap();
}

#[async_std::test]
async fn handshake_fails_when_client_does_not_trust_the_certificate() {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let key = tls::PrivateKey::new(cert.serialize_private_key_der()).unwrap();
    let server = tls::Config::new(key, vec![tls::Certificate::new(cert.serialize_der().unwrap())]).unwrap();
    // Only the webpki roots: a self-signed localhost certificate must be rejected.
    let (a, b) = run(server, tls::Config::client()).await;
    assert!(b.is_err(), "client must reject an untrusted certificate");
    assert!(a.is_err());
}

#[test]
fn malformed_private_key_is_an_error_not_a_panic() {
    assert!(tls::PrivateKey::new(Vec::new()).is_err());
    assert!(tls::PrivateKey::new(b"not a DER key".to_vec()).is_err());
}

#[test]
fn mismatched_key_and_certificate_is_an_error() {
    let a = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let b = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let key = tls::PrivateKey::new(a.serialize_private_key_der()).unwrap();
    let cfg = tls::Config::new(key, vec![tls::Certificate::new(b.serialize_der().unwrap())]);
    assert!(cfg.is_err());
}
