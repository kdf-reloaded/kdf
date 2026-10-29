#![cfg_attr(target_arch = "wasm32", allow(unused_macros))]
#![cfg_attr(target_arch = "wasm32", allow(dead_code))]

use crate::utxo::header_layout_fallback::HeaderReadPolicy;
use crate::utxo::{output_script, sat_from_big_decimal};
use crate::{big_decimal_from_sat_unsigned, NumConversError, RpcTransportEventHandler, RpcTransportEventHandlerShared};
use async_trait::async_trait;
use bigdecimal::BigDecimal;
use chain::{BlockHeader, BlockHeaderBits, BlockHeaderNonce, OutPoint, Transaction as UtxoTx};
use common::custom_futures::{select_ok_sequential, FutureTimerExt};
use common::custom_iter::{CollectInto, TryIntoGroupMap};
use common::executor::{spawn, Timer};
use common::jsonrpc_client::{JsonRpcBatchClient, JsonRpcBatchResponse, JsonRpcClient, JsonRpcError, JsonRpcErrorType,
                             JsonRpcId, JsonRpcMultiClient, JsonRpcRemoteAddr, JsonRpcRequest, JsonRpcRequestEnum,
                             JsonRpcResponse, JsonRpcResponseEnum, JsonRpcResponseFut, RpcRes};
use common::log::{error, info, warn};
use common::mm_number::{BigInt, MmNumber};
use common::{median, now_float, now_ms, OrdRange};
use derive_more::Display;
use futures::channel::oneshot as async_oneshot;
use futures::compat::{Future01CompatExt, Stream01CompatExt};
use futures::future::{select as select_func, FutureExt, TryFutureExt};
use futures::lock::Mutex as AsyncMutex;
use futures::{select, StreamExt};
use futures01::future::select_ok;
use futures01::sync::{mpsc, oneshot};
use futures01::{Future, Sink, Stream};
use http::Uri;
use itertools::Itertools;
use keys::hash::H256;
use keys::{Address, Type as ScriptType};
use mm2_err_handle::prelude::*;
#[cfg(test)] use mocktopus::macros::*;
use rpc::v1::types::{Bytes as BytesJson, Transaction as RpcTransaction, H256 as H256Json};
use serde_json::{self as json, Value as Json};
use serialization::{deserialize, serialize, serialize_with_flags, CoinVariant, CompactInteger, Reader,
                    SERIALIZE_TRANSACTION_WITNESS};
use sha2::{Digest, Sha256};
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::fmt;
use std::io;
use std::net::{SocketAddr, ToSocketAddrs};
use std::num::NonZeroU64;
use std::ops::Deref;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::time::Duration;

cfg_native! {
    use futures::future::Either;
    use futures::io::Error;
    use http::header::AUTHORIZATION;
    use http::{Request, StatusCode};
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::crypto::{ring as rustls_ring, verify_tls12_signature, verify_tls13_signature, CryptoProvider};
    use rustls::pki_types::{CertificateDer, DnsName, ServerName, UnixTime};
    use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
    use std::convert::TryFrom;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf};
    use tokio::net::TcpStream;
    use tokio_rustls::{client::TlsStream, TlsConnector};
    use webpki_roots::TLS_SERVER_ROOTS;
}

pub type AddressesByLabelResult = HashMap<String, AddressPurpose>;
pub type JsonRpcPendingRequestsShared = Arc<AsyncMutex<JsonRpcPendingRequests>>;
pub type JsonRpcPendingRequests = HashMap<JsonRpcId, async_oneshot::Sender<JsonRpcResponseEnum>>;
pub type UnspentMap = HashMap<Address, Vec<UnspentInfo>>;

#[path = "rpc_clients/native_rpc_client.rs"]
pub(crate) mod native_rpc_client;
pub use native_rpc_client::*;

#[path = "rpc_clients/electrum_types.rs"]
pub(crate) mod electrum_types;
pub use electrum_types::*;

#[path = "rpc_clients/electrum_measure.rs"]
pub(crate) mod electrum_measure;

#[path = "rpc_clients/electrum_pool.rs"]
pub(crate) mod electrum_pool;
pub use electrum_pool::{ElectrumPoolOptions, ElectrumPoolTimings, ServerOrder};

