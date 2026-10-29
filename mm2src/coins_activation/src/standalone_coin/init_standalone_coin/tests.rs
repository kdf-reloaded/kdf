//! The task-framework contract (CRD ch. 55 §55.2) and the standalone-coin rules (§55.5), exercised on a
//! scripted coin so the real `init`/`status`/`user_action`/`cancel` handlers run end to end.

use super::*;
use crate::activation_serialization_tests::{case_a_second_waits_and_the_coin_is_built_once,
                                            case_b_second_proceeds_after_a_failure,
                                            case_c_two_paths_wait_for_each_other,
                                            case_d_different_tickers_do_not_wait, case_e_cancel_while_waiting,
                                            outcome_of_status, scripted_build, Family, L2OneShot, PlatformOneShot,
                                            Running, TokenOneShot, TokenTask};
use crate::prelude::CurrentBlock;
use coins::{CoinProtocol, RegisterCoinError, TestCoin};
use common::{block_on, HttpStatusCode, StatusCode};
use crypto::hw_rpc_task::{HwRpcTaskAwaitingStatus, HwRpcTaskUserAction};
use futures::future::pending;
use mm2_core::mm_ctx::MmCtxBuilder;
use rpc_task::rpc_common::RpcTaskStatusError;
use serde::Serialize;
use serde_json::{json, Value as Json};

const TICKER: &str = "TST";

#[derive(Debug, Deserialize)]
pub struct ScriptedRequest {
    pub script: String,
}

impl TxHistory for ScriptedRequest {
    fn tx_history(&self) -> bool { false }
}

pub struct ScriptedProtocol;

impl TryFromCoinProtocol for ScriptedProtocol {
    fn try_from_coin_protocol(proto: CoinProtocol) -> Result<Self, MmError<CoinProtocol>> {
        match proto {
            CoinProtocol::UTXO => Ok(ScriptedProtocol),
            protocol => MmError::err(protocol),
        }
    }
}

#[derive(Clone, Serialize)]
pub struct ScriptedResult {
    pub ticker: String,
    pub current_block: u64,
}

impl CurrentBlock for ScriptedResult {
    fn current_block(&self) -> u64 { self.current_block }
}

#[derive(Clone, Serialize)]
pub enum ScriptedStatus {
    ActivatingCoin,
}

impl InitStandaloneCoinInitialStatus for ScriptedStatus {
    fn initial_status() -> Self { ScriptedStatus::ActivatingCoin }
}

#[async_trait]
impl InitStandaloneCoinActivationOps for TestCoin {
    type ActivationRequest = ScriptedRequest;
    type StandaloneProtocol = ScriptedProtocol;
    type ActivationResult = ScriptedResult;
    type ActivationError = InitStandaloneCoinError;
    type InProgressStatus = ScriptedStatus;
    type AwaitingStatus = HwRpcTaskAwaitingStatus;
    type UserAction = HwRpcTaskUserAction;

    fn rpc_task_manager(activation_ctx: &CoinsActivationContext) -> &InitStandaloneCoinTaskManagerShared<Self> {
        &activation_ctx.init_test_coin_task_manager
    }

    async fn init_standalone_coin(
        _ctx: MmArc,
        ticker: String,
        _coin_conf: Json,
        activation_request: &ScriptedRequest,
        _protocol_info: ScriptedProtocol,
        _task_handle: &InitStandaloneCoinTaskHandle<Self>,
    ) -> MmResult<Self, InitStandaloneCoinError> {
        scripted_build(&ticker, &activation_request.script).await;
        Ok(TestCoin::default())
    }

    async fn get_activation_result(
        &self,
        ctx: MmArc,
        _task_handle: &InitStandaloneCoinTaskHandle<Self>,
        activation_request: &ScriptedRequest,
    ) -> MmResult<ScriptedResult, InitStandaloneCoinError> {
        match activation_request.script.as_str() {
            "ok" => {},
            "gate" => {},
            "fail" | "gate_fail" => {
                return MmError::err(InitStandaloneCoinError::CoinCreationError {
                    ticker: TICKER.to_owned(),
                    error: "scripted failure".to_owned(),
                })
            },
            // Another activation registers the same ticker while this one is still working.
            "registered_elsewhere" => {
                register_test_coin(&ctx).await;
            },
            _ => pending::<()>().await,
        }
        Ok(ScriptedResult {
            ticker: TICKER.to_owned(),
            current_block: 7,
        })
    }
}

