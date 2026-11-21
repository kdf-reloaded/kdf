//! The background work of one coin instance (CRD ch.43 R43.1E.5 item 4, ch.55 R55.4.9).
//!
//! A [`CoinWork`] collects the abort handles of the tasks that serve one instance of a coin: its history collector, its
//! block-header loop and the like. The registry keeps one beside each registered entry, so removing the entry can end
//! the work, and the id of the `CoinWork` is the identity of the instance: a loop compares it with the id the registry
//! holds for its ticker, and a loop of an instance that was removed (and perhaps registered again) ends.
//!
//! An activation creates a [`PendingCoinWork`] before it builds the instance. Work spawned during the build and after
//! the registration registers with it. Registering the instance hands the `CoinWork` over to the registry; an
//! activation that ends without registering the instance (refused, failed, cancelled) drops the `PendingCoinWork`, and
//! that ends the work already spawned.
//!
//! The scope is per poll, not per logical owner, so only work owned by exactly the instance being built may call
//! [`CoinWork::register_with_current`] (or run in [`carry_current_scope`]). The task of a shared resource (a shared
//! Electrum client built during the first instance's activation, a coalesced request) must never register with the
//! current scope: removing that one instance would abort work that other instances use. A scoped poll must not run a
//! nested `block_on`, because that polls foreign futures inside the scope.
//!
//! Node-wide work that is not per coin stays in `MmCtx::abort_handlers`.

use futures::future::AbortHandle;
use std::cell::RefCell;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// The instance ids of a process, in the order the instances were created.
static NEXT_INSTANCE_ID: AtomicU64 = AtomicU64::new(1);

enum WorkState {
    Open(Vec<AbortHandle>),
    Ended,
}

/// The abort handles of the background work of one coin instance, and the identity of the instance.
pub struct CoinWork {
    id: u64,
    state: Mutex<WorkState>,
}

impl std::fmt::Debug for CoinWork {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoinWork").field("id", &self.id).finish()
    }
}

impl CoinWork {
    pub fn new() -> Arc<CoinWork> {
        Arc::new(CoinWork {
            id: NEXT_INSTANCE_ID.fetch_add(1, Ordering::Relaxed),
            state: Mutex::new(WorkState::Open(Vec::new())),
        })
    }

    /// The instance id, unique in the process.
    pub fn id(&self) -> u64 { self.id }

    fn state(&self) -> std::sync::MutexGuard<'_, WorkState> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Registers the handle of a task of this instance. After [`Self::abort_all`] the task is aborted at once.
    pub fn add(&self, handle: AbortHandle) {
        let mut state = self.state();
        match &mut *state {
            WorkState::Open(handles) => handles.push(handle),
            WorkState::Ended => handle.abort(),
        }
    }

    /// Aborts every registered task and refuses later ones: they are aborted when they register.
    pub fn abort_all(&self) {
        let mut state = self.state();
        if let WorkState::Open(handles) = std::mem::replace(&mut *state, WorkState::Ended) {
            for handle in handles {
                handle.abort();
            }
        }
    }

    /// Whether [`Self::abort_all`] has run.
    pub fn is_ended(&self) -> bool { matches!(*self.state(), WorkState::Ended) }

    /// The number of the registered tasks (zero after [`Self::abort_all`]).
    pub fn len(&self) -> usize {
        match &*self.state() {
            WorkState::Open(handles) => handles.len(),
            WorkState::Ended => 0,
        }
    }

    pub fn is_empty(&self) -> bool { self.len() == 0 }

    /// The `CoinWork` of the activation whose build the current future runs in, if any. See
    /// [`PendingCoinWork::scope`].
    pub fn current() -> Option<Arc<CoinWork>> { CURRENT.with(|current| current.borrow().clone()) }

