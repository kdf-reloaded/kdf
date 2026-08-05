use super::*;

#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
pub enum MmCoinEnum {
    UtxoCoin(UtxoStandardCoin),
    QtumCoin(QtumCoin),
    Qrc20Coin(Qrc20Coin),
    EthCoin(EthCoin),
    #[cfg(not(target_arch = "wasm32"))]
    ZCoin(ZCoin),
    Bch(BchCoin),
    SlpToken(SlpToken),
    #[cfg(not(target_arch = "wasm32"))]
    SolanaCoin(SolanaCoin),
    #[cfg(not(target_arch = "wasm32"))]
    SplToken(SplToken),
    #[cfg(not(target_arch = "wasm32"))]
    LightningCoin(LightningCoin),
    SiaCoin(siacoin::SiaCoin),
    TendermintCoin(tendermint::TendermintCoin),
    TendermintToken(tendermint::TendermintToken),
    Test(TestCoin),
}
impl From<UtxoStandardCoin> for MmCoinEnum {
    fn from(c: UtxoStandardCoin) -> MmCoinEnum { MmCoinEnum::UtxoCoin(c) }
}
impl From<EthCoin> for MmCoinEnum {
    fn from(c: EthCoin) -> MmCoinEnum { MmCoinEnum::EthCoin(c) }
}
impl From<TestCoin> for MmCoinEnum {
    fn from(c: TestCoin) -> MmCoinEnum { MmCoinEnum::Test(c) }
}
#[cfg(not(target_arch = "wasm32"))]
impl From<SolanaCoin> for MmCoinEnum {
    fn from(c: SolanaCoin) -> MmCoinEnum { MmCoinEnum::SolanaCoin(c) }
}
#[cfg(not(target_arch = "wasm32"))]
impl From<SplToken> for MmCoinEnum {
    fn from(c: SplToken) -> MmCoinEnum { MmCoinEnum::SplToken(c) }
}
impl From<QtumCoin> for MmCoinEnum {
    fn from(coin: QtumCoin) -> Self { MmCoinEnum::QtumCoin(coin) }
}
impl From<Qrc20Coin> for MmCoinEnum {
    fn from(c: Qrc20Coin) -> MmCoinEnum { MmCoinEnum::Qrc20Coin(c) }
}
impl From<BchCoin> for MmCoinEnum {
    fn from(c: BchCoin) -> MmCoinEnum { MmCoinEnum::Bch(c) }
}
impl From<SlpToken> for MmCoinEnum {
    fn from(c: SlpToken) -> MmCoinEnum { MmCoinEnum::SlpToken(c) }
}
#[cfg(not(target_arch = "wasm32"))]
impl From<LightningCoin> for MmCoinEnum {
    fn from(c: LightningCoin) -> MmCoinEnum { MmCoinEnum::LightningCoin(c) }
}
#[cfg(not(target_arch = "wasm32"))]
impl From<ZCoin> for MmCoinEnum {
    fn from(c: ZCoin) -> MmCoinEnum { MmCoinEnum::ZCoin(c) }
}
impl From<siacoin::SiaCoin> for MmCoinEnum {
    fn from(c: siacoin::SiaCoin) -> MmCoinEnum { MmCoinEnum::SiaCoin(c) }
}
impl From<tendermint::TendermintCoin> for MmCoinEnum {
    fn from(c: tendermint::TendermintCoin) -> MmCoinEnum { MmCoinEnum::TendermintCoin(c) }
}
impl From<tendermint::TendermintToken> for MmCoinEnum {
    fn from(c: tendermint::TendermintToken) -> MmCoinEnum { MmCoinEnum::TendermintToken(c) }
}
impl Deref for MmCoinEnum {
    type Target = dyn MmCoin;
    fn deref(&self) -> &dyn MmCoin {
        match self {
            MmCoinEnum::UtxoCoin(ref c) => c,
            MmCoinEnum::QtumCoin(ref c) => c,
            MmCoinEnum::Qrc20Coin(ref c) => c,
            MmCoinEnum::EthCoin(ref c) => c,
            MmCoinEnum::Bch(ref c) => c,
            MmCoinEnum::SlpToken(ref c) => c,
            #[cfg(not(target_arch = "wasm32"))]
            MmCoinEnum::LightningCoin(ref c) => c,
            #[cfg(not(target_arch = "wasm32"))]
            MmCoinEnum::ZCoin(ref c) => c,
            MmCoinEnum::Test(ref c) => c,
            #[cfg(not(target_arch = "wasm32"))]
            MmCoinEnum::SolanaCoin(ref c) => c,
            #[cfg(not(target_arch = "wasm32"))]
            MmCoinEnum::SplToken(ref c) => c,
            MmCoinEnum::SiaCoin(ref c) => c,
            MmCoinEnum::TendermintCoin(ref c) => c,
            MmCoinEnum::TendermintToken(ref c) => c,
        }
    }
}
impl MmCoinEnum {
    pub fn is_utxo_in_native_mode(&self) -> bool {
        match self {
            MmCoinEnum::UtxoCoin(ref c) => c.as_ref().rpc_client.is_native(),
            MmCoinEnum::QtumCoin(ref c) => c.as_ref().rpc_client.is_native(),
            MmCoinEnum::Qrc20Coin(ref c) => c.as_ref().rpc_client.is_native(),
            MmCoinEnum::Bch(ref c) => c.as_ref().rpc_client.is_native(),
            MmCoinEnum::SlpToken(ref c) => c.as_ref().rpc_client.is_native(),
            #[cfg(all(not(target_arch = "wasm32"), feature = "zhtlc"))]
            MmCoinEnum::ZCoin(ref c) => c.as_ref().rpc_client.is_native(),
            _ => false,
        }
    }
}
pub struct CoinsContext {
    /// A map from a currency ticker symbol to the corresponding coin.
    /// Similar to `LP_coins`.
    pub(crate) coins: AsyncMutex<HashMap<String, MmCoinEnum>>,
    pub(crate) balance_update_handlers: AsyncMutex<Vec<Box<dyn BalanceTradeFeeUpdatedHandler + Send + Sync>>>,
    pub(crate) withdraw_task_manager: WithdrawTaskManagerShared,
    pub(crate) create_account_manager: CreateAccountTaskManagerShared,
    pub(crate) scan_addresses_manager: ScanAddressesTaskManagerShared,
    pub(crate) account_balance_task_manager: AccountBalanceTaskManagerShared,
    #[cfg(target_arch = "wasm32")]
    pub(crate) tx_history_db: SharedDb<TxHistoryDb>,
    #[cfg(target_arch = "wasm32")]
    pub(crate) hd_wallet_db: SharedDb<HDWalletDb>,
    #[cfg(target_arch = "wasm32")]
    pub(crate) block_headers_storage_db: SharedDb<BlockHeaderStorageDb>,
}
#[derive(Debug)]
pub struct CoinIsAlreadyActivatedErr {
    pub ticker: String,
}
#[derive(Debug)]
pub struct PlatformIsAlreadyActivatedErr {
    pub ticker: String,
}
impl CoinsContext {
    /// Obtains a reference to this crate context, creating it if necessary.
    pub fn from_ctx(ctx: &MmArc) -> Result<Arc<CoinsContext>, String> {
        Ok(try_s!(from_ctx(&ctx.coins_ctx, move || {
            Ok(CoinsContext {
                coins: AsyncMutex::new(HashMap::new()),
                balance_update_handlers: AsyncMutex::new(vec![]),
                withdraw_task_manager: WithdrawTaskManager::new_shared(),
                create_account_manager: CreateAccountTaskManager::new_shared(),
                scan_addresses_manager: ScanAddressesTaskManager::new_shared(),
                account_balance_task_manager: AccountBalanceTaskManager::new_shared(),
                #[cfg(target_arch = "wasm32")]
                tx_history_db: ConstructibleDb::new_shared(ctx),
                #[cfg(target_arch = "wasm32")]
                hd_wallet_db: ConstructibleDb::new_shared(ctx),
                #[cfg(target_arch = "wasm32")]
                block_headers_storage_db: ConstructibleDb::new_shared(ctx),
            })
        })))
    }