#[path = "rpc_clients/electrum_rpc_client.rs"]
pub(crate) mod electrum_rpc_client;
pub use electrum_rpc_client::*;

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "rpc_clients/electrum_stub_server.rs"]
pub(crate) mod electrum_stub_server;

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "rpc_clients/electrum_pool_tests.rs"]
mod electrum_pool_tests;

type ElectrumScriptHash = String;
type ScriptHashUnspents = Vec<ElectrumUnspent>;

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct AddressPurpose {
    purpose: String,
}

/// Skips the server certificate verification on TLS connection.
///
/// Only the certificate *chain and name* checks are skipped. The handshake signature is still
/// verified against the public key of the presented certificate (as rustls 0.20 did by default),
/// so the certificate must still parse and the peer must prove possession of its key.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
pub struct NoCertificateVerification {
    provider: Arc<CryptoProvider>,
}

#[cfg(not(target_arch = "wasm32"))]
impl NoCertificateVerification {
    pub fn new(provider: Arc<CryptoProvider>) -> Self { NoCertificateVerification { provider } }
}

#[cfg(not(target_arch = "wasm32"))]
impl ServerCertVerifier for NoCertificateVerification {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

#[derive(Debug)]
pub enum UtxoRpcClientEnum {
    Native(NativeClient),
    Electrum(ElectrumClient),
    /// The sole backend of a Light-mode ZHTLC coin (CRD ch.39 R39.11.1, R39.11.4g). Native builds only:
    /// the shielded wallet database it serves does not exist on WASM.
    #[cfg(not(target_arch = "wasm32"))]
    LightWalletD(crate::z_coin::LightwalletdClient),
}

impl From<ElectrumClient> for UtxoRpcClientEnum {
    fn from(client: ElectrumClient) -> UtxoRpcClientEnum { UtxoRpcClientEnum::Electrum(client) }
}

impl From<NativeClient> for UtxoRpcClientEnum {
    fn from(client: NativeClient) -> UtxoRpcClientEnum { UtxoRpcClientEnum::Native(client) }
}

impl Deref for UtxoRpcClientEnum {
    type Target = dyn UtxoRpcClientOps;
    fn deref(&self) -> &dyn UtxoRpcClientOps {
        match self {
            UtxoRpcClientEnum::Native(ref c) => c,
            UtxoRpcClientEnum::Electrum(ref c) => c,
            #[cfg(not(target_arch = "wasm32"))]
            UtxoRpcClientEnum::LightWalletD(ref c) => c,
        }
    }
}

impl Clone for UtxoRpcClientEnum {
    fn clone(&self) -> Self {
        match self {
            UtxoRpcClientEnum::Native(c) => UtxoRpcClientEnum::Native(c.clone()),
            UtxoRpcClientEnum::Electrum(c) => UtxoRpcClientEnum::Electrum(c.clone()),
            #[cfg(not(target_arch = "wasm32"))]
            UtxoRpcClientEnum::LightWalletD(c) => UtxoRpcClientEnum::LightWalletD(c.clone()),
        }
    }
}

impl UtxoRpcClientEnum {
    pub fn wait_for_confirmations(
        &self,
        tx_hash: H256Json,
        expiry_height: u32,
        confirmations: u32,
        requires_notarization: bool,
        wait_until: u64,
        check_every: u64,
    ) -> Box<dyn Future<Item = (), Error = String> + Send> {
        // The lightwalletd backend derives confirmations locally from the mined height and the tip
        // and treats an uncorroborated expiry differently (CRD ch.39 R39.11.4e, R39.11.12).
        #[cfg(not(target_arch = "wasm32"))]
        if let UtxoRpcClientEnum::LightWalletD(client) = self {
            let client = client.clone();
            let fut = async move {
                client
                    .wait_for_confirmations(
                        tx_hash,
                        expiry_height,
                        confirmations,
                        requires_notarization,
                        wait_until,
                        check_every,
                    )
                    .await
            };
            return Box::new(fut.boxed().compat());
        }
        let selfi = self.clone();
        let fut = async move {
            loop {
                if now_ms() / 1000 > wait_until {
                    return ERR!(
                        "Waited too long until {} for transaction {:?} to be confirmed {} times",
                        wait_until,
                        tx_hash,
                        confirmations
                    );
                }

                match selfi.get_verbose_transaction(&tx_hash).compat().await {
                    Ok(t) => {
                        let tx_confirmations = if requires_notarization {
                            t.confirmations
                        } else {
                            t.rawconfirmations.unwrap_or(t.confirmations)
                        };
                        if tx_confirmations >= confirmations {
                            return Ok(());
                        } else {
                            info!(
                                "Waiting for tx {:?} confirmations, now {}, required {}, requires_notarization {}",
                                tx_hash, tx_confirmations, confirmations, requires_notarization
                            )
                        }
                    },
                    Err(e) => {
                        if expiry_height > 0 {
                            let block = match selfi.get_block_count().compat().await {
                                Ok(b) => b,
                                Err(e) => {
                                    error!("Error {} getting block number, retrying in 10 seconds", e);
                                    Timer::sleep(check_every as f64).await;
                                    continue;
                                },
                            };

                            if block > expiry_height as u64 {
                                return ERR!("The transaction {:?} has expired, current block {}", tx_hash, block);
                            }
                        }
                        error!(
                            "Error {:?} getting the transaction {:?}, retrying in 10 seconds",
                            e, tx_hash
                        )
                    },
                }

                Timer::sleep(check_every as f64).await;
            }
        };
        Box::new(fut.boxed().compat())
    }

