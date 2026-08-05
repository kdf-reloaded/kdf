use super::*;
use crate::my_tx_history_v2::{TxDetailsBuilder, TxHistoryStorage, TxHistoryStorageError};
use crate::utxo::rpc_clients::UtxoRpcFut;
use crate::utxo::slp::{parse_slp_script, ParseSlpScriptError, SlpGenesisParams, SlpTokenInfo, SlpTransaction,
                       SlpUnspent};
use crate::utxo::utxo_builder::{UtxoArcBuilder, UtxoCoinBuilder};
use crate::utxo::utxo_common::big_decimal_from_sat_unsigned;
use crate::{BlockHeightAndTime, CanRefundHtlc, CoinBalance, CoinProtocol, DexFee, NegotiateSwapContractAddrErr,
            PrivKeyBuildPolicy, RawTransactionFut, RawTransactionRequest, SignRawTransactionRequest, SignatureResult,
            SwapOps, TradePreimageValue, TransactionFut, TransactionType, TxFeeDetails, UnexpectedDerivationMethod,
            ValidateAddressResult, ValidateFeeArgs, ValidatePaymentInput, VerificationResult, WatcherOps, WithdrawFut};
use common::log::warn;
use common::mm_number::MmNumber;
use derive_more::Display;
use futures::{FutureExt, TryFutureExt};
use itertools::Either as EitherIter;
use keys::hash::H256;
use keys::CashAddress;
pub use keys::NetworkPrefix as CashAddrPrefix;
use mm2_metrics::MetricsArc;
use serde_json::{self as json, Value as Json};
use serialization::{deserialize, CoinVariant};
use std::sync::MutexGuard;

pub type BchUnspentMap = HashMap<Address, BchUnspents>;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BchActivationRequest {
    #[serde(default)]
    allow_slp_unsafe_conf: bool,
    bchd_urls: Vec<String>,
    #[serde(flatten)]
    pub utxo_params: UtxoActivationParams,
}

#[derive(Debug, Display)]
pub enum BchFromLegacyReqErr {
    InvalidUtxoParams(UtxoFromLegacyReqErr),
    InvalidBchdUrls(json::Error),
}

impl From<UtxoFromLegacyReqErr> for BchFromLegacyReqErr {
    fn from(err: UtxoFromLegacyReqErr) -> Self { BchFromLegacyReqErr::InvalidUtxoParams(err) }
}

impl BchActivationRequest {
    pub fn from_legacy_req(req: &Json) -> Result<Self, MmError<BchFromLegacyReqErr>> {
        let bchd_urls = json::from_value(req["bchd_urls"].clone()).map_to_mm(BchFromLegacyReqErr::InvalidBchdUrls)?;
        let allow_slp_unsafe_conf = req["allow_slp_unsafe_conf"].as_bool().unwrap_or_default();
        let utxo_params = UtxoActivationParams::from_legacy_req(req).mm_err(Into::into)?;

        Ok(BchActivationRequest {
            allow_slp_unsafe_conf,
            bchd_urls,
            utxo_params,
        })
    }
}

#[derive(Clone, Debug)]
pub struct BchCoin {
    utxo_arc: UtxoArc,
    slp_addr_prefix: CashAddrPrefix,
    bchd_urls: Vec<String>,
    slp_tokens_infos: Arc<Mutex<HashMap<String, SlpTokenInfo>>>,
}

#[allow(clippy::large_enum_variant)]
pub enum IsSlpUtxoError {
    Rpc(UtxoRpcError),
    TxDeserialization(serialization::Error),
}

#[derive(Debug, Default)]
pub struct BchUnspents {
    /// Standard BCH UTXOs
    standard: Vec<UnspentInfo>,
    /// SLP related UTXOs
    slp: HashMap<H256, Vec<SlpUnspent>>,
    /// SLP minting batons outputs, DO NOT use them as MM2 doesn't support SLP minting by default
    slp_batons: Vec<UnspentInfo>,
    /// The unspents of transaction with an undetermined protocol (OP_RETURN in 0 output but not SLP)
    /// DO NOT ever use them to avoid burning users funds
    undetermined: Vec<UnspentInfo>,
}

impl BchUnspents {
    fn add_standard(&mut self, utxo: UnspentInfo) { self.standard.push(utxo) }

    fn add_slp(&mut self, token_id: H256, bch_unspent: UnspentInfo, slp_amount: u64) {
        let slp_unspent = SlpUnspent {
            bch_unspent,
            slp_amount,
        };
        self.slp.entry(token_id).or_insert_with(Vec::new).push(slp_unspent);
    }

    fn add_slp_baton(&mut self, utxo: UnspentInfo) { self.slp_batons.push(utxo) }

    fn add_undetermined(&mut self, utxo: UnspentInfo) { self.undetermined.push(utxo) }

