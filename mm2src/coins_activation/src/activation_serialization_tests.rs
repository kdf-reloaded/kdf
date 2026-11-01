//! Serialized same-ticker activation (CRD ch. 55 R55.1.12, T55.21).
//!
//! Every activation family is driven through a scripted coin: the platform-with-tokens, token and layer-2
//! families as tasks and as one-shot calls here, the standalone-coin task in its own test module (which
//! shares the cases below). The scripted build counts itself per ticker and can wait on a per-ticker gate, so
//! a test holds one activation in the middle of its build while a second one of the same ticker starts.
//!
//! The cases are generic over a [`Family`], so one body covers every family.

use crate::context::{lock_activation, CoinsActivationContext};
use crate::init_platform_coin_with_tokens::{cancel_init_platform_coin_with_tokens, init_platform_coin_with_tokens,
                                            init_platform_coin_with_tokens_status,
                                            InitPlatformCoinWithTokensActivationOps, InitPlatformCoinWithTokensTask,
                                            InitPlatformCoinWithTokensTaskManagerShared};
use crate::init_token::{cancel_init_token, init_token, init_token_status, InitTokenActivationOps, InitTokenError,
                        InitTokenInProgressStatus, InitTokenTaskHandle, InitTokenTaskManagerShared};
use crate::l2::{cancel_l2_activation, enable_l2, init_l2, init_l2_status, EnableL2Error, InitL2ActivationOps,
                L2ActivationError, L2ActivationOps, L2ActivationTask, L2InitialStatus, L2ProtocolParams,
                L2TaskManagerShared};
use crate::platform_coin_with_tokens::{enable_platform_coin_with_tokens, EnablePlatformCoinWithTokensError,
                                       GetPlatformBalance, PlatformWithTokensActivationOps, TokenAsMmCoinInitializer};
use crate::prelude::{CurrentBlock, TryFromCoinProtocol, TryPlatformCoinFromMmCoinEnum, TxHistory};
use crate::token::{enable_token, EnableTokenError, TokenActivationOps, TokenProtocolParams};
use async_trait::async_trait;
use coins::hd_pubkey::HDXPubExtractor;
use coins::my_tx_history_v2::TxHistoryStorage;
use coins::{lp_coinfind, lp_register_coin, CoinProtocol, CoinsContext, MmCoinEnum, RegisterCoinError,
            RegisterCoinParams, TestCoin};
use common::executor::Timer;
use common::mm_number::BigDecimal;
use crypto::hw_rpc_task::{HwRpcTaskAwaitingStatus, HwRpcTaskUserAction};
use derive_more::Display;
use futures::future::{abortable, AbortHandle, FutureExt};
use mm2_core::mm_ctx::{MmArc, MmCtxBuilder};
use mm2_err_handle::prelude::*;
use mm2_metrics::MetricsArc;
use rpc_task::rpc_common::RpcTaskStatusRequest;
use rpc_task::RpcTaskHandle;
use ser_error_derive::SerializeErrorType;
use serde::Serialize;
use serde_derive::Deserialize;
use serde_json::{json, Value as Json};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The platform coin the token and layer-2 families rest on; the test context registers it.
const PLATFORM_TICKER: &str = "PLAT";

// ---------------------------------------------------------------------------------------------------------
// The scripted build: a per-ticker build counter and a per-ticker gate.
// ---------------------------------------------------------------------------------------------------------

#[derive(Default)]
struct ScriptState {
    builds: HashMap<String, usize>,
    open_gates: HashSet<String>,
}

fn script_state() -> &'static Mutex<ScriptState> {
    static STATE: OnceLock<Mutex<ScriptState>> = OnceLock::new();
    STATE.get_or_init(Default::default)
}

/// How many times the coin of `ticker` was built. Tests use tickers of their own, so the counts do not mix.
pub(crate) fn builds(ticker: &str) -> usize {
    script_state()
        .lock()
        .unwrap()
        .builds
        .get(ticker)
        .copied()
        .unwrap_or_default()
}

/// Lets every build of `ticker` that waits on the gate go on, now and later.
pub(crate) fn open_gate(ticker: &str) { script_state().lock().unwrap().open_gates.insert(ticker.to_owned()); }

/// The scripted build of one coin: counts itself, then waits on the gate of the ticker when the script asks
/// for it (`gate`, `gate_fail`).
pub(crate) async fn scripted_build(ticker: &str, script: &str) {
    *script_state()
        .lock()
        .unwrap()
        .builds
        .entry(ticker.to_owned())
        .or_default() += 1;
    if script.starts_with("gate") {
        while !script_state().lock().unwrap().open_gates.contains(ticker) {
            Timer::sleep(0.01).await;
        }
    }
}