    #[inline]
    pub fn is_native(&self) -> bool {
        match self {
            UtxoRpcClientEnum::Native(_) => true,
            UtxoRpcClientEnum::Electrum(_) => false,
            #[cfg(not(target_arch = "wasm32"))]
            UtxoRpcClientEnum::LightWalletD(_) => false,
        }
    }
}

/// Generic unspent info required to build transactions, we need this separate type because native
/// and Electrum provide different list_unspent format.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct UnspentInfo {
    pub outpoint: OutPoint,
    pub value: u64,
    /// The block height transaction mined in.
    /// Note None if the transaction is not mined yet.
    pub height: Option<u64>,
}

impl From<ElectrumUnspent> for UnspentInfo {
    fn from(electrum: ElectrumUnspent) -> UnspentInfo {
        UnspentInfo {
            outpoint: OutPoint {
                hash: electrum.tx_hash.reversed().into(),
                index: electrum.tx_pos,
            },
            value: electrum.value,
            height: electrum.height,
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum BlockHashOrHeight {
    Height(i64),
    Hash(H256Json),
}

#[derive(Debug, PartialEq)]
pub struct SpentOutputInfo {
    // The transaction spending the output
    pub spending_tx: UtxoTx,
    // The input index that spends the output
    pub input_index: usize,
    // The block hash or height the includes the spending transaction
    // For electrum clients the block height will be returned, for native clients the block hash will be returned
    pub spent_in_block: BlockHashOrHeight,
}

pub type UtxoRpcResult<T> = Result<T, MmError<UtxoRpcError>>;
pub type UtxoRpcFut<T> = Box<dyn Future<Item = T, Error = MmError<UtxoRpcError>> + Send + 'static>;

#[derive(Debug, Display)]
pub enum UtxoRpcError {
    Transport(JsonRpcError),
    ResponseParseError(JsonRpcError),
    InvalidResponse(String),
    Internal(String),
}

impl From<JsonRpcError> for UtxoRpcError {
    fn from(e: JsonRpcError) -> Self {
        match e.error {
            JsonRpcErrorType::InvalidRequest(_) => UtxoRpcError::Internal(e.to_string()),
            JsonRpcErrorType::Transport(_) => UtxoRpcError::Transport(e),
            JsonRpcErrorType::Parse(_, _) | JsonRpcErrorType::Response(_, _) => UtxoRpcError::ResponseParseError(e),
        }
    }
}

impl From<serialization::Error> for UtxoRpcError {
    fn from(e: serialization::Error) -> Self { UtxoRpcError::InvalidResponse(format!("{:?}", e)) }
}

impl From<NumConversError> for UtxoRpcError {
    fn from(e: NumConversError) -> Self { UtxoRpcError::Internal(e.to_string()) }
}

/// Common operations that both types of UTXO clients have but implement them differently
#[async_trait]
pub trait UtxoRpcClientOps: fmt::Debug + Send + Sync + 'static {
    /// Returns available unspents for the given `address`.
    fn list_unspent(&self, address: &Address, decimals: u8) -> UtxoRpcFut<Vec<UnspentInfo>>;

    /// Returns available unspents for every given `addresses`.
    fn list_unspent_group(&self, addresses: Vec<Address>, decimals: u8) -> UtxoRpcFut<UnspentMap>;

    /// Submits the given `tx` transaction to blockchain network.
    fn send_transaction(&self, tx: &UtxoTx) -> UtxoRpcFut<H256Json>;

    /// Submits the raw `tx` transaction (serialized, hex-encoded) to blockchain network.
    fn send_raw_transaction(&self, tx: BytesJson) -> UtxoRpcFut<H256Json>;

    /// Returns raw transaction (serialized, hex-encoded) by the given `txid`.
    fn get_transaction_bytes(&self, txid: &H256Json) -> UtxoRpcFut<BytesJson>;

    /// Returns verbose transaction by the given `txid`.
    fn get_verbose_transaction(&self, txid: &H256Json) -> UtxoRpcFut<RpcTransaction>;

    /// Returns verbose transactions in the same order they were requested.
    fn get_verbose_transactions(&self, tx_ids: &[H256Json]) -> UtxoRpcFut<Vec<RpcTransaction>>;

    /// Returns the height of the most-work fully-validated chain.
    fn get_block_count(&self) -> UtxoRpcFut<u64>;

    /// Requests balance of the given `address`.
    fn display_balance(&self, address: Address, decimals: u8) -> RpcRes<BigDecimal>;

    /// Requests balances of the given `addresses`.
    /// The pairs `(Address, BigDecimal)` are guaranteed to be in the same order in which they were requested.
    fn display_balances(&self, addresses: Vec<Address>, decimals: u8) -> UtxoRpcFut<Vec<(Address, BigDecimal)>>;

    /// Returns fee estimation per KByte in satoshis.
    fn estimate_fee_sat(
        &self,
        decimals: u8,
        fee_method: &EstimateFeeMethod,
        mode: &Option<EstimateFeeMode>,
        n_blocks: u32,
    ) -> UtxoRpcFut<u64>;

    /// Returns the minimum fee a low-priority transaction must pay in order to be accepted to the daemon’s memory pool.
    fn get_relay_fee(&self) -> RpcRes<BigDecimal>;

    /// Tries to find a transaction that spends the specified `vout` output of the `tx_hash` transaction.
    fn find_output_spend(
        &self,
        tx_hash: H256,
        script_pubkey: &[u8],
        vout: usize,
        from_block: BlockHashOrHeight,
    ) -> Box<dyn Future<Item = Option<SpentOutputInfo>, Error = String> + Send>;

    /// Get median time past for `count` blocks in the past including `starting_block`
    fn get_median_time_past(
        &self,
        starting_block: u64,
        count: NonZeroU64,
        coin_variant: CoinVariant,
    ) -> UtxoRpcFut<u32>;

    /// As [`UtxoRpcClient::get_median_time_past`], reading the served header list under `policy`
    /// (the coin's one layout and the structural fallback, CRD ch.37 R37.5.1a /
    /// R37.5.9e). Clients that read no header list use only the layout of `policy`.
    fn get_median_time_past_with_policy(
        &self,
        starting_block: u64,
        count: NonZeroU64,
        policy: HeaderReadPolicy,
    ) -> UtxoRpcFut<u32> {
        self.get_median_time_past(starting_block, count, policy.layout())
    }

    /// Returns block time in seconds since epoch (Jan 1 1970 GMT).
    async fn get_block_timestamp(&self, height: u64) -> Result<u64, MmError<UtxoRpcError>>;

    /// As [`UtxoRpcClient::get_block_timestamp`], reading the served header under `policy` (the
    /// coin's pinned layout, or the standard layout with the structural fallback, CRD ch.37
    /// R37.5.1a / R37.5.9e). Clients that parse no header ignore `policy`.
    async fn get_block_timestamp_with_policy(
        &self,
        height: u64,
        _policy: &HeaderReadPolicy,
    ) -> Result<u64, MmError<UtxoRpcError>> {
        self.get_block_timestamp(height).await
    }
}
