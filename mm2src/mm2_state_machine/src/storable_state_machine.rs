//! Persistence-aware state machine extension with event sourcing.
//!
//! Builds on `state_machine` to add:
//! - Event recording on every state transition
//! - DB-backed storage for machine state and events
//! - Reentrancy locks preventing duplicate execution
//! - Recovery: recreate a machine from stored events and resume

use crate::prelude::*;
use crate::state_machine::{ChangeGuard, ErrorGuard};
use async_trait::async_trait;

/// Marker for the very first state of a storable machine. This state has no
/// `StorableState` impl (it hasn't produced an event yet) but can transition
/// to the first storable state via `ChangeInitialStateExt`.
pub trait InitialState {
    type StateMachine: StorableStateMachine;
}

/// Hook called on every state transition so the machine can react (e.g. update
/// in-memory context, emit SSE events).
#[async_trait]
pub trait OnNewState<S>: StateMachineTrait {
    async fn on_new_state(&mut self, state: &S) -> Result<(), <Self as StateMachineTrait>::Error>;
}

/// Serializable representation of a machine's state for DB storage.
pub trait StateMachineDbRepr: Send {
    type Event: Send;

    /// Append an event to this representation (used during replay/recovery).
    fn add_event(&mut self, event: Self::Event);
}

/// Backend for persisting machine representations and events.
#[async_trait]
pub trait StateMachineStorage: Send + Sync {
    type MachineId: Send + Sync;
    type DbRepr: StateMachineDbRepr;
    type Error: Send;

    /// Persist the initial machine representation.
    async fn store_repr(&mut self, id: Self::MachineId, repr: Self::DbRepr) -> Result<(), Self::Error>;

    /// Load a machine's representation from storage.
    async fn get_repr(&self, id: Self::MachineId) -> Result<Self::DbRepr, Self::Error>;

    /// Check whether a record for this ID already exists.
    async fn has_record_for(&mut self, id: &Self::MachineId) -> Result<bool, Self::Error>;

    /// Append a single event to the machine's event log.
    async fn store_event(
        &mut self,
        id: Self::MachineId,
        event: <Self::DbRepr as StateMachineDbRepr>::Event,
    ) -> Result<(), Self::Error>;

    /// Return IDs of all machines that haven't been marked finished.
    async fn get_unfinished(&self) -> Result<Vec<Self::MachineId>, Self::Error>;

    /// Mark a machine as finished (no longer returned by `get_unfinished`).
    async fn mark_finished(&mut self, id: Self::MachineId) -> Result<(), Self::Error>;
}

/// A state that can be restored from storage and converted back into a live `State`.
pub trait RestoredState: StorableState + Send {
    fn into_state(self: Box<Self>) -> Box<dyn State<StateMachine = Self::StateMachine>>;
}

// Any StorableState that is also a State can trivially become a RestoredState.
impl<T: StorableState + State<StateMachine = Self::StateMachine> + Send> RestoredState for T {
    fn into_state(self: Box<Self>) -> Box<dyn State<StateMachine = Self::StateMachine>> { self }
}

/// Wraps a machine that was recreated from stored events.
pub struct RestoredMachine<M: StorableStateMachine> {
    machine: M,
}

impl<M: StorableStateMachine> RestoredMachine<M> {
    pub fn new(machine: M) -> Self { RestoredMachine { machine } }

    /// The recreated machine itself, for a caller that runs one step of a state by hand instead of resuming the
    /// machine (a manual recovery).
    pub fn into_inner(self) -> M { self.machine }

    /// Resume execution from a recovered state.
    pub async fn kickstart(
        &mut self,
        from_state: Box<dyn RestoredState<StateMachine = M>>,
    ) -> Result<M::Result, M::Error> {
        let event = from_state.get_event();
        self.machine.on_kickstart_event(event);
        self.machine.run(from_state.into_state()).await
    }
}