fn test_ctx() -> MmArc {
    MmCtxBuilder::new()
        .with_conf(json!({
            "coins": [
                { "coin": TICKER, "protocol": { "type": "UTXO" } },
                { "coin": "ETH1", "protocol": { "type": "ETH" } },
                { "coin": "ERC1", "protocol": { "type": "ERC20", "protocol_data": {
                    "platform": "ETH", "contract_address": "0xdAC17F958D2ee523a2206206994597C13D831ec7" } } },
            ]
        }))
        .into_mm_arc()
}

async fn register_test_coin(ctx: &MmArc) {
    lp_register_coin(ctx, TestCoin::default().into(), RegisterCoinParams {
        ticker: TICKER.to_owned(),
        tx_history: false,
    })
    .await
    .unwrap_or_else(|e| panic!("cannot register the coin: {}", e));
}

fn init_request(script: &str) -> InitStandaloneCoinReq<ScriptedRequest> {
    serde_json::from_value(json!({
        "ticker": TICKER,
        "activation_params": { "script": script },
        "client_id": 3,
    }))
    .unwrap_or_else(|e| panic!("bad request: {}", e))
}

fn start(ctx: &MmArc, script: &str) -> u64 {
    block_on(init_standalone_coin::<TestCoin>(ctx.clone(), init_request(script)))
        .unwrap_or_else(|e| panic!("init failed: {}", e))
        .task_id
}

fn status_result(ctx: &MmArc, task_id: u64, forget_if_finished: bool) -> MmResult<Json, RpcTaskStatusError> {
    let req = InitStandaloneCoinStatusRequest {
        task_id,
        forget_if_finished,
    };
    block_on(init_standalone_coin_status::<TestCoin>(ctx.clone(), req))
        .map(|status| serde_json::to_value(status).expect("serializable status"))
}

fn cancel(ctx: &MmArc, task_id: u64) -> MmResult<SuccessResponse, CancelRpcTaskError> {
    let req = InitStandaloneCoinStatusRequest {
        task_id,
        forget_if_finished: true,
    };
    block_on(cancel_init_standalone_coin::<TestCoin>(ctx.clone(), req))
}

fn status(ctx: &MmArc, task_id: u64, forget_if_finished: bool) -> Json {
    status_result(ctx, task_id, forget_if_finished).unwrap_or_else(|e| panic!("status failed: {}", e))
}