    pub async fn add_coin(&self, coin: MmCoinEnum) -> Result<(), MmError<CoinIsAlreadyActivatedErr>> {
        let mut coins = self.coins.lock().await;
        if coins.contains_key(coin.ticker()) {
            return MmError::err(CoinIsAlreadyActivatedErr {
                ticker: coin.ticker().into(),
            });
        }

        coins.insert(coin.ticker().into(), coin);
        Ok(())
    }

    pub async fn add_platform_with_tokens(
        &self,
        platform: MmCoinEnum,
        tokens: Vec<MmCoinEnum>,
    ) -> Result<(), MmError<PlatformIsAlreadyActivatedErr>> {
        let mut coins = self.coins.lock().await;
        if coins.contains_key(platform.ticker()) {
            return MmError::err(PlatformIsAlreadyActivatedErr {
                ticker: platform.ticker().into(),
            });
        }

        coins.insert(platform.ticker().into(), platform);

        // Tokens can't be activated without platform coin so we can safely insert them without checking prior existence
        for token in tokens {
            coins.insert(token.ticker().into(), token);
        }
        Ok(())
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) async fn tx_history_db(&self) -> TxHistoryResult<TxHistoryDbLocked<'_>> {
        Ok(self.tx_history_db.get_or_initialize().await.map_mm_err()?)
    }
}
/// This enum is used in coin activation requests.
#[derive(Copy, Clone, Debug, Deserialize, Serialize)]
pub enum PrivKeyActivationPolicy {
    /// Use whatever key policy the CryptoCtx was initialized with (Iguana or GlobalHD).
    ContextPrivKey,
    /// Force legacy Iguana single-key mode.
    IguanaPrivKey,
    Trezor,
}
impl PrivKeyActivationPolicy {
    /// The function can be used as a default deserialization constructor:
    /// `#[serde(default = "PrivKeyActivationPolicy::context_priv_key")]`
    pub fn context_priv_key() -> PrivKeyActivationPolicy { PrivKeyActivationPolicy::ContextPrivKey }

