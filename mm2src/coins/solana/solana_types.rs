// solana_types — Constants, structs, enums, error types, and core types.

use super::*;
use crate::solana::rpc_pool::SolanaRpcPool;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};

/// The text of every swap operation of a Solana coin or token (CRD ch.40 R40.6B.14): swaps are not supported.
pub(crate) fn swaps_unsupported(ticker: &str) -> String { format!("Solana swaps are not supported ({ticker})") }

pub const SOLANA_DEFAULT_DECIMALS: u64 = 9;
pub const LAMPORTS_DUMMY_AMOUNT: u64 = 10;

#[async_trait]
pub trait SolanaCommonOps {
    fn rpc(&self) -> &SolanaRpcPool;

    fn is_token(&self) -> bool;

    async fn check_balance_and_prepare_transfer(
        &self,
        max: bool,
        amount: BigDecimal,
        fees: u64,
    ) -> Result<PrepareTransferData, MmError<SufficientBalanceError>>;
}

impl From<RpcError> for BalanceError {
    fn from(e: RpcError) -> Self {
        match e.kind {
            RpcErrorKind::Transport(s) => BalanceError::Transport(s),
            RpcErrorKind::Decode(s) => BalanceError::InvalidResponse(s),
            RpcErrorKind::Rpc(obj) => BalanceError::Transport(format!("server error {}: {}", obj.code, obj.message)),
        }
    }
}

impl From<ParsePubkeyError> for BalanceError {
    fn from(e: ParsePubkeyError) -> Self { BalanceError::Internal(format!("{:?}", e)) }
}

impl From<RpcError> for WithdrawError {
    fn from(e: RpcError) -> Self {
        match e.kind {
            RpcErrorKind::Transport(s) => WithdrawError::Transport(s),
            RpcErrorKind::Decode(s) => WithdrawError::InternalError(s),
            RpcErrorKind::Rpc(obj) => WithdrawError::Transport(format!("server error {}: {}", obj.code, obj.message)),
        }
    }
}

impl From<ParsePubkeyError> for WithdrawError {
    fn from(e: ParsePubkeyError) -> Self { WithdrawError::InvalidAddress(format!("{:?}", e)) }
}

impl From<ProgramError> for WithdrawError {
    fn from(e: ProgramError) -> Self { WithdrawError::InternalError(format!("{:?}", e)) }
}

#[derive(Debug)]
pub enum AccountError {
    NotFundedError(String),
    ParsePubKeyError(String),
    ClientError(RpcErrorKind),
}

impl From<RpcError> for AccountError {
    fn from(e: RpcError) -> Self { AccountError::ClientError(e.kind) }
}

impl From<ParsePubkeyError> for AccountError {
    fn from(e: ParsePubkeyError) -> Self { AccountError::ParsePubKeyError(format!("{:?}", e)) }
}