    pub fn platform_balance(&self, decimals: u8) -> CoinBalance {
        let spendable_sat = total_unspent_value(&self.standard);

        let unspendable_slp = self.slp.iter().fold(0, |cur, (_, slp_unspents)| {
            let bch_value = total_unspent_value(slp_unspents.iter().map(|slp| &slp.bch_unspent));
            cur + bch_value
        });

        let unspendable_slp_batons = total_unspent_value(&self.slp_batons);
        let unspendable_undetermined = total_unspent_value(&self.undetermined);

        let total_unspendable = unspendable_slp + unspendable_slp_batons + unspendable_undetermined;
        CoinBalance {
            spendable: big_decimal_from_sat_unsigned(spendable_sat, decimals),
            unspendable: big_decimal_from_sat_unsigned(total_unspendable, decimals),
        }
    }

    pub fn slp_token_balance(&self, token_id: &H256, decimals: u8) -> CoinBalance {
        self.slp
            .get(token_id)
            .map(|unspents| {
                let total_sat = unspents.iter().fold(0, |cur, unspent| cur + unspent.slp_amount);
                CoinBalance {
                    spendable: big_decimal_from_sat_unsigned(total_sat, decimals),
                    unspendable: 0.into(),
                }
            })
            .unwrap_or_default()
    }
}

impl From<UtxoRpcError> for IsSlpUtxoError {
    fn from(err: UtxoRpcError) -> IsSlpUtxoError { IsSlpUtxoError::Rpc(err) }
}

impl From<serialization::Error> for IsSlpUtxoError {
    fn from(err: serialization::Error) -> IsSlpUtxoError { IsSlpUtxoError::TxDeserialization(err) }
}

#[path = "bch/bch_coin_ops.rs"] mod bch_coin_ops;

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum GetTxDetailsError<E: TxHistoryStorageError> {
    StorageError(E),
    AddressesFromScriptError(String),
    SlpTokenIdIsNotGenesisTx(H256),
    TxDeserializationError(serialization::Error),
    RpcError(UtxoRpcError),
    ParseSlpScriptError(ParseSlpScriptError),
    ToSlpAddressError(String),
    InvalidSlpTransaction(H256),
    AddressDerivationError(UnexpectedDerivationMethod),
}

impl<E: TxHistoryStorageError> From<UtxoRpcError> for GetTxDetailsError<E> {
    fn from(err: UtxoRpcError) -> Self { GetTxDetailsError::RpcError(err) }
}

impl<E: TxHistoryStorageError> From<E> for GetTxDetailsError<E> {
    fn from(err: E) -> Self { GetTxDetailsError::StorageError(err) }
}

impl<E: TxHistoryStorageError> From<serialization::Error> for GetTxDetailsError<E> {
    fn from(err: serialization::Error) -> Self { GetTxDetailsError::TxDeserializationError(err) }
}

impl<E: TxHistoryStorageError> From<ParseSlpScriptError> for GetTxDetailsError<E> {
    fn from(err: ParseSlpScriptError) -> Self { GetTxDetailsError::ParseSlpScriptError(err) }
}

impl<E: TxHistoryStorageError> From<UnexpectedDerivationMethod> for GetTxDetailsError<E> {
    fn from(err: UnexpectedDerivationMethod) -> Self { GetTxDetailsError::AddressDerivationError(err) }
}

impl AsRef<UtxoCoinFields> for BchCoin {
    fn as_ref(&self) -> &UtxoCoinFields { &self.utxo_arc }
}

pub async fn bch_coin_from_conf_and_params(
    ctx: &MmArc,
    ticker: &str,
    conf: &Json,
    params: BchActivationRequest,
    slp_addr_prefix: CashAddrPrefix,
    priv_key: &[u8],
) -> Result<BchCoin, String> {
    if params.bchd_urls.is_empty() && !params.allow_slp_unsafe_conf {
        return Err("Using empty bchd_urls is unsafe for SLP users!".into());
    }

    let bchd_urls = params.bchd_urls;
    let slp_tokens_infos = Arc::new(Mutex::new(HashMap::new()));
    let constructor = {
        move |utxo_arc| BchCoin {
            utxo_arc,
            slp_addr_prefix: slp_addr_prefix.clone(),
            bchd_urls: bchd_urls.clone(),
            slp_tokens_infos: slp_tokens_infos.clone(),
        }
    };

    let priv_key_policy = PrivKeyBuildPolicy::IguanaPrivKey(keys::Secret::from(priv_key));
    let coin = try_s!(
        UtxoArcBuilder::new(ctx, ticker, conf, &params.utxo_params, priv_key_policy, constructor)
            .build()
            .await
    );
    Ok(coin)
}

#[derive(Debug)]
pub enum BchActivationError {
    CoinInitError(String),
    TokenConfIsNotFound {
        token: String,
    },
    TokenCoinProtocolParseError {
        token: String,
        error: json::Error,
    },
    TokenCoinProtocolIsNotSlp {
        token: String,
        protocol: CoinProtocol,
    },
    TokenPlatformCoinIsInvalidInConf {
        token: String,
        expected_platform: String,
        actual_platform: String,
    },
    RpcError(UtxoRpcError),
    SlpPrefixParseError(String),
}