    /// Runs `fut` so that [`CoinWork::current`] is this `CoinWork` inside it. The scope is set on every poll, so it
    /// holds across `.await` points and on every executor thread. A history loop runs in the scope of its instance, to
    /// read the instance id it has to compare with the registry's.
    pub async fn scope<F: Future>(self: &Arc<Self>, fut: F) -> F::Output {
        let work = self.clone();
        let mut fut = Box::pin(fut);
        futures::future::poll_fn(move |cx| {
            let _scope = EnterScope::enter(&work);
            fut.as_mut().poll(cx)
        })
        .await
    }

    /// Runs the synchronous `f` with [`Self::current`] set to this `CoinWork`. A function that spawns a task during `f`
    /// can make the task run in the same scope with [`carry_current_scope`].
    pub fn run_in_scope<R>(self: &Arc<Self>, f: impl FnOnce() -> R) -> R {
        let _scope = EnterScope::enter(self);
        f()
    }

    /// The id of [`Self::current`]: the instance a loop that runs in its scope belongs to.
    pub fn current_id() -> Option<u64> { CoinWork::current().map(|work| work.id()) }

    /// Registers the handle with the `CoinWork` of the activation being built, if the caller runs inside one, and
    /// returns `None`; otherwise returns the handle to the caller, who keeps it where it kept it before.
    pub fn register_with_current(handle: AbortHandle) -> Option<AbortHandle> {
        match CoinWork::current() {
            Some(work) => {
                work.add(handle);
                None
            },
            None => Some(handle),
        }
    }
}

/// Makes `fut` run in the scope that is current now, so that a task a function spawns inside [`CoinWork::run_in_scope`]
/// or [`CoinWork::scope`] belongs to the same instance. Outside any scope `fut` is returned as it is.
pub fn carry_current_scope<F: Future>(fut: F) -> impl Future<Output = F::Output> {
    let work = CoinWork::current();
    async move {
        match work {
            Some(work) => work.scope(fut).await,
            None => fut.await,
        }
    }
}

/// Whether a loop of the instance `own` still belongs to the registered instance of its ticker (CRD ch.55 R55.4.9):
/// the ticker is registered, and as that instance. A loop that runs outside any instance scope (`own` is `None`)
/// only needs the ticker to be registered.
pub fn is_same_instance(registered: Option<u64>, own: Option<u64>) -> bool {
    match (registered, own) {
        (Some(registered), Some(own)) => registered == own,
        (Some(_), None) => true,
        (None, _) => false,
    }
}

thread_local! {
    static CURRENT: RefCell<Option<Arc<CoinWork>>> = const { RefCell::new(None) };
}

/// Puts `work` into [`CURRENT`] for the length of one poll and restores the previous value afterwards.
struct EnterScope(Option<Arc<CoinWork>>);

impl EnterScope {
    fn enter(work: &Arc<CoinWork>) -> EnterScope {
        EnterScope(CURRENT.with(|current| current.borrow_mut().replace(work.clone())))
    }
}

impl Drop for EnterScope {
    fn drop(&mut self) { CURRENT.with(|current| *current.borrow_mut() = self.0.take()); }
}

/// The `CoinWork` of an instance that is being built and is not registered yet. Dropping it ends the work registered
/// with it; registering the instance takes the `CoinWork` out with [`Self::into_work`], which leaves the work running.
pub struct PendingCoinWork {
    work: Arc<CoinWork>,
    armed: bool,
}

impl PendingCoinWork {
    /// Created when an activation starts to build an instance (CRD ch.43 §43.1E not-established item (1)).
    pub fn new() -> PendingCoinWork {
        PendingCoinWork {
            work: CoinWork::new(),
            armed: true,
        }
    }

    pub fn work(&self) -> &Arc<CoinWork> { &self.work }

    /// Disarms the guard and hands the `CoinWork` over, for the registry to keep.
    pub fn into_work(mut self) -> Arc<CoinWork> {
        self.armed = false;
        self.work.clone()
    }