impl From<AccountError> for WithdrawError {
    fn from(e: AccountError) -> Self {
        match e {
            AccountError::NotFundedError(_) => WithdrawError::ZeroBalanceToWithdrawMax,
            AccountError::ParsePubKeyError(err) => WithdrawError::InternalError(err),
            AccountError::ClientError(e) => WithdrawError::Transport(format!("{:?}", e)),
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct SolanaActivationParams {
    confirmation_commitment: CommitmentLevel,
    /// Single-endpoint shorthand. Kept for backwards compatibility
    /// with existing GUI configs; if `client_urls` is also supplied
    /// the two lists are concatenated (this URL first).
    #[serde(default)]
    client_url: Option<String>,
    /// Multi-endpoint pool. Activation succeeds if at least one URL
    /// is provided across both fields. Endpoints are tried in the
    /// order given; on a transport failure the offender is
    /// quarantined for `QUARANTINE_TTL_SECS` and traffic moves on.
    #[serde(default)]
    client_urls: Vec<String>,
}

/// The debug form names each node by its host-only form (CRD ch.40 R40.5A.1, ch.35 R35.1.17).
impl std::fmt::Debug for SolanaActivationParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let client_urls: Vec<String> = self
            .client_urls
            .iter()
            .map(|url| crate::node_url::host_only(url))
            .collect();
        f.debug_struct("SolanaActivationParams")
            .field("confirmation_commitment", &self.confirmation_commitment)
            .field(
                "client_url",
                &self.client_url.as_deref().map(crate::node_url::host_only),
            )
            .field("client_urls", &client_urls)
            .finish()
    }
}

impl SolanaActivationParams {
    /// Flattened endpoint list in dispatch order.
    pub(crate) fn collected_urls(&self) -> Vec<String> {
        let mut v: Vec<String> = Vec::with_capacity(self.client_urls.len() + 1);
        if let Some(u) = self.client_url.as_ref() {
            v.push(u.clone());
        }
        v.extend(self.client_urls.iter().cloned());
        v
    }
}

#[derive(Debug, Display)]
pub enum SolanaFromLegacyReqErr {
    InvalidCommitmentLevel(String),
    InvalidClientParsing(json::Error),
    ClientNoAvailableNodes(String),
}

#[derive(Debug, Display)]
pub enum KeyPairCreationError {
    #[display(fmt = "Signature error: {}", _0)]
    SignatureError(ed25519_dalek::SignatureError),
    #[display(fmt = "KeyPairFromSeed error: {}", _0)]
    KeyPairFromSeed(String),
}

impl From<ed25519_dalek::SignatureError> for KeyPairCreationError {
    fn from(e: ed25519_dalek::SignatureError) -> Self { KeyPairCreationError::SignatureError(e) }
}

fn generate_keypair_from_slice(priv_key: &[u8]) -> Result<Keypair, MmError<KeyPairCreationError>> {
    let secret: [u8; 32] = priv_key
        .try_into()
        .map_to_mm(|_| KeyPairCreationError::KeyPairFromSeed("invalid ed25519 secret key length".to_string()))?;
    let signing_key = ed25519_dalek::SigningKey::from_bytes(&secret);
    solana_keypair::keypair_from_seed(signing_key.to_keypair_bytes().as_ref())
        .map_to_mm(|e| KeyPairCreationError::KeyPairFromSeed(e.to_string()))
}

/// The value of `hd_path_layout` that selects `<derivation_path>/N'/0'` (CRD ch.40 R40.1A.2). The default.
pub const SOLANA_HD_LAYOUT_BIP44_CHANGE: &str = "bip44_change";
/// The value of `hd_path_layout` that selects `<derivation_path>/N'` (CRD ch.40 R40.1A.2).
pub const SOLANA_HD_LAYOUT_BIP44: &str = "bip44";
/// The base path of a coin entry without `derivation_path` (SLIP-0044 coin type 501).
pub const SOLANA_DEFAULT_DERIVATION_PATH: &str = "m/44'/501'";

/// The `hd_path_layout` of a request: where the account level sits under an HD login (CRD ch.40 R40.1A.2).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum SolanaHdLayout {
    /// `<derivation_path>/N'/0'`
    #[default]
    Bip44Change,
    /// `<derivation_path>/N'`
    Bip44,
}

impl SolanaHdLayout {
    pub fn as_str(&self) -> &'static str {
        match self {
            SolanaHdLayout::Bip44Change => SOLANA_HD_LAYOUT_BIP44_CHANGE,
            SolanaHdLayout::Bip44 => SOLANA_HD_LAYOUT_BIP44,
        }
    }
}

impl<'de> serde::Deserialize<'de> for SolanaHdLayout {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = <String as serde::Deserialize>::deserialize(deserializer)?;
        match value.as_str() {
            SOLANA_HD_LAYOUT_BIP44_CHANGE => Ok(SolanaHdLayout::Bip44Change),
            SOLANA_HD_LAYOUT_BIP44 => Ok(SolanaHdLayout::Bip44),
            _ => Err(serde::de::Error::custom(format!(
                "Invalid `hd_path_layout`: expected `{SOLANA_HD_LAYOUT_BIP44_CHANGE}` or `{SOLANA_HD_LAYOUT_BIP44}`"
            ))),
        }
    }
}

/// One hardened level of a base path such as `44'`: the index without the hardened bit.
fn parse_hardened_level(level: &str) -> Option<u32> {
    let digits = level.strip_suffix('\'')?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u32>().ok().filter(|index| *index < 0x8000_0000)
}

/// The full derivation path of the platform key under a global-HD login (CRD ch.40 R40.1A.2): the coin entry's
/// `derivation_path` (default `m/44'/501'`), then the account level and, for [`SolanaHdLayout::Bip44Change`], a
/// change level `0'`. A base path that is not a two-level hardened path, or an account that cannot be hardened, is
/// an error naming the offending member.
pub fn solana_hd_derivation_path(conf: &Json, layout: SolanaHdLayout, account_id: u32) -> Result<String, String> {
    let base = match conf.get("derivation_path") {
        None | Some(Json::Null) => SOLANA_DEFAULT_DERIVATION_PATH,
        Some(Json::String(path)) => path.as_str(),
        Some(_) => return Err("Invalid `derivation_path`: expected a string".to_owned()),
    };
    let invalid = || {
        format!("Invalid `derivation_path` `{base}`: expected a two-level hardened path such as `{SOLANA_DEFAULT_DERIVATION_PATH}`")
    };
    let levels: Vec<&str> = base.strip_prefix("m/").ok_or_else(invalid)?.split('/').collect();
    if levels.len() != 2 || levels.iter().any(|level| parse_hardened_level(level).is_none()) {
        return Err(invalid());
    }
    if account_id >= 0x8000_0000 {
        return Err("Invalid `path_to_address`: `account_id` is out of range".to_owned());
    }
    Ok(match layout {
        SolanaHdLayout::Bip44Change => format!("{base}/{account_id}'/0'"),
        SolanaHdLayout::Bip44 => format!("{base}/{account_id}'"),
    })
}

