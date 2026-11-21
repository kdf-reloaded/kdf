//! Offline tests of the background work that ends with a removed coin instance (CRD ch.43 R43.1E.5 item 4, §43.1E
//! not-established item (1), ch.55 R55.4.9). The coins are test coins; the work is a pending future that is only ever
//! aborted.

use super::*;
use crate::coin_work::{is_same_instance, CoinWork, PendingCoinWork};
use common::block_on;
use futures::future::{abortable, Aborted};
use mm2_core::mm_ctx::MmCtxBuilder;
use mm2_event_stream::{mpsc, oneshot, Broadcaster, EventStreamer, NoDataIn, StreamerId};

fn test_coin(ticker: &str) -> MmCoinEnum { MmCoinEnum::Test(TestCoin::new(ticker)) }

fn ctx_and_coins() -> (MmArc, Arc<CoinsContext>) {
    let ctx = MmCtxBuilder::default().into_mm_arc();
    let coins_ctx = CoinsContext::from_ctx(&ctx).unwrap();
    (ctx, coins_ctx)
}

/// A future that never completes by itself, registered with `work`; it completes with `Err(Aborted)` once the work is
/// aborted.
fn dummy_task(work: &CoinWork) -> impl std::future::Future<Output = Result<(), Aborted>> {
    let (fut, handle) = abortable(futures::future::pending::<()>());
    work.add(handle);
    fut
}

fn is_aborted(task: impl std::future::Future<Output = Result<(), Aborted>>) -> bool { block_on(task).is_err() }

/// A work that is registered with the registry together with the coin, as an activation does.
fn register_with_task(
    coins_ctx: &CoinsContext,
    ticker: &str,
) -> impl std::future::Future<Output = Result<(), Aborted>> {
    let pending = PendingCoinWork::new();
    let task = dummy_task(pending.work());
    block_on(coins_ctx.add_coin_with_work(test_coin(ticker), pending)).unwrap();
    task
}

#[test]
fn disable_coin_aborts_the_work_of_the_removed_instance_only() {
    let (ctx, coins_ctx) = ctx_and_coins();
    let removed = register_with_task(&coins_ctx, "AAA");
    let kept = register_with_task(&coins_ctx, "BBB");
    let kept_work = block_on(coins_ctx.coin_work("BBB")).unwrap();

    block_on(disable_coin(&ctx, "AAA")).unwrap();

    assert!(is_aborted(removed), "the work of the removed coin goes on");
    assert!(!kept_work.is_ended(), "the work of another coin ended");
    block_on(disable_coin(&ctx, "BBB")).unwrap();
    assert!(is_aborted(kept));
}

#[test]
fn the_forced_removal_aborts_the_work_of_the_platform_and_of_every_dependent() {
    let (ctx, coins_ctx) = ctx_and_coins();
    let pending = PendingCoinWork::new();
    let platform_task = dummy_task(pending.work());
    block_on(
        coins_ctx.add_platform_with_tokens_and_work(test_coin("PLAT"), pending, vec![test_coin("T1"), test_coin("T2")]),
    )
    .unwrap();
    let token_tasks: Vec<_> = ["T1", "T2"]
        .iter()
        .map(|ticker| dummy_task(&block_on(coins_ctx.coin_work(ticker)).expect("the token has work")))
        .collect();

    let removed = block_on(disable_platform_with_dependents(&ctx, "PLAT")).unwrap();

    assert_eq!(removed.len(), 3);
    assert!(is_aborted(platform_task));
    for task in token_tasks {
        assert!(is_aborted(task));
    }
}

#[test]
fn a_passive_platform_keeps_its_work_until_it_is_removed() {
    let (ctx, coins_ctx) = ctx_and_coins();
    let pending = PendingCoinWork::new();
    let platform_task = dummy_task(pending.work());
    block_on(coins_ctx.add_platform_with_tokens_and_work(test_coin("PLAT"), pending, vec![test_coin("T1")])).unwrap();
    assert!(block_on(coins_ctx.make_passive("PLAT")));
    assert!(!block_on(coins_ctx.coin_work("PLAT")).unwrap().is_ended());

    block_on(disable_coin(&ctx, "PLAT")).unwrap();
    assert!(is_aborted(platform_task));
}

#[test]
fn a_quick_disable_and_re_enable_leaves_one_instance_and_ends_the_old_loop() {
    let (ctx, coins_ctx) = ctx_and_coins();
    block_on(coins_ctx.add_coin(test_coin("AAA"))).unwrap();
    let old = block_on(coins_ctx.instance_id("AAA")).unwrap();
    block_on(disable_coin(&ctx, "AAA")).unwrap();
    block_on(coins_ctx.add_coin(test_coin("AAA"))).unwrap();
    let new = block_on(coins_ctx.instance_id("AAA")).unwrap();

    assert_ne!(old, new);
    // The check of the history loops: the old loop ends, the new one goes on.
    assert!(!is_same_instance(Some(new), Some(old)));
    assert!(is_same_instance(Some(new), Some(new)));
}

#[test]
fn an_activation_refused_after_its_instance_was_built_aborts_the_work_it_spawned() {
    let (_ctx, coins_ctx) = ctx_and_coins();
    block_on(coins_ctx.add_coin(test_coin("AAA"))).unwrap();
    block_on(coins_ctx.add_platform_with_tokens(test_coin("PLAT"), vec![])).unwrap();

    // A coin built for a ticker that is registered already.
    let pending = PendingCoinWork::new();
    let task = dummy_task(pending.work());
    assert!(block_on(coins_ctx.add_coin_with_work(test_coin("AAA"), pending)).is_err());
    assert!(is_aborted(task));

    // A token, and a platform.
    let pending = PendingCoinWork::new();
    let task = dummy_task(pending.work());
    assert!(block_on(coins_ctx.add_token_with_work("PLAT", test_coin("AAA"), pending)).is_err());
    assert!(is_aborted(task));
    let pending = PendingCoinWork::new();
    let task = dummy_task(pending.work());
    assert!(block_on(coins_ctx.add_platform_with_tokens_and_work(test_coin("PLAT"), pending, vec![])).is_err());
    assert!(is_aborted(task));

    // The registered instance of the ticker kept its own work.
    assert!(!block_on(coins_ctx.coin_work("AAA")).unwrap().is_ended());
}