    /// The function can be used as a default deserialization constructor:
    /// `#[serde(default = "PrivKeyActivationPolicy::iguana_priv_key")]`
    pub fn iguana_priv_key() -> PrivKeyActivationPolicy { PrivKeyActivationPolicy::IguanaPrivKey }

    /// The function can be used as a default deserialization constructor:
    /// `#[serde(default = "PrivKeyActivationPolicy::trezor")]`
    pub fn trezor() -> PrivKeyActivationPolicy { PrivKeyActivationPolicy::Trezor }
}
#[derive(Debug)]
pub enum PrivKeyPolicy<T> {
    /// Legacy single-key (Iguana passphrase hashed to one key pair).
    KeyPair(T),
    /// HD wallet mode: derived key at a specific BIP44 path, with the root extended key
    /// available for deriving additional addresses.
    HDWallet {
        /// The key pair derived at the user's chosen address path.
        activated_key: T,
        /// The BIP32 root extended private key for deriving additional coin keys.
        bip39_secp_priv_key: bip32::ExtendedPrivateKey<secp256k1::SecretKey>,
    },
    Trezor,
}
impl<T> PrivKeyPolicy<T> {
    pub fn key_pair(&self) -> Option<&T> {
        match self {
            PrivKeyPolicy::KeyPair(key_pair) => Some(key_pair),
            PrivKeyPolicy::HDWallet { activated_key, .. } => Some(activated_key),
            PrivKeyPolicy::Trezor => None,
        }
    }

    pub fn key_pair_or_err(&self) -> Result<&T, MmError<PrivKeyNotAllowed>> {
        self.key_pair()
            .or_mm_err(|| PrivKeyNotAllowed::HardwareWalletNotSupported)
    }