/// Full storable state machine contract. Implementors provide storage wiring,
/// reentrancy logic, and context lifecycle hooks.
#[async_trait]
pub trait StorableStateMachine: Send + Sync + Sized + 'static {
    type Storage: StateMachineStorage;
    type Result: Send;
    type Error: From<<Self::Storage as StateMachineStorage>::Error> + Send;
    type ReentrancyLock: Send;
    type RecreateCtx: Send;
    type RecreateError: Send;

    /// Serialize current machine state for DB storage.
    fn to_db_repr(&self) -> <Self::Storage as StateMachineStorage>::DbRepr;

    /// Mutable access to the storage backend.
    fn storage(&mut self) -> &mut Self::Storage;

    /// Unique identifier for this machine instance.
    fn id(&self) -> <Self::Storage as StateMachineStorage>::MachineId;

    /// Recreate a machine and its current state from stored events.
    async fn recreate_machine(
        id: <Self::Storage as StateMachineStorage>::MachineId,
        storage: Self::Storage,
        repr: <Self::Storage as StateMachineStorage>::DbRepr,
        from_repr_ctx: Self::RecreateCtx,
    ) -> Result<(RestoredMachine<Self>, Box<dyn RestoredState<StateMachine = Self>>), Self::RecreateError>;

    /// Persist a single event (delegates to storage).
    async fn store_event(
        &mut self,
        event: <<Self::Storage as StateMachineStorage>::DbRepr as StateMachineDbRepr>::Event,
    ) -> Result<(), <Self::Storage as StateMachineStorage>::Error> {
        let id = self.id();
        self.storage().store_event(id, event).await
    }

    /// Mark this machine as finished in storage.
    async fn mark_finished(&mut self) -> Result<(), <Self::Storage as StateMachineStorage>::Error> {
        let id = self.id();
        self.storage().mark_finished(id).await
    }

    /// Acquire a reentrancy lock preventing parallel execution of the same machine.
    async fn acquire_reentrancy_lock(&self) -> Result<Self::ReentrancyLock, Self::Error>;

    /// Spawn a task that periodically renews the reentrancy lock.
    fn spawn_reentrancy_lock_renew(&mut self, guard: Self::ReentrancyLock);

    /// Initialize any additional runtime context (spawn background tasks, etc.).
    fn init_additional_context(&mut self);

    /// Clean up runtime context on completion.
    fn clean_up_context(&mut self);

    /// React to a state event during normal execution (e.g. update in-memory state).
    fn on_event(&mut self, event: &<<Self::Storage as StateMachineStorage>::DbRepr as StateMachineDbRepr>::Event);

    /// React to the event of the state we're resuming from after recovery.
    fn on_kickstart_event(
        &mut self,
        event: <<Self::Storage as StateMachineStorage>::DbRepr as StateMachineDbRepr>::Event,
    );
}

// StorableStateMachine must NOT also be a StandardStateMachine — the two use
// different transition helpers (ChangeStateOnNewExt vs ChangeStateExt).
impl<T: StorableStateMachine> !StandardStateMachine for T {}

// An InitialState hasn't produced an event yet, so it must not implement StorableState.
impl<T: StorableState> !InitialState for T {}

// Provide StateMachineTrait implementation for all StorableStateMachines.
// on_start persists the initial representation and acquires the reentrancy lock.
// on_finished marks the machine complete and cleans up context.
#[async_trait]
impl<T: StorableStateMachine> StateMachineTrait for T {
    type Result = T::Result;
    type Error = T::Error;

    async fn on_start(&mut self) -> Result<(), Self::Error> {
        let reentrancy_lock = self.acquire_reentrancy_lock().await?;
        let id = self.id();
        if !self.storage().has_record_for(&id).await? {
            let repr = self.to_db_repr();
            self.storage().store_repr(id, repr).await?;
        }
        self.spawn_reentrancy_lock_renew(reentrancy_lock);
        self.init_additional_context();
        Ok(())
    }

    async fn on_finished(&mut self) -> Result<(), T::Error> {
        self.mark_finished().await?;
        self.clean_up_context();
        Ok(())
    }
}

/// Trait binding a state to its storable machine and providing the event it produces.
pub trait StorableState {
    type StateMachine: StorableStateMachine;

    fn get_event(
        &self,
    ) -> <<<Self::StateMachine as StorableStateMachine>::Storage as StateMachineStorage>::DbRepr as StateMachineDbRepr>::Event;
}

// Auto-implement OnNewState for any StorableStateMachine + matching StorableState:
// persist the event and notify the machine.
#[async_trait]
impl<T: StorableStateMachine + Sync, S: StorableState<StateMachine = T> + Sync> OnNewState<S> for T {
    async fn on_new_state(&mut self, state: &S) -> Result<(), T::Error> {
        let event = state.get_event();
        self.on_event(&event);
        Ok(self.store_event(event).await?)
    }
}