impl From<UtxoRpcError> for BchActivationError {
    fn from(e: UtxoRpcError) -> Self { BchActivationError::RpcError(e) }
}

// if mockable is placed before async_trait there is `munmap_chunk(): invalid pointer` error on async fn mocking attempt
#[async_trait]
#[cfg_attr(test, mockable)]
impl UtxoTxBroadcastOps for BchCoin {
    async fn broadcast_tx(&self, tx: &UtxoTx) -> Result<H256Json, MmError<BroadcastTxErr>> {
        utxo_common::broadcast_tx(self, tx).await
    }
}

// if mockable is placed before async_trait there is `munmap_chunk(): invalid pointer` error on async fn mocking attempt
#[async_trait]
#[cfg_attr(test, mockable)]
impl UtxoTxGenerationOps for BchCoin {
    async fn get_tx_fee(&self) -> UtxoRpcResult<ActualTxFee> { utxo_common::get_tx_fee(&self.utxo_arc).await }

    async fn calc_interest_if_required(
        &self,
        unsigned: TransactionInputSigner,
        data: AdditionalTxData,
        my_script_pub: Bytes,
    ) -> UtxoRpcResult<(TransactionInputSigner, AdditionalTxData)> {
        utxo_common::calc_interest_if_required(self, unsigned, data, my_script_pub).await
    }
}

#[async_trait]
#[cfg_attr(test, mockable)]
impl GetUtxoListOps for BchCoin {
    async fn get_unspent_ordered_list(
        &self,
        address: &Address,
    ) -> UtxoRpcResult<(Vec<UnspentInfo>, RecentlySpentOutPointsGuard<'_>)> {
        let (bch_unspents, recently_spent) = self.bch_unspents_for_spend(address).await?;
        Ok((bch_unspents.standard, recently_spent))
    }

    async fn get_all_unspent_ordered_list(
        &self,
        address: &Address,
    ) -> UtxoRpcResult<(Vec<UnspentInfo>, RecentlySpentOutPointsGuard<'_>)> {
        utxo_common::get_all_unspent_ordered_list(self, address).await
    }

    async fn get_mature_unspent_ordered_list(
        &self,
        address: &Address,
    ) -> UtxoRpcResult<(MatureUnspentList, RecentlySpentOutPointsGuard<'_>)> {
        let (unspents, recently_spent) = utxo_common::get_all_unspent_ordered_list(self, address).await?;
        Ok((MatureUnspentList::new_mature(unspents), recently_spent))
    }
}

// if mockable is placed before async_trait there is `munmap_chunk(): invalid pointer` error on async fn mocking attempt
#[async_trait]
#[cfg_attr(test, mockable)]
impl UtxoCommonOps for BchCoin {
    async fn get_htlc_spend_fee(&self, tx_size: u64) -> UtxoRpcResult<u64> {
        utxo_common::get_htlc_spend_fee(self, tx_size).await
    }

    fn addresses_from_script(&self, script: &Script) -> Result<Vec<Address>, String> {
        utxo_common::addresses_from_script(self, script)
    }

    fn denominate_satoshis(&self, satoshi: i64) -> f64 { utxo_common::denominate_satoshis(&self.utxo_arc, satoshi) }

    fn my_public_key(&self) -> Result<&Public, MmError<UnexpectedDerivationMethod>> {
        utxo_common::my_public_key(self.as_ref())
    }

    fn address_from_str(&self, address: &str) -> Result<Address, String> {
        utxo_common::checked_address_from_str(self, address)
    }

    async fn get_current_mtp(&self) -> UtxoRpcResult<u32> {
        utxo_common::get_current_mtp(&self.utxo_arc, CoinVariant::Standard).await
    }

    fn is_unspent_mature(&self, output: &RpcTransaction) -> bool {
        utxo_common::is_unspent_mature(self.utxo_arc.conf.mature_confirmations, output)
    }

    async fn calc_interest_of_tx(&self, tx: &UtxoTx, input_transactions: &mut HistoryUtxoTxMap) -> UtxoRpcResult<u64> {
        utxo_common::calc_interest_of_tx(self, tx, input_transactions).await
    }

    async fn get_mut_verbose_transaction_from_map_or_rpc<'a, 'b>(
        &'a self,
        tx_hash: H256Json,
        utxo_tx_map: &'b mut HistoryUtxoTxMap,
    ) -> UtxoRpcResult<&'b mut HistoryUtxoTx> {
        utxo_common::get_mut_verbose_transaction_from_map_or_rpc(self, tx_hash, utxo_tx_map).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn p2sh_spending_tx(
        &self,
        prev_transaction: UtxoTx,
        redeem_script: Bytes,
        outputs: Vec<TransactionOutput>,
        script_data: Script,
        sequence: u32,
        lock_time: u32,
        keypair: &KeyPair,
    ) -> Result<UtxoTx, String> {
        utxo_common::p2sh_spending_tx(
            self,
            prev_transaction,
            redeem_script,
            outputs,
            script_data,
            sequence,
            lock_time,
            keypair,
        )
        .await
    }

