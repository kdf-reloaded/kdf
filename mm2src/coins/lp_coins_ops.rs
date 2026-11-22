use super::*;
use crate::coin_work::{CoinWork, PendingCoinWork};

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
/// Whether TRON (TRX, TRC20) coins can be activated.
///
/// `true` since 2026-11-20 (CRD ch.21 D6): every activation route of the family (`enable_eth_with_tokens`,
/// `task::enable_eth::*`, `enable_erc20`, `task::enable_erc20::*`, and the legacy `enable` and `electrum`) builds
/// and registers the coin. No route reads this constant any more; it records the state of the family next to the
/// other family switches. Whether a TRON coin can trade is a separate switch, [`TRON_SWAPS_AVAILABLE`].
pub const TRON_ACTIVATION_AVAILABLE: bool = true;

/// Beta switch: whether Tendermint (Cosmos-family) coins and their tokens can take part in orders and swaps.
///
/// Swaps for the family are postponed for the current beta. While this is `false`, every Tendermint platform
/// coin and every Tendermint token reports itself as wallet-only: activation, balances, addresses and
/// `withdraw` work as before, while `setprice`, `buy`, `sell` and the other ways to start a new trade refuse the
/// coin with the usual wallet-only error, and an order or request that already exists is not matched. A swap
/// that has already started is not affected (nothing on the swap, kick-start, refund or recovery path asks
/// whether a coin is wallet-only). The read-only views (`orderbook`, `best_orders`, `trade_preimage`) look at the
/// coins-config `wallet_only` flag alone and are not changed by this switch. Set it to `true` to give the family
/// back the behavior its coins-config entry asks for.
pub const TENDERMINT_SWAPS_AVAILABLE: bool = false;

/// Switch: whether TRON (TRX, TRC20) coins can take part in orders and swaps.
///
/// Reloaded has no TRON swap (CRD ch.21 R-D1, R21.B3). While this is `false`, every TRON platform coin and every
/// TRC20 token reports itself as wallet-only, whatever its coins-config entry says: activation, balances,
/// addresses, `withdraw` and the transaction history work, while `setprice`, `buy`, `sell` and the other ways to
/// start a new trade refuse the coin with the usual wallet-only error, and an order or request that already exists
/// is not matched. An order with a TRON coin that was saved earlier is kept at startup but never announced or
/// matched (CRD ch.60 R60.6.5), and an unfinished saved swap that names a TRON coin is not resumed at startup (its
/// record is kept). The read-only views
/// (`orderbook`, `best_orders`, `trade_preimage`) look at the coins-config `wallet_only` flag alone and are not
/// changed by this switch, and neither are `max_taker_vol`, `max_maker_vol` and the legacy `get_trade_fee`, which
/// do not ask whether a coin is wallet-only. Set it to `true` only together with a TRON swap implementation.
pub const TRON_SWAPS_AVAILABLE: bool = false;

/// Adds a new currency into the list of currencies configured.
///
/// Returns an error if the currency already exists. Initializing the same currency twice is a bad habit
/// (might lead to misleading and confusing information during debugging and maintenance, see DRY)
/// and should be fixed on the call site.
///
/// * `req` - Payload of the corresponding "enable" or "electrum" RPC request.
pub async fn lp_coininit(ctx: &MmArc, ticker: &str, req: &Json) -> Result<MmCoinEnum, String> {
    // The work the build spawns belongs to the instance; it ends if the activation does not register the instance
    // (CRD ch.43 R43.1E.5 item 4, §43.1E not-established item (1)).
    let work = PendingCoinWork::new();
    let scope = work.work().clone();
    scope.scope(lp_coininit_building(ctx, ticker, req, work)).await
}

