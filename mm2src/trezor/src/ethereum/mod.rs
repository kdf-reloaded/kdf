mod eth_command;
mod sign_eth;

pub use sign_eth::{TrezorEthSignature, TrezorEthTxInput};

/// The maximum length of a single Ethereum transaction payload chunk that can be
/// transmitted in one protobuf message (`EthereumSignTx.data_initial_chunk` or
/// `EthereumTxAck.data_chunk`).
///
/// See https://docs.trezor.io/trezor-firmware/common/communication/ethereum-signing.html
pub(crate) const ETH_MAX_CHUNK_LEN: usize = 1024;
