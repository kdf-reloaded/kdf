use crate::client::TrezorSession;
use crate::proto::messages_ethereum as proto_ethereum;
use crate::result_handler::ResultHandler;
use crate::{serialize_derivation_path, TrezorError, TrezorResponse, TrezorResult};
use hw_common::primitives::{DerivationPath, XPub};
use mm2_err_handle::prelude::*;

// Ethereum (EVM) operations.
impl<'a> TrezorSession<'a> {
    /// Get an EVM address (hex-encoded, `0x`-prefixed) for the given derivation path.
    pub async fn get_eth_address<'b>(
        &'b mut self,
        path: DerivationPath,
        show_display: bool,
    ) -> TrezorResult<TrezorResponse<'a, 'b, String>> {
        let req = proto_ethereum::EthereumGetAddress {
            address_n: serialize_derivation_path(&path),
            show_display: Some(show_display),
        };
        let result_handler = ResultHandler::new(|m: proto_ethereum::EthereumAddress| {
            m.address
                .or_mm_err(|| TrezorError::ProtocolError("'EthereumAddress::address' is expected to be set".to_owned()))
        });
        self.call(req, result_handler).await
    }

    /// Get the secp256k1 public node (serialized `xpub`) for the given derivation path.
    pub async fn get_eth_public_key<'b>(
        &'b mut self,
        path: DerivationPath,
        show_display: bool,
    ) -> TrezorResult<TrezorResponse<'a, 'b, XPub>> {
        let req = proto_ethereum::EthereumGetPublicKey {
            address_n: serialize_derivation_path(&path),
            show_display: Some(show_display),
        };
        let result_handler = ResultHandler::new(|m: proto_ethereum::EthereumPublicKey| Ok(m.xpub));
        self.call(req, result_handler).await
    }
}