    /// Returns true if this is an HD wallet policy.
    pub fn is_hd_wallet(&self) -> bool { matches!(self, PrivKeyPolicy::HDWallet { .. }) }
}
#[derive(Clone)]
pub enum PrivKeyBuildPolicy {
    IguanaPrivKey(IguanaPrivKey),
    GlobalHDAccount(GlobalHDAccountArc),
    Trezor,
}
impl PrivKeyBuildPolicy {
    /// Detects the `PrivKeyBuildPolicy` from the `CryptoCtx` key pair policy.
    pub fn detect_priv_key_policy(ctx: &MmArc) -> MmResult<PrivKeyBuildPolicy, CryptoCtxError> {
        let crypto_ctx = CryptoCtx::from_ctx(ctx)?;
        match crypto_ctx.key_pair_policy() {
            KeyPairPolicy::Iguana => Ok(PrivKeyBuildPolicy::IguanaPrivKey(
                crypto_ctx.mm2_internal_privkey_secret(),
            )),
            KeyPairPolicy::GlobalHDAccount(global_hd) => Ok(PrivKeyBuildPolicy::GlobalHDAccount(global_hd.clone())),
        }
    }
}
#[derive(Debug)]
pub enum DerivationMethod<Address, HDWallet> {
    Iguana(Address),
    HDWallet(HDWallet),
}
impl<Address, HDWallet> DerivationMethod<Address, HDWallet> {
    pub fn iguana(&self) -> Option<&Address> {
        match self {
            DerivationMethod::Iguana(my_address) => Some(my_address),
            DerivationMethod::HDWallet(_) => None,
        }
    }

    pub fn iguana_or_err(&self) -> MmResult<&Address, UnexpectedDerivationMethod> {
        self.iguana()
            .or_mm_err(|| UnexpectedDerivationMethod::IguanaPrivKeyUnavailable)
    }

    pub fn hd_wallet(&self) -> Option<&HDWallet> {
        match self {
            DerivationMethod::Iguana(_) => None,
            DerivationMethod::HDWallet(hd_wallet) => Some(hd_wallet),
        }
    }

    pub fn hd_wallet_or_err(&self) -> MmResult<&HDWallet, UnexpectedDerivationMethod> {
        self.hd_wallet()
            .or_mm_err(|| UnexpectedDerivationMethod::HDWalletUnavailable)
    }

    /// # Panic
    ///
    /// Panic if the address mode is [`DerivationMethod::HDWallet`].
    pub fn unwrap_iguana(&self) -> &Address { self.iguana_or_err().unwrap() }
}
#[allow(clippy::upper_case_acronyms)]
/// Deserialize a Tendermint `decimals` value, rejecting any value above 18 as
/// invalid protocol data (R36.3.1). The Cosmos base-denom-to-whole-coin scale is
/// bounded at 18 places; a higher value would make every balance/amount
/// conversion undefined, so it fails `protocol_data` parsing rather than
/// silently activating a mis-scaled coin.
fn deserialize_tendermint_decimals<'de, D>(deserializer: D) -> Result<u8, D::Error>
where
    D: Deserializer<'de>,
{
    let decimals = u8::deserialize(deserializer)?;
    if decimals > 18 {
        return Err(serde::de::Error::custom(format!(
            "Tendermint `decimals` must be 18 or lower, got {decimals}"
        )));
    }
    Ok(decimals)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "protocol_data")]