/// Polls (without forgetting) until the task reports a terminal status.
fn wait_for_terminal(ctx: &MmArc, task_id: u64) -> Json {
    for _ in 0..200 {
        let current = status(ctx, task_id, false);
        if current["status"] == json!("Ok") || current["status"] == json!("Error") {
            return current;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("the task did not finish in time")
}

fn expect_err<T, E: NotMmError>(result: MmResult<T, E>) -> E {
    match result {
        Ok(_) => panic!("the call must fail"),
        Err(error) => error.into_inner(),
    }
}

fn wire_of_cancel_error<T>(result: MmResult<T, CancelRpcTaskError>) -> (Json, StatusCode) {
    let error = expect_err(result);
    (serde_json::to_value(&error).expect("serializable"), error.status_code())
}

#[test]
fn unknown_task_id_answers_per_method_with_its_http_status() {
    let ctx = test_ctx();
    let unknown = 987_654;

    // T55.2: `cancel` is 404, `status` and `user_action` are 400, and the task id is an integer.
    let (wire, http) = wire_of_cancel_error(cancel(&ctx, unknown));
    assert_eq!(wire, json!({ "error_type": "NoSuchTask", "error_data": unknown }));
    assert_eq!(http, StatusCode::NOT_FOUND);

    let status_error = expect_err(status_result(&ctx, unknown, true));
    assert_eq!(
        serde_json::to_value(&status_error).unwrap(),
        json!({ "error_type": "NoSuchTask", "error_data": unknown })
    );
    assert_eq!(status_error.status_code(), StatusCode::BAD_REQUEST);

    let user_action_req: InitStandaloneCoinUserActionRequest<HwRpcTaskUserAction> = serde_json::from_value(json!({
        "task_id": unknown,
        "user_action": { "action_type": "TrezorPassphrase", "passphrase": "" },
    }))
    .unwrap();
    let user_action_error = expect_err(block_on(init_standalone_coin_user_action::<TestCoin>(
        ctx,
        user_action_req,
    )));
    assert_eq!(
        serde_json::to_value(&user_action_error).unwrap(),
        json!({ "error_type": "NoSuchTask", "error_data": unknown })
    );
    assert_eq!(user_action_error.status_code(), StatusCode::BAD_REQUEST);
}

#[test]
fn init_accepts_client_id_and_rejects_a_negative_one() {
    let accepted: Result<InitStandaloneCoinReq<ScriptedRequest>, _> = serde_json::from_value(json!({
        "ticker": TICKER, "activation_params": { "script": "ok" }, "client_id": 0,
    }));
    assert!(accepted.is_ok());
    let without: Result<InitStandaloneCoinReq<ScriptedRequest>, _> = serde_json::from_value(json!({
        "ticker": TICKER, "activation_params": { "script": "ok" },
    }));
    assert!(without.is_ok());
    let negative: Result<InitStandaloneCoinReq<ScriptedRequest>, _> = serde_json::from_value(json!({
        "ticker": TICKER, "activation_params": { "script": "ok" }, "client_id": -1,
    }));
    assert!(negative.is_err());
}

#[test]
fn init_checks_run_in_the_bound_order_and_answer_at_mmrpc_level() {
    let ctx = test_ctx();

    // R55.5.3 (2): the ticker is absent from the configuration.
    let mut missing = init_request("ok");
    missing.ticker = "NOPE".to_owned();
    let error = expect_err(block_on(init_standalone_coin::<TestCoin>(ctx.clone(), missing)));
    assert_eq!(
        serde_json::to_value(&error).unwrap(),
        json!({ "error_type": "CoinConfigIsNotFound", "error_data": "NOPE" })
    );
    assert_eq!(error.status_code(), StatusCode::BAD_REQUEST);

    // R55.5.3 (4): the configured protocol is not the member's.
    let mut wrong_protocol = init_request("ok");
    wrong_protocol.ticker = "ETH1".to_owned();
    let error = expect_err(block_on(init_standalone_coin::<TestCoin>(ctx.clone(), wrong_protocol)));
    let wire = serde_json::to_value(&error).unwrap();
    assert_eq!(wire["error_type"], json!("UnexpectedCoinProtocol"));
    assert_eq!(wire["error_data"]["ticker"], json!("ETH1"));
    assert!(wire["error_data"]["protocol"]["type"].is_string());
    assert_eq!(error.status_code(), StatusCode::BAD_REQUEST);

    // CRD ch. 36 R36.0.3: the offending protocol is in the canonical coin-configuration form, with its
    // `protocol_data`.
    let mut erc20 = init_request("ok");
    erc20.ticker = "ERC1".to_owned();
    let error = expect_err(block_on(init_standalone_coin::<TestCoin>(ctx.clone(), erc20)));
    assert_eq!(
        serde_json::to_value(&error).unwrap()["error_data"]["protocol"],
        json!({ "type": "ERC20", "protocol_data": {
            "platform": "ETH", "contract_address": "0xdAC17F958D2ee523a2206206994597C13D831ec7" } })
    );

    // R55.5.3 (1): an active coin is reported before anything else.
    block_on(register_test_coin(&ctx));
    let error = expect_err(block_on(init_standalone_coin::<TestCoin>(ctx, init_request("ok"))));
    assert_eq!(
        serde_json::to_value(&error).unwrap(),
        json!({ "error_type": "CoinIsAlreadyActivated", "error_data": { "ticker": TICKER } })
    );
}

#[test]
fn cancel_of_a_finished_task_is_task_finished_and_the_result_stays_readable() {
    let ctx = test_ctx();
    // The failing task runs first: the successful one leaves its coin registered.
    let failed = start(&ctx, "fail");
    let error = wait_for_terminal(&ctx, failed);
    let finished = start(&ctx, "ok");
    let ok = wait_for_terminal(&ctx, finished);
    assert_eq!(ok["status"], json!("Ok"));
    assert_eq!(ok["details"], json!({ "ticker": TICKER, "current_block": 7 }));
    assert_eq!(error["status"], json!("Error"));
    assert_eq!(error["details"]["error_type"], json!("CoinCreationError"));
    assert!(error["details"].get("result").is_none());

    // T55.1: `TaskFinished` carries the integer task id and answers HTTP 409, for `Ok` and for `Error`.
    for task_id in [finished, failed] {
        let (wire, http) = wire_of_cancel_error(cancel(&ctx, task_id));
        assert_eq!(wire, json!({ "error_type": "TaskFinished", "error_data": task_id }));
        assert_eq!(http, StatusCode::CONFLICT);
    }

    // The terminal result is kept (R55.2.8 b), and a read that forgets it removes it (R55.2.8 a).
    assert_eq!(status(&ctx, finished, false), ok);
    assert_eq!(status(&ctx, finished, true), ok);
    let (wire, http) = wire_of_cancel_error(cancel(&ctx, finished));
    assert_eq!(wire, json!({ "error_type": "NoSuchTask", "error_data": finished }));
    assert_eq!(http, StatusCode::NOT_FOUND);
    assert!(status_result(&ctx, finished, true).is_err());
}

#[test]
fn a_non_terminal_task_is_never_removed_by_status_and_a_cancelled_one_is_gone() {
    let ctx = test_ctx();
    let task_id = start(&ctx, "hang");

    // R55.2.8 (c): a non-terminal task is never removed by `status`.
    for forget_if_finished in [true, false] {
        let current = status(&ctx, task_id, forget_if_finished);
        assert_eq!(current, json!({ "status": "InProgress", "details": "ActivatingCoin" }));
    }

    cancel(&ctx, task_id).unwrap_or_else(|e| panic!("cancel failed: {}", e));

    // R55.2.8 (d) and R55.2.16: the cancelled task is not observable, and a repeated cancel is `NoSuchTask`.
    let status_error = expect_err(status_result(&ctx, task_id, false));
    assert_eq!(
        serde_json::to_value(&status_error).unwrap(),
        json!({ "error_type": "NoSuchTask", "error_data": task_id })
    );
    let (wire, http) = wire_of_cancel_error(cancel(&ctx, task_id));
    assert_eq!(wire, json!({ "error_type": "NoSuchTask", "error_data": task_id }));
    assert_eq!(http, StatusCode::NOT_FOUND);
}

#[test]
fn a_lost_registration_race_ends_with_coin_is_already_activated() {
    let ctx = test_ctx();
    let task_id = start(&ctx, "registered_elsewhere");

    let outcome = wait_for_terminal(&ctx, task_id);

    // R55.5.5: the task must not report success for a coin instance it did not register.
    assert_eq!(outcome["status"], json!("Error"));
    assert_eq!(outcome["details"]["error_type"], json!("CoinIsAlreadyActivated"));
    assert_eq!(outcome["details"]["error_data"], json!({ "ticker": TICKER }));
}

fn task_for_cleanup(ctx: &MmArc) -> InitStandaloneCoinTask<TestCoin> {
    InitStandaloneCoinTask::<TestCoin> {
        ctx: ctx.clone(),
        request: init_request("ok"),
        coin_conf: json!({}),
        protocol_info: ScriptedProtocol,
        registered_by_task: Arc::new(AtomicBool::new(false)),
    }
}

#[test]
fn cancel_cleanup_deregisters_only_the_coin_the_task_registered() {
    let ctx = test_ctx();
    block_on(register_test_coin(&ctx));
    let task = task_for_cleanup(&ctx);

    // The coin belongs to another activation: the cleanup leaves it alone.
    let cleanup = task.cancel_cleanup().expect("a standalone task has a cancel cleanup");
    block_on(cleanup());
    assert!(block_on(lp_coinfind(&ctx, TICKER)).unwrap().is_some());

    // The task registered the coin itself: the cancelled activation leaves no trace (R55.2.14).
    task.registered_by_task.store(true, Ordering::SeqCst);
    let cleanup = task.cancel_cleanup().expect("a standalone task has a cancel cleanup");
    block_on(cleanup());
    assert!(block_on(lp_coinfind(&ctx, TICKER)).unwrap().is_none());
}

#[test]
fn registration_conflict_maps_to_the_standalone_error_set() {
    let error = InitStandaloneCoinError::from(RegisterCoinError::CoinIsInitializedAlready {
        coin: TICKER.to_owned(),
    });
    assert_eq!(
        serde_json::to_value(&error).unwrap(),
        json!({ "error_type": "CoinIsAlreadyActivated", "error_data": { "ticker": TICKER } })
    );
}

// T55.2 and T55.3: every task family keeps its own registry, and each answers an unknown task id
// (including an id another family issued) with `NoSuchTask`, integer, HTTP 404 on `cancel`.
#[test]
fn every_family_answers_an_unknown_or_foreign_task_id_with_no_such_task() {
    use crate::init_platform_coin_with_tokens::cancel_init_platform_coin_with_tokens;
    use coins::eth::EthCoin;
    use coins::siacoin::SiaCoin;
    use coins::tendermint::TendermintCoin;
    use coins::utxo::bch::BchCoin;
    use coins::utxo::qtum::QtumCoin;
    use coins::utxo::utxo_standard::UtxoStandardCoin;

    let ctx = test_ctx();
    // A task of the scripted family: its id is foreign to every other family.
    let foreign = start(&ctx, "hang");
    let req = || InitStandaloneCoinStatusRequest {
        task_id: foreign,
        forget_if_finished: true,
    };

    let mut answers = vec![
        wire_of_cancel_error(block_on(cancel_init_standalone_coin::<UtxoStandardCoin>(
            ctx.clone(),
            req(),
        ))),
        wire_of_cancel_error(block_on(cancel_init_standalone_coin::<QtumCoin>(ctx.clone(), req()))),
        wire_of_cancel_error(block_on(cancel_init_standalone_coin::<BchCoin>(ctx.clone(), req()))),
        wire_of_cancel_error(block_on(cancel_init_standalone_coin::<SiaCoin>(ctx.clone(), req()))),
        wire_of_cancel_error(block_on(cancel_init_platform_coin_with_tokens::<EthCoin>(
            ctx.clone(),
            req(),
        ))),
        wire_of_cancel_error(block_on(cancel_init_platform_coin_with_tokens::<TendermintCoin>(
            ctx.clone(),
            req(),
        ))),
    ];
    #[cfg(not(target_arch = "wasm32"))]
    {
        use crate::l2::cancel_l2_activation;
        use coins::lightning::LightningCoin;
        use coins::z_coin::ZCoin;
        answers.push(wire_of_cancel_error(block_on(cancel_init_standalone_coin::<ZCoin>(
            ctx.clone(),
            req(),
        ))));
        answers.push(wire_of_cancel_error(block_on(cancel_l2_activation::<LightningCoin>(
            ctx.clone(),
            req(),
        ))));
    }
    for (wire, http) in answers {
        assert_eq!(wire, json!({ "error_type": "NoSuchTask", "error_data": foreign }));
        assert_eq!(http, StatusCode::NOT_FOUND);
    }

    // The scripted family still owns its task.
    assert_eq!(
        status(&ctx, foreign, false),
        json!({ "status": "InProgress", "details": "ActivatingCoin" })
    );
}

/// The standalone-coin task, driven by the cases of T55.21 (CRD ch. 55 R55.1.12).
struct StandaloneTask;

impl Family for StandaloneTask {
    const NAME: &'static str = "standalone task";

    fn start(ctx: &MmArc, ticker: &str, script: &str) -> Running {
        let mut request = init_request(script);
        request.ticker = ticker.to_owned();
        let task_id = block_on(init_standalone_coin::<TestCoin>(ctx.clone(), request))
            .unwrap_or_else(|e| panic!("init failed: {}", e))
            .task_id;
        let (status_ctx, cancel_ctx) = (ctx.clone(), ctx.clone());
        Running::new(
            move || outcome_of_status(&status(&status_ctx, task_id, false), "CoinIsAlreadyActivated"),
            move || {
                cancel(&cancel_ctx, task_id).unwrap_or_else(|e| panic!("cancel failed: {}", e));
            },
        )
    }
}

#[test]
fn t55_21_a_second_waits_and_the_coin_is_built_once() {
    case_a_second_waits_and_the_coin_is_built_once::<StandaloneTask>();
}

#[test]
fn t55_21_b_second_proceeds_after_a_failure() { case_b_second_proceeds_after_a_failure::<StandaloneTask>(); }

#[test]
fn t55_21_c_a_standalone_task_and_other_paths_of_one_ticker_wait_for_each_other() {
    case_c_two_paths_wait_for_each_other::<StandaloneTask, StandaloneTask>();
    case_c_two_paths_wait_for_each_other::<StandaloneTask, PlatformOneShot>();
    case_c_two_paths_wait_for_each_other::<StandaloneTask, TokenOneShot>();
    case_c_two_paths_wait_for_each_other::<StandaloneTask, L2OneShot>();
    case_c_two_paths_wait_for_each_other::<PlatformOneShot, StandaloneTask>();
    case_c_two_paths_wait_for_each_other::<TokenTask, StandaloneTask>();
}

#[test]
fn t55_21_d_different_tickers_do_not_wait() { case_d_different_tickers_do_not_wait::<StandaloneTask>(); }

#[test]
fn t55_21_e_cancel_while_waiting() { case_e_cancel_while_waiting::<StandaloneTask>(); }