async fn lp_coininit_building(
    ctx: &MmArc,
    ticker: &str,
    req: &Json,
    work: PendingCoinWork,
) -> Result<MmCoinEnum, String> {
    let cctx = try_s!(CoinsContext::from_ctx(ctx));
    {
        let coins = cctx.coins.lock().await;
        // A passive platform is registered, so the legacy `enable` and `electrum` refuse its ticker as already
        // activated (CRD ch.43 R43.1E.6 item 6).
        if coins.registered().contains(ticker) {
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
    let protocol: CoinProtocol = try_s!(CoinProtocol::from_conf_json(coins_en["protocol"].clone()));

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
        CoinProtocol::ZHTLC(_) => return ERR!("ZHTLC protocol is not supported by lp_coininit"),
        #[cfg(not(target_arch = "wasm32"))]
        CoinProtocol::LIGHTNING { .. } => return ERR!("Lightning protocol is not supported by lp_coininit"),
        CoinProtocol::SOLANA => {
            return ERR!("Solana protocol is not supported by lp_coininit - use enable_solana_with_tokens instead")
        },
        CoinProtocol::SPLTOKEN { .. } | CoinProtocol::SOLANATOKEN { .. } => {
            return ERR!("SplToken protocol is not supported by lp_coininit - use enable_spl instead")
        },
        // The legacy `enable` method activates a Sia-protocol coin synchronously: the Sia members sit at
        // the top level of the legacy request, next to `coin`, and construction (including the walletd
        // reachability probe) is the one `task::enable_sia::init` uses (CRD ch.46 R46.6.3, ch.55 R55.4.8).
        // Any failure is reported through the legacy error envelope.
        CoinProtocol::SIA => {
            let request = try_s!(siacoin::SiaCoinActivationRequest::from_legacy_req(req));
            let priv_key_policy = try_s!(PrivKeyBuildPolicy::from_activation_policy(ctx, request.priv_key_policy));
            try_s!(siacoin::SiaCoin::new(ctx, coins_en.clone(), &request, priv_key_policy).await).into()
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
        CoinProtocol::NFT { .. } => {
            return ERR!("NFT protocol is not supported by lp_coininit - use enable_nft instead")
        },
        // TRON activation routes through a dedicated builder that populates
        // `EthCoin.tron_api`. P10.2 wiring.
        // Docs vs reference (CRD ch.55 R55.4.7, D55.5): the reference release rejects TRX and TRC20 on the
        // legacy path and requires an mmrpc 2.0 activation. Reloaded has that activation now
        // (`enable_eth_with_tokens`, `task::enable_eth`), and builds the TRON HD wallet there under a global-HD
        // login (CRD ch.21 R-A2). This legacy route stays as a documented extension (maintainer decision
        // 2026-10-11). Under a global-HD login it keeps the node's internal key, as the EVM and UTXO legacy routes
        // do in Reloaded (decided 2026-11-19), so its address differs from the one the mmrpc 2.0 HD route gives for
        // the same seed. A client written for the reference never sends such a request.
        CoinProtocol::TRX { .. } | CoinProtocol::TRC20 { .. } => try_s!(
            crate::eth::tron::tron_coin_from_conf_and_request(ctx, ticker, &coins_en, req, &secret, protocol).await
        )
        .into(),
    };

    let register_params = RegisterCoinParams {
        ticker: ticker.to_owned(),
        tx_history: req["tx_history"].as_bool().unwrap_or(false),
    };
    try_s!(lp_register_coin_with_work(ctx, coin.clone(), register_params, work).await);
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
    lp_register_coin_with_work(ctx, coin, params, PendingCoinWork::new()).await
}
/// Like [`lp_register_coin`], for an instance whose build spawned background work with `work`: the registry keeps the
/// work on success, and it ends when the ticker is refused or the registration fails (CRD ch.43 R43.1E.5 item 4).
pub async fn lp_register_coin_with_work(
    ctx: &MmArc,
    coin: MmCoinEnum,
    params: RegisterCoinParams,
    work: PendingCoinWork,
) -> Result<(), MmError<RegisterCoinError>> {
    let RegisterCoinParams { ticker, tx_history } = params;
    let cctx = CoinsContext::from_ctx(ctx).map_to_mm(RegisterCoinError::Internal)?;

    // TODO AP: locking the coins list during the entire initialization prevents different coins from being
    // activated concurrently which results in long activation time: https://github.com/KomodoPlatform/atomicDEX/issues/24
    // So I'm leaving the possibility of race condition intentionally in favor of faster concurrent activation.
    // Should consider refactoring: maybe extract the RPC client initialization part from coin init functions.
    let mut coins = cctx.coins.lock().await;
    // A passive platform is registered: its ticker is refused as already initialized.
    if coins.registered().contains(&ticker) {
        return MmError::err(RegisterCoinError::CoinIsInitializedAlready { coin: ticker.clone() });
    }
    cctx.fee_estimate_cache.forget_coin(&ticker);
    let work = work.into_work();
    coins.insert_with_work(ticker.clone(), coin.clone(), work.clone());
    if tx_history {
        lp_spawn_tx_history(ctx.clone(), coin, work).map_to_mm(RegisterCoinError::Internal)?;
    }
    Ok(())
}
/// Starts the history loop of a registered instance. The loop is abortable and registered with the instance's
/// `CoinWork`, so removing the instance ends it; it runs in the instance's scope, where it reads its instance id (CRD
/// ch.43 R43.1E.5 item 4, ch.55 R55.4.9).
#[cfg(not(target_arch = "wasm32"))]
fn lp_spawn_tx_history(ctx: MmArc, coin: MmCoinEnum, work: Arc<CoinWork>) -> Result<(), String> {
    // A plain std::thread + futures01 .wait() has no ambient Tokio runtime
    // context. That was never a problem for the coins that have used this
    // path so far (their history fetch doesn't need one), but a coin whose
    // history loop makes async HTTP calls through a Tokio-based client
    // (e.g. Sia's, built on reqwest) silently fails the moment it tries to
    // do any I/O: reqwest/tokio's internals require a runtime handle a raw
    // OS thread doesn't have. This surfaces as the loop never completing
    // even its first pass -- sync_status stuck at its initial NotStarted
    // value forever, with nothing to log, because the failure happens
    // before the loop body ever runs far enough to call
    // set_history_sync_state even once.
    //
    // ETH already hit this and worked around it by routing its own history
    // loop through `spawn` (the shared Tokio runtime, CORE) instead of this
    // function (see start_history_background_fetching in
    // eth_with_tokens_activation.rs) -- do the same here, generically, so
    // every coin gets a runtime-aware spawn rather than only the one coin
    // that happened to need it worked around first. This mirrors the
    // wasm32 branch below, which already spawns onto its own executor
    // rather than a raw thread.
    let scope = work.clone();
    let (fut, handle) = futures::future::abortable(async move {
        let _res = coin.process_history_loop(ctx).compat().await;
    });
    work.add(handle);
    common::executor::spawn(async move {
        let _ = scope.scope(fut).await;
    });
    Ok(())
}
#[cfg(target_arch = "wasm32")]
fn lp_spawn_tx_history(ctx: MmArc, coin: MmCoinEnum, work: Arc<CoinWork>) -> Result<(), String> {
    let scope = work.clone();
    let (fut, handle) = futures::future::abortable(async move {
        let _res = coin.process_history_loop(ctx).compat().await;
    });
    work.add(handle);
    common::executor::spawn_local(async move {
        let _ = scope.scope(fut).await;
    });
    Ok(())
}
/// NB: Returns only the enabled (aka active) coins: a passive platform coin is not found (CRD ch.43 R43.1E.6
/// item 1). A caller that a rule says must still see a passive coin uses [`lp_coinfind_registered`].
pub async fn lp_coinfind(ctx: &MmArc, ticker: &str) -> Result<Option<MmCoinEnum>, String> {
    let cctx = try_s!(CoinsContext::from_ctx(ctx));
    let coins = cctx.coins.lock().await;
    Ok(coins.active().get(ticker).cloned())
}
/// Returns a registered coin, active or passive (CRD ch.43 R43.1E.6). Use it only where a rule says that a passive
/// platform coin still counts: `get_enabled_coins`, the already-activated checks, a dependent token's own look-up of
/// its platform (D43.1F.3), and the background work of a passive platform.
pub async fn lp_coinfind_registered(ctx: &MmArc, ticker: &str) -> Result<Option<MmCoinEnum>, String> {
    let cctx = try_s!(CoinsContext::from_ctx(ctx));
    let coins = cctx.coins.lock().await;
    Ok(coins.registered().get(ticker).cloned())
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
/// The request of `convertaddress` (legacy members, unchanged).
#[derive(Deserialize)]
pub struct ConvertAddressReq {
    pub coin: String,
    pub from: String,
    /// format to that the input address should be converted
    pub to_address_format: Json,
}
/// The success of `convertaddress`: the content that the legacy body carries under `result`.
#[derive(Serialize)]
pub struct ConvertAddressResponse {
    pub address: String,
}
/// The typed core of `convertaddress`.
pub async fn convert_address_core(
    ctx: MmArc,
    req: ConvertAddressReq,
) -> Result<ConvertAddressResponse, MmError<ConvertAddressError>> {
    let coin = match lp_coinfind(&ctx, &req.coin).await {
        Ok(Some(t)) => t,
        Ok(None) => return MmError::err(ConvertAddressError::NoSuchCoin { coin: req.coin.clone() }),
        Err(err) => {
            return MmError::err(ConvertAddressError::InternalError(format!(
                "!lp_coinfind({}): {}",
                req.coin, err
            )))
        },
    };
    let address = coin
        .convert_to_address(&req.from, req.to_address_format)
        .map_to_mm(ConvertAddressError::InvalidParam)?;
    Ok(ConvertAddressResponse { address })
}
/// The request of `kmd_rewards_info`: it takes no members. Any object (members are ignored) and, through the
/// versioned dispatcher, an absent or `null` `params` decode to it.
#[derive(Deserialize)]
pub struct KmdRewardsInfoRequest {}
/// The typed core of `kmd_rewards_info`: the rewards info of the unspent outputs of KMD, ordered by output value.
pub async fn kmd_rewards_info_core(
    ctx: MmArc,
    _req: KmdRewardsInfoRequest,
) -> Result<Vec<utxo::KmdRewardsInfoElement>, MmError<KmdRewardsInfoError>> {
    let coin = match lp_coinfind(&ctx, "KMD").await {
        Ok(Some(MmCoinEnum::UtxoCoin(t))) => t,
        Ok(Some(_)) => {
            return MmError::err(KmdRewardsInfoError::InternalError(
                "KMD was expected to be UTXO".to_owned(),
            ))
        },
        Ok(None) => return MmError::err(KmdRewardsInfoError::NoSuchCoin { coin: "KMD".to_owned() }),
        Err(err) => {
            return MmError::err(KmdRewardsInfoError::InternalError(format!(
                "!lp_coinfind({}): KMD",
                err
            )))
        },
    };
    // The one failure of the computation that is not a read of the node: a wallet without an Iguana key.
    if let Err(error) = coin.as_ref().derivation_method.iguana_or_err() {
        return MmError::err(KmdRewardsInfoError::InternalError(error.get_inner().to_string()));
    }
    utxo::kmd_rewards_info(&coin)
        .await
        .map_to_mm(KmdRewardsInfoError::Transport)
}
/// The request of `validateaddress` (legacy members, unchanged).
#[derive(Deserialize)]
pub struct ValidateAddressReq {
    pub coin: String,
    pub address: String,
}
#[derive(Serialize)]
pub struct ValidateAddressResult {
    pub is_valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
/// The typed core of `validateaddress`. An address that is not valid is a success with `is_valid: false`.
pub async fn validate_address_core(
    ctx: MmArc,
    req: ValidateAddressReq,
) -> Result<ValidateAddressResult, MmError<ValidateAddressError>> {
    let coin = match lp_coinfind(&ctx, &req.coin).await {
        Ok(Some(t)) => t,
        Ok(None) => return MmError::err(ValidateAddressError::NoSuchCoin { coin: req.coin.clone() }),
        Err(err) => {
            return MmError::err(ValidateAddressError::InternalError(format!(
                "!lp_coinfind({}): {}",
                req.coin, err
            )))
        },
    };
    Ok(coin.validate_address(&req.address))
}
pub async fn withdraw(ctx: MmArc, req: WithdrawRequest) -> WithdrawResult {
    let coin = lp_coinfind_or_err(&ctx, &req.coin).await.mm_err(Into::into)?;
    // CRD ch.63 R21: the fee-rail check comes before any other processing of the request, for every coin family.
    check_withdraw_fee_rail(&coin, &req)?;
    // A withdrawal is not built while the coin's Electrum servers disagree about the chain (CRD ch.38 R38.6.4o item 12).
    if let Some(error) = coin.chain_disagreement() {
        return MmError::err(WithdrawError::ChainDisagreement(error));
    }
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
/// The request of `send_raw_transaction` (legacy members, unchanged): the coin and one transaction carrier, `tx_hex`
/// or `tx_json` (CRD ch.20 R-W8). Both carriers are kept as the request spelled them, so that a carrier of the wrong
/// type is refused by [`select_raw_tx_carrier`] and not by the decoder; `null` counts as absent.
#[derive(Deserialize)]
pub struct SendRawTransactionReq {
    pub coin: String,
    #[serde(default)]
    pub tx_hex: Option<Json>,
    #[serde(default)]
    pub tx_json: Option<Json>,
}
/// The success of `send_raw_transaction`: the whole legacy body, a bare object.
#[derive(Serialize)]
pub struct SendRawTransactionResponse {
    pub tx_hash: String,
}
/// The typed core of `send_raw_transaction`.
pub async fn send_raw_transaction_core(
    ctx: MmArc,
    req: SendRawTransactionReq,
) -> Result<SendRawTransactionResponse, MmError<SendRawTransactionError>> {
    let coin = match lp_coinfind(&ctx, &req.coin).await {
        Ok(Some(t)) => t,
        Ok(None) => return MmError::err(SendRawTransactionError::NoSuchCoin { coin: req.coin.clone() }),
        Err(err) => {
            return MmError::err(SendRawTransactionError::InternalError(format!(
                "!lp_coinfind({}): {}",
                req.coin, err
            )))
        },
    };
    let carriers = json!({ "tx_hex": req.tx_hex, "tx_json": req.tx_json });
    let carrier = select_raw_tx_carrier(&carriers)
        .map_err(|text| MmError::new(SendRawTransactionError::InvalidParam(text.to_owned())))?;
    let tx_hash = match carrier {
        RawTxCarrier::Hex(hex) => coin
            .send_raw_tx(hex)
            .compat()
            .await
            .map_to_mm(SendRawTransactionError::Transport)?,
        // A JSON carrier is already the coin's native serialised form, so its
        // bytes go to the bytes-taking half of the trait rather than through
        // the hex layer `send_raw_tx` exists to strip.
        RawTxCarrier::Json(tx_json) => {
            let bytes = json::to_vec(tx_json).map_to_mm(|e| SendRawTransactionError::InternalError(e.to_string()))?;
            coin.send_raw_tx_bytes(&bytes)
                .compat()
                .await
                .map_to_mm(SendRawTransactionError::Transport)?
        },
    };
    Ok(SendRawTransactionResponse { tx_hash })
}

/// Which of `send_raw_transaction`'s two accepted transaction carriers a
/// request selected (CRD ch.20 R-W8).
#[derive(Debug, PartialEq)]
enum RawTxCarrier<'a> {
    /// `tx_hex`: hex, for every coin whose transactions have a binary wire form.
    Hex(&'a str),
    /// `tx_json`: the transaction's native JSON object, for a coin whose native
    /// serialisation is JSON text -- currently Sia only.
    Json(&'a Json),
}

/// Pick the carrier a `send_raw_transaction` request supplied, per ch.20 R-W8.
///
/// `tx_hex` wins whenever it is present, so a caller that echoes back a whole
/// transaction-details object -- which for Sia carries both (ch.20 R-W6/R-W7) --
/// broadcasts the same transaction whichever coin it holds. A present-but-
/// malformed carrier is an error rather than a reason to fall through to the
/// other one: falling through would broadcast a transaction the caller did not
/// select. A `null` counts as absent, so that echoing an object whose optional
/// carrier was serialised as `null` behaves like omitting it.
fn select_raw_tx_carrier(req: &Json) -> Result<RawTxCarrier<'_>, &'static str> {
    let present = |field| req.get(field).filter(|value| !value.is_null());
    match (present("tx_hex"), present("tx_json")) {
        (Some(tx_hex), _) => match tx_hex.as_str() {
            Some(tx_hex) => Ok(RawTxCarrier::Hex(tx_hex)),
            None => Err("'tx_hex' must be a string"),
        },
        (None, Some(tx_json)) if tx_json.is_object() => Ok(RawTxCarrier::Json(tx_json)),
        (None, Some(_)) => Err("'tx_json' must be an object"),
        (None, None) => Err("Request must carry either a 'tx_hex' or a 'tx_json' field"),
    }
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
    // A Solana coin or SPL token has no history yet (CRD ch.40 R40.6B.15, D40.6B.1): a structured error naming the coin.
    if matches!(coin, MmCoinEnum::SolanaCoin(_) | MmCoinEnum::SplToken(_)) {
        return ERR!("my_tx_history is not supported for {}", request.coin);
    }

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
    // A passive platform is listed, with its address as for an active coin (CRD ch.43 R43.1E.6 item 2).
    let enabled_coins: Vec<_> = try_s!(coins
        .registered()
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
/// The work that ends with a removed instance, at once and wherever the instance is. Every removal path calls it, once
/// per removed instance: `disable_coin` and each removal of `disable_platform_with_dependents`.
///
/// The background work that the instance registered with its `CoinWork` (history collector, block-header loop) is
/// aborted (CRD ch.43 R43.1E.5 item 4). Work that holds the instance only weakly ends with its last handle. The work
/// of a Lightning coin is not registered and goes on until the node stops.
///
/// A QRC20 token's history collector ends with its instance; the token's Electrum client closes with the last handle of
/// the instance (CRD ch.38 R38.6.4Q.10 item 1, D38.6.4Q.5).
///
/// A removed QTUM or QRC20 instance leaves the subscriptions of its Electrum client; one that no other instance
/// remembers is no longer placed again after a reconnect (CRD ch.38 D38.6.4Q.1 status note, item 6). This takes no
/// registry lock and sends nothing.
fn end_removed_instance_work(removed: &MmCoinEnum, work: &CoinWork) {
    work.abort_all();
    match removed {
        MmCoinEnum::QtumCoin(qtum) => {
            AsRef::<crate::utxo::UtxoCoinFields>::as_ref(qtum).forget_electrum_subscriptions()
        },
        MmCoinEnum::Qrc20Coin(token) => {
            token.end_history_collector();
            token.utxo.forget_electrum_subscriptions();
        },
        _ => (),
    }
}
/// Ends the event streamers that are keyed by the ticker of a removed coin, for every client (CRD ch.43 R43.1E.5 item
/// 4): the balance, fee-estimation and transaction-history streams. The clients' subscriptions end as after a last
/// unsubscribe. This takes no registry lock.
fn end_event_streams_of(ctx: &MmArc, ticker: &str) {
    let manager = &ctx.event_stream_manager;
    for streamer_id in [
        mm2_event_stream::StreamerId::Balance(ticker.to_owned()),
        mm2_event_stream::StreamerId::FeeEstimation(ticker.to_owned()),
        mm2_event_stream::StreamerId::TxHistory(ticker.to_owned()),
    ] {
        manager.end_streamer(&streamer_id);
    }
}

/// Removes one registered coin, active or passive, ends its background work, and removes its records on its platform
/// (CRD ch.43 R43.1E.5 item 4).
pub async fn disable_coin(ctx: &MmArc, ticker: &str) -> Result<(), String> {
    disable_coin_of_instance(ctx, ticker, None).await.map(|_| ())
}

/// Like [`disable_coin`], for the cleanup of an activation that registered an instance and then failed: with
/// `instance` set, the coin is removed only when the registered instance of `ticker` is that one (CRD ch.55 R55.4.9).
/// Returns `false`, and changes nothing, when another instance of the ticker is registered.
pub async fn disable_coin_of_instance(ctx: &MmArc, ticker: &str, instance: Option<u64>) -> Result<bool, String> {
    let coins_ctx = try_s!(CoinsContext::from_ctx(ctx));
    let mut coins = coins_ctx.coins.lock().await;
    if let Some(instance) = instance {
        match coins.registered().instance_id(ticker) {
            Some(registered) if registered != instance => return Ok(false),
            _ => (),
        }
    }
    match coins.remove(ticker) {
        Some((removed, work)) => {
            coins_ctx.fee_estimate_cache.forget_coin(ticker);
            end_removed_instance_work(&removed, &work);
            end_event_streams_of(ctx, ticker);
            forget_token_records(&coins, &removed, ticker);
            Ok(true)
        },
        None => ERR!("{} is disabled already", ticker),
    }
}

/// Removes the registered platform coin `platform` and every one of its dependent tokens and layer-two coins in one
/// step, under one hold of the registry: the forced removal of CRD ch.43 R43.1E.5 item 4. Each dependent leaves
/// through the same record clean-up as a single [`disable_coin`], before the platform does. Returns the tickers
/// removed, the dependents first, and the NFT chain among them when it was a dependent. The background work of every
/// removed instance ends (CRD ch.43 R43.1E.5 item 4).
pub async fn disable_platform_with_dependents(ctx: &MmArc, platform: &str) -> Result<Vec<String>, String> {
    let coins_ctx = try_s!(CoinsContext::from_ctx(ctx));
    let mut coins = coins_ctx.coins.lock().await;
    // The platform must be registered before any dependent is removed.
    if !coins.registered().contains(platform) {
        return ERR!("{} is disabled already", platform);
    }
    let mut tickers: Vec<String> = coins.dependents_of(platform).into_iter().collect();
    tickers.push(platform.to_owned());
    let mut removed_tickers = Vec::with_capacity(tickers.len());
    for ticker in tickers {
        // A dependent that was removed meanwhile is simply not there; the platform must be.
        match coins.remove(&ticker) {
            Some((removed, work)) => {
                coins_ctx.fee_estimate_cache.forget_coin(&ticker);
                end_removed_instance_work(&removed, &work);
                end_event_streams_of(ctx, &ticker);
                forget_token_records(&coins, &removed, &ticker);
                removed_tickers.push(ticker);
            },
            None if ticker == platform => return ERR!("{} is disabled already", ticker),
            // The NFT chain of the platform is a dependent with no coin entry: it is deactivated with the others
            // (CRD ch.43 R43.1F.8 item 3).
            None => {
                if let Ok(chain) = <crate::nft::Chain as crate::nft::ChainTicker>::from_nft_ticker(&ticker) {
                    if let Ok(nft_ctx) = crate::nft::NftCtx::from_mm_ctx(ctx) {
                        nft_ctx.unmark_activated(chain);
                    }
                    removed_tickers.push(ticker);
                }
            },
        }
    }
    Ok(removed_tickers)
}

/// Takes a disabled token out of the record its platform keeps of its tokens, so that the platform no longer
/// reports it (CRD ch.40 T40.6B.20 for SPL; CRD ch.43 §43.1E for the other families). The platform and its other
/// tokens are untouched. Calling it again, or when the platform is not registered, changes nothing.
///
/// `registry` is the registry after the token was removed. Only the EVM and TRON families look the platform up
/// there, because their token holds just the platform ticker; the other families hold the platform itself.
pub(crate) fn forget_token_records(registry: &CoinRegistry, removed: &MmCoinEnum, ticker: &str) {
    match removed {
        MmCoinEnum::EthCoin(token) => {
            use crate::eth::EthCoinType;
            if let EthCoinType::Erc20 { platform, .. } | EthCoinType::Trc20 { platform, .. } = &token.coin_type {
                if let Some(MmCoinEnum::EthCoin(platform_coin)) = registry.registered().get(platform) {
                    platform_coin.remove_erc20_token_info(ticker);
                }
            }
        },
        MmCoinEnum::SlpToken(token) => token.platform_coin().remove_slp_token_info(ticker),
        MmCoinEnum::TendermintToken(token) => token.platform_coin.remove_activated_token_info(ticker),
        MmCoinEnum::SplToken(token) => token.platform_coin.remove_spl_token_info(ticker),
        _ => (),
    }
}
/// The request of `set_required_confirmations` (legacy members, unchanged).
#[derive(Deserialize)]
pub struct ConfirmationsReq {
    pub coin: String,
    pub confirmations: u64,
}
/// The success of `set_required_confirmations`: the content that the legacy body carries under `result`.
#[derive(Serialize)]
pub struct ConfirmationsResponse {
    pub coin: String,
    pub confirmations: u64,
}
/// The typed core of `set_required_confirmations`. The answer carries the setting as the coin now reports it.
pub async fn set_required_confirmations_core(
    ctx: MmArc,
    req: ConfirmationsReq,
) -> Result<ConfirmationsResponse, MmError<CoinSettingError>> {
    let coin = match lp_coinfind(&ctx, &req.coin).await {
        Ok(Some(t)) => t,
        Ok(None) => return MmError::err(CoinSettingError::NoSuchCoin { coin: req.coin.clone() }),
        Err(err) => {
            return MmError::err(CoinSettingError::InternalError(format!(
                "!lp_coinfind ({}): {}",
                req.coin, err
            )))
        },
    };
    coin.set_required_confirmations(req.confirmations);
    Ok(ConfirmationsResponse {
        coin: req.coin,
        confirmations: coin.required_confirmations(),
    })
}
/// The request of `set_requires_notarization` (legacy members, unchanged).
#[derive(Deserialize)]
pub struct RequiresNotaReq {
    pub coin: String,
    pub requires_notarization: bool,
}
/// The success of `set_requires_notarization`: the content that the legacy body carries under `result`.
#[derive(Serialize)]
pub struct RequiresNotaResponse {
    pub coin: String,
    pub requires_notarization: bool,
}
/// The typed core of `set_requires_notarization`. The answer carries the setting as the coin now reports it.
pub async fn set_requires_notarization_core(
    ctx: MmArc,
    req: RequiresNotaReq,
) -> Result<RequiresNotaResponse, MmError<CoinSettingError>> {
    let coin = match lp_coinfind(&ctx, &req.coin).await {
        Ok(Some(t)) => t,
        Ok(None) => return MmError::err(CoinSettingError::NoSuchCoin { coin: req.coin.clone() }),
        Err(err) => {
            return MmError::err(CoinSettingError::InternalError(format!(
                "!lp_coinfind ({}): {}",
                req.coin, err
            )))
        },
    };
    coin.set_requires_notarization(req.requires_notarization);
    Ok(RequiresNotaResponse {
        coin: req.coin,
        requires_notarization: coin.requires_notarization(),
    })
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
pub async fn register_balance_update_handler(
    ctx: MmArc,
    handler: Box<dyn BalanceTradeFeeUpdatedHandler + Send + Sync>,
) {
    let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();
    coins_ctx.balance_update_handlers.lock().await.push(handler);
}
/// Registers the swap layer's reader of the saved swaps that are not finished, which the activation of a
/// shielded coin checks its key against (CRD ch.39 R39.8.0bd). A second registration replaces the first.
///
/// # Errors
///
/// Returns the reason when the coins context cannot be created.
pub fn register_unfinished_swaps_view(
    ctx: &MmArc,
    view: Arc<dyn crate::z_coin::UnfinishedSwapsView>,
) -> Result<(), String> {
    let coins_ctx = CoinsContext::from_ctx(ctx)?;
    coins_ctx.set_unfinished_swaps_view(view);
    Ok(())
}
/// Whether a reader of the unfinished swaps is registered in this context.
///
/// # Errors
///
/// Returns the reason when the coins context cannot be created.
pub fn unfinished_swaps_view_registered(ctx: &MmArc) -> Result<bool, String> {
    Ok(CoinsContext::from_ctx(ctx)?.has_unfinished_swaps_view())
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
                    // This legacy-upgrade path never knows a chain_id, so build the
                    // wire shape directly instead of round-tripping
                    // `CoinProtocol::ETH { chain_id: None }` through serde: the
                    // adjacently-tagged derive emits `"protocol_data": {"chain_id":
                    // null}` for that value (chain_id's `#[serde(default)]` only
                    // affects deserialization, not serialization), which is a
                    // config-shape regression a pre-chain_id `coins` file never
                    // had and callers do not expect from this upgrade step.
                    json!({ "type": "ETH" })
                } else {
                    let contract_address = etomic.to_owned();
                    json::to_value(CoinProtocol::ERC20 {
                        platform: "ETH".into(),
                        contract_address,
                    })
                    .map_err(|e| ERRL!("Error {:?} on process {:?}", e, coin_as_str))?
                }
            },
            _ => json::to_value(CoinProtocol::UTXO).map_err(|e| ERRL!("Error {:?} on process {:?}", e, coin_as_str))?,
        };

        coin.insert("protocol".into(), protocol);
    }

    Ok(config)
}
/// The request of `convert_utxo_address` (legacy members, unchanged).
#[derive(Deserialize)]
pub struct ConvertUtxoAddressReq {
    pub address: String,
    pub to_coin: String,
}
/// The typed core of `convert_utxo_address`: the address in the format of the target coin. The success is the string
/// that the legacy body carries under `result`.
pub async fn convert_utxo_address_core(
    ctx: MmArc,
    req: ConvertUtxoAddressReq,
) -> Result<String, MmError<ConvertUtxoAddressError>> {
    let mut addr = req
        .address
        .parse::<utxo::Address>()
        .map_to_mm(|e| ConvertUtxoAddressError::InvalidAddress(format!("{}", e)))?;
    let coin = match lp_coinfind(&ctx, &req.to_coin).await {
        Ok(Some(c)) => c,
        _ => {
            return MmError::err(ConvertUtxoAddressError::NoSuchCoin {
                coin: req.to_coin.clone(),
            })
        },
    };
    let coin = match coin {
        MmCoinEnum::UtxoCoin(utxo) => utxo,
        _ => {
            return MmError::err(ConvertUtxoAddressError::UnsupportedError(format!(
                "Coin {} is not utxo",
                req.to_coin
            )))
        },
    };
    addr.prefix = coin.as_ref().conf.pub_addr_prefix;
    addr.t_addr_prefix = coin.as_ref().conf.pub_t_addr_prefix;
    addr.checksum_type = coin.as_ref().conf.checksum_type;
    Ok(addr.to_string())
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

#[cfg(test)]
mod send_raw_transaction_carrier_tests {
    //! `send_raw_transaction`'s carrier selection (CRD ch.20 R-W8, T-W2/T-W3).
    //! Only the selection is exercised here: the broadcast itself needs an
    //! activated coin and a live node.

    use super::*;

    #[test]
    fn a_hex_carrier_is_selected() {
        let req = json!({ "coin": "SC", "tx_hex": "7b7d" });
        assert_eq!(select_raw_tx_carrier(&req), Ok(RawTxCarrier::Hex("7b7d")));
    }

    #[test]
    fn a_json_carrier_is_selected_when_hex_is_absent() {
        let req = json!({ "coin": "SC", "tx_json": { "minerFee": "0" } });
        let expected = json!({ "minerFee": "0" });
        assert_eq!(select_raw_tx_carrier(&req), Ok(RawTxCarrier::Json(&expected)));
    }

    /// T-W2: a caller that echoes back a whole transaction-details object hands
    /// in both carriers. `tx_hex` wins, so the same transaction is broadcast
    /// whichever coin the object came from.
    #[test]
    fn hex_wins_when_both_carriers_are_present() {
        let req = json!({ "coin": "SC", "tx_hex": "7b7d", "tx_json": { "minerFee": "0" } });
        assert_eq!(select_raw_tx_carrier(&req), Ok(RawTxCarrier::Hex("7b7d")));
    }

    /// T-W2: a malformed selected carrier is an error, never a reason to fall
    /// through to the other one -- falling through would broadcast a
    /// transaction the caller did not select.
    #[test]
    fn a_malformed_hex_carrier_does_not_fall_through_to_json() {
        let req = json!({ "coin": "SC", "tx_hex": 42, "tx_json": { "minerFee": "0" } });
        assert!(select_raw_tx_carrier(&req).is_err());
    }

    /// T-W3: neither carrier present fails validation, naming both fields.
    #[test]
    fn neither_carrier_is_a_validation_error_naming_both() {
        let err = select_raw_tx_carrier(&json!({ "coin": "SC" })).expect_err("neither carrier is supplied");
        assert!(err.contains("tx_hex"), "{err}");
        assert!(err.contains("tx_json"), "{err}");
    }

    /// A `null` counts as absent, so echoing an object whose optional carrier
    /// serialised as `null` behaves like omitting it rather than failing.
    #[test]
    fn a_null_carrier_counts_as_absent() {
        let req = json!({ "coin": "SC", "tx_hex": null, "tx_json": { "minerFee": "0" } });
        let expected = json!({ "minerFee": "0" });
        assert_eq!(select_raw_tx_carrier(&req), Ok(RawTxCarrier::Json(&expected)));
    }

    #[test]
    fn a_non_object_json_carrier_is_rejected() {
        let req = json!({ "coin": "SC", "tx_json": "not an object" });
        assert!(select_raw_tx_carrier(&req).is_err());
    }
}

#[cfg(test)]
mod postponed_family_switch_tests {
    use super::*;

    /// Flipping a family switch is a deliberate change: this test fails when someone does, and must be updated in
    /// the same commit. TRON can be activated since 2026-11-20 (CRD ch.21 D6) and Solana since its switch-on (CRD
    /// ch.40 D40.4), which left no activation switch for it; the swaps of both families stay off.
    #[test]
    fn family_switches_have_their_documented_values() {
        assert!(TRON_ACTIVATION_AVAILABLE);
        assert!(!TRON_SWAPS_AVAILABLE);
        assert!(!TENDERMINT_SWAPS_AVAILABLE);
    }
}