    fn get_verbose_transactions_from_cache_or_rpc(
        &self,
        tx_ids: HashSet<H256Json>,
    ) -> UtxoRpcFut<HashMap<H256Json, VerboseTransactionFrom>> {
        let selfi = self.clone();
        let fut = async move { utxo_common::get_verbose_transactions_from_cache_or_rpc(&selfi.utxo_arc, tx_ids).await };
        Box::new(fut.boxed().compat())
    }

    async fn preimage_trade_fee_required_to_send_outputs(
        &self,
        outputs: Vec<TransactionOutput>,
        fee_policy: FeePolicy,
        gas_fee: Option<u64>,
        stage: &FeeApproxStage,
    ) -> TradePreimageResult<BigDecimal> {
        utxo_common::preimage_trade_fee_required_to_send_outputs(self, outputs, fee_policy, gas_fee, stage).await
    }

    fn increase_dynamic_fee_by_stage(&self, dynamic_fee: u64, stage: &FeeApproxStage) -> u64 {
        utxo_common::increase_dynamic_fee_by_stage(self, dynamic_fee, stage)
    }

    async fn p2sh_tx_locktime(&self, htlc_locktime: u32) -> Result<u32, MmError<UtxoRpcError>> {
        utxo_common::p2sh_tx_locktime(self, &self.utxo_arc.conf.ticker, htlc_locktime).await
    }

    fn addr_format(&self) -> &UtxoAddressFormat { utxo_common::addr_format(self) }

    fn addr_format_for_standard_scripts(&self) -> UtxoAddressFormat {
        utxo_common::addr_format_for_standard_scripts(self)
    }

    fn address_from_pubkey(&self, pubkey: &Public) -> Address {
        let conf = &self.utxo_arc.conf;
        let addr_format = self.addr_format().clone();
        utxo_common::address_from_pubkey(
            pubkey,
            conf.pub_addr_prefix,
            conf.pub_t_addr_prefix,
            conf.checksum_type,
            conf.bech32_hrp.clone(),
            addr_format,
        )
    }
}

#[async_trait]
impl SwapOps for BchCoin {
    fn send_taker_fee(&self, dex_fee: &DexFee, fee_addr: &[u8], _uuid: &[u8]) -> TransactionFut {
        utxo_common::send_taker_fee(self.clone(), dex_fee, fee_addr)
    }

    fn send_maker_payment(
        &self,
        time_lock: u32,
        maker_pub: &[u8],
        taker_pub: &[u8],
        secret_hash: &[u8],
        amount: BigDecimal,
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        utxo_common::send_maker_payment(self.clone(), time_lock, maker_pub, taker_pub, secret_hash, amount)
    }

    fn send_taker_payment(
        &self,
        time_lock: u32,
        taker_pub: &[u8],
        maker_pub: &[u8],
        secret_hash: &[u8],
        amount: BigDecimal,
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        utxo_common::send_taker_payment(self.clone(), time_lock, taker_pub, maker_pub, secret_hash, amount)
    }

    fn send_maker_spends_taker_payment(
        &self,
        taker_payment_tx: &[u8],
        time_lock: u32,
        taker_pub: &[u8],
        secret: &[u8],
        htlc_privkey: &[u8],
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        utxo_common::send_maker_spends_taker_payment(
            self.clone(),
            taker_payment_tx,
            time_lock,
            taker_pub,
            secret,
            htlc_privkey,
        )
    }

    fn send_taker_spends_maker_payment(
        &self,
        maker_payment_tx: &[u8],
        time_lock: u32,
        maker_pub: &[u8],
        secret: &[u8],
        htlc_privkey: &[u8],
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        utxo_common::send_taker_spends_maker_payment(
            self.clone(),
            maker_payment_tx,
            time_lock,
            maker_pub,
            secret,
            htlc_privkey,
        )
    }

    fn send_taker_refunds_payment(
        &self,
        taker_tx: &[u8],
        time_lock: u32,
        maker_pub: &[u8],
        secret_hash: &[u8],
        htlc_privkey: &[u8],
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        utxo_common::send_taker_refunds_payment(self.clone(), taker_tx, time_lock, maker_pub, secret_hash, htlc_privkey)
    }

    fn send_maker_refunds_payment(
        &self,
        maker_tx: &[u8],
        time_lock: u32,
        taker_pub: &[u8],
        secret_hash: &[u8],
        htlc_privkey: &[u8],
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        utxo_common::send_maker_refunds_payment(self.clone(), maker_tx, time_lock, taker_pub, secret_hash, htlc_privkey)
    }

    fn validate_fee(&self, args: ValidateFeeArgs<'_>) -> Box<dyn Future<Item = (), Error = String> + Send> {
        let tx = match args.fee_tx {
            TransactionEnum::UtxoTx(tx) => tx.clone(),
            _ => panic!(),
        };
        utxo_common::validate_fee(
            self.clone(),
            tx,
            utxo_common::DEFAULT_FEE_VOUT,
            args.expected_sender,
            args.dex_fee,
            args.min_block_number,
            args.fee_addr,
        )
    }

