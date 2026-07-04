use crate::client::TrezorSession;
use crate::ethereum::ETH_MAX_CHUNK_LEN;
use crate::proto::messages_ethereum as proto_ethereum;
use crate::result_handler::ResultHandler;
use crate::{ProcessTrezorResponse, TrezorError, TrezorProcessingError, TrezorRequestProcessor, TrezorResponse,
            TrezorResult};
use mm2_err_handle::prelude::*;

/// Transport-neutral input for signing a legacy (EIP-155) Ethereum transaction.
///
/// All byte vectors are big-endian and expected to be minimally trimmed (no leading
/// zero bytes) by the caller. `to` is a `0x`-prefixed hex string, or empty for a
/// contract-creation transaction.
#[derive(Clone, Debug, Default)]
pub struct TrezorEthTxInput {
    pub address_n: Vec<u32>,
    pub nonce: Vec<u8>,
    pub gas_price: Vec<u8>,
    pub gas_limit: Vec<u8>,
    pub to: String,
    pub value: Vec<u8>,
    pub data: Vec<u8>,
    pub chain_id: u64,
}

/// The raw signature components returned by the device for a legacy transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrezorEthSignature {
    pub v: u32,
    pub r: Vec<u8>,
    pub s: Vec<u8>,
}

/// Returns the initial payload chunk (at most [`ETH_MAX_CHUNK_LEN`] bytes) that must be
/// sent with the [`EthereumSignTx`](proto_ethereum::EthereumSignTx) message.
pub(crate) fn initial_chunk(data: &[u8]) -> &[u8] { &data[..std::cmp::min(ETH_MAX_CHUNK_LEN, data.len())] }

/// Returns the next payload chunk starting at `offset`, bounded by the device-requested
/// `requested` length, the remaining payload, and [`ETH_MAX_CHUNK_LEN`].
pub(crate) fn next_chunk(data: &[u8], offset: usize, requested: usize) -> &[u8] {
    if offset >= data.len() {
        return &[];
    }
    let end = std::cmp::min(
        data.len(),
        offset.saturating_add(std::cmp::min(requested, ETH_MAX_CHUNK_LEN)),
    );
    &data[offset..end]
}

/// Builds the [`EthereumSignTx`](proto_ethereum::EthereumSignTx) message for the given input,
/// embedding the initial payload chunk and the total payload length.
pub(crate) fn build_sign_tx_message(input: &TrezorEthTxInput) -> proto_ethereum::EthereumSignTx {
    let initial = initial_chunk(&input.data);
    proto_ethereum::EthereumSignTx {
        address_n: input.address_n.clone(),
        nonce: Some(input.nonce.clone()),
        gas_price: input.gas_price.clone(),
        gas_limit: input.gas_limit.clone(),
        to: if input.to.is_empty() {
            None
        } else {
            Some(input.to.clone())
        },
        value: Some(input.value.clone()),
        data_initial_chunk: if initial.is_empty() {
            None
        } else {
            Some(initial.to_vec())
        },
        data_length: Some(input.data.len() as u32),
        chain_id: input.chain_id,
    }
}

impl<'a> TrezorSession<'a> {
    /// Sign a legacy (EIP-155) Ethereum transaction on the device.
    ///
    /// https://docs.trezor.io/trezor-firmware/common/communication/ethereum-signing.html
    ///
    /// The transaction payload (`input.data`) is streamed to the device in chunks:
    /// the initial `<= 1024` bytes accompany the `EthereumSignTx` message, and each
    /// subsequent `EthereumTxRequest` that reports a non-zero `data_length` is answered
    /// with an `EthereumTxAck` carrying the next chunk. When the device returns a
    /// request with the signature fields populated, the raw `(v, r, s)` is returned.
    pub async fn sign_eth_tx(&mut self, input: TrezorEthTxInput) -> TrezorResult<TrezorEthSignature> {
        let mut offset = initial_chunk(&input.data).len();

        let req = build_sign_tx_message(&input);
        let mut tx_request = self.eth_sign_tx(req).await?.ack_all().await?;

        loop {
            if let Some(len) = tx_request.data_length {
                if len > 0 {
                    let chunk = next_chunk(&input.data, offset, len as usize);
                    offset += chunk.len();
                    let ack = proto_ethereum::EthereumTxAck {
                        data_chunk: chunk.to_vec(),
                    };
                    tx_request = self.eth_tx_ack(ack).await?.ack_all().await?;
                    continue;
                }
            }

            return match (tx_request.signature_v, tx_request.signature_r, tx_request.signature_s) {
                (Some(v), Some(r), Some(s)) => Ok(TrezorEthSignature { v, r, s }),
                _ => {
                    let error = "'EthereumTxRequest' is missing signature fields".to_owned();
                    MmError::err(TrezorError::ProtocolError(error))
                },
            };
        }
    }