pub enum CoinProtocol {
    UTXO,
    QTUM,
    QRC20 {
        platform: String,
        contract_address: String,
    },
    ETH {
        /// Optional EVM chain id carried under `protocol_data` in newer coins
        /// configs. The coin builder reads the authoritative `chain_id` from the
        /// top-level coin config, so this field is accepted for
        /// forward-compatibility and is otherwise unused.
        #[serde(default)]
        chain_id: Option<u64>,
    },
    ERC20 {
        platform: String,
        contract_address: String,
    },
    SLPTOKEN {
        platform: String,
        token_id: H256Json,
        decimals: u8,
        required_confirmations: Option<u64>,
    },
    BCH {
        slp_prefix: String,
    },
    #[cfg(not(target_arch = "wasm32"))]
    LIGHTNING {
        platform: String,
        network: BlockchainNetwork,
        confirmations: PlatformCoinConfirmations,
    },
    #[cfg(not(target_arch = "wasm32"))]
    SOLANA,
    #[cfg(not(target_arch = "wasm32"))]
    SPLTOKEN {
        platform: String,
        token_contract_address: String,
        decimals: u8,
    },
    /// ZHTLC (Zcash-HTLC) shielded coin, e.g. ARRR/PIRATE and the ZOMBIE test
    /// coin. The variant carries a **required** `protocol_data` payload
    /// (R39.1.2): the Zcash `consensus_params`, an optional `check_point_block`
    /// sync anchor and an optional `z_derivation_path`. The shielded-coin
    /// builder sources all of its network parameters from this payload
    /// (R39.6.4). Because `consensus_params` is required, a bare
    /// `{"type":"ZHTLC"}` with no `protocol_data` is non-conformant and fails
    /// deserialization by design.
    #[cfg(not(target_arch = "wasm32"))]
    ZHTLC(ZcoinProtocolInfo),
    SIA,
    TENDERMINT {
        /// The platform chain's native base denomination (the smallest-unit bank
        /// denom, e.g. `uatom`, `uiris`, `uosmo`). It is the denom the platform
        /// coin queries for its own balance, denominates fees in, and signs
        /// bank/HTLC/IBC messages against (R36.3.1). Required.
        denom: String,
        /// The number of decimal places between the base denom and one whole
        /// coin, used to scale base-unit balances/amounts (R36.3.1). Required and
        /// bounded at 18; a value above 18 fails protocol-data parsing.
        #[serde(deserialize_with = "deserialize_tendermint_decimals")]
        decimals: u8,
        /// The bech32 human-readable prefix (HRP) of the chain's account
        /// addresses (e.g. `cosmos`, `iaa`, `osmo`). Required.
        account_prefix: String,
        /// The Cosmos/Tendermint chain identifier (e.g. `cosmoshub-4`). Required.
        chain_id: String,
        /// Map whose keys are a target chain's bech32 account-prefix (HRP) and
        /// whose values are the integer ICS-20 channel number `N` on this chain's
        /// transfer port toward that target (the integer `N` denoting the channel
        /// identifier `channel-N`). Seeds the configured destination-prefix ->
        /// channel resolution used by the IBC/HTLC layer (R36.3.1). Optional;
        /// defaults to an empty map when absent.
        #[serde(default)]
        ibc_channels: HashMap<String, u64>,
    },
    TENDERMINTTOKEN {
        platform: String,
        denom: String,
        decimals: u8,
    },
    /// Native TRON coin (TRX). Carries the network identity so the
    /// activation layer can pick the right set of full-node URLs and
    /// chain parameters.
    TRX {
        #[serde(default)]
        network: crate::eth::tron::Network,
    },
    /// TRC20 token deployed on TRON. `platform` is the parent TRX coin
    /// ticker; `contract_address` is the on-chain TRC20 contract in
    /// hex (with `0x41` prefix) or Base58Check.
    TRC20 {
        platform: String,
        contract_address: String,
    },
    /// NFT entry in the coins config (e.g. `NFT_ETH`). NFT support is
    /// activated through the dedicated `enable_nft` RPC method, not
    /// through `electrum`/`enable`. This variant exists so that startup
    /// config parsing does not fail with a confusing serde error when an
    /// NFT entry is present; `lp_coininit` rejects it with a helpful
    /// message directing callers to `enable_nft`.
    NFT {
        /// The parent EVM platform coin ticker (e.g. `"ETH"`).
        platform: String,
    },
}

