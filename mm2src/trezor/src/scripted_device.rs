//! A scripted Trezor device for tests of the crates above this one (feature `test-helpers`).
//!
//! The device answers exactly two requests, each from a fixed script: `Initialize` with an empty `Features`, and
//! `GetPublicKey` with the extended public key it was built with. Any other request is answered with an error. It
//! needs no transport, no emulator and no user interaction, so a test can stand in for "the connected device holds
//! this key" without a device.

use crate::client::TrezorClient;
use crate::proto::messages::MessageType;
use crate::proto::messages_bitcoin::PublicKey;
use crate::proto::messages_common::HdNodeType;
use crate::proto::messages_management::Features;
use crate::proto::ProtoMessage;
use crate::transport::Transport;
use crate::{TrezorError, TrezorResult};
use async_trait::async_trait;
use mm2_err_handle::prelude::*;
use prost::Message;

struct ScriptedTransport {
    xpub: String,
    last_request: Option<MessageType>,
}

#[async_trait]
impl Transport for ScriptedTransport {
    async fn session_begin(&mut self) -> TrezorResult<()> { Ok(()) }

    async fn session_end(&mut self) -> TrezorResult<()> { Ok(()) }

    async fn write_message(&mut self, message: ProtoMessage) -> TrezorResult<()> {
        self.last_request = Some(message.message_type());
        Ok(())
    }

    async fn read_message(&mut self) -> TrezorResult<ProtoMessage> {
        let mut payload = Vec::new();
        match self.last_request.take() {
            Some(MessageType::Initialize) => {
                Features::default()
                    .encode(&mut payload)
                    .map_to_mm(|e| TrezorError::Internal(e.to_string()))?;
                Ok(ProtoMessage::new(MessageType::Features, payload))
            },
            Some(MessageType::GetPublicKey) => {
                PublicKey {
                    node: HdNodeType::default(),
                    xpub: self.xpub.clone(),
                    root_fingerprint: None,
                }
                .encode(&mut payload)
                .map_to_mm(|e| TrezorError::Internal(e.to_string()))?;
                Ok(ProtoMessage::new(MessageType::PublicKey, payload))
            },
            other => MmError::err(TrezorError::Internal(format!(
                "The scripted device answers only Initialize and GetPublicKey, got {:?}",
                other
            ))),
        }
    }
}

/// A client of a scripted device that answers every `GetPublicKey` request with `xpub`.
pub fn scripted_device_with_xpub(xpub: &str) -> TrezorClient {
    TrezorClient::from_transport(ScriptedTransport {
        xpub: xpub.to_owned(),
        last_request: None,
    })
}