/// Whether the script ends the activation with a failure after the build.
pub(crate) fn scripted_failure(script: &str) -> bool { script == "gate_fail" }

/// A test ticker nobody else uses, so the per-ticker counters and gates stay apart.
pub(crate) fn unique_ticker(prefix: &str) -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    format!("{}{}", prefix, NEXT.fetch_add(1, Ordering::SeqCst))
}

/// A context that configures every ticker as a plain UTXO coin (which the scripted protocol of every family
/// accepts) and has the platform coin of the token and layer-2 families registered.
pub(crate) fn serialization_ctx(tickers: &[&str]) -> MmArc {
    let mut coins: Vec<Json> = tickers
        .iter()
        .map(|ticker| json!({ "coin": ticker, "protocol": { "type": "UTXO" } }))
        .collect();
    coins.push(json!({ "coin": PLATFORM_TICKER, "protocol": { "type": "UTXO" } }));
    let ctx = MmCtxBuilder::new().with_conf(json!({ "coins": coins })).into_mm_arc();
    crypto::CryptoCtx::init_with_iguana_passphrase(ctx.clone(), "activation serialization test passphrase")
        .expect("initialize the crypto context");
    common::block_on(lp_register_coin(
        &ctx,
        TestCoin::new(PLATFORM_TICKER).into(),
        RegisterCoinParams {
            ticker: PLATFORM_TICKER.to_owned(),
            tx_history: false,
        },
    ))
    .unwrap_or_else(|e| panic!("cannot register the platform coin: {}", e));
    ctx
}

pub(crate) fn is_registered(ctx: &MmArc, ticker: &str) -> bool {
    common::block_on(lp_coinfind(ctx, ticker)).unwrap().is_some()
}

// ---------------------------------------------------------------------------------------------------------
// The scripted types shared by the families.
// ---------------------------------------------------------------------------------------------------------

/// The `activation_params` of the token and layer-2 families.
#[derive(Clone, Debug, Deserialize)]
pub struct ScriptedParams {
    script: String,
}

impl TxHistory for ScriptedParams {
    fn tx_history(&self) -> bool { false }
}

/// The request of the platform family: the script and the tokens it would register beside the platform.
#[derive(Clone, Debug, Deserialize)]
pub struct ScriptedPlatformRequest {
    script: String,
    #[serde(default)]
    tokens: Vec<String>,
}

impl TxHistory for ScriptedPlatformRequest {
    fn tx_history(&self) -> bool { false }
}

/// A plain UTXO coin is a platform coin of its own; an ERC20 entry names its platform.
#[derive(Clone)]
pub struct ScriptedProtocol {
    platform: String,
}

impl TryFromCoinProtocol for ScriptedProtocol {
    fn try_from_coin_protocol(proto: CoinProtocol) -> Result<Self, MmError<CoinProtocol>> {
        match proto {
            CoinProtocol::UTXO => Ok(ScriptedProtocol {
                platform: PLATFORM_TICKER.to_owned(),
            }),
            CoinProtocol::ERC20 { platform, .. } => Ok(ScriptedProtocol { platform }),
            protocol => MmError::err(protocol),
        }
    }
}

impl TokenProtocolParams for ScriptedProtocol {
    fn platform_coin_ticker(&self) -> &str { &self.platform }
}

impl L2ProtocolParams for ScriptedProtocol {
    fn platform_coin_ticker(&self) -> &str { &self.platform }
}

#[derive(Clone, Debug, Serialize)]
pub struct ScriptedResult {
    current_block: u64,
}

impl CurrentBlock for ScriptedResult {
    fn current_block(&self) -> u64 { self.current_block }
}

impl GetPlatformBalance for ScriptedResult {
    fn get_platform_balance(&self) -> BigDecimal { BigDecimal::from(0) }
}

impl TryPlatformCoinFromMmCoinEnum for TestCoin {
    fn try_from_mm_coin(coin: MmCoinEnum) -> Option<Self> {
        match coin {
            MmCoinEnum::Test(coin) => Some(coin),
            _ => None,
        }
    }
}

fn scripted_result() -> ScriptedResult { ScriptedResult { current_block: 7 } }

// ---------------------------------------------------------------------------------------------------------
// The platform-with-tokens family.
// ---------------------------------------------------------------------------------------------------------

#[async_trait]
impl PlatformWithTokensActivationOps for TestCoin {
    type ActivationRequest = ScriptedPlatformRequest;
    type PlatformProtocolInfo = ScriptedProtocol;
    type ActivationResult = ScriptedResult;
    type ActivationError = EnablePlatformCoinWithTokensError;

