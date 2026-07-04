use super::*;

pub fn coin_conf(ctx: &MmArc, ticker: &str) -> Json {
    match ctx.conf["coins"].as_array() {
        Some(coins) => coins
            .iter()
            .find(|coin| coin["coin"].as_str() == Some(ticker))
            .cloned()
            .unwrap_or(Json::Null),
        None => Json::Null,
    }
}
pub fn is_wallet_only_conf(conf: &Json) -> bool { conf["wallet_only"].as_bool().unwrap_or(false) }
pub fn is_wallet_only_ticker(ctx: &MmArc, ticker: &str) -> bool {
    let coin_conf = coin_conf(ctx, ticker);
    coin_conf["wallet_only"].as_bool().unwrap_or(false)
}
/// Adds a new currency into the list of currencies configured.
///
/// Returns an error if the currency already exists. Initializing the same currency twice is a bad habit
/// (might lead to misleading and confusing information during debugging and maintenance, see DRY)
/// and should be fixed on the call site.
///
/// * `req` - Payload of the corresponding "enable" or "electrum" RPC request.
pub async fn lp_coininit(ctx: &MmArc, ticker: &str, req: &Json) -> Result<MmCoinEnum, String> {
    let cctx = try_s!(CoinsContext::from_ctx(ctx));
    {
        let coins = cctx.coins.lock().await;
        if coins.get(ticker).is_some() {
            return ERR!("Coin {} already initialized", ticker);
        }
    }

    let coins_en = coin_conf(ctx, ticker);

    if coins_en.is_null() {
        ctx.log.log(
            "😅",
            #[allow(clippy::unnecessary_cast)]
            &[&("coin" as &str), &ticker, &("no-conf" as &str)],
            &fomat! ("Warning, coin " (ticker) " is used without a corresponding configuration."),
        );
    }

    if coins_en["mm2"].is_null() && req["mm2"].is_null() {
        return ERR!(concat!(
            "mm2 param is not set neither in coins config nor enable request, ",
            "assuming that coin is not supported"
        ));
    }
    let secret = try_s!(CryptoCtx::from_ctx(ctx)).mm2_internal_privkey_slice().to_vec();

    if coins_en["protocol"].is_null() {
        return ERR!(
            r#""protocol" field is missing in coins file. The file format is deprecated, please execute ./mm2 update_config command to convert it or download a new one"#
        );
    }
    let protocol: CoinProtocol = try_s!(json::from_value(coins_en["protocol"].clone()));

    let coin: MmCoinEnum = match &protocol {
        CoinProtocol::UTXO => {
            let params = try_s!(UtxoActivationParams::from_legacy_req(req));
            try_s!(utxo_standard_coin_with_priv_key(ctx, ticker, &coins_en, &params, &secret).await).into()
        },
        CoinProtocol::QTUM => {
            let params = try_s!(UtxoActivationParams::from_legacy_req(req));
            try_s!(qtum_coin_with_priv_key(ctx, ticker, &coins_en, &params, &secret).await).into()
        },
        CoinProtocol::ETH { .. } | CoinProtocol::ERC20 { .. } => {
            try_s!(eth_coin_from_conf_and_request(ctx, ticker, &coins_en, req, &secret, protocol).await).into()
        },
        CoinProtocol::QRC20 {
            platform,
            contract_address,
        } => {
            let params = try_s!(Qrc20ActivationParams::from_legacy_req(req));
            let contract_address = try_s!(qtum::contract_addr_from_str(contract_address));

            try_s!(
                qrc20_coin_from_conf_and_params(ctx, ticker, platform, &coins_en, &params, &secret, contract_address)
                    .await
            )
            .into()
        },
        CoinProtocol::BCH { slp_prefix } => {
            let prefix = try_s!(CashAddrPrefix::from_str(slp_prefix));
            let params = try_s!(BchActivationRequest::from_legacy_req(req));

            let bch = try_s!(bch_coin_from_conf_and_params(ctx, ticker, &coins_en, params, prefix, &secret).await);
            bch.into()
        },
        CoinProtocol::SLPTOKEN {
            platform,
            token_id,
            decimals,
            required_confirmations,
        } => {
            let platform_coin = try_s!(lp_coinfind(ctx, platform).await);
            let platform_coin = match platform_coin {
                Some(MmCoinEnum::Bch(coin)) => coin,
                Some(_) => return ERR!("Platform coin {} is not BCH", platform),
                None => return ERR!("Platform coin {} is not activated", platform),
            };

            let confs = required_confirmations.unwrap_or(platform_coin.required_confirmations());
            let token = SlpToken::new(*decimals, ticker.into(), (*token_id).into(), platform_coin, confs);
            token.into()
        },
        #[cfg(not(target_arch = "wasm32"))]
        CoinProtocol::ZHTLC => return ERR!("ZHTLC protocol is not supported by lp_coininit"),
        #[cfg(not(target_arch = "wasm32"))]
        CoinProtocol::LIGHTNING { .. } => return ERR!("Lightning protocol is not supported by lp_coininit"),
        #[cfg(not(target_arch = "wasm32"))]
        CoinProtocol::SOLANA => {
            return ERR!("Solana protocol is not supported by lp_coininit - use enable_solana_with_tokens instead")
        },
        #[cfg(not(target_arch = "wasm32"))]
        CoinProtocol::SPLTOKEN { .. } => {
            return ERR!("SplToken protocol is not supported by lp_coininit - use enable_spl instead")
        },
        CoinProtocol::SIA => {
            return ERR!("SIA protocol is not supported by lp_coininit - use task::enable_sia::init instead")
        },
        CoinProtocol::TENDERMINT { .. } => {
            return ERR!(
                "TENDERMINT protocol is not supported by lp_coininit - use enable_tendermint_with_assets instead"
            )
        },
        CoinProtocol::TENDERMINTTOKEN { .. } => {
            return ERR!(
                "TENDERMINTTOKEN protocol is not supported by lp_coininit - use enable_tendermint_token instead"
            )
        },
        // TRON activation routes through a dedicated builder that populates
        // `EthCoin.tron_api`. P10.2 wiring.
        CoinProtocol::TRX { .. } | CoinProtocol::TRC20 { .. } => try_s!(
            crate::eth::tron::tron_coin_from_conf_and_request(ctx, ticker, &coins_en, req, &secret, protocol).await
        )
        .into(),
    };

    let register_params = RegisterCoinParams {
        ticker: ticker.to_owned(),
        tx_history: req["tx_history"].as_bool().unwrap_or(false),
    };
    try_s!(lp_register_coin(ctx, coin.clone(), register_params).await);
    Ok(coin)
}
#[derive(Debug, Display)]
pub enum RegisterCoinError {
    #[display(fmt = "Coin '{}' is initialized already", coin)]
    CoinIsInitializedAlready {
        coin: String,
    },
    Internal(String),
}
pub struct RegisterCoinParams {
    pub ticker: String,
    pub tx_history: bool,
}
pub async fn lp_register_coin(
    ctx: &MmArc,
    coin: MmCoinEnum,
    params: RegisterCoinParams,
) -> Result<(), MmError<RegisterCoinError>> {
    let RegisterCoinParams { ticker, tx_history } = params;
    let cctx = CoinsContext::from_ctx(ctx).map_to_mm(RegisterCoinError::Internal)?;

    // TODO AP: locking the coins list during the entire initialization prevents different coins from being
    // activated concurrently which results in long activation time: https://github.com/KomodoPlatform/atomicDEX/issues/24
    // So I'm leaving the possibility of race condition intentionally in favor of faster concurrent activation.
    // Should consider refactoring: maybe extract the RPC client initialization part from coin init functions.
    let mut coins = cctx.coins.lock().await;
    if coins.contains_key(&ticker) {
        return MmError::err(RegisterCoinError::CoinIsInitializedAlready { coin: ticker.clone() });
    }
    coins.insert(ticker.clone(), coin.clone());
    if tx_history {
        lp_spawn_tx_history(ctx.clone(), coin).map_to_mm(RegisterCoinError::Internal)?;
    }
    let ctx_weak = ctx.weak();
    spawn(async move { check_balance_update_loop(ctx_weak, ticker).await });
    Ok(())
}
#[cfg(not(target_arch = "wasm32"))]
fn lp_spawn_tx_history(ctx: MmArc, coin: MmCoinEnum) -> Result<(), String> {
    try_s!(std::thread::Builder::new()
        .name(format!("tx_history_{}", coin.ticker()))
        .spawn(move || coin.process_history_loop(ctx).wait()));
    Ok(())
}
#[cfg(target_arch = "wasm32")]
fn lp_spawn_tx_history(ctx: MmArc, coin: MmCoinEnum) -> Result<(), String> {
    let fut = async move {
        let _res = coin.process_history_loop(ctx).compat().await;
    };
    common::executor::spawn_local(fut);
    Ok(())
}
/// NB: Returns only the enabled (aka active) coins.
pub async fn lp_coinfind(ctx: &MmArc, ticker: &str) -> Result<Option<MmCoinEnum>, String> {
    let cctx = try_s!(CoinsContext::from_ctx(ctx));
    let coins = cctx.coins.lock().await;
    Ok(coins.get(ticker).cloned())
}
/// Attempts to find a pair of active coins returning None if one is not enabled
pub async fn find_pair(ctx: &MmArc, base: &str, rel: &str) -> Result<Option<(MmCoinEnum, MmCoinEnum)>, String> {
    let fut_base = lp_coinfind(ctx, base);
    let fut_rel = lp_coinfind(ctx, rel);

    futures::future::try_join(fut_base, fut_rel)
        .map_ok(|(base, rel)| base.zip(rel))
        .await
}
#[derive(Debug, Display)]
pub enum CoinFindError {
    #[display(fmt = "No such coin: {}", coin)]
    NoSuchCoin { coin: String },
}
pub async fn lp_coinfind_or_err(ctx: &MmArc, ticker: &str) -> CoinFindResult<MmCoinEnum> {
    match lp_coinfind(ctx, ticker).await {
        Ok(Some(coin)) => Ok(coin),
        Ok(None) => MmError::err(CoinFindError::NoSuchCoin {
            coin: ticker.to_owned(),
        }),
        Err(e) => panic!("Unexpected error: {}", e),
    }
}
#[derive(Deserialize)]
struct ConvertAddressReq {
    pub(crate) coin: String,
    pub(crate) from: String,
    /// format to that the input address should be converted
    pub(crate) to_address_format: Json,
}
pub async fn convert_address(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let req: ConvertAddressReq = try_s!(json::from_value(req));
    let coin = match lp_coinfind(&ctx, &req.coin).await {
        Ok(Some(t)) => t,
        Ok(None) => return ERR!("No such coin: {}", req.coin),
        Err(err) => return ERR!("!lp_coinfind({}): {}", req.coin, err),
    };
    let result = json!({
        "result": {
            "address": try_s!(coin.convert_to_address(&req.from, req.to_address_format)),
        },
    });
    let body = try_s!(json::to_vec(&result));
    Ok(try_s!(Response::builder().body(body)))
}
pub async fn kmd_rewards_info(ctx: MmArc) -> Result<Response<Vec<u8>>, String> {
    let coin = match lp_coinfind(&ctx, "KMD").await {
        Ok(Some(MmCoinEnum::UtxoCoin(t))) => t,
        Ok(Some(_)) => return ERR!("KMD was expected to be UTXO"),
        Ok(None) => return ERR!("KMD is not activated"),
        Err(err) => return ERR!("!lp_coinfind({}): KMD", err),
    };

    let res = json!({
        "result": try_s!(utxo::kmd_rewards_info(&coin).await),
    });
    let res = try_s!(json::to_vec(&res));
    Ok(try_s!(Response::builder().body(res)))
}
#[derive(Deserialize)]
struct ValidateAddressReq {
    pub(crate) coin: String,
    pub(crate) address: String,
}
#[derive(Serialize)]
pub struct ValidateAddressResult {
    pub is_valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
pub async fn validate_address(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let req: ValidateAddressReq = try_s!(json::from_value(req));
    let coin = match lp_coinfind(&ctx, &req.coin).await {
        Ok(Some(t)) => t,
        Ok(None) => return ERR!("No such coin: {}", req.coin),
        Err(err) => return ERR!("!lp_coinfind({}): {}", req.coin, err),
    };

    let res = json!({ "result": coin.validate_address(&req.address) });
    let body = try_s!(json::to_vec(&res));
    Ok(try_s!(Response::builder().body(body)))
}
pub async fn withdraw(ctx: MmArc, req: WithdrawRequest) -> WithdrawResult {
    let coin = lp_coinfind_or_err(&ctx, &req.coin).await.mm_err(Into::into)?;
    #[cfg(not(target_arch = "wasm32"))]
    let publish_zcoin_tx_history = matches!(coin, MmCoinEnum::ZCoin(_));
    #[cfg(target_arch = "wasm32")]
    let publish_zcoin_tx_history = false;
    let tx_details = coin.withdraw(req).compat().await?;
    if publish_zcoin_tx_history {
        crate::tx_history_streaming::publish_tx_history_records(&ctx, &tx_details.coin, [tx_details.clone()]);
    }
    Ok(tx_details)
}
pub async fn get_raw_transaction(ctx: MmArc, req: RawTransactionRequest) -> RawTransactionResult {
    let coin = lp_coinfind_or_err(&ctx, &req.coin).await.mm_err(Into::into)?;
    coin.get_raw_transaction(req).compat().await
}
pub async fn sign_raw_transaction(ctx: MmArc, req: SignRawTransactionRequest) -> RawTransactionResult {
    let coin = lp_coinfind_or_err(&ctx, &req.coin).await.mm_err(Into::into)?;
    coin.sign_raw_tx(&req).compat().await
}
pub async fn sign_message(ctx: MmArc, req: SignatureRequest) -> SignatureResult<SignatureResponse> {
    let coin = lp_coinfind_or_err(&ctx, &req.coin).await.mm_err(Into::into)?;
    let signature = coin.sign_message(&req.message)?;
    Ok(SignatureResponse { signature })
}
pub async fn verify_message(ctx: MmArc, req: VerificationRequest) -> VerificationResult<VerificationResponse> {
    let coin = lp_coinfind_or_err(&ctx, &req.coin).await.mm_err(Into::into)?;

    let validate_address_result = coin.validate_address(&req.address);
    if !validate_address_result.is_valid {
        return MmError::err(VerificationError::InvalidRequest(
            validate_address_result.reason.unwrap_or_else(|| "Unknown".to_string()),
        ));
    }

    let is_valid = coin.verify_message(&req.signature, &req.message, &req.address)?;

    Ok(VerificationResponse { is_valid })
}
pub async fn remove_delegation(ctx: MmArc, req: RemoveDelegateRequest) -> DelegationResult {
    let coin = lp_coinfind_or_err(&ctx, &req.coin).await.mm_err(Into::into)?;
    match &coin {
        MmCoinEnum::QtumCoin(qtum) => qtum.remove_delegation().compat().await,
        MmCoinEnum::TendermintCoin(_) | MmCoinEnum::TendermintToken(_) => {
            let payload = match req.staking_details {
                Some(StakingDetails::Cosmos(p)) => *p,
                _ => {
                    return MmError::err(DelegationError::InvalidPayload {
                        reason: "Cosmos undelegation requires staking_details with type Cosmos".into(),
                    })
                },
            };
            match coin {
                MmCoinEnum::TendermintCoin(c) => c.undelegate(payload).await,
                MmCoinEnum::TendermintToken(t) => t.platform_coin.undelegate(payload).await,
                _ => unreachable!(),
            }
        },
        _ => MmError::err(DelegationError::CoinDoesntSupportDelegation {
            coin: coin.ticker().to_string(),
        }),
    }
}
pub async fn get_staking_infos(ctx: MmArc, req: GetStakingInfosRequest) -> StakingInfosResult {
    let coin = lp_coinfind_or_err(&ctx, &req.coin).await.mm_err(Into::into)?;
    match coin {
        MmCoinEnum::QtumCoin(qtum) => qtum.get_delegation_infos().compat().await,
        _ => {
            return MmError::err(StakingInfosError::CoinDoesntSupportStakingInfos {
                coin: coin.ticker().to_string(),
            })
        },
    }
}
pub async fn add_delegation(ctx: MmArc, req: AddDelegateRequest) -> DelegationResult {
    let coin = lp_coinfind_or_err(&ctx, &req.coin).await.mm_err(Into::into)?;
    match req.staking_details {
        StakingDetails::Qtum(qtum_staking) => match coin {
            MmCoinEnum::QtumCoin(qtum) => qtum.add_delegation(qtum_staking).compat().await,
            _ => MmError::err(DelegationError::CoinDoesntSupportDelegation {
                coin: coin.ticker().to_string(),
            }),
        },
        StakingDetails::Cosmos(payload) => match coin {
            MmCoinEnum::TendermintCoin(c) => c.delegate(*payload).await,
            MmCoinEnum::TendermintToken(t) => t.platform_coin.delegate(*payload).await,
            _ => MmError::err(DelegationError::CoinDoesntSupportDelegation {
                coin: coin.ticker().to_string(),
            }),
        },
    }
}
#[derive(Deserialize)]
pub struct ClaimStakingRewardsRequest {
    pub coin: String,
    #[serde(flatten)]
    pub payload: rpc_command::tendermint::staking::ClaimRewardsPayload,
}
#[derive(Deserialize)]
pub struct DelegationsInfoRequest {
    pub coin: String,
    #[serde(flatten)]
    pub paging: common::PagingOptions,
}
#[derive(Deserialize)]
pub struct UndelegationsInfoRequest {
    pub coin: String,
    #[serde(flatten)]
    pub paging: common::PagingOptions,
}
#[derive(Deserialize)]
pub struct ValidatorsInfoRequest {
    pub coin: String,
    #[serde(flatten)]
    pub inner: rpc_command::tendermint::staking::ValidatorsQuery,
}
pub async fn claim_staking_rewards(ctx: MmArc, req: ClaimStakingRewardsRequest) -> DelegationResult {
    let coin = lp_coinfind_or_err(&ctx, &req.coin).await.mm_err(Into::into)?;
    match coin {
        MmCoinEnum::TendermintCoin(c) => c.claim_staking_rewards(req.payload).await,
        MmCoinEnum::TendermintToken(t) => t.platform_coin.claim_staking_rewards(req.payload).await,
        _ => MmError::err(DelegationError::CoinDoesntSupportDelegation { coin: req.coin }),
    }
}
pub async fn delegations_info(
    ctx: MmArc,
    req: DelegationsInfoRequest,
) -> Result<rpc_command::tendermint::staking::DelegationsQueryResponse, MmError<StakingInfosError>> {
    let coin = lp_coinfind_or_err(&ctx, &req.coin).await.mm_err(Into::into)?;
    match coin {
        MmCoinEnum::TendermintCoin(c) => c.delegations_list(req.paging).await.map_mm_err(),
        MmCoinEnum::TendermintToken(t) => t.platform_coin.delegations_list(req.paging).await.map_mm_err(),
        _ => MmError::err(StakingInfosError::CoinDoesntSupportStakingInfos { coin: req.coin }),
    }
}
pub async fn ongoing_undelegations_info(
    ctx: MmArc,
    req: UndelegationsInfoRequest,
) -> Result<rpc_command::tendermint::staking::UndelegationsQueryResponse, MmError<StakingInfosError>> {
    let coin = lp_coinfind_or_err(&ctx, &req.coin).await.mm_err(Into::into)?;
    match coin {
        MmCoinEnum::TendermintCoin(c) => c.ongoing_undelegations_list(req.paging).await.map_mm_err(),
        MmCoinEnum::TendermintToken(t) => t
            .platform_coin
            .ongoing_undelegations_list(req.paging)
            .await
            .map_mm_err(),
        _ => MmError::err(StakingInfosError::CoinDoesntSupportStakingInfos { coin: req.coin }),
    }
}
pub async fn validators_info(
    ctx: MmArc,
    req: ValidatorsInfoRequest,
) -> Result<rpc_command::tendermint::staking::ValidatorsQueryResponse, MmError<StakingInfosError>> {
    rpc_command::tendermint::staking::validators_rpc(
        lp_coinfind_or_err(&ctx, &req.coin).await.mm_err(Into::into)?,
        req.inner,
    )
    .await
}
pub async fn send_raw_transaction(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let ticker = try_s!(req["coin"].as_str().ok_or("No 'coin' field")).to_owned();
    let coin = match lp_coinfind(&ctx, &ticker).await {
        Ok(Some(t)) => t,
        Ok(None) => return ERR!("No such coin: {}", ticker),
        Err(err) => return ERR!("!lp_coinfind({}): {}", ticker, err),
    };
    let bytes_string = try_s!(req["tx_hex"].as_str().ok_or("No 'tx_hex' field"));
    let res = try_s!(coin.send_raw_tx(bytes_string).compat().await);
    let body = try_s!(json::to_vec(&json!({ "tx_hash": res })));
    Ok(try_s!(Response::builder().body(body)))
}
#[derive(Deserialize)]
struct MyTxHistoryRequest {
    pub(crate) coin: String,
    pub(crate) from_id: Option<BytesJson>,
    #[serde(default)]
    pub(crate) max: bool,
    #[serde(default = "ten")]
    pub(crate) limit: usize,
    pub(crate) page_number: Option<NonZeroUsize>,
}
/// Returns the transaction history of selected coin. Returns no more than `limit` records (default: 10).
/// Skips the first records up to from_id (skipping the from_id too).
/// Transactions are sorted by number of confirmations in ascending order.
pub async fn my_tx_history(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let request: MyTxHistoryRequest = try_s!(json::from_value(req));
    let coin = match lp_coinfind(&ctx, &request.coin).await {
        Ok(Some(t)) => t,
        Ok(None) => return ERR!("No such coin: {}", request.coin),
        Err(err) => return ERR!("!lp_coinfind({}): {}", request.coin, err),
    };

    let history = try_s!(coin.load_history_from_file(&ctx).compat().await);
    let total_records = history.len();
    let limit = if request.max { total_records } else { request.limit };

    let block_number = try_s!(coin.current_block().compat().await);
    let skip = match &request.from_id {
        Some(id) => {
            try_s!(history
                .iter()
                .position(|item| item.internal_id == *id)
                .ok_or(format!("from_id {:02x} is not found", id)))
                + 1
        },
        None => match request.page_number {
            Some(page_n) => (page_n.get() - 1) * request.limit,
            None => 0,
        },
    };

    let history = history.into_iter().skip(skip).take(limit);
    let history: Vec<Json> = history
        .map(|item| {
            let tx_block = item.block_height;
            let mut json = json::to_value(item).unwrap();
            json["confirmations"] = if tx_block == 0 {
                Json::from(0)
            } else if block_number >= tx_block {
                Json::from((block_number - tx_block) + 1)
            } else {
                Json::from(0)
            };
            json
        })
        .collect();

    let response = json!({
        "result": {
            "transactions": history,
            "limit": limit,
            "skipped": skip,
            "from_id": request.from_id,
            "total": total_records,
            "current_block": block_number,
            "sync_status": coin.history_sync_status(),
            "page_number": request.page_number,
            "total_pages": calc_total_pages(total_records, request.limit),
        }
    });
    let body = try_s!(json::to_vec(&response));
    Ok(try_s!(Response::builder().body(body)))
}
pub async fn get_trade_fee(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let ticker = try_s!(req["coin"].as_str().ok_or("No 'coin' field")).to_owned();
    let coin = match lp_coinfind(&ctx, &ticker).await {
        Ok(Some(t)) => t,
        Ok(None) => return ERR!("No such coin: {}", ticker),
        Err(err) => return ERR!("!lp_coinfind({}): {}", ticker, err),
    };
    let fee_info = try_s!(coin.get_trade_fee().compat().await);
    let res = try_s!(json::to_vec(&json!({
        "result": {
            "coin": fee_info.coin,
            "amount": fee_info.amount.to_decimal(),
            "amount_fraction": fee_info.amount.to_fraction(),
            "amount_rat": fee_info.amount.to_ratio(),
        }
    })));
    Ok(try_s!(Response::builder().body(res)))
}
#[derive(Serialize)]
struct EnabledCoin {
    pub(crate) ticker: String,
    pub(crate) address: String,
}
pub async fn get_enabled_coins(ctx: MmArc) -> Result<Response<Vec<u8>>, String> {
    let coins_ctx: Arc<CoinsContext> = try_s!(CoinsContext::from_ctx(&ctx));
    let coins = coins_ctx.coins.lock().await;
    let enabled_coins: Vec<_> = try_s!(coins
        .iter()
        .map(|(ticker, coin)| {
            let address = try_s!(coin.my_address());
            Ok(EnabledCoin {
                ticker: ticker.clone(),
                address,
            })
        })
        .collect());

    let res = try_s!(json::to_vec(&json!({ "result": enabled_coins })));
    Ok(try_s!(Response::builder().body(res)))
}
pub async fn disable_coin(ctx: &MmArc, ticker: &str) -> Result<(), String> {
    let coins_ctx = try_s!(CoinsContext::from_ctx(ctx));
    let mut coins = coins_ctx.coins.lock().await;
    match coins.remove(ticker) {
        Some(_) => Ok(()),
        None => ERR!("{} is disabled already", ticker),
    }
}
#[derive(Deserialize)]
pub struct ConfirmationsReq {
    pub(crate) coin: String,
    pub(crate) confirmations: u64,
}
pub async fn set_required_confirmations(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let req: ConfirmationsReq = try_s!(json::from_value(req));
    let coin = match lp_coinfind(&ctx, &req.coin).await {
        Ok(Some(t)) => t,
        Ok(None) => return ERR!("No such coin {}", req.coin),
        Err(err) => return ERR!("!lp_coinfind ({}): {}", req.coin, err),
    };
    coin.set_required_confirmations(req.confirmations);
    let res = try_s!(json::to_vec(&json!({
        "result": {
            "coin": req.coin,
            "confirmations": coin.required_confirmations(),
        }
    })));
    Ok(try_s!(Response::builder().body(res)))
}
#[derive(Deserialize)]
pub struct RequiresNotaReq {
    pub(crate) coin: String,
    pub(crate) requires_notarization: bool,
}
pub async fn set_requires_notarization(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let req: RequiresNotaReq = try_s!(json::from_value(req));
    let coin = match lp_coinfind(&ctx, &req.coin).await {
        Ok(Some(t)) => t,
        Ok(None) => return ERR!("No such coin {}", req.coin),
        Err(err) => return ERR!("!lp_coinfind ({}): {}", req.coin, err),
    };
    coin.set_requires_notarization(req.requires_notarization);
    let res = try_s!(json::to_vec(&json!({
        "result": {
            "coin": req.coin,
            "requires_notarization": coin.requires_notarization(),
        }
    })));
    Ok(try_s!(Response::builder().body(res)))
}
pub async fn show_priv_key(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let ticker = try_s!(req["coin"].as_str().ok_or("No 'coin' field")).to_owned();
    let coin = match lp_coinfind(&ctx, &ticker).await {
        Ok(Some(t)) => t,
        Ok(None) => return ERR!("No such coin: {}", ticker),
        Err(err) => return ERR!("!lp_coinfind({}): {}", ticker, err),
    };
    let res = try_s!(json::to_vec(&json!({
        "result": {
            "coin": ticker,
            "priv_key": try_s!(coin.display_priv_key()),
        }
    })));
    Ok(try_s!(Response::builder().body(res)))
}
pub async fn check_balance_update_loop(ctx: MmWeak, ticker: String) {
    let mut current_balance = None;
    loop {
        Timer::sleep(10.).await;
        let ctx = match MmArc::from_weak(&ctx) {
            Some(ctx) => ctx,
            None => return,
        };

        match lp_coinfind(&ctx, &ticker).await {
            Ok(Some(coin)) => {
                let balance = match coin.my_spendable_balance().compat().await {
                    Ok(balance) => balance,
                    Err(_) => continue,
                };
                if Some(&balance) != current_balance.as_ref() {
                    let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();
                    coins_ctx.balance_updated(&coin, &balance).await;
                    current_balance = Some(balance);
                }
            },
            Ok(None) => break,
            Err(_) => continue,
        }
    }
}
pub async fn register_balance_update_handler(
    ctx: MmArc,
    handler: Box<dyn BalanceTradeFeeUpdatedHandler + Send + Sync>,
) {
    let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();
    coins_ctx.balance_update_handlers.lock().await.push(handler);
}
pub fn update_coins_config(mut config: Json) -> Result<Json, String> {
    let coins = match config.as_array_mut() {
        Some(c) => c,
        _ => return ERR!("Coins config must be an array"),
    };

    for coin in coins {
        // the coin_as_str is used only to be formatted
        let coin_as_str = format!("{}", coin);
        let coin = try_s!(coin
            .as_object_mut()
            .ok_or(ERRL!("Expected object, found {:?}", coin_as_str)));
        if coin.contains_key("protocol") {
            // the coin is up-to-date
            continue;
        }
        let protocol = match coin.remove("etomic") {
            Some(etomic) => {
                let etomic = etomic
                    .as_str()
                    .ok_or(ERRL!("Expected etomic as string, found {:?}", etomic))?;
                if etomic == "0x0000000000000000000000000000000000000000" {
                    CoinProtocol::ETH { chain_id: None }
                } else {
                    let contract_address = etomic.to_owned();
                    CoinProtocol::ERC20 {
                        platform: "ETH".into(),
                        contract_address,
                    }
                }
            },
            _ => CoinProtocol::UTXO,
        };

        let protocol = json::to_value(protocol).map_err(|e| ERRL!("Error {:?} on process {:?}", e, coin_as_str))?;
        coin.insert("protocol".into(), protocol);
    }

    Ok(config)
}
#[derive(Deserialize)]
struct ConvertUtxoAddressReq {
    pub(crate) address: String,
    pub(crate) to_coin: String,
}
pub async fn convert_utxo_address(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let req: ConvertUtxoAddressReq = try_s!(json::from_value(req));
    let mut addr: utxo::Address = try_s!(req.address.parse());
    let coin = match lp_coinfind(&ctx, &req.to_coin).await {
        Ok(Some(c)) => c,
        _ => return ERR!("Coin {} is not activated", req.to_coin),
    };
    let coin = match coin {
        MmCoinEnum::UtxoCoin(utxo) => utxo,
        _ => return ERR!("Coin {} is not utxo", req.to_coin),
    };
    addr.prefix = coin.as_ref().conf.pub_addr_prefix;
    addr.t_addr_prefix = coin.as_ref().conf.pub_t_addr_prefix;
    addr.checksum_type = coin.as_ref().conf.checksum_type;

    let response = try_s!(json::to_vec(&json!({
        "result": addr.to_string(),
    })));
    Ok(try_s!(Response::builder().body(response)))
}
pub fn address_by_coin_conf_and_pubkey_str(
    ctx: &MmArc,
    coin: &str,
    conf: &Json,
    pubkey: &str,
    addr_format: UtxoAddressFormat,
) -> Result<String, String> {
    let protocol: CoinProtocol = try_s!(json::from_value(conf["protocol"].clone()));
    match protocol {
        CoinProtocol::ERC20 { .. } | CoinProtocol::ETH { .. } => eth::addr_from_pubkey_str(pubkey),
        CoinProtocol::UTXO | CoinProtocol::QTUM | CoinProtocol::QRC20 { .. } | CoinProtocol::BCH { .. } => {
            utxo::address_by_conf_and_pubkey_str(coin, conf, pubkey, addr_format)
        },
        CoinProtocol::SLPTOKEN { platform, .. } => {
            let platform_conf = coin_conf(ctx, &platform);
            if platform_conf.is_null() {
                return ERR!("platform {} conf is null", platform);
            }
            // TODO is there any way to make it better without duplicating the prefix in the SLP conf?
            let platform_protocol: CoinProtocol = try_s!(json::from_value(platform_conf["protocol"].clone()));
            match platform_protocol {
                CoinProtocol::BCH { slp_prefix } => {
                    slp_addr_from_pubkey_str(pubkey, &slp_prefix).map_err(|e| ERRL!("{}", e))
                },
                _ => ERR!("Platform protocol {:?} is not BCH", platform_protocol),
            }
        },
        #[cfg(not(target_arch = "wasm32"))]
        CoinProtocol::LIGHTNING { .. } => {
            ERR!("address_by_coin_conf_and_pubkey_str is not implemented for lightning protocol yet!")
        },
        #[cfg(not(target_arch = "wasm32"))]
        CoinProtocol::SOLANA | CoinProtocol::SPLTOKEN { .. } => {
            ERR!("Solana pubkey is the public address - you do not need to use this rpc call.")
        },
        #[cfg(not(target_arch = "wasm32"))]
        CoinProtocol::ZHTLC => ERR!("address_by_coin_conf_and_pubkey_str is not supported for ZHTLC protocol!"),
        CoinProtocol::SIA => ERR!("address_by_coin_conf_and_pubkey_str is not supported for SIA protocol!"),
        CoinProtocol::TENDERMINT { .. } | CoinProtocol::TENDERMINTTOKEN { .. } => {
            ERR!("address_by_coin_conf_and_pubkey_str is not supported for Tendermint protocol!")
        },
        CoinProtocol::TRX { .. } | CoinProtocol::TRC20 { .. } => {
            ERR!("address_by_coin_conf_and_pubkey_str is not supported for TRON protocol!")
        },
    }
}
#[cfg(target_arch = "wasm32")]
pub(crate) fn load_history_from_file_impl<T>(coin: &T, ctx: &MmArc) -> TxHistoryFut<Vec<TransactionDetails>>
where
    T: MmCoin + ?Sized,
{
    let ctx = ctx.clone();
    let ticker = coin.ticker().to_owned();
    let my_address = try_f!(coin.my_address().map_to_mm(TxHistoryError::InternalError));

    let fut = async move {
        let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();
        let db = coins_ctx.tx_history_db().await?;
        let err = match db.load_history(&ticker, &my_address).await {
            Ok(history) => return Ok(history),
            Err(e) => e,
        };

        if let TxHistoryError::ErrorDeserializing(e) = err.get_inner() {
            ctx.log.log(
                "🌋",
                &[&"tx_history", &ticker.to_owned()],
                &ERRL!("Error {} on history deserialization, resetting the cache.", e),
            );
            db.clear(&ticker, &my_address).await?;
            return Ok(Vec::new());
        }

        Err(err)
    };
    Box::new(fut.boxed().compat())
}
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn load_history_from_file_impl<T>(coin: &T, ctx: &MmArc) -> TxHistoryFut<Vec<TransactionDetails>>
where
    T: MmCoin + ?Sized,
{
    let ticker = coin.ticker().to_owned();
    let history_path = coin.tx_history_path(ctx);
    let ctx = ctx.clone();

    let fut = async move {
        let content = match fs::read(&history_path).await {
            Ok(content) => content,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Ok(Vec::new());
            },
            Err(err) => {
                let error = format!(
                    "Error '{}' reading from the history file {}",
                    err,
                    history_path.display()
                );
                return MmError::err(TxHistoryError::ErrorLoading(error));
            },
        };
        let serde_err = match json::from_slice(&content) {
            Ok(txs) => return Ok(txs),
            Err(e) => e,
        };

        ctx.log.log(
            "🌋",
            &[&"tx_history", &ticker],
            &ERRL!("Error {} on history deserialization, resetting the cache.", serde_err),
        );
        fs::remove_file(&history_path)
            .await
            .map_to_mm(|e| TxHistoryError::ErrorClearing(e.to_string()))?;
        Ok(Vec::new())
    };
    Box::new(fut.boxed().compat())
}
#[cfg(target_arch = "wasm32")]
pub(crate) fn save_history_to_file_impl<T>(coin: &T, ctx: &MmArc, history: Vec<TransactionDetails>) -> TxHistoryFut<()>
where
    T: MmCoin + MarketCoinOps + ?Sized,
{
    let ctx = ctx.clone();
    let ticker = coin.ticker().to_owned();
    let my_address = try_f!(coin.my_address().map_to_mm(TxHistoryError::InternalError));

    let fut = async move {
        let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();
        let db = coins_ctx.tx_history_db().await?;
        db.save_history(&ticker, &my_address, history).await?;
        Ok(())
    };
    Box::new(fut.boxed().compat())
}
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn save_history_to_file_impl<T>(coin: &T, ctx: &MmArc, history: Vec<TransactionDetails>) -> TxHistoryFut<()>
where
    T: MmCoin + MarketCoinOps + ?Sized,
{
    let history_path = coin.tx_history_path(ctx);
    let tmp_file = format!("{}.tmp", history_path.display());

    let fut = async move {
        let content = json::to_vec(&history).map_to_mm(|e| TxHistoryError::ErrorSerializing(e.to_string()))?;

        let fs_fut = async {
            let mut file = fs::File::create(&tmp_file).await?;
            file.write_all(&content).await?;
            file.flush().await?;
            fs::rename(&tmp_file, &history_path).await?;
            Ok(())
        };

        let res: io::Result<_> = fs_fut.await;
        if let Err(e) = res {
            let error = format!("Error '{}' creating/writing/renaming the tmp file {}", e, tmp_file);
            return MmError::err(TxHistoryError::ErrorSaving(error));
        }
        Ok(())
    };
    Box::new(fut.boxed().compat())
}
