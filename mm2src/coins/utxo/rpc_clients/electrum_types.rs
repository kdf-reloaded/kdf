//! Electrum protocol response/request data types and the `electrum_script_hash`
//! helper.
//!
//! Carved out of `electrum_rpc_client.rs` (P13.5 follow-up: shrink the
//! 1500-line transport file by moving its inert data layer into a sibling
//! module). No behavior change — every type below was previously declared in
//! `electrum_rpc_client.rs` and is still re-exported through the same
//! `pub use electrum_rpc_client::*` path.

use super::*;

#[derive(Clone, Debug, Deserialize)]
pub struct ElectrumUnspent {
    pub height: Option<u64>,
    pub tx_hash: H256Json,
    pub tx_pos: u32,
    pub value: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum ElectrumNonce {
    Number(u64),
    Hash(H256Json),
}

#[allow(clippy::from_over_into)]
impl Into<BlockHeaderNonce> for ElectrumNonce {
    fn into(self) -> BlockHeaderNonce {
        match self {
            ElectrumNonce::Number(n) => BlockHeaderNonce::U32(n as u32),
            ElectrumNonce::Hash(h) => BlockHeaderNonce::H256(h.into()),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ElectrumBlockHeadersRes {
    pub count: u64,
    pub hex: BytesJson,
    #[allow(dead_code)]
    max: u64,
}

/// The block header compatible with Electrum 1.2
#[derive(Clone, Debug, Deserialize)]
pub struct ElectrumBlockHeaderV12 {
    pub bits: u64,
    pub block_height: u64,
    pub merkle_root: H256Json,
    pub nonce: ElectrumNonce,
    pub prev_block_hash: H256Json,
    pub timestamp: u64,
    pub version: u64,
}

impl ElectrumBlockHeaderV12 {
    fn as_block_header(&self) -> BlockHeader {
        BlockHeader {
            version: self.version as u32,
            previous_header_hash: self.prev_block_hash.into(),
            merkle_root_hash: self.merkle_root.into(),
            hash_final_sapling_root: None,
            time: self.timestamp as u32,
            bits: BlockHeaderBits::U32(self.bits as u32),
            nonce: self.nonce.clone().into(),
            solution: None,
            aux_pow: None,
            prog_pow: None,
            mtp_pow: None,
            is_verus: false,
            hash_state_root: None,
            hash_utxo_root: None,
            prevout_stake: None,
            vch_block_sig_dlgt: None,
            n_height: None,
            n_nonce_u64: None,
            mix_hash: None,
            wide_time: false,
            claim_trie_root: None,
            pivx_trailer: None,
        }
    }

    #[inline]
    pub fn as_hex(&self) -> String {
        let block_header = self.as_block_header();
        let serialized = serialize(&block_header);
        hex::encode(serialized)
    }

    #[inline]
    pub fn hash(&self) -> H256Json {
        let block_header = self.as_block_header();
        BlockHeader::hash(&block_header).into()
    }
}

/// The block header compatible with Electrum 1.4
#[derive(Clone, Debug, Deserialize)]
pub struct ElectrumBlockHeaderV14 {
    pub height: u64,
    pub hex: BytesJson,
}

impl ElectrumBlockHeaderV14 {
    pub fn hash(&self) -> H256Json { self.hex.clone().into_vec()[..].into() }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum ElectrumBlockHeader {
    V12(ElectrumBlockHeaderV12),
    V14(ElectrumBlockHeaderV14),
}

/// The merkle branch of a confirmed transaction
#[derive(Clone, Debug, Deserialize)]
pub struct TxMerkleBranch {
    pub merkle: Vec<H256Json>,
    pub block_height: u64,
    pub pos: usize,
}

#[derive(Debug, PartialEq)]
pub struct BestBlock {
    pub height: u64,
    pub hash: H256Json,
}

impl From<ElectrumBlockHeader> for BestBlock {
    fn from(block_header: ElectrumBlockHeader) -> Self {
        BestBlock {
            height: block_header.block_height(),
            hash: block_header.block_hash(),
        }
    }
}

#[allow(clippy::upper_case_acronyms)]
#[derive(Debug, Deserialize, Serialize)]
pub enum EstimateFeeMode {
    ECONOMICAL,
    CONSERVATIVE,
    UNSET,
}

impl ElectrumBlockHeader {
    pub fn block_height(&self) -> u64 {
        match self {
            ElectrumBlockHeader::V12(h) => h.block_height,
            ElectrumBlockHeader::V14(h) => h.height,
        }
    }

    fn block_hash(&self) -> H256Json {
        match self {
            ElectrumBlockHeader::V12(h) => h.hash(),
            ElectrumBlockHeader::V14(h) => h.hash(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ElectrumTxHistoryItem {
    pub height: i64,
    pub tx_hash: H256Json,
    pub fee: Option<i64>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ElectrumBalance {
    pub(crate) confirmed: i128,
    pub(crate) unconfirmed: i128,
}

impl ElectrumBalance {
    #[inline]
    pub fn to_big_decimal(&self, decimals: u8) -> BigDecimal {
        let balance_sat = BigInt::from(self.confirmed) + BigInt::from(self.unconfirmed);
        BigDecimal::from(balance_sat) / BigDecimal::from(10u64.pow(decimals as u32))
    }
}

#[inline]
fn sha_256(input: &[u8]) -> Vec<u8> {
    let mut sha = Sha256::new();
    sha.update(input);
    sha.finalize().to_vec()
}

#[inline]
pub fn electrum_script_hash(script: &[u8]) -> Vec<u8> {
    let mut result = sha_256(script);
    result.reverse();
    result
}

#[allow(clippy::upper_case_acronyms)]
#[derive(Clone, Debug, Deserialize, Serialize)]
/// Deserializable Electrum protocol representation for RPC
pub enum ElectrumProtocol {
    /// TCP
    TCP,
    /// SSL/TLS
    SSL,
    /// Insecure WebSocket.
    WS,
    /// Secure WebSocket.
    WSS,
}

#[cfg(not(target_arch = "wasm32"))]
impl Default for ElectrumProtocol {
    fn default() -> Self { ElectrumProtocol::TCP }
}

#[cfg(target_arch = "wasm32")]
impl Default for ElectrumProtocol {
    fn default() -> Self { ElectrumProtocol::WSS }
}

#[derive(Debug, Deserialize, Serialize)]
/// Deserializable Electrum protocol version representation for RPC
/// https://electrumx-spesmilo.readthedocs.io/en/latest/protocol-methods.html#server.version
pub struct ElectrumProtocolVersion {
    pub server_software_version: String,
    pub protocol_version: String,
}

#[derive(Clone, Deserialize, Serialize)]
/// Electrum request RPC representation
pub struct ElectrumRpcRequest {
    pub url: String,
    #[serde(default)]
    pub protocol: ElectrumProtocol,
    #[serde(default)]
    pub disable_cert_verification: bool,
    /// The time to wait for this server to answer a request, in seconds. The default applies when unset.
    #[serde(default)]
    pub timeout_sec: Option<f64>,
}

/// The debug form names the server as `host:port` only (CRD ch.38 R38.6.9a).
impl fmt::Debug for ElectrumRpcRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ElectrumRpcRequest")
            .field("url", &self.name())
            .field("protocol", &self.protocol)
            .field("disable_cert_verification", &self.disable_cert_verification)
            .field("timeout_sec", &self.timeout_sec)
            .finish()
    }
}

impl ElectrumRpcRequest {
    /// The server in the only form a text may name it: `host:port`, or a fixed placeholder when the address
    /// cannot be reduced to a host (CRD ch.38 R38.6.9a).
    pub fn name(&self) -> String { electrum_server_name(&self.url).unwrap_or_else(|| UNNAMED_SERVER.to_owned()) }

    /// The per-server request timeout, if the request set a usable one (finite and positive).
    pub fn request_timeout(&self) -> Option<Duration> {
        self.timeout_sec
            .filter(|secs| secs.is_finite() && *secs > 0.)
            .map(Duration::from_secs_f64)
    }
}