    fn validate_maker_payment(&self, input: ValidatePaymentInput) -> Box<dyn Future<Item = (), Error = String> + Send> {
        utxo_common::validate_maker_payment(self, input)
    }

    fn validate_taker_payment(&self, input: ValidatePaymentInput) -> Box<dyn Future<Item = (), Error = String> + Send> {
        utxo_common::validate_taker_payment(self, input)
    }

    fn check_if_my_payment_sent(
        &self,
        time_lock: u32,
        my_pub: &[u8],
        other_pub: &[u8],
        secret_hash: &[u8],
        _search_from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
    ) -> Box<dyn Future<Item = Option<TransactionEnum>, Error = String> + Send> {
        utxo_common::check_if_my_payment_sent(self.clone(), time_lock, my_pub, other_pub, secret_hash)
    }

    async fn search_for_swap_tx_spend_my(
        &self,
        time_lock: u32,
        other_pub: &[u8],
        secret_hash: &[u8],
        tx: &[u8],
        search_from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
    ) -> Result<Option<FoundSwapTxSpend>, String> {
        utxo_common::search_for_swap_tx_spend_my(
            &self.utxo_arc,
            time_lock,
            other_pub,
            secret_hash,
            tx,
            utxo_common::DEFAULT_SWAP_VOUT,
            search_from_block,
        )
        .await
    }

    async fn search_for_swap_tx_spend_other(
        &self,
        time_lock: u32,
        other_pub: &[u8],
        secret_hash: &[u8],
        tx: &[u8],
        search_from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
    ) -> Result<Option<FoundSwapTxSpend>, String> {
        utxo_common::search_for_swap_tx_spend_other(
            &self.utxo_arc,
            time_lock,
            other_pub,
            secret_hash,
            tx,
            utxo_common::DEFAULT_SWAP_VOUT,
            search_from_block,
        )
        .await
    }

    fn extract_secret(&self, secret_hash: &[u8], spend_tx: &[u8]) -> Result<Vec<u8>, String> {
        utxo_common::extract_secret(secret_hash, spend_tx)
    }

    fn can_refund_htlc(&self, locktime: u64) -> Box<dyn Future<Item = CanRefundHtlc, Error = String> + Send + '_> {
        Box::new(
            utxo_common::can_refund_htlc(self, locktime)
                .boxed()
                .map_err(|e| ERRL!("{}", e))
                .compat(),
        )
    }

    fn negotiate_swap_contract_addr(
        &self,
        _other_side_address: Option<&[u8]>,
    ) -> Result<Option<BytesJson>, MmError<NegotiateSwapContractAddrErr>> {
        Ok(None)
    }

    fn get_htlc_key_pair(&self) -> Option<KeyPair> { utxo_common::get_htlc_key_pair(self) }
}

#[async_trait]
impl WatcherOps for BchCoin {
    fn is_supported_by_watchers(&self) -> bool { true }
}

fn total_unspent_value<'a>(unspents: impl IntoIterator<Item = &'a UnspentInfo>) -> u64 {
    unspents.into_iter().fold(0, |cur, unspent| cur + unspent.value)
}

impl MarketCoinOps for BchCoin {
    fn ticker(&self) -> &str { &self.utxo_arc.conf.ticker }

    fn my_address(&self) -> Result<String, String> { utxo_common::my_address(self) }

    fn get_public_key(&self) -> Result<String, MmError<UnexpectedDerivationMethod>> {
        let pubkey = utxo_common::my_public_key(self.as_ref())?;
        Ok(pubkey.to_string())
    }

    fn sign_message_hash(&self, message: &str) -> Option<[u8; 32]> {
        utxo_common::sign_message_hash(self.as_ref(), message)
    }

    fn sign_message(&self, message: &str) -> SignatureResult<String> {
        utxo_common::sign_message(self.as_ref(), message)
    }

    fn verify_message(&self, signature_base64: &str, message: &str, address: &str) -> VerificationResult<bool> {
        utxo_common::verify_message(self, signature_base64, message, address)
    }

    fn my_balance(&self) -> BalanceFut<CoinBalance> {
        let coin = self.clone();
        let fut = async move {
            let my_address = coin.as_ref().derivation_method.iguana_or_err().mm_err(Into::into)?;
            let bch_unspents = coin.bch_unspents_for_display(my_address).await.mm_err(Into::into)?;
            Ok(bch_unspents.platform_balance(coin.as_ref().decimals))
        };
        Box::new(fut.boxed().compat())
    }

    fn base_coin_balance(&self) -> BalanceFut<BigDecimal> { utxo_common::base_coin_balance(self) }

    fn platform_ticker(&self) -> &str { self.ticker() }

    #[inline(always)]
    fn send_raw_tx(&self, tx: &str) -> Box<dyn Future<Item = String, Error = String> + Send> {
        utxo_common::send_raw_tx(&self.utxo_arc, tx)
    }