impl CoinProtocol {
    /// Deserialize a coin's `protocol` config value, tolerating the standard
    /// `{"type": "ETH"}` form that omits `protocol_data`.
    ///
    /// `CoinProtocol` uses adjacent tagging (`content = "protocol_data"`), and the
    /// `ETH` variant is a struct variant (`chain_id`), so serde otherwise rejects a
    /// bare `{"type": "ETH"}` with `missing field protocol_data` even though its only
    /// field is optional. Backfill an empty `protocol_data` for exactly that case and
    /// defer to the derived deserialization (all other variants are untouched), so
    /// both `{"type": "ETH"}` and `{"type": "ETH", "protocol_data": {"chain_id": N}}`
    /// parse. This is the canonical way to parse a coin `protocol` from config.
    pub fn from_conf_json(mut protocol: serde_json::Value) -> serde_json::Result<CoinProtocol> {
        let is_eth = protocol.get("type").and_then(serde_json::Value::as_str) == Some("ETH");
        if is_eth && protocol.get("protocol_data").is_none() {
            if let Some(obj) = protocol.as_object_mut() {
                obj.insert(
                    "protocol_data".to_owned(),
                    serde_json::Value::Object(serde_json::Map::new()),
                );
            }
        }
        serde_json::from_value(protocol)
    }
}

pub enum RpcClientType {
    Native,
    Electrum,
    Ethereum,
}
impl ToString for RpcClientType {
    fn to_string(&self) -> String {
        match self {
            RpcClientType::Native => "native".into(),
            RpcClientType::Electrum => "electrum".into(),
            RpcClientType::Ethereum => "ethereum".into(),
        }
    }
}
#[derive(Clone)]
pub struct CoinTransportMetrics {
    /// Using a weak reference by default in order to avoid circular references and leaks.
    pub(crate) metrics: MetricsWeak,
    /// Name of coin the rpc client is intended to work with.
    pub(crate) ticker: String,
    /// RPC client type.
    pub(crate) client: String,
}
impl CoinTransportMetrics {
    pub(crate) fn new(metrics: MetricsWeak, ticker: String, client: RpcClientType) -> CoinTransportMetrics {
        CoinTransportMetrics {
            metrics,
            ticker,
            client: client.to_string(),
        }
    }

    pub(crate) fn into_shared(self) -> RpcTransportEventHandlerShared { Arc::new(self) }
}
impl RpcTransportEventHandler for CoinTransportMetrics {
    fn debug_info(&self) -> String { "CoinTransportMetrics".into() }

    fn on_outgoing_request(&self, data: &[u8]) {
        mm_counter!(self.metrics, "rpc_client.traffic.out", data.len() as u64,
            "coin" => self.ticker.clone(), "client" => self.client.clone());
        mm_counter!(self.metrics, "rpc_client.request.count", 1,
            "coin" => self.ticker.clone(), "client" => self.client.clone());
    }

    fn on_incoming_response(&self, data: &[u8]) {
        mm_counter!(self.metrics, "rpc_client.traffic.in", data.len() as u64,
            "coin" => self.ticker.clone(), "client" => self.client.clone());
        mm_counter!(self.metrics, "rpc_client.response.count", 1,
            "coin" => self.ticker.clone(), "client" => self.client.clone());
    }

    fn on_connected(&self, _address: String) -> Result<(), String> {
        // Handle a new connected endpoint if necessary.
        // Now just return the Ok
        Ok(())
    }
}
#[async_trait]
impl BalanceTradeFeeUpdatedHandler for CoinsContext {
    async fn balance_updated(&self, coin: &MmCoinEnum, new_balance: &BigDecimal) {
        for sub in self.balance_update_handlers.lock().await.iter() {
            sub.balance_updated(coin, new_balance).await
        }
    }
}

#[cfg(test)]
mod coin_protocol_tests {
    use super::CoinProtocol;
    use serde_json::json;