/// Internal helper: persist the new state's event, then wrap it in a ChangeGuard.
async fn change_state_impl<Next>(next_state: Next, machine: &mut Next::StateMachine) -> StateResult<Next::StateMachine>
where
    Next: State + ChangeStateOnNewExt,
    Next::StateMachine: OnNewState<Next> + Sync,
{
    if let Err(e) = machine.on_new_state(&next_state).await {
        return StateResult::Error(ErrorGuard::new(e));
    }
    StateResult::ChangeState(ChangeGuard::next(next_state))
}

/// Transition helper for storable states. Like `ChangeStateExt` but also persists events.
#[async_trait]
pub trait ChangeStateOnNewExt {
    async fn change_state<Next>(next_state: Next, machine: &mut Next::StateMachine) -> StateResult<Next::StateMachine>
    where
        Self: Sized,
        Next: State + TransitionFrom<Self> + ChangeStateOnNewExt,
        Next::StateMachine: OnNewState<Next> + Sync,
    {
        change_state_impl(next_state, machine).await
    }
}

impl<M: StorableStateMachine, T: StorableState<StateMachine = M>> ChangeStateOnNewExt for T {}

/// Transition helper for the initial state (which is not itself storable).
#[async_trait]
pub trait ChangeInitialStateExt: InitialState {
    async fn change_state<Next>(next_state: Next, machine: &mut Next::StateMachine) -> StateResult<Next::StateMachine>
    where
        Self: Sized,
        Next: State + TransitionFrom<Self> + ChangeStateOnNewExt,
        Next::StateMachine: OnNewState<Next> + Sync,
    {
        change_state_impl(next_state, machine).await
    }
}

impl<M: StorableStateMachine, T: InitialState<StateMachine = M>> ChangeInitialStateExt for T {}

#[cfg(test)]
mod tests {
    use super::*;
    use common::block_on;
    use std::collections::HashMap;
    use std::convert::Infallible;

    // --- Storage mock --------------------------------------------------------

    struct MockStorage {
        unfinished: HashMap<usize, Vec<TestEvent>>,
        finished: HashMap<usize, Vec<TestEvent>>,
    }

    impl MockStorage {
        fn empty() -> Self {
            MockStorage {
                unfinished: HashMap::new(),
                finished: HashMap::new(),
            }
        }
    }

    #[derive(Debug, Eq, PartialEq)]
    enum TestEvent {
        EnteredB,
        EnteredC,
        EnteredD,
    }

    struct TestRepr;

    impl StateMachineDbRepr for TestRepr {
        type Event = TestEvent;
        fn add_event(&mut self, _event: Self::Event) { /* no-op for tests */
        }
    }

    #[async_trait]
    impl StateMachineStorage for MockStorage {
        type MachineId = usize;
        type DbRepr = TestRepr;
        type Error = Infallible;

        async fn store_repr(&mut self, _id: usize, _repr: TestRepr) -> Result<(), Infallible> { Ok(()) }
        async fn get_repr(&self, _id: usize) -> Result<TestRepr, Infallible> { Ok(TestRepr) }
        async fn has_record_for(&mut self, _id: &usize) -> Result<bool, Infallible> { Ok(false) }

        async fn store_event(&mut self, id: usize, event: TestEvent) -> Result<(), Infallible> {
            self.unfinished.entry(id).or_default().push(event);
            Ok(())
        }

        async fn get_unfinished(&self) -> Result<Vec<usize>, Infallible> {
            Ok(self.unfinished.keys().copied().collect())
        }

        async fn mark_finished(&mut self, id: usize) -> Result<(), Infallible> {
            let events = self.unfinished.remove(&id).unwrap();
            self.finished.insert(id, events);
            Ok(())
        }
    }

    // --- Machine -------------------------------------------------------------

    struct TestMachine {
        id: usize,
        storage: MockStorage,
    }

    #[async_trait]
    impl StorableStateMachine for TestMachine {
        type Storage = MockStorage;
        type Result = ();
        type Error = Infallible;
        type ReentrancyLock = ();
        type RecreateCtx = ();
        type RecreateError = Infallible;