    /// Sign a legacy (EIP-155) Ethereum transaction on the device, driving every
    /// device interaction (button / PIN / passphrase) through `processor`.
    ///
    /// Unlike [`sign_eth_tx`](Self::sign_eth_tx) — which uses `ack_all` and so
    /// aborts on a PIN or passphrase request — this variant surfaces those
    /// requests to the processor. A signing session is a *fresh* device session
    /// (a new `Initialize` with no cached passphrase), so a passphrase-protected
    /// (or PIN-protected) device re-requests its secret at signing time; this
    /// method lets the caller answer it (e.g. as an RPC-task user action).
    pub async fn sign_eth_tx_with_processor<Processor>(
        &mut self,
        input: TrezorEthTxInput,
        processor: &Processor,
    ) -> MmResult<TrezorEthSignature, TrezorProcessingError<Processor::Error>>
    where
        Processor: TrezorRequestProcessor + Sync,
    {
        let mut offset = initial_chunk(&input.data).len();

        let req = build_sign_tx_message(&input);
        let mut tx_request = self
            .eth_sign_tx(req)
            .await
            .mm_err(TrezorProcessingError::TrezorError)?
            .process(processor)
            .await?;

        loop {
            if let Some(len) = tx_request.data_length {
                if len > 0 {
                    let chunk = next_chunk(&input.data, offset, len as usize);
                    offset += chunk.len();
                    let ack = proto_ethereum::EthereumTxAck {
                        data_chunk: chunk.to_vec(),
                    };
                    tx_request = self
                        .eth_tx_ack(ack)
                        .await
                        .mm_err(TrezorProcessingError::TrezorError)?
                        .process(processor)
                        .await?;
                    continue;
                }
            }

            return match (tx_request.signature_v, tx_request.signature_r, tx_request.signature_s) {
                (Some(v), Some(r), Some(s)) => Ok(TrezorEthSignature { v, r, s }),
                _ => {
                    let error = "'EthereumTxRequest' is missing signature fields".to_owned();
                    MmError::err(TrezorProcessingError::TrezorError(TrezorError::ProtocolError(error)))
                },
            };
        }
    }