    #[inline(always)]
    fn send_raw_tx_bytes(&self, tx: &[u8]) -> Box<dyn Future<Item = String, Error = String> + Send> {
        utxo_common::send_raw_tx_bytes(&self.utxo_arc, tx)
    }

    fn wait_for_confirmations(
        &self,
        tx: &[u8],
        confirmations: u64,
        requires_nota: bool,
        wait_until: u64,
        check_every: u64,
    ) -> Box<dyn Future<Item = (), Error = String> + Send> {
        utxo_common::wait_for_confirmations(
            &self.utxo_arc,
            tx,
            confirmations,
            requires_nota,
            wait_until,
            check_every,
        )
    }

    fn wait_for_tx_spend(
        &self,
        transaction: &[u8],
        wait_until: u64,
        from_block: u64,
        _swap_contract_address: &Option<BytesJson>,
    ) -> TransactionFut {
        utxo_common::wait_for_output_spend(
            &self.utxo_arc,
            transaction,
            utxo_common::DEFAULT_SWAP_VOUT,
            from_block,
            wait_until,
        )
    }

    fn tx_enum_from_bytes(&self, bytes: &[u8]) -> Result<TransactionEnum, String> {
        utxo_common::tx_enum_from_bytes(self.as_ref(), bytes)
    }

    fn current_block(&self) -> Box<dyn Future<Item = u64, Error = String> + Send> {
        utxo_common::current_block(&self.utxo_arc)
    }

    fn display_priv_key(&self) -> Result<String, String> { utxo_common::display_priv_key(&self.utxo_arc) }

    fn min_tx_amount(&self) -> BigDecimal { utxo_common::min_tx_amount(self.as_ref()) }

    fn min_trading_vol(&self) -> MmNumber { utxo_common::min_trading_vol(self.as_ref()) }

    fn sign_raw_tx(&self, args: &SignRawTransactionRequest) -> RawTransactionFut {
        Box::new(utxo_common::sign_raw_tx(self.clone(), args.clone()).boxed().compat())
    }
}

#[async_trait]
impl UtxoStandardOps for BchCoin {
    async fn tx_details_by_hash(
        &self,
        hash: &[u8],
        input_transactions: &mut HistoryUtxoTxMap,
    ) -> Result<TransactionDetails, String> {
        utxo_common::tx_details_by_hash(self, hash, input_transactions).await
    }

    async fn request_tx_history(&self, metrics: MetricsArc) -> RequestTxHistoryResult {
        utxo_common::request_tx_history(self, metrics).await
    }

    async fn update_kmd_rewards(
        &self,
        tx_details: &mut TransactionDetails,
        input_transactions: &mut HistoryUtxoTxMap,
    ) -> UtxoRpcResult<()> {
        utxo_common::update_kmd_rewards(self, tx_details, input_transactions).await
    }
}

#[async_trait]
impl MmCoin for BchCoin {
    fn is_asset_chain(&self) -> bool { utxo_common::is_asset_chain(&self.utxo_arc) }

    fn get_raw_transaction(&self, req: RawTransactionRequest) -> RawTransactionFut {
        Box::new(utxo_common::get_raw_transaction(&self.utxo_arc, req).boxed().compat())
    }

    fn withdraw(&self, req: WithdrawRequest) -> WithdrawFut {
        Box::new(utxo_common::withdraw(self.clone(), req).boxed().compat())
    }

    fn decimals(&self) -> u8 { utxo_common::decimals(&self.utxo_arc) }

    fn convert_to_address(&self, from: &str, to_address_format: Json) -> Result<String, String> {
        utxo_common::convert_to_address(self, from, to_address_format)
    }

    fn validate_address(&self, address: &str) -> ValidateAddressResult { utxo_common::validate_address(self, address) }

    fn process_history_loop(&self, ctx: MmArc) -> Box<dyn Future<Item = (), Error = ()> + Send> {
        Box::new(
            utxo_common::process_history_loop(self.clone(), ctx)
                .map(|_| Ok(()))
                .boxed()
                .compat(),
        )
    }

    fn history_sync_status(&self) -> HistorySyncState { utxo_common::history_sync_status(&self.utxo_arc) }

    fn get_trade_fee(&self) -> Box<dyn Future<Item = TradeFee, Error = String> + Send> {
        utxo_common::get_trade_fee(self.clone())
    }

    async fn get_sender_trade_fee(
        &self,
        value: TradePreimageValue,
        stage: FeeApproxStage,
    ) -> TradePreimageResult<TradeFee> {
        utxo_common::get_sender_trade_fee(self, value, stage).await
    }

    fn get_receiver_trade_fee(&self, _stage: FeeApproxStage) -> TradePreimageFut<TradeFee> {
        utxo_common::get_receiver_trade_fee(self.clone())
    }