    async fn enable_platform_coin(
        _ctx: MmArc,
        ticker: String,
        _coin_conf: Json,
        activation_request: Self::ActivationRequest,
        _protocol_conf: Self::PlatformProtocolInfo,
        _priv_key: &[u8],
    ) -> Result<Self, MmError<Self::ActivationError>> {
        scripted_build(&ticker, &activation_request.script).await;
        Ok(TestCoin::new(&ticker))
    }

    fn token_initializers(
        &self,
    ) -> Vec<Box<dyn TokenAsMmCoinInitializer<PlatformCoin = Self, ActivationRequest = Self::ActivationRequest>>> {
        Vec::new()
    }

    fn requested_token_tickers(activation_request: &Self::ActivationRequest) -> Vec<String> {
        activation_request.tokens.clone()
    }

    async fn get_activation_result<XPubExtractor>(
        &self,
        _ctx: &MmArc,
        activation_request: &Self::ActivationRequest,
        _xpub_extractor: Option<&XPubExtractor>,
    ) -> Result<Self::ActivationResult, MmError<Self::ActivationError>>
    where
        XPubExtractor: HDXPubExtractor + Sync,
    {
        if scripted_failure(&activation_request.script) {
            return MmError::err(EnablePlatformCoinWithTokensError::Internal(
                "scripted failure".to_owned(),
            ));
        }
        Ok(scripted_result())
    }

    fn start_history_background_fetching(
        &self,
        _ctx: MmArc,
        _metrics: MetricsArc,
        _storage: impl TxHistoryStorage + 'static,
        _initial_balance: BigDecimal,
    ) -> AbortHandle {
        AbortHandle::new_pair().0
    }
}

#[async_trait]
impl InitPlatformCoinWithTokensActivationOps for TestCoin {
    fn rpc_task_manager(activation_ctx: &CoinsActivationContext) -> &InitPlatformCoinWithTokensTaskManagerShared<Self> {
        &activation_ctx.init_test_platform_task_manager
    }

    async fn enable_platform_coin_with_task(
        ctx: MmArc,
        ticker: String,
        coin_conf: Json,
        activation_request: ScriptedPlatformRequest,
        protocol_conf: ScriptedProtocol,
        priv_key: &[u8],
        _task_handle: &RpcTaskHandle<InitPlatformCoinWithTokensTask<Self>>,
    ) -> Result<Self, MmError<EnablePlatformCoinWithTokensError>> {
        <TestCoin as PlatformWithTokensActivationOps>::enable_platform_coin(
            ctx,
            ticker,
            coin_conf,
            activation_request,
            protocol_conf,
            priv_key,
        )
        .await
    }
}

// ---------------------------------------------------------------------------------------------------------
// The token family.
// ---------------------------------------------------------------------------------------------------------

#[async_trait]
impl TokenActivationOps for TestCoin {
    type PlatformCoin = TestCoin;
    type ActivationParams = ScriptedParams;
    type ProtocolInfo = ScriptedProtocol;
    type ActivationResult = ScriptedResult;
    type ActivationError = EnableTokenError;

    async fn enable_token(
        ticker: String,
        _platform_coin: TestCoin,
        activation_params: ScriptedParams,
        _protocol_conf: ScriptedProtocol,
    ) -> Result<(Self, ScriptedResult), MmError<EnableTokenError>> {
        scripted_build(&ticker, &activation_params.script).await;
        if scripted_failure(&activation_params.script) {
            return MmError::err(EnableTokenError::Internal("scripted failure".to_owned()));
        }
        Ok((TestCoin::new(&ticker), scripted_result()))
    }
}

#[async_trait]
impl InitTokenActivationOps for TestCoin {
    type PlatformCoin = TestCoin;
    type ActivationRequest = ScriptedParams;
    type ProtocolInfo = ScriptedProtocol;
    type ActivationResult = ScriptedResult;

    fn rpc_task_manager(activation_ctx: &CoinsActivationContext) -> &InitTokenTaskManagerShared<Self> {
        &activation_ctx.init_test_token_task_manager
    }

    fn resolve_protocol(
        ctx: &MmArc,
        ticker: &str,
        _inline_protocol: Option<&CoinProtocol>,
    ) -> Result<ScriptedProtocol, InitTokenError> {
        crate::prelude::coin_conf_with_protocol(ctx, ticker)
            .map(|(_, protocol): (_, ScriptedProtocol)| protocol)
            .map_err(|e| InitTokenError::Internal(format!("{:?}", e.get_inner())))
    }

    async fn init_token(
        _ctx: &MmArc,
        ticker: &str,
        _platform_coin: &TestCoin,
        activation_request: &ScriptedParams,
        _protocol_info: &ScriptedProtocol,
        task_handle: &InitTokenTaskHandle<Self>,
    ) -> Result<(Self, ScriptedResult), MmError<InitTokenError>> {
        task_handle
            .update_in_progress_status(InitTokenInProgressStatus::ActivatingCoin)
            .map_err(|e| MmError::new(InitTokenError::from(e.into_inner())))?;
        scripted_build(ticker, &activation_request.script).await;
        if scripted_failure(&activation_request.script) {
            return MmError::err(InitTokenError::Internal("scripted failure".to_owned()));
        }
        Ok((TestCoin::new(ticker), scripted_result()))
    }