/// The 32-byte ed25519 secret at `path` (SLIP-0010 over the wallet's BIP-39 seed); the Solana keypair is built from
/// it the same way as from the single key. The error text never carries key material.
pub fn solana_hd_secret(global_hd: &crypto::GlobalHDAccountArc, path: &str) -> Result<[u8; 32], String> {
    let derivation_path: ed25519_dalek_bip32::DerivationPath =
        path.parse().map_err(|_| format!("Invalid derivation path `{path}`"))?;
    let key = global_hd
        .derive_ed25519_signing_key(&derivation_path)
        .map_err(|_| format!("Could not derive the key at `{path}`"))?;
    Ok(key.signing_key.to_bytes())
}

pub async fn solana_coin_from_conf_and_params(
    ticker: &str,
    _conf: &Json,
    params: SolanaActivationParams,
    priv_key: &[u8],
) -> Result<SolanaCoin, String> {
    let urls = params.collected_urls();
    if urls.is_empty() {
        return Err("Solana activation requires at least one RPC endpoint (client_url or client_urls)".to_owned());
    }
    let client = SolanaRpcPool::with_commitment(urls, CommitmentConfig {
        commitment: params.confirmation_commitment,
    })
    .ok_or_else(|| "Solana RPC pool initialisation returned no clients".to_owned())?;
    // The platform has 9 decimals whatever the entry says (CRD ch.40 R40.6B.2); no top-level key is read.
    let decimals = SOLANA_DEFAULT_DECIMALS as u8;
    let key_pair = try_s!(generate_keypair_from_slice(priv_key));
    let my_address = key_pair.pubkey().to_string();
    let spl_tokens_infos = Arc::new(Mutex::new(HashMap::new()));
    let solana_coin = SolanaCoin(Arc::new(SolanaCoinImpl {
        my_address,
        key_pair,
        ticker: ticker.to_string(),
        client,
        decimals,
        spl_tokens_infos,
        settings: SolanaCoinSettings::default(),
    }));
    Ok(solana_coin)
}

/// pImpl idiom.
pub struct SolanaCoinImpl {
    pub(crate) ticker: String,
    pub(crate) key_pair: Keypair,
    pub(crate) client: SolanaRpcPool,
    pub(crate) decimals: u8,
    pub(crate) my_address: String,
    pub(crate) spl_tokens_infos: Arc<Mutex<HashMap<String, SplTokenInfo>>>,
    pub(crate) settings: SolanaCoinSettings,
}

/// The per-coin confirmation settings that `set_required_confirmations` and `set_requires_notarization` store and
/// `required_confirmations` and `requires_notarization` read back (CRD ch.40 §40.6B.5, D40.6B.1 option (b)). No
/// Solana code path waits on them; they exist so that the two RPCs answer and report what was set. Defaults: 1 and
/// `false`.
#[derive(Debug)]
pub struct SolanaCoinSettings {
    required_confirmations: AtomicU64,
    requires_notarization: AtomicBool,
}

impl Default for SolanaCoinSettings {
    fn default() -> Self {
        SolanaCoinSettings {
            required_confirmations: AtomicU64::new(1),
            requires_notarization: AtomicBool::new(false),
        }
    }
}

impl SolanaCoinSettings {
    pub fn required_confirmations(&self) -> u64 { self.required_confirmations.load(AtomicOrdering::Relaxed) }

    pub fn set_required_confirmations(&self, confirmations: u64) {
        self.required_confirmations
            .store(confirmations, AtomicOrdering::Relaxed)
    }

    pub fn requires_notarization(&self) -> bool { self.requires_notarization.load(AtomicOrdering::Relaxed) }

    pub fn set_requires_notarization(&self, requires_nota: bool) {
        self.requires_notarization.store(requires_nota, AtomicOrdering::Relaxed)
    }
}

impl Debug for SolanaCoinImpl {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult { f.write_str(&*self.ticker) }
}

#[derive(Clone, Debug)]
pub struct SolanaCoin(pub(crate) Arc<SolanaCoinImpl>);
impl Deref for SolanaCoin {
    type Target = SolanaCoinImpl;
    fn deref(&self) -> &SolanaCoinImpl { &*self.0 }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SolanaFeeDetails {
    pub amount: BigDecimal,
}