    async fn get_fee_to_send_taker_fee(
        &self,
        dex_fee_amount: BigDecimal,
        stage: FeeApproxStage,
    ) -> TradePreimageResult<TradeFee> {
        utxo_common::get_fee_to_send_taker_fee(self, dex_fee_amount, stage).await
    }

    fn required_confirmations(&self) -> u64 { utxo_common::required_confirmations(&self.utxo_arc) }

    fn requires_notarization(&self) -> bool { utxo_common::requires_notarization(&self.utxo_arc) }

    fn set_required_confirmations(&self, confirmations: u64) {
        utxo_common::set_required_confirmations(&self.utxo_arc, confirmations)
    }

    fn set_requires_notarization(&self, requires_nota: bool) {
        utxo_common::set_requires_notarization(&self.utxo_arc, requires_nota)
    }

    fn swap_contract_address(&self) -> Option<BytesJson> { utxo_common::swap_contract_address() }

    fn mature_confirmations(&self) -> Option<u32> { Some(self.utxo_arc.conf.mature_confirmations) }

    fn coin_protocol_info(&self) -> Vec<u8> { utxo_common::coin_protocol_info(self) }

    fn is_coin_protocol_supported(&self, info: &Option<Vec<u8>>) -> bool {
        utxo_common::is_coin_protocol_supported(self, info)
    }
}

// testnet
#[cfg(test)]
pub fn tbch_coin_for_test() -> BchCoin {
    use common::block_on;
    use crypto::privkey::key_pair_from_seed;
    use mm2_core::mm_ctx::MmCtxBuilder;

    let ctx = MmCtxBuilder::default().into_mm_arc();
    let keypair = key_pair_from_seed("BCH SLP test").unwrap();

    let conf = json!({"coin":"BCH","pubtype":0,"p2shtype":5,"mm2":1,"fork_id":"0x40","protocol":{"type":"UTXO"}, "sign_message_prefix": "Bitcoin Signed Message:\n",
         "address_format":{"format":"cashaddress","network":"bchtest"}});
    let req = json!({
        "method": "electrum",
        "coin": "BCH",
        "servers": [{"url":"blackie.c3-soft.com:60001"},{"url":"testnet.imaginary.cash:50001"},{"url":"tbch.loping.net:60001"},{"url":"bitcoin-cash.devmole.eu:5001"}],
        "bchd_urls": ["https://bchd-testnet.electroncash.de:18335"],
        "allow_slp_unsafe_conf": false,
    });

    let params = BchActivationRequest::from_legacy_req(&req).unwrap();
    block_on(bch_coin_from_conf_and_params(
        &ctx,
        "BCH",
        &conf,
        params,
        CashAddrPrefix::SlpTest,
        &*keypair.private().secret,
    ))
    .unwrap()
}

// mainnet
#[cfg(test)]
pub fn bch_coin_for_test() -> BchCoin {
    use common::block_on;
    use crypto::privkey::key_pair_from_seed;
    use mm2_core::mm_ctx::MmCtxBuilder;

    let ctx = MmCtxBuilder::default().into_mm_arc();
    let keypair = key_pair_from_seed("BCH SLP test").unwrap();

    let conf = json!({"coin":"BCH","pubtype":0,"p2shtype":5,"mm2":1,"fork_id":"0x40","protocol":{"type":"UTXO"},
         "address_format":{"format":"cashaddress","network":"bitcoincash"}});
    let req = json!({
        "method": "electrum",
        "coin": "BCH",
        "servers": [{"url":"electrum1.cipig.net:10055"},{"url":"electrum2.cipig.net:10055"},{"url":"cashnode.bch.ninja:50001"}],
        "bchd_urls": [],
        "allow_slp_unsafe_conf": true,
    });

    let params = BchActivationRequest::from_legacy_req(&req).unwrap();
    block_on(bch_coin_from_conf_and_params(
        &ctx,
        "BCH",
        &conf,
        params,
        CashAddrPrefix::SimpleLedger,
        &*keypair.private().secret,
    ))
    .unwrap()
}

#[cfg(test)]
mod bch_tests {
    use super::*;
    use crate::sql_tx_history_storage::SqliteTxHistoryStorage;
    use crate::{TransactionType, TxFeeDetails};
    use common::block_on;

    fn init_storage_for(ticker: &str) -> impl TxHistoryStorage {
        let storage = SqliteTxHistoryStorage::in_memory();
        block_on(storage.init(ticker)).unwrap();
        storage
    }

    #[test]
    fn test_get_slp_genesis_params() {
        let coin = tbch_coin_for_test();
        let token_id = "bb309e48930671582bea508f9a1d9b491e49b69be3d6f372dc08da2ac6e90eb7".into();
        let storage = init_storage_for(coin.ticker());

        let slp_params = block_on(coin.get_slp_genesis_params(token_id, &storage)).unwrap();
        assert_eq!("USDF", slp_params.token_ticker);
        assert_eq!(4, slp_params.decimals[0]);
    }