    fn register_token_info(_platform_coin: &TestCoin, _token: &Self) {}

    fn unregister_token_info(_platform_coin: &TestCoin, _ticker: &str) {}
}

// ---------------------------------------------------------------------------------------------------------
// The layer-2 family.
// ---------------------------------------------------------------------------------------------------------

#[derive(Clone, Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum ScriptedL2Error {
    #[display(fmt = "Layer 2 {} is already activated", _0)]
    L2IsAlreadyActivated(String),
    Internal(String),
}

impl From<RegisterCoinError> for ScriptedL2Error {
    fn from(err: RegisterCoinError) -> Self {
        match err {
            RegisterCoinError::CoinIsInitializedAlready { coin } => ScriptedL2Error::L2IsAlreadyActivated(coin),
            RegisterCoinError::Internal(e) => ScriptedL2Error::Internal(e),
        }
    }
}

impl From<ScriptedL2Error> for L2ActivationError {
    fn from(err: ScriptedL2Error) -> Self {
        match err {
            ScriptedL2Error::L2IsAlreadyActivated(ticker) => L2ActivationError::L2IsAlreadyActivated(ticker),
            ScriptedL2Error::Internal(e) => L2ActivationError::Internal(e),
        }
    }
}

impl From<ScriptedL2Error> for EnableL2Error {
    fn from(err: ScriptedL2Error) -> Self {
        match err {
            ScriptedL2Error::L2IsAlreadyActivated(ticker) => EnableL2Error::L2IsAlreadyActivated(ticker),
            ScriptedL2Error::Internal(e) => EnableL2Error::Internal(e),
        }
    }
}

#[derive(Clone, Serialize)]
pub enum ScriptedL2Status {
    ActivatingCoin,
}

impl L2InitialStatus for ScriptedL2Status {
    fn initial_status() -> Self { ScriptedL2Status::ActivatingCoin }
}

async fn scripted_l2(
    ticker: &str,
    params: &ScriptedParams,
) -> Result<(TestCoin, ScriptedResult), MmError<ScriptedL2Error>> {
    scripted_build(ticker, &params.script).await;
    if scripted_failure(&params.script) {
        return MmError::err(ScriptedL2Error::Internal("scripted failure".to_owned()));
    }
    Ok((TestCoin::new(ticker), scripted_result()))
}

#[async_trait]
impl L2ActivationOps for TestCoin {
    type PlatformCoin = TestCoin;
    type ActivationParams = ScriptedParams;
    type ProtocolInfo = ScriptedProtocol;
    type ValidatedParams = ScriptedParams;
    type CoinConf = Json;
    type ActivationResult = ScriptedResult;
    type ActivationError = ScriptedL2Error;

    fn coin_conf_from_json(json: Json) -> Result<Json, MmError<ScriptedL2Error>> { Ok(json) }

    fn validate_platform_configuration(
        _ctx: &MmArc,
        _platform_coin: &TestCoin,
    ) -> Result<(), MmError<ScriptedL2Error>> {
        Ok(())
    }

    fn validate_activation_params(
        activation_params: ScriptedParams,
    ) -> Result<ScriptedParams, MmError<ScriptedL2Error>> {
        Ok(activation_params)
    }

    async fn enable_l2(
        _ctx: &MmArc,
        _platform_coin: TestCoin,
        validated_params: ScriptedParams,
        protocol_conf: ScriptedProtocol,
        coin_conf: Json,
    ) -> Result<(Self, ScriptedResult), MmError<ScriptedL2Error>> {
        let _ = protocol_conf;
        let ticker = coin_conf["coin"].as_str().unwrap_or_default().to_owned();
        scripted_l2(&ticker, &validated_params).await
    }
}

#[async_trait]
impl InitL2ActivationOps for TestCoin {
    type PlatformCoin = TestCoin;
    type ActivationParams = ScriptedParams;
    type ProtocolInfo = ScriptedProtocol;
    type ValidatedParams = ScriptedParams;
    type CoinConf = Json;
    type ActivationResult = ScriptedResult;
    type ActivationError = ScriptedL2Error;
    type InProgressStatus = ScriptedL2Status;
    type AwaitingStatus = HwRpcTaskAwaitingStatus;
    type UserAction = HwRpcTaskUserAction;

    fn rpc_task_manager(activation_ctx: &CoinsActivationContext) -> &L2TaskManagerShared<Self> {
        &activation_ctx.init_test_l2_task_manager
    }