    async fn eth_sign_tx<'b>(
        &'b mut self,
        req: proto_ethereum::EthereumSignTx,
    ) -> TrezorResult<TrezorResponse<'a, 'b, proto_ethereum::EthereumTxRequest>> {
        let result_handler = ResultHandler::<proto_ethereum::EthereumTxRequest>::new(Ok);
        self.call(req, result_handler).await
    }

    async fn eth_tx_ack<'b>(
        &'b mut self,
        req: proto_ethereum::EthereumTxAck,
    ) -> TrezorResult<TrezorResponse<'a, 'b, proto_ethereum::EthereumTxRequest>> {
        let result_handler = ResultHandler::<proto_ethereum::EthereumTxRequest>::new(Ok);
        self.call(req, result_handler).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    fn eth_input_with_data(data: Vec<u8>) -> TrezorEthTxInput {
        TrezorEthTxInput {
            address_n: vec![44 + 0x8000_0000, 60 + 0x8000_0000, 0x8000_0000, 0, 0],
            nonce: vec![0x01],
            gas_price: vec![0x04, 0xa8, 0x17, 0xc8, 0x00],
            gas_limit: vec![0x52, 0x08],
            to: "0x1234567890123456789012345678901234567890".to_owned(),
            value: vec![0x0d, 0xe0, 0xb6, 0xb3, 0xa7, 0x64, 0x00, 0x00],
            data,
            chain_id: 1,
        }
    }

    #[test]
    fn sign_tx_message_prost_round_trip() {
        let msg = proto_ethereum::EthereumSignTx {
            address_n: vec![0x8000_002c, 0x8000_003c, 0x8000_0000, 0, 0],
            nonce: Some(vec![0x01]),
            gas_price: vec![0x04, 0xa8, 0x17, 0xc8, 0x00],
            gas_limit: vec![0x52, 0x08],
            to: Some("0xabcdef0000000000000000000000000000000001".to_owned()),
            value: Some(vec![0xff, 0xee]),
            data_initial_chunk: Some(vec![0xde, 0xad, 0xbe, 0xef]),
            data_length: Some(4),
            chain_id: 137,
        };

        let mut buf = Vec::new();
        msg.encode(&mut buf).unwrap();
        let decoded = proto_ethereum::EthereumSignTx::decode(buf.as_slice()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn tx_ack_prost_round_trip() {
        let msg = proto_ethereum::EthereumTxAck {
            data_chunk: vec![0x01, 0x02, 0x03, 0x04, 0x05],
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf).unwrap();
        let decoded = proto_ethereum::EthereumTxAck::decode(buf.as_slice()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn tx_request_prost_round_trip() {
        let msg = proto_ethereum::EthereumTxRequest {
            data_length: None,
            signature_v: Some(37),
            signature_r: Some(vec![0xaa; 32]),
            signature_s: Some(vec![0xbb; 32]),
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf).unwrap();
        let decoded = proto_ethereum::EthereumTxRequest::decode(buf.as_slice()).unwrap();
        assert_eq!(msg, decoded);

        // A "more data requested" variant.
        let msg = proto_ethereum::EthereumTxRequest {
            data_length: Some(1024),
            signature_v: None,
            signature_r: None,
            signature_s: None,
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf).unwrap();
        let decoded = proto_ethereum::EthereumTxRequest::decode(buf.as_slice()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn chunking_empty_data() {
        let data: Vec<u8> = Vec::new();
        assert!(initial_chunk(&data).is_empty());
        assert!(next_chunk(&data, 0, 100).is_empty());

        let input = eth_input_with_data(data);
        let msg = build_sign_tx_message(&input);
        assert_eq!(msg.data_length, Some(0));
        assert_eq!(msg.data_initial_chunk, None);
    }

    #[test]
    fn chunking_erc20_single_message() {
        // ERC20 transfer(address,uint256): 4-byte selector + 32-byte address + 32-byte amount.
        let data: Vec<u8> = (0..68u8).collect();
        let initial = initial_chunk(&data);
        assert_eq!(initial.len(), 68);
        assert_eq!(initial, data.as_slice());
        // Everything fit into the initial chunk; nothing left to stream.
        assert!(next_chunk(&data, initial.len(), 100).is_empty());

        let input = eth_input_with_data(data.clone());
        let msg = build_sign_tx_message(&input);
        assert_eq!(msg.data_length, Some(68));
        assert_eq!(msg.data_initial_chunk.as_deref(), Some(data.as_slice()));
    }

    #[test]
    fn chunking_multi_chunk_payload() {
        // 2100 bytes -> initial 1024, then 1024, then 52.
        let data: Vec<u8> = (0..2100u32).map(|i| (i % 251) as u8).collect();

        let initial = initial_chunk(&data);
        assert_eq!(initial.len(), ETH_MAX_CHUNK_LEN);
        assert_eq!(initial, &data[..ETH_MAX_CHUNK_LEN]);

        let mut offset = initial.len();
        let mut reconstructed = initial.to_vec();

        // Device requests 1024 bytes.
        let chunk = next_chunk(&data, offset, 1024);
        assert_eq!(chunk.len(), ETH_MAX_CHUNK_LEN);
        offset += chunk.len();
        reconstructed.extend_from_slice(chunk);

        // Device requests the remaining 52 bytes.
        let chunk = next_chunk(&data, offset, 1024);
        assert_eq!(chunk.len(), 2100 - 2 * ETH_MAX_CHUNK_LEN);
        offset += chunk.len();
        reconstructed.extend_from_slice(chunk);

        assert_eq!(offset, data.len());
        assert_eq!(reconstructed, data);
        assert!(next_chunk(&data, offset, 1024).is_empty());

        let input = eth_input_with_data(data);
        let msg = build_sign_tx_message(&input);
        assert_eq!(msg.data_length, Some(2100));
        assert_eq!(msg.data_initial_chunk.map(|c| c.len()), Some(ETH_MAX_CHUNK_LEN));
    }

    #[test]
    fn chunking_request_capped_to_max_chunk_len() {
        // Even if the device asks for more than ETH_MAX_CHUNK_LEN, a single slice is capped.
        let data: Vec<u8> = vec![7u8; 3000];
        let chunk = next_chunk(&data, 0, 5000);
        assert_eq!(chunk.len(), ETH_MAX_CHUNK_LEN);
    }

    #[test]
    fn build_sign_tx_message_contract_creation_has_no_to() {
        let mut input = eth_input_with_data(vec![0xab, 0xcd]);
        input.to = String::new();
        let msg = build_sign_tx_message(&input);
        assert_eq!(msg.to, None);
    }
}