        fn to_db_repr(&self) -> TestRepr { TestRepr }
        fn storage(&mut self) -> &mut MockStorage { &mut self.storage }
        fn id(&self) -> usize { self.id }

        async fn recreate_machine(
            id: usize,
            storage: MockStorage,
            _repr: TestRepr,
            _ctx: (),
        ) -> Result<(RestoredMachine<Self>, Box<dyn RestoredState<StateMachine = Self>>), Infallible> {
            let last_event = storage.unfinished.get(&id).and_then(|v| v.last());
            let state: Box<dyn RestoredState<StateMachine = Self>> = match last_event {
                Some(TestEvent::EnteredB) => Box::new(StateB),
                _ => unimplemented!("test only supports resuming from StateB"),
            };
            let machine = TestMachine { id, storage };
            Ok((RestoredMachine { machine }, state))
        }

        async fn acquire_reentrancy_lock(&self) -> Result<(), Infallible> { Ok(()) }
        fn spawn_reentrancy_lock_renew(&mut self, _guard: ()) {}
        fn init_additional_context(&mut self) {}
        fn clean_up_context(&mut self) {}
        fn on_event(&mut self, _event: &TestEvent) {}
        fn on_kickstart_event(&mut self, _event: TestEvent) {}
    }

    // --- States: A → B → C → D (terminal) -----------------------------------

    struct StateA;

    impl InitialState for StateA {
        type StateMachine = TestMachine;
    }

    struct StateB;

    impl StorableState for StateB {
        type StateMachine = TestMachine;
        fn get_event(&self) -> TestEvent { TestEvent::EnteredB }
    }

    impl TransitionFrom<StateA> for StateB {}

    struct StateC;

    impl StorableState for StateC {
        type StateMachine = TestMachine;
        fn get_event(&self) -> TestEvent { TestEvent::EnteredC }
    }

    impl TransitionFrom<StateB> for StateC {}

    struct StateD;

    impl StorableState for StateD {
        type StateMachine = TestMachine;
        fn get_event(&self) -> TestEvent { TestEvent::EnteredD }
    }

    impl TransitionFrom<StateC> for StateD {}

    #[async_trait]
    impl LastState for StateD {
        type StateMachine = TestMachine;
        async fn on_changed(self: Box<Self>, _ctx: &mut TestMachine) {}
    }

    #[async_trait]
    impl State for StateA {
        type StateMachine = TestMachine;
        async fn on_changed(self: Box<Self>, ctx: &mut TestMachine) -> StateResult<TestMachine> {
            Self::change_state(StateB, ctx).await
        }
    }

    #[async_trait]
    impl State for StateB {
        type StateMachine = TestMachine;
        async fn on_changed(self: Box<Self>, ctx: &mut TestMachine) -> StateResult<TestMachine> {
            Self::change_state(StateC, ctx).await
        }
    }

    #[async_trait]
    impl State for StateC {
        type StateMachine = TestMachine;
        async fn on_changed(self: Box<Self>, ctx: &mut TestMachine) -> StateResult<TestMachine> {
            Self::change_state(StateD, ctx).await
        }
    }

    // --- Tests ---------------------------------------------------------------

    #[test]
    fn test_storable_machine_full_run() {
        let mut machine = TestMachine {
            id: 1,
            storage: MockStorage::empty(),
        };
        block_on(machine.run(Box::new(StateA))).unwrap();

        let expected = HashMap::from([(1, vec![TestEvent::EnteredB, TestEvent::EnteredC, TestEvent::EnteredD])]);
        assert_eq!(expected, machine.storage.finished);
    }

    #[test]
    fn test_storable_machine_restore_and_resume() {
        let mut storage = MockStorage::empty();
        storage.unfinished.insert(1, vec![TestEvent::EnteredB]);

        let (mut restored, from_state) = block_on(TestMachine::recreate_machine(1, storage, TestRepr, ())).unwrap();

        block_on(restored.kickstart(from_state)).unwrap();

        // Should contain B (from before crash) + C + D (from resumed execution)
        let expected = HashMap::from([(1, vec![TestEvent::EnteredB, TestEvent::EnteredC, TestEvent::EnteredD])]);
        assert_eq!(expected, restored.machine.storage.finished);
    }
}