    /// Runs `fut`, a build of the instance, so that the work it spawns through [`CoinWork::register_with_current`]
    /// registers with this `CoinWork`. See [`CoinWork::scope`].
    pub async fn scope<F: Future>(&self, fut: F) -> F::Output { self.work.scope(fut).await }
}

impl Default for PendingCoinWork {
    fn default() -> Self { PendingCoinWork::new() }
}

impl Drop for PendingCoinWork {
    fn drop(&mut self) {
        if self.armed {
            self.work.abort_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::future::abortable;

    #[test]
    fn abort_all_aborts_every_registered_handle() {
        let work = CoinWork::new();
        let (fut1, handle1) = abortable(futures::future::pending::<()>());
        let (fut2, handle2) = abortable(futures::future::pending::<()>());
        work.add(handle1);
        work.add(handle2);
        assert_eq!(work.len(), 2);
        work.abort_all();
        assert!(work.is_ended());
        assert!(common::block_on(fut1).is_err());
        assert!(common::block_on(fut2).is_err());
    }

    #[test]
    fn add_after_abort_all_aborts_at_once() {
        let work = CoinWork::new();
        work.abort_all();
        let (fut, handle) = abortable(futures::future::pending::<()>());
        work.add(handle);
        assert!(common::block_on(fut).is_err());
        assert_eq!(work.len(), 0);
    }

    #[test]
    fn instance_ids_are_unique() {
        assert_ne!(CoinWork::new().id(), CoinWork::new().id());
    }

    #[test]
    fn dropping_a_pending_work_aborts_it_and_handing_it_over_does_not() {
        let dropped = PendingCoinWork::new();
        let (fut, handle) = abortable(futures::future::pending::<()>());
        dropped.work().add(handle);
        drop(dropped);
        assert!(common::block_on(fut).is_err());

        let kept = PendingCoinWork::new();
        let (fut, handle) = abortable(async { 7u8 });
        kept.work().add(handle);
        let work = kept.into_work();
        assert!(!work.is_ended());
        assert_eq!(common::block_on(fut), Ok(7));
    }

    #[test]
    fn the_scope_reaches_work_registered_while_building() {
        let pending = PendingCoinWork::new();
        let (fut, handle) = abortable(futures::future::pending::<()>());
        let build = async move {
            // A yield point, so the scope has to hold across a re-poll.
            futures::future::ready(()).await;
            let rest = CoinWork::register_with_current(handle);
            assert!(rest.is_none());
        };
        common::block_on(pending.scope(build));
        assert_eq!(pending.work().len(), 1);
        assert!(CoinWork::current().is_none());
        drop(pending);
        assert!(common::block_on(fut).is_err());
    }

    #[test]
    fn a_loop_ends_when_another_instance_of_its_ticker_is_registered() {
        let old = CoinWork::new();
        let new = CoinWork::new();
        assert!(is_same_instance(Some(old.id()), Some(old.id())));
        assert!(!is_same_instance(Some(new.id()), Some(old.id())));
        assert!(!is_same_instance(None, Some(old.id())));
        assert!(is_same_instance(Some(new.id()), None));
        assert!(!is_same_instance(None, None));
    }

    #[test]
    fn a_task_spawned_in_a_scope_carries_the_scope() {
        let work = CoinWork::new();
        let id = work.id();
        let task = work.run_in_scope(|| carry_current_scope(async { CoinWork::current_id() }));
        // Polled outside any scope, the task still sees its instance.
        assert!(CoinWork::current().is_none());
        assert_eq!(common::block_on(task), Some(id));
        assert_eq!(
            common::block_on(carry_current_scope(async { CoinWork::current_id() })),
            None
        );
    }

    #[test]
    fn outside_a_scope_the_handle_goes_back_to_the_caller() {
        let (_fut, handle) = abortable(futures::future::pending::<()>());
        assert!(CoinWork::register_with_current(handle).is_some());
    }
}