#[test]
fn an_activation_that_ends_before_registering_aborts_its_work() {
    let pending = PendingCoinWork::new();
    let task = dummy_task(pending.work());
    // The activation returns an error: the pending work is dropped with it.
    drop(pending);
    assert!(is_aborted(task));
}

#[test]
fn work_spawned_in_a_build_scope_registers_with_the_instance_and_not_with_the_node() {
    let (ctx, coins_ctx) = ctx_and_coins();
    let mut old_tasks = Vec::new();
    for _ in 0..3 {
        let pending = PendingCoinWork::new();
        let (fut, handle) = abortable(futures::future::pending::<()>());
        old_tasks.push(fut);
        let build = async {
            // What a spawn site does: the handle goes to the instance being built, if there is one.
            if let Some(handle) = CoinWork::register_with_current(handle) {
                ctx.abort_handlers.lock().unwrap().push(handle);
            }
        };
        block_on(pending.scope(build));
        block_on(coins_ctx.add_coin_with_work(test_coin("AAA"), pending)).unwrap();
        block_on(disable_coin(&ctx, "AAA")).unwrap();
        assert!(
            ctx.abort_handlers.lock().unwrap().is_empty(),
            "the node's list grew with a disable and re-enable cycle"
        );
    }
    for task in old_tasks {
        assert!(block_on(task).is_err(), "the work of a removed instance goes on");
    }
}

/// CRD ch.43 R43.1E.5 item 4, ch.55 R55.4.9: the TRON history process is work of the registered instance. Built inside
/// the instance's scope it registers its abort handle with the instance's `CoinWork`, not with the node's list, it
/// runs in that scope, and the forced removal of the platform aborts it.
#[test]
fn the_tron_history_process_is_work_of_the_registered_platform_and_ends_with_its_removal() {
    let (ctx, coins_ctx) = ctx_and_coins();
    let pending = PendingCoinWork::new();
    let work = pending.work().clone();
    block_on(coins_ctx.add_platform_with_tokens_and_work(test_coin("PLAT"), pending, vec![])).unwrap();
    let registered = block_on(coins_ctx.coin_work("PLAT")).unwrap();
    assert_eq!(registered.id(), work.id());

    // The process body never ends by itself and reports the instance id it runs in.
    let seen = Arc::new(std::sync::Mutex::new(None));
    let seen_in_task = seen.clone();
    let task = registered.run_in_scope(|| {
        crate::eth::tron::history::instance_task(&ctx, async move {
            *seen_in_task.lock().unwrap() = CoinWork::current_id();
            futures::future::pending::<()>().await;
        })
    });
    assert_eq!(registered.len(), 1, "the process is not on the instance's record");
    assert!(
        ctx.abort_handlers.lock().unwrap().is_empty(),
        "the process is on the node's list"
    );

    // Polled once outside any scope, it still runs in its instance's scope.
    let mut task = Box::pin(task);
    assert!(futures::FutureExt::now_or_never(task.as_mut()).is_none());
    assert_eq!(*seen.lock().unwrap(), Some(registered.id()));

    block_on(disable_platform_with_dependents(&ctx, "PLAT")).unwrap();
    assert!(registered.is_ended());
    // The aborted process ends at once; a process that was not aborted would wait for ever here.
    block_on(task);
}

/// A streamer keyed like the balance streamer of a coin, that waits for its shutdown.
struct StreamerOfCoin(String);

#[async_trait::async_trait]
impl EventStreamer for StreamerOfCoin {
    type DataInType = NoDataIn;

    fn streamer_id(&self) -> StreamerId { StreamerId::Balance(self.0.clone()) }

    async fn handle(
        self,
        _broadcaster: Broadcaster,
        ready_tx: oneshot::Sender<Result<(), String>>,
        shutdown_rx: oneshot::Receiver<()>,
        _data_rx: mpsc::UnboundedReceiver<NoDataIn>,
    ) {
        let _ = ready_tx.send(Ok(()));
        let _ = shutdown_rx.await;
    }
}

#[test]
fn removing_a_coin_ends_its_balance_streamer_and_leaves_the_streamers_of_other_coins() {
    let (ctx, coins_ctx) = ctx_and_coins();
    block_on(coins_ctx.add_coin(test_coin("AAA"))).unwrap();
    block_on(coins_ctx.add_coin(test_coin("BBB"))).unwrap();
    let manager = &ctx.event_stream_manager;
    let _client = manager.new_client(1);
    block_on(manager.add(1, StreamerOfCoin("AAA".to_owned()))).unwrap();
    block_on(manager.add(1, StreamerOfCoin("BBB".to_owned()))).unwrap();
    assert!(manager.is_active(&StreamerId::Balance("AAA".to_owned())));

    block_on(disable_coin(&ctx, "AAA")).unwrap();

    assert!(!manager.is_active(&StreamerId::Balance("AAA".to_owned())));
    assert!(!manager.client_subscribed_to(1, &StreamerId::Balance("AAA".to_owned())));
    assert!(manager.is_active(&StreamerId::Balance("BBB".to_owned())));
}
