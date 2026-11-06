//! The shielded coin's refusal of `fee` and `from` on a withdrawal (CRD ch.49 R49.27, R49.27a, T49.27a).
//!
//! The harness is the one of the parent module: a real `ZCoin` over a real `zcash_client_sqlite` wallet
//! database and the scripted lightwalletd transport. No refusal reads the backend or selects a note, so
//! the tests need no network and make no proof; they need the Sapling parameter files only because the
//! harness loads them, and say so where those are absent.

use super::*;
use crate::common::HttpStatusCode;
use crate::rpc_command::init_withdraw::{init_withdraw, withdraw_status, WithdrawCompatRpcStatus, WithdrawStatusRequest};
use crate::{MmCoinEnum, WithdrawError, WithdrawRequest};
use mm2_core::mm_ctx::MmCtxBuilder;

/// A destination that decodes as no address at all: a request that reaches the decoder answers
/// `InvalidAddress`, so a refusal that wins over it was decided before the decoder ran.
const UNDECODABLE_TO: &str = "not an address";

fn request(extra: serde_json::Value) -> WithdrawRequest {
    let mut req = json!({ "coin": "ARRR", "to": UNDECODABLE_TO, "amount": "0.01" });
    req.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    serde_json::from_value(req).unwrap()
}

fn fee_option() -> serde_json::Value { json!({ "fee": { "type": "UtxoFixed", "amount": "0.0001" } }) }

fn from_option() -> serde_json::Value { json!({ "from": { "derivation_path": "m/44'/133'/0'/0/0" } }) }

/// The wire form of the refusal, compared by discriminant and the JSON type of `error_data` only.
fn assert_unsupported(err: &WithdrawError) {
    assert!(matches!(err, WithdrawError::UnsupportedError(_)), "{err:?}");
    assert_eq!(err.status_code(), http::StatusCode::BAD_REQUEST);
    let json = serde_json::to_value(err).unwrap();
    assert_eq!(json["error_type"], "UnsupportedError");
    assert!(json["error_data"].is_string());
}

/// Nothing was selected, signed or broadcast.
fn assert_untouched(fixture: &LightCoinFixture) {
    assert_eq!(fixture.broadcasts(), 0);
    assert_eq!(fixture.wallet_balance(), NOTE_VALUE);
    assert!(fixture.in_flight().is_empty());
}

/// T49.27a, shielded row: the direct `withdraw` with `fee`, then with `from`, then with both, which
/// answers for `fee` first (R49.27a (b) and (c)). Each refusal comes before the destination is decoded.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t49_27a_direct_withdraw_refuses_fee_then_from_with_the_shared_arm() {
    let Some(fixture) = LightCoinFixture::new() else {
        return;
    };
    let calls_before = fixture.transport.calls.lock().len();

    let fee = fixture.coin.withdraw(request(fee_option())).compat().await.unwrap_err();
    assert_unsupported(fee.get_inner());

    let from = fixture
        .coin
        .withdraw(request(from_option()))
        .compat()
        .await
        .unwrap_err();
    assert_unsupported(from.get_inner());

    // Both options: the answer is the one for `fee`, which names the fee and not the sender.
    let mut both = fee_option();
    both.as_object_mut()
        .unwrap()
        .extend(from_option().as_object().unwrap().clone());
    let both = fixture.coin.withdraw(request(both)).compat().await.unwrap_err();
    assert_unsupported(both.get_inner());
    let (WithdrawError::UnsupportedError(both_text), WithdrawError::UnsupportedError(fee_text)) =
        (both.get_inner(), fee.get_inner())
    else {
        unreachable!()
    };
    assert_eq!(both_text, fee_text, "`fee` is checked before `from`");
    let WithdrawError::UnsupportedError(from_text) = from.get_inner() else {
        unreachable!()
    };
    assert_ne!(both_text, from_text);

    // The backend was not asked anything either.
    assert_eq!(fixture.transport.calls.lock().len(), calls_before);
    assert_untouched(&fixture);
}

/// T49.27a, shielded row, task route: `task::withdraw::init` carrying `fee` reaches `status: "Error"` with
/// `details.error_type == "UnsupportedError"` and a string `details.error_data`; so does one carrying
/// `from`. The task reaches the same code as the direct method (`init_withdraw` calls `withdraw`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t49_27a_task_route_refuses_fee_and_from_with_the_shared_arm() {
    let Some(fixture) = LightCoinFixture::new() else {
        return;
    };
    let ctx = MmCtxBuilder::default().into_mm_arc();
    let coins_ctx = crate::CoinsContext::from_ctx(&ctx).unwrap();
    coins_ctx
        .add_coin(MmCoinEnum::ZCoin(fixture.coin.clone()))
        .await
        .unwrap();

    for option in [fee_option(), from_option()] {
        let init = init_withdraw(ctx.clone(), request(option)).await.unwrap();
        let mut terminal = None;
        for _ in 0..500 {
            let status = withdraw_status(ctx.clone(), WithdrawStatusRequest {
                task_id: init.task_id,
                forget_if_finished: false,
            })
            .await
            .unwrap_or_else(|e| panic!("withdraw_status failed: {e}"));
            if !matches!(status, WithdrawCompatRpcStatus::InProgress(_)) {
                terminal = Some(status);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let status = serde_json::to_value(terminal.expect("the task reached no terminal status")).unwrap();
        assert_eq!(status["status"], "Error", "{status}");
        assert_eq!(status["details"]["error_type"], "UnsupportedError", "{status}");
        assert!(status["details"]["error_data"].is_string(), "{status}");
    }
    assert_untouched(&fixture);
}
