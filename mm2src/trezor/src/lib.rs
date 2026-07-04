#![allow(dead_code)] // Protobuf scaffolding — many message types defined for protocol completeness

#[macro_use] extern crate serde_derive;

pub mod client;
pub mod error;
pub mod ethereum;
#[allow(dead_code)] mod proto;
pub mod response;
mod response_processor;
mod result_handler;
pub mod transport;
pub mod trezor_rpc_task;
pub mod user_interaction;
pub mod utxo;

pub use client::{TrezorClient, TrezorSession};
pub use error::{OperationFailure, TrezorError, TrezorResult};
pub use ethereum::{TrezorEthSignature, TrezorEthTxInput};
pub use hw_common::primitives::{DerivationPath, EcdsaCurve};
pub use response::{ButtonRequest, PassphraseRequest, PinMatrixRequest, TrezorResponse};
pub use response_processor::{ProcessTrezorResponse, TrezorProcessingError, TrezorRequestProcessor};
pub use user_interaction::{TrezorPassphraseResponse, TrezorPinMatrix3x3Response, TrezorUserInteraction};

pub(crate) fn serialize_derivation_path(path: &DerivationPath) -> Vec<u32> {
    path.iter().map(|index| index.0).collect()
}

pub(crate) fn ecdsa_curve_to_string(curve: EcdsaCurve) -> String {
    match curve {
        EcdsaCurve::Secp256k1 => "secp256k1".to_owned(),
    }
}