    #[test]
    fn eth_protocol_accepts_bare_and_explicit_protocol_data() {
        // The standard `{"type":"ETH"}` form (no protocol_data) must parse.
        match CoinProtocol::from_conf_json(json!({"type": "ETH"})).unwrap() {
            CoinProtocol::ETH { chain_id } => assert_eq!(chain_id, None),
            other => panic!("expected ETH, got {:?}", other),
        }
        // Explicit empty protocol_data.
        match CoinProtocol::from_conf_json(json!({"type": "ETH", "protocol_data": {}})).unwrap() {
            CoinProtocol::ETH { chain_id } => assert_eq!(chain_id, None),
            other => panic!("expected ETH, got {:?}", other),
        }
        // protocol_data carrying chain_id is preserved.
        match CoinProtocol::from_conf_json(json!({"type": "ETH", "protocol_data": {"chain_id": 137}})).unwrap() {
            CoinProtocol::ETH { chain_id } => assert_eq!(chain_id, Some(137)),
            other => panic!("expected ETH, got {:?}", other),
        }
    }

    #[test]
    fn other_variants_are_unaffected() {
        // A struct variant with required fields still needs its protocol_data.
        assert!(CoinProtocol::from_conf_json(json!({"type": "ERC20"})).is_err());
        CoinProtocol::from_conf_json(json!({
            "type": "ERC20",
            "protocol_data": {"platform": "ETH", "contract_address": "0x0"}
        }))
        .unwrap();
        // A unit variant still parses.
        assert!(matches!(
            CoinProtocol::from_conf_json(json!({"type": "UTXO"})).unwrap(),
            CoinProtocol::UTXO
        ));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn zhtlc_requires_full_protocol_data() {
        // Bare ZHTLC (no protocol_data) is non-conformant now that the arm carries
        // a required payload whose required member is `consensus_params` (R39.1.2).
        assert!(CoinProtocol::from_conf_json(json!({"type": "ZHTLC"})).is_err());
        // A ZHTLC whose protocol_data omits the required consensus_params also fails.
        assert!(CoinProtocol::from_conf_json(json!({"type": "ZHTLC", "protocol_data": {}})).is_err());
        // Production ZHTLC (ARRR/PIRATE) ships consensus params, a checkpoint block
        // and the z-derivation path under protocol_data; all of it must be accepted.
        let arrr = json!({
            "type": "ZHTLC",
            "protocol_data": {
                "consensus_params": {
                    "overwinter_activation_height": 152855,
                    "sapling_activation_height": 152855,
                    "blossom_activation_height": null,
                    "heartwood_activation_height": null,
                    "canopy_activation_height": null,
                    "coin_type": 133,
                    "hrp_sapling_extended_spending_key": "secret-extended-key-main",
                    "hrp_sapling_extended_full_viewing_key": "zxviews",
                    "hrp_sapling_payment_address": "zs",
                    "b58_pubkey_address_prefix": [28, 184],
                    "b58_script_address_prefix": [28, 189]
                },
                "check_point_block": {
                    "height": 1900000,
                    "time": 1652512363,
                    "hash": "44797f3bb78323a7717007f1e289a3689e0b5b3433385dbd8e6f6a1700000000",
                    "sapling_tree": "01e40c26f4"
                },
                "z_derivation_path": "m/32'/141'"
            }
        });
        match CoinProtocol::from_conf_json(arrr).unwrap() {
            CoinProtocol::ZHTLC(info) => {
                use zcash_protocol::consensus::NetworkConstants;
                assert!(info.check_point_block.is_some());
                assert!(info.z_derivation_path.is_some());
                assert_eq!(info.consensus_params.hrp_sapling_payment_address(), "zs");
                assert_eq!(info.consensus_params.coin_type(), 133);
            },
            other => panic!("expected ZHTLC, got {:?}", other),
        }
    }

    #[test]
    fn tendermint_protocol_data_parses_full_config() {
        // A full TENDERMINT protocol_data with denom/decimals/account_prefix/
        // chain_id and a configured ibc_channels map parses, and surplus benign
        // keys (gas_price, chain_registry_name, and forward-compat tuning hints)
        // are accepted and ignored -- no deny_unknown_fields (R36.3.1/R36.3.3).
        let conf = json!({
            "type": "TENDERMINT",
            "protocol_data": {
                "denom": "uatom",
                "decimals": 6,
                "account_prefix": "cosmos",
                "chain_id": "cosmoshub-4",
                "gas_price": 0.025,
                "chain_registry_name": "cosmoshub",
                "ibc_channels": {"osmo": 141, "iaa": 0},
                "min_balance_for_ibc_routing": 1000
            }
        });
        match CoinProtocol::from_conf_json(conf).unwrap() {
            CoinProtocol::TENDERMINT {
                denom,
                decimals,
                account_prefix,
                chain_id,
                ibc_channels,
            } => {
                assert_eq!(denom, "uatom");
                assert_eq!(decimals, 6);
                assert_eq!(account_prefix, "cosmos");
                assert_eq!(chain_id, "cosmoshub-4");
                assert_eq!(ibc_channels.get("osmo"), Some(&141));
                assert_eq!(ibc_channels.get("iaa"), Some(&0));
            },
            other => panic!("expected TENDERMINT, got {:?}", other),
        }
    }

    #[test]
    fn tendermint_protocol_data_defaults_ibc_channels_empty() {
        // ibc_channels is optional and defaults to an empty map (R36.3.1).
        match CoinProtocol::from_conf_json(json!({
            "type": "TENDERMINT",
            "protocol_data": {
                "denom": "uiris",
                "decimals": 6,
                "account_prefix": "iaa",
                "chain_id": "irishub-1"
            }
        }))
        .unwrap()
        {
            CoinProtocol::TENDERMINT { ibc_channels, .. } => assert!(ibc_channels.is_empty()),
            other => panic!("expected TENDERMINT, got {:?}", other),
        }
    }

    #[test]
    fn tendermint_protocol_data_requires_denom_and_decimals() {
        // denom and decimals are required members of TENDERMINT protocol_data.
        assert!(CoinProtocol::from_conf_json(json!({
            "type": "TENDERMINT",
            "protocol_data": {"decimals": 6, "account_prefix": "cosmos", "chain_id": "cosmoshub-4"}
        }))
        .is_err());
        assert!(CoinProtocol::from_conf_json(json!({
            "type": "TENDERMINT",
            "protocol_data": {"denom": "uatom", "account_prefix": "cosmos", "chain_id": "cosmoshub-4"}
        }))
        .is_err());
    }

    #[test]
    fn tendermint_protocol_data_rejects_decimals_above_18() {
        // decimals must be 18 or lower; a higher value fails protocol-data parsing
        // as a bounded-input check (R36.3.1).
        assert!(CoinProtocol::from_conf_json(json!({
            "type": "TENDERMINT",
            "protocol_data": {
                "denom": "uatom",
                "decimals": 19,
                "account_prefix": "cosmos",
                "chain_id": "cosmoshub-4"
            }
        }))
        .is_err());
        // The boundary value 18 is accepted.
        assert!(CoinProtocol::from_conf_json(json!({
            "type": "TENDERMINT",
            "protocol_data": {
                "denom": "aevmos",
                "decimals": 18,
                "account_prefix": "evmos",
                "chain_id": "evmos_9001-2"
            }
        }))
        .is_ok());
    }

    #[test]
    fn nft_protocol_parses_and_carries_platform() {
        // A GLEEC-style NFT config entry {"type":"NFT","protocol_data":{"platform":"ETH"}}
        // must deserialize without error so that startup config loading and
        // from_conf_json callers don't see a confusing serde error (ch.19 §19.11).
        match CoinProtocol::from_conf_json(json!({
            "type": "NFT",
            "protocol_data": {"platform": "ETH"}
        }))
        .unwrap()
        {
            CoinProtocol::NFT { platform } => assert_eq!(platform, "ETH"),
            other => panic!("expected NFT, got {:?}", other),
        }
    }
}