    #[test]
    fn test_plain_bch_tx_details() {
        let coin = tbch_coin_for_test();
        let storage = init_storage_for(coin.ticker());

        let hash = "a8dcc3c6776e93e7bd21fb81551e853447c55e2d8ac141b418583bc8095ce390".into();
        let tx = block_on(coin.tx_from_storage_or_rpc(&hash, &storage)).unwrap();

        let details = block_on(coin.bch_tx_details(&hash, &tx, None, &storage)).unwrap();
        let expected_total: BigDecimal = "0.11407782".parse().unwrap();
        assert_eq!(expected_total, details.total_amount);

        let expected_received: BigDecimal = "0.11405301".parse().unwrap();
        assert_eq!(expected_received, details.received_by_me);

        let expected_spent: BigDecimal = "0.11407782".parse().unwrap();
        assert_eq!(expected_spent, details.spent_by_me);

        let expected_balance_change: BigDecimal = "-0.00002481".parse().unwrap();
        assert_eq!(expected_balance_change, details.my_balance_change);

        let expected_from = vec!["bchtest:qzx0llpyp8gxxsmad25twksqnwd62xm3lsnnczzt66".to_owned()];
        assert_eq!(expected_from, details.from);

        let expected_to = vec![
            "bchtest:qrhdt5adye8lc68upfj9fctfdgcd3aq9hctf8ft6md".to_owned(),
            "bchtest:qzx0llpyp8gxxsmad25twksqnwd62xm3lsnnczzt66".to_owned(),
        ];
        assert_eq!(expected_to, details.to);

        let expected_internal_id = BytesJson::from("a8dcc3c6776e93e7bd21fb81551e853447c55e2d8ac141b418583bc8095ce390");
        assert_eq!(expected_internal_id, details.internal_id);

        let expected_fee = Some(TxFeeDetails::Utxo(UtxoFeeDetails {
            coin: Some("BCH".into()),
            amount: "0.00001481".parse().unwrap(),
        }));
        assert_eq!(expected_fee, details.fee_details);

        assert_eq!(coin.ticker(), details.coin);
    }

    #[test]
    fn test_slp_tx_details() {
        let coin = tbch_coin_for_test();
        let storage = init_storage_for(coin.ticker());

        let hash = "a8dcc3c6776e93e7bd21fb81551e853447c55e2d8ac141b418583bc8095ce390".into();
        let tx = block_on(coin.tx_from_storage_or_rpc(&hash, &storage)).unwrap();

        let slp_details = parse_slp_script(&tx.outputs[0].script_pubkey).unwrap();

        let slp_tx_details = block_on(coin.slp_tx_details(&tx, slp_details.transaction, None, None, &storage)).unwrap();

        let expected_total: BigDecimal = "6.2974".parse().unwrap();
        assert_eq!(expected_total, slp_tx_details.total_amount);

        let expected_spent: BigDecimal = "6.2974".parse().unwrap();
        assert_eq!(expected_spent, slp_tx_details.spent_by_me);

        let expected_received: BigDecimal = "5.2974".parse().unwrap();
        assert_eq!(expected_received, slp_tx_details.received_by_me);

        let expected_balance_change = BigDecimal::from(-1i32);
        assert_eq!(expected_balance_change, slp_tx_details.my_balance_change);

        let expected_from = vec!["slptest:qzx0llpyp8gxxsmad25twksqnwd62xm3lsg8lecug8".to_owned()];
        assert_eq!(expected_from, slp_tx_details.from);

        let expected_to = vec![
            "slptest:qrhdt5adye8lc68upfj9fctfdgcd3aq9hcsaqj3dfs".to_owned(),
            "slptest:qzx0llpyp8gxxsmad25twksqnwd62xm3lsg8lecug8".to_owned(),
        ];
        assert_eq!(expected_to, slp_tx_details.to);

        let expected_tx_type =
            TransactionType::TokenTransfer("bb309e48930671582bea508f9a1d9b491e49b69be3d6f372dc08da2ac6e90eb7".into());
        assert_eq!(expected_tx_type, slp_tx_details.transaction_type);

        assert_eq!(coin.ticker(), slp_tx_details.coin);
    }

    #[test]
    fn test_sign_message() {
        let coin = tbch_coin_for_test();
        let signature = coin.sign_message("test").unwrap();
        assert_eq!(
            signature,
            "ILuePKMsycXwJiNDOT7Zb7TfIlUW7Iq+5ylKd15AK72vGVYXbnf7Gj9Lk9MFV+6Ub955j7MiAkp0wQjvuIoRPPA="
        );
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn test_verify_message() {
        let coin = tbch_coin_for_test();
        let is_valid = coin
            .verify_message(
                "ILuePKMsycXwJiNDOT7Zb7TfIlUW7Iq+5ylKd15AK72vGVYXbnf7Gj9Lk9MFV+6Ub955j7MiAkp0wQjvuIoRPPA=",
                "test",
                "bchtest:qzx0llpyp8gxxsmad25twksqnwd62xm3lsnnczzt66",
            )
            .unwrap();
        assert!(is_valid);
    }
}