    fn coin_conf_from_json(json: Json) -> Result<Json, MmError<ScriptedL2Error>> { Ok(json) }

    fn validate_platform_configuration(
        _ctx: &MmArc,
        _platform_coin: &TestCoin,
    ) -> Result<(), MmError<ScriptedL2Error>> {
        Ok(())
    }

    fn validate_activation_params(
        activation_params: ScriptedParams,
    ) -> Result<ScriptedParams, MmError<ScriptedL2Error>> {
        Ok(activation_params)
    }

    async fn init_l2(
        _ctx: &MmArc,
        _platform_coin: TestCoin,
        validated_params: ScriptedParams,
        _protocol_conf: ScriptedProtocol,
        coin_conf: Json,
        _task_handle: &RpcTaskHandle<L2ActivationTask<Self>>,
    ) -> Result<(Self, ScriptedResult), MmError<ScriptedL2Error>> {
        let ticker = coin_conf["coin"].as_str().unwrap_or_default().to_owned();
        scripted_l2(&ticker, &validated_params).await
    }
}

// ---------------------------------------------------------------------------------------------------------
// The families as the cases drive them.
// ---------------------------------------------------------------------------------------------------------

/// How an activation ended.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Outcome {
    /// The activation succeeded.
    Registered,
    /// It ended with the already-activated discriminant of its family.
    AlreadyActivated,
    /// It ended with another failure.
    Failed(String),
}

/// An activation in flight.
pub(crate) struct Running {
    poll: Box<dyn Fn() -> Option<Outcome> + Send + Sync>,
    cancel: Box<dyn Fn() + Send + Sync>,
}

impl Running {
    pub(crate) fn new(
        poll: impl Fn() -> Option<Outcome> + Send + Sync + 'static,
        cancel: impl Fn() + Send + Sync + 'static,
    ) -> Running {
        Running {
            poll: Box::new(poll),
            cancel: Box::new(cancel),
        }
    }

    /// The outcome, or `None` while the activation is still running.
    pub(crate) fn poll(&self) -> Option<Outcome> { (self.poll)() }

    /// Cancels the task, or drops the one-shot call the way a closed connection does.
    pub(crate) fn cancel(&self) { (self.cancel)() }

