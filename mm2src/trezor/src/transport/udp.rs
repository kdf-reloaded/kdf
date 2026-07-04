//! UDP transport for the Trezor emulator.
//!
//! The emulator speaks the same v1 framing (`ProtocolV1`) as the USB transport, but each
//! 64-byte protocol chunk is exchanged as a single UDP datagram. This transport is intended
//! for the test harness only and is gated behind the `trezor-udp` cargo feature (native only).

use crate::client::TrezorClient;
use crate::proto::ProtoMessage;
use crate::transport::protocol::{Link, Protocol, ProtocolV1};
use crate::transport::Transport;
use crate::{TrezorError, TrezorResult};
use async_trait::async_trait;
use common::async_blocking;
use mm2_err_handle::prelude::*;
use std::net::UdpSocket;
use std::sync::Arc;
use std::time::Duration;

/// Environment variable overriding the emulator UDP address.
const EMULATOR_ADDR_ENV: &str = "TREZOR_EMULATOR_UDP";
/// Default Trezor emulator UDP address.
const DEFAULT_EMULATOR_ADDR: &str = "127.0.0.1:21324";
/// Read timeout for a single datagram.
const READ_TIMEOUT: Duration = Duration::from_secs(600);

pub struct UdpTransport {
    protocol: ProtocolV1<UdpLink>,
}

#[async_trait]
impl Transport for UdpTransport {
    async fn session_begin(&mut self) -> TrezorResult<()> { self.protocol.session_begin().await }

    async fn session_end(&mut self) -> TrezorResult<()> { self.protocol.session_end().await }

    async fn write_message(&mut self, message: ProtoMessage) -> TrezorResult<()> { self.protocol.write(message).await }

    async fn read_message(&mut self) -> TrezorResult<ProtoMessage> { self.protocol.read().await }
}

impl UdpTransport {
    /// Connect to the emulator listening on the address from `TREZOR_EMULATOR_UDP`
    /// (defaults to `127.0.0.1:21324`).
    pub fn connect() -> TrezorResult<UdpTransport> {
        let addr = std::env::var(EMULATOR_ADDR_ENV).unwrap_or_else(|_| DEFAULT_EMULATOR_ADDR.to_owned());
        let socket = UdpSocket::bind("0.0.0.0:0")
            .map_to_mm(|e| TrezorError::UnderlyingError(format!("Failed to bind UDP socket: {}", e)))?;
        socket
            .connect(&addr)
            .map_to_mm(|e| TrezorError::UnderlyingError(format!("Failed to connect to '{}': {}", addr, e)))?;
        socket
            .set_read_timeout(Some(READ_TIMEOUT))
            .map_to_mm(|e| TrezorError::UnderlyingError(format!("Failed to set read timeout: {}", e)))?;

        let link = UdpLink {
            socket: Arc::new(socket),
        };
        Ok(UdpTransport {
            protocol: ProtocolV1 { link },
        })
    }
}

struct UdpLink {
    socket: Arc<UdpSocket>,
}

#[async_trait]
impl Link for UdpLink {
    async fn write_chunk(&mut self, chunk: Vec<u8>) -> TrezorResult<()> {
        let socket = self.socket.clone();
        async_blocking(move || {
            socket
                .send(&chunk)
                .map(|_| ())
                .map_to_mm(|e| TrezorError::UnderlyingError(format!("Failed to send UDP chunk: {}", e)))
        })
        .await
    }

    async fn read_chunk(&mut self, chunk_len: u32) -> TrezorResult<Vec<u8>> {
        let socket = self.socket.clone();
        async_blocking(move || {
            let mut buf = vec![0u8; chunk_len as usize];
            let n = socket
                .recv(&mut buf)
                .map_to_mm(|e| TrezorError::UnderlyingError(format!("Failed to read UDP chunk: {}", e)))?;
            buf.truncate(n);
            Ok(buf)
        })
        .await
    }
}

/// Open a UDP-backed [`TrezorClient`] connected to the emulator. Intended for tests.
pub fn trezor_udp_client() -> TrezorResult<TrezorClient> {
    let transport = UdpTransport::connect()?;
    Ok(TrezorClient::from_transport(transport))
}