    fn wait(&self) -> Outcome {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(outcome) = self.poll() {
                return outcome;
            }
            assert!(Instant::now() < deadline, "the activation did not finish in time");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// The outcome a task status answer stands for, or `None` while the task is running.
pub(crate) fn outcome_of_status(status: &Json, already_activated: &str) -> Option<Outcome> {
    match status["status"].as_str() {
        Some("Ok") => Some(Outcome::Registered),
        Some("Error") if status["details"]["error_type"] == json!(already_activated) => Some(Outcome::AlreadyActivated),
        Some("Error") => Some(Outcome::Failed(status.to_string())),
        _ => None,
    }
}

/// Runs a one-shot call in the background; the returned handle reports its outcome and aborts it on cancel.
fn spawn_one_shot<T, E>(
    already_activated: &'static str,
    call: impl Future<Output = Result<T, MmError<E>>> + Send + 'static,
) -> Running
where
    T: Send + 'static,
    E: NotMmError + Serialize + Send + 'static,
{
    let slot = Arc::new(Mutex::new(None));
    let slot_writer = slot.clone();
    let (call, abort) = abortable(async move {
        let outcome = match call.await {
            Ok(_) => Outcome::Registered,
            Err(error) => {
                let wire = serde_json::to_value(error.get_inner()).unwrap();
                if wire["error_type"] == json!(already_activated) {
                    Outcome::AlreadyActivated
                } else {
                    Outcome::Failed(wire.to_string())
                }
            },
        };
        *slot_writer.lock().unwrap() = Some(outcome);
    });
    common::executor::spawn(async move {
        let _ = call.await;
    });
    Running::new(move || slot.lock().unwrap().clone(), move || abort.abort())
}

/// An activation family the cases drive.
pub(crate) trait Family {
    const NAME: &'static str;

    /// Starts an activation of `ticker` with the script and returns it in flight.
    fn start(ctx: &MmArc, ticker: &str, script: &str) -> Running;
}

macro_rules! task_family {
    ($name:ident, $label:literal, $already:literal, $init:expr, $status:expr, $cancel:expr, $request:expr) => {
        pub(crate) struct $name;

        impl Family for $name {
            const NAME: &'static str = $label;

            fn start(ctx: &MmArc, ticker: &str, script: &str) -> Running {
                let request = $request(ticker, script);
                let task_id = common::block_on($init(ctx.clone(), request))
                    .unwrap_or_else(|e| panic!("{}: init failed: {}", $label, e))
                    .task_id;
                let (status_ctx, cancel_ctx) = (ctx.clone(), ctx.clone());
                Running::new(
                    move || {
                        let req = RpcTaskStatusRequest {
                            task_id,
                            forget_if_finished: false,
                        };
                        let status = common::block_on($status(status_ctx.clone(), req))
                            .unwrap_or_else(|e| panic!("{}: status failed: {}", $label, e));
                        outcome_of_status(&serde_json::to_value(status).unwrap(), $already)
                    },
                    move || {
                        let req = RpcTaskStatusRequest {
                            task_id,
                            forget_if_finished: true,
                        };
                        common::block_on($cancel(cancel_ctx.clone(), req))
                            .unwrap_or_else(|e| panic!("{}: cancel failed: {}", $label, e));
                    },
                )
            }
        }
    };
}

fn platform_request(ticker: &str, script: &str) -> Json { json!({ "ticker": ticker, "script": script }) }

fn token_request(ticker: &str, script: &str) -> Json {
    json!({ "ticker": ticker, "activation_params": { "script": script } })
}

fn from_json<T: serde::de::DeserializeOwned>(request: Json) -> T {
    serde_json::from_value(request).unwrap_or_else(|e| panic!("bad request: {}", e))
}

task_family!(
    PlatformTask,
    "platform task",
    "PlatformIsAlreadyActivated",
    |ctx, request: Json| init_platform_coin_with_tokens::<TestCoin>(ctx, from_json(request)),
    init_platform_coin_with_tokens_status::<TestCoin>,
    cancel_init_platform_coin_with_tokens::<TestCoin>,
    platform_request
);

task_family!(
    TokenTask,
    "token task",
    "TokenIsAlreadyActivated",
    |ctx, request: Json| init_token::<TestCoin>(ctx, from_json(request)),
    init_token_status::<TestCoin>,
    cancel_init_token::<TestCoin>,
    token_request
);

task_family!(
    L2Task,
    "layer-2 task",
    "L2IsAlreadyActivated",
    |ctx, request: Json| init_l2::<TestCoin>(ctx, from_json(request)),
    init_l2_status::<TestCoin>,
    cancel_l2_activation::<TestCoin>,
    token_request
);

pub(crate) struct PlatformOneShot;

impl Family for PlatformOneShot {
    const NAME: &'static str = "platform one-shot";

    fn start(ctx: &MmArc, ticker: &str, script: &str) -> Running {
        let request = from_json(platform_request(ticker, script));
        spawn_one_shot(
            "PlatformIsAlreadyActivated",
            enable_platform_coin_with_tokens::<TestCoin>(ctx.clone(), request),
        )
    }
}

pub(crate) struct TokenOneShot;

impl Family for TokenOneShot {
    const NAME: &'static str = "token one-shot";

    fn start(ctx: &MmArc, ticker: &str, script: &str) -> Running {
        let request = from_json(token_request(ticker, script));
        spawn_one_shot(
            "TokenIsAlreadyActivated",
            enable_token::<TestCoin>(ctx.clone(), request),
        )
    }
}

pub(crate) struct L2OneShot;

impl Family for L2OneShot {
    const NAME: &'static str = "layer-2 one-shot";

    fn start(ctx: &MmArc, ticker: &str, script: &str) -> Running {
        let request = from_json(token_request(ticker, script));
        spawn_one_shot("L2IsAlreadyActivated", enable_l2::<TestCoin>(ctx.clone(), request))
    }
}

// ---------------------------------------------------------------------------------------------------------
// T55.21: the cases.
// ---------------------------------------------------------------------------------------------------------

fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for: {}", what);
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// How long a test lets a waiting activation prove that it does not move.
fn settle() { std::thread::sleep(Duration::from_millis(300)); }

/// T55.21 (a): the second activation waits for the first, ends with the already-activated discriminant of its
/// family, and the coin is built only once.
pub(crate) fn case_a_second_waits_and_the_coin_is_built_once<F: Family>() {
    let ticker = unique_ticker("SA");
    let ctx = serialization_ctx(&[&ticker]);

    let first = F::start(&ctx, &ticker, "gate");
    wait_until("the first build to start", || builds(&ticker) == 1);
    let second = F::start(&ctx, &ticker, "ok");
    settle();
    assert_eq!(second.poll(), None, "{}: the second activation did not wait", F::NAME);
    assert_eq!(
        builds(&ticker),
        1,
        "{}: the second activation built a coin meanwhile",
        F::NAME
    );

    open_gate(&ticker);
    assert_eq!(first.wait(), Outcome::Registered, "{}", F::NAME);
    assert_eq!(second.wait(), Outcome::AlreadyActivated, "{}", F::NAME);
    assert_eq!(builds(&ticker), 1, "{}: the coin was built twice", F::NAME);
    assert!(is_registered(&ctx, &ticker), "{}", F::NAME);
}

/// T55.21 (b): when the first activation failed, the second proceeds and activates the coin.
pub(crate) fn case_b_second_proceeds_after_a_failure<F: Family>() {
    let ticker = unique_ticker("SB");
    let ctx = serialization_ctx(&[&ticker]);

    let first = F::start(&ctx, &ticker, "gate_fail");
    wait_until("the first build to start", || builds(&ticker) == 1);
    let second = F::start(&ctx, &ticker, "ok");
    settle();
    assert_eq!(second.poll(), None, "{}: the second activation did not wait", F::NAME);

    open_gate(&ticker);
    assert!(matches!(first.wait(), Outcome::Failed(_)), "{}", F::NAME);
    assert_eq!(second.wait(), Outcome::Registered, "{}", F::NAME);
    assert_eq!(builds(&ticker), 2, "{}", F::NAME);
    assert!(is_registered(&ctx, &ticker), "{}", F::NAME);
}

/// T55.21 (c): an activation of `Second`'s family waits for a running activation of `First`'s family, whichever
/// path each takes, and ends with its own family's discriminant.
pub(crate) fn case_c_two_paths_wait_for_each_other<First: Family, Second: Family>() {
    let ticker = unique_ticker("SC");
    let ctx = serialization_ctx(&[&ticker]);

    let first = First::start(&ctx, &ticker, "gate");
    wait_until("the first build to start", || builds(&ticker) == 1);
    let second = Second::start(&ctx, &ticker, "ok");
    settle();
    let names = (First::NAME, Second::NAME);
    assert_eq!(second.poll(), None, "{:?}: the second path did not wait", names);
    assert_eq!(
        builds(&ticker),
        1,
        "{:?}: the second path built a coin meanwhile",
        names
    );

    open_gate(&ticker);
    assert_eq!(first.wait(), Outcome::Registered, "{:?}", names);
    assert_eq!(second.wait(), Outcome::AlreadyActivated, "{:?}", names);
    assert_eq!(builds(&ticker), 1, "{:?}", names);
}

/// T55.21 (d): activations of two different tickers do not wait for each other.
pub(crate) fn case_d_different_tickers_do_not_wait<F: Family>() {
    let (held, other) = (unique_ticker("SD"), unique_ticker("SD"));
    let ctx = serialization_ctx(&[&held, &other]);

    let first = F::start(&ctx, &held, "gate");
    wait_until("the first build to start", || builds(&held) == 1);
    let second = F::start(&ctx, &other, "ok");
    assert_eq!(second.wait(), Outcome::Registered, "{}", F::NAME);
    assert_eq!(first.poll(), None, "{}: the held activation moved", F::NAME);

    open_gate(&held);
    assert_eq!(first.wait(), Outcome::Registered, "{}", F::NAME);
}

/// T55.21 (e): cancelling an activation that is still waiting registers nothing and deregisters nothing.
pub(crate) fn case_e_cancel_while_waiting<F: Family>() {
    // The holder succeeds: its coin stays registered.
    let ticker = unique_ticker("SE");
    let ctx = serialization_ctx(&[&ticker]);
    let first = F::start(&ctx, &ticker, "gate");
    wait_until("the first build to start", || builds(&ticker) == 1);
    let waiting = F::start(&ctx, &ticker, "ok");
    settle();
    assert_eq!(waiting.poll(), None, "{}", F::NAME);
    waiting.cancel();
    open_gate(&ticker);
    assert_eq!(first.wait(), Outcome::Registered, "{}", F::NAME);
    settle();
    assert!(
        is_registered(&ctx, &ticker),
        "{}: the cancelled activation deregistered the coin",
        F::NAME
    );
    assert_eq!(builds(&ticker), 1, "{}: the cancelled activation built a coin", F::NAME);

    // The holder fails: the cancelled activation must not have taken over.
    let ticker = unique_ticker("SE");
    let ctx = serialization_ctx(&[&ticker]);
    let first = F::start(&ctx, &ticker, "gate_fail");
    wait_until("the first build to start", || builds(&ticker) == 1);
    let waiting = F::start(&ctx, &ticker, "ok");
    settle();
    waiting.cancel();
    open_gate(&ticker);
    assert!(matches!(first.wait(), Outcome::Failed(_)), "{}", F::NAME);
    settle();
    assert!(
        !is_registered(&ctx, &ticker),
        "{}: the cancelled activation registered a coin",
        F::NAME
    );
    assert_eq!(builds(&ticker), 1, "{}: the cancelled activation built a coin", F::NAME);
}

/// The lock of an activation covers every ticker the request registers: a platform request names its tokens,
/// and another request that shares one of them waits, whichever order the tokens are listed in.
#[test]
fn a_platform_request_locks_its_tokens_too_and_overlapping_requests_do_not_deadlock() {
    let (platform_a, platform_b) = (unique_ticker("SP"), unique_ticker("SP"));
    let (token_one, token_two) = (unique_ticker("ST"), unique_ticker("ST"));
    let ctx = serialization_ctx(&[&platform_a, &platform_b, &token_one, &token_two]);

    let start = |platform: &str, script: &str, tokens: [&str; 2]| {
        let request = from_json(json!({ "ticker": platform, "script": script, "tokens": tokens }));
        spawn_one_shot(
            "PlatformIsAlreadyActivated",
            enable_platform_coin_with_tokens::<TestCoin>(ctx.clone(), request),
        )
    };
    let first = start(&platform_a, "gate", [&token_one, &token_two]);
    wait_until("the first build to start", || builds(&platform_a) == 1);
    // A different platform, the same two tokens in the opposite order.
    let second = start(&platform_b, "ok", [&token_two, &token_one]);
    settle();
    assert_eq!(second.poll(), None, "the request that shares the tokens did not wait");
    assert_eq!(builds(&platform_b), 0);
    // Each token is held on its own, too.
    for token in [&token_one, &token_two] {
        let waiting = common::block_on(async { lock_activation(&ctx, &[token]).now_or_never() });
        assert!(waiting.is_none(), "the lock of token {} is not held", token);
    }

    open_gate(&platform_a);
    assert_eq!(first.wait(), Outcome::Registered);
    assert_eq!(second.wait(), Outcome::Registered);
    assert_eq!(builds(&platform_b), 1);
}

/// The legacy `enable` and `electrum` live in another crate and take their lock through
/// `CoinsContext::lock_activation`: it is the lock the task paths hold, and it is free again once they finish.
#[test]
fn t55_21_c_the_lock_of_the_legacy_calls_is_the_one_the_task_paths_hold() {
    let ticker = unique_ticker("SL");
    let ctx = serialization_ctx(&[&ticker]);
    let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();

    let task = PlatformTask::start(&ctx, &ticker, "gate");
    wait_until("the build to start", || builds(&ticker) == 1);
    assert!(
        coins_ctx.lock_activation(&[&ticker]).now_or_never().is_none(),
        "the legacy lock does not wait for the task"
    );
    open_gate(&ticker);
    assert_eq!(task.wait(), Outcome::Registered);
    assert!(coins_ctx.lock_activation(&[&ticker]).now_or_never().is_some());
}

// One test per family for each case: the cases of T55.21 for the platform, token and layer-2 families.
macro_rules! family_cases {
    ($($module:ident => $family:ty),+ $(,)?) => {
        $(
            mod $module {
                use super::*;

                #[test]
                fn t55_21_a_second_waits_and_the_coin_is_built_once() {
                    case_a_second_waits_and_the_coin_is_built_once::<$family>();
                }

                #[test]
                fn t55_21_b_second_proceeds_after_a_failure() { case_b_second_proceeds_after_a_failure::<$family>(); }

                #[test]
                fn t55_21_d_different_tickers_do_not_wait() { case_d_different_tickers_do_not_wait::<$family>(); }

                #[test]
                fn t55_21_e_cancel_while_waiting() { case_e_cancel_while_waiting::<$family>(); }
            }
        )+
    };
}

family_cases!(
    platform_task => PlatformTask,
    platform_one_shot => PlatformOneShot,
    token_task => TokenTask,
    token_one_shot => TokenOneShot,
    l2_task => L2Task,
    l2_one_shot => L2OneShot,
);

// (c): a task and a one-shot call, and tasks of two families, of one ticker wait for each other.
#[test]
fn t55_21_c_a_task_and_a_one_shot_call_of_one_ticker_wait_for_each_other() {
    case_c_two_paths_wait_for_each_other::<PlatformTask, PlatformOneShot>();
    case_c_two_paths_wait_for_each_other::<PlatformOneShot, PlatformTask>();
    case_c_two_paths_wait_for_each_other::<TokenTask, TokenOneShot>();
    case_c_two_paths_wait_for_each_other::<TokenOneShot, TokenTask>();
    case_c_two_paths_wait_for_each_other::<L2Task, L2OneShot>();
    case_c_two_paths_wait_for_each_other::<L2OneShot, L2Task>();
}

#[test]
fn t55_21_c_paths_of_different_families_wait_for_each_other() {
    case_c_two_paths_wait_for_each_other::<PlatformTask, TokenOneShot>();
    case_c_two_paths_wait_for_each_other::<TokenTask, L2OneShot>();
    case_c_two_paths_wait_for_each_other::<L2Task, PlatformOneShot>();
}
